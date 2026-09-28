//! Session store port plus an in-memory fake.
//!
//! The port is what the middleware and login service program against; the
//! production adapter (rusqlite/sqlx over the baseline `auth_tokens` table)
//! lands with the persistence slice. The store only ever sees hashes: callers
//! hash the presented token before lookup, so raw tokens never reach storage.
//!
//! Throttled `last_seen_at` writes (at most one per ~5 min per token) and the
//! R6 list/revoke surface belong to the session-management slice, not here.

use std::collections::HashMap;
use std::sync::RwLock;

use serde::Serialize;
use thiserror::Error;

use super::tokens::constant_time_eq;

/// Session kind, v2 `session_kind` values kept verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// Browser cookie session or Bearer [REDACTED] login session; may mint device sessions.
    Standard,
    /// Named companion Bearer [REDACTED] one trusted device; must never mint (403).
    Companion,
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

/// Storage failure. Transports map this to a 500 without leaking detail.
#[derive(Debug, Error)]
pub enum SessionStoreError {
    /// The backend failed (lock poisoned in the fake; io in production).
    #[error("session store unavailable")]
    Unavailable,
    /// A programming error supplied a duplicate hash.
    #[error("token hash already stored")]
    Duplicate,
}

/// Persistence port for opaque sessions. `Clone` so axum state shares it.
pub trait SessionStore: Clone + Send + Sync + 'static {
    /// Persist a freshly minted session.
    fn insert(
        &self,
        record: SessionRecord,
    ) -> impl Future<Output = Result<(), SessionStoreError>> + Send;

    /// Look up by storage hash; returns `None` for unknown, revoked, or
    /// expired rows. Comparison is constant-time over the candidate hash.
    fn lookup_valid(
        &self,
        token_hash: &str,
        now_unix: i64,
    ) -> impl Future<Output = Result<Option<SessionRecord>, SessionStoreError>> + Send;
}

/// Current unix time in whole seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// In-memory store for tests and scratch harnesses. Never production: sessions
/// must survive restarts, which needs the SQLite adapter. Clones share the
/// same rows, like a connection pool handle.
#[derive(Debug, Clone, Default)]
pub struct MemorySessionStore {
    rows: std::sync::Arc<RwLock<HashMap<String, SessionRecord>>>,
}

impl MemorySessionStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for MemorySessionStore {
    async fn insert(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| SessionStoreError::Unavailable)?;
        if rows.contains_key(&record.token_hash) {
            return Err(SessionStoreError::Duplicate);
        }
        rows.insert(record.token_hash.clone(), record);
        Ok(())
    }

    async fn lookup_valid(
        &self,
        token_hash: &str,
        now_unix: i64,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        let rows = self
            .rows
            .read()
            .map_err(|_| SessionStoreError::Unavailable)?;
        for (stored_hash, record) in rows.iter() {
            if constant_time_eq(stored_hash, token_hash) {
                if record.revoked || record.expires_at <= now_unix {
                    return Ok(None);
                }
                return Ok(Some(record.clone()));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tokens;
    use super::*;

    fn record(hash: &str, now: i64) -> SessionRecord {
        SessionRecord {
            id: "id-1".to_owned(),
            user_id: "user-1".to_owned(),
            token_hash: hash.to_owned(),
            kind: SessionKind::Standard,
            label: None,
            issued_at: now,
            expires_at: tokens::expires_at(now),
            last_seen_at: now,
            revoked: false,
            user_agent: None,
        }
    }

    #[tokio::test]
    async fn lookup_accepts_live_and_rejects_revoked_or_expired() {
        let now = 1_700_000_000;
        let store = MemorySessionStore::new();
        let live = tokens::hash_token("live-token");
        let mut revoked = record(&tokens::hash_token("revoked-token"), now);
        revoked.revoked = true;
        let mut old = record(
            &tokens::hash_token("old-token"),
            now - tokens::SESSION_MAX_AGE_SECS - 1,
        );
        old.expires_at = now - 1;
        store.insert(record(&live, now)).await.unwrap();
        store.insert(revoked).await.unwrap();
        store.insert(old).await.unwrap();

        assert!(store.lookup_valid(&live, now).await.unwrap().is_some());
        assert!(
            store
                .lookup_valid(&tokens::hash_token("revoked-token"), now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .lookup_valid(&tokens::hash_token("old-token"), now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .lookup_valid(&tokens::hash_token("unknown"), now)
                .await
                .unwrap()
                .is_none()
        );
    }
}
