//! Spotify playlist import: the owner-only playlist listing and the
//! populate worker the `spotify:import` job runs.
//!
//! Imported playlists persist through the collections playlist store (see
//! [`super::spotify_store`]); the memory stores here are test doubles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::models::{SpotifyAuthUrlResponse, SpotifyPlaylistItem, SpotifyPlaylistListResponse};
use super::spotify_client::{
    SpotifyClient, SpotifyError, TokenRefresher, best_image_url, playlist_track_count,
};
use super::spotify_oauth::{
    SPOTIFY_SCOPES, SpotifyConnection, SpotifyConnectionStore, SpotifySettingsStore, percent_encode,
};

/// The playlists an import keys on `spotify:{id}` source refs (v2
/// `PlaylistService` source-ref half). Errors carry a log-only cause.
pub trait PlaylistIndex: Send + Sync {
    /// Internal id for a source ref, if imported before.
    fn get_by_source_ref<'a>(
        &'a self,
        source_ref: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
    /// Create a playlist record, or answer the one a racing import made
    /// for the same source ref; returns its internal id.
    fn create<'a>(
        &'a self,
        user_id: &'a str,
        name: &'a str,
        source_ref: &'a str,
    ) -> BoxFuture<'a, Result<String, String>>;
    /// One user's `(source_ref, playlist_id)` pairs, for the imported
    /// mapping.
    fn source_refs_for<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>, String>>;
}

/// Track rows the populate replaces wholesale (v2 async-playlist half).
/// Same backing as [`PlaylistIndex`]. Errors carry a log-only cause.
pub trait PlaylistTrackSink: Send + Sync {
    /// Replace every row on a playlist with `tracks`, in order.
    fn replace_tracks<'a>(
        &'a self,
        playlist_id: &'a str,
        tracks: &'a [ImportedTrack],
    ) -> BoxFuture<'a, Result<(), String>>;
    /// Store the picked provider image as the local cover. Answers false
    /// when a cover already exists (v2 `kept_existing`).
    fn set_imported_cover<'a>(
        &'a self,
        playlist_id: &'a str,
        data: &'a [u8],
        content_type: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>>;
}

/// One imported track row (v2 `populate_playlist` dict shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedTrack {
    /// Track title.
    pub track_name: String,
    /// Comma-joined artist names.
    pub artist_name: String,
    /// Album title.
    pub album_name: String,
    /// Resolved MusicBrainz release-group id, else empty.
    pub album_id: String,
    /// Chosen source type; empty until auto-link runs.
    pub source_type: String,
    /// Track number, when Spotify reports one.
    pub track_number: Option<i64>,
    /// Disc number, when Spotify reports one.
    pub disc_number: Option<i64>,
    /// Duration in whole seconds, when Spotify reports one.
    pub duration: Option<i64>,
    /// MB cover URL when resolved, else the Spotify image pick.
    pub cover_url: Option<String>,
}

/// Album-to-MBID resolver (v2 `_resolve_album_mbids` ISRC + title-search
/// fallback). Tests resolve from a fixed map.
pub trait AlbumMbidResolver: Send + Sync {
    /// Resolve one album, if the catalog knows it.
    fn resolve(&self, isrc: Option<&str>, artist: &str, album: &str) -> Option<String>;
}

/// Fixed-map resolver for tests.
#[derive(Debug, Default)]
pub struct FixedMbidResolver {
    inner: Mutex<HashMap<(String, String), String>>,
}

impl FixedMbidResolver {
    /// Empty resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Teach one `(artist, album) -> mbid` row.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed(&self, artist: &str, album: &str, mbid: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((artist.to_owned(), album.to_owned()), mbid.to_owned());
    }
}

impl AlbumMbidResolver for FixedMbidResolver {
    fn resolve(&self, _isrc: Option<&str>, artist: &str, album: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(artist.to_owned(), album.to_owned()))
            .cloned()
    }
}

/// In-memory playlist index, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryPlaylistIndex {
    inner: Mutex<MemoryPlaylists>,
}

/// In-memory playlist rows.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct MemoryPlaylists {
    next: u64,
    by_ref: HashMap<(String, String), String>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryPlaylistIndex {
    /// Empty index.
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> std::sync::MutexGuard<'_, MemoryPlaylists> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PlaylistIndex for MemoryPlaylistIndex {
    fn get_by_source_ref<'a>(
        &'a self,
        source_ref: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        let id = self
            .rows()
            .by_ref
            .get(&(user_id.to_owned(), source_ref.to_owned()))
            .cloned();
        Box::pin(async move { Ok(id) })
    }

    fn create<'a>(
        &'a self,
        user_id: &'a str,
        _name: &'a str,
        source_ref: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        let mut rows = self.rows();
        let key = (user_id.to_owned(), source_ref.to_owned());
        let id = match rows.by_ref.get(&key) {
            Some(existing) => existing.clone(),
            None => {
                rows.next += 1;
                let id = format!("pl-{}", rows.next);
                rows.by_ref.insert(key, id.clone());
                id
            }
        };
        Box::pin(async move { Ok(id) })
    }

    fn source_refs_for<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>, String>> {
        let refs = self
            .rows()
            .by_ref
            .iter()
            .filter(|((owner, _), _)| owner == user_id)
            .map(|((_, source_ref), id)| (source_ref.clone(), id.clone()))
            .collect();
        Box::pin(async move { Ok(refs) })
    }
}

/// In-memory track rows plus covers, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryTrackSink {
    inner: Mutex<MemoryTracks>,
}

/// In-memory track rows.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct MemoryTracks {
    rows: HashMap<String, Vec<ImportedTrack>>,
    covers: HashMap<String, (Vec<u8>, String)>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryTrackSink {
    /// Empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rows currently on a playlist.
    pub fn tracks(&self, playlist_id: &str) -> Vec<ImportedTrack> {
        self.lock()
            .rows
            .get(playlist_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Stored cover bytes and type for a playlist, if any.
    pub fn cover(&self, playlist_id: &str) -> Option<(Vec<u8>, String)> {
        self.lock().covers.get(playlist_id).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryTracks> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PlaylistTrackSink for MemoryTrackSink {
    fn replace_tracks<'a>(
        &'a self,
        playlist_id: &'a str,
        tracks: &'a [ImportedTrack],
    ) -> BoxFuture<'a, Result<(), String>> {
        self.lock()
            .rows
            .insert(playlist_id.to_owned(), tracks.to_vec());
        Box::pin(async { Ok(()) })
    }

    fn set_imported_cover<'a>(
        &'a self,
        playlist_id: &'a str,
        data: &'a [u8],
        content_type: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        let mut inner = self.lock();
        let stored = if inner.covers.contains_key(playlist_id) {
            false
        } else {
            inner.covers.insert(
                playlist_id.to_owned(),
                (data.to_vec(), content_type.to_owned()),
            );
            true
        };
        Box::pin(async move { Ok(stored) })
    }
}

/// Spotify import service: OAuth helpers, playlist listing, and the
/// populate worker the `spotify:import` job runs (v2
/// `SpotifyImportService`).
pub struct SpotifyImportService {
    client: SpotifyClient,
    settings: Arc<dyn SpotifySettingsStore>,
    connections: Arc<dyn SpotifyConnectionStore>,
    playlists: Arc<dyn PlaylistIndex>,
    tracks: Arc<dyn PlaylistTrackSink>,
    resolver: Arc<dyn AlbumMbidResolver>,
}

/// Cover outcome the populate degrades on, never fails on.
#[derive(Debug, Clone, PartialEq)]
pub enum CoverOutcome {
    /// Stored the fetched bytes.
    Stored,
    /// A cover already existed; kept it.
    KeptExisting,
    /// Unusable or failed; the import is untouched.
    Skipped(String),
}

impl SpotifyImportService {
    /// Wire the service from its parts.
    pub fn new(
        client: SpotifyClient,
        settings: Arc<dyn SpotifySettingsStore>,
        connections: Arc<dyn SpotifyConnectionStore>,
        playlists: Arc<dyn PlaylistIndex>,
        tracks: Arc<dyn PlaylistTrackSink>,
        resolver: Arc<dyn AlbumMbidResolver>,
    ) -> Self {
        Self {
            client,
            settings,
            connections,
            playlists,
            tracks,
            resolver,
        }
    }

    /// Build the authorize URL for one user and state (v2
    /// `spotify_auth_url`). Requires the admin app to be enabled and
    /// complete; else the v2 400 text.
    pub fn auth_url(
        &self,
        redirect_uri: &str,
        state: &str,
    ) -> Result<SpotifyAuthUrlResponse, String> {
        let raw = self.settings.get_raw();
        if !raw.enabled || raw.client_id.is_empty() || raw.client_secret.expose().is_empty() {
            return Err("Spotify is not configured by the administrator".to_owned());
        }
        let query = [
            ("client_id", raw.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("scope", SPOTIFY_SCOPES),
            ("state", state),
        ]
        .iter()
        .map(|(key, value)| format!("{key}={}", percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
        Ok(SpotifyAuthUrlResponse {
            auth_url: format!("https://accounts.spotify.com/authorize?{query}"),
        })
    }

    /// One user's stored link, or `NotLinked`.
    async fn link(&self, user_id: &str) -> Result<SpotifyConnection, SpotifyError> {
        self.connections
            .get(user_id)
            .await
            .map_err(SpotifyError::Store)?
            .ok_or(SpotifyError::NotLinked)
    }

    /// Refreshing API access over one user's link (v2 `_get_client`).
    fn refresher(&self, user_id: &str, connection: SpotifyConnection) -> TokenRefresher<'_> {
        let raw = self.settings.get_raw();
        TokenRefresher::new(
            &self.client,
            self.connections.as_ref(),
            user_id,
            &raw.client_id,
            raw.client_secret.expose(),
            connection,
        )
    }

    /// Owned playlists annotated with the imported mapping (v2
    /// `list_playlists`). Only playlists whose owner id matches the
    /// caller's Spotify id are listed; the id is fetched from `/me` when
    /// the link row does not carry it yet.
    pub async fn list_playlists(
        &self,
        user_id: &str,
    ) -> Result<SpotifyPlaylistListResponse, SpotifyError> {
        let stored = self.link(user_id).await?;
        let mut spotify_user_id = stored.spotify_user_id.clone();
        let refresher = self.refresher(user_id, stored);
        if spotify_user_id.is_empty() {
            let me = self.client.current_user(&refresher).await?;
            spotify_user_id = me
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_owned();
        }
        let raw = self.client.user_playlists(&refresher).await?;

        let mut imported: HashMap<String, String> = HashMap::new();
        let refs = self
            .playlists
            .source_refs_for(user_id)
            .await
            .map_err(SpotifyError::Store)?;
        for (source_ref, id) in refs {
            if let Some(pid) = source_ref.strip_prefix("spotify:") {
                imported.insert(pid.to_owned(), id);
            }
        }

        let mut playlists = Vec::new();
        for item in &raw {
            let owner = item
                .get("owner")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if owner.get("id").and_then(|id| id.as_str()).unwrap_or("") != spotify_user_id {
                continue;
            }
            let pid = item
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_owned();
            let images = item
                .get("images")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            playlists.push(SpotifyPlaylistItem {
                id: pid.clone(),
                name: item
                    .get("name")
                    .and_then(|name| name.as_str())
                    .unwrap_or("")
                    .to_owned(),
                description: item
                    .get("description")
                    .and_then(|description| description.as_str())
                    .unwrap_or("")
                    .to_owned(),
                track_count: playlist_track_count(item),
                cover_url: best_image_url(&images, 250),
                owner: owner
                    .get("display_name")
                    .and_then(|name| name.as_str())
                    .unwrap_or("")
                    .to_owned(),
                imported_playlist_id: imported.get(&pid).cloned(),
            });
        }
        Ok(SpotifyPlaylistListResponse { playlists })
    }

    /// Ensure the internal record for one Spotify playlist (v2
    /// `ensure_playlist_record`): the existing id wins, else a record named
    /// `name` or `Spotify Playlist` under `spotify:{id}`.
    pub async fn ensure_playlist_record(
        &self,
        user_id: &str,
        spotify_playlist_id: &str,
        name: &str,
    ) -> Result<String, SpotifyError> {
        let source_ref = format!("spotify:{spotify_playlist_id}");
        if let Some(existing) = self
            .playlists
            .get_by_source_ref(&source_ref, user_id)
            .await
            .map_err(SpotifyError::Store)?
        {
            return Ok(existing);
        }
        let name = if name.is_empty() {
            "Spotify Playlist".to_owned()
        } else {
            name.to_owned()
        };
        self.playlists
            .create(user_id, &name, &source_ref)
            .await
            .map_err(SpotifyError::Store)
    }

    /// Populate one playlist: fetch metadata + tracks, resolve MBIDs,
    /// replace the track rows wholesale, then persist the cover
    /// best-effort (v2 `populate_playlist`). Returns the tracks written.
    pub async fn populate_playlist(
        &self,
        user_id: &str,
        spotify_playlist_id: &str,
        playlist_id: &str,
    ) -> Result<usize, SpotifyError> {
        let refresher = self.refresher(user_id, self.link(user_id).await?);
        let (info, raw_tracks) = tokio::join!(
            self.client.playlist(&refresher, spotify_playlist_id),
            self.client.playlist_tracks(&refresher, spotify_playlist_id)
        );
        let info = info?;
        let raw_tracks = raw_tracks?;

        let mut album_mbid: HashMap<String, Option<String>> = HashMap::new();
        for track in &raw_tracks {
            let album = track
                .get("album")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let album_id = album.get("id").and_then(|id| id.as_str()).unwrap_or("");
            if album_id.is_empty() || album_mbid.contains_key(album_id) {
                continue;
            }
            let isrc = track
                .get("external_ids")
                .and_then(|ids| ids.get("isrc"))
                .and_then(|isrc| isrc.as_str());
            let artist = joined_artists(track);
            let album_name = album
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or("");
            album_mbid.insert(
                album_id.to_owned(),
                self.resolver.resolve(isrc, &artist, album_name),
            );
        }

        let mut rows = Vec::with_capacity(raw_tracks.len());
        for track in &raw_tracks {
            let album = track
                .get("album")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let album_spotify_id = album.get("id").and_then(|id| id.as_str()).unwrap_or("");
            let mbid = album_mbid.get(album_spotify_id).cloned().flatten();
            let cover_url = match &mbid {
                Some(mbid) => Some(format!("/api/v1/covers/release-group/{mbid}?size=250")),
                None => {
                    let images = album
                        .get("images")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    best_image_url(&images, 250)
                }
            };
            let duration_ms = track.get("duration_ms").and_then(|ms| ms.as_i64());
            rows.push(ImportedTrack {
                track_name: track
                    .get("name")
                    .and_then(|name| name.as_str())
                    .unwrap_or("")
                    .to_owned(),
                artist_name: joined_artists(track),
                album_name: album
                    .get("name")
                    .and_then(|name| name.as_str())
                    .unwrap_or("")
                    .to_owned(),
                album_id: mbid.unwrap_or_default(),
                source_type: String::new(),
                track_number: track.get("track_number").and_then(|number| number.as_i64()),
                disc_number: track.get("disc_number").and_then(|number| number.as_i64()),
                duration: duration_ms.map(|ms| ms / 1000),
                cover_url,
            });
        }
        let written = rows.len();
        self.tracks
            .replace_tracks(playlist_id, &rows)
            .await
            .map_err(SpotifyError::Store)?;
        if let CoverOutcome::Skipped(reason) = self
            .persist_cover(spotify_playlist_id, playlist_id, &info)
            .await
        {
            tracing::debug!(%reason, "spotify playlist imported without a cover");
        }
        Ok(written)
    }

    /// Store the picked provider image as the local cover. Any failure
    /// degrades and leaves the import untouched: artwork must never fail a
    /// playlist import, and no cover is normal (v2 `_persist_playlist_cover`).
    pub async fn persist_cover(
        &self,
        spotify_playlist_id: &str,
        playlist_id: &str,
        info: &serde_json::Value,
    ) -> CoverOutcome {
        let images = info
            .get("images")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let Some(cover_url) = best_image_url(&images, 250) else {
            return CoverOutcome::Skipped("no cover offered".to_owned());
        };
        let fetched = match self.client.fetch_cover(&cover_url).await {
            Ok(fetched) => fetched,
            Err(cause) => {
                return CoverOutcome::Skipped(format!(
                    "cover fetch failed for {spotify_playlist_id}: {cause:?}"
                ));
            }
        };
        let Some((data, content_type)) = fetched else {
            return CoverOutcome::Skipped(format!(
                "cover rejected for {spotify_playlist_id} ({cover_url})"
            ));
        };
        match self
            .tracks
            .set_imported_cover(playlist_id, &data, &content_type)
            .await
        {
            Ok(true) => CoverOutcome::Stored,
            Ok(false) => CoverOutcome::KeptExisting,
            Err(cause) => CoverOutcome::Skipped(format!("cover write failed: {cause}")),
        }
    }
}

/// Comma-joined track artist names (v2 populate loop).
fn joined_artists(track: &serde_json::Value) -> String {
    track
        .get("artists")
        .and_then(|artists| artists.as_array())
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| artist.get("name"))
                .filter_map(|name| name.as_str())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}
