//! Spotify OAuth: scopes, the redirect URI, authorize states, the
//! per-user link and the admin app settings.
//!
//! Ports the v2 OAuth half: the exact scopes and the single-source redirect
//! URI (GH-298). Links and states persist in SQLite (see
//! [`super::spotify_store`]); the memory stores here are test doubles.

#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

use futures_util::future::BoxFuture;

use super::models::SpotifySettings;
#[cfg(any(test, feature = "test-support"))]
use droppedneedle_kernel::runtime_config::SECRET_MASK;
use droppedneedle_kernel::runtime_config::secret_sections::SpotifySettings as SpotifySection;

/// OAuth scopes (v2 `_SPOTIFY_SCOPES`, verbatim).
pub const SPOTIFY_SCOPES: &str =
    "playlist-read-private playlist-read-collaborative user-read-private";

/// OAuth callback path, as the callback's route annotation names it.
pub const SPOTIFY_CALLBACK_PATH: &str = "/api/v3/acquire/spotify/auth/callback";
/// The v2 callback path. Spotify apps registered against v2 list it, so the
/// server keeps answering there and sends it while the settings carry the
/// import's legacy marker.
pub const SPOTIFY_LEGACY_CALLBACK_PATH: &str = "/api/v1/me/connections/spotify/auth/callback";

/// Build the Spotify OAuth redirect URI, the single source of truth (v2
/// `PreferencesService.spotify_redirect_uri`, GH-298). The authorize call,
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
    /// The saved app settings with the real secret. The secret sits in a
    /// [`Secret`], so it never reaches a response.
    ///
    /// [`Secret`]: droppedneedle_kernel::runtime_config::Secret
    fn get_raw(&self) -> SpotifySection;
    /// Save; a masked secret preserves the stored one. Rejects a
    /// non-absolute redirect origin (v2 `ConfigurationError` text kept) as
    /// invalid input.
    fn save<'a>(
        &'a self,
        settings: &'a SpotifySettings,
    ) -> BoxFuture<'a, Result<(), super::error::ImportsError>>;
    /// True when the app was registered against v2, so its dashboard lists
    /// the v2 callback path. The v2 import sets it; saving a different
    /// client id clears it.
    fn legacy_callback(&self) -> bool;
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

/// How long an authorize state stays valid (v2 `store_spotify_state`).
pub const STATE_TTL_SECS: i64 = 600;

/// Single-use OAuth states (v2 `AuthStore` spotify-state half). Errors
/// carry a log-only cause.
pub trait SpotifyStateStore: Send + Sync {
    /// Remember `state` for [`STATE_TTL_SECS`], pruning expired states.
    fn store_state<'a>(
        &'a self,
        state: &'a str,
        pending: &'a PendingAuth,
    ) -> BoxFuture<'a, Result<(), String>>;
    /// Consume `state` once. Unknown, expired or replayed states read as
    /// none.
    fn consume_state<'a>(
        &'a self,
        state: &'a str,
    ) -> BoxFuture<'a, Result<Option<PendingAuth>, String>>;
}

/// Per-user Spotify links (v2 `UserConnectionsStore` spotify rows).
/// Errors carry a log-only cause.
pub trait SpotifyConnectionStore: Send + Sync {
    /// Insert or replace one user's link (the OAuth callback).
    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        connection: &'a SpotifyConnection,
    ) -> BoxFuture<'a, Result<(), String>>;
    /// Write refreshed tokens only when the link still exists, so a token
    /// refresh racing a disconnect never brings the link back. Answers
    /// whether a row was updated.
    fn update_tokens_if_present<'a>(
        &'a self,
        user_id: &'a str,
        connection: &'a SpotifyConnection,
    ) -> BoxFuture<'a, Result<bool, String>>;
    /// Read one user's link, if any.
    fn get<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SpotifyConnection>, String>>;
    /// Drop one user's link. Answers whether a link was there.
    fn remove<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<bool, String>>;
}

/// In-memory Spotify settings rows.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemorySpotifySettings {
    inner: Mutex<SpotifySettings>,
    legacy_callback: std::sync::atomic::AtomicBool,
}

#[cfg(any(test, feature = "test-support"))]
impl MemorySpotifySettings {
    /// Empty rows.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the app as registered against v2, as the import does.
    pub fn set_legacy_callback(&self, legacy: bool) {
        self.legacy_callback
            .store(legacy, std::sync::atomic::Ordering::SeqCst);
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
                SECRET_MASK.to_owned()
            },
            enabled: inner.enabled,
            spotify_redirect_origin: inner.spotify_redirect_origin.clone(),
        }
    }

    fn get_raw(&self) -> SpotifySection {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        SpotifySection {
            client_id: inner.client_id.clone(),
            client_secret: droppedneedle_kernel::runtime_config::Secret::new(
                inner.client_secret.clone(),
            ),
            enabled: inner.enabled,
            spotify_redirect_origin: inner.spotify_redirect_origin.clone(),
            legacy_callback: self.legacy_callback(),
        }
    }

    fn save<'a>(
        &'a self,
        settings: &'a SpotifySettings,
    ) -> BoxFuture<'a, Result<(), super::error::ImportsError>> {
        Box::pin(async move { self.save_now(settings) })
    }

    fn legacy_callback(&self) -> bool {
        self.legacy_callback
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl MemorySpotifySettings {
    fn save_now(&self, settings: &SpotifySettings) -> Result<(), super::error::ImportsError> {
        let origin = settings.spotify_redirect_origin.trim();
        if !(origin.is_empty() || origin.starts_with("http://") || origin.starts_with("https://")) {
            return Err(super::error::ImportsError::InvalidInput {
                message: "Spotify redirect origin must be an absolute http(s) URL".to_owned(),
            });
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.client_id.trim() != settings.client_id.trim() {
            self.set_legacy_callback(false);
        }
        inner.client_id = settings.client_id.clone();
        if settings.client_secret != SECRET_MASK {
            inner.client_secret = settings.client_secret.clone();
        }
        inner.enabled = settings.enabled;
        inner.spotify_redirect_origin = origin.trim_end_matches('/').to_owned();
        Ok(())
    }
}

/// In-memory single-use OAuth states, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemorySpotifyStates {
    inner: Mutex<HashMap<String, PendingAuth>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemorySpotifyStates {
    /// Empty states.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl SpotifyStateStore for MemorySpotifyStates {
    fn store_state<'a>(
        &'a self,
        state: &'a str,
        pending: &'a PendingAuth,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(state.to_owned(), pending.clone());
        Box::pin(async { Ok(()) })
    }

    fn consume_state<'a>(
        &'a self,
        state: &'a str,
    ) -> BoxFuture<'a, Result<Option<PendingAuth>, String>> {
        let pending = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(state);
        Box::pin(async move { Ok(pending) })
    }
}

/// In-memory per-user Spotify links, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemorySpotifyConnections {
    inner: Mutex<HashMap<String, SpotifyConnection>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemorySpotifyConnections {
    /// Empty links.
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> std::sync::MutexGuard<'_, HashMap<String, SpotifyConnection>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl SpotifyConnectionStore for MemorySpotifyConnections {
    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        connection: &'a SpotifyConnection,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.rows().insert(user_id.to_owned(), connection.clone());
        Box::pin(async { Ok(()) })
    }

    fn update_tokens_if_present<'a>(
        &'a self,
        user_id: &'a str,
        connection: &'a SpotifyConnection,
    ) -> BoxFuture<'a, Result<bool, String>> {
        let updated = match self.rows().get_mut(user_id) {
            Some(row) => {
                row.clone_from(connection);
                true
            }
            None => false,
        };
        Box::pin(async move { Ok(updated) })
    }

    fn get<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SpotifyConnection>, String>> {
        let row = self.rows().get(user_id).cloned();
        Box::pin(async move { Ok(row) })
    }

    fn remove<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        let removed = self.rows().remove(user_id).is_some();
        Box::pin(async move { Ok(removed) })
    }
}

/// Minimal percent-encoder for the authorize query (no new dependency).
pub(super) fn percent_encode(value: &str) -> String {
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
