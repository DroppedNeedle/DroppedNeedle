//! Sessions over `auth_tokens`: one repository for logins, the session
//! gate, the session list and federated sign-ins.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::OptionalExtension as _;
use sqlx::Row as _;

use super::{
    AuthDb, is_write_conflict, op_error, parse_local_data, render_local_data, store_error,
    store_unavailable,
};
use crate::federated::FederatedError;
use crate::federated::password_import::HashScheme;
use crate::passwords::StoredPassword;
use crate::session::store::{
    NewSession, SessionKind, SessionRecord, SessionStoreError, ValidSession,
};
use crate::session::tokens::{constant_time_eq, expires_at};
use crate::times::{parse_iso, to_iso};
use crate::users::models::{ManagedSession, SessionOwner};
use crate::users::roles::Role;
use crate::users::services::COMPANION_LABEL_PREFIX;
use crate::users::stores::StoreError;
use droppedneedle_kernel::db::{Lane, map_sqlx_busy};
use droppedneedle_kernel::ids::IdGenerator;

/// How stale `last_seen_at` must be before a lookup rewrites it (5 minutes).
const LAST_SEEN_TOUCH_SECS: i64 = 5 * 60;

/// The one session repository over `auth_tokens`. Clones share the
/// database, the id generator and the touches in flight.
///
/// Every row id comes from the id generator. Each insert is followed by a
/// separate write that deletes a batch of revoked and expired rows, so the
/// table does not grow without bound.
///
/// A lookup that finds `last_seen_at` older than [`LAST_SEEN_TOUCH_SECS`]
/// hands the refresh to a background write it does not wait for, at most
/// one in flight per session; a failed refresh is logged and the request is
/// unaffected. Companion labels round-trip through the `user_agent` column
/// with the shared `DroppedNeedle companion` prefix, and listings expose no
/// token hashes.
#[derive(Clone)]
pub struct Sessions {
    db: AuthDb,
    ids: Arc<dyn IdGenerator>,
    touching: Arc<Mutex<HashSet<String>>>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

/// What [`Sessions::insert_login`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginWrite {
    /// The session, any rehash and `last_login_at` were written.
    Stored,
    /// The stored credential is no longer the one the login verified (or
    /// is gone): nothing was written.
    CredentialChanged,
}

impl Sessions {
    /// The repository over one handle, minting row ids with `ids`.
    pub fn new(db: &AuthDb, ids: Arc<dyn IdGenerator>) -> Self {
        Self {
            db: db.clone(),
            ids,
            touching: Arc::default(),
        }
    }

    /// The stored row for a new standard session, with a fresh id.
    fn standard_row(&self, session: NewSession) -> SessionRecord {
        SessionRecord {
            id: self.ids.new_id(),
            user_id: session.user_id,
            token_hash: session.token_hash,
            kind: SessionKind::Standard,
            label: None,
            issued_at: session.issued_at,
            expires_at: expires_at(session.issued_at),
            last_seen_at: session.issued_at,
            revoked: false,
            user_agent: session.user_agent,
        }
    }

    /// Store a new standard session.
    pub async fn insert(&self, session: NewSession) -> Result<(), SessionStoreError> {
        self.insert_record(self.standard_row(session)).await
    }

    /// Store a row exactly as given, its id included. Tests seed with it;
    /// the server stores sessions through [`Sessions::insert`] and the
    /// other writers, which take the id from the generator.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn seed(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        self.insert_record(record).await
    }

    async fn insert_record(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
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

    /// A password login's write: the session row, the rehash (if any) and
    /// `last_login_at` (the session's `issued_at`) in one transaction, and
    /// only while the user's local credential still has the scheme and hash
    /// in `verified`. Otherwise nothing is written and the answer is
    /// [`LoginWrite::CredentialChanged`]. A failed write rolls back all
    /// three, so the stored hash is never half-changed.
    pub async fn insert_login(
        &self,
        session: NewSession,
        verified: &StoredPassword,
        rehash: Option<String>,
    ) -> Result<LoginWrite, SessionStoreError> {
        let record = self.standard_row(session);
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

    /// Look up by storage hash, reading the token and its owner together;
    /// returns `None` for unknown, revoked or expired rows and for a token
    /// whose owner is gone. Comparison is constant-time over the candidate
    /// hash. Never waits for the `last_seen_at` refresh it may start.
    pub async fn lookup_valid(
        &self,
        token_hash: &str,
        now_unix: i64,
    ) -> Result<Option<ValidSession>, SessionStoreError> {
        // The owner's row comes in the same read: a token whose user is
        // gone matches nothing, and the role is the one for this request.
        let row = sqlx::query(
            "SELECT t.id, t.user_id, t.token_hash, t.session_kind, t.user_agent, t.issued_at, \
             t.expires_at, t.last_seen_at, t.revoked, u.username, \
             COALESCE(u.username_display, u.username) AS username_display, u.role \
             FROM auth_tokens t JOIN auth_users u ON u.id = t.user_id WHERE t.token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(self.db.pool())
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
        let id: String = row.get("id");
        if now_unix - last_seen_at >= LAST_SEEN_TOUCH_SECS {
            self.touch_later(id.clone(), now_unix);
        }
        let role: String = row.get("role");
        Ok(Some(ValidSession {
            record: SessionRecord {
                id,
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

    /// Refresh one session's `last_seen_at` in a background write that
    /// nothing waits for. A session with a refresh still in flight gets no
    /// second one, so a held lane cannot pile them up.
    fn touch_later(&self, id: String, now_unix: i64) {
        if !lock(&self.touching).insert(id.clone()) {
            return;
        }
        let (db, touching) = (self.db.clone(), Arc::clone(&self.touching));
        tokio::spawn(async move {
            let (row, touched) = (id.clone(), to_iso(now_unix));
            let outcome = db
                .lane()
                .write(Lane::Background, "auth.session.touch", move |tx| {
                    tx.execute(
                        "UPDATE auth_tokens SET last_seen_at = ? WHERE id = ?",
                        rusqlite::params![touched, row],
                    )?;
                    Ok(())
                })
                .await;
            lock(&touching).remove(&id);
            if let Err(error) = outcome {
                tracing::warn!(%error, "session last_seen touch failed");
            }
        });
    }

    /// Live sessions for one user (unrevoked, unexpired): newest first, id
    /// tiebreak. Rows carry no token hashes.
    pub async fn list_for_user(&self, user_id: &str) -> Result<Vec<ManagedSession>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, user_id, session_kind, user_agent, issued_at, last_seen_at, \
             expires_at FROM auth_tokens WHERE user_id = ? AND revoked = 0",
        )
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| store_error(map_sqlx_busy("auth.sessions.list", error)))?;
        let now = self.db.now_unix();
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
    }

    /// Revoke one session, but only when it belongs to the user. Returns
    /// false for unknown or foreign ids (no cross-user oracle).
    pub async fn revoke_scoped(&self, user_id: &str, session_id: &str) -> Result<bool, StoreError> {
        let (user_id, session_id) = (user_id.to_owned(), session_id.to_owned());
        let changed: usize = self
            .db
            .lane()
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
    }

    /// Revoke every session of one user. Returns the revoked count.
    pub async fn revoke_all_for_user(&self, user_id: &str) -> Result<u64, StoreError> {
        let user_id = user_id.to_owned();
        let changed: usize = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.sessions.revoke_all", move |tx| {
                Ok(tx.execute(
                    "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ? AND revoked = 0",
                    rusqlite::params![user_id],
                )?)
            })
            .await
            .map_err(store_error)?;
        Ok(changed as u64)
    }

    /// Insert a companion token and revoke the prior live same-label
    /// companion token for the user, atomically, in one transaction.
    pub async fn replace_companion(
        &self,
        user_id: &str,
        token_hash: &str,
        label: &str,
        issued_at: i64,
        expires_at: i64,
    ) -> Result<ManagedSession, StoreError> {
        let agent = format!("{COMPANION_LABEL_PREFIX}{label}");
        let (id, user_id) = (self.ids.new_id(), user_id.to_owned());
        let (cid, cuser, chash, cagent) = (
            id.clone(),
            user_id.clone(),
            token_hash.to_owned(),
            agent.clone(),
        );
        let outcome: Result<(), super::WriteError> = self
            .db
            .lane()
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
                label: agent,
                created_at: issued_at,
                last_seen_at: issued_at,
                expires_at,
            }),
            Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
            Err(error) => Err(store_error(error)),
        }
    }

    /// Resolve a live token hash to its owner. None for unknown, revoked,
    /// or expired hashes. Sign-out uses it to find the session to revoke.
    pub async fn owner_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<SessionOwner>, StoreError> {
        let row = sqlx::query(
            "SELECT id, user_id, session_kind, expires_at FROM auth_tokens \
             WHERE token_hash = ? AND revoked = 0",
        )
        .bind(token_hash)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| store_error(map_sqlx_busy("auth.sessions.owner", error)))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let expires_raw: String = row.get("expires_at");
        if parse_iso(&expires_raw).is_none_or(|at| at <= self.db.now_unix()) {
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
    }

    /// A federated login's session: mint an opaque token, store the
    /// `standard` row and stamp `last_login_at`. Returns the raw token once.
    pub async fn issue_session(
        &self,
        user_id: &str,
        user_agent: Option<&str>,
    ) -> Result<String, FederatedError> {
        let raw =
            crate::session::tokens::mint_token().map_err(|_| FederatedError::RngUnavailable)?;
        let now = self.db.now_unix();
        self.insert(NewSession {
            user_id: user_id.to_owned(),
            token_hash: crate::session::tokens::hash_token(&raw),
            issued_at: now,
            user_agent: user_agent.map(str::to_owned),
        })
        .await
        .map_err(|error| store_unavailable(format!("cannot store session: {error}")))?;
        let user_id = user_id.to_owned();
        self.db
            .lane()
            .write(Lane::Foreground, "auth.users.touch", move |tx| {
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

/// The in-flight touch set. A poisoned lock still holds a usable set.
fn lock(touching: &Mutex<HashSet<String>>) -> MutexGuard<'_, HashSet<String>> {
    touching
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// Map a session write's outcome onto the session errors.
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

/// Session failure from a lane or pool error: busy stays busy, and
/// anything else is logged here, since the session error carries no cause.
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
