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
//! - A busy database (a lock, a full lane queue, no free reader in time)
//!   is its own error, never the generic failure: the session store, the
//!   login lookup and the user stores answer busy, which the routes render
//!   as a 503 with `Retry-After`. The federated store has no busy variant,
//!   so there a busy database is still the generic failure.

mod app_passwords;
mod federated;
mod lastfm;
mod oidc;
mod recovery;
mod sessions;
mod users;

use rusqlite::OptionalExtension as _;
use sqlx::SqlitePool;

use super::federated::FederatedError;
use super::federated::password_import::HashScheme;
use super::users::stores::StoreError;
use droppedneedle_kernel::db::{Db, DbError, Lane};

pub use app_passwords::SqliteAppPasswordStore;
pub use federated::SqliteFederatedStore;
pub use lastfm::SqliteLastFmStore;
pub use oidc::SqliteOidcStateStore;
pub use recovery::SqliteRecoveryStore;
pub use sessions::{
    SqliteSessionIssuer, SqliteSessionManager, SqliteSessionStore, session_expires_at,
};
pub use users::{SqliteCredentialLookup, SqliteUserStore};

/// Shared SQLite handle for every auth adapter. `Clone` is cheap; clones
/// share the database.
#[derive(Clone, Debug)]
pub struct AuthDb {
    db: Db,
}

impl AuthDb {
    /// Handle over the runtime's database.
    pub fn new(db: &Db) -> Self {
        Self { db: db.clone() }
    }

    /// The sqlx reader pool.
    fn pool(&self) -> &SqlitePool {
        self.db.sqlx()
    }

    /// The writer lane, its closures failing with [`WriteError`].
    fn lane(&self) -> AuthLane<'_> {
        AuthLane(&self.db)
    }

    /// Current unix time in whole seconds.
    fn now_unix() -> i64 {
        super::session::store::now_unix()
    }
}

/// The writer lane with the auth closure error pinned, so a closure that
/// only uses `?` needs no annotation.
#[derive(Clone, Copy)]
struct AuthLane<'a>(&'a Db);

impl AuthLane<'_> {
    /// [`Db::write`].
    async fn write<R, F>(self, lane: Lane, name: &'static str, op: F) -> Result<R, WriteError>
    where
        F: FnOnce(&rusqlite::Transaction) -> Result<R, WriteError> + Send + 'static,
        R: Send + 'static,
    {
        self.0.write(lane, name, op).await
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

/// Store failure from a lane or pool error: busy stays busy, the rest is
/// [`internal`]. A conflict reaches here only where the caller does not
/// treat it as its own, so it is internal too.
fn store_error(error: impl Into<WriteError>) -> StoreError {
    match error.into() {
        WriteError::Db(DbError::Busy { .. }) => StoreError::Busy,
        error => internal(error),
    }
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

/// What an auth write closure fails with. The transaction rolls back
/// either way.
#[derive(Debug, thiserror::Error)]
enum WriteError {
    /// A constraint refused the write: the caller's domain conflict.
    #[error("conflicting state")]
    Conflict,
    /// Any other failure, the lane's own included.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for WriteError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(error.into())
    }
}

/// Convert a rusqlite failure inside a write closure: constraint violations
/// become [`WriteError::Conflict`], everything else a database error.
fn op_error(error: rusqlite::Error) -> WriteError {
    if is_constraint(&error) {
        WriteError::Conflict
    } else {
        error.into()
    }
}

/// True when a write failed on a constraint.
fn is_write_conflict(error: &WriteError) -> bool {
    matches!(error, WriteError::Conflict)
}

/// True when a row with `id` exists in `table`. Used after updates that may
/// legitimately change nothing, so an unchanged row still reads as found.
/// The table name is always a literal at the call site, never caller input.
fn row_exists(tx: &rusqlite::Transaction, table: &str, id: &str) -> Result<bool, WriteError> {
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
