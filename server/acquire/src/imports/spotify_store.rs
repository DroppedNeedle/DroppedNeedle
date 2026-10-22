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
//!
//! Errors carry a log-only cause.

use std::sync::Arc;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::spotify::ImportedTrack;
use super::spotify_client::{SpotifyError, now_unix_secs};
use super::spotify_oauth::{PendingAuth, STATE_TTL_SECS, SpotifyConnection};
use crate::db::AcquireDb;
use droppedneedle_auth::times::{parse_iso, to_iso};
use droppedneedle_catalog::reads::collections::db::StoreError;
use droppedneedle_catalog::reads::collections::store::PlaylistStore;
use droppedneedle_catalog::reads::collections::store::playlists::{CoverImage, NewEntry, Written};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::runtime_config::crypto::Crypto;

/// The `user_connections.service` tag for Spotify links (v2's).
const SERVICE: &str = "spotify";

/// Single-use authorize states over the shared database.
#[derive(Clone)]
pub struct SqliteSpotifyStates {
    db: AcquireDb,
}

impl SqliteSpotifyStates {
    /// States over the serving database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Remember `state` for [`STATE_TTL_SECS`], pruning expired states.
    pub async fn store_state(&self, state: &str, pending: &PendingAuth) -> Result<(), String> {
        let (state, pending) = (state.to_owned(), pending.clone());
        let clock = self.db.clock().clone();
        self.db
            .write("spotify.state.store", move |tx| {
                let now = now_unix_secs(clock.as_ref());
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
    }

    /// Consume `state` once. Unknown, expired or replayed states read as
    /// none.
    pub async fn consume_state(&self, state: &str) -> Result<Option<PendingAuth>, String> {
        let state = state.to_owned();
        let clock = self.db.clock().clone();
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
                    .filter(|(_, _, expires_at)| *expires_at > now_unix_secs(clock.as_ref()))
                    .map(|(user_id, redirect_uri, _)| PendingAuth {
                        user_id,
                        redirect_uri,
                    }))
            })
            .await
            .map_err(|error| error.to_string())
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

    /// The platform clock the links are stamped with.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        self.db.clock()
    }

    fn seal(&self, link: &SpotifyConnection) -> Result<String, String> {
        let document = serde_json::to_string(&LinkDocument::from(link))
            .map_err(|error| format!("spotify link encode: {error}"))?;
        self.crypto
            .encrypt(&document)
            .map_err(|error| format!("spotify link seal: {error}"))
    }

    /// Insert or replace one user's link (the OAuth callback).
    pub async fn upsert(
        &self,
        user_id: &str,
        connection: &SpotifyConnection,
    ) -> Result<(), String> {
        let (user_id, sealed) = (user_id.to_owned(), self.seal(connection)?);
        let clock = self.db.clock().clone();
        self.db
            .write("spotify.link.upsert", move |tx| {
                let now = to_iso(now_unix_secs(clock.as_ref()));
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
    }

    /// Write refreshed tokens only when the link still exists, so a token
    /// refresh racing a disconnect never brings the link back. Answers
    /// whether a row was updated.
    pub async fn update_tokens_if_present(
        &self,
        user_id: &str,
        connection: &SpotifyConnection,
    ) -> Result<bool, String> {
        let (user_id, sealed) = (user_id.to_owned(), self.seal(connection)?);
        let clock = self.db.clock().clone();
        self.db
            .write("spotify.link.refresh", move |tx| {
                let changed = tx.execute(
                    "UPDATE user_connections SET connection_data = ?3, updated_at = ?4 \
                     WHERE user_id = ?1 AND service = ?2",
                    params![
                        user_id,
                        SERVICE,
                        sealed,
                        to_iso(now_unix_secs(clock.as_ref()))
                    ],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Read one user's link, if any.
    pub async fn get(&self, user_id: &str) -> Result<Option<SpotifyConnection>, String> {
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
    }

    /// Drop one user's link. Answers whether a link was there.
    pub async fn remove(&self, user_id: &str) -> Result<bool, String> {
        let user_id = user_id.to_owned();
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
    }
}

/// Imported playlists as collection playlists, keyed on `spotify:{id}`
/// source refs (v2 `PlaylistService` source-ref half).
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

    /// Internal id for a source ref, if imported before.
    pub async fn get_by_source_ref(
        &self,
        source_ref: &str,
        user_id: &str,
    ) -> Result<Option<String>, String> {
        self.playlists
            .find_by_source(user_id, source_ref)
            .await
            .map_err(|error| error.to_string())
    }

    /// Create a playlist record, or answer the one a racing import made
    /// for the same source ref; returns its internal id.
    pub async fn create(
        &self,
        user_id: &str,
        name: &str,
        source_ref: &str,
    ) -> Result<String, String> {
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
    }

    /// One user's `(source_ref, playlist_id)` pairs, for the imported
    /// mapping.
    pub async fn source_refs_for(&self, user_id: &str) -> Result<Vec<(String, String)>, String> {
        sqlx::query_as(
            "SELECT source_ref, id FROM playlists \
             WHERE user_id = ?1 AND source_ref LIKE 'spotify:%'",
        )
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| format!("spotify playlist refs read: {error}"))
    }

    /// Replace every row on a playlist with `tracks`, in order, in one
    /// transaction: a failed write keeps the rows the playlist had.
    pub async fn replace_tracks(
        &self,
        playlist_id: &str,
        tracks: &[ImportedTrack],
    ) -> Result<(), SpotifyError> {
        let entries = tracks.iter().map(entry_for).collect();
        match self
            .playlists
            .replace_entries(playlist_id, entries)
            .await
            .map_err(|error| match error {
                StoreError::Busy(_) => SpotifyError::Busy,
                other => SpotifyError::Store(other.to_string()),
            })? {
            Written::Done(()) => Ok(()),
            Written::Missing => Err(SpotifyError::Store(format!(
                "playlist {playlist_id} is gone"
            ))),
        }
    }

    /// How many rows a playlist holds.
    pub async fn track_count(&self, playlist_id: &str) -> Result<usize, String> {
        self.playlists
            .tracks(playlist_id)
            .await
            .map(|rows| rows.len())
            .map_err(|error| error.to_string())
    }

    /// Store the picked provider image as the local cover. Answers false
    /// when a cover already exists (v2 `kept_existing`).
    pub async fn set_imported_cover(
        &self,
        playlist_id: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<bool, String> {
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
