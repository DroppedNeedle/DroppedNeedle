//! Spotify OAuth: scopes, the redirect URI, the authorize URL, authorize
//! states and the per-user link.
//!
//! The redirect URI has a single source (issue #298). Links and states persist
//! in SQLite (see
//! [`super::spotify_store`]); the admin app settings live in the
//! `spotify_settings` section ([`crate::settings::ConfigSpotifySettings`]).

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// OAuth scopes.
pub const SPOTIFY_SCOPES: &str =
    "playlist-read-private playlist-read-collaborative user-read-private";

/// Spotify's authorize page.
pub const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";

/// OAuth callback path, as the callback's route annotation names it.
pub const SPOTIFY_CALLBACK_PATH: &str = "/api/v3/acquire/spotify/auth/callback";
/// The v2 callback path. Spotify apps registered against v2 list it, so the
/// server keeps answering there and sends it while the settings carry the
/// import's legacy marker.
pub const SPOTIFY_LEGACY_CALLBACK_PATH: &str = "/api/v1/me/connections/spotify/auth/callback";

/// How long an authorize state stays valid.
pub const STATE_TTL_SECS: i64 = 600;

/// Characters a query value or path segment keeps as they are (RFC 3986
/// unreserved); everything else is percent-encoded.
pub(super) const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Build the Spotify OAuth redirect URI, the single source of truth
/// (issue #298). The authorize call,
/// the token exchange, and the admin display endpoint all derive through
/// here so the value matches the dashboard byte-for-byte. An
/// admin-configured origin wins; empty keeps the request-derived base. The
/// deployment base path lands between origin and callback exactly once.
/// `callback_path` is [`SPOTIFY_CALLBACK_PATH`], or
/// [`SPOTIFY_LEGACY_CALLBACK_PATH`] for an app registered against v2.
pub fn redirect_uri(
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

/// The authorize URL for one app, redirect URI and state.
pub fn authorize_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
    let query = [
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", SPOTIFY_SCOPES),
        ("state", state),
    ]
    .iter()
    .map(|(key, value)| format!("{key}={}", utf8_percent_encode(value, UNRESERVED)))
    .collect::<Vec<_>>()
    .join("&");
    format!("{SPOTIFY_AUTHORIZE_URL}?{query}")
}

/// One stored per-user Spotify link.
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

/// One pending authorization: who asked, and the exact redirect URI the
/// authorize step sent, which the token exchange must repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAuth {
    /// The user linking their account.
    pub user_id: String,
    /// Redirect URI sent to Spotify's authorize page.
    pub redirect_uri: String,
}
