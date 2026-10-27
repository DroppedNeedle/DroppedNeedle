//! The session row types and errors of [`Sessions`](crate::sqlite::Sessions),
//! the one repository over `auth_tokens`. It only ever sees hashes: callers
//! hash the presented token before lookup, so raw tokens never reach storage.

use serde::Serialize;
use thiserror::Error;

use crate::users::roles::Role;

/// Session kind, v2 `session_kind` values kept verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// Browser cookie session or Bearer-mode login session; may mint device sessions.
    Standard,
    /// Named companion Bearer token for one trusted device; must never mint (403).
    Companion,
}

/// A standard session to store. The repository gives it its id, expires
/// it [`SESSION_MAX_AGE_SECS`](super::tokens::SESSION_MAX_AGE_SECS) after
/// `issued_at` and counts it as last seen then.
#[derive(Debug, Clone)]
pub struct NewSession {
    /// Owning user id.
    pub user_id: String,
    /// SHA-256 hex of the raw token.
    pub token_hash: String,
    /// Issue time, unix seconds.
    pub issued_at: i64,
    /// Client label from login.
    pub user_agent: Option<String>,
}

/// One row of the token store. `token_hash` is SHA-256 hex, never the raw token.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// Row id (uuid text in the production table).
    pub id: String,
    /// Owning user id.
    pub user_id: String,
    /// SHA-256 hex of the raw token.
    pub token_hash: String,
    /// Standard or companion.
    pub kind: SessionKind,
    /// Companion device label (`None` for standard sessions).
    pub label: Option<String>,
    /// Issue time, unix seconds.
    pub issued_at: i64,
    /// Absolute expiry, unix seconds.
    pub expires_at: i64,
    /// Last use, unix seconds.
    pub last_seen_at: i64,
    /// Revoked sessions never verify again.
    pub revoked: bool,
    /// Client label from login.
    pub user_agent: Option<String>,
}

/// A session that verified, with what its owner's row said in the same read.
#[derive(Debug, Clone)]
pub struct ValidSession {
    /// The token row.
    pub record: SessionRecord,
    /// The owner's login username, when the account has one.
    pub username: Option<String>,
    /// The owner's login as typed at sign-up, else their login, when they
    /// have one. Not the profile's `display_name`.
    pub username_display: Option<String>,
    /// The owner's role. An unknown stored value reads as `user`.
    pub role: Role,
}

/// Storage failure. Transports map `Busy` to a 503 with `Retry-After` and
/// the rest to a 500, without leaking detail.
#[derive(Debug, Error)]
pub enum SessionStoreError {
    /// The backend failed.
    #[error("session store unavailable")]
    Unavailable,
    /// The database is busy: no reader or write turn came in time. Nothing
    /// is wrong with it, and the same call succeeds once the load passes.
    #[error("session store busy")]
    Busy,
    /// A programming error supplied a duplicate hash.
    #[error("token hash already stored")]
    Duplicate,
}
