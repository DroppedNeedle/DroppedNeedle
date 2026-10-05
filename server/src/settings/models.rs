//! Wire DTOs for the settings surface.
//!
//! Every shape here mirrors its typed config section field for field, so a
//! GET response round-trips through PUT byte-identically (masks aside).
//! Secret fields are plain strings on the wire: reads carry the mask when a
//! secret is set (empty when unset), and a save whose value equals the mask
//! keeps the stored ciphertext. `services` bridges these DTOs to the
//! section types; unknown JSON fields decode leniently except on the
//! allowlisted advanced shape, which rejects them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// --- shared enums (mirror the section enums exactly) -------------------------

/// Automatic-scan cadence values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum ScanFrequencyDto {
    /// Never scan automatically.
    #[serde(rename = "manual")]
    Manual,
    /// Every 5 minutes.
    #[serde(rename = "5min")]
    Min5,
    /// Every 10 minutes.
    #[serde(rename = "10min")]
    Min10,
    /// Every 30 minutes.
    #[serde(rename = "30min")]
    Min30,
    /// Every hour.
    #[serde(rename = "1hr")]
    Hr1,
    /// Every 6 hours.
    #[serde(rename = "6hr")]
    Hr6,
    /// Every 12 hours.
    #[serde(rename = "12hr")]
    Hr12,
    /// Every 24 hours (rolling gap).
    #[serde(rename = "24hr")]
    #[default]
    Hr24,
    /// Every 3 days.
    #[serde(rename = "3d")]
    Days3,
    /// Every 7 days.
    #[serde(rename = "7d")]
    Days7,
    /// Once a day at `daily_scan_time`.
    #[serde(rename = "daily")]
    Daily,
}

/// Which service backs scrobble-targeted discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MusicSourceDto {
    /// ListenBrainz.
    #[default]
    Listenbrainz,
    /// Last.fm.
    #[serde(rename = "lastfm")]
    Lastfm,
}

/// Transcode target for remote playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormatDto {
    /// FLAC.
    #[default]
    Flac,
    /// MP3.
    Mp3,
    /// Opus (transcode targets only).
    Opus,
}

/// Who may download library files. `trusted` admits trusted AND admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DownloadAccessDto {
    /// Everyone.
    #[default]
    Everyone,
    /// Trusted and admin roles.
    Trusted,
    /// Admins only.
    Admin,
}

/// Compat discovery mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum DiscoverModeDto {
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

/// Which Usenet search side is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum UsenetBackendDto {
    /// The native Newznab priority list.
    #[default]
    Indexers,
    /// The single Prowlarr connection.
    Prowlarr,
}

/// Events sweep scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventsSweepScopeDto {
    /// Followed artists only.
    #[default]
    Followed,
    /// Every artist in the library index.
    Library,
}

/// MusicBrainz source tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MbSourceModeDto {
    /// musicbrainz.org, hard 1 req/s ceiling.
    Official,
    /// User-owned mirror.
    Mirror,
    /// Community infrastructure.
    Community,
    /// Built-in server-owned source.
    #[default]
    Brainzmash,
}

/// Per-path identification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationPolicyDto {
    /// Trust local metadata only.
    LocalMetadata,
    /// Automatic identification.
    #[default]
    Automatic,
    /// Excluded from the library.
    Excluded,
}

// --- user_preferences --------------------------------------------------------

/// Release-type filters for discovery and search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct UserPreferencesDto {
    /// Primary release types (album, ep, single, ...).
    pub primary_types: Vec<String>,
    /// Secondary release types (studio, live, ...).
    pub secondary_types: Vec<String>,
}

impl Default for UserPreferencesDto {
    fn default() -> Self {
        Self {
            primary_types: ["album", "ep", "single"]
                .iter()
                .map(ToString::to_string)
                .collect(),
            secondary_types: ["studio"].iter().map(ToString::to_string).collect(),
        }
    }
}

// --- library_scan_schedule ---------------------------------------------------

/// Native automatic-scan schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryScanScheduleDto {
    /// Rolling-gap cadence, or `daily` for a fixed clock time.
    pub scan_frequency: ScanFrequencyDto,
    /// Server-local `HH:MM` used when `scan_frequency` is `daily`.
    pub daily_scan_time: String,
    /// Unix time of the last scan, if any.
    pub last_scan: Option<i64>,
    /// Whether the last scan succeeded.
    pub last_scan_success: bool,
}

impl Default for LibraryScanScheduleDto {
    fn default() -> Self {
        Self {
            scan_frequency: ScanFrequencyDto::Hr24,
            daily_scan_time: "03:00".to_owned(),
            last_scan: None,
            last_scan_success: true,
        }
    }
}

/// GET payload: the persisted schedule plus the server's timezone label, so
/// the UI can show what "daily at HH:MM" is relative to. The label is
/// computed per request and never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryScanScheduleResponse {
    /// Rolling-gap cadence, or `daily` for a fixed clock time.
    pub scan_frequency: ScanFrequencyDto,
    /// Server-local `HH:MM` used when `scan_frequency` is `daily`.
    pub daily_scan_time: String,
    /// Unix time of the last scan, if any.
    pub last_scan: Option<i64>,
    /// Whether the last scan succeeded.
    pub last_scan_success: bool,
    /// Server-local timezone label for the daily-scan picker.
    pub server_timezone: String,
}

impl Default for LibraryScanScheduleResponse {
    fn default() -> Self {
        Self {
            scan_frequency: ScanFrequencyDto::Hr24,
            daily_scan_time: "03:00".to_owned(),
            last_scan: None,
            last_scan_success: true,
            server_timezone: String::new(),
        }
    }
}

// --- library_scan_filesystem_watcher -----------------------------------------

/// Zero-dependency filesystem poller knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FilesystemWatcherDto {
    /// Master switch.
    pub enabled: bool,
    /// Stat-snapshot cadence in seconds (minimum 1).
    pub poll_interval_seconds: f64,
    /// Burst-collapse window in seconds (minimum 0).
    pub batch_window_seconds: f64,
}

impl Default for FilesystemWatcherDto {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_seconds: 300.0,
            batch_window_seconds: 60.0,
        }
    }
}

// --- wanted ------------------------------------------------------------------

/// Wanted watcher toggles. Cadence stays code constants on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct WantedWatcherDto {
    /// Master switch (rollback lever, read per sweep).
    pub enabled: bool,
    /// Off means badge-only even for auto-tier finds.
    pub auto_download_on_find: bool,
    /// Watch partial albums too.
    pub watch_partial_albums: bool,
    /// Max artist checks per sweep.
    pub max_checks_per_sweep: i64,
    /// Days before a watch goes dormant.
    pub dormant_after_days: i64,
}

impl Default for WantedWatcherDto {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_download_on_find: true,
            watch_partial_albums: true,
            max_checks_per_sweep: 3,
            dormant_after_days: 365,
        }
    }
}

// --- source_priority ---------------------------------------------------------

/// Acquisition source try-order, e.g. `["soulseek", "usenet"]`. Bundled
/// sources are always present; well-formed `plugin:<name>` keys pass
/// through order-preserved; anything else is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SourcePriorityDto {
    /// Try-order, best first.
    pub order: Vec<String>,
}

// --- usenet_search_backend ---------------------------------------------------

/// The active Usenet search backend (either/or): `"indexers"` for the
/// native Newznab priority list, `"prowlarr"` for the single Prowlarr
/// connection. Unknown values are a 400, never a silent reset.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct UsenetSearchBackendDto {
    /// Active backend.
    pub backend: UsenetBackendDto,
}

// --- scrobble_settings / primary_music_source --------------------------------

/// Scrobble targets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ScrobbleSettingsDto {
    /// Scrobble to Last.fm (per-user credentials).
    pub scrobble_to_lastfm: bool,
    /// Scrobble to ListenBrainz.
    pub scrobble_to_listenbrainz: bool,
}

/// Primary music source selection.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PrimaryMusicSourceDto {
    /// Active source.
    pub source: MusicSourceDto,
}

// --- free_music / get_it -----------------------------------------------------

/// Free-music acquisition settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FreeMusicDto {
    /// Master switch.
    pub enabled: bool,
    /// Preferred format (`flac` or `mp3`).
    pub preferred_format: AudioFormatDto,
}

impl Default for FreeMusicDto {
    fn default() -> Self {
        Self {
            enabled: true,
            preferred_format: AudioFormatDto::Flac,
        }
    }
}

/// Store-region settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct GetItDto {
    /// Two-letter store region.
    pub store_region: String,
}

impl Default for GetItDto {
    fn default() -> Self {
        Self {
            store_region: "US".to_owned(),
        }
    }
}

// --- security_settings -------------------------------------------------------

/// Security posture settings (no secrets; the HIBP path is a local file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SecuritySettingsDto {
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
    pub library_download_access: DownloadAccessDto,
}

impl Default for SecuritySettingsDto {
    fn default() -> Self {
        Self {
            hibp_check: true,
            hibp_local_path: String::new(),
            hsts_max_age: 0,
            hsts_include_subdomains: false,
            hsts_preload: false,
            library_download_access: DownloadAccessDto::Everyone,
        }
    }
}

// --- connect_apps ------------------------------------------------------------

/// Inbound Connect Apps config. Both protocols default OFF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ConnectAppsDto {
    /// Serve the Subsonic API.
    pub subsonic_enabled: bool,
    /// Serve the Jellyfin API.
    pub jellyfin_enabled: bool,
    /// Capability flag for the approval-safe exact-track endpoint.
    pub exact_track_approval_supported: bool,
    /// Allow transcoding for compat clients.
    pub transcoding_enabled: bool,
    /// Default transcode format (`mp3` or `opus`).
    pub transcode_default_format: AudioFormatDto,
    /// Transcode ceiling in kbps (32-1411).
    pub transcode_max_bitrate_kbps: i64,
    /// Advertised server name.
    pub advertise_server_name: String,
    /// Advertised server version.
    pub advertise_server_version: String,
    /// Compat discovery mode.
    pub discover_mode: DiscoverModeDto,
}

impl Default for ConnectAppsDto {
    fn default() -> Self {
        Self {
            subsonic_enabled: false,
            jellyfin_enabled: false,
            exact_track_approval_supported: true,
            transcoding_enabled: true,
            transcode_default_format: AudioFormatDto::Mp3,
            transcode_max_bitrate_kbps: 320,
            advertise_server_name: "DroppedNeedle".to_owned(),
            advertise_server_version: "10.10.6".to_owned(),
            discover_mode: DiscoverModeDto::LocalOnly,
        }
    }
}

// --- lastfm_settings (R7: switch only) ---------------------------------------

/// Last.fm master switch. Credentials are per-user (see
/// `/me/connections/lastfm`); the admin-global pair is deleted.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LastFmSettingsDto {
    /// Master switch for Last.fm fan-out.
    pub enabled: bool,
}

// --- download_client (slskd) -------------------------------------------------

/// slskd download-client connection. `api_key` is masked on read and
/// preserved on save when the masked sentinel comes back unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SlskdConnectionDto {
    /// Master switch.
    pub enabled: bool,
    /// Client type tag.
    pub client_type: String,
    /// slskd base URL.
    pub url: String,
    /// API key (masked unless unset).
    pub api_key: String,
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

impl Default for SlskdConnectionDto {
    fn default() -> Self {
        Self {
            enabled: false,
            client_type: "slskd".to_owned(),
            url: String::new(),
            api_key: String::new(),
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

// --- download_clients (sabnzbd sub-object) -----------------------------------

/// SABnzbd connection. `api_key` is the FULL key (the add-only nzbkey
/// cannot do queue/history/delete); masked on read, preserved on a masked
/// save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SabnzbdConnectionDto {
    /// Master switch.
    pub enabled: bool,
    /// Client type tag.
    pub client_type: String,
    /// SABnzbd base URL.
    pub url: String,
    /// Full API key (masked unless unset).
    pub api_key: String,
    /// Category (`*`: a fresh SABnzbd has no `droppedneedle` category).
    pub category: String,
    /// Job priority.
    pub priority: i64,
    /// Post-processing level.
    pub post_processing: i64,
    /// Where DroppedNeedle sees SABnzbd's completed dir.
    pub downloads_mount: String,
}

impl Default for SabnzbdConnectionDto {
    fn default() -> Self {
        Self {
            enabled: false,
            client_type: "sabnzbd".to_owned(),
            url: String::new(),
            api_key: String::new(),
            category: "*".to_owned(),
            priority: 0,
            post_processing: 3,
            downloads_mount: "/sabnzbd-downloads".to_owned(),
        }
    }
}

// --- prowlarr ----------------------------------------------------------------

/// Single Prowlarr connection (Prowlarr multiplexes its own indexers).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ProwlarrConnectionDto {
    /// Master switch.
    pub enabled: bool,
    /// Prowlarr base URL (LAN service, `http://` default).
    pub url: String,
    /// API key (masked unless unset).
    pub api_key: String,
}

// --- jellyfin_settings -------------------------------------------------------

/// Jellyfin connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct JellyfinConnectionDto {
    /// Jellyfin base URL.
    pub jellyfin_url: String,
    /// API key (masked unless unset).
    pub api_key: String,
    /// Jellyfin user id.
    pub user_id: String,
    /// Master switch.
    pub enabled: bool,
    /// Jellyfin login switch.
    pub login_enabled: bool,
}

impl Default for JellyfinConnectionDto {
    fn default() -> Self {
        Self {
            jellyfin_url: "http://jellyfin:8096".to_owned(),
            api_key: String::new(),
            user_id: String::new(),
            enabled: false,
            login_enabled: false,
        }
    }
}

/// One Jellyfin user, for the admin user picker after a verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JellyfinUserInfo {
    /// User id.
    pub id: String,
    /// Display name.
    pub name: String,
}

/// Jellyfin verify verdict, with the user list on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JellyfinVerifyResponse {
    /// Whether the connection checked out.
    pub success: bool,
    /// Human summary.
    pub message: String,
    /// Server users (empty unless the probe succeeded).
    pub users: Vec<JellyfinUserInfo>,
}

// --- navidrome_settings ------------------------------------------------------

/// Navidrome connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NavidromeConnectionDto {
    /// Navidrome base URL.
    pub navidrome_url: String,
    /// Username.
    pub username: String,
    /// Password (masked unless unset).
    pub password: String,
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

impl Default for NavidromeConnectionDto {
    fn default() -> Self {
        Self {
            navidrome_url: String::new(),
            username: String::new(),
            password: String::new(),
            enabled: false,
            playlist_sync_enabled: false,
            playlist_sync_path: String::new(),
            playlist_sync_scope: "public".to_owned(),
            playlist_sync_remove_deleted: true,
        }
    }
}

// --- plex_settings -----------------------------------------------------------

/// Plex connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PlexConnectionDto {
    /// Plex base URL.
    pub plex_url: String,
    /// Plex token (masked unless unset).
    pub plex_token: String,
    /// Master switch.
    pub enabled: bool,
    /// Plex login switch.
    pub login_enabled: bool,
    /// Music library ids.
    pub music_library_ids: Vec<String>,
    /// Scrobble back to Plex.
    pub scrobble_to_plex: bool,
}

impl Default for PlexConnectionDto {
    fn default() -> Self {
        Self {
            plex_url: String::new(),
            plex_token: String::new(),
            enabled: false,
            login_enabled: false,
            music_library_ids: Vec::new(),
            scrobble_to_plex: true,
        }
    }
}

/// One Plex music-library section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlexLibrarySectionInfo {
    /// Section key.
    pub key: String,
    /// Section title.
    pub title: String,
}

/// Plex verify verdict, with music libraries on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlexVerifyResponse {
    /// Whether the connection checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
    /// Music libraries (empty unless the probe succeeded).
    pub libraries: Vec<PlexLibrarySectionInfo>,
}

// --- listenbrainz_settings ---------------------------------------------------

/// ListenBrainz connection.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ListenBrainzConnectionDto {
    /// Username.
    pub username: String,
    /// User token (masked unless unset).
    pub user_token: String,
    /// Master switch.
    pub enabled: bool,
}

// --- youtube_settings --------------------------------------------------------

/// YouTube connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct YouTubeConnectionDto {
    /// API key (masked unless unset).
    pub api_key: String,
    /// Master switch.
    pub enabled: bool,
    /// API search switch.
    pub api_enabled: bool,
    /// Daily quota limit (1-10000).
    pub daily_quota_limit: i64,
}

impl Default for YouTubeConnectionDto {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            enabled: false,
            api_enabled: false,
            daily_quota_limit: 80,
        }
    }
}

// --- events ------------------------------------------------------------------

/// Upcoming-events sources. The sweep runs daily at `poll_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct EventsSettingsDto {
    /// Master switch.
    pub enabled: bool,
    /// Ticketmaster switch.
    pub ticketmaster_enabled: bool,
    /// Ticketmaster key (masked unless unset).
    pub ticketmaster_api_key: String,
    /// Skiddle switch.
    pub skiddle_enabled: bool,
    /// Skiddle key (masked unless unset).
    pub skiddle_api_key: String,
    /// Daily sweep time, server-local `HH:MM`.
    pub poll_time: String,
    /// Sweep scope.
    pub sweep_scope: EventsSweepScopeDto,
}

impl Default for EventsSettingsDto {
    fn default() -> Self {
        Self {
            enabled: false,
            ticketmaster_enabled: false,
            ticketmaster_api_key: String::new(),
            skiddle_enabled: false,
            skiddle_api_key: String::new(),
            poll_time: "06:00".to_owned(),
            sweep_scope: EventsSweepScopeDto::Followed,
        }
    }
}

// --- wrapped_settings --------------------------------------------------------

/// Shared secret for the wrapped endpoints (service-to-service).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct WrappedSettingsDto {
    /// API key (masked unless unset).
    pub api_key: String,
}

// --- oidc_settings -----------------------------------------------------------

/// OIDC connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OidcConnectionDto {
    /// Master switch.
    pub enabled: bool,
    /// Issuer URL.
    pub issuer: String,
    /// Client id.
    pub client_id: String,
    /// Client secret (masked unless unset).
    pub client_secret: String,
    /// Scopes.
    pub scopes: String,
    /// Redirect URI.
    pub redirect_uri: String,
}

impl Default for OidcConnectionDto {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scopes: "openid email profile".to_owned(),
            redirect_uri: String::new(),
        }
    }
}

// --- shared verify shapes ----------------------------------------------------

/// Generic connection-test verdict. Reachable/bad-credential distinctions
/// ride in the body, never as a leaked 5xx, and the URL or host is never
/// echoed back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VerifyConnectionResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
}

/// slskd test verdict: validity plus the reported version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestConnectionResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
}

/// Prowlarr test verdict: validity plus version and member-indexer count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProwlarrTestResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Enabled member indexers, when listed.
    pub indexer_count: Option<i64>,
}

/// SABnzbd test verdict: version plus the category list (for the picker),
/// the SABnzbd-side completed dir (the mount hint), and the mount
/// diagnosis over the SUBMITTED mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SabnzbdTestResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Known categories.
    pub categories: Vec<String>,
    /// SABnzbd-side completed dir, when known.
    pub complete_dir: Option<String>,
    /// Whether the submitted mount holds files, when diagnosed.
    pub mount_has_files: Option<bool>,
    /// Sampled downloads resolving under the submitted mount.
    pub resolvable_downloads: Option<i64>,
    /// Sampled downloads.
    pub sampled_downloads: Option<i64>,
    /// Actionable mount guidance, when the mount looks wrong.
    pub mount_message: Option<String>,
}

// --- download_policy ---------------------------------------------------------

/// One ordered, closed format-quality recipe entry. Unknown keys are
/// rejected at decode so a future setting cannot silently become a
/// different recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QualityRecipeEntryDto {
    /// Container: `flac` or `mp3`.
    pub format: String,
    /// Quality id (per-format closed set, or `custom`).
    pub quality: String,
    /// Custom MP3 lower bound, or canonical standard bound.
    pub min_bitrate_kbps: Option<i64>,
    /// Custom MP3 target, or canonical standard bound.
    pub target_bitrate_kbps: Option<i64>,
    /// Custom MP3 upper bound (`None` when open-ended), or canonical.
    pub max_bitrate_kbps: Option<i64>,
    /// Custom FLAC bit depth.
    pub bit_depth: Option<i64>,
    /// Custom FLAC sample rate.
    pub sample_rate_hz: Option<i64>,
}

/// Source-agnostic acquisition policy. Lives in its own section so a
/// Usenet-only install still has quality/threshold/timeout/retry settings
/// to read. Validation is strict on submitted bodies (never silently
/// clamped); the read-only recipe-status projection is recomputed at GET
/// time instead of persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DownloadPolicyDto {
    /// Worst accepted tier label.
    pub quality_min: String,
    /// Best accepted tier label.
    pub quality_max: String,
    /// Reject non-FLAC/MP3 candidates. A v2 recipe requires this on.
    pub flac_mp3_only: bool,
    /// Verify downloads after landing.
    pub verify_downloads: bool,
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
    /// Minimum Usenet release age.
    pub usenet_min_release_age_minutes: i64,
    /// Max download size in MB (0 unlimited).
    pub max_size_mb: i64,
    /// Usenet retention in days (0 unknown).
    pub usenet_retention_days: i64,
    /// Terms that reject a candidate.
    pub ignored_terms: Vec<String>,
    /// Terms a candidate must carry.
    pub required_terms: Vec<String>,
    /// Stop-upgrading tier label.
    pub quality_cutoff: String,
    /// Background upgrades switch.
    pub upgrade_allowed: bool,
    /// Recycle-bin path ("" disables).
    pub recycle_bin_path: String,
    /// Recycle retention in days.
    pub recycle_retention_days: i64,
    /// Library size cap in GB (0 unlimited).
    pub max_library_size_gb: i64,
    /// Default per-user request quota (0 unlimited).
    pub default_request_quota_count: i64,
    /// Request quota window in days.
    pub default_request_quota_days: i64,
    /// Default per-user storage quota in GB (0 unlimited).
    pub default_storage_quota_gb: i64,
    /// Background upgrade sweep switch.
    pub background_upgrade_scan_enabled: bool,
    /// Sweep interval in hours.
    pub background_upgrade_scan_interval_hours: i64,
    /// Sweep upgrades per run.
    pub background_upgrade_max_per_run: i64,
    /// Closed v2 quality recipe (empty means a v1 policy).
    pub quality_recipe: Vec<QualityRecipeEntryDto>,
    /// Tier preference order, most preferred first.
    pub quality_preference_order: Vec<String>,
    /// Preferred lossy bitrate target.
    pub preferred_lossy_bitrate_kbps: Option<i64>,
    /// Lossy lower bound.
    pub lossy_min_bitrate_kbps: Option<i64>,
    /// Lossy upper bound.
    pub lossy_max_bitrate_kbps: Option<i64>,
    /// Lossless edition preference.
    pub lossless_preference: String,
    /// Lossless bit-depth ceiling.
    pub lossless_max_bit_depth: Option<i64>,
    /// Lossless sample-rate ceiling.
    pub lossless_max_sample_rate_hz: Option<i64>,
    /// Unknown-quality handling.
    pub unknown_quality_behavior: String,
    /// Source-vs-quality selection.
    pub source_selection_mode: String,
    /// Read-only recipe verdict (`v1`, `v2`, `non_convertible`,
    /// `invalid`). Recomputed on every read; client values are ignored
    /// on save.
    pub quality_recipe_status: String,
    /// Recipe verdict detail, when any. Read-only, like the status.
    pub quality_recipe_error: Option<String>,
}

impl Default for DownloadPolicyDto {
    fn default() -> Self {
        Self {
            quality_min: "mp3_320".to_owned(),
            quality_max: "lossless".to_owned(),
            flac_mp3_only: true,
            verify_downloads: true,
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
            usenet_min_release_age_minutes: 30,
            max_size_mb: 0,
            usenet_retention_days: 0,
            ignored_terms: Vec::new(),
            required_terms: Vec::new(),
            quality_cutoff: "lossless".to_owned(),
            upgrade_allowed: false,
            recycle_bin_path: String::new(),
            recycle_retention_days: 30,
            max_library_size_gb: 0,
            default_request_quota_count: 0,
            default_request_quota_days: 7,
            default_storage_quota_gb: 0,
            background_upgrade_scan_enabled: false,
            background_upgrade_scan_interval_hours: 12,
            background_upgrade_max_per_run: 3,
            quality_recipe: Vec::new(),
            quality_preference_order: Vec::new(),
            preferred_lossy_bitrate_kbps: None,
            lossy_min_bitrate_kbps: None,
            lossy_max_bitrate_kbps: None,
            lossless_preference: "highest".to_owned(),
            lossless_max_bit_depth: None,
            lossless_max_sample_rate_hz: None,
            unknown_quality_behavior: "allow_as_fallback".to_owned(),
            source_selection_mode: "source_first".to_owned(),
            quality_recipe_status: "v1".to_owned(),
            quality_recipe_error: None,
        }
    }
}

/// Safe, signed-in-user projection of the acquisition policy: the quality
/// summary sentence plus the source-mode label only, no admin internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicySummaryResponse {
    /// Backend-composed contract sentence.
    pub summary: String,
    /// Source-mode label.
    pub source_mode: String,
    /// Whether a down-level image reproduces acquisition behavior.
    pub legacy_rollback_compatible: bool,
    /// Read-only recipe verdict: `v1`, `v2`, `non_convertible`, `invalid`.
    pub quality_recipe_status: String,
    /// Recipe verdict detail, when any.
    pub quality_recipe_error: Option<String>,
}

/// Admin preview of an UNSAVED policy against persisted state:
/// persisted-state bucket counts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicyImpactResponse {
    /// Manual search jobs that would re-resolve.
    pub manual_search_jobs: i64,
    /// Queued rows with no attempts yet.
    pub queued_without_attempts: i64,
    /// Rows awaiting review.
    pub awaiting_review: i64,
    /// Remote-queued zero-byte rows.
    pub remote_queued_zero_byte: i64,
    /// Transferring rows (immutable under the new policy).
    pub transferring_immutable: i64,
    /// Held reviews.
    pub held_reviews: i64,
    /// Whether a down-level image would preserve acquisition behavior.
    pub legacy_representable: bool,
}

// --- indexers ----------------------------------------------------------------

/// One configured Newznab indexer. DroppedNeedle ships none; the user adds
/// their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NewznabIndexerDto {
    /// Stable id (blank on create; the path wins on update).
    pub id: String,
    /// Indexer type tag.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub indexer_type: String,
    /// Display name.
    pub name: String,
    /// Indexer base URL.
    pub url: String,
    /// API key (masked unless unset; per element).
    pub api_key: String,
    /// Newznab categories.
    pub categories: Vec<i64>,
    /// Master switch.
    pub enabled: bool,
    /// Priority (lower first).
    pub priority: i64,
}

impl Default for NewznabIndexerDto {
    fn default() -> Self {
        Self {
            id: String::new(),
            indexer_type: "newznab".to_owned(),
            name: String::new(),
            url: String::new(),
            api_key: String::new(),
            categories: vec![3000, 3010, 3040],
            enabled: true,
            priority: 1,
        }
    }
}

/// Indexer save acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexerSavedResponse {
    /// Saved indexer id.
    pub id: String,
}

/// Dragged-card priority order (1-based on save).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexerReorderRequest {
    /// Indexer ids in the new order.
    pub ordered_ids: Vec<String>,
}

/// Indexer caps-test verdict. `supports_audio_search` tells the user
/// whether structured music search will be used or the `t=search`
/// fallback; `suggested_url` is a one-click fix when the URL looks like
/// the site homepage but `/api` answers as a real endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexerTestResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Whether structured music search is advertised.
    pub supports_audio_search: bool,
    /// Advertised category count.
    pub category_count: i64,
    /// One-click `/api` fix, when the submitted URL was the homepage.
    pub suggested_url: Option<String>,
}

/// Bare success acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OperationResult {
    /// Whether the operation succeeded.
    pub success: bool,
}

// --- musicbrainz_settings ----------------------------------------------------

/// Active BrainzMash binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BrainzmashActiveBindingDto {
    /// Pinned endpoint.
    pub endpoint: String,
    /// Access revision.
    pub access_revision: String,
    /// Source identity.
    pub source_id: String,
    /// Source generation.
    pub generation: i64,
    /// Disclosure version.
    pub disclosure_version: String,
    /// Consent recorded.
    pub consented: bool,
    /// Endpoint verified.
    pub verified: bool,
}

impl Default for BrainzmashActiveBindingDto {
    fn default() -> Self {
        Self {
            endpoint: "https://api.brainzmash.cc/ws/2".to_owned(),
            access_revision: String::new(),
            source_id: String::new(),
            generation: 0,
            disclosure_version: "brainzmash-v1".to_owned(),
            consented: false,
            verified: false,
        }
    }
}

/// Transient pending BrainzMash proposal (process memory, echoed so the
/// UI can drive consent/verify/activate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BrainzmashPendingProposalDto {
    /// Pinned endpoint.
    pub endpoint: String,
    /// Proposed access revision.
    pub access_revision: String,
    /// Proposed source identity.
    pub source_id: String,
    /// Proposed source generation.
    pub generation: i64,
    /// Proposed disclosure version.
    pub disclosure_version: String,
    /// Consent recorded.
    pub consented: bool,
    /// Endpoint verified.
    pub verified: bool,
}

impl Default for BrainzmashPendingProposalDto {
    fn default() -> Self {
        Self {
            endpoint: "https://api.brainzmash.cc/ws/2".to_owned(),
            access_revision: String::new(),
            source_id: String::new(),
            generation: 1,
            disclosure_version: "brainzmash-v1".to_owned(),
            consented: false,
            verified: false,
        }
    }
}

/// MusicBrainz connection settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MusicBrainzSettingsDto {
    /// Active source tier.
    pub source_mode: MbSourceModeDto,
    /// API base (canonicalized for official/brainzmash).
    pub api_url: String,
    /// Requests per second (0 = Unlimited, off-official only).
    pub rate_limit: f64,
    /// Concurrent searches.
    pub concurrent_searches: i64,
    /// Community-tier disclosure acknowledged.
    pub community_acknowledged: bool,
    /// Last deliberately chosen tier.
    pub selected_source_mode: MbSourceModeDto,
    /// Source identity.
    pub source_id: String,
    /// Source generation.
    pub generation: i64,
    /// Active BrainzMash binding, if any.
    pub active_brainzmash: Option<BrainzmashActiveBindingDto>,
    /// True when the official-host clamp forced values down (or lifted a
    /// 0 sentinel up). Rendered, never refused.
    pub clamped_to_official_limits: bool,
    /// Transient pending proposal, when one is staged.
    pub pending_brainzmash: Option<BrainzmashPendingProposalDto>,
}

impl Default for MusicBrainzSettingsDto {
    fn default() -> Self {
        Self {
            source_mode: MbSourceModeDto::Brainzmash,
            api_url: "https://api.brainzmash.cc/ws/2".to_owned(),
            rate_limit: 10.0,
            concurrent_searches: 1,
            community_acknowledged: false,
            selected_source_mode: MbSourceModeDto::Brainzmash,
            source_id: String::new(),
            generation: 1,
            active_brainzmash: None,
            clamped_to_official_limits: false,
            pending_brainzmash: None,
        }
    }
}

/// Client-submitted MusicBrainz source change. BrainzMash is never a
/// direct update: it moves through stage/consent/verify/activate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MusicBrainzSettingsUpdate {
    /// Desired source tier.
    pub source_mode: MbSourceModeDto,
    /// API base for mirror/community tiers.
    pub api_url: Option<String>,
    /// Requests per second.
    pub rate_limit: f64,
    /// Concurrent searches.
    pub concurrent_searches: i64,
    /// Community-tier disclosure acknowledged.
    pub community_acknowledged: Option<bool>,
}

impl Default for MusicBrainzSettingsUpdate {
    fn default() -> Self {
        Self {
            source_mode: MbSourceModeDto::Official,
            api_url: None,
            rate_limit: 1.0,
            concurrent_searches: 6,
            community_acknowledged: Some(false),
        }
    }
}

/// Consent-bound BrainzMash binding request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzBindingRequest {
    /// Proposed access revision.
    pub access_revision: String,
    /// Proposed source identity.
    pub source_id: String,
    /// Proposed source generation.
    pub generation: i64,
    /// Proposed disclosure version.
    pub disclosure_version: String,
}

/// Verify payload: a BrainzMash consent binding or a plain source
/// update. The binding is tried first: it pins the exact staged
/// proposal, while an update only names a tier to probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum MusicBrainzVerifyRequest {
    /// Consent-bound BrainzMash verification.
    Binding(MusicBrainzBindingRequest),
    /// Plain tier probe (never BrainzMash).
    Update(MusicBrainzSettingsUpdate),
}

// --- library_settings (typed roots + policies) ----------------------------------

/// One path-policy rule inside a root (ordered by depth on save).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryPathRuleDto {
    /// Rule id.
    pub id: String,
    /// Path relative to the root.
    pub relative_path: String,
    /// Policy for this path.
    pub policy: IdentificationPolicyDto,
}

impl Default for LibraryPathRuleDto {
    fn default() -> Self {
        Self {
            id: String::new(),
            relative_path: String::new(),
            policy: IdentificationPolicyDto::Automatic,
        }
    }
}

/// One library root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryRootDto {
    /// Stable root id.
    pub id: String,
    /// Absolute path.
    pub path: String,
    /// Display label.
    pub label: String,
    /// Default policy.
    pub policy: IdentificationPolicyDto,
    /// Path rules.
    pub rules: Vec<LibraryPathRuleDto>,
}

impl Default for LibraryRootDto {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: String::new(),
            label: String::new(),
            policy: IdentificationPolicyDto::Automatic,
            rules: Vec::new(),
        }
    }
}

/// Typed library settings: roots, policies, staging, naming, AcoustID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibrarySettingsDto {
    /// Library roots.
    pub library_roots: Vec<LibraryRootDto>,
    /// Staging path.
    pub staging_path: String,
    /// Naming template.
    pub naming_template: String,
    /// AcoustID key (masked unless unset).
    pub acoustid_api_key: String,
    /// Master switch (excluded from the revision hash).
    pub enabled: bool,
}

impl Default for LibrarySettingsDto {
    fn default() -> Self {
        Self {
            library_roots: Vec::new(),
            staging_path: String::new(),
            naming_template: "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"
                .to_owned(),
            acoustid_api_key: String::new(),
            enabled: true,
        }
    }
}

/// GET view: settings plus the policy revision, the reconciliation
/// projection, and non-blocking warnings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibrarySettingsResponse {
    /// Library roots.
    pub library_roots: Vec<LibraryRootDto>,
    /// Staging path.
    pub staging_path: String,
    /// Naming template.
    pub naming_template: String,
    /// AcoustID key (masked unless unset).
    pub acoustid_api_key: String,
    /// Master switch.
    pub enabled: bool,
    /// Content revision (write CAS token).
    pub policy_revision: String,
    /// Whether reconciliation is still required.
    pub reconciliation_required: bool,
    /// `applied` or `awaiting_reconciliation`.
    pub reconciliation_state: String,
    /// Pending revision, when awaiting reconciliation.
    pub pending_policy_revision: Option<String>,
    /// Affected scope ids.
    pub affected_scope_ids: Vec<String>,
    /// Actions the save applied.
    pub actions_applied: Vec<String>,
    /// Non-blocking warnings.
    pub warnings: Vec<String>,
}

impl Default for LibrarySettingsResponse {
    fn default() -> Self {
        Self {
            library_roots: Vec::new(),
            staging_path: String::new(),
            naming_template: "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"
                .to_owned(),
            acoustid_api_key: String::new(),
            enabled: true,
            policy_revision: String::new(),
            reconciliation_required: false,
            reconciliation_state: "applied".to_owned(),
            pending_policy_revision: None,
            affected_scope_ids: Vec::new(),
            actions_applied: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// PUT body: full settings plus the required CAS token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibrarySettingsSaveRequest {
    /// Full candidate settings.
    pub settings: LibrarySettingsDto,
    /// Compare-and-swap token from the last GET.
    pub expected_policy_revision: String,
}

/// One policy-tree node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyTreeNode {
    /// Node id (root or rule id).
    pub id: String,
    /// `root` or `rule`.
    pub kind: String,
    /// Display label.
    pub label: String,
    /// Absolute path.
    pub path: String,
    /// Effective policy.
    pub policy: IdentificationPolicyDto,
    /// Id the policy inherits from, when any.
    pub inherited_from_id: Option<String>,
    /// Whether the path is currently available.
    pub available: bool,
    /// Indexed file count, when the catalog port is wired.
    pub indexed_file_count: Option<i64>,
    /// On-disk file count, when the catalog port is wired.
    pub on_disk_file_count: Option<i64>,
    /// Child rule nodes.
    pub children: Vec<LibraryPolicyTreeNode>,
}

/// Policy-tree response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyTreeResponse {
    /// Content revision the tree was built from.
    pub policy_revision: String,
    /// Root nodes.
    pub roots: Vec<LibraryPolicyTreeNode>,
    /// Non-blocking warnings.
    pub warnings: Vec<String>,
}

/// Impact preview request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyImpactRequest {
    /// Full candidate settings.
    pub settings: LibrarySettingsDto,
    /// Compare-and-swap token from the last GET, if any.
    pub expected_policy_revision: Option<String>,
}

/// Impact preview: revision change plus affected scopes and counts.
/// Without the pending-policy machinery (library-engine follow-up),
/// reconciliation fields project the applied state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyImpactResponse {
    /// Current stored revision.
    pub current_policy_revision: String,
    /// Normalized candidate revision.
    pub proposed_policy_revision: String,
    /// The caller's revision was already stale.
    pub stale: bool,
    /// Whether the change needs reconciliation.
    pub reconciliation_required: bool,
    /// Affected scope ids.
    pub affected_scope_ids: Vec<String>,
    /// Indexed file count under the affected scopes, when wired.
    pub indexed_file_count: Option<i64>,
    /// On-disk file count under the affected scopes, when wired.
    pub on_disk_file_count: Option<i64>,
    /// Whether catalog content becomes unavailable.
    pub content_will_become_unavailable: bool,
    /// Whether queued work is cancelled.
    pub queued_work_will_be_cancelled: bool,
    /// Non-blocking warnings.
    pub warnings: Vec<String>,
}

// --- advanced_settings (closed allowlist, frontend units) ----------------------
///
/// The wire shape is v2's `AdvancedSettingsFrontend` minus the dropped
/// internal-tuning fields: values are human units (hours/minutes/seconds)
/// scaled to backend units (seconds/milliseconds) on save, floored back on
/// read. This shape is a CLOSED allowlist: unknown JSON fields are
/// rejected at decode (400), so a client holding a dropped tuning field
/// learns it is gone instead of believing it saved.
/// Advanced tunables in frontend units. See the section comment for the
/// unit contract and the allowlist rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct AdvancedSettingsDto {
    /// Album cache TTL for library albums, hours (1-168).
    pub cache_ttl_album_library: i64,
    /// Album cache TTL for non-library albums, hours (1-24).
    pub cache_ttl_album_non_library: i64,
    /// Artist cache TTL for library artists, hours (1-168).
    pub cache_ttl_artist_library: i64,
    /// Artist cache TTL for non-library artists, hours (1-168).
    pub cache_ttl_artist_non_library: i64,
    /// Discovery cache TTL for library artists, hours (1-168).
    pub cache_ttl_artist_discovery_library: i64,
    /// Discovery cache TTL for non-library artists, hours (1-168).
    pub cache_ttl_artist_discovery_non_library: i64,
    /// Search cache TTL, minutes (1-1440).
    pub cache_ttl_search: i64,
    /// Jellyfin recently-played TTL, minutes (1-60).
    pub cache_ttl_jellyfin_recently_played: i64,
    /// Jellyfin favorites TTL, minutes (1-60).
    pub cache_ttl_jellyfin_favorites: i64,
    /// Jellyfin genres TTL, minutes (1-1440).
    pub cache_ttl_jellyfin_genres: i64,
    /// Jellyfin library-stats TTL, minutes (1-60).
    pub cache_ttl_jellyfin_library_stats: i64,
    /// Navidrome albums TTL, minutes (1-60).
    pub cache_ttl_navidrome_albums: i64,
    /// Navidrome artists TTL, minutes (1-60).
    pub cache_ttl_navidrome_artists: i64,
    /// Navidrome recent TTL, minutes (1-60).
    pub cache_ttl_navidrome_recent: i64,
    /// Navidrome favorites TTL, minutes (1-60).
    pub cache_ttl_navidrome_favorites: i64,
    /// Navidrome search TTL, minutes (1-60).
    pub cache_ttl_navidrome_search: i64,
    /// Navidrome genres TTL, minutes (1-1440).
    pub cache_ttl_navidrome_genres: i64,
    /// Navidrome stats TTL, minutes (1-60).
    pub cache_ttl_navidrome_stats: i64,
    /// Plex albums TTL, minutes (1-60).
    pub cache_ttl_plex_albums: i64,
    /// Plex search TTL, minutes (1-60).
    pub cache_ttl_plex_search: i64,
    /// Plex genres TTL, minutes (1-1440).
    pub cache_ttl_plex_genres: i64,
    /// Plex stats TTL, minutes (1-60).
    pub cache_ttl_plex_stats: i64,
    /// Outbound HTTP timeout, seconds (5-60).
    pub http_timeout: i64,
    /// Outbound connect timeout, seconds (1-30).
    pub http_connect_timeout: i64,
    /// Outbound pool size (50-500).
    pub http_max_connections: i64,
    /// Artist-image batch size (1-20).
    pub batch_artist_images: i64,
    /// Album batch size (1-20).
    pub batch_albums: i64,
    /// Artist delay, seconds (0-5).
    pub delay_artist: f64,
    /// Album delay, seconds (0-5).
    pub delay_albums: f64,
    /// Memory-cache entries (1000-100000).
    pub memory_cache_max_entries: i64,
    /// Memory-cache cleanup cadence, seconds (60-3600).
    pub memory_cache_cleanup_interval: i64,
    /// Cover memory-cache entries (16-2048).
    pub cover_memory_cache_max_entries: i64,
    /// Cover memory-cache MB (1-1024).
    pub cover_memory_cache_max_size_mb: i64,
    /// Disk-cache cleanup cadence, minutes (1-60).
    pub disk_cache_cleanup_interval: i64,
    /// Recent-metadata cap, MB (100-5000).
    pub recent_metadata_max_size_mb: i64,
    /// Recent-covers cap, MB (100-10000).
    pub recent_covers_max_size_mb: i64,
    /// Persistent-metadata TTL, hours (1-168).
    pub persistent_metadata_ttl_hours: i64,
    /// Discover queue size (1-20).
    pub discover_queue_size: i64,
    /// Discover queue TTL, hours (1-168).
    pub discover_queue_ttl: i64,
    /// Discover queue auto-generate.
    pub discover_queue_auto_generate: bool,
    /// Discover queue polling, seconds (1-30).
    pub discover_queue_polling_interval: i64,
    /// Discover seed artists (1-10).
    pub discover_queue_seed_artists: i64,
    /// Discover wildcard slots (0-10).
    pub discover_queue_wildcard_slots: i64,
    /// Discover genre-affinity weight (0-1).
    pub discover_picks_genre_affinity_weight: f64,
    /// Discover picks count (4-30).
    pub discover_picks_count: i64,
    /// Home frontend TTL, minutes (1-60).
    pub frontend_ttl_home: i64,
    /// Discover frontend TTL, minutes (1-1440).
    pub frontend_ttl_discover: i64,
    /// Library frontend TTL, minutes (1-60).
    pub frontend_ttl_library: i64,
    /// Recently-added frontend TTL, minutes (1-60).
    pub frontend_ttl_recently_added: i64,
    /// Discover-queue frontend TTL, minutes (60-10080).
    pub frontend_ttl_discover_queue: i64,
    /// Search frontend TTL, minutes (1-60).
    pub frontend_ttl_search: i64,
    /// Local-files sidebar TTL, minutes (1-60).
    pub frontend_ttl_local_files_sidebar: i64,
    /// Jellyfin sidebar TTL, minutes (1-60).
    pub frontend_ttl_jellyfin_sidebar: i64,
    /// Plex sidebar TTL, minutes (1-60).
    pub frontend_ttl_plex_sidebar: i64,
    /// Playlist-sources frontend TTL, minutes (1-60).
    pub frontend_ttl_playlist_sources: i64,
    /// AudioDB provider switch.
    pub audiodb_enabled: bool,
    /// AudioDB name-search fallback.
    pub audiodb_name_search_fallback: bool,
    /// Serve remote images directly.
    pub direct_remote_images_enabled: bool,
    /// Prefer local cover art.
    pub prefer_local_cover_art: bool,
    /// AudioDB API key (masked unless unset).
    pub audiodb_api_key: String,
    /// AudioDB found TTL, hours (1-720).
    pub cache_ttl_audiodb_found: i64,
    /// AudioDB not-found TTL, hours (1-168).
    pub cache_ttl_audiodb_not_found: i64,
    /// AudioDB library TTL, hours (24-720).
    pub cache_ttl_audiodb_library: i64,
    /// Genre section TTL, hours (1-168).
    pub genre_section_ttl: i64,
    /// Request history retention, days (30-3650).
    pub request_history_retention_days: i64,
    /// Ignored-releases retention, days (30-3650).
    pub ignored_releases_retention_days: i64,
    /// Orphan-cover demote cadence, hours (1-168).
    pub orphan_cover_demote_interval_hours: i64,
    /// Store prune cadence, hours (1-168).
    pub store_prune_interval_hours: i64,
    /// Sync stall timeout, minutes (2-30).
    pub sync_stall_timeout_minutes: i64,
    /// Sync max timeout, hours (1-48).
    pub sync_max_timeout_hours: i64,
    /// Request concurrency (1-5).
    pub request_concurrency: i64,
}

impl Default for AdvancedSettingsDto {
    fn default() -> Self {
        Self {
            cache_ttl_album_library: 24,
            cache_ttl_album_non_library: 6,
            cache_ttl_artist_library: 6,
            cache_ttl_artist_non_library: 6,
            cache_ttl_artist_discovery_library: 6,
            cache_ttl_artist_discovery_non_library: 1,
            cache_ttl_search: 60,
            cache_ttl_jellyfin_recently_played: 5,
            cache_ttl_jellyfin_favorites: 5,
            cache_ttl_jellyfin_genres: 60,
            cache_ttl_jellyfin_library_stats: 10,
            cache_ttl_navidrome_albums: 5,
            cache_ttl_navidrome_artists: 5,
            cache_ttl_navidrome_recent: 2,
            cache_ttl_navidrome_favorites: 2,
            cache_ttl_navidrome_search: 2,
            cache_ttl_navidrome_genres: 60,
            cache_ttl_navidrome_stats: 10,
            cache_ttl_plex_albums: 5,
            cache_ttl_plex_search: 2,
            cache_ttl_plex_genres: 60,
            cache_ttl_plex_stats: 10,
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
            disk_cache_cleanup_interval: 10,
            recent_metadata_max_size_mb: 500,
            recent_covers_max_size_mb: 1024,
            persistent_metadata_ttl_hours: 24,
            discover_queue_size: 10,
            discover_queue_ttl: 24,
            discover_queue_auto_generate: true,
            discover_queue_polling_interval: 4,
            discover_queue_seed_artists: 3,
            discover_queue_wildcard_slots: 2,
            discover_picks_genre_affinity_weight: 0.7,
            discover_picks_count: 12,
            frontend_ttl_home: 5,
            frontend_ttl_discover: 30,
            frontend_ttl_library: 5,
            frontend_ttl_recently_added: 5,
            frontend_ttl_discover_queue: 1440,
            frontend_ttl_search: 5,
            frontend_ttl_local_files_sidebar: 2,
            frontend_ttl_jellyfin_sidebar: 2,
            frontend_ttl_plex_sidebar: 2,
            frontend_ttl_playlist_sources: 15,
            audiodb_enabled: true,
            audiodb_name_search_fallback: false,
            direct_remote_images_enabled: true,
            prefer_local_cover_art: true,
            audiodb_api_key: String::new(),
            cache_ttl_audiodb_found: 168,
            cache_ttl_audiodb_not_found: 24,
            cache_ttl_audiodb_library: 336,
            genre_section_ttl: 6,
            request_history_retention_days: 180,
            ignored_releases_retention_days: 365,
            orphan_cover_demote_interval_hours: 24,
            store_prune_interval_hours: 6,
            sync_stall_timeout_minutes: 10,
            sync_max_timeout_hours: 8,
            request_concurrency: 2,
        }
    }
}

/// Frontend cache TTLs in BACKEND units (milliseconds), verbatim from
/// the stored advanced section. The SPA reads this one endpoint instead
/// of the whole advanced surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FrontendCacheTTLs {
    /// Home TTL, ms.
    pub home: i64,
    /// Discover TTL, ms.
    pub discover: i64,
    /// Library TTL, ms.
    pub library: i64,
    /// Recently-added TTL, ms.
    pub recently_added: i64,
    /// Discover-queue TTL, ms.
    pub discover_queue: i64,
    /// Search TTL, ms.
    pub search: i64,
    /// Local-files sidebar TTL, ms.
    pub local_files_sidebar: i64,
    /// Jellyfin sidebar TTL, ms.
    pub jellyfin_sidebar: i64,
    /// Plex sidebar TTL, ms.
    pub plex_sidebar: i64,
    /// Playlist-sources TTL, ms.
    pub playlist_sources: i64,
    /// Discover-queue polling interval, ms.
    pub discover_queue_polling_interval: i64,
    /// Discover-queue auto-generate switch.
    pub discover_queue_auto_generate: bool,
}

impl Default for FrontendCacheTTLs {
    fn default() -> Self {
        Self {
            home: 300000,
            discover: 1800000,
            library: 300000,
            recently_added: 300000,
            discover_queue: 86400000,
            search: 300000,
            local_files_sidebar: 120000,
            jellyfin_sidebar: 120000,
            plex_sidebar: 120000,
            playlist_sources: 900000,
            discover_queue_polling_interval: 4000,
            discover_queue_auto_generate: true,
        }
    }
}

/// Add-one-library-path body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPathRequest {
    /// Directory to add as a library root.
    pub path: String,
}

/// Remove-one-library-path query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPathQuery {
    /// Directory to remove.
    pub path: String,
}

// --- library_management (Value-bridged; shapes must equal the section) -------

/// Tag-field write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldModeDto {
    /// Do not touch.
    Disabled,
    /// Overwrite.
    #[default]
    Replace,
    /// Fill when empty.
    FillMissing,
    /// Merge values.
    Merge,
    /// Keep existing.
    Preserve,
}

/// Genre write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenreModeDto {
    /// Overwrite.
    #[default]
    Replace,
    /// Merge values.
    Merge,
    /// Fill when empty.
    FillMissing,
}

/// Genre source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenreSourceDto {
    /// MusicBrainz.
    Musicbrainz,
    /// ListenBrainz.
    Listenbrainz,
    /// Last.fm.
    Lastfm,
    /// Keep local genres.
    ExistingLocal,
}

/// Artwork provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkProviderDto {
    /// CAA release.
    CoverArtArchiveRelease,
    /// CAA release group.
    CoverArtArchiveReleaseGroup,
    /// Local files.
    LocalFiles,
    /// Embedded art.
    Embedded,
    /// AudioDB.
    Audiodb,
}

/// Artwork image type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkImageTypeDto {
    /// Front cover.
    Front,
    /// Back cover.
    Back,
    /// Booklet.
    Booklet,
    /// Medium.
    Medium,
    /// Tray.
    Tray,
    /// Obi.
    Obi,
    /// Spine.
    Spine,
    /// Track art.
    Track,
    /// Other.
    Other,
}

/// Artwork output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkOutputFormatDto {
    /// Keep original.
    #[default]
    Original,
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
    /// WebP.
    Webp,
}

/// Artwork download size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkDownloadSizeDto {
    /// Full size.
    #[default]
    Full,
    /// 1200px.
    #[serde(rename = "1200")]
    Size1200,
    /// 500px.
    #[serde(rename = "500")]
    Size500,
    /// 250px.
    #[serde(rename = "250")]
    Size250,
}

/// Artist credit standardization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistStandardizationDto {
    /// As credited.
    #[default]
    Credited,
    /// Accepted variations.
    Variations,
    /// Canonical name.
    Canonical,
}

/// Credited relationship type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipTypeDto {
    /// Composer.
    Composer,
    /// Lyricist.
    Lyricist,
    /// Conductor.
    Conductor,
    /// Performer.
    Performer,
    /// Arranger.
    Arranger,
    /// Remixer.
    Remixer,
    /// Producer.
    Producer,
    /// Other.
    Other,
}

/// Source-tree cleanup after a confirmed move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceCleanupModeDto {
    /// Leave sources alone.
    Keep,
    /// Remove after a confirmed move.
    #[default]
    RemoveAfterConfirmedMove,
}

/// ID3 version for MP3 writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum Id3VersionDto {
    /// ID3v2.4.
    #[serde(rename = "2.4")]
    #[default]
    V24,
    /// ID3v2.3.
    #[serde(rename = "2.3")]
    V23,
}

/// APEv2 policy for MP3 writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mp3ApePolicyDto {
    /// Preserve APEv2 tags.
    #[default]
    Preserve,
    /// Remove APEv2 tags.
    Remove,
}

/// Raw-AAC tag policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RawAacTagPolicyDto {
    /// Write APEv2.
    #[default]
    SaveApev2,
    /// Write nothing.
    DoNotWrite,
    /// Remove APEv2.
    RemoveApev2,
}

/// WAV tag policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WavTagPolicyDto {
    /// ID3 chunk.
    #[default]
    Id3,
    /// RIFF INFO chunk.
    RiffInfo,
    /// Leave existing tags alone.
    PreserveExisting,
}

/// ID3 text encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Id3TextEncodingDto {
    /// UTF-8.
    #[default]
    Utf8,
    /// UTF-16.
    Utf16,
}

/// Unicode normalization for paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum UnicodeNormalizationDto {
    /// NFC.
    #[default]
    NFC,
    /// NFKC.
    NFKC,
}

/// Extension case policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCaseDto {
    /// Keep as-is.
    #[default]
    Preserve,
    /// Lowercase.
    Lower,
    /// Uppercase.
    Upper,
}

/// ReplayGain write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplayGainModeDto {
    /// Leave tags alone.
    #[default]
    Preserve,
    /// Fill missing tags.
    FillMissing,
    /// Overwrite tags.
    Replace,
}

/// Multi-disc naming mode for a root override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MultiDiscNamingModeDto {
    /// Inherit the profile.
    #[default]
    Inherit,
    /// Standard naming.
    Standard,
    /// Naming script.
    Script,
}

/// One managed tag field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ManagedFieldDto {
    /// Field name.
    pub field: String,
    /// Write mode.
    pub mode: FieldModeDto,
    /// Clear when the canonical value is missing.
    pub clear_when_canonical_missing: bool,
}

impl Default for ManagedFieldDto {
    fn default() -> Self {
        Self {
            field: String::new(),
            mode: FieldModeDto::Replace,
            clear_when_canonical_missing: false,
        }
    }
}

/// Artist-credit handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ArtistCreditSettingsDto {
    /// Standardization level.
    pub standardization: ArtistStandardizationDto,
    /// Translate names.
    pub translate_names: bool,
    /// Preferred locales.
    pub preferred_locales: Vec<String>,
}

impl Default for ArtistCreditSettingsDto {
    fn default() -> Self {
        Self {
            standardization: ArtistStandardizationDto::Credited,
            translate_names: false,
            preferred_locales: Vec::new(),
        }
    }
}

/// Relationship-credit handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RelationshipCreditSettingsDto {
    /// Master switch.
    pub enabled: bool,
    /// Credited relationship types.
    pub types: Vec<RelationshipTypeDto>,
}

impl Default for RelationshipCreditSettingsDto {
    fn default() -> Self {
        Self {
            enabled: true,
            types: vec![
                RelationshipTypeDto::Composer,
                RelationshipTypeDto::Lyricist,
                RelationshipTypeDto::Conductor,
                RelationshipTypeDto::Performer,
                RelationshipTypeDto::Arranger,
                RelationshipTypeDto::Remixer,
                RelationshipTypeDto::Producer,
            ],
        }
    }
}

/// Format-compatibility handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FormatCompatibilitySettingsDto {
    /// ID3 version for MP3 writes.
    pub id3_version: Id3VersionDto,
    /// ID3v2.3 multi-value join delimiter.
    pub id3v23_join_delimiter: String,
    /// ID3 text encoding.
    pub id3_text_encoding: Id3TextEncodingDto,
    /// Strip ID3 chunks from FLAC.
    pub remove_id3_from_flac: bool,
    /// APEv2 policy for MP3.
    pub mp3_apev2_policy: Mp3ApePolicyDto,
    /// Raw-AAC tag policy.
    pub raw_aac_tag_policy: RawAacTagPolicyDto,
    /// WAV tag policy.
    pub wav_tag_policy: WavTagPolicyDto,
    /// Primary genre only for constrained formats.
    pub constrained_genres_primary_only: bool,
}

impl Default for FormatCompatibilitySettingsDto {
    fn default() -> Self {
        Self {
            id3_version: Id3VersionDto::V24,
            id3v23_join_delimiter: "; ".to_owned(),
            id3_text_encoding: Id3TextEncodingDto::Utf8,
            remove_id3_from_flac: false,
            mp3_apev2_policy: Mp3ApePolicyDto::Preserve,
            raw_aac_tag_policy: RawAacTagPolicyDto::SaveApev2,
            wav_tag_policy: WavTagPolicyDto::Id3,
            constrained_genres_primary_only: false,
        }
    }
}

/// Metadata management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MetadataManagementSettingsDto {
    /// Master switch.
    pub enabled: bool,
    /// Managed fields.
    pub fields: Vec<ManagedFieldDto>,
    /// Artist credits.
    pub artist_credits: ArtistCreditSettingsDto,
    /// Relationship credits.
    pub relationships: RelationshipCreditSettingsDto,
    /// Tagging script ids.
    pub tagging_script_ids: Vec<String>,
    /// Fields never touched.
    pub preserve_fields: Vec<String>,
    /// Scrub unmanaged tags.
    pub scrub_unmanaged_tags: bool,
    /// Keep embedded art during a scrub.
    pub preserve_embedded_art_during_scrub: bool,
    /// Format compatibility.
    pub format_compatibility: FormatCompatibilitySettingsDto,
}

impl Default for MetadataManagementSettingsDto {
    fn default() -> Self {
        Self {
            enabled: false,
            fields: Vec::new(),
            artist_credits: ArtistCreditSettingsDto::default(),
            relationships: RelationshipCreditSettingsDto::default(),
            tagging_script_ids: Vec::new(),
            preserve_fields: Vec::new(),
            scrub_unmanaged_tags: false,
            preserve_embedded_art_during_scrub: true,
            format_compatibility: FormatCompatibilitySettingsDto::default(),
        }
    }
}

/// One genre alias.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct GenreAliasDto {
    /// Source label.
    pub source: String,
    /// Target label.
    pub target: String,
}

/// Genre management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct GenreManagementSettingsDto {
    /// Master switch.
    pub enabled: bool,
    /// Write mode.
    pub mode: GenreModeDto,
    /// Genre sources.
    pub sources: Vec<GenreSourceDto>,
    /// Max genres written.
    pub maximum_count: i64,
    /// MusicBrainz vote floor.
    pub musicbrainz_minimum_count: i64,
    /// ListenBrainz vote floor.
    pub listenbrainz_minimum_count: i64,
    /// Last.fm weight floor.
    pub lastfm_minimum_weight: i64,
    /// ListenBrainz curated tags only.
    pub listenbrainz_curated_only: bool,
    /// Last.fm whitelisted tags only.
    pub lastfm_whitelist_only: bool,
    /// Canonicalize labels.
    pub canonicalize: bool,
    /// Max genre-ancestry depth.
    pub maximum_ancestry_depth: i64,
    /// Allowed labels.
    pub allowlist: Vec<String>,
    /// Blocked labels.
    pub denylist: Vec<String>,
    /// Aliases.
    pub aliases: Vec<GenreAliasDto>,
    /// Preferred casing.
    pub preferred_casing: Vec<String>,
    /// Primary genre only for constrained formats.
    pub write_primary_only_for_constrained_formats: bool,
}

impl Default for GenreManagementSettingsDto {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: GenreModeDto::Replace,
            sources: vec![GenreSourceDto::Musicbrainz, GenreSourceDto::Listenbrainz],
            maximum_count: 5,
            musicbrainz_minimum_count: 1,
            listenbrainz_minimum_count: 1,
            lastfm_minimum_weight: 10,
            listenbrainz_curated_only: true,
            lastfm_whitelist_only: true,
            canonicalize: true,
            maximum_ancestry_depth: 4,
            allowlist: Vec::new(),
            denylist: Vec::new(),
            aliases: Vec::new(),
            preferred_casing: Vec::new(),
            write_primary_only_for_constrained_formats: false,
        }
    }
}

/// Artwork management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ArtworkManagementSettingsDto {
    /// Embed art in files.
    pub embedded_enabled: bool,
    /// Write external art files.
    pub external_enabled: bool,
    /// Provider order.
    pub providers: Vec<ArtworkProviderDto>,
    /// Approved art only.
    pub approved_only: bool,
    /// Download size.
    pub download_size: ArtworkDownloadSizeDto,
    /// Local filename patterns.
    pub local_file_patterns: Vec<String>,
    /// Image types to fetch.
    pub image_types: Vec<ArtworkImageTypeDto>,
    /// Minimum width (0 any).
    pub minimum_width: i64,
    /// Minimum height (0 any).
    pub minimum_height: i64,
    /// Embedded size cap (0 uncapped).
    pub embedded_maximum_size: i64,
    /// Embedded output format.
    pub embedded_format: ArtworkOutputFormatDto,
    /// External size cap (0 uncapped).
    pub external_maximum_size: i64,
    /// External output format.
    pub external_format: ArtworkOutputFormatDto,
    /// Embedded front only.
    pub embedded_front_only: bool,
    /// External front only.
    pub external_front_only: bool,
    /// Never replace art with smaller art.
    pub never_replace_with_smaller: bool,
    /// Existing types never replaced.
    pub preserve_existing_types: Vec<ArtworkImageTypeDto>,
    /// External naming script id.
    pub external_naming_script_id: Option<String>,
    /// Overwrite external files.
    pub overwrite_external_files: bool,
}

impl Default for ArtworkManagementSettingsDto {
    fn default() -> Self {
        Self {
            embedded_enabled: true,
            external_enabled: true,
            providers: vec![
                ArtworkProviderDto::CoverArtArchiveRelease,
                ArtworkProviderDto::CoverArtArchiveReleaseGroup,
                ArtworkProviderDto::LocalFiles,
                ArtworkProviderDto::Embedded,
            ],
            approved_only: true,
            download_size: ArtworkDownloadSizeDto::Full,
            local_file_patterns: [
                "cover.jpg",
                "cover.jpeg",
                "cover.png",
                "cover.webp",
                "folder.jpg",
                "folder.png",
                "front.jpg",
                "front.png",
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
            image_types: vec![ArtworkImageTypeDto::Front],
            minimum_width: 0,
            minimum_height: 0,
            embedded_maximum_size: 1200,
            embedded_format: ArtworkOutputFormatDto::Jpeg,
            external_maximum_size: 0,
            external_format: ArtworkOutputFormatDto::Original,
            embedded_front_only: true,
            external_front_only: true,
            never_replace_with_smaller: true,
            preserve_existing_types: Vec::new(),
            external_naming_script_id: None,
            overwrite_external_files: false,
        }
    }
}

/// Path-compatibility handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PathCompatibilitySettingsDto {
    /// Windows-safe names.
    pub windows_compatible: bool,
    /// Replace non-ASCII characters.
    pub replace_non_ascii: bool,
    /// Replace spaces with underscores.
    pub replace_spaces_with_underscores: bool,
    /// Path-separator replacement.
    pub separator_replacement: String,
    /// Max path-component length.
    pub maximum_component_length: i64,
    /// Max path length.
    pub maximum_path_length: i64,
    /// Unicode normalization.
    pub unicode_normalization: UnicodeNormalizationDto,
    /// Extension case.
    pub extension_case: ExtensionCaseDto,
    /// Honor the legacy Windows path limit.
    pub windows_legacy_path_limit: bool,
}

impl Default for PathCompatibilitySettingsDto {
    fn default() -> Self {
        Self {
            windows_compatible: true,
            replace_non_ascii: false,
            replace_spaces_with_underscores: false,
            separator_replacement: "_".to_owned(),
            maximum_component_length: 240,
            maximum_path_length: 4096,
            unicode_normalization: UnicodeNormalizationDto::NFC,
            extension_case: ExtensionCaseDto::Preserve,
            windows_legacy_path_limit: false,
        }
    }
}

/// Organization management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OrganizationManagementSettingsDto {
    /// Rename files.
    pub rename_enabled: bool,
    /// Move files.
    pub move_enabled: bool,
    /// Naming script id.
    pub naming_script_id: String,
    /// Multi-disc naming script id.
    pub multi_disc_naming_script_id: Option<String>,
    /// Path compatibility.
    pub compatibility: PathCompatibilitySettingsDto,
    /// Move sidecar files along.
    pub move_sidecars: bool,
    /// Sidecar filename patterns.
    pub sidecar_patterns: Vec<String>,
    /// Source cleanup after a confirmed move.
    pub source_cleanup: SourceCleanupModeDto,
    /// Remove newly empty directories.
    pub remove_empty_directories: bool,
}

impl Default for OrganizationManagementSettingsDto {
    fn default() -> Self {
        use crate::runtime_config::sections::{
            DEFAULT_SIDECAR_PATTERNS, PICARD_ORGANIZER_NAMING_SCRIPT_ID,
        };
        Self {
            rename_enabled: true,
            move_enabled: true,
            naming_script_id: PICARD_ORGANIZER_NAMING_SCRIPT_ID.to_owned(),
            multi_disc_naming_script_id: None,
            compatibility: PathCompatibilitySettingsDto::default(),
            move_sidecars: true,
            sidecar_patterns: DEFAULT_SIDECAR_PATTERNS
                .iter()
                .map(ToString::to_string)
                .collect(),
            source_cleanup: SourceCleanupModeDto::RemoveAfterConfirmedMove,
            remove_empty_directories: true,
        }
    }
}

/// File-behavior gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FileBehaviorSettingsDto {
    /// Preserve mtimes.
    pub preserve_timestamps: bool,
    /// Preserve permission bits.
    pub preserve_permissions: bool,
    /// Refuse writes the format cannot hold.
    pub strict_capability_gate: bool,
    /// Refuse symlinked media.
    pub reject_symlinks: bool,
    /// Re-read tags after writing.
    pub validate_written_metadata: bool,
    /// Re-probe audio after writing.
    pub validate_technical_audio: bool,
}

impl Default for FileBehaviorSettingsDto {
    fn default() -> Self {
        Self {
            preserve_timestamps: true,
            preserve_permissions: true,
            strict_capability_gate: true,
            reject_symlinks: true,
            validate_written_metadata: true,
            validate_technical_audio: true,
        }
    }
}

/// Lyrics enrichment block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LyricsManagementSettingsDto {
    /// Master switch.
    pub enabled: bool,
    /// Lyrics provider.
    pub provider: String,
    /// Write plain lyrics.
    pub write_plain: bool,
    /// Write synced lyrics.
    pub write_synced: bool,
    /// Keep existing lyrics.
    pub preserve_existing: bool,
    /// Lyrics required for completion.
    pub required: bool,
}

impl Default for LyricsManagementSettingsDto {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "lrclib".to_owned(),
            write_plain: true,
            write_synced: true,
            preserve_existing: false,
            required: false,
        }
    }
}

/// ReplayGain enrichment block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ReplayGainManagementSettingsDto {
    /// Master switch.
    pub enabled: bool,
    /// Write mode.
    pub mode: ReplayGainModeDto,
    /// Album-aware gain.
    pub album_aware: bool,
    /// Gain required for completion.
    pub required: bool,
}

impl Default for ReplayGainManagementSettingsDto {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ReplayGainModeDto::Preserve,
            album_aware: true,
            required: false,
        }
    }
}

/// Enrichment management block.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct EnrichmentManagementSettingsDto {
    /// Lyrics.
    pub lyrics: LyricsManagementSettingsDto,
    /// ReplayGain.
    pub replaygain: ReplayGainManagementSettingsDto,
}

/// Catalog-identity policy (no file writes).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct IdentityManagementSettingsDto {
    /// Automatic edition acceptance.
    pub automatic_edition_acceptance_enabled: bool,
}

/// Post-publish notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ProfileNotificationSettingsDto {
    /// Refresh DroppedNeedle views.
    pub refresh_droppedneedle: bool,
    /// Refresh external servers.
    pub refresh_external_servers: bool,
}

impl Default for ProfileNotificationSettingsDto {
    fn default() -> Self {
        Self {
            refresh_droppedneedle: true,
            refresh_external_servers: false,
        }
    }
}

/// One named management profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryManagementProfileDto {
    /// Profile id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Preset origin.
    pub preset_origin: Option<String>,
    /// Preset version.
    pub preset_version: Option<i64>,
    /// Content revision.
    pub revision: String,
    /// Metadata block.
    pub metadata: MetadataManagementSettingsDto,
    /// Genre block.
    pub genres: GenreManagementSettingsDto,
    /// Artwork block.
    pub artwork: ArtworkManagementSettingsDto,
    /// Organization block.
    pub organization: OrganizationManagementSettingsDto,
    /// File-behavior gates.
    pub file_behavior: FileBehaviorSettingsDto,
    /// Enrichment block.
    pub enrichment: EnrichmentManagementSettingsDto,
    /// Identity policy.
    pub identity: IdentityManagementSettingsDto,
    /// Notifications.
    pub notification: ProfileNotificationSettingsDto,
}

impl Default for LibraryManagementProfileDto {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            preset_origin: None,
            preset_version: None,
            revision: String::new(),
            metadata: MetadataManagementSettingsDto {
                enabled: true,
                preserve_embedded_art_during_scrub: true,
                ..MetadataManagementSettingsDto::default()
            },
            genres: GenreManagementSettingsDto::default(),
            artwork: ArtworkManagementSettingsDto::default(),
            organization: OrganizationManagementSettingsDto::default(),
            file_behavior: FileBehaviorSettingsDto::default(),
            enrichment: EnrichmentManagementSettingsDto::default(),
            identity: IdentityManagementSettingsDto::default(),
            notification: ProfileNotificationSettingsDto::default(),
        }
    }
}

/// One naming script.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NamingScriptDto {
    /// Script id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Script source.
    pub source: String,
    /// Content revision.
    pub revision: String,
    /// Preset origin.
    pub preset_origin: Option<String>,
    /// Preset version.
    pub preset_version: Option<i64>,
}

/// One tagging script.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct TaggingScriptDto {
    /// Script id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Script source.
    pub source: String,
    /// Content revision.
    pub revision: String,
    /// Preset origin.
    pub preset_origin: Option<String>,
    /// Preset version.
    pub preset_version: Option<i64>,
}

/// Per-root profile overrides (`None` inherits the profile).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryManagementRootOverridesDto {
    /// Metadata switch.
    pub metadata_enabled: Option<bool>,
    /// Genre switch.
    pub genres_enabled: Option<bool>,
    /// Embedded-artwork switch.
    pub embedded_artwork_enabled: Option<bool>,
    /// External-artwork switch.
    pub external_artwork_enabled: Option<bool>,
    /// Rename switch.
    pub rename_enabled: Option<bool>,
    /// Move switch.
    pub move_enabled: Option<bool>,
    /// Sidecar-move switch.
    pub move_sidecars: Option<bool>,
    /// Source cleanup.
    pub source_cleanup: Option<SourceCleanupModeDto>,
    /// Timestamp preservation.
    pub preserve_timestamps: Option<bool>,
    /// Naming script id.
    pub naming_script_id: Option<String>,
    /// Multi-disc naming mode.
    pub multi_disc_naming_mode: MultiDiscNamingModeDto,
    /// Multi-disc naming script id.
    pub multi_disc_naming_script_id: Option<String>,
    /// Automatic edition acceptance.
    pub automatic_edition_acceptance_enabled: Option<bool>,
}

/// One root-to-profile assignment.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryManagementRootAssignmentDto {
    /// Library root id.
    pub root_id: String,
    /// Assigned profile id.
    pub profile_id: Option<String>,
    /// Per-root overrides.
    pub overrides: Option<LibraryManagementRootOverridesDto>,
    /// Assignment switch.
    pub enabled: bool,
    /// Automatic acquisitions.
    pub automatic_acquisitions: bool,
    /// Automatic drop imports.
    pub automatic_drop_imports: bool,
    /// Automatic scan-discovered organization.
    pub automatic_scan_discovered: bool,
    /// Automatic custom editions.
    pub automatic_custom_editions: bool,
    /// Activation pins (set by the activation flow).
    pub activation_profile_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_naming_policy_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_policy_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_settings_revision: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_preview_token: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_preview_hash: Option<String>,
    /// Activation pins (set by the activation flow).
    pub activation_confirmed_at: Option<f64>,
}

/// External-server refresh after publishing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ExternalRefreshSettingsDto {
    /// Master switch.
    pub enabled: bool,
    /// Refresh Plex.
    pub plex_enabled: bool,
    /// Refresh Jellyfin.
    pub jellyfin_enabled: bool,
    /// Refresh Navidrome.
    pub navidrome_enabled: bool,
    /// Retry attempts.
    pub retry_attempts: i64,
    /// Retry delay in seconds.
    pub retry_delay_seconds: i64,
}

impl Default for ExternalRefreshSettingsDto {
    fn default() -> Self {
        Self {
            enabled: false,
            plex_enabled: false,
            jellyfin_enabled: false,
            navidrome_enabled: false,
            retry_attempts: 3,
            retry_delay_seconds: 30,
        }
    }
}

/// Full management settings (deliberately secret-free).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryManagementSettingsDto {
    /// Settings schema version.
    pub schema_version: i64,
    /// Preset catalog version.
    pub preset_catalog_version: i64,
    /// Named profiles.
    pub profiles: Vec<LibraryManagementProfileDto>,
    /// Default profile id.
    pub default_profile_id: String,
    /// Root assignments.
    pub root_assignments: Vec<LibraryManagementRootAssignmentDto>,
    /// Naming scripts.
    pub naming_scripts: Vec<NamingScriptDto>,
    /// Tagging scripts.
    pub tagging_scripts: Vec<TaggingScriptDto>,
    /// Undo retention in days.
    pub undo_retention_days: i64,
    /// Preview retention in hours.
    pub preview_retention_hours: i64,
    /// Recycle-bin path ("" disables).
    pub recycle_bin_path: String,
    /// External refresh.
    pub external_refresh: ExternalRefreshSettingsDto,
}

impl Default for LibraryManagementSettingsDto {
    fn default() -> Self {
        Self {
            schema_version: 1,
            preset_catalog_version: 0,
            profiles: Vec::new(),
            default_profile_id: String::new(),
            root_assignments: Vec::new(),
            naming_scripts: Vec::new(),
            tagging_scripts: Vec::new(),
            undo_retention_days: 90,
            preview_retention_hours: 24,
            recycle_bin_path: String::new(),
            external_refresh: ExternalRefreshSettingsDto::default(),
        }
    }
}

/// GET/PUT view: the settings plus their content revision (the write CAS
/// token).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryManagementSettingsResponse {
    /// Settings schema version.
    pub schema_version: i64,
    /// Preset catalog version.
    pub preset_catalog_version: i64,
    /// Named profiles.
    pub profiles: Vec<LibraryManagementProfileDto>,
    /// Default profile id.
    pub default_profile_id: String,
    /// Root assignments.
    pub root_assignments: Vec<LibraryManagementRootAssignmentDto>,
    /// Naming scripts.
    pub naming_scripts: Vec<NamingScriptDto>,
    /// Tagging scripts.
    pub tagging_scripts: Vec<TaggingScriptDto>,
    /// Undo retention in days.
    pub undo_retention_days: i64,
    /// Preview retention in hours.
    pub preview_retention_hours: i64,
    /// Recycle-bin path ("" disables).
    pub recycle_bin_path: String,
    /// External refresh.
    pub external_refresh: ExternalRefreshSettingsDto,
    /// Settings content revision.
    pub settings_revision: String,
}

impl Default for LibraryManagementSettingsResponse {
    fn default() -> Self {
        Self {
            schema_version: 1,
            preset_catalog_version: 0,
            profiles: Vec::new(),
            default_profile_id: String::new(),
            root_assignments: Vec::new(),
            naming_scripts: Vec::new(),
            tagging_scripts: Vec::new(),
            undo_retention_days: 90,
            preview_retention_hours: 24,
            recycle_bin_path: String::new(),
            external_refresh: ExternalRefreshSettingsDto::default(),
            settings_revision: String::new(),
        }
    }
}

/// Dry-run activation health for active automatic roots. A root is stale
/// when its saved activation no longer matches the current effective
/// profile, naming policy, or library policy and needs a fresh dry run.
/// A root is blocked when no dry run could help. `blocked_reason` is only
/// set alongside a non-empty `blocked_root_ids`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LibraryManagementActivationHealthResponse {
    /// Active roots whose activation no longer matches.
    pub stale_root_ids: Vec<String>,
    /// Active roots no dry run could help.
    pub blocked_root_ids: Vec<String>,
    /// Policy error, set only with a non-empty `blocked_root_ids`.
    pub blocked_reason: Option<String>,
}

/// PUT body: full settings plus the required CAS token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementSaveRequest {
    /// Full candidate settings.
    pub settings: LibraryManagementSettingsDto,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Change-impact classification: `no_change`, `harmless`, `restrictive`,
/// `destructive`.
pub type ManagementImpactClassification = String;

/// Impact/validate verdict: which automatic roots gain file-writing
/// scope under the candidate, and whether a fresh dry run must be
/// confirmed first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementChangeImpact {
    /// Current stored revision.
    pub current_settings_revision: String,
    /// Normalized candidate revision.
    pub proposed_settings_revision: String,
    /// The caller's revision was already stale.
    pub stale: bool,
    /// `no_change`, `harmless`, `restrictive`, or `destructive`.
    pub classification: ManagementImpactClassification,
    /// A current dry run must be confirmed before enabling.
    pub preview_required: bool,
    /// Affected root ids.
    pub affected_root_ids: Vec<String>,
    /// Human reasons.
    pub reasons: Vec<String>,
}

/// Impact/validate request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementSettingsImpactRequest {
    /// Full candidate settings.
    pub settings: LibraryManagementSettingsDto,
    /// Compare-and-swap token from the last GET, if any.
    pub expected_settings_revision: Option<String>,
}

/// Create-profile request (clones the default profile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileCreateRequest {
    /// Name for the new profile.
    pub name: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
    /// Description for the new profile.
    pub description: String,
}

/// Copy-profile request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileCopyRequest {
    /// Name for the copy.
    pub name: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Update-profile request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileUpdateRequest {
    /// Full replacement profile (id must match the path).
    pub profile: LibraryManagementProfileDto,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Delete-profile request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileDeleteRequest {
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Profile-mutation acknowledgement: the saved profile plus the fresh
/// revision (a second read, so concurrent saves surface as staleness on
/// the next write instead of silently winning).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileMutationResponse {
    /// Saved profile.
    pub profile: LibraryManagementProfileDto,
    /// Fresh settings revision.
    pub settings_revision: String,
}

/// Preset-drift diff: which groups differ from the tracked preset, plus
/// the preset profile itself for the UI to render against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementPresetDiff {
    /// Profile id.
    pub profile_id: String,
    /// Preset origin, when the profile tracks one.
    pub preset_origin: Option<String>,
    /// Preset version, when the profile tracks one.
    pub preset_version: Option<i64>,
    /// Whether any group differs.
    pub differs: bool,
    /// Differing groups.
    pub changed_groups: Vec<String>,
    /// Groups a preset version upgrade would touch.
    pub version_upgrade_groups: Vec<String>,
    /// The preset profile, when the profile tracks one.
    pub preset_profile: Option<LibraryManagementProfileDto>,
}

/// Export request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileExportRequest {
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Exported bundle: a portable document plus a share code, both pinned
/// by the bundle hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileExportResponse {
    /// Suggested download filename.
    pub filename: String,
    /// Bundle MIME type.
    pub mime_type: String,
    /// Portable document (JSON).
    pub document: String,
    /// Share code (`DNLP1:...`).
    pub share_code: String,
    /// Bundle hash (import pins to the reviewed hash).
    pub bundle_hash: String,
    /// Settings revision at export time.
    pub settings_revision: String,
}

/// Import-preview request (accepts a document or a share code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportPreviewRequest {
    /// Bundle document or share code.
    pub content: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// One import warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportWarning {
    /// Warning code.
    pub code: String,
    /// `warning` or `danger`.
    pub severity: String,
    /// Short title.
    pub title: String,
    /// Human explanation.
    pub message: String,
}

/// Import-preview response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportPreviewResponse {
    /// Materialized profile (names resolved against stored settings).
    pub profile: LibraryManagementProfileDto,
    /// Reviewed bundle hash (the import must pin to this).
    pub bundle_hash: String,
    /// Settings revision at preview time.
    pub settings_revision: String,
    /// Naming scripts the bundle carries.
    pub naming_scripts: Vec<NamingScriptDto>,
    /// Tagging scripts the bundle carries.
    pub tagging_scripts: Vec<TaggingScriptDto>,
    /// Capability aspects the profile uses.
    pub aspects: Vec<String>,
    /// Import warnings.
    pub warnings: Vec<LibraryManagementProfileImportWarning>,
}

/// Import-confirm request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportRequest {
    /// Bundle document or share code.
    pub content: String,
    /// Bundle hash the admin reviewed.
    pub reviewed_bundle_hash: String,
    /// Name for the imported profile.
    pub name: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Import-confirm response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportResponse {
    /// Imported profile.
    pub profile: LibraryManagementProfileDto,
    /// New settings revision.
    pub settings_revision: String,
    /// Naming scripts the import added.
    pub naming_scripts: Vec<NamingScriptDto>,
    /// Tagging scripts the import added.
    pub tagging_scripts: Vec<TaggingScriptDto>,
}

// --- section prefs -----------------------------------------------------------

/// One toggleable UI section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SectionPrefItem {
    /// Section key.
    pub key: String,
    /// Display title.
    pub title: String,
    /// Description.
    pub description: String,
    /// Layout zone.
    pub zone: String,
    /// User toggle.
    pub enabled: bool,
    /// Backend availability (requires + linked services).
    pub available: bool,
    /// Service requirement, when any.
    pub requires: Option<String>,
}

/// Section prefs by page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SectionPrefsResponse {
    /// Sections per page (`home`, `discover`, `sidebar`).
    pub pages: std::collections::BTreeMap<String, Vec<SectionPrefItem>>,
}

/// One section toggle update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SectionPrefUpdateItem {
    /// Section key.
    pub key: String,
    /// New toggle value.
    pub enabled: bool,
}

/// Section prefs update (one page per call).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SectionPrefsUpdate {
    /// Page to update (`home`, `discover`, `sidebar`).
    pub page: String,
    /// Toggles to apply.
    pub sections: Vec<SectionPrefUpdateItem>,
}
