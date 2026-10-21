//! Settings reads.
//!
//! Acquire reads every settings section through a [`Live`] handle, so a
//! saved setting reaches the next use without a restart. A section that
//! does not read ([`Live::read`] answers `None`) stops the work it
//! controls; only the client connection sections read as unconfigured.
//!
//! Credentials and URLs live in the config store (encrypted at rest,
//! never in plaintext): [`ConfigLidarrSettings`] and
//! [`ConfigSpotifySettings`] serve the imports over the `lidarr_import`
//! and `spotify_settings` secret sections. They read through live handles
//! and hold the store only to save.

use std::sync::Arc;

use super::imports::models::{LidarrConnectionSettings, SpotifySettings as ImportsSpotifySettings};
use droppedneedle_kernel::runtime_config::mask::display_mask;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LidarrImportConnection, SpotifySettings,
};
use droppedneedle_kernel::runtime_config::{ConfigError, ConfigStore, Live, Secret, UpdateError};

use super::imports::error::ImportsError;

/// Clamp a settings integer to a non-negative `u32` (0 = unlimited).
pub(crate) fn clamp_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// Clamp a settings integer to a non-negative `u64`.
pub(crate) fn clamp_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(0)
}

/// Clamp a settings integer to a non-negative `usize`.
pub(crate) fn clamp_usize(value: i64) -> usize {
    usize::try_from(value.max(0)).unwrap_or(usize::MAX)
}

/// A settings store for tests over `settings.db` in `dir`, with a fixed
/// test key. Opening the same `dir` again reopens the same rows.
#[cfg(any(test, feature = "test-support"))]
pub fn scratch_settings(dir: &std::path::Path) -> Result<ConfigStore, String> {
    let crypto = droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32])
        .map_err(|error| error.to_string())?;
    ConfigStore::scratch(dir, crypto)
}

/// Lidarr connection settings over the `lidarr_import` secret section.
/// A section that does not read reads as empty (the import then reports
/// Lidarr as not connected, exactly like an unconfigured deployment).
pub struct ConfigLidarrSettings {
    live: Live<LidarrImportConnection>,
    store: Arc<ConfigStore>,
}

impl ConfigLidarrSettings {
    /// Settings over a shared config store, which saves go to.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self {
            live: store.live(),
            store,
        }
    }

    /// Masked settings for reads.
    pub fn get(&self) -> LidarrConnectionSettings {
        let section = self.live.read().unwrap_or_default();
        LidarrConnectionSettings {
            url: section.url.clone(),
            api_key: display_mask(section.api_key.expose()).to_owned(),
        }
    }

    /// The saved connection with its real key, for probes and imports.
    /// The key sits in a [`Secret`], so it never reaches a response.
    pub fn get_raw(&self) -> LidarrImportConnection {
        (**self.live.read().unwrap_or_default()).clone()
    }

    /// Save; a masked key keeps the stored one. A failed save is logged,
    /// and the read that follows shows what is stored.
    pub async fn save(&self, settings: &LidarrConnectionSettings) {
        let incoming = LidarrImportConnection {
            url: settings.url.clone(),
            api_key: Secret::new(settings.api_key.clone()),
        };
        if let Err(error) = self.store.save_secret(incoming).await {
            tracing::warn!(%error, "lidarr settings save failed");
        }
    }
}

/// Spotify app settings over the `spotify_settings` secret section. A
/// section that does not read reads as empty (OAuth then reports the app
/// as unconfigured).
pub struct ConfigSpotifySettings {
    live: Live<SpotifySettings>,
    store: Arc<ConfigStore>,
}

impl ConfigSpotifySettings {
    /// Settings over a shared config store, which saves go to.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self {
            live: store.live(),
            store,
        }
    }
}

impl ConfigSpotifySettings {
    /// True when the app was registered against v2, so its dashboard lists
    /// the v2 callback path. The v2 import sets it; saving a different
    /// client id clears it.
    pub fn legacy_callback(&self) -> bool {
        self.live.read().unwrap_or_default().legacy_callback
    }

    /// Masked settings for reads.
    pub fn get(&self) -> ImportsSpotifySettings {
        let section = self.live.read().unwrap_or_default();
        ImportsSpotifySettings {
            client_id: section.client_id.clone(),
            client_secret: display_mask(section.client_secret.expose()).to_owned(),
            enabled: section.enabled,
            spotify_redirect_origin: section.spotify_redirect_origin.clone(),
        }
    }

    /// The saved app settings with the real secret, which sits in a
    /// [`Secret`] so it never reaches a response.
    pub fn get_raw(&self) -> SpotifySettings {
        (**self.live.read().unwrap_or_default()).clone()
    }

    /// Save; a masked secret keeps the stored one. A redirect origin that
    /// is not an absolute http(s) address is invalid input.
    pub async fn save(&self, settings: &ImportsSpotifySettings) -> Result<(), ImportsError> {
        let submitted = settings.clone();
        // The v2 marker belongs to the app it was stamped for: a different
        // client id is a new app, registered with the v3 path. The check
        // reads the stored row in the same write, and a stored section that
        // does not read fails the save rather than dropping the marker.
        self.store
            .update_secret::<SpotifySettings, ConfigError, _>(move |stored| {
                let stored = stored.into_inner();
                let same_app = stored.client_id.trim() == submitted.client_id.trim();
                Ok(SpotifySettings {
                    client_id: submitted.client_id,
                    client_secret: Secret::new(submitted.client_secret),
                    enabled: submitted.enabled,
                    spotify_redirect_origin: submitted.spotify_redirect_origin,
                    legacy_callback: stored.legacy_callback && same_app,
                })
            })
            .await
            .map(|_| ())
            .map_err(|error| match error {
                UpdateError::Rejected(error) | UpdateError::Config(error) => error,
            })
            .map_err(|error| match error {
                ConfigError::Validation { .. } => ImportsError::InvalidInput {
                    message: error.to_string(),
                },
                error if error.is_busy() => ImportsError::Busy {
                    cause: error.to_string(),
                },
                error => ImportsError::internal(&error),
            })
    }
}
