//! Typed config sections holding secrets (tier 2 of 2).
//!
//! Secret fields use [`Secret`](super::secret::Secret), so a derived `Debug`
//! can never leak them. Each section lists its secret fields through
//! [`SecretSection::secret_fields`]; the store drives masking, mask
//! resolution, and encryption generically from that list:
//!
//! - masked read: decrypt every secret, replace non-empty with the mask;
//! - raw read: decrypt every secret;
//! - save: incoming mask keeps the stored ciphertext, anything else is
//!   stripped and re-encrypted (empty stays empty).
//!
//! `validate()` never inspects secret contents (on the save path they may
//! hold the mask) and `normalize()` never touches them.

use serde::{Deserialize, Serialize};

use super::error::ConfigError;
use super::mask::{
    ACOUSTID_KEY_MASK, AUDIODB_API_KEY_MASK, JELLYFIN_API_KEY_MASK, LIDARR_API_KEY_MASK,
    LISTENBRAINZ_TOKEN_MASK, NAVIDROME_PASSWORD_MASK, OIDC_SECRET_MASK, PLEX_TOKEN_MASK,
    PROWLARR_API_KEY_MASK, SABNZBD_API_KEY_MASK, SKIDDLE_KEY_MASK, SLSKD_API_KEY_MASK,
    SPOTIFY_SECRET_MASK, TICKETMASTER_KEY_MASK, WRAPPED_API_KEY_MASK, YOUTUBE_API_KEY_MASK,
};
use super::secret::Secret;
use super::sections::{
    DEFAULT_NAMING_TEMPLATE, Section, check_range, is_valid_hhmm, normalize_http_url,
    sanitize_absolute_mount, sanitize_subpath, strip_api_suffix, tier_rank,
};

/// One mutable secret field plus its mask sentinel.
pub struct SecretField<'a> {
    /// The secret value (ciphertext on load, plaintext after decrypt,
    /// mask or new value on save).
    pub value: &'a mut Secret,
    /// This field's exact-match mask.
    pub mask: &'static str,
    /// Whether v2 strips paste whitespace for this secret. True for API
    /// keys (a stray space earns a 403); false for passwords and
    /// verbatim-saved tokens, where edge whitespace may be meaningful.
    pub strip: bool,
}

/// A [`Section`] with encrypted secret fields.
pub trait SecretSection: Section {
    /// Every secret field in struct order. Order is part of the contract:
    /// the store pairs incoming and stored fields positionally, which is
    /// why variable-length lists (indexers) do not implement this trait.
    fn secret_fields(&mut self) -> Vec<SecretField<'_>>;
}

// --- download_client (slskd) ------------------------------------------------
// Dropped: min_bitrate_kbps (v2-deprecated, superseded by quality_min/max, zero consumers).

/// slskd download-client connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlskdConnection {
    /// Master switch.
    pub enabled: bool,
    /// Client type tag.
    pub client_type: String,
    /// slskd base URL.
    pub url: String,
    /// API key (encrypted at rest).
    pub api_key: Secret,
    /// Verify downloads after landing.
    pub verify_downloads: bool,
    /// Worst accepted tier label.
    pub quality_min: String,
    /// Best accepted tier label.
    pub quality_max: String,
    /// FLAC/MP3 only.
    pub flac_mp3_only: bool,
    /// Relative subpath inside the mount where slskd saves (confined).
    pub downloads_subpath: String,
    /// Absolute incomplete-downloads dir ("" disables the fallback).
    pub slskd_incomplete_mount: String,
    /// Preflight auto-accept score.
    pub preflight_score_auto_accept: f64,
    /// Preflight manual-review floor.
    pub preflight_score_manual_min: f64,
    /// Frozen-transfer stall timeout.
    pub download_stall_timeout_minutes: i64,
    /// Remote-queue wait timeout.
    pub download_queued_timeout_minutes: i64,
    /// Preferred-quality wait.
    pub preferred_quality_wait_minutes: i64,
    /// Failover attempts per request.
    pub max_failover_attempts: i64,
    /// Concurrent downloads.
    pub max_concurrent_downloads: i64,
    /// Automatic retry switch.
    pub auto_retry_enabled: bool,
    /// Retry attempts.
    pub auto_retry_max_attempts: i64,
    /// Retry base interval.
    pub auto_retry_base_interval_minutes: i64,
}

impl Default for SlskdConnection {
    fn default() -> Self {
        Self {
            enabled: false,
            client_type: "slskd".to_owned(),
            url: String::new(),
            api_key: Secret::default(),
            verify_downloads: true,
            quality_min: "mp3_320".to_owned(),
            quality_max: "lossless".to_owned(),
            flac_mp3_only: true,
            downloads_subpath: String::new(),
            slskd_incomplete_mount: String::new(),
            preflight_score_auto_accept: 0.70,
            preflight_score_manual_min: 0.50,
            download_stall_timeout_minutes: 30,
            download_queued_timeout_minutes: 120,
            preferred_quality_wait_minutes: 15,
            max_failover_attempts: 3,
            max_concurrent_downloads: 3,
            auto_retry_enabled: true,
            auto_retry_max_attempts: 6,
            auto_retry_base_interval_minutes: 15,
        }
    }
}

impl Section for SlskdConnection {
    const KEY: &'static str = "download_client";

    fn validate(&self) -> Result<(), ConfigError> {
        let key = Self::KEY;
        check_range(
            key,
            "download_stall_timeout_minutes",
            self.download_stall_timeout_minutes,
            2,
            240,
        )?;
        check_range(
            key,
            "download_queued_timeout_minutes",
            self.download_queued_timeout_minutes,
            5,
            1440,
        )?;
        check_range(
            key,
            "preferred_quality_wait_minutes",
            self.preferred_quality_wait_minutes,
            1,
            1440,
        )?;
        check_range(
            key,
            "max_failover_attempts",
            self.max_failover_attempts,
            1,
            10,
        )?;
        check_range(
            key,
            "max_concurrent_downloads",
            self.max_concurrent_downloads,
            1,
            10,
        )?;
        check_range(
            key,
            "auto_retry_max_attempts",
            self.auto_retry_max_attempts,
            0,
            20,
        )?;
        check_range(
            key,
            "auto_retry_base_interval_minutes",
            self.auto_retry_base_interval_minutes,
            1,
            1440,
        )?;
        Ok(())
    }

    fn normalize(&mut self) {
        normalize_http_url(&mut self.url, "https://");
        if tier_rank(&self.quality_min).is_none() {
            self.quality_min = "mp3_320".to_owned();
        }
        if tier_rank(&self.quality_max).is_none() {
            self.quality_max = "lossless".to_owned();
        }
        let min_rank = tier_rank(&self.quality_min).unwrap_or(3);
        let max_rank = tier_rank(&self.quality_max).unwrap_or(4);
        if min_rank > max_rank {
            self.quality_min = self.quality_max.clone();
        }
        self.downloads_subpath = sanitize_subpath(&self.downloads_subpath);
        self.slskd_incomplete_mount = sanitize_absolute_mount(&self.slskd_incomplete_mount);
    }
}

impl SecretSection for SlskdConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: SLSKD_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- download_clients (sabnzbd sub-object) ---------------------------------

/// SABnzbd connection. The key is the full key (the add-only nzbkey cannot
/// do queue/history/delete).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SabnzbdConnection {
    /// Master switch.
    pub enabled: bool,
    /// Client type tag.
    pub client_type: String,
    /// SABnzbd base URL.
    pub url: String,
    /// Full API key (encrypted at rest).
    pub api_key: Secret,
    /// Category (`*`: a fresh SABnzbd has no `droppedneedle` category).
    pub category: String,
    /// Job priority.
    pub priority: i64,
    /// Post-processing level.
    pub post_processing: i64,
    /// Where DroppedNeedle sees SABnzbd's completed dir.
    pub downloads_mount: String,
}

impl Default for SabnzbdConnection {
    fn default() -> Self {
        Self {
            enabled: false,
            client_type: "sabnzbd".to_owned(),
            url: String::new(),
            api_key: Secret::default(),
            category: "*".to_owned(),
            priority: 0,
            post_processing: 3,
            downloads_mount: "/sabnzbd-downloads".to_owned(),
        }
    }
}

/// The `download_clients` section (SABnzbd sub-object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DownloadClients {
    /// SABnzbd connection.
    pub sabnzbd: SabnzbdConnection,
}

impl Section for DownloadClients {
    const KEY: &'static str = "download_clients";

    fn normalize(&mut self) {
        normalize_http_url(&mut self.sabnzbd.url, "https://");
    }
}

impl SecretSection for DownloadClients {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.sabnzbd.api_key,
            mask: SABNZBD_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- indexers (order-preserved Newznab list) --------------------------------

/// One configured Newznab indexer. DroppedNeedle ships none; the user adds
/// their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewznabIndexer {
    /// Stable id.
    pub id: String,
    /// Indexer type tag (stored as `type`; renamed for the keyword).
    #[serde(rename = "type")]
    pub indexer_type: String,
    /// Display name (list identity for import).
    pub name: String,
    /// Indexer base URL.
    pub url: String,
    /// API key (encrypted at rest, per element).
    pub api_key: Secret,
    /// Newznab categories.
    pub categories: Vec<i64>,
    /// Master switch.
    pub enabled: bool,
    /// Priority (lower first).
    pub priority: i64,
}

impl Default for NewznabIndexer {
    fn default() -> Self {
        Self {
            id: String::new(),
            indexer_type: "newznab".to_owned(),
            name: String::new(),
            url: String::new(),
            api_key: Secret::default(),
            categories: vec![3000, 3010, 3040],
            enabled: true,
            priority: 1,
        }
    }
}

/// The `indexers` section: order-preserved list (priority order must
/// survive export/import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Indexers(pub Vec<NewznabIndexer>);

impl Section for Indexers {
    const KEY: &'static str = "indexers";

    fn normalize(&mut self) {
        for indexer in &mut self.0 {
            normalize_http_url(&mut indexer.url, "https://");
        }
    }
}

// No SecretSection impl on purpose: masks resolve per element matched BY ID
// (upsert, delete, reorder), never by list position. `ConfigStore` carries
// the dedicated indexer methods instead, so a whole-list save cannot pair
// the wrong ciphertext with the wrong row.

// --- prowlarr ---------------------------------------------------------------

/// Single Prowlarr connection (Prowlarr multiplexes its own indexers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProwlarrConnection {
    /// Master switch.
    pub enabled: bool,
    /// Prowlarr base URL (LAN service, `http://` default).
    pub url: String,
    /// API key (encrypted at rest).
    pub api_key: Secret,
}

impl Section for ProwlarrConnection {
    const KEY: &'static str = "prowlarr";

    fn normalize(&mut self) {
        normalize_http_url(&mut self.url, "http://");
        strip_api_suffix(&mut self.url);
    }
}

impl SecretSection for ProwlarrConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: PROWLARR_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- lidarr_import (read-only import connection) ----------------------------

/// Read-only Lidarr import connection: a single admin-configured Lidarr the
/// monitored-artist importer reads from. Not a management integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LidarrImportConnection {
    /// Lidarr base URL (plain-http LAN service, `http://` default).
    pub url: String,
    /// API key (encrypted at rest).
    pub api_key: Secret,
}

impl Section for LidarrImportConnection {
    const KEY: &'static str = "lidarr_import";

    fn normalize(&mut self) {
        normalize_http_url(&mut self.url, "http://");
        strip_api_suffix(&mut self.url);
    }
}

impl SecretSection for LidarrImportConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: LIDARR_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- jellyfin_settings (the section owns the URL; mask gap closed) ---------

/// Jellyfin connection. The section URL is the single owner (the top-level
/// mirror and the in-memory `Settings` mutation are gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinConnection {
    /// Jellyfin base URL.
    pub jellyfin_url: String,
    /// API key (encrypted at rest; v2 returned it plaintext on GET).
    pub api_key: Secret,
    /// Jellyfin user id.
    pub user_id: String,
    /// Master switch.
    pub enabled: bool,
    /// Jellyfin login switch.
    pub login_enabled: bool,
}

impl Default for JellyfinConnection {
    fn default() -> Self {
        Self {
            jellyfin_url: "http://jellyfin:8096".to_owned(),
            api_key: Secret::default(),
            user_id: String::new(),
            enabled: false,
            login_enabled: false,
        }
    }
}

impl Section for JellyfinConnection {
    const KEY: &'static str = "jellyfin_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        if !self.jellyfin_url.starts_with("http://") && !self.jellyfin_url.starts_with("https://") {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "jellyfin_url",
                reason: "jellyfin_url must start with http:// or https://".to_owned(),
            });
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.jellyfin_url = self.jellyfin_url.trim().to_owned();
        while self.jellyfin_url.ends_with('/') && self.jellyfin_url.len() > 1 {
            self.jellyfin_url.pop();
        }
    }
}

impl SecretSection for JellyfinConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: JELLYFIN_API_KEY_MASK,
            strip: false,
        }]
    }
}

// --- navidrome_settings -----------------------------------------------------

/// Navidrome connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NavidromeConnection {
    /// Navidrome base URL.
    pub navidrome_url: String,
    /// Username.
    pub username: String,
    /// Password (encrypted at rest).
    pub password: Secret,
    /// Master switch.
    pub enabled: bool,
    /// .m3u8 export switch (off by default).
    pub playlist_sync_enabled: bool,
    /// Export directory Navidrome scans.
    pub playlist_sync_path: String,
    /// Export scope (`public`, or opt-in `all`).
    pub playlist_sync_scope: String,
    /// Remove exports that stop qualifying.
    pub playlist_sync_remove_deleted: bool,
}

impl Default for NavidromeConnection {
    fn default() -> Self {
        Self {
            navidrome_url: String::new(),
            username: String::new(),
            password: Secret::default(),
            enabled: false,
            playlist_sync_enabled: false,
            playlist_sync_path: String::new(),
            playlist_sync_scope: "public".to_owned(),
            playlist_sync_remove_deleted: true,
        }
    }
}

impl Section for NavidromeConnection {
    const KEY: &'static str = "navidrome_settings";

    fn normalize(&mut self) {
        self.navidrome_url = self.navidrome_url.trim().to_owned();
        while self.navidrome_url.ends_with('/') && self.navidrome_url.len() > 1 {
            self.navidrome_url.pop();
        }
        self.playlist_sync_path = self.playlist_sync_path.trim().to_owned();
        if self.playlist_sync_scope != "all" {
            self.playlist_sync_scope = "public".to_owned();
        }
    }
}

impl SecretSection for NavidromeConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.password,
            mask: NAVIDROME_PASSWORD_MASK,
            strip: false,
        }]
    }
}

// --- plex_settings ----------------------------------------------------------

/// Plex connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlexConnection {
    /// Plex base URL.
    pub plex_url: String,
    /// Plex token (encrypted at rest).
    pub plex_token: Secret,
    /// Master switch.
    pub enabled: bool,
    /// Plex login switch.
    pub login_enabled: bool,
    /// Music library ids.
    pub music_library_ids: Vec<String>,
    /// Scrobble back to Plex.
    pub scrobble_to_plex: bool,
}

impl Default for PlexConnection {
    fn default() -> Self {
        Self {
            plex_url: String::new(),
            plex_token: Secret::default(),
            enabled: false,
            login_enabled: false,
            music_library_ids: Vec::new(),
            scrobble_to_plex: true,
        }
    }
}

impl Section for PlexConnection {
    const KEY: &'static str = "plex_settings";

    fn normalize(&mut self) {
        self.plex_url = self.plex_url.trim().to_owned();
        while self.plex_url.ends_with('/') && self.plex_url.len() > 1 {
            self.plex_url.pop();
        }
    }
}

impl SecretSection for PlexConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.plex_token,
            mask: PLEX_TOKEN_MASK,
            strip: false,
        }]
    }
}

// --- listenbrainz_settings (mask gap closed) --------------------------------

/// ListenBrainz connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListenBrainzConnection {
    /// Username.
    pub username: String,
    /// User token (encrypted at rest; v2 returned it plaintext on GET).
    pub user_token: Secret,
    /// Master switch.
    pub enabled: bool,
}

impl Section for ListenBrainzConnection {
    const KEY: &'static str = "listenbrainz_settings";
}

impl SecretSection for ListenBrainzConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.user_token,
            mask: LISTENBRAINZ_TOKEN_MASK,
            strip: false,
        }]
    }
}

// --- youtube_settings (mask gap closed) -------------------------------------

/// YouTube connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct YouTubeConnection {
    /// API key (encrypted at rest; v2 returned it plaintext on GET).
    pub api_key: Secret,
    /// Master switch.
    pub enabled: bool,
    /// API search switch.
    pub api_enabled: bool,
    /// Daily quota limit (1-10000).
    pub daily_quota_limit: i64,
}

impl Default for YouTubeConnection {
    fn default() -> Self {
        Self {
            api_key: Secret::default(),
            enabled: false,
            api_enabled: false,
            daily_quota_limit: 80,
        }
    }
}

impl Section for YouTubeConnection {
    const KEY: &'static str = "youtube_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "daily_quota_limit",
            self.daily_quota_limit,
            1,
            10000,
        )
    }
}

impl SecretSection for YouTubeConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: YOUTUBE_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- spotify_settings -------------------------------------------------------

/// Spotify settings (OAuth client + import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SpotifySettings {
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret (encrypted at rest).
    pub client_secret: Secret,
    /// Master switch.
    pub enabled: bool,
    /// Redirect origin for OAuth.
    pub spotify_redirect_origin: String,
}

/// Whether the redirect origin is a bare http(s) origin: absolute URL
/// with a host and no path, query, or fragment (v2 GH-298: anything else
/// silently corrupts the value admins register in the Spotify dashboard).
/// Empty means the dynamic fallback and is always accepted.
#[must_use]
pub fn is_valid_spotify_redirect_origin(origin: &str) -> bool {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(after_scheme) = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
    else {
        return false;
    };
    if after_scheme.is_empty() {
        return false;
    }
    let host_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    if host_end == 0 {
        return false;
    }
    let rest = &after_scheme[host_end..];
    rest.is_empty() || rest == "/"
}

impl Section for SpotifySettings {
    const KEY: &'static str = "spotify_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_valid_spotify_redirect_origin(&self.spotify_redirect_origin) {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "spotify_redirect_origin",
                reason: "Spotify redirect origin must be an absolute http(s) URL \
                     with no path, query, or fragment"
                    .to_owned(),
            });
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.client_id = self.client_id.trim().to_owned();
        self.spotify_redirect_origin = self.spotify_redirect_origin.trim().to_owned();
        while self.spotify_redirect_origin.ends_with('/') && self.spotify_redirect_origin.len() > 1
        {
            self.spotify_redirect_origin.pop();
        }
    }
}

impl SecretSection for SpotifySettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.client_secret,
            mask: SPOTIFY_SECRET_MASK,
            strip: false,
        }]
    }
}

// --- events -----------------------------------------------------------------

/// Events sweep scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventsSweepScope {
    /// Followed artists only.
    #[default]
    Followed,
    /// Every artist in the library index.
    Library,
}

/// Upcoming-events sources. The sweep runs daily at `poll_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventsSettings {
    /// Master switch.
    pub enabled: bool,
    /// Ticketmaster switch.
    pub ticketmaster_enabled: bool,
    /// Ticketmaster key (encrypted at rest).
    pub ticketmaster_api_key: Secret,
    /// Skiddle switch.
    pub skiddle_enabled: bool,
    /// Skiddle key (encrypted at rest).
    pub skiddle_api_key: Secret,
    /// Daily sweep time, server-local `HH:MM`.
    pub poll_time: String,
    /// Sweep scope.
    pub sweep_scope: EventsSweepScope,
}

impl Default for EventsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ticketmaster_enabled: false,
            ticketmaster_api_key: Secret::default(),
            skiddle_enabled: false,
            skiddle_api_key: Secret::default(),
            poll_time: "06:00".to_owned(),
            sweep_scope: EventsSweepScope::Followed,
        }
    }
}

impl Section for EventsSettings {
    const KEY: &'static str = "events";

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_valid_hhmm(&self.poll_time) {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "poll_time",
                reason: format!("must be HH:MM (00:00-23:59), got {:?}", self.poll_time),
            });
        }
        Ok(())
    }
}

impl SecretSection for EventsSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![
            SecretField {
                value: &mut self.ticketmaster_api_key,
                mask: TICKETMASTER_KEY_MASK,
                strip: true,
            },
            SecretField {
                value: &mut self.skiddle_api_key,
                mask: SKIDDLE_KEY_MASK,
                strip: true,
            },
        ]
    }
}

// --- wrapped_settings -------------------------------------------------------

/// Shared secret for the wrapped endpoints (service-to-service).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WrappedSettings {
    /// API key (encrypted at rest).
    pub api_key: Secret,
}

impl Section for WrappedSettings {
    const KEY: &'static str = "wrapped_settings";
}

impl SecretSection for WrappedSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: WRAPPED_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- oidc_settings ----------------------------------------------------------

/// OIDC connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OidcConnection {
    /// Master switch.
    pub enabled: bool,
    /// Issuer URL.
    pub issuer: String,
    /// Client id.
    pub client_id: String,
    /// Client secret (encrypted at rest).
    pub client_secret: Secret,
    /// Scopes.
    pub scopes: String,
    /// Redirect URI.
    pub redirect_uri: String,
}

impl Default for OidcConnection {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: Secret::default(),
            scopes: "openid email profile".to_owned(),
            redirect_uri: String::new(),
        }
    }
}

impl Section for OidcConnection {
    const KEY: &'static str = "oidc_settings";
}

impl SecretSection for OidcConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.client_secret,
            mask: OIDC_SECRET_MASK,
            strip: false,
        }]
    }
}

// --- library_settings (typed roots + policies) ------------------------------

/// Per-path identification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationPolicy {
    /// Trust local metadata only.
    LocalMetadata,
    /// Automatic identification.
    #[default]
    Automatic,
    /// Excluded from the library.
    Excluded,
}

/// One path-policy rule inside a root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LibraryPathRule {
    /// Rule id.
    pub id: String,
    /// Path relative to the root.
    pub relative_path: String,
    /// Policy for this path.
    pub policy: IdentificationPolicy,
}

/// One library root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryRoot {
    /// Stable root id.
    pub id: String,
    /// Absolute path.
    pub path: String,
    /// Display label.
    pub label: String,
    /// Default policy.
    pub policy: IdentificationPolicy,
    /// Path rules.
    pub rules: Vec<LibraryPathRule>,
}

impl Default for LibraryRoot {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: String::new(),
            label: String::new(),
            policy: IdentificationPolicy::Automatic,
            rules: Vec::new(),
        }
    }
}

/// Typed library settings: roots, policies, staging, naming, AcoustID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TypedLibrary {
    /// Library roots.
    pub library_roots: Vec<LibraryRoot>,
    /// Staging path.
    pub staging_path: String,
    /// Naming template.
    pub naming_template: String,
    /// AcoustID key (encrypted at rest).
    pub acoustid_api_key: Secret,
    /// Master switch: when false the app claims no new library work.
    pub enabled: bool,
}

impl Default for TypedLibrary {
    fn default() -> Self {
        Self {
            library_roots: Vec::new(),
            staging_path: String::new(),
            naming_template: DEFAULT_NAMING_TEMPLATE.to_owned(),
            acoustid_api_key: Secret::default(),
            enabled: true,
        }
    }
}

impl Section for TypedLibrary {
    const KEY: &'static str = "library_settings";
}

impl SecretSection for TypedLibrary {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.acoustid_api_key,
            mask: ACOUSTID_KEY_MASK,
            strip: false,
        }]
    }
}

// --- advanced_settings (closed export allowlist) ---------------------------
// Kept: user-meaningful TTL/perf fields below. Dropped as internal tuning:
// artist_discovery_warm_interval, artist_discovery_warm_delay,
// artist_discovery_precache_delay, artist_discovery_precache_concurrency,
// discover_queue_warm_cycle_build, discover_queue_similar_artists_limit,
// discover_queue_albums_per_similar, discover_queue_enrich_ttl,
// discover_queue_lastfm_mbid_max_lookups, audiodb_prewarm_concurrency,
// audiodb_prewarm_delay, cache_ttl_recently_viewed_bytes,
// cache_ttl_local_files_recently_added.
// The AudioDB key is encrypted at rest now (v2 stored it plaintext).

/// Advanced tuning: cache TTLs, HTTP trio, batching, queues, AudioDB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedSettings {
    /// Library album TTL.
    pub cache_ttl_album_library: i64,
    /// Non-library album TTL.
    pub cache_ttl_album_non_library: i64,
    /// Library artist TTL.
    pub cache_ttl_artist_library: i64,
    /// Non-library artist TTL.
    pub cache_ttl_artist_non_library: i64,
    /// Library discovery TTL.
    pub cache_ttl_artist_discovery_library: i64,
    /// Non-library discovery TTL.
    pub cache_ttl_artist_discovery_non_library: i64,
    /// Search TTL.
    pub cache_ttl_search: i64,
    /// Jellyfin recently-played TTL.
    pub cache_ttl_jellyfin_recently_played: i64,
    /// Jellyfin favorites TTL.
    pub cache_ttl_jellyfin_favorites: i64,
    /// Jellyfin genres TTL.
    pub cache_ttl_jellyfin_genres: i64,
    /// Jellyfin library-stats TTL.
    pub cache_ttl_jellyfin_library_stats: i64,
    /// Navidrome albums TTL.
    pub cache_ttl_navidrome_albums: i64,
    /// Navidrome artists TTL.
    pub cache_ttl_navidrome_artists: i64,
    /// Navidrome recent TTL.
    pub cache_ttl_navidrome_recent: i64,
    /// Navidrome favorites TTL.
    pub cache_ttl_navidrome_favorites: i64,
    /// Navidrome search TTL.
    pub cache_ttl_navidrome_search: i64,
    /// Navidrome genres TTL.
    pub cache_ttl_navidrome_genres: i64,
    /// Navidrome stats TTL.
    pub cache_ttl_navidrome_stats: i64,
    /// Plex albums TTL.
    pub cache_ttl_plex_albums: i64,
    /// Plex search TTL.
    pub cache_ttl_plex_search: i64,
    /// Plex genres TTL.
    pub cache_ttl_plex_genres: i64,
    /// Plex stats TTL.
    pub cache_ttl_plex_stats: i64,
    /// Outbound HTTP timeout (overrides the factory default).
    pub http_timeout: i64,
    /// Outbound connect timeout.
    pub http_connect_timeout: i64,
    /// Outbound pool size.
    pub http_max_connections: i64,
    /// Artist-image batch size.
    pub batch_artist_images: i64,
    /// Album batch size.
    pub batch_albums: i64,
    /// Artist delay.
    pub delay_artist: f64,
    /// Album delay.
    pub delay_albums: f64,
    /// Memory-cache entries.
    pub memory_cache_max_entries: i64,
    /// Memory-cache cleanup cadence.
    pub memory_cache_cleanup_interval: i64,
    /// Cover memory-cache entries.
    pub cover_memory_cache_max_entries: i64,
    /// Cover memory-cache MB.
    pub cover_memory_cache_max_size_mb: i64,
    /// Disk-cache cleanup cadence.
    pub disk_cache_cleanup_interval: i64,
    /// Recent-metadata MB.
    pub recent_metadata_max_size_mb: i64,
    /// Recent-covers MB.
    pub recent_covers_max_size_mb: i64,
    /// Persistent-metadata TTL hours.
    pub persistent_metadata_ttl_hours: i64,
    /// Discover queue size.
    pub discover_queue_size: i64,
    /// Discover queue TTL.
    pub discover_queue_ttl: i64,
    /// Discover queue auto-generate.
    pub discover_queue_auto_generate: bool,
    /// Discover queue polling ms.
    pub discover_queue_polling_interval: i64,
    /// Discover seed artists.
    pub discover_queue_seed_artists: i64,
    /// Discover wildcard slots.
    pub discover_queue_wildcard_slots: i64,
    /// Discover picks count.
    pub discover_picks_count: i64,
    /// Discover genre-affinity weight.
    pub discover_picks_genre_affinity_weight: f64,
    /// Frontend home TTL ms.
    pub frontend_ttl_home: i64,
    /// Frontend discover TTL ms.
    pub frontend_ttl_discover: i64,
    /// Frontend library TTL ms.
    pub frontend_ttl_library: i64,
    /// Frontend recently-added TTL ms.
    pub frontend_ttl_recently_added: i64,
    /// Frontend discover-queue TTL ms.
    pub frontend_ttl_discover_queue: i64,
    /// Frontend search TTL ms.
    pub frontend_ttl_search: i64,
    /// Frontend local-files sidebar TTL ms.
    pub frontend_ttl_local_files_sidebar: i64,
    /// Frontend Jellyfin sidebar TTL ms.
    pub frontend_ttl_jellyfin_sidebar: i64,
    /// Frontend Plex sidebar TTL ms.
    pub frontend_ttl_plex_sidebar: i64,
    /// Frontend playlist-sources TTL ms.
    pub frontend_ttl_playlist_sources: i64,
    /// AudioDB master switch.
    pub audiodb_enabled: bool,
    /// AudioDB name-search fallback.
    pub audiodb_name_search_fallback: bool,
    /// Direct remote images.
    pub direct_remote_images_enabled: bool,
    /// Prefer local cover art.
    pub prefer_local_cover_art: bool,
    /// AudioDB key (encrypted at rest; v2 stored it plaintext). A missing
    /// field reads as empty (then the read path falls back to the "123"
    /// default); only an explicitly stored value decrypts.
    #[serde(default)]
    pub audiodb_api_key: Secret,
    /// AudioDB hit TTL.
    pub cache_ttl_audiodb_found: i64,
    /// AudioDB miss TTL.
    pub cache_ttl_audiodb_not_found: i64,
    /// AudioDB library TTL.
    pub cache_ttl_audiodb_library: i64,
    /// Sync stall timeout minutes.
    pub sync_stall_timeout_minutes: i64,
    /// Sync max timeout hours.
    pub sync_max_timeout_hours: i64,
    /// Genre-section TTL.
    pub genre_section_ttl: i64,
    /// Request concurrency.
    pub request_concurrency: i64,
    /// Request-history retention days.
    pub request_history_retention_days: i64,
    /// Ignored-releases retention days.
    pub ignored_releases_retention_days: i64,
    /// Orphan-cover demote cadence hours.
    pub orphan_cover_demote_interval_hours: i64,
    /// Store-prune cadence hours.
    pub store_prune_interval_hours: i64,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            cache_ttl_album_library: 86400,
            cache_ttl_album_non_library: 21600,
            cache_ttl_artist_library: 21600,
            cache_ttl_artist_non_library: 21600,
            cache_ttl_artist_discovery_library: 21600,
            cache_ttl_artist_discovery_non_library: 3600,
            cache_ttl_search: 3600,
            cache_ttl_jellyfin_recently_played: 300,
            cache_ttl_jellyfin_favorites: 300,
            cache_ttl_jellyfin_genres: 3600,
            cache_ttl_jellyfin_library_stats: 600,
            cache_ttl_navidrome_albums: 300,
            cache_ttl_navidrome_artists: 300,
            cache_ttl_navidrome_recent: 120,
            cache_ttl_navidrome_favorites: 120,
            cache_ttl_navidrome_search: 120,
            cache_ttl_navidrome_genres: 3600,
            cache_ttl_navidrome_stats: 600,
            cache_ttl_plex_albums: 300,
            cache_ttl_plex_search: 120,
            cache_ttl_plex_genres: 3600,
            cache_ttl_plex_stats: 600,
            http_timeout: 10,
            http_connect_timeout: 5,
            http_max_connections: 200,
            batch_artist_images: 10,
            batch_albums: 8,
            delay_artist: 0.5,
            delay_albums: 0.3,
            memory_cache_max_entries: 10000,
            memory_cache_cleanup_interval: 300,
            cover_memory_cache_max_entries: 128,
            cover_memory_cache_max_size_mb: 16,
            disk_cache_cleanup_interval: 600,
            recent_metadata_max_size_mb: 500,
            recent_covers_max_size_mb: 1024,
            persistent_metadata_ttl_hours: 24,
            discover_queue_size: 10,
            discover_queue_ttl: 86400,
            discover_queue_auto_generate: true,
            discover_queue_polling_interval: 4000,
            discover_queue_seed_artists: 3,
            discover_queue_wildcard_slots: 2,
            discover_picks_count: 12,
            discover_picks_genre_affinity_weight: 0.7,
            frontend_ttl_home: 300000,
            frontend_ttl_discover: 1800000,
            frontend_ttl_library: 300000,
            frontend_ttl_recently_added: 300000,
            frontend_ttl_discover_queue: 86400000,
            frontend_ttl_search: 300000,
            frontend_ttl_local_files_sidebar: 120000,
            frontend_ttl_jellyfin_sidebar: 120000,
            frontend_ttl_plex_sidebar: 120000,
            frontend_ttl_playlist_sources: 900000,
            audiodb_enabled: true,
            audiodb_name_search_fallback: false,
            direct_remote_images_enabled: true,
            prefer_local_cover_art: true,
            audiodb_api_key: Secret::default(),
            cache_ttl_audiodb_found: 604800,
            cache_ttl_audiodb_not_found: 86400,
            cache_ttl_audiodb_library: 1209600,
            sync_stall_timeout_minutes: 10,
            sync_max_timeout_hours: 8,
            genre_section_ttl: 21600,
            request_concurrency: 2,
            request_history_retention_days: 180,
            ignored_releases_retention_days: 365,
            orphan_cover_demote_interval_hours: 24,
            store_prune_interval_hours: 6,
        }
    }
}

impl Section for AdvancedSettings {
    const KEY: &'static str = "advanced_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        let key = Self::KEY;
        let ints: &[(&str, i64, i64, i64)] = &[
            (
                "cache_ttl_album_library",
                self.cache_ttl_album_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_album_non_library",
                self.cache_ttl_album_non_library,
                60,
                86400,
            ),
            (
                "cache_ttl_artist_library",
                self.cache_ttl_artist_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_artist_non_library",
                self.cache_ttl_artist_non_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_artist_discovery_library",
                self.cache_ttl_artist_discovery_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_artist_discovery_non_library",
                self.cache_ttl_artist_discovery_non_library,
                3600,
                604800,
            ),
            ("cache_ttl_search", self.cache_ttl_search, 60, 86400),
            (
                "cache_ttl_jellyfin_recently_played",
                self.cache_ttl_jellyfin_recently_played,
                60,
                3600,
            ),
            (
                "cache_ttl_jellyfin_favorites",
                self.cache_ttl_jellyfin_favorites,
                60,
                3600,
            ),
            (
                "cache_ttl_jellyfin_genres",
                self.cache_ttl_jellyfin_genres,
                60,
                86400,
            ),
            (
                "cache_ttl_jellyfin_library_stats",
                self.cache_ttl_jellyfin_library_stats,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_albums",
                self.cache_ttl_navidrome_albums,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_artists",
                self.cache_ttl_navidrome_artists,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_recent",
                self.cache_ttl_navidrome_recent,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_favorites",
                self.cache_ttl_navidrome_favorites,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_search",
                self.cache_ttl_navidrome_search,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_genres",
                self.cache_ttl_navidrome_genres,
                60,
                86400,
            ),
            (
                "cache_ttl_navidrome_stats",
                self.cache_ttl_navidrome_stats,
                60,
                3600,
            ),
            (
                "cache_ttl_plex_albums",
                self.cache_ttl_plex_albums,
                60,
                3600,
            ),
            (
                "cache_ttl_plex_search",
                self.cache_ttl_plex_search,
                60,
                3600,
            ),
            (
                "cache_ttl_plex_genres",
                self.cache_ttl_plex_genres,
                60,
                86400,
            ),
            ("cache_ttl_plex_stats", self.cache_ttl_plex_stats, 60, 3600),
            ("http_timeout", self.http_timeout, 5, 60),
            ("http_connect_timeout", self.http_connect_timeout, 1, 30),
            ("http_max_connections", self.http_max_connections, 50, 500),
            ("batch_artist_images", self.batch_artist_images, 1, 20),
            ("batch_albums", self.batch_albums, 1, 20),
            (
                "memory_cache_max_entries",
                self.memory_cache_max_entries,
                1000,
                100000,
            ),
            (
                "memory_cache_cleanup_interval",
                self.memory_cache_cleanup_interval,
                60,
                3600,
            ),
            (
                "cover_memory_cache_max_entries",
                self.cover_memory_cache_max_entries,
                16,
                2048,
            ),
            (
                "cover_memory_cache_max_size_mb",
                self.cover_memory_cache_max_size_mb,
                1,
                1024,
            ),
            (
                "disk_cache_cleanup_interval",
                self.disk_cache_cleanup_interval,
                60,
                3600,
            ),
            (
                "recent_metadata_max_size_mb",
                self.recent_metadata_max_size_mb,
                100,
                5000,
            ),
            (
                "recent_covers_max_size_mb",
                self.recent_covers_max_size_mb,
                100,
                10000,
            ),
            (
                "persistent_metadata_ttl_hours",
                self.persistent_metadata_ttl_hours,
                1,
                168,
            ),
            ("discover_queue_size", self.discover_queue_size, 1, 20),
            ("discover_queue_ttl", self.discover_queue_ttl, 3600, 604800),
            (
                "discover_queue_polling_interval",
                self.discover_queue_polling_interval,
                1000,
                30000,
            ),
            (
                "discover_queue_seed_artists",
                self.discover_queue_seed_artists,
                1,
                10,
            ),
            (
                "discover_queue_wildcard_slots",
                self.discover_queue_wildcard_slots,
                0,
                10,
            ),
            ("discover_picks_count", self.discover_picks_count, 4, 30),
            ("frontend_ttl_home", self.frontend_ttl_home, 60000, 3600000),
            (
                "frontend_ttl_discover",
                self.frontend_ttl_discover,
                60000,
                86400000,
            ),
            (
                "frontend_ttl_library",
                self.frontend_ttl_library,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_recently_added",
                self.frontend_ttl_recently_added,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_discover_queue",
                self.frontend_ttl_discover_queue,
                3600000,
                604800000,
            ),
            (
                "frontend_ttl_search",
                self.frontend_ttl_search,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_local_files_sidebar",
                self.frontend_ttl_local_files_sidebar,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_jellyfin_sidebar",
                self.frontend_ttl_jellyfin_sidebar,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_plex_sidebar",
                self.frontend_ttl_plex_sidebar,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_playlist_sources",
                self.frontend_ttl_playlist_sources,
                60000,
                3600000,
            ),
            (
                "cache_ttl_audiodb_found",
                self.cache_ttl_audiodb_found,
                3600,
                2592000,
            ),
            (
                "cache_ttl_audiodb_not_found",
                self.cache_ttl_audiodb_not_found,
                3600,
                604800,
            ),
            (
                "cache_ttl_audiodb_library",
                self.cache_ttl_audiodb_library,
                86400,
                2592000,
            ),
            (
                "sync_stall_timeout_minutes",
                self.sync_stall_timeout_minutes,
                2,
                30,
            ),
            ("sync_max_timeout_hours", self.sync_max_timeout_hours, 1, 48),
            ("genre_section_ttl", self.genre_section_ttl, 3600, 604800),
            ("request_concurrency", self.request_concurrency, 1, 5),
            (
                "request_history_retention_days",
                self.request_history_retention_days,
                30,
                3650,
            ),
            (
                "ignored_releases_retention_days",
                self.ignored_releases_retention_days,
                30,
                3650,
            ),
            (
                "orphan_cover_demote_interval_hours",
                self.orphan_cover_demote_interval_hours,
                1,
                168,
            ),
            (
                "store_prune_interval_hours",
                self.store_prune_interval_hours,
                1,
                168,
            ),
        ];
        for (field, value, min, max) in ints {
            check_range(key, field, *value, *min, *max)?;
        }
        let floats: &[(&str, f64, f64, f64)] = &[
            ("delay_artist", self.delay_artist, 0.0, 5.0),
            ("delay_albums", self.delay_albums, 0.0, 5.0),
            (
                "discover_picks_genre_affinity_weight",
                self.discover_picks_genre_affinity_weight,
                0.0,
                1.0,
            ),
        ];
        for (field, value, min, max) in floats {
            check_range(key, field, *value, *min, *max)?;
        }
        Ok(())
    }

    fn normalize(&mut self) {
        if self.audiodb_api_key.expose().trim().is_empty() {
            self.audiodb_api_key = Secret::new("123");
        }
    }
}

impl SecretSection for AdvancedSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.audiodb_api_key,
            mask: AUDIODB_API_KEY_MASK,
            strip: false,
        }]
    }
}
