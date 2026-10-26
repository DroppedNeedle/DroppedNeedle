//! Password hashing: Argon2id native, bcrypt legacy, dummy unknown.
//!
//! Scheme dispatch is always on the explicit tag, never by sniffing: rows carry
//! `bcrypt` (v2 imports) or `argon2id` (every v3 hash). A missing, unreadable
//! or unknown-scheme credential costs one dummy verify, so bad-user and
//! bad-password stay indistinguishable by status, body, and hash work.
//!
//! Argon2id parameters are pinned to the OWASP Password Storage Cheat Sheet
//! minimum (verified 2026-09-28 against the live sheet): 19 MiB of memory, an
//! iteration count of 2, and 1 lane of parallelism (`m=19456, t=2, p=1`),
//! Argon2id version 1.3. The [`OWASP_*`] constants are the pin; the params test
//! fails if the live parameters ever drift from them.
//!
//! A verify that succeeds against a hash weaker than the pin (bcrypt, or an
//! Argon2 hash of another algorithm, an older version, or less memory or
//! fewer iterations) returns a fresh hash at the pin, which the login writes
//! in the same transaction as its session. A hash at or above the pin is
//! never rehashed, so a login never weakens a stored hash.

use std::sync::Arc;

use argon2::password_hash::SaltString;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher as _, PasswordVerifier as _};
use thiserror::Error;

use super::federated::password_import::HashScheme;

/// OWASP minimum memory cost in KiB (19 MiB).
pub const OWASP_M_COST_KIB: u32 = 19 * 1024;
/// OWASP minimum iteration count.
pub const OWASP_T_COST: u32 = 2;
/// OWASP minimum parallelism.
pub const OWASP_P_COST: u32 = 1;

/// Fixed dummy password. It never authenticates; the dummy verify burns one
/// native verify against [`DUMMY_HASH`] so a missing credential costs what a
/// current-scheme user costs. Legacy bcrypt rows cost a different (older)
/// amount; that gap closes itself as logins rehash them to Argon2id.
const DUMMY_PASSWORD: &str = "droppedneedle-dummy-verify";

/// Fixed dummy hash: Argon2id(m=19456, t=2, p=1) of [`DUMMY_PASSWORD`] under
/// the fixed salt `dn-dummy-salt-01`. Deterministic so any checkout regenerates it (see
/// `dummy_vector_regenerates`); safe to embed because it authenticates nothing.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$ZG4tZHVtbXktc2FsdC0wMQ$BTpixShEELs7xt5OldVvrReW8gSKKFDGSZle7l8LuaI";

/// Every way Argon2id hashing can fail. The message goes to the log only.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HashError {
    /// The OS random source failed; no salt, no hash, no fallback.
    #[error("random source unavailable")]
    RngUnavailable,
    /// Salt encoding or the Argon2id run itself failed.
    #[error("password hash failed")]
    HashFailed,
}

/// A local credential as its row stores it: the scheme tag and the hash.
/// The tag is kept as read, so an unknown one still reaches [`Passwords::verify`]
/// and fails closed there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPassword {
    /// Scheme tag (`bcrypt` or `argon2id`; anything else verifies as wrong).
    pub scheme: String,
    /// The stored hash string.
    pub hash: String,
}

/// What a password check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Wrong password, or no usable credential (after a dummy verify).
    Wrong,
    /// The password matched. `rehash` is a fresh Argon2id hash at the pin
    /// when the stored one is weaker, for the caller to write.
    Verified {
        /// Replacement hash, tagged `argon2id`, or `None` when the stored
        /// hash is at or above the pin (or the new hash could not be made).
        rehash: Option<String>,
    },
}

/// Hashes and verifies passwords. Synchronous by design: hashing is CPU
/// work, and callers run it on the blocking pool. Verification fails
/// closed: malformed hashes, overlong passwords and unknown scheme tags all
/// verify as wrong.
#[derive(Clone)]
pub struct Passwords {
    argon2: Arc<Argon2<'static>>,
    #[cfg(test)]
    verifies: Arc<std::sync::atomic::AtomicUsize>,
}

impl std::fmt::Debug for Passwords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Passwords(..)")
    }
}

impl Passwords {
    /// Build with the pinned OWASP parameters. Construction is infallible:
    /// the pinned constants are valid, and the crate default (identical
    /// OWASP values in argon2 0.5) is the unreachable fallback.
    pub fn new() -> Self {
        let params =
            Params::new(OWASP_M_COST_KIB, OWASP_T_COST, OWASP_P_COST, None).unwrap_or_default();
        Self {
            argon2: Arc::new(Argon2::new(
                Algorithm::Argon2id,
                argon2::Version::V0x13,
                params,
            )),
            #[cfg(test)]
            verifies: Arc::default(),
        }
    }

    /// Hash a password with Argon2id at the pin, or fail. There is no
    /// fallback value: a failed hash is never written.
    pub fn hash(&self, password: &str) -> Result<String, HashError> {
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

    /// Check `password` against a stored credential. `None` (unknown user,
    /// no local password, unreadable row) runs one dummy verify and answers
    /// [`Verdict::Wrong`], as does an unknown scheme tag or a malformed hash.
    /// Every call runs exactly one verify, real or dummy.
    pub fn verify(&self, password: &str, stored: Option<&StoredPassword>) -> Verdict {
        let Some(stored) = stored else {
            self.dummy_verify();
            return Verdict::Wrong;
        };
        match HashScheme::from_tag(&stored.scheme) {
            Some(HashScheme::Bcrypt) => {
                self.count_verify();
                match bcrypt::verify(password, &stored.hash) {
                    Ok(true) => Verdict::Verified {
                        rehash: self.rehash(password),
                    },
                    Ok(false) => Verdict::Wrong,
                    // A malformed bcrypt hash fails before any work; burn
                    // the dummy so it costs what a wrong password costs.
                    Err(_) => {
                        self.dummy_work();
                        Verdict::Wrong
                    }
                }
            }
            Some(HashScheme::Argon2id) => {
                let Ok(parsed) = PasswordHash::new(&stored.hash) else {
                    self.dummy_verify();
                    return Verdict::Wrong;
                };
                self.count_verify();
                if self
                    .argon2
                    .verify_password(password.as_bytes(), &parsed)
                    .is_err()
                {
                    return Verdict::Wrong;
                }
                Verdict::Verified {
                    rehash: if below_pin(&parsed) {
                        self.rehash(password)
                    } else {
                        None
                    },
                }
            }
            None => {
                self.dummy_verify();
                Verdict::Wrong
            }
        }
    }

    /// A fresh hash for a verified password. A failure here never fails the
    /// login that just succeeded: it logs and the next login tries again.
    fn rehash(&self, password: &str) -> Option<String> {
        match self.hash(password) {
            Ok(hash) => Some(hash),
            Err(error) => {
                tracing::warn!(%error, "login rehash skipped: hash failed");
                None
            }
        }
    }

    /// One verify against the fixed dummy hash, counted as one verify.
    fn dummy_verify(&self) {
        self.count_verify();
        self.dummy_work();
    }

    /// The dummy's hash work: the same code path as a real native verify,
    /// so the cost matches by construction. The result is discarded.
    fn dummy_work(&self) {
        if let Ok(parsed) = PasswordHash::new(DUMMY_HASH) {
            let _ = self
                .argon2
                .verify_password(DUMMY_PASSWORD.as_bytes(), &parsed);
        }
    }

    #[cfg(test)]
    fn count_verify(&self) {
        self.verifies
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(not(test))]
    fn count_verify(&self) {}
}

impl Default for Passwords {
    fn default() -> Self {
        Self::new()
    }
}

/// True when an Argon2 hash is weaker than the pin: another algorithm, a
/// version below 0x13, or less memory or fewer iterations. Parallelism is
/// not compared. Parameters that cannot be read count as weaker.
fn below_pin(hash: &PasswordHash<'_>) -> bool {
    if hash.algorithm != argon2::ARGON2ID_IDENT {
        return true;
    }
    if hash
        .version
        .is_none_or(|version| version < argon2::Version::V0x13 as u32)
    {
        return true;
    }
    match Params::try_from(hash) {
        Ok(params) => params.m_cost() < OWASP_M_COST_KIB || params.t_cost() < OWASP_T_COST,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "correct horse battery staple";

    fn stored(scheme: &str, hash: &str) -> StoredPassword {
        StoredPassword {
            scheme: scheme.to_owned(),
            hash: hash.to_owned(),
        }
    }

    fn verifies(passwords: &Passwords) -> usize {
        passwords.verifies.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn params_match_the_owasp_pin() {
        let passwords = Passwords::new();
        let first = passwords.hash(PASSWORD).unwrap();
        assert!(
            first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{first}"
        );
        // Random salt: two hashes of one password differ, and both verify.
        let second = passwords.hash(PASSWORD).unwrap();
        assert_ne!(first, second);
        for hash in [&first, &second] {
            assert_eq!(
                passwords.verify(PASSWORD, Some(&stored("argon2id", hash))),
                Verdict::Verified { rehash: None },
                "a hash at the pin is never rehashed"
            );
        }
        assert_eq!(
            passwords.verify("wrong password here!", Some(&stored("argon2id", &first))),
            Verdict::Wrong
        );
    }

    #[test]
    fn bcrypt_rows_verify_then_rehash_to_argon2id() {
        let passwords = Passwords::new();
        let legacy = stored("bcrypt", &bcrypt::hash(PASSWORD, 4).unwrap());
        let Verdict::Verified {
            rehash: Some(upgraded),
        } = passwords.verify(PASSWORD, Some(&legacy))
        else {
            panic!("a bcrypt success must rehash");
        };
        assert!(upgraded.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert_eq!(
            passwords.verify(PASSWORD, Some(&stored("argon2id", &upgraded))),
            Verdict::Verified { rehash: None }
        );
        assert_eq!(
            passwords.verify("wrong password here!", Some(&legacy)),
            Verdict::Wrong
        );
    }

    #[test]
    fn scheme_dispatch_never_sniffs() {
        let passwords = Passwords::new();
        let bcrypt_hash = bcrypt::hash(PASSWORD, 4).unwrap();
        let argon_hash = passwords.hash(PASSWORD).unwrap();
        // A hash under the other scheme's tag, an unknown tag, and garbage
        // all fail closed, exactly like a wrong password.
        for (scheme, hash) in [
            ("argon2id", bcrypt_hash.as_str()),
            ("bcrypt", argon_hash.as_str()),
            ("scrypt", argon_hash.as_str()),
            ("argon2id", "garbage"),
            ("", ""),
        ] {
            assert_eq!(
                passwords.verify(PASSWORD, Some(&stored(scheme, hash))),
                Verdict::Wrong,
                "{scheme} {hash}"
            );
        }
    }

    /// No credential, a wrong password and an unknown scheme each cost
    /// exactly one verify, so neither the answer nor its cost names an
    /// account.
    #[test]
    fn every_failure_costs_exactly_one_verify() {
        let argon_hash = Passwords::new().hash(PASSWORD).unwrap();
        for credential in [
            None,
            Some(stored("argon2id", &argon_hash)),
            Some(stored("scrypt", &argon_hash)),
            Some(stored("argon2id", "not-a-hash")),
        ] {
            let passwords = Passwords::new();
            assert_eq!(
                passwords.verify("wrong password here!", credential.as_ref()),
                Verdict::Wrong
            );
            assert_eq!(verifies(&passwords), 1, "{credential:?}");
        }
    }

    #[test]
    fn dummy_vector_regenerates() {
        let passwords = Passwords::new();
        let salt = SaltString::encode_b64(b"dn-dummy-salt-01").unwrap();
        let regenerated = passwords
            .argon2
            .hash_password(DUMMY_PASSWORD.as_bytes(), &salt)
            .unwrap()
            .to_string();
        assert_eq!(regenerated, DUMMY_HASH);
    }
}
