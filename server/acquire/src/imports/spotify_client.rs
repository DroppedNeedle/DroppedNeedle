//! Spotify for the import: the user's tokens over the providers' Spotify
//! client.
//!
//! The calls themselves go through the `spotify` client in the providers
//! crate, which paces them on its rate row and rests the row on a 429.
//! What is kept here is what needs the user's link: the 60-second refresh
//! buffer with one refresh-then-retry on a 401, the paging, the
//! `items`/`tracks` track-count quirk (issue #353), the `track`/`item`
//! alias with `is_local` skips, and the cover fetch that never fails an
//! import.

use std::future::Future;
use std::sync::Mutex;

use super::spotify_oauth::SpotifyConnection;
use super::spotify_store::SqliteSpotifyLinks;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_providers::images::ImageClient;
use droppedneedle_providers::spotify::models::{
    EntryTrack, Image, PlaylistInfo, Profile, SimplifiedPlaylist, TrackCount,
};
use droppedneedle_providers::spotify::{self, TokenGrant};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};

/// Seconds before expiry a token counts as expired.
pub const REFRESH_BUFFER_SECS: i64 = 60;
/// Network read bound for one playlist cover. Looser than the
/// storage cap on purpose: it only stops an unbounded wire read.
pub const MAX_COVER_FETCH_BYTES: usize = 5 * 1024 * 1024;

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

/// A provider failure as the import sees it.
fn unavailable(what: &str, error: &ProviderError) -> SpotifyError {
    SpotifyError::Unavailable(format!("{what}: {error}"))
}

/// The import's view of Spotify: the providers' Spotify client and the
/// image client its covers come through.
#[derive(Debug, Clone)]
pub struct SpotifyClient {
    api: spotify::SpotifyClient,
    images: ImageClient,
}

impl SpotifyClient {
    /// Over the shared providers.
    pub fn new(providers: &Providers) -> Self {
        Self {
            api: providers.spotify.clone(),
            images: providers.images.clone(),
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

    /// The authorize page for one app, redirect URI and state, or `None`
    /// when the accounts address is unusable.
    pub fn authorize_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
    ) -> Option<String> {
        self.api
            .authorize_url(client_id, redirect_uri, state)
            .map(String::from)
    }

    /// Exchange an OAuth code for tokens, for the callback a person waits
    /// on.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
    ) -> Result<TokenGrant, SpotifyError> {
        self.api
            .exchange_code(
                client_id,
                client_secret,
                code,
                redirect_uri,
                RequestPriority::UserInitiated,
            )
            .await
            .map_err(|error| unavailable("token exchange", &error))
    }

    /// Refresh an access token at `priority`.
    async fn refresh(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
        priority: RequestPriority,
    ) -> Result<TokenGrant, SpotifyError> {
        if refresh_token.is_empty() {
            return Err(SpotifyError::NotLinked);
        }
        self.api
            .refresh(client_id, client_secret, refresh_token, priority)
            .await
            .map_err(|error| unavailable("token refresh", &error))
    }

    /// One call with the refresher's token: refresh-then-once on a 401.
    async fn authed<T, F, Fut>(
        &self,
        refresher: &TokenRefresher<'_>,
        what: &str,
        call: F,
    ) -> Result<T, SpotifyError>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let token = refresher.valid_token().await?;
        let answer = match call(token).await {
            Err(ProviderError::Unauthorized { .. }) => {
                let token = refresher.refresh_now().await?;
                call(token).await
            }
            answer => answer,
        };
        answer.map_err(|error| unavailable(what, &error))
    }

    /// Current user (`GET /me`).
    pub async fn current_user(
        &self,
        refresher: &TokenRefresher<'_>,
    ) -> Result<Profile, SpotifyError> {
        let priority = refresher.priority;
        self.authed(refresher, "/me", |token| async move {
            self.api.me(&token, priority).await
        })
        .await
    }

    /// The profile behind a fresh grant, for the OAuth callback. A
    /// transport failure is an error; an answer that is not a profile (a
    /// refusal, a body that does not decode) is logged and reads as none,
    /// so the link still lands and the listing asks again later.
    pub async fn profile(&self, access_token: &str) -> Result<Option<Profile>, SpotifyError> {
        match self
            .api
            .me(access_token, RequestPriority::UserInitiated)
            .await
        {
            Ok(profile) => Ok(Some(profile)),
            Err(error @ ProviderError::Transport { .. }) => {
                Err(unavailable("callback /me", &error))
            }
            Err(error) => {
                tracing::warn!(%error, "spotify callback /me unusable");
                Ok(None)
            }
        }
    }

    /// Every owned-or-followed playlist, page by page.
    pub async fn user_playlists(
        &self,
        refresher: &TokenRefresher<'_>,
    ) -> Result<Vec<SimplifiedPlaylist>, SpotifyError> {
        let priority = refresher.priority;
        let mut playlists = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let page = self
                .authed(refresher, "/me/playlists", |token| async move {
                    self.api.playlists_page(&token, offset, priority).await
                })
                .await?;
            let page_len = page.items.len();
            playlists.extend(page.items);
            if playlists.len() as i64 >= page.total || page_len == 0 {
                break;
            }
            offset += spotify::PLAYLISTS_PAGE;
        }
        Ok(playlists)
    }

    /// Playlist metadata.
    pub async fn playlist(
        &self,
        refresher: &TokenRefresher<'_>,
        playlist_id: &str,
    ) -> Result<PlaylistInfo, SpotifyError> {
        let priority = refresher.priority;
        self.authed(refresher, "playlist", |token| async move {
            self.api.playlist(&token, playlist_id, priority).await
        })
        .await
    }

    /// Playlist tracks, page by page. Each entry carries the track under
    /// `item` (new) or `track` (legacy alias); locals, id-less rows, and
    /// non-track types are skipped.
    pub async fn playlist_tracks(
        &self,
        refresher: &TokenRefresher<'_>,
        playlist_id: &str,
    ) -> Result<Vec<EntryTrack>, SpotifyError> {
        let priority = refresher.priority;
        let mut tracks = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let page = self
                .authed(refresher, "playlist items", |token| async move {
                    self.api
                        .playlist_items_page(&token, playlist_id, offset, priority)
                        .await
                })
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
            offset += spotify::ITEMS_PAGE;
        }
        Ok(tracks)
    }

    /// A playlist image and its content type, read under
    /// [`MAX_COVER_FETCH_BYTES`] from Spotify's CDN with no redirect
    /// followed. Unusable answers are none; network errors propagate for
    /// the caller to degrade.
    pub async fn fetch_cover(
        &self,
        url: &str,
        priority: RequestPriority,
    ) -> Result<Option<(Vec<u8>, String)>, SpotifyError> {
        self.images
            .spotify_cover(url, MAX_COVER_FETCH_BYTES, priority)
            .await
            .map_err(|error| unavailable("cover fetch", &error))
    }
}

/// Refreshing token holder behind the authed calls, at the priority of the
/// work it serves. Refresh writes the new tokens back to the connection
/// row, but only while the row exists: a refresh that lands after a
/// disconnect must not relink the account.
pub struct TokenRefresher<'a> {
    client: &'a SpotifyClient,
    connections: &'a SqliteSpotifyLinks,
    user_id: String,
    client_id: String,
    client_secret: String,
    priority: RequestPriority,
    cached: Mutex<SpotifyConnection>,
}

impl<'a> TokenRefresher<'a> {
    /// Wrap one user's link for work at `priority`.
    pub fn new(
        client: &'a SpotifyClient,
        connections: &'a SqliteSpotifyLinks,
        user_id: &str,
        (client_id, client_secret): (&str, &str),
        connection: SpotifyConnection,
        priority: RequestPriority,
    ) -> Self {
        Self {
            client,
            connections,
            user_id: user_id.to_owned(),
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            priority,
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
                self.priority,
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
            .await?;
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
