//! Access and inbound apps: security posture and Connect Apps.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{AudioFormat, Section, check_range};
use crate::runtime_config::error::ConfigError;

// --- security_settings ----------------------------------------------------

/// Who may download library files. `trusted` admits trusted and admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DownloadAccess {
    /// Everyone.
    #[default]
    Everyone,
    /// Trusted and admin roles.
    Trusted,
    /// Admins only.
    Admin,
}

/// Security posture settings (no secrets; the HIBP path is a local file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SecuritySettings {
    /// Check passwords against Have-I-Been-Pwned.
    pub hibp_check: bool,
    /// Local HIBP file; used instead of the API when present.
    pub hibp_local_path: String,
    /// HSTS max-age in seconds; 0 disables.
    pub hsts_max_age: i64,
    /// HSTS include-subdomains flag.
    pub hsts_include_subdomains: bool,
    /// HSTS preload flag.
    pub hsts_preload: bool,
    /// Who may download library files.
    pub library_download_access: DownloadAccess,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            hibp_check: true,
            hibp_local_path: String::new(),
            hsts_max_age: 0,
            hsts_include_subdomains: false,
            hsts_preload: false,
            library_download_access: DownloadAccess::Everyone,
        }
    }
}

impl SecuritySettings {
    /// Whether `role` may download library files.
    #[must_use]
    pub fn download_allowed(&self, role: &str) -> bool {
        match self.library_download_access {
            DownloadAccess::Everyone => true,
            DownloadAccess::Trusted => role == "admin" || role == "trusted",
            DownloadAccess::Admin => role == "admin",
        }
    }
}

impl Section for SecuritySettings {
    const KEY: &'static str = "security_settings";
}

// --- connect_apps ---------------------------------------------------------

/// Compat discovery mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum DiscoverMode {
    /// Local library only.
    #[serde(rename = "local-only")]
    #[default]
    LocalOnly,
    /// Lazy MusicBrainz enrichment.
    #[serde(rename = "lazy-mb")]
    LazyMb,
    /// Use scrobble targets.
    #[serde(rename = "use-scrobble-targets")]
    UseScrobbleTargets,
}

/// Inbound Connect Apps config. Both protocols default off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ConnectApps {
    /// Serve the Subsonic API.
    pub subsonic_enabled: bool,
    /// Serve the Jellyfin API.
    pub jellyfin_enabled: bool,
    /// Capability flag for the approval-safe exact-track endpoint.
    pub exact_track_approval_supported: bool,
    /// Allow transcoding for compat clients.
    pub transcoding_enabled: bool,
    /// Default transcode format.
    pub transcode_default_format: AudioFormat,
    /// Transcode ceiling in kbps (32-1411).
    pub transcode_max_bitrate_kbps: i64,
    /// Advertised server name.
    pub advertise_server_name: String,
    /// Advertised server version.
    pub advertise_server_version: String,
    /// Compat discovery mode.
    pub discover_mode: DiscoverMode,
}

impl Default for ConnectApps {
    fn default() -> Self {
        Self {
            subsonic_enabled: false,
            jellyfin_enabled: false,
            exact_track_approval_supported: true,
            transcoding_enabled: true,
            transcode_default_format: AudioFormat::Mp3,
            transcode_max_bitrate_kbps: 320,
            advertise_server_name: "DroppedNeedle".to_owned(),
            advertise_server_version: "10.10.6".to_owned(),
            discover_mode: DiscoverMode::LocalOnly,
        }
    }
}

impl Section for ConnectApps {
    const KEY: &'static str = "connect_apps";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "transcode_max_bitrate_kbps",
            self.transcode_max_bitrate_kbps,
            32,
            1411,
        )?;
        if self.transcode_default_format == AudioFormat::Flac {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "transcode_default_format",
                reason: "must be mp3 or opus".to_owned(),
            });
        }
        Ok(())
    }
}
