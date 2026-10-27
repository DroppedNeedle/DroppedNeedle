//! Per-user Last.fm links over `user_connections`.

use rusqlite::OptionalExtension as _;

use super::sql;
use super::{AuthDb, op_error, store_error};
use crate::times::to_iso;
use crate::users::models::LastFmConnection;
use crate::users::stores::StoreError;
use droppedneedle_kernel::db::Lane;

/// Per-user Last.fm links over `user_connections` (`service = 'lastfm'`).
///
/// The three secrets stay in their individually sealed `v3:` envelopes inside
/// a plain JSON document; the record round-trips exactly, and only the service
/// layer decrypts, in memory.
#[derive(Clone, Debug)]
pub struct LastFmLinks {
    db: AuthDb,
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

impl LastFmLinks {
    /// Last.fm links over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }

    /// Fetch one user's link. None is absence, never failure; a corrupt
    /// row reads as unlinked, with a warning.
    pub async fn get(&self, user_id: &str) -> Result<Option<LastFmConnection>, StoreError> {
        let user_id = user_id.to_owned();
        let row: Option<String> = self
            .db
            .read("auth.lastfm.get", move |conn| {
                conn.prepare_cached(sql::LASTFM_LINK.text())?
                    .query_row([user_id], |row| row.get(0))
                    .optional()
            })
            .await?;
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
    }

    /// Insert or replace one user's link.
    pub async fn upsert(&self, user_id: &str, link: LastFmConnection) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        let now = to_iso(self.db.now_unix());
        self.db
            .lane()
            .write(Lane::Foreground, "auth.lastfm.upsert", move |tx| {
                tx.execute(
                    sql::UPSERT_LASTFM_LINK.text(),
                    rusqlite::params![user_id, render_lastfm(&link), now],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    /// Delete one user's link. Returns false when none existed.
    pub async fn delete(&self, user_id: &str) -> Result<bool, StoreError> {
        let user_id = user_id.to_owned();
        let changed: usize = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.lastfm.delete", move |tx| {
                tx.execute(sql::DELETE_LASTFM_LINK.text(), rusqlite::params![user_id])
                    .map_err(op_error)
            })
            .await
            .map_err(store_error)?;
        Ok(changed > 0)
    }
}
