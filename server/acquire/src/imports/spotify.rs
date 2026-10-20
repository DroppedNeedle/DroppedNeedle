//! Spotify playlist import: the owner-only playlist listing and the
//! populate worker the `spotify:import` job runs.
//!
//! Imported playlists persist through the collections playlist store (see
//! [`super::spotify_store`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::models::{SpotifyAuthUrlResponse, SpotifyPlaylistItem, SpotifyPlaylistListResponse};
use super::spotify_client::{
    SpotifyClient, SpotifyError, TokenRefresher, best_image_url, playlist_track_count,
};
use super::spotify_models::{EntryTrack, PlaylistInfo};
use super::spotify_oauth::{SpotifyConnection, authorize_url};
use super::spotify_store::{CollectionsPlaylistBridge, SqliteSpotifyLinks};
use crate::settings::ConfigSpotifySettings;

/// One imported track row (v2 `populate_playlist` dict shape).
#[derive(Debug, Clone, PartialEq)]
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

/// Spotify import service: OAuth helpers, playlist listing, and the
/// populate worker the `spotify:import` job runs (v2
/// `SpotifyImportService`).
pub struct SpotifyImportService {
    client: SpotifyClient,
    settings: Arc<ConfigSpotifySettings>,
    connections: Arc<SqliteSpotifyLinks>,
    playlists: Arc<CollectionsPlaylistBridge>,
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
        settings: Arc<ConfigSpotifySettings>,
        connections: Arc<SqliteSpotifyLinks>,
        playlists: Arc<CollectionsPlaylistBridge>,
        resolver: Arc<dyn AlbumMbidResolver>,
    ) -> Self {
        Self {
            client,
            settings,
            connections,
            playlists,
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
        Ok(SpotifyAuthUrlResponse {
            auth_url: authorize_url(&raw.client_id, redirect_uri, state),
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
            spotify_user_id = self.client.current_user(&refresher).await?.id;
        }
        let listed = self.client.user_playlists(&refresher).await?;

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

        let playlists = listed
            .into_iter()
            .filter(|playlist| playlist.owner.id == spotify_user_id)
            .map(|playlist| SpotifyPlaylistItem {
                track_count: playlist_track_count(&playlist),
                cover_url: best_image_url(playlist.images.as_deref(), 250),
                imported_playlist_id: imported.get(&playlist.id).cloned(),
                owner: playlist.owner.display_name.unwrap_or_default(),
                description: playlist.description.unwrap_or_default(),
                name: playlist.name,
                id: playlist.id,
            })
            .collect();
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
            "Spotify Playlist"
        } else {
            name
        };
        self.playlists
            .create(user_id, name, &source_ref)
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
        let (info, tracks) = tokio::join!(
            self.client.playlist(&refresher, spotify_playlist_id),
            self.client.playlist_tracks(&refresher, spotify_playlist_id)
        );
        let info = info?;
        let tracks = tracks?;

        let mut album_mbid: HashMap<String, Option<String>> = HashMap::new();
        for track in &tracks {
            let Some(album) = &track.album else { continue };
            let album_id = album.id.as_deref().unwrap_or("");
            if album_id.is_empty() || album_mbid.contains_key(album_id) {
                continue;
            }
            let isrc = track
                .external_ids
                .as_ref()
                .and_then(|ids| ids.isrc.as_deref());
            let album_name = album.name.as_deref().unwrap_or("");
            album_mbid.insert(
                album_id.to_owned(),
                self.resolver
                    .resolve(isrc, &joined_artists(track), album_name),
            );
        }

        let rows: Vec<ImportedTrack> = tracks
            .iter()
            .map(|track| {
                let album = track.album.as_ref();
                let mbid = album
                    .and_then(|album| album.id.as_deref())
                    .and_then(|id| album_mbid.get(id).cloned().flatten());
                let cover_url = match &mbid {
                    Some(mbid) => Some(format!("/api/v1/covers/release-group/{mbid}?size=250")),
                    None => best_image_url(album.and_then(|album| album.images.as_deref()), 250),
                };
                ImportedTrack {
                    track_name: track.name.clone(),
                    artist_name: joined_artists(track),
                    album_name: album
                        .and_then(|album| album.name.clone())
                        .unwrap_or_default(),
                    album_id: mbid.unwrap_or_default(),
                    source_type: String::new(),
                    track_number: track.track_number,
                    disc_number: track.disc_number,
                    duration: track.duration_ms.map(|ms| ms / 1000),
                    cover_url,
                }
            })
            .collect();
        let written = rows.len();
        self.playlists
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
        info: &PlaylistInfo,
    ) -> CoverOutcome {
        let Some(cover_url) = best_image_url(info.images.as_deref(), 250) else {
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
            .playlists
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
fn joined_artists(track: &EntryTrack) -> String {
    track
        .artists
        .iter()
        .filter_map(|artist| artist.name.as_deref())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}
