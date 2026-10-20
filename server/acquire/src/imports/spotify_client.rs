//! Spotify Web API client.
//!
//! Ports v2's Spotify client: the Basic-auth token exchange, the 60-second
//! refresh buffer with one 401 retry and one short 429 wait, the
//! `items`/`tracks` track-count quirk (v2.9.0 issue #353), the `/items`
//! tracks endpoint (the old `/tracks` 403s for dev-mode apps after the
//! March 2026 migration), the `track`/`item` alias with `is_local` skips,
//! and the bounded no-redirect cover fetch that never fails an import.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::spotify_oauth::{SpotifyConnection, SpotifyConnectionStore};
use droppedneedle_kernel::http_client::{BodyError, read_capped};

/// Seconds before expiry a token counts as expired (v2 buffer).
pub const REFRESH_BUFFER_SECS: i64 = 60;
/// Network read bound for one playlist cover (v2 5 MiB). Looser than the
/// storage cap on purpose: it only stops an unbounded wire read.
pub const MAX_COVER_FETCH_BYTES: usize = 5 * 1024 * 1024;
/// Largest Web API answer read.
pub const MAX_API_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Seconds since the Unix epoch, saturating on clock failure.
pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
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
    /// A local store read or write failed. The detail reaches the log only.
    Store(String),
}

impl SpotifyError {
    /// The user-facing line a background import records (v2 texts).
    pub fn user_message(&self) -> String {
        match self {
            Self::NotLinked => "Spotify account not linked".to_owned(),
            Self::Unavailable(_) => "Failed to fetch playlist from Spotify".to_owned(),
            Self::Store(_) => "Failed to save the imported playlist".to_owned(),
        }
    }
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
    /// type, and a capped read refusing anything past
    /// [`MAX_COVER_FETCH_BYTES`] (v2 `fetch_spotify_playlist_cover`).
    /// Unusable answers return none;
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
        let body = match read_capped(response, MAX_COVER_FETCH_BYTES).await {
            Ok(body) => body,
            Err(BodyError::TooLarge) => return Ok(None),
            Err(BodyError::Transport(cause)) => {
                return Err(SpotifyError::Unavailable(format!(
                    "cover read failed: {cause}"
                )));
            }
        };
        Ok(Some((body, content_type)))
    }
}

/// Decode a JSON body read under [`MAX_API_BODY_BYTES`].
async fn json_body(
    response: reqwest::Response,
    what: &str,
) -> Result<serde_json::Value, SpotifyError> {
    let bytes = read_capped(response, MAX_API_BODY_BYTES)
        .await
        .map_err(|cause| match cause {
            BodyError::TooLarge => SpotifyError::Unavailable(format!("{what} answer too large")),
            BodyError::Transport(cause) => {
                SpotifyError::Unavailable(format!("{what} read failed: {cause}"))
            }
        })?;
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
/// tokens back to the connection row, but only while the row exists: a
/// refresh that lands after a disconnect must not relink the account.
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
        let kept = self
            .connections
            .update_tokens_if_present(&self.user_id, &updated)
            .await
            .map_err(SpotifyError::Store)?;
        if !kept {
            return Err(SpotifyError::NotLinked);
        }
        self.cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone_from(&updated);
        Ok(updated.access_token)
    }
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
