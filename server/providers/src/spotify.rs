//! Spotify: the accounts host (the authorize page and the token grants)
//! and the Web API reads a playlist import makes.
//!
//! Every call sends through the executor on the `spotify` rate row, at the
//! priority the caller names. Spotify counts calls per app in a rolling
//! 30-second window and answers 429 with `Retry-After` past it; the
//! executor turns that wait into a cooldown on the row. The caller holds
//! the user's tokens and refreshes them: an expired token is
//! [`ProviderError::Unauthorized`]. The wire shapes are in
//! `spotify/models.rs`; the playlist covers come through
//! [`ImageClient::spotify_cover`](crate::images::ImageClient::spotify_cover).

use std::time::Duration;

use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{Backoff, Executor, Request, Spec, Url, standard_classify};

pub mod models;

use models::{EntryPage, PlaylistInfo, PlaylistPage, Profile, TokenAnswer};

/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "spotify";
/// Spotify Web API root.
pub const API_BASE: &str = "https://api.spotify.com/v1";
/// Spotify accounts origin: the authorize page and the token grants.
pub const ACCOUNTS_BASE: &str = "https://accounts.spotify.com";
/// OAuth scopes the import asks for.
pub const SCOPES: &str = "playlist-read-private playlist-read-collaborative user-read-private";
/// Largest Web API or accounts answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Playlists read per page of the caller's list (Spotify's maximum).
pub const PLAYLISTS_PAGE: i64 = 50;
/// Entries read per page of one playlist (Spotify's maximum).
pub const ITEMS_PAGE: i64 = 100;

/// Characters a query value keeps as they are (RFC 3986 unreserved);
/// everything else is percent-encoded.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Spotify on the wire: the standard status rules. Spotify normally names
/// its wait on a 429; one that does not rests the row a few seconds,
/// doubling. A long wait it names (an app over its allowance) is kept up
/// to an hour.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Exponential {
        base: Duration::from_secs(2),
        max: Duration::from_secs(60),
    },
    retry_after_cap: Duration::from_secs(3600),
    classify: standard_classify,
};

/// A token grant from the accounts host. Debug is hand-written: the tokens
/// never appear in debug output.
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

/// The Spotify client. Built once with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct SpotifyClient {
    executor: Executor,
    api_base: Option<Url>,
    accounts_base: Option<Url>,
}

impl SpotifyClient {
    /// Calls through `executor` to the Web API at `api_base` and the
    /// accounts host at `accounts_base`.
    #[must_use]
    pub fn new(executor: Executor, api_base: &str, accounts_base: &str) -> Self {
        Self {
            executor,
            api_base: parse_base(api_base, SOURCE),
            accounts_base: parse_base(accounts_base, SOURCE),
        }
    }

    /// The authorize page for one app, redirect URI and state, or `None`
    /// when the accounts address is unusable.
    #[must_use]
    pub fn authorize_url(&self, client_id: &str, redirect_uri: &str, state: &str) -> Option<Url> {
        let mut url = self.accounts_base.clone()?;
        url.path_segments_mut()
            .ok()?
            .pop_if_empty()
            .push("authorize");
        let query = [
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPES),
            ("state", state),
        ]
        .iter()
        .map(|(key, value)| format!("{key}={}", utf8_percent_encode(value, UNRESERVED)))
        .collect::<Vec<_>>()
        .join("&");
        url.set_query(Some(&query));
        Some(url)
    }

    /// Exchange an OAuth code for tokens.
    ///
    /// # Errors
    ///
    /// The executor's typed failure, or a payload error for an answer that
    /// does not decode.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
        priority: RequestPriority,
    ) -> Result<TokenGrant, ProviderError> {
        self.token(
            client_id,
            client_secret,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ],
            priority,
        )
        .await
    }

    /// Refresh an access token.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::exchange_code`].
    pub async fn refresh(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
        priority: RequestPriority,
    ) -> Result<TokenGrant, ProviderError> {
        self.token(
            client_id,
            client_secret,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
            priority,
        )
        .await
    }

    /// One Basic-auth grant on the accounts host.
    async fn token(
        &self,
        client_id: &str,
        client_secret: &str,
        form: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<TokenGrant, ProviderError> {
        let base = self
            .accounts_base
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let form = form
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        let credentials = base64::engine::general_purpose::STANDARD
            .encode(format!("{client_id}:{client_secret}"));
        let request = Request::post_form(base, &["api", "token"], form)
            .map_err(|_| unusable_base(SOURCE))?
            .header("authorization", format!("Basic {credentials}"));
        let reply = self.executor.send(request, priority).await?;
        let answer: TokenAnswer = decode(&reply)?;
        Ok(TokenGrant::from(answer))
    }

    /// The user `access_token` belongs to (`GET /me`).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::exchange_code`]; an expired token is
    /// [`ProviderError::Unauthorized`].
    pub async fn me(
        &self,
        access_token: &str,
        priority: RequestPriority,
    ) -> Result<Profile, ProviderError> {
        self.get(access_token, &["me"], &[], priority).await
    }

    /// One page of the caller's playlists (`GET /me/playlists`), from
    /// `offset`, [`PLAYLISTS_PAGE`] at a time.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn playlists_page(
        &self,
        access_token: &str,
        offset: i64,
        priority: RequestPriority,
    ) -> Result<PlaylistPage, ProviderError> {
        let (limit, offset) = (PLAYLISTS_PAGE.to_string(), offset.to_string());
        self.get(
            access_token,
            &["me", "playlists"],
            &[("limit", &limit), ("offset", &offset)],
            priority,
        )
        .await
    }

    /// One playlist's metadata (`GET /playlists/{id}`).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn playlist(
        &self,
        access_token: &str,
        playlist_id: &str,
        priority: RequestPriority,
    ) -> Result<PlaylistInfo, ProviderError> {
        self.get(
            access_token,
            &["playlists", playlist_id],
            &[("fields", "id,name,images,tracks.total")],
            priority,
        )
        .await
    }

    /// One page of a playlist's entries (`GET /playlists/{id}/items`), from
    /// `offset`, [`ITEMS_PAGE`] at a time. The older `/tracks` endpoint is
    /// deprecated and 403s for development-mode apps after the March 2026
    /// migration: never switch this back.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn playlist_items_page(
        &self,
        access_token: &str,
        playlist_id: &str,
        offset: i64,
        priority: RequestPriority,
    ) -> Result<EntryPage, ProviderError> {
        let (limit, offset) = (ITEMS_PAGE.to_string(), offset.to_string());
        self.get(
            access_token,
            &["playlists", playlist_id, "items"],
            &[("limit", &limit), ("offset", &offset)],
            priority,
        )
        .await
    }

    /// One authenticated Web API GET of `path`. Each path value is one
    /// segment, so an id from a request cannot add a path or a query.
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        access_token: &str,
        path: &[&str],
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<T, ProviderError> {
        let base = self
            .api_base
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, path, query)
            .map_err(|_| unusable_base(SOURCE))?
            .header("authorization", format!("Bearer {access_token}"));
        let reply = self.executor.send(request, priority).await?;
        decode(&reply)
    }
}
