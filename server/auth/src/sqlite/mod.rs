//! The auth repositories over SQLite.
//!
//! Every repository shares a single [`AuthDb`] handle (the read pools plus
//! the writer lane, cloned out of the database runtime):
//!
//! - [`sessions`]: [`Sessions`], every session over `auth_tokens`;
//! - [`users`]: [`Users`], accounts and their local credentials and
//!   sign-in bindings, over `auth_users` and `auth_providers`, and the
//!   sign-in lookup;
//! - [`federated`]: [`FederatedUsers`] over `auth_users` plus the
//!   federated rows of `auth_providers`;
//! - [`app_passwords`] ([`AppPasswords`]), [`recovery`] ([`RecoveryCodes`]),
//!   [`lastfm`] ([`LastFmLinks`]) and [`oidc`]: one each over their
//!   like-named tables.
//!
//! The account, credential, app-password, recovery and Last.fm statements
//! are in [`sql`]; their reads run on the read pool.
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
//!   login lookup and the repositories answer busy, which the routes
//!   render as a 503 with `Retry-After`. The federated store has no busy
//!   variant, so there a busy database is still the generic failure.

mod app_passwords;
mod federated;
mod lastfm;
mod oidc;
mod recovery;
mod sessions;
mod sql;
mod users;

use std::sync::Arc;

use sqlx::SqlitePool;

use super::federated::FederatedError;
use super::federated::password_import::HashScheme;
use super::users::stores::StoreError;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};

pub use app_passwords::AppPasswords;
pub use federated::FederatedUsers;
pub use lastfm::LastFmLinks;
pub use oidc::OidcStates;
pub use recovery::RecoveryCodes;
pub use sessions::{LoginWrite, Sessions, session_expires_at};
pub use users::Users;

use sql::USER_COLUMNS;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![sql::registered()]
}

/// What [`forget_sealed`] cleared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForgottenAuth {
    /// App passwords revoked.
    pub app_passwords: usize,
    /// Federated sign-in token sets dropped.
    pub sign_in_tokens: usize,
}

/// Clear what auth holds sealed under a data key that is gone, inside the
/// caller's transaction. An app password is revoked as well as emptied:
/// the Subsonic token sign-in needs its plaintext, and an active row with
/// an empty secret would accept an empty password. A federated binding
/// keeps its row with no tokens, which is how a pre-linked one looks and
/// is refilled at the next sign-in. Local password rows hold no ciphertext.
pub fn forget_sealed(conn: &rusqlite::Connection) -> rusqlite::Result<ForgottenAuth> {
    let app_passwords = conn.execute(sql::FORGET_APP_PASSWORD_SECRETS.text(), [])?;
    let sign_in_tokens = conn.execute(sql::FORGET_SIGN_IN_TOKENS.text(), [])?;
    Ok(ForgottenAuth {
        app_passwords,
        sign_in_tokens,
    })
}

/// Shared SQLite handle for every auth adapter, with the clock its rows
/// are stamped by. `Clone` is cheap; clones share the database.
#[derive(Clone)]
pub struct AuthDb {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for AuthDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthDb")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl AuthDb {
    /// Handle over the runtime's database and clock.
    pub fn new(db: &Db, clock: Arc<dyn Clock>) -> Self {
        Self {
            db: db.clone(),
            clock,
        }
    }

    /// The sqlx reader pool.
    fn pool(&self) -> &SqlitePool {
        self.db.sqlx()
    }

    /// One read on the request read pool. A busy database stays busy.
    async fn read<R, F>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.db
            .read(name, move |conn| op(conn).map_err(DbError::from))
            .await
            .map_err(store_error)
    }

    /// The writer lane, its closures failing with [`WriteError`].
    fn lane(&self) -> AuthLane<'_> {
        AuthLane(&self.db)
    }

    /// Now, as whole unix seconds.
    fn now_unix(&self) -> i64 {
        self.clock.now_unix()
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

/// What an auth write fails with, inside the lane or inside a caller's
/// own transaction. The transaction rolls back either way.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
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
