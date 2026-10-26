//! v2 password-hash import: bcrypt verifies once, success rehashes to
//! Argon2id, and scheme-tagged rows route every later verify.
//!
//! Import keeps v2 bcrypt hashes verbatim (no forced resets on instances
//! without email). The verifier ([`crate::passwords::Passwords`]) never
//! sniffs formats: each local row carries an explicit scheme tag and
//! dispatches on it. A bcrypt success returns the replacement Argon2id hash,
//! which the login writes with its session.
//!
//! Sessions do not survive import ([`SESSIONS_SURVIVE_IMPORT`]): the export
//! carries no sessions and no transient OIDC or Spotify states, so each
//! user logs in once more. Recovery codes and app passwords do survive:
//! recovery code hashes carry over, `secret_sha256` carries over verbatim,
//! and `secret_encrypted` is re-sealed under the v3 key at import.

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

/// Import drops every session: users log in once more after migration.
/// None of `auth_tokens`, `auth_oidc_states` or `spotify_oauth_states` is
/// exported from v2, while recovery codes and app passwords are, and app
/// passwords still verify after import.
pub const SESSIONS_SURVIVE_IMPORT: bool = false;
