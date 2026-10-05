//! Acquisition: quality tiers, the wanted watcher, source priority, the
//! Usenet backend, Free Music, store links, and the download policy.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Section, check_range, validation};
use crate::runtime_config::error::ConfigError;

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

// --- wanted ---------------------------------------------------------------

/// Wanted watcher toggles. Cadence stays code constants on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
/// saves only accept known values (the derived decode and `parse` are
/// both strict).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
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

// --- free_music / get_it --------------------------------------------------

/// Transcode target for remote playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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
