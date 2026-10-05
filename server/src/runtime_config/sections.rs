//! Typed config sections without secrets (tier 2 of 2).
//!
//! Every user-editable runtime value lives in exactly one section struct
//! here (or in `secret_sections` when it holds secrets). Sections are plain
//! serde data with two hooks:
//!
//! - `validate()` runs on the save path and rejects bad submitted values
//!   with typed errors. It never heals: a save either lands or fails.
//! - `normalize()` runs on both paths and heals stored drift the way v2
//!   `__post_init__` did (unknown tier labels fall back, out-of-range
//!   optional bounds clear to `None`). Loading an old or hand-edited file
//!   never bricks the boot.
//!
//! Unknown JSON fields inside a section decode leniently (forward
//! compatibility); unknown section keys at the top level are reported by
//! `ConfigStore::unknown_top_level_keys` for boot diagnostics.

use serde::{Deserialize, Serialize};

use super::error::ConfigError;

/// One user-editable runtime section stored under a config-file key.
pub trait Section: Default + Serialize + serde::de::DeserializeOwned {
    /// Config-file key, kept byte-identical to v2 for import fidelity.
    const KEY: &'static str;

    /// Reject bad submitted values with typed errors. Secret fields are
    /// never validated here: on the save path they may hold the mask.
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }

    /// Heal stored drift and apply v2 `__post_init__` sanitization. Must be
    /// infallible. Must not touch secret fields, with one exception: the
    /// AudioDB blank-to-"123" default healing (v2 does it on every
    /// construction). The store runs `normalize` before masking on reads,
    /// so the healed default still masks.
    fn normalize(&mut self) {}
}

/// Marker for sections with no secret fields. Only these may use the plain
/// `ConfigStore::save`; secret sections must go through `save_secret` (and
/// indexers/plugins through their dedicated methods), so storing a secret
/// as plaintext is a compile error rather than a code-review catch.
/// `Plugins` is not plain, on purpose: its secret-flagged values need the
/// manifest's secret-key set at save time.
pub trait PlainSection: Section {}

impl PlainSection for UserPreferences {}
impl PlainSection for LibraryScanSchedule {}
impl PlainSection for FilesystemWatcher {}
impl PlainSection for WantedWatcher {}
impl PlainSection for SourcePriority {}
impl PlainSection for UsenetBackendSetting {}
impl PlainSection for ScrobbleSettings {}
impl PlainSection for PrimaryMusicSource {}
impl PlainSection for FreeMusic {}
impl PlainSection for GetIt {}
impl PlainSection for SecuritySettings {}
impl PlainSection for ConnectApps {}
impl PlainSection for DownloadPolicy {}
impl PlainSection for MusicBrainzSettings {}
impl PlainSection for LastFmSettings {}
impl PlainSection for LyricsSettings {}
impl PlainSection for InternalState {}
impl PlainSection for LibraryManagement {}

/// Reject `value` outside `min..=max` with a typed validation error.
pub fn check_range<T>(
    section: &'static str,
    field: &'static str,
    value: T,
    min: T,
    max: T,
) -> Result<(), ConfigError>
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    if value < min || value > max {
        return Err(ConfigError::Validation {
            section,
            field,
            reason: format!("must be between {min} and {max}, got {value}"),
        });
    }
    Ok(())
}

/// `HH:MM` server-local time (`^([01]\d|2[0-3]):[0-5]\d$`), no regex crate.
#[must_use]
pub fn is_valid_hhmm(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return false;
    }
    let digit = |index: usize| bytes[index].is_ascii_digit();
    if !(digit(0) && digit(1) && digit(3) && digit(4)) {
        return false;
    }
    let hour = (bytes[0] - b'0') * 10 + (bytes[1] - b'0');
    let minute = (bytes[3] - b'0') * 10 + (bytes[4] - b'0');
    hour <= 23 && minute <= 59
}

/// v2 URL normalization: strip, prepend `default_scheme` when schemeless,
/// drop trailing slashes.
pub fn normalize_http_url(url: &mut String, default_scheme: &str) {
    let trimmed = url.trim().to_owned();
    let mut owned = if trimmed.is_empty()
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
    {
        trimmed
    } else {
        format!("{default_scheme}{trimmed}")
    };
    while owned.ends_with('/') && owned.len() > 1 {
        owned.pop();
    }
    *url = owned;
}

/// Strip one pasted `/api/v1` or `/api` suffix (Prowlarr/Lidarr precedent).
pub fn strip_api_suffix(url: &mut String) {
    for suffix in ["/api/v1", "/api"] {
        if let Some(prefix) = url.strip_suffix(suffix) {
            let mut owned = prefix.to_owned();
            while owned.ends_with('/') && owned.len() > 1 {
                owned.pop();
            }
            *url = owned;
            return;
        }
    }
}

/// Keep only safe relative components (slskd subpath precedent): drop "",
/// ".", "..", and any drive/anchor, so the result can never escape the
/// mount it joins onto.
#[must_use]
pub fn sanitize_subpath(raw: &str) -> String {
    raw.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .collect::<Vec<_>>()
        .join("/")
}

/// Absolute-container-path hygiene (slskd incomplete-mount precedent):
/// relative input or a bare "/" has no safe meaning, so it becomes "".
#[must_use]
pub fn sanitize_absolute_mount(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with('/') {
        return String::new();
    }
    let cleaned = sanitize_subpath(trimmed);
    if cleaned.is_empty() {
        String::new()
    } else {
        format!("/{cleaned}")
    }
}

/// Acquisition quality tiers, worst to best (mirrors v2 `TIER_KEYS`).
pub const TIER_KEYS_WORST_FIRST: [&str; 5] = ["low", "mp3_192", "mp3_256", "mp3_320", "lossless"];
/// Acquisition quality tiers, best first (v2 `_TIER_KEYS_BEST_FIRST`).
pub const TIER_KEYS_BEST_FIRST: [&str; 5] = ["lossless", "mp3_320", "mp3_256", "mp3_192", "low"];

/// Rank of a tier label, worst (0) to best (4).
#[must_use]
pub fn tier_rank(tier: &str) -> Option<usize> {
    TIER_KEYS_WORST_FIRST.iter().position(|key| *key == tier)
}

/// Default preference order for an accepted tier range: every tier from
/// `quality_max` down to `quality_min` (v2 `derive_default_order`).
#[must_use]
pub fn derive_default_order(quality_min: &str, quality_max: &str) -> Vec<String> {
    let lo = TIER_KEYS_BEST_FIRST
        .iter()
        .position(|key| *key == quality_max);
    let hi = TIER_KEYS_BEST_FIRST
        .iter()
        .position(|key| *key == quality_min);
    match (lo, hi) {
        (Some(lo), Some(hi)) if lo <= hi => TIER_KEYS_BEST_FIRST[lo..=hi]
            .iter()
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// v2 default naming template (kept in sync with the publisher).
pub const DEFAULT_NAMING_TEMPLATE: &str =
    "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}";

// --- user_preferences -----------------------------------------------------

/// Release-type filters for discovery and search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserPreferences {
    /// Primary release types (album, ep, single, ...).
    pub primary_types: Vec<String>,
    /// Secondary release types (studio, live, ...).
    pub secondary_types: Vec<String>,
}

impl Default for UserPreferences {
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

impl Section for UserPreferences {
    const KEY: &'static str = "user_preferences";
}

// --- library_scan_schedule (the v2 sync section is dropped, this one kept) -

/// Automatic-scan cadence values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ScanFrequency {
    /// Never scan automatically.
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

/// Native automatic-scan schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryScanSchedule {
    /// Rolling-gap cadence, or `daily` for a fixed clock time.
    pub scan_frequency: ScanFrequency,
    /// Server-local `HH:MM` used when `scan_frequency` is `daily`.
    pub daily_scan_time: String,
    /// Unix time of the last scan, if any.
    pub last_scan: Option<i64>,
    /// Whether the last scan succeeded.
    pub last_scan_success: bool,
}

impl Default for LibraryScanSchedule {
    fn default() -> Self {
        Self {
            scan_frequency: ScanFrequency::Hr24,
            daily_scan_time: "03:00".to_owned(),
            last_scan: None,
            last_scan_success: true,
        }
    }
}

impl Section for LibraryScanSchedule {
    const KEY: &'static str = "library_scan_schedule";

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_valid_hhmm(&self.daily_scan_time) {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "daily_scan_time",
                reason: format!(
                    "must be HH:MM (00:00-23:59), got {:?}",
                    self.daily_scan_time
                ),
            });
        }
        Ok(())
    }
}

// --- library_scan_filesystem_watcher --------------------------------------

/// Zero-dependency filesystem poller knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesystemWatcher {
    /// Master switch.
    pub enabled: bool,
    /// Stat-snapshot cadence in seconds (minimum 1).
    pub poll_interval_seconds: f64,
    /// Burst-collapse window in seconds (minimum 0).
    pub batch_window_seconds: f64,
}

impl Default for FilesystemWatcher {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_seconds: 300.0,
            batch_window_seconds: 60.0,
        }
    }
}

impl Section for FilesystemWatcher {
    const KEY: &'static str = "library_scan_filesystem_watcher";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "poll_interval_seconds",
            self.poll_interval_seconds,
            1.0,
            f64::MAX,
        )?;
        check_range(
            Self::KEY,
            "batch_window_seconds",
            self.batch_window_seconds,
            0.0,
            f64::MAX,
        )?;
        Ok(())
    }
}

// --- wanted ---------------------------------------------------------------

/// Wanted watcher toggles. Cadence stays code constants on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WantedWatcher {
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

impl Default for WantedWatcher {
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

impl Section for WantedWatcher {
    const KEY: &'static str = "wanted";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "max_checks_per_sweep",
            self.max_checks_per_sweep,
            1,
            20,
        )?;
        check_range(
            Self::KEY,
            "dormant_after_days",
            self.dormant_after_days,
            30,
            3650,
        )?;
        Ok(())
    }
}

// --- source_priority ------------------------------------------------------

/// Acquisition source try-order. Bundled sources are always present;
/// well-formed `plugin:<name>` keys pass through order-preserved (stale keys
/// survive reload so Settings can grey them); anything else is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct SourcePriority(pub Vec<String>);

/// `^plugin:[a-z0-9][a-z0-9-]{0,31}$`, without a regex crate.
#[must_use]
pub fn is_plugin_source_key(key: &str) -> bool {
    let Some(name) = key.strip_prefix("plugin:") else {
        return false;
    };
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

impl SourcePriority {
    fn clean(order: &[String]) -> Vec<String> {
        let mut clean = Vec::new();
        for entry in order {
            let keep = entry == "soulseek" || entry == "usenet" || is_plugin_source_key(entry);
            if keep && !clean.contains(entry) {
                clean.push(entry.clone());
            }
        }
        for bundled in ["soulseek", "usenet"] {
            if !clean.iter().any(|entry| entry == bundled) {
                clean.push(bundled.to_owned());
            }
        }
        clean
    }
}

impl Section for SourcePriority {
    const KEY: &'static str = "source_priority";

    fn normalize(&mut self) {
        self.0 = Self::clean(&self.0);
    }
}

// --- usenet_search_backend ------------------------------------------------

/// Which Usenet search side is active. Reads collapse unknown values to
/// `Indexers` (v2: pre-existing setups behave as before with no migration);
/// saves only accept known values, via `parse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsenetBackend {
    /// The native Newznab priority list.
    #[default]
    Indexers,
    /// The single Prowlarr connection.
    Prowlarr,
}

impl UsenetBackend {
    /// Strict parse for submitted values. Unknown backends are a typed
    /// error, never a silent collapse.
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw {
            "indexers" => Ok(Self::Indexers),
            "prowlarr" => Ok(Self::Prowlarr),
            _ => Err(ConfigError::Validation {
                section: UsenetBackendSetting::KEY,
                field: "backend",
                reason: format!(
                    "unknown Usenet search backend {raw:?} (expected 'indexers' or 'prowlarr')"
                ),
            }),
        }
    }

    #[must_use]
    fn as_str(self) -> &'static str {
        match self {
            Self::Indexers => "indexers",
            Self::Prowlarr => "prowlarr",
        }
    }
}

/// The `usenet_search_backend` section: a bare JSON string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsenetBackendSetting(pub UsenetBackend);

impl Serialize for UsenetBackendSetting {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for UsenetBackendSetting {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self(match raw.as_str() {
            "prowlarr" => UsenetBackend::Prowlarr,
            _ => UsenetBackend::Indexers,
        }))
    }
}

impl Section for UsenetBackendSetting {
    const KEY: &'static str = "usenet_search_backend";
}

// --- scrobble_settings / primary_music_source -----------------------------

/// Scrobble targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScrobbleSettings {
    /// Scrobble to Last.fm (per-user credentials).
    pub scrobble_to_lastfm: bool,
    /// Scrobble to ListenBrainz.
    pub scrobble_to_listenbrainz: bool,
}

impl Section for ScrobbleSettings {
    const KEY: &'static str = "scrobble_settings";
}

/// Which service backs scrobble-targeted discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MusicSource {
    /// ListenBrainz.
    #[default]
    Listenbrainz,
    /// Last.fm.
    #[serde(rename = "lastfm")]
    Lastfm,
}

/// Primary music source selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrimaryMusicSource {
    /// Active source.
    pub source: MusicSource,
}

impl Section for PrimaryMusicSource {
    const KEY: &'static str = "primary_music_source";
}

// --- free_music / get_it --------------------------------------------------

/// Transcode target for remote playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    /// FLAC.
    #[default]
    Flac,
    /// MP3.
    Mp3,
    /// Opus (transcode targets only).
    Opus,
}

/// Free-music acquisition settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FreeMusic {
    /// Master switch.
    pub enabled: bool,
    /// Preferred format.
    pub preferred_format: AudioFormat,
}

impl Default for FreeMusic {
    fn default() -> Self {
        Self {
            enabled: true,
            preferred_format: AudioFormat::Flac,
        }
    }
}

impl Section for FreeMusic {
    const KEY: &'static str = "free_music";

    fn validate(&self) -> Result<(), ConfigError> {
        if self.preferred_format == AudioFormat::Opus {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "preferred_format",
                reason: "must be flac or mp3".to_owned(),
            });
        }
        Ok(())
    }
}

/// Store-region settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GetIt {
    /// Two-letter store region.
    pub store_region: String,
}

impl Default for GetIt {
    fn default() -> Self {
        Self {
            store_region: "US".to_owned(),
        }
    }
}

impl Section for GetIt {
    const KEY: &'static str = "get_it";

    fn validate(&self) -> Result<(), ConfigError> {
        let valid = self.store_region.len() == 2
            && self
                .store_region
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic());
        if !valid {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "store_region",
                reason: format!(
                    "must be a two-letter region code, got {:?}",
                    self.store_region
                ),
            });
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.store_region = self.store_region.to_ascii_uppercase();
    }
}

// --- security_settings ----------------------------------------------------

/// Who may download library files. `trusted` admits trusted and admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

// --- download_policy ------------------------------------------------------
// Dropped from storage: quality_recipe_status / quality_recipe_error. In v2
// those are read-only projections computed at GET time (route surfaces
// "v2"/"non_convertible"/"invalid"); persisting them would pin stale
// verdicts, so readers recompute the verdict instead.
// The save-time cross-check stays live here: a v2 recipe requires
// flac_mp3_only, and validate() refuses anything else (v2 gates recipe
// saves in both settings.py and save_download_policy).

/// Lossy bitrate bound limits (v2 `_QUALITY_KBPS_MIN/MAX`).
pub const QUALITY_KBPS_MIN: i64 = 16;
/// Lossy bitrate bound limits (v2 `_QUALITY_KBPS_MIN/MAX`).
pub const QUALITY_KBPS_MAX: i64 = 2048;

/// One ordered, closed format-quality recipe entry. Unknown keys are
/// rejected at decode so a future setting cannot silently become a
/// different recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityRecipeEntry {
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

/// Canonical MP3 standard bounds: (min, target, max).
fn mp3_standard_bounds(quality: &str) -> Option<(i64, i64, Option<i64>)> {
    match quality {
        "below_192" => Some((16, 128, Some(191))),
        "192_255" => Some((192, 192, Some(255))),
        "256_319" => Some((256, 256, Some(319))),
        "320_plus" => Some((320, 320, None)),
        _ => None,
    }
}

fn is_mp3_quality(quality: &str) -> bool {
    matches!(
        quality,
        "below_192" | "192_255" | "256_319" | "320_plus" | "custom"
    )
}

fn is_flac_quality(quality: &str) -> bool {
    matches!(
        quality,
        "cd" | "24_48" | "24_96" | "24_192" | "hi_res" | "custom"
    )
}

fn recipe_error(reason: String) -> ConfigError {
    ConfigError::Validation {
        section: DownloadPolicy::KEY,
        field: "quality_recipe",
        reason,
    }
}

impl QualityRecipeEntry {
    /// Strict per-entry validation (v2 `_validate_recipe_entry_fields`).
    pub fn validate_entry(&self) -> Result<(), ConfigError> {
        if self.format != "flac" && self.format != "mp3" {
            return Err(recipe_error(format!(
                "unsupported quality recipe format: {:?}",
                self.format
            )));
        }
        if self.format == "mp3" {
            if !is_mp3_quality(&self.quality) {
                return Err(recipe_error(format!(
                    "unsupported mp3 quality recipe value: {:?}",
                    self.quality
                )));
            }
            if self.bit_depth.is_some() || self.sample_rate_hz.is_some() {
                return Err(recipe_error(
                    "MP3 recipe entries cannot define FLAC resolution".to_owned(),
                ));
            }
            if self.quality == "custom" {
                let (Some(minimum), Some(target), Some(maximum)) = (
                    self.min_bitrate_kbps,
                    self.target_bitrate_kbps,
                    self.max_bitrate_kbps,
                ) else {
                    return Err(recipe_error(
                        "custom MP3 recipe requires min, target, and max bitrates".to_owned(),
                    ));
                };
                for (name, value) in [
                    ("min_bitrate_kbps", minimum),
                    ("target_bitrate_kbps", target),
                    ("max_bitrate_kbps", maximum),
                ] {
                    if !(QUALITY_KBPS_MIN..=QUALITY_KBPS_MAX).contains(&value) {
                        return Err(recipe_error(format!(
                            "{name} must be an integer between {QUALITY_KBPS_MIN} and {QUALITY_KBPS_MAX}"
                        )));
                    }
                }
                if !(minimum <= target && target <= maximum) {
                    return Err(recipe_error(
                        "custom MP3 recipe requires min_bitrate_kbps <= \
                         target_bitrate_kbps <= max_bitrate_kbps"
                            .to_owned(),
                    ));
                }
                return Ok(());
            }
            let Some((exp_min, exp_target, exp_max)) = mp3_standard_bounds(&self.quality) else {
                return Err(recipe_error(format!(
                    "unsupported mp3 quality recipe value: {:?}",
                    self.quality
                )));
            };
            let actual = (
                self.min_bitrate_kbps,
                self.target_bitrate_kbps,
                self.max_bitrate_kbps,
            );
            if actual != (None, None, None) && actual != (Some(exp_min), Some(exp_target), exp_max)
            {
                return Err(recipe_error(
                    "standard MP3 recipe fields must use their canonical bounds".to_owned(),
                ));
            }
            return Ok(());
        }
        if !is_flac_quality(&self.quality) {
            return Err(recipe_error(format!(
                "unsupported flac quality recipe value: {:?}",
                self.quality
            )));
        }
        if self.min_bitrate_kbps.is_some()
            || self.target_bitrate_kbps.is_some()
            || self.max_bitrate_kbps.is_some()
        {
            return Err(recipe_error(
                "FLAC recipe entries cannot define a bitrate".to_owned(),
            ));
        }
        if self.quality == "custom" {
            let (Some(depth), Some(rate)) = (self.bit_depth, self.sample_rate_hz) else {
                return Err(recipe_error(
                    "custom FLAC recipe requires bit_depth and sample_rate_hz".to_owned(),
                ));
            };
            if !(1..=64).contains(&depth) {
                return Err(recipe_error(
                    "bit_depth must be an integer between 1 and 64".to_owned(),
                ));
            }
            if !(8000..=768000).contains(&rate) {
                return Err(recipe_error(
                    "sample_rate_hz must be an integer between 8000 and 768000".to_owned(),
                ));
            }
            return Ok(());
        }
        if self.bit_depth.is_some() || self.sample_rate_hz.is_some() {
            return Err(recipe_error(
                "standard FLAC recipe entries cannot define exact resolution".to_owned(),
            ));
        }
        Ok(())
    }

    /// Fill canonical MP3 standard bounds (v2 canonicalization).
    fn canonicalize(&mut self) {
        if self.format == "mp3"
            && self.quality != "custom"
            && let Some((min, target, max)) = mp3_standard_bounds(&self.quality)
        {
            self.min_bitrate_kbps = Some(min);
            self.target_bitrate_kbps = Some(target);
            self.max_bitrate_kbps = max;
        }
    }
}

fn intervals_overlap(
    left_min: i64,
    left_max: Option<i64>,
    right_min: i64,
    right_max: Option<i64>,
) -> bool {
    (left_max.is_none_or(|max| right_min <= max)) && (right_max.is_none_or(|max| left_min <= max))
}

/// List-level recipe validation (v2 `validate_quality_recipe`): standard
/// duplicates, MP3 range overlaps, and custom FLAC duplicates rejected. An
/// empty recipe is valid (a v1 policy); only submitted entries are checked.
pub fn validate_quality_recipe(entries: &[QualityRecipeEntry]) -> Result<(), ConfigError> {
    for entry in entries {
        entry.validate_entry()?;
    }
    let mut seen_standard = Vec::new();
    let mut mp3_ranges: Vec<(i64, Option<i64>)> = Vec::new();
    let mut flac_custom_pairs = Vec::new();
    for entry in entries {
        if entry.quality != "custom" {
            let key = (entry.format.clone(), entry.quality.clone());
            if seen_standard.contains(&key) {
                return Err(recipe_error(format!(
                    "quality_recipe contains duplicate {}/{}",
                    entry.format, entry.quality
                )));
            }
            seen_standard.push(key);
        }
        if entry.format == "mp3" {
            let mut bounds = (entry.min_bitrate_kbps, entry.max_bitrate_kbps);
            if entry.quality != "custom"
                && bounds == (None, None)
                && let Some((min, _, max)) = mp3_standard_bounds(&entry.quality)
            {
                bounds = (Some(min), max);
            }
            let (Some(minimum), maximum) = bounds else {
                return Err(recipe_error(
                    "MP3 recipe entry is missing its bitrate bounds".to_owned(),
                ));
            };
            for (existing_min, existing_max) in &mp3_ranges {
                if intervals_overlap(minimum, maximum, *existing_min, *existing_max) {
                    if entry.quality == "custom" {
                        return Err(recipe_error(
                            "custom MP3 quality recipe overlaps another MP3 range".to_owned(),
                        ));
                    }
                    return Err(recipe_error("MP3 quality recipe ranges overlap".to_owned()));
                }
            }
            mp3_ranges.push((minimum, maximum));
        } else if entry.quality == "custom"
            && let (Some(depth), Some(rate)) = (entry.bit_depth, entry.sample_rate_hz)
        {
            if flac_custom_pairs.contains(&(depth, rate)) {
                return Err(recipe_error(
                    "quality_recipe contains duplicate custom FLAC resolution".to_owned(),
                ));
            }
            flac_custom_pairs.push((depth, rate));
        }
    }
    Ok(())
}

/// Download policy: quality tiers, recipe v2, timeouts, retry, retention,
/// recycle bin, quotas, and the upgrade scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadPolicy {
    /// Worst accepted tier label.
    pub quality_min: String,
    /// Best accepted tier label.
    pub quality_max: String,
    /// Reject non-FLAC/MP3 candidates. A v2 recipe requires this on: v2
    /// gates recipe saves on it, the quality snapshot carries it, and the
    /// matcher, free-music, and orchestrator consumers all read it.
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
    pub quality_recipe: Vec<QualityRecipeEntry>,
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
}

impl Default for DownloadPolicy {
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
        }
    }
}

fn validation(section: &'static str, field: &'static str, reason: String) -> ConfigError {
    ConfigError::Validation {
        section,
        field,
        reason,
    }
}

impl DownloadPolicy {
    fn heal_tiers(&mut self) {
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
        if tier_rank(&self.quality_cutoff).is_none() {
            self.quality_cutoff = self.quality_max.clone();
        }
        let cutoff = tier_rank(&self.quality_cutoff).unwrap_or(max_rank);
        let floor = tier_rank(&self.quality_min).unwrap_or(0);
        let ceiling = tier_rank(&self.quality_max).unwrap_or(4);
        if cutoff < floor {
            self.quality_cutoff = self.quality_min.clone();
        } else if cutoff > ceiling {
            self.quality_cutoff = self.quality_max.clone();
        }
    }
}

impl Section for DownloadPolicy {
    const KEY: &'static str = "download_policy";

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
        check_range(
            key,
            "usenet_min_release_age_minutes",
            self.usenet_min_release_age_minutes,
            0,
            1440,
        )?;
        check_range(key, "max_size_mb", self.max_size_mb, 0, 1_000_000)?;
        check_range(
            key,
            "usenet_retention_days",
            self.usenet_retention_days,
            0,
            100_000,
        )?;
        check_range(
            key,
            "recycle_retention_days",
            self.recycle_retention_days,
            1,
            3650,
        )?;
        check_range(
            key,
            "max_library_size_gb",
            self.max_library_size_gb,
            0,
            1_000_000,
        )?;
        check_range(
            key,
            "default_request_quota_count",
            self.default_request_quota_count,
            0,
            100_000,
        )?;
        check_range(
            key,
            "default_request_quota_days",
            self.default_request_quota_days,
            1,
            3650,
        )?;
        check_range(
            key,
            "default_storage_quota_gb",
            self.default_storage_quota_gb,
            0,
            1_000_000,
        )?;
        check_range(
            key,
            "background_upgrade_scan_interval_hours",
            self.background_upgrade_scan_interval_hours,
            1,
            720,
        )?;
        check_range(
            key,
            "background_upgrade_max_per_run",
            self.background_upgrade_max_per_run,
            1,
            100,
        )?;
        if tier_rank(&self.quality_min).is_none() {
            return Err(validation(
                key,
                "quality_min",
                format!("unknown quality tier {:?}", self.quality_min),
            ));
        }
        if tier_rank(&self.quality_max).is_none() {
            return Err(validation(
                key,
                "quality_max",
                format!("unknown quality tier {:?}", self.quality_max),
            ));
        }
        let min_rank = tier_rank(&self.quality_min).unwrap_or(0);
        let max_rank = tier_rank(&self.quality_max).unwrap_or(0);
        if min_rank > max_rank {
            return Err(validation(
                key,
                "quality_min",
                format!(
                    "quality_min {:?} ranks above quality_max {:?}",
                    self.quality_min, self.quality_max
                ),
            ));
        }
        if !self.quality_preference_order.is_empty() {
            let expected = derive_default_order(&self.quality_min, &self.quality_max);
            let mut submitted = self.quality_preference_order.clone();
            let mut want = expected.clone();
            submitted.sort();
            want.sort();
            if submitted != want || self.quality_preference_order.len() != expected.len() {
                return Err(validation(
                    key,
                    "quality_preference_order",
                    format!(
                        "must contain exactly the accepted tiers {expected:?} \
                         (min={}, max={}), got {:?}",
                        self.quality_min, self.quality_max, self.quality_preference_order
                    ),
                ));
            }
        }
        if !self.quality_recipe.is_empty() && !self.flac_mp3_only {
            return Err(validation(
                key,
                "quality_recipe",
                "quality_recipe requires flac_mp3_only=true; \
                 replace the non-convertible policy first"
                    .to_owned(),
            ));
        }
        validate_quality_recipe(&self.quality_recipe)?;
        if !matches!(
            self.lossless_preference.as_str(),
            "cd" | "24_48" | "24_96" | "24_192" | "highest"
        ) {
            return Err(validation(
                key,
                "lossless_preference",
                format!(
                    "invalid lossless_preference: {:?}",
                    self.lossless_preference
                ),
            ));
        }
        if !matches!(
            self.unknown_quality_behavior.as_str(),
            "reject" | "review" | "allow_as_fallback"
        ) {
            return Err(validation(
                key,
                "unknown_quality_behavior",
                format!(
                    "invalid unknown_quality_behavior: {:?}",
                    self.unknown_quality_behavior
                ),
            ));
        }
        if !matches!(
            self.source_selection_mode.as_str(),
            "source_first" | "quality_first"
        ) {
            return Err(validation(
                key,
                "source_selection_mode",
                format!(
                    "invalid source_selection_mode: {:?}",
                    self.source_selection_mode
                ),
            ));
        }
        for (field, value) in [
            (
                "preferred_lossy_bitrate_kbps",
                self.preferred_lossy_bitrate_kbps,
            ),
            ("lossy_min_bitrate_kbps", self.lossy_min_bitrate_kbps),
            ("lossy_max_bitrate_kbps", self.lossy_max_bitrate_kbps),
        ] {
            if let Some(bound) = value
                && !(QUALITY_KBPS_MIN..=QUALITY_KBPS_MAX).contains(&bound)
            {
                return Err(validation(
                    key,
                    field,
                    format!(
                        "{field} must be between {QUALITY_KBPS_MIN} and {QUALITY_KBPS_MAX} kbps"
                    ),
                ));
            }
        }
        if let (Some(minimum), Some(maximum)) =
            (self.lossy_min_bitrate_kbps, self.lossy_max_bitrate_kbps)
            && minimum > maximum
        {
            return Err(validation(
                key,
                "lossy_min_bitrate_kbps",
                "lossy_min_bitrate_kbps exceeds lossy_max_bitrate_kbps".to_owned(),
            ));
        }
        if let Some(target) = self.preferred_lossy_bitrate_kbps {
            let below = self
                .lossy_min_bitrate_kbps
                .is_some_and(|minimum| target < minimum);
            let above = self
                .lossy_max_bitrate_kbps
                .is_some_and(|maximum| target > maximum);
            if below || above {
                return Err(validation(
                    key,
                    "preferred_lossy_bitrate_kbps",
                    "preferred_lossy_bitrate_kbps must sit inside \
                     [lossy_min_bitrate_kbps, lossy_max_bitrate_kbps]"
                        .to_owned(),
                ));
            }
        }
        if let Some(depth) = self.lossless_max_bit_depth
            && !(1..=64).contains(&depth)
        {
            return Err(validation(
                key,
                "lossless_max_bit_depth",
                "lossless_max_bit_depth must be 1..64 bits".to_owned(),
            ));
        }
        if let Some(rate) = self.lossless_max_sample_rate_hz
            && !(8000..=768000).contains(&rate)
        {
            return Err(validation(
                key,
                "lossless_max_sample_rate_hz",
                "lossless_max_sample_rate_hz must be 8000..768000 Hz".to_owned(),
            ));
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.heal_tiers();
        let accepted = derive_default_order(&self.quality_min, &self.quality_max);
        let mut order = self.quality_preference_order.clone();
        let mut want = accepted.clone();
        order.sort();
        want.sort();
        if order != want || self.quality_preference_order.len() != accepted.len() {
            self.quality_preference_order = accepted;
        }
        if !matches!(
            self.lossless_preference.as_str(),
            "cd" | "24_48" | "24_96" | "24_192" | "highest"
        ) {
            self.lossless_preference = "highest".to_owned();
        }
        if !matches!(
            self.unknown_quality_behavior.as_str(),
            "reject" | "review" | "allow_as_fallback"
        ) {
            self.unknown_quality_behavior = "allow_as_fallback".to_owned();
        }
        if !matches!(
            self.source_selection_mode.as_str(),
            "source_first" | "quality_first"
        ) {
            self.source_selection_mode = "source_first".to_owned();
        }
        for bound in [
            &mut self.preferred_lossy_bitrate_kbps,
            &mut self.lossy_min_bitrate_kbps,
            &mut self.lossy_max_bitrate_kbps,
        ] {
            if bound.is_some_and(|value| !(QUALITY_KBPS_MIN..=QUALITY_KBPS_MAX).contains(&value)) {
                *bound = None;
            }
        }
        if let (Some(minimum), Some(maximum)) =
            (self.lossy_min_bitrate_kbps, self.lossy_max_bitrate_kbps)
            && minimum > maximum
        {
            self.lossy_min_bitrate_kbps = None;
            self.lossy_max_bitrate_kbps = None;
        }
        if self
            .lossless_max_bit_depth
            .is_some_and(|depth| !(1..=64).contains(&depth))
        {
            self.lossless_max_bit_depth = None;
        }
        if self
            .lossless_max_sample_rate_hz
            .is_some_and(|rate| !(8000..=768000).contains(&rate))
        {
            self.lossless_max_sample_rate_hz = None;
        }
        for entry in &mut self.quality_recipe {
            entry.canonicalize();
        }
    }
}

// --- musicbrainz_settings -------------------------------------------------
// Dropped transients: pending_brainzmash, source_quarantined,
// quarantine_reason. Kept: source_mode, api_url, rate_limit,
// concurrent_searches, community_acknowledged, selected_source_mode,
// source_id, generation, active_brainzmash.

/// Official MusicBrainz API base (v2 `OFFICIAL_MB_API_BASE`).
pub const OFFICIAL_MB_API_BASE: &str = "https://musicbrainz.org/ws/2";
/// Server-owned BrainzMash endpoint. Never accepted from a client.
pub const BRAINZMASH_ENDPOINT: &str = "https://api.brainzmash.cc/ws/2";
/// Forced BrainzMash throughput (v2 `_BRAINZMASH_RATE_LIMIT`).
pub const BRAINZMASH_RATE_LIMIT: f64 = 10.0;
/// Forced BrainzMash concurrency (v2 `_BRAINZMASH_CONCURRENT_SEARCHES`).
pub const BRAINZMASH_CONCURRENT_SEARCHES: i64 = 1;
/// Official-host ceilings (never raised).
pub const OFFICIAL_MB_RATE_LIMIT: f64 = 1.0;
/// Official-host ceilings (never raised).
pub const OFFICIAL_MB_CONCURRENT_SEARCHES: i64 = 6;
/// Widest allowed off-official throughput (0 = Unlimited sentinel).
pub const MAX_MB_RATE_LIMIT: f64 = 500.0;
/// Widest allowed off-official concurrency.
pub const MAX_MB_CONCURRENT_SEARCHES: i64 = 64;

/// MusicBrainz source tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MbSourceMode {
    /// musicbrainz.org, hard 1 req/s ceiling.
    Official,
    /// User-owned mirror.
    Mirror,
    /// Community infrastructure.
    Community,
    /// Built-in server-owned source (the v2 effective default).
    #[default]
    Brainzmash,
}

/// Active BrainzMash binding (kept; the pending proposal is transient).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainzmashActiveBinding {
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

impl Default for BrainzmashActiveBinding {
    fn default() -> Self {
        Self {
            endpoint: BRAINZMASH_ENDPOINT.to_owned(),
            access_revision: String::new(),
            source_id: String::new(),
            generation: 0,
            disclosure_version: "brainzmash-v1".to_owned(),
            consented: false,
            verified: false,
        }
    }
}

/// MusicBrainz connection settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MusicBrainzSettings {
    /// Active source tier.
    pub source_mode: MbSourceMode,
    /// API base (canonicalized for official/brainzmash).
    pub api_url: String,
    /// Requests per second (0 = Unlimited, off-official only).
    pub rate_limit: f64,
    /// Concurrent searches.
    pub concurrent_searches: i64,
    /// Community-tier disclosure acknowledged.
    pub community_acknowledged: bool,
    /// Last tier the admin chose.
    pub selected_source_mode: MbSourceMode,
    /// Source identity.
    pub source_id: String,
    /// Source generation.
    pub generation: i64,
    /// Active BrainzMash binding, if any.
    pub active_brainzmash: Option<BrainzmashActiveBinding>,
    /// True when the official-host clamp forced values down (or lifted a
    /// 0 sentinel up). Rendered, never refused.
    pub clamped_to_official_limits: bool,
}

impl Default for MusicBrainzSettings {
    fn default() -> Self {
        Self {
            source_mode: MbSourceMode::Brainzmash,
            api_url: BRAINZMASH_ENDPOINT.to_owned(),
            rate_limit: BRAINZMASH_RATE_LIMIT,
            concurrent_searches: BRAINZMASH_CONCURRENT_SEARCHES,
            community_acknowledged: false,
            selected_source_mode: MbSourceMode::Brainzmash,
            source_id: uuid::Uuid::new_v4().to_string(),
            generation: 1,
            active_brainzmash: None,
            clamped_to_official_limits: false,
        }
    }
}

/// Public MusicBrainz origins for transport-rate policy (v2
/// `_MB_RATE_POLICY_PUBLIC_ORIGINS`, both schemes so insecure transport
/// cannot bypass the ceiling). Never identity proof.
const MB_RATE_POLICY_PUBLIC_ORIGINS: [&str; 8] = [
    "http://musicbrainz.org",
    "http://musicbrainz.org:80",
    "http://www.musicbrainz.org",
    "http://www.musicbrainz.org:80",
    "https://musicbrainz.org",
    "https://musicbrainz.org:443",
    "https://www.musicbrainz.org",
    "https://www.musicbrainz.org:443",
];

/// Privacy-safe origin label: `scheme://host[:port]`, lowercased, no
/// credentials or path (v2 `normalize_mb_source_label`, without a URL
/// crate). Returns "" for non-HTTP(S) input.
#[must_use]
pub fn normalize_mb_source_label(url: &str) -> String {
    let trimmed = url.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return String::new();
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let authority = rest.split('/').next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    if host_port.is_empty() {
        return String::new();
    }
    let (host, port) = if host_port.starts_with('[') {
        match host_port.split_once("]:") {
            Some((host, port)) => (format!("{host}]"), Some(port)),
            None => {
                if host_port.ends_with(']') {
                    (host_port.to_owned(), None)
                } else {
                    return String::new();
                }
            }
        }
    } else {
        match host_port.split_once(':') {
            Some((host, port)) => (host.to_owned(), Some(port)),
            None => (host_port.to_owned(), None),
        }
    };
    if host.is_empty() || host.contains(':') && !host.starts_with('[') {
        return String::new();
    }
    let mut label = format!("{scheme}://{}", host.to_ascii_lowercase());
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return String::new();
        }
        label.push(':');
        label.push_str(port);
    }
    label
}

/// Whether the URL sits on a public MusicBrainz origin (rate ceiling
/// applies).
#[must_use]
pub fn is_mb_rate_policy_public_host(url: &str) -> bool {
    MB_RATE_POLICY_PUBLIC_ORIGINS.contains(&normalize_mb_source_label(url).as_str())
}

impl MusicBrainzSettings {
    fn canonicalize_urls(&mut self) {
        self.api_url = self.api_url.trim().to_owned();
        match self.source_mode {
            MbSourceMode::Official => self.api_url = OFFICIAL_MB_API_BASE.to_owned(),
            MbSourceMode::Brainzmash => self.api_url = BRAINZMASH_ENDPOINT.to_owned(),
            MbSourceMode::Mirror | MbSourceMode::Community => {}
        }
        while self.api_url.ends_with('/') && self.api_url.len() > 1 {
            self.api_url.pop();
        }
    }
}

impl Section for MusicBrainzSettings {
    const KEY: &'static str = "musicbrainz_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        let key = Self::KEY;
        if !self.rate_limit.is_finite() {
            return Err(validation(
                key,
                "rate_limit",
                "rate_limit must be finite".to_owned(),
            ));
        }
        if self.concurrent_searches < 1 {
            return Err(validation(
                key,
                "concurrent_searches",
                "concurrent_searches must be at least 1".to_owned(),
            ));
        }
        match self.source_mode {
            MbSourceMode::Official | MbSourceMode::Brainzmash => {}
            MbSourceMode::Mirror | MbSourceMode::Community => {
                let trimmed = self.api_url.trim();
                if trimmed.is_empty()
                    || !(trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                {
                    return Err(validation(
                        key,
                        "api_url",
                        "api_url must be an absolute HTTP(S) URL for non-official sources"
                            .to_owned(),
                    ));
                }
            }
        }
        if !is_mb_rate_policy_public_host(&self.api_url) {
            if self.rate_limit < 0.0
                || (self.rate_limit > 0.0 && self.rate_limit < 0.1)
                || self.rate_limit > MAX_MB_RATE_LIMIT
            {
                return Err(validation(
                    key,
                    "rate_limit",
                    format!(
                        "rate_limit must be 0 (unlimited) or between 0.1 and {MAX_MB_RATE_LIMIT}"
                    ),
                ));
            }
            if self.concurrent_searches > MAX_MB_CONCURRENT_SEARCHES {
                return Err(validation(
                    key,
                    "concurrent_searches",
                    format!(
                        "concurrent_searches must be between 1 and {MAX_MB_CONCURRENT_SEARCHES}"
                    ),
                ));
            }
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.canonicalize_urls();
        self.clamped_to_official_limits = false;
        if self.source_mode == MbSourceMode::Brainzmash {
            self.rate_limit = BRAINZMASH_RATE_LIMIT;
            self.concurrent_searches = BRAINZMASH_CONCURRENT_SEARCHES;
        }
        if is_mb_rate_policy_public_host(&self.api_url) {
            let before = (self.rate_limit, self.concurrent_searches);
            self.rate_limit = self.rate_limit.min(OFFICIAL_MB_RATE_LIMIT);
            self.concurrent_searches = self
                .concurrent_searches
                .min(OFFICIAL_MB_CONCURRENT_SEARCHES);
            if self.rate_limit <= 0.0 {
                self.rate_limit = OFFICIAL_MB_RATE_LIMIT;
            }
            self.clamped_to_official_limits = before != (self.rate_limit, self.concurrent_searches);
        }
    }
}

// --- lastfm_settings (per-user credentials only) --------------------------
// The admin-global credential pair is deleted; the section keeps only the
// master switch. The v2 importer decrypts sealed lastfm secrets, then
// drops them, and the per-user store behind LASTFM_SECRET_MASK holds the
// credentials.

/// Last.fm settings: master switch only (per-user credentials live in
/// the per-user store, not here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LastFmSettings {
    /// Master switch for Last.fm fan-out.
    pub enabled: bool,
}

impl Section for LastFmSettings {
    const KEY: &'static str = "lastfm_settings";
}

// --- lyrics_settings (read-path lyrics provider) ---------------------------
// The master switch for live LRCLIB lyrics on the library read path. This is
// the read-path provider toggle, not the library-management write block
// (`LyricsManagementSettings`): with this off, lyrics reads stay on the
// empty memory port and the server makes no lyrics network calls.

/// Lyrics settings: master switch for the live LRCLIB read path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LyricsSettings {
    /// Master switch for live LRCLIB lyrics fan-out.
    pub enabled: bool,
}

impl Section for LyricsSettings {
    const KEY: &'static str = "lyrics_settings";
}

// --- plugins --------------------------------------------------------------
// Secret-flagged values are encrypted at rest (v2 stored them plaintext).
// Which keys are secret comes from each plugin manifest at runtime, so the
// section itself is a plain map and `ConfigStore` offers secret-aware
// plugin helpers that take the manifest's secret-key set.

/// One plugin's persisted state. Disabled by default: dropping a folder
/// in must never run code.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PluginConfig {
    /// Enabled switch.
    pub enabled: bool,
    /// Settings map; secret-flagged values are ciphertext at rest.
    pub settings: std::collections::HashMap<String, String>,
}

impl std::fmt::Debug for PluginConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The map can hold decrypted secrets in the get_plugin_raw form,
        // and this struct cannot tell secret keys from plain ones without
        // the plugin manifest, so every value redacts; only keys print.
        let redacted: std::collections::BTreeMap<&str, &str> = self
            .settings
            .keys()
            .map(|key| (key.as_str(), "[redacted]"))
            .collect();
        f.debug_struct("PluginConfig")
            .field("enabled", &self.enabled)
            .field("settings", &redacted)
            .finish()
    }
}

/// The `plugins` section: per-plugin state by plugin name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Plugins(pub std::collections::HashMap<String, PluginConfig>);

impl Section for Plugins {
    const KEY: &'static str = "plugins";
}

// --- _internal ------------------------------------------------------------
// Kept: plex_client_id, droppedneedle_device_id,
// brainzmash_consent_admin. Dropped as recomputed/derived: audiodb sweep
// cursor + completion, release_type_policy_revision,
// musicbrainz_settings_revision, official_source_selected.

/// Typed subset of the `_internal` section. Only the kept keys exist;
/// anything else in the file is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InternalState {
    /// Stable Plex client id.
    pub plex_client_id: Option<String>,
    /// Stable Jellyfin device id.
    pub droppedneedle_device_id: Option<String>,
    /// Admin id that consented to BrainzMash.
    pub brainzmash_consent_admin: Option<String>,
}

impl Section for InternalState {
    const KEY: &'static str = "_internal";
}

// --- library_management (secret-free on purpose) ------------------------

/// Settings schema version (v2 `LIBRARY_MANAGEMENT_SCHEMA_VERSION`).
pub const LIBRARY_MANAGEMENT_SCHEMA_VERSION: i64 = 1;
/// Default organization naming script (v2 Picard organizer id).
pub const PICARD_ORGANIZER_NAMING_SCRIPT_ID: &str = "69202666-cb88-52b0-bac2-0afc62b1e909";

/// Default sidecar patterns (v2 `DEFAULT_SIDECAR_PATTERNS`).
pub const DEFAULT_SIDECAR_PATTERNS: [&str; 27] = [
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "cover.webp",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "front.jpg",
    "front.png",
    "back.jpg",
    "back.jpeg",
    "back.png",
    "back.webp",
    "booklet*.jpg",
    "booklet*.jpeg",
    "booklet*.png",
    "booklet*.webp",
    "medium*.jpg",
    "medium*.jpeg",
    "medium*.png",
    "medium*.webp",
    "*.cue",
    "*.log",
    "*.lrc",
    "*.m3u",
    "*.m3u8",
    "*.pls",
];

/// Tag-field write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldMode {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenreMode {
    /// Overwrite.
    #[default]
    Replace,
    /// Merge values.
    Merge,
    /// Fill when empty.
    FillMissing,
}

/// Genre source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenreSource {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkProvider {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkImageType {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkOutputFormat {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkDownloadSize {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtistStandardization {
    /// As credited.
    #[default]
    Credited,
    /// Accepted variations.
    Variations,
    /// Canonical name.
    Canonical,
}

/// Credited relationship type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipType {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCleanupMode {
    /// Leave sources alone.
    Keep,
    /// Remove after a confirmed move.
    #[default]
    RemoveAfterConfirmedMove,
}

/// ID3 version for MP3 writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Id3Version {
    /// ID3v2.4.
    #[serde(rename = "2.4")]
    #[default]
    V24,
    /// ID3v2.3.
    #[serde(rename = "2.3")]
    V23,
}

/// APEv2 policy for MP3 writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mp3ApePolicy {
    /// Preserve APEv2 tags.
    #[default]
    Preserve,
    /// Remove APEv2 tags.
    Remove,
}

/// Raw-AAC tag policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawAacTagPolicy {
    /// Write APEv2.
    #[default]
    SaveApev2,
    /// Write nothing.
    DoNotWrite,
    /// Remove APEv2.
    RemoveApev2,
}

/// WAV tag policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WavTagPolicy {
    /// ID3 chunk.
    #[default]
    Id3,
    /// RIFF INFO chunk.
    RiffInfo,
    /// Leave existing tags alone.
    PreserveExisting,
}

/// ID3 text encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Id3TextEncoding {
    /// UTF-8.
    #[default]
    Utf8,
    /// UTF-16.
    Utf16,
}

/// Unicode normalization for paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UnicodeNormalization {
    /// NFC.
    #[default]
    NFC,
    /// NFKC.
    NFKC,
}

/// Extension case policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCase {
    /// Keep as-is.
    #[default]
    Preserve,
    /// Lowercase.
    Lower,
    /// Uppercase.
    Upper,
}

/// ReplayGain write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayGainMode {
    /// Leave tags alone.
    #[default]
    Preserve,
    /// Fill missing tags.
    FillMissing,
    /// Overwrite tags.
    Replace,
}

/// Multi-disc naming mode for a root override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiDiscNamingMode {
    /// Inherit the profile.
    #[default]
    Inherit,
    /// Standard naming.
    Standard,
    /// Naming script.
    Script,
}

/// One managed tag field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagedField {
    /// Field name.
    pub field: String,
    /// Write mode.
    pub mode: FieldMode,
    /// Clear when the canonical value is missing.
    pub clear_when_canonical_missing: bool,
}

impl Default for ManagedField {
    fn default() -> Self {
        Self {
            field: String::new(),
            mode: FieldMode::Replace,
            clear_when_canonical_missing: false,
        }
    }
}

/// Artist-credit handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistCreditSettings {
    /// Standardization level.
    pub standardization: ArtistStandardization,
    /// Translate names.
    pub translate_names: bool,
    /// Preferred locales.
    pub preferred_locales: Vec<String>,
}

impl Default for ArtistCreditSettings {
    fn default() -> Self {
        Self {
            standardization: ArtistStandardization::Credited,
            translate_names: false,
            preferred_locales: Vec::new(),
        }
    }
}

/// Relationship-credit handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RelationshipCreditSettings {
    /// Master switch.
    pub enabled: bool,
    /// Credited relationship types.
    pub types: Vec<RelationshipType>,
}

impl Default for RelationshipCreditSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            types: vec![
                RelationshipType::Composer,
                RelationshipType::Lyricist,
                RelationshipType::Conductor,
                RelationshipType::Performer,
                RelationshipType::Arranger,
                RelationshipType::Remixer,
                RelationshipType::Producer,
            ],
        }
    }
}

/// Format-compatibility handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatCompatibilitySettings {
    /// ID3 version for MP3 writes.
    pub id3_version: Id3Version,
    /// ID3v2.3 multi-value join delimiter.
    pub id3v23_join_delimiter: String,
    /// ID3 text encoding.
    pub id3_text_encoding: Id3TextEncoding,
    /// Strip ID3 chunks from FLAC.
    pub remove_id3_from_flac: bool,
    /// APEv2 policy for MP3.
    pub mp3_apev2_policy: Mp3ApePolicy,
    /// Raw-AAC tag policy.
    pub raw_aac_tag_policy: RawAacTagPolicy,
    /// WAV tag policy.
    pub wav_tag_policy: WavTagPolicy,
    /// Primary genre only for constrained formats.
    pub constrained_genres_primary_only: bool,
}

impl Default for FormatCompatibilitySettings {
    fn default() -> Self {
        Self {
            id3_version: Id3Version::V24,
            id3v23_join_delimiter: "; ".to_owned(),
            id3_text_encoding: Id3TextEncoding::Utf8,
            remove_id3_from_flac: false,
            mp3_apev2_policy: Mp3ApePolicy::Preserve,
            raw_aac_tag_policy: RawAacTagPolicy::SaveApev2,
            wav_tag_policy: WavTagPolicy::Id3,
            constrained_genres_primary_only: false,
        }
    }
}

/// Metadata management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MetadataManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Managed fields.
    pub fields: Vec<ManagedField>,
    /// Artist credits.
    pub artist_credits: ArtistCreditSettings,
    /// Relationship credits.
    pub relationships: RelationshipCreditSettings,
    /// Tagging script ids.
    pub tagging_script_ids: Vec<String>,
    /// Fields never touched.
    pub preserve_fields: Vec<String>,
    /// Scrub unmanaged tags.
    pub scrub_unmanaged_tags: bool,
    /// Keep embedded art during a scrub.
    pub preserve_embedded_art_during_scrub: bool,
    /// Format compatibility.
    pub format_compatibility: FormatCompatibilitySettings,
}

/// One genre alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GenreAlias {
    /// Source label.
    pub source: String,
    /// Target label.
    pub target: String,
}

/// Genre management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenreManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Write mode.
    pub mode: GenreMode,
    /// Genre sources.
    pub sources: Vec<GenreSource>,
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
    pub aliases: Vec<GenreAlias>,
    /// Preferred casing.
    pub preferred_casing: Vec<String>,
    /// Primary genre only for constrained formats.
    pub write_primary_only_for_constrained_formats: bool,
}

impl Default for GenreManagementSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: GenreMode::Replace,
            sources: vec![GenreSource::Musicbrainz, GenreSource::Listenbrainz],
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtworkManagementSettings {
    /// Embed art in files.
    pub embedded_enabled: bool,
    /// Write external art files.
    pub external_enabled: bool,
    /// Provider order.
    pub providers: Vec<ArtworkProvider>,
    /// Approved art only.
    pub approved_only: bool,
    /// Download size.
    pub download_size: ArtworkDownloadSize,
    /// Local filename patterns.
    pub local_file_patterns: Vec<String>,
    /// Image types to fetch.
    pub image_types: Vec<ArtworkImageType>,
    /// Minimum width (0 any).
    pub minimum_width: i64,
    /// Minimum height (0 any).
    pub minimum_height: i64,
    /// Embedded size cap (0 uncapped).
    pub embedded_maximum_size: i64,
    /// Embedded output format.
    pub embedded_format: ArtworkOutputFormat,
    /// External size cap (0 uncapped).
    pub external_maximum_size: i64,
    /// External output format.
    pub external_format: ArtworkOutputFormat,
    /// Embedded front only.
    pub embedded_front_only: bool,
    /// External front only.
    pub external_front_only: bool,
    /// Never replace art with smaller art.
    pub never_replace_with_smaller: bool,
    /// Existing types never replaced.
    pub preserve_existing_types: Vec<ArtworkImageType>,
    /// External naming script id.
    pub external_naming_script_id: Option<String>,
    /// Overwrite external files.
    pub overwrite_external_files: bool,
}

impl Default for ArtworkManagementSettings {
    fn default() -> Self {
        Self {
            embedded_enabled: true,
            external_enabled: true,
            providers: vec![
                ArtworkProvider::CoverArtArchiveRelease,
                ArtworkProvider::CoverArtArchiveReleaseGroup,
                ArtworkProvider::LocalFiles,
                ArtworkProvider::Embedded,
            ],
            approved_only: true,
            download_size: ArtworkDownloadSize::Full,
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
            image_types: vec![ArtworkImageType::Front],
            minimum_width: 0,
            minimum_height: 0,
            embedded_maximum_size: 1200,
            embedded_format: ArtworkOutputFormat::Jpeg,
            external_maximum_size: 0,
            external_format: ArtworkOutputFormat::Original,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PathCompatibilitySettings {
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
    pub unicode_normalization: UnicodeNormalization,
    /// Extension case.
    pub extension_case: ExtensionCase,
    /// Honor the legacy Windows path limit.
    pub windows_legacy_path_limit: bool,
}

impl Default for PathCompatibilitySettings {
    fn default() -> Self {
        Self {
            windows_compatible: true,
            replace_non_ascii: false,
            replace_spaces_with_underscores: false,
            separator_replacement: "_".to_owned(),
            maximum_component_length: 240,
            maximum_path_length: 4096,
            unicode_normalization: UnicodeNormalization::NFC,
            extension_case: ExtensionCase::Preserve,
            windows_legacy_path_limit: false,
        }
    }
}

/// Organization management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrganizationManagementSettings {
    /// Rename files.
    pub rename_enabled: bool,
    /// Move files.
    pub move_enabled: bool,
    /// Naming script id.
    pub naming_script_id: String,
    /// Multi-disc naming script id.
    pub multi_disc_naming_script_id: Option<String>,
    /// Path compatibility.
    pub compatibility: PathCompatibilitySettings,
    /// Move sidecar files along.
    pub move_sidecars: bool,
    /// Sidecar filename patterns.
    pub sidecar_patterns: Vec<String>,
    /// Source cleanup after a confirmed move.
    pub source_cleanup: SourceCleanupMode,
    /// Remove newly empty directories.
    pub remove_empty_directories: bool,
}

impl Default for OrganizationManagementSettings {
    fn default() -> Self {
        Self {
            rename_enabled: true,
            move_enabled: true,
            naming_script_id: PICARD_ORGANIZER_NAMING_SCRIPT_ID.to_owned(),
            multi_disc_naming_script_id: None,
            compatibility: PathCompatibilitySettings::default(),
            move_sidecars: true,
            sidecar_patterns: DEFAULT_SIDECAR_PATTERNS
                .iter()
                .map(ToString::to_string)
                .collect(),
            source_cleanup: SourceCleanupMode::RemoveAfterConfirmedMove,
            remove_empty_directories: true,
        }
    }
}

/// File-behavior gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileBehaviorSettings {
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

impl Default for FileBehaviorSettings {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsManagementSettings {
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

impl Default for LyricsManagementSettings {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayGainManagementSettings {
    /// Master switch.
    pub enabled: bool,
    /// Write mode.
    pub mode: ReplayGainMode,
    /// Album-aware gain.
    pub album_aware: bool,
    /// Gain required for completion.
    pub required: bool,
}

impl Default for ReplayGainManagementSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ReplayGainMode::Preserve,
            album_aware: true,
            required: false,
        }
    }
}

/// Enrichment management block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EnrichmentManagementSettings {
    /// Lyrics.
    pub lyrics: LyricsManagementSettings,
    /// ReplayGain.
    pub replaygain: ReplayGainManagementSettings,
}

/// Catalog-identity policy (no file writes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct IdentityManagementSettings {
    /// Automatic edition acceptance.
    pub automatic_edition_acceptance_enabled: bool,
}

/// Post-publish notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileNotificationSettings {
    /// Refresh DroppedNeedle views.
    pub refresh_droppedneedle: bool,
    /// Refresh external servers.
    pub refresh_external_servers: bool,
}

impl Default for ProfileNotificationSettings {
    fn default() -> Self {
        Self {
            refresh_droppedneedle: true,
            refresh_external_servers: false,
        }
    }
}

/// One named management profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryManagementProfile {
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
    pub metadata: MetadataManagementSettings,
    /// Genre block.
    pub genres: GenreManagementSettings,
    /// Artwork block.
    pub artwork: ArtworkManagementSettings,
    /// Organization block.
    pub organization: OrganizationManagementSettings,
    /// File-behavior gates.
    pub file_behavior: FileBehaviorSettings,
    /// Enrichment block.
    pub enrichment: EnrichmentManagementSettings,
    /// Identity policy.
    pub identity: IdentityManagementSettings,
    /// Notifications.
    pub notification: ProfileNotificationSettings,
}

impl Default for LibraryManagementProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            preset_origin: None,
            preset_version: None,
            revision: String::new(),
            metadata: MetadataManagementSettings {
                enabled: true,
                preserve_embedded_art_during_scrub: true,
                ..MetadataManagementSettings::default()
            },
            genres: GenreManagementSettings::default(),
            artwork: ArtworkManagementSettings::default(),
            organization: OrganizationManagementSettings::default(),
            file_behavior: FileBehaviorSettings::default(),
            enrichment: EnrichmentManagementSettings::default(),
            identity: IdentityManagementSettings::default(),
            notification: ProfileNotificationSettings::default(),
        }
    }
}

/// One naming script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NamingScript {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TaggingScript {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LibraryManagementRootOverrides {
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
    pub source_cleanup: Option<SourceCleanupMode>,
    /// Timestamp preservation.
    pub preserve_timestamps: Option<bool>,
    /// Naming script id.
    pub naming_script_id: Option<String>,
    /// Multi-disc naming mode.
    pub multi_disc_naming_mode: MultiDiscNamingMode,
    /// Multi-disc naming script id.
    pub multi_disc_naming_script_id: Option<String>,
    /// Automatic edition acceptance.
    pub automatic_edition_acceptance_enabled: Option<bool>,
}

/// One root-to-profile assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LibraryManagementRootAssignment {
    /// Library root id.
    pub root_id: String,
    /// Assigned profile id.
    pub profile_id: Option<String>,
    /// Per-root overrides.
    pub overrides: Option<LibraryManagementRootOverrides>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalRefreshSettings {
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

impl Default for ExternalRefreshSettings {
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

/// Library management settings (secret-free on purpose).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryManagement {
    /// Settings schema version.
    pub schema_version: i64,
    /// Preset catalog version.
    pub preset_catalog_version: i64,
    /// Named profiles.
    pub profiles: Vec<LibraryManagementProfile>,
    /// Default profile id.
    pub default_profile_id: String,
    /// Root assignments.
    pub root_assignments: Vec<LibraryManagementRootAssignment>,
    /// Naming scripts.
    pub naming_scripts: Vec<NamingScript>,
    /// Tagging scripts.
    pub tagging_scripts: Vec<TaggingScript>,
    /// Undo retention in days.
    pub undo_retention_days: i64,
    /// Preview retention in hours.
    pub preview_retention_hours: i64,
    /// Recycle-bin path ("" disables).
    pub recycle_bin_path: String,
    /// External refresh.
    pub external_refresh: ExternalRefreshSettings,
}

impl Default for LibraryManagement {
    fn default() -> Self {
        Self {
            schema_version: LIBRARY_MANAGEMENT_SCHEMA_VERSION,
            preset_catalog_version: 0,
            profiles: Vec::new(),
            default_profile_id: String::new(),
            root_assignments: Vec::new(),
            naming_scripts: Vec::new(),
            tagging_scripts: Vec::new(),
            undo_retention_days: 90,
            preview_retention_hours: 24,
            recycle_bin_path: String::new(),
            external_refresh: ExternalRefreshSettings::default(),
        }
    }
}

impl Section for LibraryManagement {
    const KEY: &'static str = "library_management";
}
