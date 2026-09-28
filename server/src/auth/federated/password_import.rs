//! v2 password-hash import: bcrypt verifies once, success rehashes to
//! Argon2id, and scheme-tagged rows route every later verify.
//!
//! Import keeps v2 bcrypt hashes verbatim (no forced resets on instances
//! without email). The verifier never sniffs formats: each local row
//! carries an explicit scheme tag and dispatches on it. A bcrypt success
//! returns the replacement Argon2id row for the caller to persist; an
//! Argon2id success returns no upgrade; unknown users cost one dummy
//! verify so bad-user and bad-password stay indistinguishable.
//!
//! Sessions do NOT survive import ([`SESSIONS_SURVIVE_IMPORT`]): the
//! import pipeline deletes every `auth_tokens` row plus the transient
//! tables (recovery codes, OIDC states, Spotify states), so each user
//! re-logs-in once. App passwords DO survive (owned by the import slice,
//! stage 11): `secret_sha256` carries over verbatim and
//! `secret_encrypted` is re-sealed under the v3 key at import.

/// Hash scheme tag for a local credential row. Imports land as
/// [`HashScheme::Bcrypt`]; every new or rehashed row is
/// [`HashScheme::Argon2id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashScheme {
    /// v2 import, verified with bcrypt, rehashed on success.
    Bcrypt,
    /// v3 native, verified with Argon2id.
    Argon2id,
}

impl HashScheme {
    /// Storage tag (`bcrypt` or `argon2id`).
    pub fn as_tag(&self) -> &'static str {
        match self {
            HashScheme::Bcrypt => "bcrypt",
            HashScheme::Argon2id => "argon2id",
        }
    }

    /// Parse a storage tag; `None` for anything else (never sniffed).
    pub fn from_tag(tag: &str) -> Option<HashScheme> {
        match tag {
            "bcrypt" => Some(HashScheme::Bcrypt),
            "argon2id" => Some(HashScheme::Argon2id),
            _ => None,
        }
    }
}

/// One local credential: explicit scheme plus the stored hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCredential {
    /// Which verifier handles this row.
    pub scheme: HashScheme,
    /// The stored hash string.
    pub hash: String,
}

impl LocalCredential {
    /// A row imported from v2.
    pub fn imported_bcrypt(hash: String) -> LocalCredential {
        LocalCredential {
            scheme: HashScheme::Bcrypt,
            hash,
        }
    }

    /// A native v3 row (new account or completed rehash).
    pub fn argon2id(hash: String) -> LocalCredential {
        LocalCredential {
            scheme: HashScheme::Argon2id,
            hash,
        }
    }
}

/// Password primitives. Synchronous by design: hashing is pure CPU work
/// and the production caller runs it on the blocking pool. The production
/// impl delegates to the sibling `session::login::PasswordVerifier` (new
/// deps at wiring: `bcrypt`, `argon2`); tests use the fake in [`super::fakes`].
pub trait PasswordHasher: Send + Sync + 'static {
    /// Check a password against a v2 bcrypt hash.
    fn verify_bcrypt(&self, password: &str, hash: &str) -> bool;

    /// Check a password against a v3 Argon2id hash.
    fn verify_argon2id(&self, password: &str, hash: &str) -> bool;

    /// Hash a password with Argon2id (OWASP-sane params, fixed at wiring).
    fn hash_argon2id(&self, password: &str) -> String;

    /// Constant-work verify against a dummy hash, for unknown users.
    fn dummy_verify(&self);
}

/// Outcome of a password check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordCheck {
    /// The password verified. `upgraded` carries the replacement row when
    /// the stored scheme was bcrypt; the caller persists it (flipping the
    /// tag) before returning success.
    Valid {
        /// Replacement Argon2id row, or `None` when already native.
        upgraded: Option<LocalCredential>,
    },
    /// Wrong password, corrupt row, or unknown user (after a dummy
    /// verify). No upgrade, no tag change.
    Invalid,
}

/// Verify a password and decide the rehash. `stored` is `None` for
/// unknown users (or local-password-less accounts): one dummy verify,
/// then [`PasswordCheck::Invalid`].
pub fn verify_and_maybe_rehash<H: PasswordHasher>(
    hasher: &H,
    password: &str,
    stored: Option<&LocalCredential>,
) -> PasswordCheck {
    let Some(credential) = stored else {
        hasher.dummy_verify();
        return PasswordCheck::Invalid;
    };
    match credential.scheme {
        HashScheme::Bcrypt => {
            if hasher.verify_bcrypt(password, &credential.hash) {
                PasswordCheck::Valid {
                    upgraded: Some(LocalCredential::argon2id(hasher.hash_argon2id(password))),
                }
            } else {
                PasswordCheck::Invalid
            }
        }
        HashScheme::Argon2id => {
            if hasher.verify_argon2id(password, &credential.hash) {
                PasswordCheck::Valid { upgraded: None }
            } else {
                PasswordCheck::Invalid
            }
        }
    }
}

/// Import drops every session: users re-log-in once after migration.
/// STAGE-11 HANDOFF: production enforcement (the actual clearing) is owned
/// by the stage-11 importer; the fake-scoped brief here only proves the
/// re-login shape. The stage-11 clearing test must prove each of these is
/// empty post-import while app passwords still verify:
/// `auth_tokens`, `auth_password_recovery_codes`, `auth_oidc_states`, and
/// `spotify_oauth_states`.
pub const SESSIONS_SURVIVE_IMPORT: bool = false;
