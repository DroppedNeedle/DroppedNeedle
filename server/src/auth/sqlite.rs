//! Production SQLite adapters over the 0001 baseline tables.
//!
//! One adapter per auth port, all sharing a single [`AuthDb`] handle (the
//! reader pool plus the writer lane, cloned out of the database runtime):
//!
//! - [`SqliteSessionStore`] (session logins + middleware) and
//!   [`SqliteSessionManager`] (the session-list UI backend) over `auth_tokens`;
//! - [`SqliteUserStore`] over `auth_users` plus the `local` rows of
//!   `auth_providers`;
//! - [`SqliteFederatedStore`] over `auth_users` plus the federated rows of
//!   `auth_providers`;
//! - [`SqliteAppPasswordStore`] over `connect_app_passwords`;
//! - [`SqliteRecoveryStore`], [`SqliteOidcStateStore`], [`SqliteLastFmStore`]
//!   over their like-named tables;
//! - [`SqliteSessionIssuer`] and [`SqliteCredentialLookup`] closing the
//!   federated and native login loops;
//! - [`FileAvatarStore`] for avatar bytes under a configured directory.
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
//! - [`AuthDb::unwired`] is the skeleton-boot handle: every operation fails
//!   closed (`Unavailable` / `Internal` / `StoreUnavailable`, lookups read as
//!   absent). Production opens [`AuthDb::new`] from the live runtime.
//! - Lock contention surfaces as the port's generic failure, not a retryable
//!   busy: the auth error types have no busy variant, so a busy lane reads
//!   as a 500 rather than a 503.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::OptionalExtension as _;
use sqlx::{Row as _, SqlitePool};

use super::federated::password_import::HashScheme;
use super::federated::users::{FederatedUserStore, NewFederatedUser, ProviderBinding, StoredUser};
use super::federated::{FederatedError, SessionIssuer};
use super::passwords::{PendingRehash, RehashQueue};
use super::session::login::CredentialLookup;
use super::session::store::{SessionKind, SessionRecord, SessionStore, SessionStoreError};
use super::session::tokens::{constant_time_eq, expires_at};
use super::times::{parse_iso, to_iso};
use super::users::models::{
    AppPasswordRecord, LastFmConnection, LocalCredential as UsersLocalCredential, ManagedSession,
    RecoveryCode, SessionOwner, UserRecord,
};
use super::users::roles::Role;
use super::users::services::COMPANION_LABEL_PREFIX;
use super::users::stores::{
    AppPasswordStore, AvatarStore, BoxFuture, Clock, LastFmStore, LoadedAvatar, RecoveryStore,
    SessionManager, StoreError, UserStore,
};
use crate::db::{Lane, WriteLane, map_sqlx_busy};
use crate::ids::IdGenerator;
use crate::runtime_config::crypto::Crypto;

/// How stale `last_seen_at` must be before a lookup rewrites it (5 minutes).
const LAST_SEEN_TOUCH_SECS: i64 = 5 * 60;

/// Last.fm rows in `user_connections` carry this service tag.
const LASTFM_SERVICE: &str = "lastfm";

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

    /// Skeleton-boot handle: every adapter operation fails closed. Only
    /// `AppState::new` (pre-boot) builds this; production uses [`AuthDb::new`].
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

// ---------------------------------------------------------------------------
// Sessions over auth_tokens
// ---------------------------------------------------------------------------

/// Session storage for logins and the middleware, over `auth_tokens`.
///
/// Successful lookups refresh `last_seen_at` when it is older than
/// [`LAST_SEEN_TOUCH_SECS`]; the refresh is best-effort (a failed touch logs
/// and the request still authenticates). Companion labels round-trip through
/// the `user_agent` column with the shared `DroppedNeedle companion` prefix.
/// Every insert also drains the rehash queue: a bcrypt login queued its
/// Argon2id upgrade at verify time, and the matching entry persists here in
/// the same transaction as the session row.
#[derive(Clone, Debug)]
pub struct SqliteSessionStore {
    db: AuthDb,
    rehash: RehashQueue,
}

impl SqliteSessionStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self {
            db: db.clone(),
            rehash: RehashQueue::default(),
        }
    }

    /// Share the production hasher's rehash queue with this store, so login
    /// inserts persist bcrypt upgrades. An unshared store keeps its own
    /// empty queue and inserts exactly as before.
    pub fn set_rehash_queue(&mut self, queue: RehashQueue) {
        self.rehash = queue;
    }
}

impl SessionStore for SqliteSessionStore {
    async fn insert(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(SessionStoreError::Unavailable);
        };
        let stored_agent = match (&record.kind, &record.label) {
            (SessionKind::Companion, Some(label)) => {
                Some(format!("{COMPANION_LABEL_PREFIX}{label}"))
            }
            _ => record.user_agent.clone(),
        };
        let kind = match record.kind {
            SessionKind::Standard => "standard",
            SessionKind::Companion => "companion",
        };
        let pending = self.rehash.drain();
        // A failed transaction drops what it was given, so keep a copy for
        // the requeue below. Almost always an empty vec (legacy logins only).
        let retry = pending.clone();
        let outcome: Result<Vec<PendingRehash>, crate::db::DbError> = lane
            .write(Lane::Foreground, "auth.session.insert", move |tx| {
                tx.execute(
                    "INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at, \
                         last_seen_at, revoked, user_agent, session_kind) \
                         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)",
                    rusqlite::params![
                        record.id,
                        record.user_id,
                        record.token_hash,
                        to_iso(record.issued_at),
                        to_iso(record.expires_at),
                        to_iso(record.last_seen_at),
                        stored_agent,
                        kind,
                    ],
                )
                .map_err(op_error)?;
                apply_pending_rehash(tx, &record.user_id, pending)
            })
            .await;
        match outcome {
            Ok(unmatched) => {
                self.rehash.requeue(unmatched);
                Ok(())
            }
            Err(error) if is_write_conflict(&error) => {
                self.rehash.requeue(retry);
                Err(SessionStoreError::Duplicate)
            }
            Err(_) => {
                self.rehash.requeue(retry);
                Err(SessionStoreError::Unavailable)
            }
        }
    }

    async fn lookup_valid(
        &self,
        token_hash: &str,
        now_unix: i64,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        let Some((pool, lane)) = self.db.live() else {
            return Err(SessionStoreError::Unavailable);
        };
        let row = sqlx::query(
            "SELECT id, user_id, token_hash, session_kind, user_agent, issued_at, expires_at, \
             last_seen_at, revoked FROM auth_tokens WHERE token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(pool)
        .await
        .map_err(|_| SessionStoreError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let stored: String = row.get("token_hash");
        if !constant_time_eq(&stored, token_hash) {
            return Ok(None);
        }
        if row.get::<i64, _>("revoked") != 0 {
            return Ok(None);
        }
        let expires_raw: String = row.get("expires_at");
        if parse_iso(&expires_raw).is_none_or(|at| at <= now_unix) {
            return Ok(None);
        }
        let kind = match row.get::<String, _>("session_kind").as_str() {
            "standard" => SessionKind::Standard,
            "companion" => SessionKind::Companion,
            // The kind gates companion minting, so an unknown value rejects
            // the session instead of guessing a privilege level.
            _ => return Ok(None),
        };
        let issued_raw: String = row.get("issued_at");
        let seen_raw: String = row.get("last_seen_at");
        let last_seen_at = parse_iso(&seen_raw).unwrap_or(0);
        let user_agent: Option<String> = row.get("user_agent");
        let label = if kind == SessionKind::Companion {
            user_agent
                .as_deref()
                .and_then(|agent| agent.strip_prefix(COMPANION_LABEL_PREFIX))
                .map(str::to_owned)
        } else {
            None
        };
        if now_unix - last_seen_at >= LAST_SEEN_TOUCH_SECS {
            let id: String = row.get("id");
            let touched = to_iso(now_unix);
            if let Err(error) = lane
                .write(Lane::Foreground, "auth.session.touch", move |tx| {
                    tx.execute(
                        "UPDATE auth_tokens SET last_seen_at = ? WHERE id = ?",
                        rusqlite::params![touched, id],
                    )?;
                    Ok(())
                })
                .await
            {
                tracing::warn!(%error, "session last_seen touch failed");
            }
        }
        Ok(Some(SessionRecord {
            id: row.get("id"),
            user_id: row.get("user_id"),
            token_hash: stored,
            kind,
            label,
            issued_at: parse_iso(&issued_raw).unwrap_or(0),
            expires_at: parse_iso(&expires_raw).unwrap_or(0),
            last_seen_at,
            revoked: false,
            user_agent,
        }))
    }
}

/// Persist the queued bcrypt upgrade for `user_id`, if any entry matches
/// the current local row. Runs inside the session-insert transaction; the
/// guard (scheme still `bcrypt`, hash still the verified one) makes a stale
/// entry a no-op instead of a wrong write. Returns the entries that did not
/// match (other users' upgrades, or a row that already moved on).
fn apply_pending_rehash(
    tx: &rusqlite::Transaction,
    user_id: &str,
    pending: Vec<PendingRehash>,
) -> Result<Vec<PendingRehash>, crate::db::OpError> {
    if pending.is_empty() {
        return Ok(pending);
    }
    let current: Option<Option<String>> = tx
        .query_row(
            "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
            rusqlite::params![user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(op_error)?;
    let Some(Some(data)) = current else {
        return Ok(pending);
    };
    let Some((scheme, hash)) = parse_local_data(&data) else {
        return Ok(pending);
    };
    if scheme != HashScheme::Bcrypt.as_tag() {
        return Ok(pending);
    }
    let mut unmatched = Vec::with_capacity(pending.len());
    for entry in pending {
        if entry.old_hash == hash {
            tx.execute(
                "UPDATE auth_providers SET provider_data = ? \
                 WHERE user_id = ? AND provider = 'local'",
                rusqlite::params![
                    render_local_data(HashScheme::Argon2id.as_tag(), &entry.new_hash),
                    user_id
                ],
            )
            .map_err(op_error)?;
        } else {
            unmatched.push(entry);
        }
    }
    Ok(unmatched)
}

/// Absolute expiry for a session issued now (30 days, the session rule).
pub fn session_expires_at(now_unix: i64) -> i64 {
    expires_at(now_unix)
}

/// Session management (the session-list UI backend) over `auth_tokens`.
///
/// Shares the table with [`SqliteSessionStore`]: listings expose no token
/// hashes, and the companion replace revokes the prior same-label token in
/// the same transaction as the insert.
#[derive(Clone)]
pub struct SqliteSessionManager {
    db: AuthDb,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for SqliteSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSessionManager")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl SqliteSessionManager {
    /// Adapter over one handle, reading expiry from `clock`.
    pub fn new(db: &AuthDb, clock: Arc<dyn Clock>) -> Self {
        Self {
            db: db.clone(),
            clock,
        }
    }
}

impl SessionManager for SqliteSessionManager {
    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ManagedSession>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth session store is not wired"));
            };
            let rows = sqlx::query(
                "SELECT id, user_id, session_kind, user_agent, issued_at, last_seen_at, \
                 expires_at FROM auth_tokens WHERE user_id = ? AND revoked = 0",
            )
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.sessions.list", error)))?;
            let now = self.clock.now_unix();
            let mut sessions: Vec<ManagedSession> = rows
                .into_iter()
                .filter_map(|row| {
                    let expires_raw: String = row.get("expires_at");
                    let expires_at = parse_iso(&expires_raw)?;
                    if expires_at <= now {
                        return None;
                    }
                    let kind = match row.get::<String, _>("session_kind").as_str() {
                        "companion" => SessionKind::Companion,
                        "standard" => SessionKind::Standard,
                        _ => return None,
                    };
                    let issued_raw: String = row.get("issued_at");
                    let seen_raw: String = row.get("last_seen_at");
                    let agent: Option<String> = row.get("user_agent");
                    Some(ManagedSession {
                        id: row.get("id"),
                        user_id: row.get("user_id"),
                        kind,
                        label: agent.unwrap_or_default(),
                        created_at: parse_iso(&issued_raw).unwrap_or(0),
                        last_seen_at: parse_iso(&seen_raw).unwrap_or(0),
                        expires_at,
                    })
                })
                .collect();
            sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
            Ok(sessions)
        })
    }

    fn revoke_scoped<'a>(
        &'a self,
        user_id: &'a str,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth session store is not wired"));
            };
            let (user_id, session_id) = (user_id.to_owned(), session_id.to_owned());
            let changed: usize = lane
                .write(Lane::Foreground, "auth.sessions.revoke", move |tx| {
                    Ok(tx.execute(
                        "UPDATE auth_tokens SET revoked = 1 \
                         WHERE id = ? AND user_id = ? AND revoked = 0",
                        rusqlite::params![session_id, user_id],
                    )?)
                })
                .await
                .map_err(internal)?;
            Ok(changed > 0)
        })
    }

    fn revoke_all_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth session store is not wired"));
            };
            let user_id = user_id.to_owned();
            let changed: usize = lane
                .write(Lane::Foreground, "auth.sessions.revoke_all", move |tx| {
                    Ok(tx.execute(
                        "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ? AND revoked = 0",
                        rusqlite::params![user_id],
                    )?)
                })
                .await
                .map_err(internal)?;
            Ok(changed as u64)
        })
    }

    fn replace_companion<'a>(
        &'a self,
        id: &'a str,
        user_id: &'a str,
        token_hash: &'a str,
        label: &'a str,
        issued_at: i64,
        expires_at: i64,
    ) -> BoxFuture<'a, Result<ManagedSession, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth session store is not wired"));
            };
            let agent = format!("{COMPANION_LABEL_PREFIX}{label}");
            let (id, user_id, token_hash) =
                (id.to_owned(), user_id.to_owned(), token_hash.to_owned());
            let (cid, cuser, chash, cagent) = (
                id.clone(),
                user_id.clone(),
                token_hash.clone(),
                agent.clone(),
            );
            let outcome: Result<(), crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.sessions.replace", move |tx| {
                    tx.execute(
                        "INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at, \
                         last_seen_at, revoked, user_agent, session_kind) \
                         VALUES (?, ?, ?, ?, ?, ?, 0, ?, 'companion')",
                        rusqlite::params![
                            cid,
                            cuser,
                            chash,
                            to_iso(issued_at),
                            to_iso(expires_at),
                            to_iso(issued_at),
                            cagent,
                        ],
                    )
                    .map_err(op_error)?;
                    // Same transaction: the replace is atomic.
                    tx.execute(
                        "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ? \
                         AND user_agent = ? AND session_kind = 'companion' \
                         AND revoked = 0 AND id != ?",
                        rusqlite::params![cuser, cagent, cid],
                    )
                    .map_err(op_error)?;
                    Ok(())
                })
                .await;
            match outcome {
                Ok(()) => Ok(ManagedSession {
                    id,
                    user_id,
                    kind: SessionKind::Companion,
                    label: format!("{COMPANION_LABEL_PREFIX}{label}"),
                    created_at: issued_at,
                    last_seen_at: issued_at,
                    expires_at,
                }),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn owner_by_hash<'a>(
        &'a self,
        token_hash: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionOwner>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth session store is not wired"));
            };
            let row = sqlx::query(
                "SELECT id, user_id, session_kind, expires_at FROM auth_tokens \
                 WHERE token_hash = ? AND revoked = 0",
            )
            .bind(token_hash)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.sessions.owner", error)))?;
            let Some(row) = row else {
                return Ok(None);
            };
            let expires_raw: String = row.get("expires_at");
            if parse_iso(&expires_raw).is_none_or(|at| at <= self.clock.now_unix()) {
                return Ok(None);
            }
            let kind = match row.get::<String, _>("session_kind").as_str() {
                "companion" => SessionKind::Companion,
                "standard" => SessionKind::Standard,
                _ => return Ok(None),
            };
            Ok(Some(SessionOwner {
                session_id: row.get("id"),
                user_id: row.get("user_id"),
                kind,
            }))
        })
    }
}

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

// ---------------------------------------------------------------------------
// Users over auth_users + the local auth_providers rows
// ---------------------------------------------------------------------------

/// Account and credential rows over `auth_users` plus the `local` rows of
/// `auth_providers`.
///
/// The local credential lives in `provider_data` as
/// `{"password_hash": ..., "scheme": ...}` (see [`parse_local_data`]); there
/// is no password column and none is needed. Renames sync the local
/// `provider_uid` in the same transaction; deletes cascade through the
/// baseline foreign keys.
#[derive(Clone, Debug)]
pub struct SqliteUserStore {
    db: AuthDb,
}

impl SqliteUserStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

/// Columns read for every user row, in mapping order.
const USER_COLUMNS: &str = "id, display_name, email, avatar_url, role, created_at, \
    last_login_at, username, username_display";

/// Map one `auth_users` row. Unknown roles degrade to least privilege;
/// unparseable display times degrade to 0.
fn map_user(row: &sqlx::sqlite::SqliteRow) -> UserRecord {
    let created_raw: String = row.get("created_at");
    let login_raw: Option<String> = row.get("last_login_at");
    let role_raw: String = row.get("role");
    UserRecord {
        id: row.get("id"),
        username: row.get("username"),
        username_display: row.get("username_display"),
        display_name: row.get("display_name"),
        email: row.get("email"),
        avatar_url: row.get("avatar_url"),
        role: Role::from_stored(&role_raw),
        created_at: parse_iso(&created_raw).unwrap_or(0),
        last_login_at: login_raw.as_deref().and_then(parse_iso),
    }
}

impl UserStore for SqliteUserStore {
    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users WHERE id = ?"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(row.as_ref().map(map_user))
        })
    }

    fn get_by_username<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users WHERE username = ?"
            ))
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(row.as_ref().map(map_user))
        })
    }

    fn get_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users WHERE email = ?"
            ))
            .bind(email)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(row.as_ref().map(map_user))
        })
    }

    fn get_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<UserRecord>, StoreError>> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let placeholders = vec!["?"; ids.len()].join(", ");
            let sql = format!("SELECT {USER_COLUMNS} FROM auth_users WHERE id IN ({placeholders})");
            let mut query = sqlx::query(&sql);
            for id in ids {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(rows.iter().map(map_user).collect())
        })
    }

    fn insert<'a>(&'a self, user: UserRecord) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let outcome: Result<(), crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.insert", move |tx| {
                    tx.execute(
                        "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
                         created_at, last_login_at, username, username_display) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        rusqlite::params![
                            user.id,
                            user.display_name,
                            user.email,
                            user.avatar_url,
                            user.role.as_str(),
                            to_iso(user.created_at),
                            user.last_login_at.map(to_iso),
                            user.username,
                            user.username_display,
                        ],
                    )
                    .map_err(op_error)?;
                    Ok(())
                })
                .await;
            match outcome {
                Ok(()) => Ok(()),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn update_profile<'a>(
        &'a self,
        id: &'a str,
        display_name: Option<&'a str>,
        avatar_url: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, display_name, avatar_url) = (
                id.to_owned(),
                display_name.map(str::to_owned),
                avatar_url.map(str::to_owned),
            );
            lane.write(Lane::Foreground, "auth.users.profile", move |tx| {
                tx.execute(
                    "UPDATE auth_users SET display_name = COALESCE(?, display_name), \
                     avatar_url = COALESCE(?, avatar_url) WHERE id = ?",
                    rusqlite::params![display_name, avatar_url, id],
                )
                .map_err(op_error)?;
                row_exists(tx, "auth_users", &id)
            })
            .await
            .map_err(internal)
        })
    }

    fn update_username<'a>(
        &'a self,
        id: &'a str,
        username: &'a str,
        username_display: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, username, username_display) = (
                id.to_owned(),
                username.to_owned(),
                username_display.to_owned(),
            );
            let outcome: Result<bool, crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.rename", move |tx| {
                    tx.execute(
                        "UPDATE auth_users SET username = ?, username_display = ? WHERE id = ?",
                        rusqlite::params![username, username_display, id],
                    )
                    .map_err(op_error)?;
                    // Same transaction: the local binding tracks the rename.
                    tx.execute(
                        "UPDATE auth_providers SET provider_uid = ? \
                         WHERE user_id = ? AND provider = 'local'",
                        rusqlite::params![username, id],
                    )
                    .map_err(op_error)?;
                    row_exists(tx, "auth_users", &id)
                })
                .await;
            match outcome {
                Ok(exists) => Ok(exists),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn update_email<'a>(
        &'a self,
        id: &'a str,
        email: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, email) = (id.to_owned(), email.map(str::to_owned));
            let outcome: Result<bool, crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.email", move |tx| {
                    tx.execute(
                        "UPDATE auth_users SET email = ? WHERE id = ?",
                        rusqlite::params![email, id],
                    )
                    .map_err(op_error)?;
                    row_exists(tx, "auth_users", &id)
                })
                .await;
            match outcome {
                Ok(exists) => Ok(exists),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn set_role<'a>(&'a self, id: &'a str, role: Role) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let id = id.to_owned();
            lane.write(Lane::Foreground, "auth.users.role", move |tx| {
                tx.execute(
                    "UPDATE auth_users SET role = ? WHERE id = ?",
                    rusqlite::params![role.as_str(), id],
                )
                .map_err(op_error)?;
                row_exists(tx, "auth_users", &id)
            })
            .await
            .map_err(internal)
        })
    }

    fn touch_login<'a>(&'a self, id: &'a str, at: i64) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let id = id.to_owned();
            lane.write(Lane::Foreground, "auth.users.touch", move |tx| {
                tx.execute(
                    "UPDATE auth_users SET last_login_at = ? WHERE id = ?",
                    rusqlite::params![to_iso(at), id],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }

    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let id = id.to_owned();
            let changed: usize = lane
                .write(Lane::Foreground, "auth.users.delete", move |tx| {
                    tx.execute("DELETE FROM auth_users WHERE id = ?", rusqlite::params![id])
                        .map_err(op_error)
                })
                .await
                .map_err(internal)?;
            Ok(changed > 0)
        })
    }

    fn list<'a>(
        &'a self,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<UserRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let rows = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users ORDER BY created_at ASC, id ASC \
                 LIMIT ? OFFSET ?"
            ))
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.list", error)))?;
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_users")
                .fetch_one(pool)
                .await
                .map_err(|error| internal(map_sqlx_busy("auth.users.list", error)))?;
            Ok((rows.iter().map(map_user).collect(), total.max(0) as u64))
        })
    }

    fn count_by_role<'a>(&'a self, role: Role) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_users WHERE role = ?")
                .bind(role.as_str())
                .fetch_one(pool)
                .await
                .map_err(|error| internal(map_sqlx_busy("auth.users.count", error)))?;
            Ok(total.max(0) as u64)
        })
    }

    fn provider_names<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<String>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT provider FROM auth_providers WHERE user_id = ? \
                 ORDER BY provider ASC",
            )
            .bind(id)
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.providers", error)))?;
            Ok(rows)
        })
    }

    fn local_credential<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<UsersLocalCredential>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(
                "SELECT id, provider_data FROM auth_providers \
                 WHERE user_id = ? AND provider = 'local'",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.credential", error)))?;
            let Some(row) = row else {
                return Ok(None);
            };
            let data: Option<String> = row.get("provider_data");
            let Some(data) = data else {
                return Ok(None);
            };
            let Some((scheme, hash)) = parse_local_data(&data) else {
                return Ok(None);
            };
            Ok(Some(UsersLocalCredential {
                id: row.get("id"),
                user_id: id.to_owned(),
                scheme,
                hash,
            }))
        })
    }

    fn insert_local_credential<'a>(
        &'a self,
        credential: UsersLocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let outcome: Result<(), crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.credential", move |tx| {
                    let username: Option<String> = tx
                        .query_row(
                            "SELECT username FROM auth_users WHERE id = ?",
                            rusqlite::params![credential.user_id],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(op_error)?
                        .flatten();
                    let Some(username) = username else {
                        return Err(crate::db::OpError::Abort(
                            "cannot set a local password without a username".to_owned(),
                        ));
                    };
                    tx.execute(
                        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
                         provider_data, created_at) VALUES (?, ?, 'local', ?, ?, ?)",
                        rusqlite::params![
                            credential.id,
                            credential.user_id,
                            username,
                            render_local_data(&credential.scheme, &credential.hash),
                            to_iso(AuthDb::now_unix()),
                        ],
                    )
                    .map_err(op_error)?;
                    Ok(())
                })
                .await;
            match outcome {
                Ok(()) => Ok(()),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn replace_local_hash<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, expected_hash, scheme, new_hash) = (
                id.to_owned(),
                expected_hash.to_owned(),
                scheme.to_owned(),
                new_hash.to_owned(),
            );
            lane.write(Lane::Foreground, "auth.users.rehash", move |tx| {
                let current: Option<Option<String>> = tx
                    .query_row(
                        "SELECT provider_data FROM auth_providers \
                         WHERE user_id = ? AND provider = 'local'",
                        rusqlite::params![id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(op_error)?;
                let Some(Some(data)) = current else {
                    return Ok(false);
                };
                let Some((_, hash)) = parse_local_data(&data) else {
                    return Ok(false);
                };
                if hash != expected_hash {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE auth_providers SET provider_data = ? \
                     WHERE user_id = ? AND provider = 'local'",
                    rusqlite::params![render_local_data(&scheme, &new_hash), id],
                )
                .map_err(op_error)?;
                Ok(true)
            })
            .await
            .map_err(internal)
        })
    }

    fn complete_recovery_reset<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, expected_hash, scheme, new_hash) = (
                id.to_owned(),
                expected_hash.to_owned(),
                scheme.to_owned(),
                new_hash.to_owned(),
            );
            lane.write(Lane::Foreground, "auth.users.recovery_reset", move |tx| {
                let current: Option<Option<String>> = tx
                    .query_row(
                        "SELECT provider_data FROM auth_providers \
                         WHERE user_id = ? AND provider = 'local'",
                        rusqlite::params![id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(op_error)?;
                let Some(Some(data)) = current else {
                    return Ok(false);
                };
                let Some((_, hash)) = parse_local_data(&data) else {
                    return Ok(false);
                };
                if hash != expected_hash {
                    return Ok(false);
                }
                // One transaction: the new hash, no live sessions, no live
                // code. A crash mid-reset retries cleanly (same guard).
                tx.execute(
                    "UPDATE auth_providers SET provider_data = ? \
                     WHERE user_id = ? AND provider = 'local'",
                    rusqlite::params![render_local_data(&scheme, &new_hash), id],
                )
                .map_err(op_error)?;
                tx.execute(
                    "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ?",
                    rusqlite::params![id],
                )
                .map_err(op_error)?;
                tx.execute(
                    "DELETE FROM auth_password_recovery_codes WHERE user_id = ?",
                    rusqlite::params![id],
                )
                .map_err(op_error)?;
                Ok(true)
            })
            .await
            .map_err(internal)
        })
    }

    fn get_provider_binding<'a>(
        &'a self,
        provider: &'a str,
        provider_uid: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderBinding>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(
                "SELECT id, user_id, provider, provider_uid FROM auth_providers \
                 WHERE provider = ? AND provider_uid = ?",
            )
            .bind(provider)
            .bind(provider_uid)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.binding", error)))?;
            Ok(row.map(|row| ProviderBinding {
                id: row.get("id"),
                user_id: row.get("user_id"),
                provider: row.get("provider"),
                provider_uid: row.get("provider_uid"),
            }))
        })
    }

    fn insert_provider_binding<'a>(
        &'a self,
        binding: ProviderBinding,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let outcome: Result<(), crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.bind", move |tx| {
                    tx.execute(
                        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
                         provider_data, created_at) VALUES (?, ?, ?, ?, NULL, ?)",
                        rusqlite::params![
                            binding.id,
                            binding.user_id,
                            binding.provider,
                            binding.provider_uid,
                            to_iso(AuthDb::now_unix()),
                        ],
                    )
                    .map_err(op_error)?;
                    Ok(())
                })
                .await;
            match outcome {
                Ok(()) => Ok(()),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }
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

// ---------------------------------------------------------------------------
// Federated import over auth_users + auth_providers
// ---------------------------------------------------------------------------

/// Federated user import over `auth_users` plus the non-local rows of
/// `auth_providers`.
///
/// Every `token_json` is sealed with the deployment key before it reaches
/// `provider_data`, on both insert and rotation. No read path decrypts: the
/// port never returns token material.
#[derive(Clone)]
pub struct SqliteFederatedStore {
    db: AuthDb,
    crypto: Arc<Crypto>,
    ids: Arc<dyn IdGenerator>,
}

impl std::fmt::Debug for SqliteFederatedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteFederatedStore")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl SqliteFederatedStore {
    /// Adapter over one handle, sealing tokens with `crypto` and minting ids
    /// with `ids`.
    pub fn new(db: &AuthDb, crypto: Arc<Crypto>, ids: Arc<dyn IdGenerator>) -> Self {
        Self {
            db: db.clone(),
            crypto,
            ids,
        }
    }
}

/// Map one `auth_users` row into the federated view. Federated users always
/// carry usernames (derived at import); a missing value degrades to empty
/// rather than failing the login that just succeeded.
fn map_federated_user(row: &sqlx::sqlite::SqliteRow) -> StoredUser {
    let username: Option<String> = row.get("username");
    let display: Option<String> = row.get("username_display");
    StoredUser {
        id: row.get("id"),
        display_name: row.get("display_name"),
        role: row.get("role"),
        email: row.get("email"),
        avatar_url: row.get("avatar_url"),
        username: username.unwrap_or_default(),
        username_display: display.unwrap_or_default(),
    }
}

impl FederatedUserStore for SqliteFederatedStore {
    async fn get_provider(
        &self,
        provider: &str,
        provider_uid: &str,
    ) -> Result<Option<ProviderBinding>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(
            "SELECT id, user_id, provider, provider_uid FROM auth_providers \
             WHERE provider = ? AND provider_uid = ?",
        )
        .bind(provider)
        .bind(provider_uid)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.map(|row| ProviderBinding {
            id: row.get("id"),
            user_id: row.get("user_id"),
            provider: row.get("provider"),
            provider_uid: row.get("provider_uid"),
        }))
    }

    async fn get_user_by_id(&self, user_id: &str) -> Result<Option<StoredUser>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS} FROM auth_users WHERE id = ?"
        ))
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.as_ref().map(map_federated_user))
    }

    async fn get_user_by_email(&self, email: &str) -> Result<Option<StoredUser>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS} FROM auth_users WHERE email = ?"
        ))
        .bind(email)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.as_ref().map(map_federated_user))
    }

    async fn get_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS} FROM auth_users WHERE username = ?"
        ))
        .bind(username)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.as_ref().map(map_federated_user))
    }

    async fn has_any_users(&self) -> Result<bool, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM auth_users LIMIT 1")
            .fetch_optional(pool)
            .await
            .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(found.is_some())
    }

    async fn create_user(&self, user: NewFederatedUser) -> Result<StoredUser, FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let id = self.ids.new_id();
        let created = to_iso(AuthDb::now_unix());
        let stored = StoredUser {
            id: id.clone(),
            display_name: user.display_name.clone(),
            role: user.role.clone(),
            email: user.email.clone(),
            avatar_url: user.avatar_url.clone(),
            username: user.username.clone(),
            username_display: user.username_display.clone(),
        };
        let outcome: Result<(), crate::db::DbError> = lane
            .write(Lane::Foreground, "auth.federated.create", move |tx| {
                tx.execute(
                    "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
                     created_at, username, username_display) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        id,
                        user.display_name,
                        user.email,
                        user.avatar_url,
                        user.role,
                        created,
                        user.username,
                        user.username_display,
                    ],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await;
        match outcome {
            Ok(()) => Ok(stored),
            // Only a uniqueness conflict retries with a fresh username; any
            // other failure must surface, not spin the retry loop.
            Err(error) if is_write_conflict(&error) => Err(FederatedError::UsernameTaken),
            Err(error) => Err(store_unavailable(error)),
        }
    }

    async fn create_provider(
        &self,
        user_id: &str,
        provider: &str,
        provider_uid: &str,
        token_json: &str,
    ) -> Result<ProviderBinding, FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let sealed = self
            .crypto
            .encrypt(token_json)
            .map_err(|error| store_unavailable(format!("cannot seal provider tokens: {error}")))?;
        let (id, user_id, provider, provider_uid) = (
            self.ids.new_id(),
            user_id.to_owned(),
            provider.to_owned(),
            provider_uid.to_owned(),
        );
        let created = to_iso(AuthDb::now_unix());
        let (bid, buser, bprovider, buid) = (
            id.clone(),
            user_id.clone(),
            provider.clone(),
            provider_uid.clone(),
        );
        lane.write(Lane::Foreground, "auth.federated.bind", move |tx| {
            tx.execute(
                "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
                 provider_data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![bid, buser, bprovider, buid, sealed, created],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)?;
        Ok(ProviderBinding {
            id,
            user_id,
            provider,
            provider_uid,
        })
    }

    async fn update_provider_tokens(
        &self,
        binding_id: &str,
        token_json: &str,
    ) -> Result<(), FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let sealed = self
            .crypto
            .encrypt(token_json)
            .map_err(|error| store_unavailable(format!("cannot seal provider tokens: {error}")))?;
        let binding_id = binding_id.to_owned();
        lane.write(Lane::Foreground, "auth.federated.rotate", move |tx| {
            tx.execute(
                "UPDATE auth_providers SET provider_data = ? WHERE id = ?",
                rusqlite::params![sealed, binding_id],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)
    }
}

// ---------------------------------------------------------------------------
// Session issuing + native credential lookup
// ---------------------------------------------------------------------------

/// Native session issuer for federated logins: mints an opaque token, stores
/// the `standard` row, and stamps `last_login_at`.
#[derive(Clone)]
pub struct SqliteSessionIssuer {
    sessions: SqliteSessionStore,
    db: AuthDb,
    ids: Arc<dyn IdGenerator>,
}

impl std::fmt::Debug for SqliteSessionIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSessionIssuer")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl SqliteSessionIssuer {
    /// Issuer over one handle, minting row ids with `ids`.
    pub fn new(db: &AuthDb, ids: Arc<dyn IdGenerator>) -> Self {
        Self {
            sessions: SqliteSessionStore::new(db),
            db: db.clone(),
            ids,
        }
    }
}

impl SessionIssuer for SqliteSessionIssuer {
    async fn issue_session(
        &self,
        user_id: &str,
        user_agent: Option<&str>,
    ) -> Result<String, FederatedError> {
        let raw =
            super::session::tokens::mint_token().map_err(|_| FederatedError::RngUnavailable)?;
        let now = AuthDb::now_unix();
        self.sessions
            .insert(SessionRecord {
                id: self.ids.new_id(),
                user_id: user_id.to_owned(),
                token_hash: super::session::tokens::hash_token(&raw),
                kind: SessionKind::Standard,
                label: None,
                issued_at: now,
                expires_at: session_expires_at(now),
                last_seen_at: now,
                revoked: false,
                user_agent: user_agent.map(str::to_owned),
            })
            .await
            .map_err(|error| store_unavailable(format!("cannot store session: {error}")))?;
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth session store is not wired"));
        };
        let user_id = user_id.to_owned();
        lane.write(Lane::Foreground, "auth.users.touch", move |tx| {
            tx.execute(
                "UPDATE auth_users SET last_login_at = ? WHERE id = ?",
                rusqlite::params![to_iso(now), user_id],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)?;
        Ok(raw)
    }
}

/// Native login lookup over `auth_users` plus the `local` provider row.
///
/// Returns the tagged stored hash (`scheme$hash`) for the session verifier;
/// unknown users, missing local rows, and corrupt documents all read as
/// absent so the caller burns exactly one dummy verify.
#[derive(Clone, Debug)]
pub struct SqliteCredentialLookup {
    db: AuthDb,
}

impl SqliteCredentialLookup {
    /// Lookup over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

impl CredentialLookup for SqliteCredentialLookup {
    async fn local_user(
        &self,
        username_lc: &str,
    ) -> Option<super::session::login::LocalCredential> {
        let (pool, _) = self.db.live()?;
        let row = sqlx::query(
            "SELECT u.id AS id, u.display_name AS display_name, \
             p.provider_data AS provider_data FROM auth_users u \
             LEFT JOIN auth_providers p ON p.user_id = u.id AND p.provider = 'local' \
             WHERE u.username = ?",
        )
        .bind(username_lc)
        .fetch_optional(pool)
        .await
        .ok()??;
        let data: Option<String> = row.get("provider_data");
        let (scheme, hash) = parse_local_data(data.as_deref()?)?;
        Some(super::session::login::LocalCredential {
            user_id: row.get("id"),
            display_name: row.get("display_name"),
            stored_hash: format!("{scheme}${hash}"),
        })
    }
}

// ---------------------------------------------------------------------------
// App passwords over connect_app_passwords
// ---------------------------------------------------------------------------

/// App-password rows over `connect_app_passwords`.
///
/// Verification stays decrypt-free: `secret_sha256` resolves the row and the
/// touch stamps last use. `secret_encrypted` round-trips untouched so exports
/// can re-encrypt it under a fresh key.
#[derive(Clone, Debug)]
pub struct SqliteAppPasswordStore {
    db: AuthDb,
}

impl SqliteAppPasswordStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

/// Columns read for every app-password row.
const APP_PASSWORD_COLUMNS: &str = "id, user_id, name, secret_sha256, secret_encrypted, \
    created_at, last_used_at, last_client";

fn map_app_password(row: &sqlx::sqlite::SqliteRow) -> AppPasswordRecord {
    let created_raw: String = row.get("created_at");
    let used_raw: Option<String> = row.get("last_used_at");
    AppPasswordRecord {
        id: row.get("id"),
        user_id: row.get("user_id"),
        name: row.get("name"),
        secret_sha256: row.get("secret_sha256"),
        secret_encrypted: row.get("secret_encrypted"),
        created_at: parse_iso(&created_raw).unwrap_or(0),
        last_used_at: used_raw.as_deref().and_then(parse_iso),
        last_client: row.get("last_client"),
    }
}

impl AppPasswordStore for SqliteAppPasswordStore {
    fn insert<'a>(&'a self, row: AppPasswordRecord) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let outcome: Result<(), crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.app_passwords.insert", move |tx| {
                    tx.execute(
                        "INSERT INTO connect_app_passwords (id, user_id, name, secret_sha256, \
                         secret_encrypted, created_at, last_used_at, last_client, revoked) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)",
                        rusqlite::params![
                            row.id,
                            row.user_id,
                            row.name,
                            row.secret_sha256,
                            row.secret_encrypted,
                            to_iso(row.created_at),
                            row.last_used_at.map(to_iso),
                            row.last_client,
                        ],
                    )
                    .map_err(op_error)?;
                    Ok(())
                })
                .await;
            match outcome {
                Ok(()) => Ok(()),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords WHERE id = ?"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.get", error)))?;
            Ok(row.as_ref().map(map_app_password))
        })
    }

    fn get_active_by_sha256<'a>(
        &'a self,
        secret_sha256: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords \
                 WHERE secret_sha256 = ? AND revoked = 0"
            ))
            .bind(secret_sha256)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.get", error)))?;
            Ok(row.as_ref().map(map_app_password))
        })
    }

    fn list_active_by_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let rows = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords \
                 WHERE user_id = ? AND revoked = 0 ORDER BY created_at ASC, id ASC"
            ))
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.list", error)))?;
            Ok(rows.iter().map(map_app_password).collect())
        })
    }

    fn count_active_by_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let total: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM connect_app_passwords WHERE user_id = ? AND revoked = 0",
            )
            .bind(user_id)
            .fetch_one(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.count", error)))?;
            Ok(total.max(0) as u64)
        })
    }

    fn list_all_active<'a>(&'a self) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let rows = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords \
                 WHERE revoked = 0 ORDER BY user_id ASC, created_at ASC, id ASC"
            ))
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.list", error)))?;
            Ok(rows.iter().map(map_app_password).collect())
        })
    }

    fn revoke<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let id = id.to_owned();
            let changed: usize = lane
                .write(Lane::Foreground, "auth.app_passwords.revoke", move |tx| {
                    tx.execute(
                        "UPDATE connect_app_passwords SET revoked = 1 \
                         WHERE id = ? AND revoked = 0",
                        rusqlite::params![id],
                    )
                    .map_err(op_error)
                })
                .await
                .map_err(internal)?;
            Ok(changed > 0)
        })
    }

    fn touch<'a>(
        &'a self,
        secret_sha256: &'a str,
        last_used_at: i64,
        last_client: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let (secret_sha256, last_client) =
                (secret_sha256.to_owned(), last_client.map(str::to_owned));
            lane.write(Lane::Foreground, "auth.app_passwords.touch", move |tx| {
                tx.execute(
                    "UPDATE connect_app_passwords SET last_used_at = ?, \
                     last_client = COALESCE(?, last_client) WHERE secret_sha256 = ?",
                    rusqlite::params![to_iso(last_used_at), last_client, secret_sha256],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }
}

// ---------------------------------------------------------------------------
// Recovery codes over auth_password_recovery_codes
// ---------------------------------------------------------------------------

/// Single-active-code recovery rows over `auth_password_recovery_codes`.
#[derive(Clone, Debug)]
pub struct SqliteRecoveryStore {
    db: AuthDb,
}

impl SqliteRecoveryStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

impl RecoveryStore for SqliteRecoveryStore {
    fn store<'a>(&'a self, code: RecoveryCode) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth recovery store is not wired"));
            };
            lane.write(Lane::Foreground, "auth.recovery.store", move |tx| {
                // Cheap sweep in the same tx (v2 parity): expired rows never
                // accumulate, and no background loop owns this table.
                let now = to_iso(AuthDb::now_unix());
                tx.execute(
                    "DELETE FROM auth_password_recovery_codes WHERE expires_at <= ?",
                    rusqlite::params![now],
                )
                .map_err(op_error)?;
                tx.execute(
                    "INSERT OR REPLACE INTO auth_password_recovery_codes \
                     (user_id, code_hash, created_at, expires_at) VALUES (?, ?, ?, ?)",
                    rusqlite::params![
                        code.user_id,
                        code.code_hash,
                        to_iso(code.created_at),
                        to_iso(code.expires_at),
                    ],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }

    fn find_live_by_hash<'a>(
        &'a self,
        code_hash: &'a str,
        now: i64,
    ) -> BoxFuture<'a, Result<Option<RecoveryCode>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth recovery store is not wired"));
            };
            let row = sqlx::query(
                "SELECT user_id, code_hash, created_at, expires_at \
                 FROM auth_password_recovery_codes WHERE code_hash = ?",
            )
            .bind(code_hash)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.recovery.get", error)))?;
            let Some(row) = row else {
                return Ok(None);
            };
            let expires_raw: String = row.get("expires_at");
            let Some(expires_at) = parse_iso(&expires_raw) else {
                return Ok(None);
            };
            if expires_at <= now {
                return Ok(None);
            }
            let created_raw: String = row.get("created_at");
            Ok(Some(RecoveryCode {
                user_id: row.get("user_id"),
                code_hash: row.get("code_hash"),
                created_at: parse_iso(&created_raw).unwrap_or(0),
                expires_at,
            }))
        })
    }

    fn delete_for_user<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth recovery store is not wired"));
            };
            let user_id = user_id.to_owned();
            lane.write(Lane::Foreground, "auth.recovery.delete", move |tx| {
                tx.execute(
                    "DELETE FROM auth_password_recovery_codes WHERE user_id = ?",
                    rusqlite::params![user_id],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }
}

// ---------------------------------------------------------------------------
// Per-user Last.fm links over user_connections
// ---------------------------------------------------------------------------

/// Per-user Last.fm links over `user_connections` (`service = 'lastfm'`).
///
/// The three secrets stay in their individually sealed `v3:` envelopes inside
/// a plain JSON document; the record round-trips exactly, and only the service
/// layer decrypts, in memory.
#[derive(Clone, Debug)]
pub struct SqliteLastFmStore {
    db: AuthDb,
}

impl SqliteLastFmStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

/// Render a link into the `connection_data` document.
fn render_lastfm(link: &LastFmConnection) -> String {
    serde_json::json!({
        "configured": link.configured,
        "api_key": link.api_key_encrypted,
        "shared_secret": link.shared_secret_encrypted,
        "username": link.username,
        "session_key": link.session_key_encrypted,
    })
    .to_string()
}

/// Parse a `connection_data` document. Corrupt rows read as absent (the user
/// re-links); the caller logs the corruption so it never fails silently.
fn parse_lastfm(data: &str) -> Option<LastFmConnection> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let object = value.as_object()?;
    let field = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    Some(LastFmConnection {
        configured: object
            .get("configured")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        api_key_encrypted: field("api_key"),
        shared_secret_encrypted: field("shared_secret"),
        username: field("username"),
        session_key_encrypted: field("session_key"),
    })
}

impl LastFmStore for SqliteLastFmStore {
    fn get<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LastFmConnection>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth last.fm store is not wired"));
            };
            let row: Option<String> = sqlx::query_scalar(
                "SELECT connection_data FROM user_connections WHERE user_id = ? AND service = ?",
            )
            .bind(user_id)
            .bind(LASTFM_SERVICE)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.lastfm.get", error)))?;
            let Some(data) = row else {
                return Ok(None);
            };
            match parse_lastfm(&data) {
                Some(link) => Ok(Some(link)),
                None => {
                    tracing::warn!("last.fm connection row is corrupt; treating as unlinked");
                    Ok(None)
                }
            }
        })
    }

    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        link: LastFmConnection,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth last.fm store is not wired"));
            };
            let user_id = user_id.to_owned();
            lane.write(Lane::Foreground, "auth.lastfm.upsert", move |tx| {
                let now = to_iso(AuthDb::now_unix());
                tx.execute(
                    "INSERT INTO user_connections (user_id, service, connection_data, enabled, \
                     created_at, updated_at) VALUES (?, 'lastfm', ?, 1, ?, ?) \
                     ON CONFLICT (user_id, service) DO UPDATE SET connection_data = ?, \
                     updated_at = ?",
                    rusqlite::params![
                        user_id,
                        render_lastfm(&link),
                        now,
                        now,
                        render_lastfm(&link),
                        now
                    ],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }

    fn delete<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth last.fm store is not wired"));
            };
            let user_id = user_id.to_owned();
            let changed: usize = lane
                .write(Lane::Foreground, "auth.lastfm.delete", move |tx| {
                    tx.execute(
                        "DELETE FROM user_connections WHERE user_id = ? AND service = 'lastfm'",
                        rusqlite::params![user_id],
                    )
                    .map_err(op_error)
                })
                .await
                .map_err(internal)?;
            Ok(changed > 0)
        })
    }
}

// ---------------------------------------------------------------------------
// OIDC PKCE states over auth_oidc_states
// ---------------------------------------------------------------------------

/// Short-lived OIDC PKCE states over `auth_oidc_states`.
///
/// Consumption is single-use and atomic: the row is deleted in the same
/// transaction that reads it, and expired rows consume as absent.
#[derive(Clone, Debug)]
pub struct SqliteOidcStateStore {
    db: AuthDb,
}

impl SqliteOidcStateStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

impl super::federated::oidc::OidcStateStore for SqliteOidcStateStore {
    async fn store_state(&self, state: &str, code_verifier: &str) -> Result<(), FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth OIDC store is not wired"));
        };
        let (state, code_verifier) = (state.to_owned(), code_verifier.to_owned());
        lane.write(Lane::Foreground, "auth.oidc.store", move |tx| {
            let now = AuthDb::now_unix();
            tx.execute(
                "INSERT OR REPLACE INTO auth_oidc_states \
                 (state, created_at, expires_at, code_verifier) VALUES (?, ?, ?, ?)",
                rusqlite::params![
                    state,
                    to_iso(now),
                    to_iso(now + super::federated::oidc::STATE_TTL_SECS as i64),
                    code_verifier,
                ],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)
    }

    async fn consume_state(&self, state: &str) -> Result<Option<String>, FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth OIDC store is not wired"));
        };
        let state = state.to_owned();
        lane.write(Lane::Foreground, "auth.oidc.consume", move |tx| {
            let found: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT code_verifier, expires_at FROM auth_oidc_states WHERE state = ?",
                    rusqlite::params![state],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(op_error)?;
            tx.execute(
                "DELETE FROM auth_oidc_states WHERE state = ?",
                rusqlite::params![state],
            )
            .map_err(op_error)?;
            let Some((verifier, expires_raw)) = found else {
                return Ok(None);
            };
            if parse_iso(&expires_raw).is_none_or(|at| at <= AuthDb::now_unix()) {
                return Ok(None);
            }
            Ok(verifier)
        })
        .await
        .map_err(store_unavailable)
    }
}

// ---------------------------------------------------------------------------
// Avatars on disk
// ---------------------------------------------------------------------------

/// Avatar bytes under `<dir>/avatars/{user_id}.{ext}`.
///
/// Only one extension variant exists per user: saving replaces any prior
/// avatar regardless of type. Reads resolve the stored variant by extension.
#[derive(Clone, Debug)]
pub struct FileAvatarStore {
    dir: Option<PathBuf>,
}

impl FileAvatarStore {
    /// Store rooted at `dir`; the `avatars` child is created lazily on save.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: Some(dir.to_owned()),
        }
    }

    /// Skeleton-boot store: saves fail, loads read as absent.
    pub fn unwired() -> Self {
        Self { dir: None }
    }

    fn path_for(&self, user_id: &str, ext: &str) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|dir| dir.join("avatars").join(format!("{user_id}.{ext}")))
    }
}

/// Extension plus content type for one supported avatar upload.
fn avatar_variant(content_type: &str) -> Option<(&'static str, &'static str)> {
    match content_type {
        "image/jpeg" => Some(("jpg", "image/jpeg")),
        "image/png" => Some(("png", "image/png")),
        "image/webp" => Some(("webp", "image/webp")),
        "image/gif" => Some(("gif", "image/gif")),
        _ => None,
    }
}

impl AvatarStore for FileAvatarStore {
    fn save<'a>(
        &'a self,
        user_id: &'a str,
        content_type: &'a str,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<String, StoreError>> {
        Box::pin(async move {
            let Some((ext, _)) = avatar_variant(content_type) else {
                return Err(internal("unsupported avatar type"));
            };
            let Some(target) = self.path_for(user_id, ext) else {
                return Err(internal("auth avatar store is not wired"));
            };
            // Blocking file work leaves the IO loop.
            let owned: Vec<u8> = bytes.to_vec();
            let user_id = user_id.to_owned();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let parent = target
                    .parent()
                    .ok_or_else(|| "avatar dir has no parent".to_owned())?;
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                for old in ["jpg", "png", "webp", "gif"] {
                    if old == ext {
                        continue;
                    }
                    let stale = parent.join(format!("{user_id}.{old}"));
                    if stale != target {
                        let _ = std::fs::remove_file(stale);
                    }
                }
                let tmp = parent.join(format!(".{user_id}.{ext}.tmp"));
                std::fs::write(&tmp, &owned).map_err(|error| error.to_string())?;
                std::fs::rename(&tmp, &target).map_err(|error| error.to_string())?;
                Ok(())
            })
            .await
            .map_err(|error| internal(format!("avatar save panicked: {error}")))?
            .map_err(internal)?;
            Ok(ext.to_owned())
        })
    }

    fn load<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LoadedAvatar>, StoreError>> {
        Box::pin(async move {
            let Some(dir) = self.dir.clone() else {
                return Ok(None);
            };
            let user_id = user_id.to_owned();
            tokio::task::spawn_blocking(move || {
                for (ext, content_type) in [
                    ("jpg", "image/jpeg"),
                    ("png", "image/png"),
                    ("webp", "image/webp"),
                    ("gif", "image/gif"),
                ] {
                    let path = dir.join("avatars").join(format!("{user_id}.{ext}"));
                    if let Ok(bytes) = std::fs::read(&path) {
                        return Ok(Some((bytes, content_type.to_owned())));
                    }
                }
                Ok(None)
            })
            .await
            .map_err(|error| internal(format!("avatar load panicked: {error}")))?
        })
    }
}
