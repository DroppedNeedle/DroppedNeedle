//! Spotify Web API client.
//!
//! The Basic-auth token exchange, the 60-second
//! refresh buffer with one 401 retry and one short 429 wait, the
//! `items`/`tracks` track-count quirk (issue #353), the `/items`
//! tracks endpoint (the old `/tracks` 403s for dev-mode apps after the
//! March 2026 migration), the `track`/`item` alias with `is_local` skips,
//! and the bounded no-redirect cover fetch that never fails an import.
//! Answers decode through [`super::spotify_models`].

use std::sync::Mutex;

use serde::de::DeserializeOwned;

use super::spotify_models::{
    EntryPage, EntryTrack, Image, PlaylistInfo, PlaylistPage, Profile, SimplifiedPlaylist,
    TokenAnswer, TrackCount,
};
use super::spotify_oauth::{SpotifyConnection, UNRESERVED};
use super::spotify_store::SqliteSpotifyLinks;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::http_client::{BodyError, read_capped};

/// Seconds before expiry a token counts as expired.
pub const REFRESH_BUFFER_SECS: i64 = 60;
/// Network read bound for one playlist cover. Looser than the
/// storage cap on purpose: it only stops an unbounded wire read.
pub const MAX_COVER_FETCH_BYTES: usize = 5 * 1024 * 1024;
/// Largest Web API answer read.
pub const MAX_API_BODY_BYTES: usize = 4 * 1024 * 1024;

/// `clock`'s time in unix seconds; a clock before the epoch reads as zero.
pub fn now_unix_secs(clock: &dyn Clock) -> i64 {
    clock.now_unix().max(0)
}

/// Track count of a listed playlist. The
/// count lives under `items` on the current shape, with `tracks` null or
/// absent; the older shape used `tracks`. Neither with a total reads as 0.
pub fn playlist_track_count(playlist: &SimplifiedPlaylist) -> i64 {
    [&playlist.items, &playlist.tracks]
        .into_iter()
        .find_map(|count| match count {
            Some(TrackCount::Counted { total }) => Some(*total),
            _ => None,
        })
        .unwrap_or(0)
}

/// Pick the smallest image at least `min_size` wide, else the largest
/// (default floor 250).
pub fn best_image_url(images: Option<&[Image]>, min_size: i64) -> Option<String> {
    let mut sorted: Vec<&Image> = images.unwrap_or_default().iter().collect();
    sorted.sort_by_key(|image| image.width.unwrap_or(0));
    sorted
        .iter()
        .find(|image| image.width.unwrap_or(0) >= min_size)
        .or(sorted.last())
        .map(|image| image.url.clone())
}

/// A cover URL from a Spotify answer, parsed, when it may be fetched:
/// https to a Spotify CDN host (`*.scdn.co`), or any URL on the
/// `extra_cdn` origin (the tests' mock CDN; production has none).
pub fn allowed_cover_url(url: &str, extra_cdn: Option<&reqwest::Url>) -> Option<reqwest::Url> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if extra_cdn.is_some_and(|cdn| cdn.origin() == parsed.origin()) {
        return Some(parsed);
    }
    let spotify_cdn = parsed.scheme() == "https"
        && parsed
            .host_str()
            .is_some_and(|host| host.to_lowercase().ends_with(".scdn.co"));
    spotify_cdn.then_some(parsed)
}

/// Classified Spotify failure. Wire detail stays out; handlers map it to
/// 400 not-linked or 502 fetch failure.
#[derive(Debug, Clone, PartialEq)]
pub enum SpotifyError {
    /// No link row, or the token flow cannot proceed.
    NotLinked,
    /// Upstream or network failure, or an answer that does not decode. The
    /// detail reaches the log only.
    Unavailable(String),
    /// A local store read or write failed. The detail reaches the log only.
    Store(String),
    /// The database was too busy to take a write; try again later.
    Busy,
}

/// Per-user Spotify Web API client. Base URLs inject
/// so tests run against the loopback mocks; production passes the
/// `api.spotify.com` / `accounts.spotify.com` pair.
#[derive(Debug, Clone)]
pub struct SpotifyClient {
    http: reqwest::Client,
    /// Redirect-refusing client for covers: a CDN URL must answer directly,
    /// so the shared redirect-following client must never fetch artwork.
    direct: reqwest::Client,
    api_base: String,
    accounts_base: String,
    /// A cover origin allowed beside the Spotify CDN. Only tests set one.
    extra_cdn: Option<reqwest::Url>,
}

impl SpotifyClient {
    /// Build over the factory's shared and no-redirect clients with
    /// explicit bases, and the extra cover origin tests serve from.
    pub fn new(
        http: reqwest::Client,
        direct: reqwest::Client,
        api_base: &str,
        accounts_base: &str,
        extra_cdn: Option<&str>,
    ) -> Self {
        Self {
            http,
            direct,
            api_base: api_base.trim_end_matches('/').to_owned(),
            accounts_base: accounts_base.trim_end_matches('/').to_owned(),
            extra_cdn: extra_cdn.and_then(|cdn| reqwest::Url::parse(cdn).ok()),
        }
    }

    /// True when the token is missing or inside the refresh buffer
    /// (unparseable reads as expired).
    pub fn is_expired(expires_at_unix: i64, now_unix: i64) -> bool {
        if expires_at_unix <= 0 {
            return true;
        }
        now_unix >= expires_at_unix - REFRESH_BUFFER_SECS
    }

    /// Exchange an OAuth code for tokens.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
    ) -> Result<TokenGrant, SpotifyError> {
        self.token(
            client_id,
            client_secret,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ],
            "token exchange",
        )
        .await
    }

    /// Refresh an access token.
    pub async fn refresh(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<TokenGrant, SpotifyError> {
        if refresh_token.is_empty() {
            return Err(SpotifyError::NotLinked);
        }
        self.token(
            client_id,
            client_secret,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
            "token refresh",
        )
        .await
    }

    /// One Basic-auth grant on the accounts host.
    async fn token(
        &self,
        client_id: &str,
        client_secret: &str,
        form: &[(&str, &str)],
        what: &str,
    ) -> Result<TokenGrant, SpotifyError> {
        let response = self
            .http
            .post(format!("{}/api/token", self.accounts_base))
            .basic_auth(client_id, Some(client_secret))
            .form(form)
            .send()
            .await
            .map_err(|cause| SpotifyError::Unavailable(format!("{what} failed: {cause}")))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(SpotifyError::Unavailable(format!(
                "{what} status {}",
                response.status().as_u16()
            )));
        }
        let answer: TokenAnswer = json_body(response, what).await?;
        Ok(TokenGrant::from(answer))
    }

    /// Authenticated GET with these retry rules: refresh-then-once on 401,
    /// one short wait on 429 when `Retry-After` is at most 10 seconds.
    async fn authed_get<T: DeserializeOwned>(
        &self,
        refresher: &TokenRefresher<'_>,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, SpotifyError> {
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
        json_body(response, path).await
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
            .bearer_auth(token)
            .send()
            .await
            .map_err(|cause| SpotifyError::Unavailable(format!("spotify GET failed: {cause}")))
    }

    /// Current user (`GET /me`).
    pub async fn current_user(
        &self,
        refresher: &TokenRefresher<'_>,
    ) -> Result<Profile, SpotifyError> {
        self.authed_get(refresher, "/me", &[]).await
    }

    /// The profile behind a fresh grant, for the OAuth callback. A
    /// transport failure is an error; an answer that is not a profile (a
    /// refusal, a body that does not decode) is logged and reads as none,
    /// so the link still lands and the listing asks again later.
    pub async fn profile(&self, access_token: &str) -> Result<Option<Profile>, SpotifyError> {
        let response = self.get_with("/me", &[], access_token).await?;
        if !response.status().is_success() {
            tracing::warn!(
                status = response.status().as_u16(),
                "spotify callback /me refused"
            );
            return Ok(None);
        }
        match json_body::<Profile>(response, "/me").await {
            Ok(profile) => Ok(Some(profile)),
            Err(cause) => {
                tracing::warn!(?cause, "spotify callback /me unreadable");
                Ok(None)
            }
        }
    }

    /// Every owned-or-followed playlist page (`GET /me/playlists`, 50/page).
    pub async fn user_playlists(
        &self,
        refresher: &TokenRefresher<'_>,
    ) -> Result<Vec<SimplifiedPlaylist>, SpotifyError> {
        let mut playlists = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let page: PlaylistPage = self
                .authed_get(
                    refresher,
                    "/me/playlists",
                    &[("limit", "50".to_owned()), ("offset", offset.to_string())],
                )
                .await?;
            let page_len = page.items.len();
            playlists.extend(page.items);
            if playlists.len() as i64 >= page.total || page_len == 0 {
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
    ) -> Result<PlaylistInfo, SpotifyError> {
        self.authed_get(
            refresher,
            &format!("/playlists/{}", path_segment(playlist_id)),
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
    ) -> Result<Vec<EntryTrack>, SpotifyError> {
        let mut tracks = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let page: EntryPage = self
                .authed_get(
                    refresher,
                    &format!("/playlists/{}/items", path_segment(playlist_id)),
                    &[("limit", "100".to_owned()), ("offset", offset.to_string())],
                )
                .await?;
            let page_len = page.items.len();
            for entry in page.items {
                if entry.is_local {
                    continue;
                }
                let Some(track) = entry.track.or(entry.item) else {
                    continue;
                };
                if track.id.as_deref().unwrap_or("").is_empty() {
                    continue;
                }
                if track.kind.as_deref().unwrap_or("track") != "track" {
                    continue;
                }
                tracks.push(track);
            }
            if page_len == 0 || page.next.is_none() {
                break;
            }
            offset += 100;
        }
        Ok(tracks)
    }

    /// Fetch and validate a playlist image: single attempt, no redirects (a
    /// CDN URL must answer directly), host allowlist, `image/*` content
    /// type, and a capped read refusing anything past
    /// [`MAX_COVER_FETCH_BYTES`].
    /// Unusable answers return none;
    /// network errors propagate for the caller to degrade.
    pub async fn fetch_cover(&self, url: &str) -> Result<Option<(Vec<u8>, String)>, SpotifyError> {
        let Some(url) = allowed_cover_url(url, self.extra_cdn.as_ref()) else {
            return Ok(None);
        };
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

/// A playlist id as one path segment: anything outside the unreserved set
/// is percent-encoded, so an id from a request cannot add a path or a query.
fn path_segment(id: &str) -> percent_encoding::PercentEncode<'_> {
    percent_encoding::utf8_percent_encode(id, UNRESERVED)
}

/// Decode a JSON body read under [`MAX_API_BODY_BYTES`].
async fn json_body<T: DeserializeOwned>(
    response: reqwest::Response,
    what: &str,
) -> Result<T, SpotifyError> {
    let bytes = read_capped(response, MAX_API_BODY_BYTES)
        .await
        .map_err(|cause| match cause {
            BodyError::TooLarge => SpotifyError::Unavailable(format!("{what} answer too large")),
            BodyError::Transport(cause) => {
                SpotifyError::Unavailable(format!("{what} read failed: {cause}"))
            }
        })?;
    serde_json::from_slice::<T>(&bytes)
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
    /// Lifetime in seconds (default 3600).
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

impl From<TokenAnswer> for TokenGrant {
    fn from(answer: TokenAnswer) -> Self {
        Self {
            access_token: answer.access_token,
            refresh_token: answer.refresh_token,
            expires_in_secs: answer.expires_in.unwrap_or(3600),
        }
    }
}

/// Refreshing token holder behind the authed GETs. Refresh writes the new
/// tokens back to the connection row, but only while the row exists: a
/// refresh that lands after a disconnect must not relink the account.
pub struct TokenRefresher<'a> {
    client: &'a SpotifyClient,
    connections: &'a SqliteSpotifyLinks,
    user_id: String,
    client_id: String,
    client_secret: String,
    cached: Mutex<SpotifyConnection>,
}

impl<'a> TokenRefresher<'a> {
    /// Wrap one user's link.
    pub fn new(
        client: &'a SpotifyClient,
        connections: &'a SqliteSpotifyLinks,
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
        let now = now_unix_secs(self.connections.clock().as_ref());
        if SpotifyClient::is_expired(connection.expires_at_unix, now) {
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
            expires_at_unix: now_unix_secs(self.connections.clock().as_ref())
                + grant.expires_in_secs,
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
