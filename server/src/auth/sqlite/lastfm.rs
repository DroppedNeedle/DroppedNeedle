//! Per-user Last.fm links over `user_connections`.

use super::{AuthDb, internal, op_error};
use crate::auth::times::to_iso;
use crate::auth::users::models::LastFmConnection;
use crate::auth::users::stores::{BoxFuture, LastFmStore, StoreError};
use crate::db::{Lane, map_sqlx_busy};

/// Last.fm rows in `user_connections` carry this service tag.
const LASTFM_SERVICE: &str = "lastfm";

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
