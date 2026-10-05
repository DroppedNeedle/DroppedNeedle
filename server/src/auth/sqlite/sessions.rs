//! Sessions over `auth_tokens`: the login/middleware store, the
//! session-list backend, and the session issuer for federated logins.

use std::sync::Arc;

use rusqlite::OptionalExtension as _;
use sqlx::Row as _;

use super::{
    AuthDb, internal, is_write_conflict, op_error, parse_local_data, render_local_data,
    store_unavailable,
};
use crate::auth::federated::password_import::HashScheme;
use crate::auth::federated::{FederatedError, SessionIssuer};
use crate::auth::passwords::{PendingRehash, RehashQueue};
use crate::auth::session::store::{SessionKind, SessionRecord, SessionStore, SessionStoreError};
use crate::auth::session::tokens::{constant_time_eq, expires_at};
use crate::auth::times::{parse_iso, to_iso};
use crate::auth::users::models::{ManagedSession, SessionOwner};
use crate::auth::users::services::COMPANION_LABEL_PREFIX;
use crate::auth::users::stores::{BoxFuture, Clock, SessionManager, StoreError};
use crate::db::{Lane, map_sqlx_busy};
use crate::ids::IdGenerator;

/// How stale `last_seen_at` must be before a lookup rewrites it (5 minutes).
const LAST_SEEN_TOUCH_SECS: i64 = 5 * 60;

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
        let raw = crate::auth::session::tokens::mint_token()
            .map_err(|_| FederatedError::RngUnavailable)?;
        let now = AuthDb::now_unix();
        self.sessions
            .insert(SessionRecord {
                id: self.ids.new_id(),
                user_id: user_id.to_owned(),
                token_hash: crate::auth::session::tokens::hash_token(&raw),
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
