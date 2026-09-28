//! Production password hashing: Argon2id native, bcrypt legacy, dummy unknown.
//!
//! Scheme dispatch is always on the explicit tag, never by sniffing: rows carry
//! `bcrypt` (v2 imports, verified then opportunistically rehashed) or `argon2id`
//! (every v3 hash). Unknown users cost one dummy verify so bad-user and
//! bad-password stay indistinguishable by status, body, and hash work.
//!
//! Argon2id parameters are pinned to the OWASP Password Storage Cheat Sheet
//! minimum (verified 2026-09-28 against the live sheet): 19 MiB of memory, an
//! iteration count of 2, and 1 lane of parallelism (`m=19456, t=2, p=1`),
//! Argon2id version 1.3. The [`OWASP_*`] constants are the pin; the params test
//! fails if the live parameters ever drift from them.

use std::sync::{Arc, Mutex};

use argon2::password_hash::SaltString;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher as _, PasswordVerifier as _};

use super::federated::password_import::{HashScheme, PasswordHasher};
use super::session::login::PasswordVerifier as SessionPasswordVerifier;
use super::users::stores::{FalliblePasswordHasher, HashError};

/// OWASP minimum memory cost in KiB (19 MiB).
pub const OWASP_M_COST_KIB: u32 = 19 * 1024;
/// OWASP minimum iteration count.
pub const OWASP_T_COST: u32 = 2;
/// OWASP minimum parallelism.
pub const OWASP_P_COST: u32 = 1;

/// Fixed dummy password. It never authenticates; [`Argon2idHasher::dummy_verify`]
/// burns one native verify against [`DUMMY_HASH`] so unknown users cost what a
/// current-scheme user costs. Legacy bcrypt rows cost a different (older) amount;
/// that gap closes itself as logins rehash them to Argon2id.
const DUMMY_PASSWORD: &str = "droppedneedle-dummy-verify";

/// Fixed dummy hash: Argon2id(m=19456, t=2, p=1) of [`DUMMY_PASSWORD`] under
/// the fixed salt `dn-dummy-salt-01`. Deterministic so any checkout regenerates it (see
/// `dummy_vector_regenerates`); safe to embed because it authenticates nothing.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$ZG4tZHVtbXktc2FsdC0wMQ$BTpixShEELs7xt5OldVvrReW8gSKKFDGSZle7l8LuaI";

/// A bcrypt success that still needs its Argon2id row persisted. The
/// session insert drains the queue and applies the entry whose old hash
/// matches the logging-in user's current row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRehash {
    /// The verified legacy bcrypt hash (match key, never persisted as new).
    pub old_hash: String,
    /// The replacement Argon2id hash, computed from the login password.
    pub new_hash: String,
}

/// Sync bridge from the bool-only session verifier to the async login
/// insert. `verify` is sync and store-blind, so a bcrypt success pushes the
/// computed upgrade here and the post-login session insert persists it.
/// Clones share one queue. Bounded: the oldest entry evicts past the cap,
/// so a failed insert never grows this without limit.
#[derive(Debug, Clone, Default)]
pub struct RehashQueue {
    inner: Arc<Mutex<Vec<PendingRehash>>>,
}

/// Cap on queued upgrades (one per legacy login; drained on insert).
const REHASH_QUEUE_CAP: usize = 64;

impl RehashQueue {
    /// Queue one upgrade, evicting the oldest past the cap.
    pub fn push(&self, pending: PendingRehash) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if guard.len() >= REHASH_QUEUE_CAP {
            guard.remove(0);
        }
        guard.push(pending);
    }

    /// Take every queued upgrade, leaving the queue empty.
    pub fn drain(&self) -> Vec<PendingRehash> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut *guard)
    }

    /// Return unconsumed upgrades (a failed insert re-queues everything it
    /// drained, so the next login for that user retries).
    pub fn requeue(&self, pending: Vec<PendingRehash>) {
        if pending.is_empty() {
            return;
        }
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.extend(pending);
        while guard.len() > REHASH_QUEUE_CAP {
            guard.remove(0);
        }
    }
}

/// Production hasher: real Argon2id and bcrypt primitives behind both slice
/// ports (the federated [`PasswordHasher`] and the session [`SessionPasswordVerifier`]).
/// Verification is fail-closed everywhere: malformed hashes, overlong
/// passwords, and unknown scheme tags all verify as false.
#[derive(Clone)]
pub struct Argon2idHasher {
    argon2: Arc<Argon2<'static>>,
    rehash: RehashQueue,
}

impl std::fmt::Debug for Argon2idHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Argon2idHasher(..)")
    }
}

impl Argon2idHasher {
    /// Build the hasher with the pinned OWASP parameters. Construction is
    /// infallible: the pinned constants are valid, and the crate default
    /// (identical OWASP values in argon2 0.5) is the unreachable fallback.
    pub fn new() -> Self {
        let params =
            Params::new(OWASP_M_COST_KIB, OWASP_T_COST, OWASP_P_COST, None).unwrap_or_default();
        Self {
            argon2: Arc::new(Argon2::new(
                Algorithm::Argon2id,
                argon2::Version::V0x13,
                params,
            )),
            rehash: RehashQueue::default(),
        }
    }

    /// The rehash queue this hasher pushes bcrypt upgrades to. Production
    /// wiring hands the same queue to the session store, whose login insert
    /// drains it; an unshared queue simply never drains.
    pub fn rehash_queue(&self) -> &RehashQueue {
        &self.rehash
    }

    /// Hash a password with Argon2id, or fail. This is the only hashing
    /// entry point production code may use; the federated port method below
    /// exists only because that trait is infallible.
    pub fn try_hash_argon2id(&self, password: &str) -> Result<String, HashError> {
        let mut salt_bytes = [0u8; 16];
        if getrandom::fill(&mut salt_bytes).is_err() {
            return Err(HashError::RngUnavailable);
        }
        let Ok(salt) = SaltString::encode_b64(&salt_bytes) else {
            return Err(HashError::HashFailed);
        };
        self.argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| HashError::HashFailed)
    }
}

impl Default for Argon2idHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl PasswordHasher for Argon2idHasher {
    fn verify_bcrypt(&self, password: &str, hash: &str) -> bool {
        bcrypt::verify(password, hash).unwrap_or(false)
    }

    fn verify_argon2id(&self, password: &str, hash: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(hash) else {
            return false;
        };
        self.argon2
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }

    /// Legacy infallible entry point, kept only because the federated
    /// port requires it. Production code must use `try_hash_argon2id`
    /// instead: on failure this logs and returns an empty string that no
    /// caller may persist.
    fn hash_argon2id(&self, password: &str) -> String {
        self.try_hash_argon2id(password).unwrap_or_else(|error| {
            tracing::error!(%error, "password hash failed; returning an unpersistable empty hash");
            String::new()
        })
    }

    fn dummy_verify(&self) {
        // Same code path as a real native verify, so the cost matches by
        // construction. The result is meaningless and discarded.
        let _ = self.verify_argon2id(DUMMY_PASSWORD, DUMMY_HASH);
    }
}

impl FalliblePasswordHasher for Argon2idHasher {
    fn try_hash_argon2id(&self, password: &str) -> Result<String, HashError> {
        Argon2idHasher::try_hash_argon2id(self, password)
    }
}

impl SessionPasswordVerifier for Argon2idHasher {
    fn verify(&self, candidate: &str, stored: &str) -> bool {
        let Some((scheme, hash)) = stored.split_once('$') else {
            PasswordHasher::dummy_verify(self);
            return false;
        };
        match HashScheme::from_tag(scheme) {
            Some(HashScheme::Bcrypt) => {
                if !self.verify_bcrypt(candidate, hash) {
                    return false;
                }
                // Opportunistic rehash: the bool-only port cannot return the
                // upgrade, so queue it for the post-login session insert. A
                // hashing failure here must never fail the login that just
                // succeeded; it logs and retries on the next login.
                match self.try_hash_argon2id(candidate) {
                    Ok(new_hash) => self.rehash.push(PendingRehash {
                        old_hash: hash.to_owned(),
                        new_hash,
                    }),
                    Err(error) => tracing::error!(%error, "login rehash skipped: hash failed"),
                }
                true
            }
            Some(HashScheme::Argon2id) => self.verify_argon2id(candidate, hash),
            None => {
                PasswordHasher::dummy_verify(self);
                false
            }
        }
    }

    fn dummy_verify(&self) {
        PasswordHasher::dummy_verify(self);
    }
}

#[cfg(test)]
mod tests {
    use super::super::federated::password_import::{
        LocalCredential, PasswordCheck, verify_and_maybe_rehash,
    };
    use super::*;

    #[test]
    fn params_match_the_owasp_pin() {
        let hasher = Argon2idHasher::new();
        let first = hasher.hash_argon2id("correct horse battery staple");
        assert!(
            first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{first}"
        );
        // Random salt: two hashes of one password differ, and both verify.
        let second = hasher.hash_argon2id("correct horse battery staple");
        assert_ne!(first, second);
        assert!(hasher.verify_argon2id("correct horse battery staple", &first));
        assert!(hasher.verify_argon2id("correct horse battery staple", &second));
        assert!(!hasher.verify_argon2id("wrong password here!", &first));
        assert!(!hasher.verify_argon2id("correct horse battery staple", "not-a-hash"));
    }

    #[test]
    fn bcrypt_rows_verify_then_rehash_to_argon2id() {
        let hasher = Argon2idHasher::new();
        let legacy = bcrypt::hash("correct horse battery staple", 4).unwrap();
        assert!(hasher.verify_bcrypt("correct horse battery staple", &legacy));
        assert!(!hasher.verify_bcrypt("wrong password here!", &legacy));
        let stored = LocalCredential::imported_bcrypt(legacy);
        let PasswordCheck::Valid { upgraded } =
            verify_and_maybe_rehash(&hasher, "correct horse battery staple", Some(&stored))
        else {
            panic!("legacy row must verify");
        };
        let upgraded = upgraded.expect("bcrypt success must rehash");
        assert_eq!(upgraded.scheme, HashScheme::Argon2id);
        assert!(hasher.verify_argon2id("correct horse battery staple", &upgraded.hash));
        assert_eq!(
            verify_and_maybe_rehash(&hasher, "correct horse battery staple", None),
            PasswordCheck::Invalid
        );
    }

    #[test]
    fn scheme_dispatch_never_sniffs() {
        let hasher = Argon2idHasher::new();
        let bcrypt_hash = bcrypt::hash("correct horse battery staple", 4).unwrap();
        let argon_hash = hasher.hash_argon2id("correct horse battery staple");
        assert!(SessionPasswordVerifier::verify(
            &hasher,
            "correct horse battery staple",
            &format!("bcrypt${bcrypt_hash}")
        ));
        assert!(SessionPasswordVerifier::verify(
            &hasher,
            "correct horse battery staple",
            &format!("argon2id${argon_hash}")
        ));
        // Bare hashes (no tag), unknown tags, and garbage all fail closed
        // after dummy work, exactly like a wrong password.
        for stored in [
            bcrypt_hash.clone(),
            argon_hash.clone(),
            format!("scrypt${argon_hash}"),
            "garbage".to_owned(),
            String::new(),
        ] {
            assert!(
                !SessionPasswordVerifier::verify(&hasher, "correct horse battery staple", &stored),
                "{stored}"
            );
        }
        assert!(!SessionPasswordVerifier::verify(
            &hasher,
            "wrong password here!",
            &format!("bcrypt${bcrypt_hash}")
        ));
    }

    #[test]
    fn dummy_vector_regenerates() {
        let hasher = Argon2idHasher::new();
        let salt = SaltString::encode_b64(b"dn-dummy-salt-01").unwrap();
        let regenerated = hasher
            .argon2
            .hash_password(DUMMY_PASSWORD.as_bytes(), &salt)
            .unwrap()
            .to_string();
        assert_eq!(regenerated, DUMMY_HASH);
    }
}
