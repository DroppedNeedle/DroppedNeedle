//! Sessions over `auth_tokens`: the login/middleware store, the
//! session-list backend, and the session issuer for federated logins.

use std::sync::Arc;

use rusqlite::OptionalExtension as _;
use sqlx::Row as _;

use super::{
    AuthDb, is_write_conflict, op_error, parse_local_data, render_local_data, store_error,
    store_unavailable,
};
use crate::federated::password_import::HashScheme;
use crate::federated::{FederatedError, SessionIssuer};
use crate::passwords::StoredPassword;
use crate::session::store::{
    SessionKind, SessionRecord, SessionStore, SessionStoreError, ValidSession,
};
use crate::session::tokens::{constant_time_eq, expires_at};
use crate::times::{parse_iso, to_iso};
use crate::users::models::{ManagedSession, SessionOwner};
use crate::users::roles::Role;
use crate::users::services::COMPANION_LABEL_PREFIX;
use crate::users::stores::{BoxFuture, SessionManager, StoreError};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Lane, map_sqlx_busy};
use droppedneedle_kernel::ids::IdGenerator;

/// How stale `last_seen_at` must be before a lookup rewrites it (5 minutes).
const LAST_SEEN_TOUCH_SECS: i64 = 5 * 60;

/// Session storage for logins and the middleware, over `auth_tokens`.
///
/// Each insert is followed by a separate write that deletes a batch of
/// revoked and expired rows, so the table does not grow without bound.
///
/// Successful lookups refresh `last_seen_at` when it is older than
/// [`LAST_SEEN_TOUCH_SECS`]; the refresh is best-effort (a failed touch logs
/// and the request still authenticates). Companion labels round-trip through
/// the `user_agent` column with the shared `DroppedNeedle companion` prefix.
#[derive(Clone, Debug)]
pub struct SqliteSessionStore {
    db: AuthDb,
}

/// What [`SqliteSessionStore::insert_login`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginWrite {
    /// The session, any rehash and `last_login_at` were written.
    Stored,
    /// The stored credential is no longer the one the login verified (or
    /// is gone): nothing was written.
    CredentialChanged,
}

impl SqliteSessionStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }

    /// A password login's write: the session row, the rehash (if any) and
    /// `last_login_at` (the session's `issued_at`) in one transaction, and
    /// only while the user's local credential still has the scheme and hash
    /// in `verified`. Otherwise nothing is written and the answer is
    /// [`LoginWrite::CredentialChanged`]. A failed write rolls back all
    /// three, so the stored hash is never half-changed.
    pub async fn insert_login(
        &self,
        record: SessionRecord,
        verified: &StoredPassword,
        rehash: Option<String>,
    ) -> Result<LoginWrite, SessionStoreError> {
        let verified = verified.clone();
        let issued_at = record.issued_at;
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.session.login", move |tx| {
                let current: Option<Option<String>> = tx
                    .query_row(
                        "SELECT provider_data FROM auth_providers \
                         WHERE user_id = ? AND provider = 'local'",
                        rusqlite::params![record.user_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(op_error)?;
                let unchanged = current
                    .flatten()
                    .as_deref()
                    .and_then(parse_local_data)
                    .is_some_and(|(scheme, hash)| {
                        scheme == verified.scheme && hash == verified.hash
                    });
                if !unchanged {
                    return Ok(LoginWrite::CredentialChanged);
                }
                insert_token_row(tx, &record)?;
                if let Some(new_hash) = rehash {
                    tx.execute(
                        "UPDATE auth_providers SET provider_data = ? \
                         WHERE user_id = ? AND provider = 'local'",
                        rusqlite::params![
                            render_local_data(HashScheme::Argon2id.as_tag(), &new_hash),
                            record.user_id
                        ],
                    )
                    .map_err(op_error)?;
                }
                tx.execute(
                    "UPDATE auth_users SET last_login_at = ? WHERE id = ?",
                    rusqlite::params![to_iso(record.issued_at), record.user_id],
                )
                .map_err(op_error)?;
                Ok(LoginWrite::Stored)
            })
            .await;
        let written = session_write_outcome("auth.session.login", outcome)?;
        if written == LoginWrite::Stored {
            self.sweep(issued_at).await;
        }
        Ok(written)
    }

    /// Delete a batch of dead tokens in a write of its own, so a large batch
    /// can never push the sign-in past its budget. The session is already
    /// stored, so a failed sweep is logged and the next sign-in tries again.
    async fn sweep(&self, now_unix: i64) {
        if let Err(error) = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.session.sweep", move |tx| {
                sweep_dead_tokens(tx, now_unix)
            })
            .await
        {
            tracing::warn!(%error, "dead session sweep failed");
        }
    }
}

/// Insert one session row inside the caller's transaction. Companion
/// labels are stored in `user_agent` behind the shared prefix.
fn insert_token_row(
    tx: &rusqlite::Transaction,
    record: &SessionRecord,
) -> Result<(), super::WriteError> {
    let stored_agent = match (&record.kind, &record.label) {
        (SessionKind::Companion, Some(label)) => Some(format!("{COMPANION_LABEL_PREFIX}{label}")),
        _ => record.user_agent.clone(),
    };
    let kind = match record.kind {
        SessionKind::Standard => "standard",
        SessionKind::Companion => "companion",
    };
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
    Ok(())
}

/// Map a session write's outcome onto the session port's errors.
fn session_write_outcome<R>(
    operation: &str,
    outcome: Result<R, super::WriteError>,
) -> Result<R, SessionStoreError> {
    match outcome {
        Ok(value) => Ok(value),
        Err(error) if is_write_conflict(&error) => Err(SessionStoreError::Duplicate),
        // A write past its budget was rolled back whole: nothing of the
        // sign-in was kept, so the client may simply try again.
        Err(super::WriteError::Db(droppedneedle_kernel::db::DbError::TxTimeout { .. })) => {
            Err(SessionStoreError::Busy)
        }
        Err(error) => Err(session_store_error(operation, error)),
    }
}

impl SessionStore for SqliteSessionStore {
    async fn insert(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        let issued_at = record.issued_at;
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.session.insert", move |tx| {
                insert_token_row(tx, &record)
            })
            .await;
        session_write_outcome("auth.session.insert", outcome)?;
        self.sweep(issued_at).await;
        Ok(())
    }

    async fn lookup_valid(
        &self,
        token_hash: &str,
        now_unix: i64,
    ) -> Result<Option<ValidSession>, SessionStoreError> {
        let (pool, lane) = (self.db.pool(), self.db.lane());
        // The owner's row comes in the same read: a token whose user is
        // gone matches nothing, and the role is the one for this request.
        let row = sqlx::query(
            "SELECT t.id, t.user_id, t.token_hash, t.session_kind, t.user_agent, t.issued_at, \
             t.expires_at, t.last_seen_at, t.revoked, u.username, \
             COALESCE(u.username_display, u.username) AS username_display, u.role \
             FROM auth_tokens t JOIN auth_users u ON u.id = t.user_id WHERE t.token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(pool)
        .await
        .map_err(|error| {
            session_store_error(
                "auth.session.lookup",
                map_sqlx_busy("auth.session.lookup", error),
            )
        })?;
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
        let role: String = row.get("role");
        Ok(Some(ValidSession {
            record: SessionRecord {
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
            },
            username: row.get("username"),
            username_display: row.get("username_display"),
            role: Role::from_stored(&role),
        }))
    }
}

/// Session-port failure from a lane or pool error: busy stays busy, and
/// anything else is logged here, since the port's error carries no cause.
fn session_store_error(operation: &str, error: impl Into<super::WriteError>) -> SessionStoreError {
    let error = error.into();
    if matches!(
        error,
        super::WriteError::Db(droppedneedle_kernel::db::DbError::Busy { .. })
    ) {
        return SessionStoreError::Busy;
    }
    tracing::error!(operation, %error, "session store failed");
    SessionStoreError::Unavailable
}

/// Most dead token rows one login removes. Logins are frequent enough
/// that small batches keep the table clean without a background loop.
const TOKEN_SWEEP_BATCH: i64 = 500;

/// Delete revoked and expired token rows (no request can use them again),
/// in the login's own transaction.
fn sweep_dead_tokens(tx: &rusqlite::Transaction, now_unix: i64) -> Result<(), super::WriteError> {
    tx.execute(
        "DELETE FROM auth_tokens WHERE id IN (SELECT id FROM auth_tokens \
         WHERE revoked = 1 OR expires_at <= ? LIMIT ?)",
        rusqlite::params![to_iso(now_unix), TOKEN_SWEEP_BATCH],
    )
    .map_err(op_error)?;
    Ok(())
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
            let pool = self.db.pool();
            let rows = sqlx::query(
                "SELECT id, user_id, session_kind, user_agent, issued_at, last_seen_at, \
                 expires_at FROM auth_tokens WHERE user_id = ? AND revoked = 0",
            )
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(|error| store_error(map_sqlx_busy("auth.sessions.list", error)))?;
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
            let lane = self.db.lane();
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
                .map_err(store_error)?;
            Ok(changed > 0)
        })
    }

    fn revoke_all_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let lane = self.db.lane();
            let user_id = user_id.to_owned();
            let changed: usize = lane
                .write(Lane::Foreground, "auth.sessions.revoke_all", move |tx| {
                    Ok(tx.execute(
                        "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ? AND revoked = 0",
                        rusqlite::params![user_id],
                    )?)
                })
                .await
                .map_err(store_error)?;
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
            let lane = self.db.lane();
            let agent = format!("{COMPANION_LABEL_PREFIX}{label}");
            let (id, user_id, token_hash) =
                (id.to_owned(), user_id.to_owned(), token_hash.to_owned());
            let (cid, cuser, chash, cagent) = (
                id.clone(),
                user_id.clone(),
                token_hash.clone(),
                agent.clone(),
            );
            let outcome: Result<(), super::WriteError> = lane
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
                Err(error) => Err(store_error(error)),
            }
        })
    }

    fn owner_by_hash<'a>(
        &'a self,
        token_hash: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionOwner>, StoreError>> {
        Box::pin(async move {
            let pool = self.db.pool();
            let row = sqlx::query(
                "SELECT id, user_id, session_kind, expires_at FROM auth_tokens \
                 WHERE token_hash = ? AND revoked = 0",
            )
            .bind(token_hash)
            .fetch_optional(pool)
            .await
            .map_err(|error| store_error(map_sqlx_busy("auth.sessions.owner", error)))?;
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
            crate::session::tokens::mint_token().map_err(|_| FederatedError::RngUnavailable)?;
        let now = self.db.now_unix();
        self.sessions
            .insert(SessionRecord {
                id: self.ids.new_id(),
                user_id: user_id.to_owned(),
                token_hash: crate::session::tokens::hash_token(&raw),
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
        let lane = self.db.lane();
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
