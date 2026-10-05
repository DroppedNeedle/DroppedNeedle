//! Production SQLite adapters over the auth tables.
//!
//! One adapter per auth port, all sharing a single [`AuthDb`] handle (the
//! reader pool plus the writer lane, cloned out of the database runtime):
//!
//! - [`sessions`]: [`SqliteSessionStore`] (logins and the middleware),
//!   [`SqliteSessionManager`] (the session-list backend) and
//!   [`SqliteSessionIssuer`] (federated logins), over `auth_tokens`;
//! - [`users`]: [`SqliteUserStore`] and [`SqliteCredentialLookup`] over
//!   `auth_users` plus the `local` rows of `auth_providers`;
//! - [`federated`]: [`SqliteFederatedStore`] over `auth_users` plus the
//!   federated rows of `auth_providers`;
//! - [`app_passwords`], [`recovery`], [`lastfm`], [`oidc`]: one adapter
//!   each over their like-named tables.
//!
//! Conventions shared by every adapter:
//!
//! - Times are ISO-8601 TEXT on disk ([`super::times`]), unix seconds on the
//!   ports. `expires_at` values that fail to parse are treated as expired
//!   (fail closed); display and bookkeeping times that fail to parse degrade
//!   to 0 (or `None` for optional fields). Auth decisions depend only on
//!   expiry, revocation, and hashes, never on display times.
//! - The local `provider_data` JSON is
//!   `{"password_hash": ..., "scheme": "bcrypt"|"argon2id"}`. A missing
//!   `scheme` means `bcrypt` (v2 rows predate the tag); corrupt JSON or a
//!   missing hash reads as no credential, matching v2's verify-false shape.
//! - Federated `provider_data` is `Crypto` ciphertext of the token JSON,
//!   sealed on every write and never decrypted on read (no port reads it).
//! - Lock contention surfaces as the port's generic failure, not a retryable
//!   busy: the auth error types have no busy variant, so a busy lane reads
//!   as a 500 rather than a 503.

mod app_passwords;
mod federated;
mod lastfm;
mod oidc;
mod recovery;
mod sessions;
mod users;

use std::sync::Arc;

use rusqlite::OptionalExtension as _;
use sqlx::SqlitePool;

use super::federated::FederatedError;
use super::federated::password_import::HashScheme;
use super::users::stores::StoreError;
use crate::db::WriteLane;

pub use app_passwords::SqliteAppPasswordStore;
pub use federated::SqliteFederatedStore;
pub use lastfm::SqliteLastFmStore;
pub use oidc::SqliteOidcStateStore;
pub use recovery::SqliteRecoveryStore;
pub use sessions::{
    SqliteSessionIssuer, SqliteSessionManager, SqliteSessionStore, session_expires_at,
};
pub use users::{SqliteCredentialLookup, SqliteUserStore};

/// Shared SQLite handle for every auth adapter: the reader pool plus the
/// writer lane. `Clone` is cheap; clones share both.
#[derive(Clone, Debug, Default)]
pub struct AuthDb {
    inner: Option<Arc<AuthDbInner>>,
}

#[derive(Debug)]
struct AuthDbInner {
    pool: SqlitePool,
    lane: WriteLane,
}

impl AuthDb {
    /// Live handle over the runtime's pool and lane.
    pub fn new(pool: &SqlitePool, lane: &WriteLane) -> Self {
        Self {
            inner: Some(Arc::new(AuthDbInner {
                pool: pool.clone(),
                lane: lane.clone(),
            })),
        }
    }

    /// Test handle with no database: every adapter operation fails closed.
    #[cfg(any(test, feature = "test-support"))]
    pub fn unwired() -> Self {
        Self::default()
    }

    fn live(&self) -> Option<(&SqlitePool, &WriteLane)> {
        self.inner.as_ref().map(|inner| (&inner.pool, &inner.lane))
    }

    /// Current unix time in whole seconds.
    fn now_unix() -> i64 {
        super::session::store::now_unix()
    }
}

/// True for SQLite constraint violations (unique, foreign key, check): the
/// caller's domain conflict, never a 500 on its own.
fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ffi::ErrorCode::ConstraintViolation
    )
}

/// Log-safe store failure from a lane or pool error. Messages name the
/// operation, never SQL text or row contents.
fn internal(error: impl std::fmt::Display) -> StoreError {
    StoreError::Internal(error.to_string())
}

/// Log-safe federated failure, same rule as [`internal`].
fn store_unavailable(error: impl std::fmt::Display) -> FederatedError {
    FederatedError::StoreUnavailable(error.to_string())
}

/// Parse a local `provider_data` document into `(scheme, hash)`. Missing
/// `scheme` defaults to `bcrypt`; corrupt documents read as absent.
fn parse_local_data(provider_data: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(provider_data).ok()?;
    let hash = value.get("password_hash")?.as_str()?.to_owned();
    let scheme = value
        .get("scheme")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(HashScheme::Bcrypt.as_tag())
        .to_owned();
    Some((scheme, hash))
}

/// Render a local `provider_data` document. Built with serde, never string
/// concatenation, so hashes with quotes stay intact.
fn render_local_data(scheme: &str, hash: &str) -> String {
    serde_json::json!({ "password_hash": hash, "scheme": scheme }).to_string()
}

/// Columns read for every user row, in mapping order.
const USER_COLUMNS: &str = "id, display_name, email, avatar_url, role, created_at, \
    last_login_at, username, username_display";

/// Marker a write closure aborts with when the failure is a domain conflict.
/// The lane renders SQL errors as message text, so closures classify the
/// typed error first (see [`op_error`]) instead of sniffing strings later.
const CONFLICT_ABORT: &str = "conflicting state";

/// Convert a rusqlite failure inside a write closure: constraint violations
/// become the conflict marker, everything else stays a SQL error.
fn op_error(error: rusqlite::Error) -> crate::db::OpError {
    if is_constraint(&error) {
        crate::db::OpError::Abort(CONFLICT_ABORT.to_owned())
    } else {
        crate::db::OpError::Sql(error)
    }
}

/// True when a lane failure carries the conflict marker.
fn is_write_conflict(error: &crate::db::DbError) -> bool {
    matches!(
        error,
        crate::db::DbError::WriteFailed { cause, .. } if cause == CONFLICT_ABORT
    )
}

/// True when a row with `id` exists in `table`. Used after updates that may
/// legitimately change nothing, so an unchanged row still reads as found.
/// The table name is always a literal at the call site, never caller input.
fn row_exists(
    tx: &rusqlite::Transaction,
    table: &str,
    id: &str,
) -> Result<bool, crate::db::OpError> {
    let found: Option<i64> = tx
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE id = ?"),
            rusqlite::params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(op_error)?;
    Ok(found.is_some())
}
