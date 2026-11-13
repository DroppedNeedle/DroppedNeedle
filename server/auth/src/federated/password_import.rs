//! v2 password-hash import: bcrypt verifies once, success rehashes to
//! Argon2id, and scheme-tagged rows route every later verify.
//!
//! Import keeps v2 bcrypt hashes verbatim (no forced resets on instances
//! without email). The verifier ([`crate::passwords::Passwords`]) never
//! sniffs formats: each local row carries an explicit scheme tag and
//! dispatches on it. A bcrypt success returns the replacement Argon2id hash,
//! which the login writes with its session.
//!
//! No v2 session reaches v3 ([`SESSIONS_SURVIVE_IMPORT`]): the export
//! carries no sessions and no transient OIDC or Spotify states, so each
//! user logs in once more. Recovery codes and app passwords do carry:
//! recovery code hashes carry over, `secret_sha256` carries over verbatim,
//! and `secret_encrypted` is re-sealed under the v3 key at import. The
//! import never replaces a credential: a user or provider row v3 already
//! holds is kept as it is, so a session v3 already has still belongs to
//! the credential it signed in with.

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
    /// Every scheme, in the order `auth_providers.hash_scheme`'s CHECK lists
    /// them.
    pub const ALL: [Self; 2] = [Self::Bcrypt, Self::Argon2id];

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

/// No v2 session carries: none of `auth_tokens`, `auth_oidc_states` or
/// `spotify_oauth_states` is exported, so users log in once more after the
/// upgrade. Recovery codes and app passwords are exported, and app
/// passwords still verify after import. The import leaves v3's own
/// sessions alone and never changes the credentials they rest on.
pub const SESSIONS_SURVIVE_IMPORT: bool = false;
