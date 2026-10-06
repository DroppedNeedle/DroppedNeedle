//! SQLite stores behind the Spotify import.
//!
//! - [`SqliteSpotifyStates`]: authorize states in `spotify_auth_requests`
//!   (migration 0023), each with the redirect URI the authorize step sent,
//!   valid for ten minutes and pruned on every insert, as v2 did.
//! - [`SqliteSpotifyLinks`]: per-user links in `user_connections` under
//!   service `spotify`. The document keeps v2's field names and is sealed
//!   whole with the instance key, like the other sealed connections.
//! - [`CollectionsPlaylistBridge`]: imported playlists are ordinary rows in
//!   `playlists` / `playlist_tracks`, written through the collections
//!   playlist store, so they show up wherever playlists do.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::spotify::{
    ImportedTrack, PendingAuth, PlaylistIndex, PlaylistTrackSink, STATE_TTL_SECS,
    SpotifyConnection, SpotifyConnectionStore, SpotifyStateStore, now_unix_secs,
};
use crate::acquire::db::AcquireDb;
use crate::auth::times::{parse_iso, to_iso};
use crate::reads::collections::store::PlaylistStore;
use crate::reads::collections::store::playlists::{CoverImage, NewEntry, Written};
use crate::runtime_config::crypto::Crypto;

/// The `user_connections.service` tag for Spotify links (v2's).
const SERVICE: &str = "spotify";

/// Authorize states over the shared database.
#[derive(Clone)]
pub struct SqliteSpotifyStates {
    db: AcquireDb,
}

impl SqliteSpotifyStates {
    /// States over the serving database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }
}

impl SpotifyStateStore for SqliteSpotifyStates {
    fn store_state<'a>(
        &'a self,
        state: &'a str,
        pending: &'a PendingAuth,
    ) -> BoxFuture<'a, Result<(), String>> {
        let (state, pending) = (state.to_owned(), pending.clone());
        Box::pin(async move {
            self.db
                .write("spotify.state.store", move |tx| {
                    let now = now_unix_secs();
                    tx.execute(
                        "DELETE FROM spotify_auth_requests WHERE expires_at <= ?1",
                        params![now],
                    )?;
                    tx.execute(
                        "INSERT INTO spotify_auth_requests \
                         (state, user_id, redirect_uri, expires_at) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            state,
                            pending.user_id,
                            pending.redirect_uri,
                            now + STATE_TTL_SECS
                        ],
                    )?;
                    Ok(())
                })
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn consume_state<'a>(
        &'a self,
        state: &'a str,
    ) -> BoxFuture<'a, Result<Option<PendingAuth>, String>> {
        let state = state.to_owned();
        Box::pin(async move {
            self.db
                .write("spotify.state.consume", move |tx| {
                    let row: Option<(String, String, i64)> = tx
                        .query_row(
                            "SELECT user_id, redirect_uri, expires_at \
                             FROM spotify_auth_requests WHERE state = ?1",
                            params![state],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .optional()?;
                    tx.execute(
                        "DELETE FROM spotify_auth_requests WHERE state = ?1",
                        params![state],
                    )?;
                    Ok(row
                        .filter(|(_, _, expires_at)| *expires_at > now_unix_secs())
                        .map(|(user_id, redirect_uri, _)| PendingAuth {
                            user_id,
                            redirect_uri,
                        }))
                })
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// The sealed link document, with v2's field names.
#[derive(Serialize, Deserialize)]
struct LinkDocument {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    /// ISO-8601 expiry, as v2 stored it.
    #[serde(default)]
    expires_at: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    spotify_user_id: String,
}

impl From<&SpotifyConnection> for LinkDocument {
    fn from(link: &SpotifyConnection) -> Self {
        Self {
            access_token: link.access_token.clone(),
            refresh_token: link.refresh_token.clone(),
            expires_at: to_iso(link.expires_at_unix),
            username: link.username.clone(),
            spotify_user_id: link.spotify_user_id.clone(),
        }
    }
}

/// Per-user links in `user_connections`.
#[derive(Clone)]
pub struct SqliteSpotifyLinks {
    db: AcquireDb,
    crypto: Arc<Crypto>,
}

impl SqliteSpotifyLinks {
    /// Links over the serving database, sealed with the instance key.
    pub fn new(db: AcquireDb, crypto: Arc<Crypto>) -> Self {
        Self { db, crypto }
    }

    fn seal(&self, link: &SpotifyConnection) -> Result<String, String> {
        let document = serde_json::to_string(&LinkDocument::from(link))
            .map_err(|error| format!("spotify link encode: {error}"))?;
        self.crypto
            .encrypt(&document)
            .map_err(|error| format!("spotify link seal: {error}"))
    }
}

impl SpotifyConnectionStore for SqliteSpotifyLinks {
    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        connection: &'a SpotifyConnection,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let (user_id, sealed) = (user_id.to_owned(), self.seal(connection)?);
            self.db
                .write("spotify.link.upsert", move |tx| {
                    let now = to_iso(now_unix_secs());
                    tx.execute(
                        "INSERT INTO user_connections \
                         (user_id, service, connection_data, enabled, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, 1, ?4, ?4) \
                         ON CONFLICT (user_id, service) DO UPDATE SET \
                         connection_data = excluded.connection_data, enabled = 1, \
                         updated_at = excluded.updated_at",
                        params![user_id, SERVICE, sealed, now],
                    )?;
                    Ok(())
                })
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn update_tokens_if_present<'a>(
        &'a self,
        user_id: &'a str,
        connection: &'a SpotifyConnection,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let (user_id, sealed) = (user_id.to_owned(), self.seal(connection)?);
            self.db
                .write("spotify.link.refresh", move |tx| {
                    let changed = tx.execute(
                        "UPDATE user_connections SET connection_data = ?3, updated_at = ?4 \
                         WHERE user_id = ?1 AND service = ?2",
                        params![user_id, SERVICE, sealed, to_iso(now_unix_secs())],
                    )?;
                    Ok(changed > 0)
                })
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn get<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SpotifyConnection>, String>> {
        Box::pin(async move {
            let sealed: Option<String> = sqlx::query_scalar(
                "SELECT connection_data FROM user_connections \
                 WHERE user_id = ?1 AND service = ?2 AND enabled = 1",
            )
            .bind(user_id)
            .bind(SERVICE)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| format!("spotify link read: {error}"))?;
            let Some(sealed) = sealed else {
                return Ok(None);
            };
            // A link that no longer opens (lost or rotated key, damaged
            // row) reads as no link, as in v2: the user connects again.
            let document = match self.crypto.decrypt(&sealed).map(|plain| {
                serde_json::from_str::<LinkDocument>(&plain).map_err(|error| error.to_string())
            }) {
                Ok(Ok(document)) => document,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "spotify link does not decode; treating as unlinked");
                    return Ok(None);
                }
                Err(error) => {
                    tracing::warn!(%error, "spotify link does not open; treating as unlinked");
                    return Ok(None);
                }
            };
            Ok(Some(SpotifyConnection {
                access_token: document.access_token,
                refresh_token: document.refresh_token,
                // An unreadable expiry reads as expired, so the next call
                // refreshes (v2 `_is_expired`).
                expires_at_unix: parse_iso(&document.expires_at).unwrap_or(0),
                username: document.username,
                spotify_user_id: document.spotify_user_id,
            }))
        })
    }

    fn remove<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        let user_id = user_id.to_owned();
        Box::pin(async move {
            self.db
                .write("spotify.link.remove", move |tx| {
                    let changed = tx.execute(
                        "DELETE FROM user_connections WHERE user_id = ?1 AND service = ?2",
                        params![user_id, SERVICE],
                    )?;
                    Ok(changed > 0)
                })
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// Imported playlists as collection playlists.
#[derive(Clone)]
pub struct CollectionsPlaylistBridge {
    playlists: PlaylistStore,
    db: AcquireDb,
}

impl CollectionsPlaylistBridge {
    /// Bridge over the collections playlist store; `db` serves the cheap
    /// source-ref read.
    pub fn new(playlists: PlaylistStore, db: AcquireDb) -> Self {
        Self { playlists, db }
    }
}

impl PlaylistIndex for CollectionsPlaylistBridge {
    fn get_by_source_ref<'a>(
        &'a self,
        source_ref: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            self.playlists
                .find_by_source(user_id, source_ref)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn create<'a>(
        &'a self,
        user_id: &'a str,
        name: &'a str,
        source_ref: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if let Some(id) = self
                .playlists
                .create(user_id, name, Some(source_ref))
                .await
                .map_err(|error| error.to_string())?
            {
                return Ok(id);
            }
            // A racing import made it first: answer that one.
            self.playlists
                .find_by_source(user_id, source_ref)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("playlist for {source_ref} vanished during create"))
        })
    }

    fn source_refs_for<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>, String>> {
        Box::pin(async move {
            sqlx::query_as(
                "SELECT source_ref, id FROM playlists \
                 WHERE user_id = ?1 AND source_ref LIKE 'spotify:%'",
            )
            .bind(user_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| format!("spotify playlist refs read: {error}"))
        })
    }
}

impl PlaylistTrackSink for CollectionsPlaylistBridge {
    fn replace_tracks<'a>(
        &'a self,
        playlist_id: &'a str,
        tracks: &'a [ImportedTrack],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let existing: Vec<String> = self
                .playlists
                .tracks(playlist_id)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|track| track.id)
                .collect();
            if !existing.is_empty() {
                self.playlists
                    .remove(playlist_id, &existing)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            let entries = tracks.iter().map(entry_for).collect();
            match self
                .playlists
                .insert(playlist_id, None, entries)
                .await
                .map_err(|error| error.to_string())?
            {
                Written::Done(_) => Ok(()),
                Written::Missing => Err(format!("playlist {playlist_id} is gone")),
            }
        })
    }

    fn set_imported_cover<'a>(
        &'a self,
        playlist_id: &'a str,
        data: &'a [u8],
        content_type: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            if self
                .playlists
                .cover(playlist_id)
                .await
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Ok(false);
            }
            let cover = CoverImage {
                content_type: content_type.to_owned(),
                bytes: data.to_vec(),
            };
            match self
                .playlists
                .set_cover(playlist_id, cover)
                .await
                .map_err(|error| error.to_string())?
            {
                Written::Done(()) => Ok(true),
                Written::Missing => Err(format!("playlist {playlist_id} is gone")),
            }
        })
    }
}

/// One imported track as a playlist entry.
fn entry_for(track: &ImportedTrack) -> NewEntry {
    NewEntry {
        track_name: track.track_name.clone(),
        artist_name: track.artist_name.clone(),
        album_name: track.album_name.clone(),
        album_id: Some(track.album_id.clone()).filter(|id| !id.is_empty()),
        cover_url: track.cover_url.clone(),
        source_type: track.source_type.clone(),
        track_number: track
            .track_number
            .and_then(|number| i32::try_from(number).ok()),
        disc_number: track
            .disc_number
            .and_then(|number| i32::try_from(number).ok()),
        duration: track.duration.map(|secs| secs as f64),
        ..NewEntry::default()
    }
}
