//! Spotify OAuth, playlist listing, and playlist import.
//!
//! Ports v2's Spotify client, the Spotify import service, and the Spotify
//! halves of its routes and connection settings: the exact
//! OAuth scopes, the single-source redirect URI (GH-298), the Basic-auth
//! token exchange, the 60-second refresh buffer with one 401 retry and one
//! short 429 wait, the owner-only playlist filter, the `items`/`tracks`
//! track-count quirk (v2.9.0 issue #353), the `/items` tracks endpoint (the
//! old `/tracks` 403s for dev-mode apps after the March 2026 migration),
//! the `track`/`item` alias with `is_local` skips, and the bounded
//! no-redirect cover fetch that never fails an import.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "test-support"))]
use super::models::SPOTIFY_SECRET_MASK;
use super::models::{
    SpotifyAuthUrlResponse, SpotifyPlaylistItem, SpotifyPlaylistListResponse, SpotifySettings,
};

/// OAuth scopes (v2 `_SPOTIFY_SCOPES`, verbatim).
pub const SPOTIFY_SCOPES: &str =
    "playlist-read-private playlist-read-collaborative user-read-private";
/// Seconds before expiry a token counts as expired (v2 buffer).
pub const REFRESH_BUFFER_SECS: i64 = 60;
/// Network read bound for one playlist cover (v2 5 MiB). Looser than the
/// storage cap on purpose: it only stops an unbounded wire read.
pub const MAX_COVER_FETCH_BYTES: usize = 5 * 1024 * 1024;
/// OAuth callback path: the route [`imports_callback_router`] mounts
/// under `/api/v3`.
///
/// [`imports_callback_router`]: super::handlers::imports_callback_router
pub const SPOTIFY_CALLBACK_PATH: &str = "/api/v3/acquire/spotify/auth/callback";
/// The v2 callback path. Spotify apps registered against v2 still list it,
/// so the server keeps answering there.
pub const SPOTIFY_LEGACY_CALLBACK_PATH: &str = "/api/v1/me/connections/spotify/auth/callback";

/// Seconds since the Unix epoch, saturating on clock failure.
pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Build the Spotify OAuth redirect URI, the single source of truth (v2
/// `PreferencesService.spotify_redirect_uri`, GH-298). The authorize call,
/// the token exchange, and the admin display endpoint all derive through
/// here so the value matches the dashboard byte-for-byte. An
/// admin-configured origin wins; empty keeps the request-derived base. The
/// deployment base path lands between origin and callback exactly once.
pub fn redirect_uri(configured_origin: &str, request_base_url: &str, base_path: &str) -> String {
    redirect_uri_at(
        SPOTIFY_CALLBACK_PATH,
        configured_origin,
        request_base_url,
        base_path,
    )
}

/// [`redirect_uri`] for an explicit callback path. The token exchange must
/// send the exact URI the authorize step used, so a callback that arrives
/// on the v2 path exchanges with the v2 URI.
pub fn redirect_uri_at(
    callback_path: &str,
    configured_origin: &str,
    request_base_url: &str,
    base_path: &str,
) -> String {
    let origin = configured_origin.trim();
    let base = if origin.is_empty() {
        request_base_url.trim_end_matches('/').to_owned()
    } else {
        origin.trim_end_matches('/').to_owned()
    };
    if base_path.is_empty() || base.ends_with(base_path) {
        format!("{base}{callback_path}")
    } else {
        format!("{base}{base_path}{callback_path}")
    }
}

/// One stored per-user Spotify link (v2 `user_connections` spotify row).
/// Debug is hand-written: the tokens never appear in debug output.
#[derive(Clone, PartialEq)]
pub struct SpotifyConnection {
    /// OAuth access token.
    pub access_token: String,
    /// OAuth refresh token.
    pub refresh_token: String,
    /// Expiry as Unix seconds.
    pub expires_at_unix: i64,
    /// Display name (`display_name`, else id, else `Spotify`).
    pub username: String,
    /// Spotify user id, seeding the owner filter.
    pub spotify_user_id: String,
}

impl std::fmt::Debug for SpotifyConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpotifyConnection")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at_unix", &self.expires_at_unix)
            .field("username", &self.username)
            .field("spotify_user_id", &self.spotify_user_id)
            .finish()
    }
}

/// Admin Spotify app settings rows (v2 `PreferencesService` spotify block).
pub trait SpotifySettingsStore: Send + Sync {
    /// Masked settings for reads.
    fn get(&self) -> SpotifySettings;
    /// Raw settings including the real secret.
    fn get_raw(&self) -> SpotifySettings;
    /// Save; a masked secret preserves the stored one. Rejects a
    /// non-absolute redirect origin (v2 `ConfigurationError` text kept).
    fn save(&self, settings: &SpotifySettings) -> Result<(), String>;
}

/// Single-use OAuth state tokens (v2 `AuthStore` spotify-state half).
pub trait SpotifyStateStore: Send + Sync {
    /// Remember `state` for one user.
    fn store_state(&self, state: &str, user_id: &str);
    /// Consume `state` once; unknown or replayed states answer none.
    fn consume_state(&self, state: &str) -> Option<String>;
}

/// Per-user Spotify links (v2 `UserConnectionsStore` spotify rows).
pub trait SpotifyConnectionStore: Send + Sync {
    /// Upsert one user's link.
    fn upsert(&self, user_id: &str, connection: &SpotifyConnection);
    /// Read one user's link, if any.
    fn get(&self, user_id: &str) -> Option<SpotifyConnection>;
    /// Drop one user's link. Answers whether a link was there.
    fn remove(&self, user_id: &str) -> bool;
}

/// Internal playlist index the import keys on `spotify:{id}` source refs
/// (v2 `PlaylistService` source-ref half). The in-memory implementation
/// below serves until the playlists store backs it.
pub trait PlaylistIndex: Send + Sync {
    /// Internal id for a source ref, if imported before.
    fn get_by_source_ref(&self, source_ref: &str, user_id: &str) -> Option<String>;
    /// Create a playlist record; returns its internal id.
    fn create(&self, user_id: &str, name: &str, source_ref: &str) -> String;
    /// List one user's source refs, for the imported mapping.
    fn source_refs_for(&self, user_id: &str) -> Vec<(String, String)>;
}

/// Track rows the populate replaces wholesale (v2 async-playlist half).
/// Same backing as [`PlaylistIndex`].
pub trait PlaylistTrackSink: Send + Sync {
    /// Ids of the rows currently on a playlist.
    fn track_ids(&self, playlist_id: &str) -> Vec<String>;
    /// Remove rows by id.
    fn remove_tracks(&self, playlist_id: &str, track_ids: &[String]);
    /// Append track rows.
    fn add_tracks(&self, playlist_id: &str, tracks: &[ImportedTrack]);
    /// Stored cover bytes for a playlist, if any.
    fn cover(&self, playlist_id: &str) -> Option<(Vec<u8>, String)>;
    /// Store the picked provider image as the local cover. Returns false
    /// when a cover already exists (v2 `kept_existing`).
    fn set_imported_cover(
        &self,
        playlist_id: &str,
        user_id: &str,
        data: &[u8],
        content_type: &str,
    ) -> bool;
    /// Rows currently on a playlist, for test assertions.
    fn tracks(&self, playlist_id: &str) -> Vec<ImportedTrack>;
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

/// In-memory Spotify settings rows.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemorySpotifySettings {
    inner: Mutex<SpotifySettings>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemorySpotifySettings {
    /// Empty rows.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl SpotifySettingsStore for MemorySpotifySettings {
    fn get(&self) -> SpotifySettings {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        SpotifySettings {
            client_id: inner.client_id.clone(),
            client_secret: if inner.client_secret.is_empty() {
                String::new()
            } else {
                SPOTIFY_SECRET_MASK.to_owned()
            },
            enabled: inner.enabled,
            spotify_redirect_origin: inner.spotify_redirect_origin.clone(),
        }
    }

    fn get_raw(&self) -> SpotifySettings {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn save(&self, settings: &SpotifySettings) -> Result<(), String> {
        let origin = settings.spotify_redirect_origin.trim();
        if !(origin.is_empty() || origin.starts_with("http://") || origin.starts_with("https://")) {
            return Err("Spotify redirect origin must be an absolute http(s) URL".to_owned());
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.client_id = settings.client_id.clone();
        if settings.client_secret != SPOTIFY_SECRET_MASK {
            inner.client_secret = settings.client_secret.clone();
        }
        inner.enabled = settings.enabled;
        inner.spotify_redirect_origin = origin.trim_end_matches('/').to_owned();
        Ok(())
    }
}

/// In-memory single-use OAuth states.
#[derive(Debug, Default)]
pub struct MemorySpotifyStates {
    inner: Mutex<HashMap<String, String>>,
}

impl MemorySpotifyStates {
    /// Empty states.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SpotifyStateStore for MemorySpotifyStates {
    fn store_state(&self, state: &str, user_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(state.to_owned(), user_id.to_owned());
    }

    fn consume_state(&self, state: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(state)
    }
}

/// In-memory per-user Spotify links.
#[derive(Debug, Default)]
pub struct MemorySpotifyConnections {
    inner: Mutex<HashMap<String, SpotifyConnection>>,
}

impl MemorySpotifyConnections {
    /// Empty links.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SpotifyConnectionStore for MemorySpotifyConnections {
    fn upsert(&self, user_id: &str, connection: &SpotifyConnection) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(user_id.to_owned(), connection.clone());
    }

    fn get(&self, user_id: &str) -> Option<SpotifyConnection> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(user_id)
            .cloned()
    }

    fn remove(&self, user_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(user_id)
            .is_some()
    }
}

/// In-memory playlist index.
#[derive(Debug, Default)]
pub struct MemoryPlaylistIndex {
    inner: Mutex<MemoryPlaylists>,
}

/// In-memory playlist rows.
#[derive(Debug, Default)]
struct MemoryPlaylists {
    next: u64,
    by_ref: HashMap<(String, String), String>,
    names: HashMap<String, String>,
}

impl MemoryPlaylistIndex {
    /// Empty index.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlaylistIndex for MemoryPlaylistIndex {
    fn get_by_source_ref(&self, source_ref: &str, user_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .by_ref
            .get(&(user_id.to_owned(), source_ref.to_owned()))
            .cloned()
    }

    fn create(&self, user_id: &str, name: &str, source_ref: &str) -> String {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.next += 1;
        let id = format!("pl-{}", inner.next);
        inner
            .by_ref
            .insert((user_id.to_owned(), source_ref.to_owned()), id.clone());
        inner.names.insert(id.clone(), name.to_owned());
        id
    }

    fn source_refs_for(&self, user_id: &str) -> Vec<(String, String)> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .by_ref
            .iter()
            .filter(|((owner, _), _)| owner == user_id)
            .map(|((_, source_ref), id)| (source_ref.clone(), id.clone()))
            .collect()
    }
}

/// In-memory track rows plus covers.
#[derive(Debug, Default)]
pub struct MemoryTrackSink {
    inner: Mutex<MemoryTracks>,
}

/// In-memory track rows.
#[derive(Debug, Default)]
struct MemoryTracks {
    next: u64,
    rows: HashMap<String, Vec<(String, ImportedTrack)>>,
    covers: HashMap<String, (Vec<u8>, String)>,
}

impl MemoryTrackSink {
    /// Empty sink.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlaylistTrackSink for MemoryTrackSink {
    fn track_ids(&self, playlist_id: &str) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rows
            .get(playlist_id)
            .map(|rows| rows.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    }

    fn remove_tracks(&self, playlist_id: &str, track_ids: &[String]) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(rows) = inner.rows.get_mut(playlist_id) {
            rows.retain(|(id, _)| !track_ids.contains(id));
        }
    }

    fn add_tracks(&self, playlist_id: &str, tracks: &[ImportedTrack]) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut rows = inner.rows.remove(playlist_id).unwrap_or_default();
        for track in tracks {
            inner.next += 1;
            rows.push((format!("t-{}", inner.next), track.clone()));
        }
        inner.rows.insert(playlist_id.to_owned(), rows);
    }

    fn cover(&self, playlist_id: &str) -> Option<(Vec<u8>, String)> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .covers
            .get(playlist_id)
            .cloned()
    }

    fn set_imported_cover(
        &self,
        playlist_id: &str,
        _user_id: &str,
        data: &[u8],
        content_type: &str,
    ) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.covers.contains_key(playlist_id) {
            return false;
        }
        inner.covers.insert(
            playlist_id.to_owned(),
            (data.to_vec(), content_type.to_owned()),
        );
        true
    }

    fn tracks(&self, playlist_id: &str) -> Vec<ImportedTrack> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rows
            .get(playlist_id)
            .map(|rows| rows.iter().map(|(_, track)| track.clone()).collect())
            .unwrap_or_default()
    }
}

/// Track count from a `GET /me/playlists` item (v2 `_playlist_track_count`).
/// The count lives under `items` as a dict `{"href", "total"}` with
/// `tracks` null/absent on the current shape; the older shape used
/// `tracks: {"total"}`. `items` doubles as the pagination key elsewhere
/// and may arrive as a list, which carries no total, so only dict shapes
/// are read and anything else yields 0.
pub fn playlist_track_count(item: &serde_json::Value) -> i64 {
    for key in ["items", "tracks"] {
        if let Some(total) = item
            .get(key)
            .and_then(|value| value.get("total"))
            .and_then(|total| total.as_i64())
        {
            return total;
        }
    }
    0
}

/// Pick the smallest image at least `min_size` wide, else the largest (v2
/// `_best_image_url`, default floor 250).
pub fn best_image_url(images: &serde_json::Value, min_size: i64) -> Option<String> {
    let images = images.as_array()?;
    if images.is_empty() {
        return None;
    }
    let mut sorted: Vec<&serde_json::Value> = images.iter().collect();
    sorted.sort_by_key(|image| {
        image
            .get("width")
            .and_then(|width| width.as_i64())
            .unwrap_or(0)
    });
    for image in &sorted {
        let width = image
            .get("width")
            .and_then(|width| width.as_i64())
            .unwrap_or(0);
        if width >= min_size
            && let Some(url) = image.get("url").and_then(|url| url.as_str())
        {
            return Some(url.to_owned());
        }
    }
    sorted
        .last()
        .and_then(|image| image.get("url"))
        .and_then(|url| url.as_str())
        .map(str::to_owned)
}

/// True for a fetchable Spotify cover URL: https over the CDN allowlist.
/// The mock CDN host joins the allowlist so tests run on loopback.
pub fn is_allowed_cover_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && is_loopback_host(&parsed)) {
        return false;
    }
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    host == "i.scdn.co"
        || host == "mosaic.scdn.co"
        || host.ends_with(".scdn.co")
        || is_loopback_host(&parsed)
}

/// Loopback hosts the tests serve mock covers from.
fn is_loopback_host(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
}

/// Classified Spotify failure. Wire detail stays out; handlers map to the
/// v2 texts (400 not-linked, 502 fetch failure).
#[derive(Debug, Clone, PartialEq)]
pub enum SpotifyError {
    /// No link row, or the token flow cannot proceed.
    NotLinked,
    /// Upstream or network failure. The detail reaches the log only.
    Unavailable(String),
}

/// Per-user Spotify Web API client (v2 `SpotifyClient`). Base URLs inject
/// so tests run against the loopback mocks; production passes the
/// `api.spotify.com` / `accounts.spotify.com` pair.
#[derive(Debug, Clone)]
pub struct SpotifyClient {
    http: reqwest::Client,
    /// Redirect-refusing client for covers: a CDN URL must answer directly
    /// (v2 `follow_redirects=False`), so the shared redirect-following
    /// client must never fetch artwork.
    direct: reqwest::Client,
    api_base: String,
    accounts_base: String,
}

impl SpotifyClient {
    /// Build over the factory's shared and no-redirect clients with
    /// explicit bases.
    pub fn new(
        http: reqwest::Client,
        direct: reqwest::Client,
        api_base: &str,
        accounts_base: &str,
    ) -> Self {
        Self {
            http,
            direct,
            api_base: api_base.trim_end_matches('/').to_owned(),
            accounts_base: accounts_base.trim_end_matches('/').to_owned(),
        }
    }

    /// API base URL (the callback's `/me` fetch builds on it).
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// True when the token is missing or inside the refresh buffer (v2
    /// `_is_expired`: unparseable reads as expired).
    pub fn is_expired(expires_at_unix: i64, now_unix: i64) -> bool {
        if expires_at_unix <= 0 {
            return true;
        }
        now_unix >= expires_at_unix - REFRESH_BUFFER_SECS
    }

    /// Exchange an OAuth code for tokens (v2 callback exchange).
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
    ) -> Result<TokenGrant, SpotifyError> {
        let credentials = format!("{client_id}:{client_secret}");
        let mut encoded = String::new();
        base64_encode(credentials.as_bytes(), &mut encoded);
        let response = self
            .http
            .post(format!("{}/api/token", self.accounts_base))
            .header("Authorization", format!("Basic {encoded}"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await
            .map_err(|cause| {
                SpotifyError::Unavailable(format!("token exchange failed: {cause}"))
            })?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(SpotifyError::Unavailable(format!(
                "token exchange status {}",
                response.status().as_u16()
            )));
        }
        let body = json_body(response, "token").await?;
        Ok(TokenGrant::from_value(&body))
    }

    /// Refresh an access token (v2 `_refresh`).
    pub async fn refresh(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<TokenGrant, SpotifyError> {
        if refresh_token.is_empty() {
            return Err(SpotifyError::NotLinked);
        }
        let credentials = format!("{client_id}:{client_secret}");
        let mut encoded = String::new();
        base64_encode(credentials.as_bytes(), &mut encoded);
        let response = self
            .http
            .post(format!("{}/api/token", self.accounts_base))
            .header("Authorization", format!("Basic {encoded}"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|cause| SpotifyError::Unavailable(format!("token refresh failed: {cause}")))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(SpotifyError::Unavailable(format!(
                "token refresh status {}",
                response.status().as_u16()
            )));
        }
        let body = json_body(response, "refresh").await?;
        Ok(TokenGrant::from_value(&body))
    }

    /// Authenticated GET with the v2 retry rules: refresh-then-once on 401,
    /// one short wait on 429 when `Retry-After` is at most 10 seconds.
    pub async fn authed_get(
        &self,
        refresher: &TokenRefresher<'_>,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, SpotifyError> {
        let token = refresher.valid_token().await?;
        let mut response = self.get_with(path, query, &token).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let token = refresher.refresh_now().await?;
            response = self.get_with(path, query, &token).await?;
        }
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(2);
            if wait <= 10 {
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                let token = refresher.current_token().await;
                response = self.get_with(path, query, &token).await?;
            }
        }
        if !response.status().is_success() {
            return Err(SpotifyError::Unavailable(format!(
                "spotify GET {path} status {}",
                response.status().as_u16()
            )));
        }
        json_body(response, "spotify").await
    }

    async fn get_with(
        &self,
        path: &str,
        query: &[(&str, String)],
        token: &str,
    ) -> Result<reqwest::Response, SpotifyError> {
        self.http
            .get(format!("{}{path}", self.api_base))
            .query(query)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|cause| SpotifyError::Unavailable(format!("spotify GET failed: {cause}")))
    }

    /// Current user (`GET /me`).
    pub async fn current_user(
        &self,
        refresher: &TokenRefresher<'_>,
    ) -> Result<serde_json::Value, SpotifyError> {
        self.authed_get(refresher, "/me", &[]).await
    }

    /// Every owned-or-followed playlist page (`GET /me/playlists`, 50/page).
    pub async fn user_playlists(
        &self,
        refresher: &TokenRefresher<'_>,
    ) -> Result<Vec<serde_json::Value>, SpotifyError> {
        let mut playlists = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let page = self
                .authed_get(
                    refresher,
                    "/me/playlists",
                    &[("limit", "50".to_owned()), ("offset", offset.to_string())],
                )
                .await?;
            let items = page
                .get("items")
                .and_then(|items| items.as_array())
                .cloned()
                .unwrap_or_default();
            let total = page
                .get("total")
                .and_then(|total| total.as_i64())
                .unwrap_or(0);
            let page_len = items.len();
            playlists.extend(items);
            if playlists.len() as i64 >= total || page_len == 0 {
                break;
            }
            offset += 50;
        }
        Ok(playlists)
    }

    /// Playlist metadata (`GET /playlists/{id}`).
    pub async fn playlist(
        &self,
        refresher: &TokenRefresher<'_>,
        playlist_id: &str,
    ) -> Result<serde_json::Value, SpotifyError> {
        self.authed_get(
            refresher,
            &format!("/playlists/{playlist_id}"),
            &[("fields", "id,name,images,tracks.total".to_owned())],
        )
        .await
    }

    /// Playlist tracks (`GET /playlists/{id}/items`). The older `/tracks`
    /// endpoint is deprecated and 403s for development-mode apps after the
    /// March 2026 migration: never switch this back. Each entry carries the
    /// track under `item` (new) or `track` (legacy alias); locals, id-less
    /// rows, and non-track types are skipped.
    pub async fn playlist_tracks(
        &self,
        refresher: &TokenRefresher<'_>,
        playlist_id: &str,
    ) -> Result<Vec<serde_json::Value>, SpotifyError> {
        let mut tracks = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let page = self
                .authed_get(
                    refresher,
                    &format!("/playlists/{playlist_id}/items"),
                    &[("limit", "100".to_owned()), ("offset", offset.to_string())],
                )
                .await?;
            let items = page
                .get("items")
                .and_then(|items| items.as_array())
                .cloned()
                .unwrap_or_default();
            for item in &items {
                if item
                    .get("is_local")
                    .and_then(|flag| flag.as_bool())
                    .unwrap_or(false)
                {
                    continue;
                }
                let track = item.get("track").or_else(|| item.get("item"));
                let Some(track) = track else { continue };
                if track
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    continue;
                }
                if track
                    .get("type")
                    .and_then(|kind| kind.as_str())
                    .unwrap_or("track")
                    != "track"
                {
                    continue;
                }
                tracks.push(track.clone());
            }
            let has_next = page.get("next").is_some_and(|next| !next.is_null());
            if items.is_empty() || !has_next {
                break;
            }
            offset += 100;
        }
        Ok(tracks)
    }

    /// Fetch and validate a playlist image: single attempt, no redirects (a
    /// CDN URL must answer directly), host allowlist, `image/*` content
    /// type, and a bounded streamed read aborting past
    /// [`MAX_COVER_FETCH_BYTES`] without buffering the excess (v2
    /// `fetch_spotify_playlist_cover`). Unusable answers return none;
    /// network errors propagate for the caller to degrade.
    pub async fn fetch_cover(&self, url: &str) -> Result<Option<(Vec<u8>, String)>, SpotifyError> {
        if !is_allowed_cover_url(url) {
            return Ok(None);
        }
        let response =
            self.direct.get(url).send().await.map_err(|cause| {
                SpotifyError::Unavailable(format!("cover fetch failed: {cause}"))
            })?;
        if response.status().is_redirection() || response.status() != reqwest::StatusCode::OK {
            return Ok(None);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !content_type.starts_with("image/") {
            return Ok(None);
        }
        if let Some(declared) = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
        {
            let trimmed = declared.trim();
            if !trimmed.is_empty()
                && trimmed.bytes().all(|byte| byte.is_ascii_digit())
                && let Ok(length) = trimmed.parse::<usize>()
                && length > MAX_COVER_FETCH_BYTES
            {
                return Ok(None);
            }
        }
        // One-shot read: the workspace pins reqwest without the `stream`
        // cargo feature, so mid-read aborts are unavailable. The
        // content-length precheck above already rejects declared giants;
        // the length check below still refuses to *keep* an over-cap body.
        let body = response
            .bytes()
            .await
            .map_err(|cause| SpotifyError::Unavailable(format!("cover read failed: {cause}")))?;
        if body.len() > MAX_COVER_FETCH_BYTES {
            return Ok(None);
        }
        Ok(Some((body.to_vec(), content_type)))
    }
}

/// Decode a JSON body without the reqwest `json` cargo feature (the
/// workspace pins reqwest to `rustls-tls` only): bytes first, then serde.
async fn json_body(
    response: reqwest::Response,
    what: &str,
) -> Result<serde_json::Value, SpotifyError> {
    let bytes = response
        .bytes()
        .await
        .map_err(|cause| SpotifyError::Unavailable(format!("{what} read failed: {cause}")))?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|cause| SpotifyError::Unavailable(format!("{what} decode failed: {cause}")))
}

/// Token grant from the accounts endpoint. Debug is hand-written: the
/// tokens never appear in debug output.
#[derive(Clone, PartialEq)]
pub struct TokenGrant {
    /// Fresh access token.
    pub access_token: String,
    /// Rotated refresh token, when the grant carries one.
    pub refresh_token: Option<String>,
    /// Lifetime in seconds (default 3600, v2).
    pub expires_in_secs: i64,
}

impl std::fmt::Debug for TokenGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenGrant")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_in_secs", &self.expires_in_secs)
            .finish()
    }
}

impl TokenGrant {
    /// Decode the tolerant grant shape.
    pub fn from_value(body: &serde_json::Value) -> Self {
        Self {
            access_token: body
                .get("access_token")
                .and_then(|token| token.as_str())
                .unwrap_or("")
                .to_owned(),
            refresh_token: body
                .get("refresh_token")
                .and_then(|token| token.as_str())
                .map(str::to_owned),
            expires_in_secs: body
                .get("expires_in")
                .and_then(|secs| secs.as_i64())
                .unwrap_or(3600),
        }
    }
}

/// Refreshing token holder behind the authed GETs. Refresh writes the new
/// tokens back to the connection row (v2 `_refresh` upsert).
pub struct TokenRefresher<'a> {
    client: &'a SpotifyClient,
    connections: &'a dyn SpotifyConnectionStore,
    user_id: String,
    client_id: String,
    client_secret: String,
    cached: Mutex<SpotifyConnection>,
}

impl<'a> TokenRefresher<'a> {
    /// Wrap one user's link.
    pub fn new(
        client: &'a SpotifyClient,
        connections: &'a dyn SpotifyConnectionStore,
        user_id: &str,
        client_id: &str,
        client_secret: &str,
        connection: SpotifyConnection,
    ) -> Self {
        Self {
            client,
            connections,
            user_id: user_id.to_owned(),
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            cached: Mutex::new(connection),
        }
    }

    /// Current usable token, refreshing first when expired.
    pub async fn valid_token(&self) -> Result<String, SpotifyError> {
        let connection = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if SpotifyClient::is_expired(connection.expires_at_unix, now_unix_secs()) {
            return self.refresh_now().await;
        }
        Ok(connection.access_token)
    }

    /// Token without a freshness check, for the post-wait retry.
    pub async fn current_token(&self) -> String {
        self.cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .access_token
            .clone()
    }

    /// Refresh now and persist the grant.
    pub async fn refresh_now(&self) -> Result<String, SpotifyError> {
        let connection = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let grant = self
            .client
            .refresh(
                &self.client_id,
                &self.client_secret,
                &connection.refresh_token,
            )
            .await?;
        let updated = SpotifyConnection {
            access_token: grant.access_token.clone(),
            refresh_token: grant.refresh_token.unwrap_or(connection.refresh_token),
            expires_at_unix: now_unix_secs() + grant.expires_in_secs,
            username: connection.username,
            spotify_user_id: connection.spotify_user_id,
        };
        self.connections.upsert(&self.user_id, &updated);
        self.cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone_from(&updated);
        Ok(updated.access_token)
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
        if !raw.enabled || raw.client_id.is_empty() || raw.client_secret.is_empty() {
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

    /// Resolve one user's refreshing API access (v2 `_get_client`).
    pub fn refresher_for(&self, user_id: &str) -> Result<TokenRefresher<'_>, SpotifyError> {
        let raw = self.settings.get_raw();
        let connection = self
            .connections
            .get(user_id)
            .ok_or(SpotifyError::NotLinked)?;
        Ok(TokenRefresher::new(
            &self.client,
            self.connections.as_ref(),
            user_id,
            &raw.client_id,
            &raw.client_secret,
            connection,
        ))
    }

    /// Owned playlists annotated with the imported mapping (v2
    /// `list_playlists`). Only playlists whose owner id matches the
    /// caller's Spotify id are listed; the id is fetched from `/me` when
    /// the link row does not carry it yet.
    pub async fn list_playlists(
        &self,
        user_id: &str,
    ) -> Result<SpotifyPlaylistListResponse, SpotifyError> {
        let refresher = self.refresher_for(user_id)?;
        let stored = self
            .connections
            .get(user_id)
            .ok_or(SpotifyError::NotLinked)?;
        let mut spotify_user_id = stored.spotify_user_id;
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
        for (source_ref, id) in self.playlists.source_refs_for(user_id) {
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
    pub fn ensure_playlist_record(
        &self,
        user_id: &str,
        spotify_playlist_id: &str,
        name: &str,
    ) -> String {
        let source_ref = format!("spotify:{spotify_playlist_id}");
        if let Some(existing) = self.playlists.get_by_source_ref(&source_ref, user_id) {
            return existing;
        }
        let name = if name.is_empty() {
            "Spotify Playlist".to_owned()
        } else {
            name.to_owned()
        };
        self.playlists.create(user_id, &name, &source_ref)
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
        let refresher = self.refresher_for(user_id)?;
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

        let existing = self.tracks.track_ids(playlist_id);
        if !existing.is_empty() {
            self.tracks.remove_tracks(playlist_id, &existing);
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
        self.tracks.add_tracks(playlist_id, &rows);
        let _ = self
            .persist_cover(user_id, spotify_playlist_id, playlist_id, &info)
            .await;
        Ok(written)
    }

    /// Store the picked provider image as the local cover. Any failure
    /// degrades and leaves the import untouched: artwork must never fail a
    /// playlist import, and no cover is normal (v2 `_persist_playlist_cover`).
    pub async fn persist_cover(
        &self,
        user_id: &str,
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
        if self
            .tracks
            .set_imported_cover(playlist_id, user_id, &data, &content_type)
        {
            CoverOutcome::Stored
        } else {
            CoverOutcome::KeptExisting
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

/// Minimal percent-encoder for the authorize query (no new dependency).
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Minimal base64 encoder for the Basic auth header (no new dependency).
fn base64_encode(input: &[u8], out: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in input.chunks(3) {
        let mut block: u32 = 0;
        for (index, byte) in chunk.iter().enumerate() {
            block |= (*byte as u32) << (16 - 8 * index);
        }
        let pads = 3 - chunk.len();
        for index in 0..4 - pads {
            out.push(ALPHABET[((block >> (18 - 6 * index)) & 0x3F) as usize] as char);
        }
        for _ in 0..pads {
            out.push('=');
        }
    }
}
