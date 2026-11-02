//! Acquisition: quality tiers, the wanted watcher, source priority, the
//! Usenet backend, store links, and the download policy.

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
}

impl Default for WantedWatcher {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_download_on_find: true,
            watch_partial_albums: true,
            max_checks_per_sweep: 3,
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
        )
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

// --- get_it --------------------------------------------------------

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
// Dropped from storage: quality_recipe_status / quality_recipe_error. They are
// read-only verdicts computed at GET time; persisting them would pin stale
// verdicts.
// A policy always holds a recipe, and the recipe owns the tier range:
// normalize() converts a row with none and sets the range from the recipe.

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
    /// Container.
    pub format: RecipeFormat,
    /// Quality id: one of the format's own, or `custom`.
    pub quality: RecipeQuality,
    /// Custom MP3 lower bound, or canonical standard bound.
    pub min_bitrate_kbps: Option<i64>,
    /// Custom MP3 upper bound (`None` when open-ended), or canonical.
    pub max_bitrate_kbps: Option<i64>,
    /// Custom FLAC bit depth.
    pub bit_depth: Option<i64>,
    /// Custom FLAC sample rate.
    pub sample_rate_hz: Option<i64>,
}

/// A recipe entry's container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecipeFormat {
    Flac,
    Mp3,
}

impl RecipeFormat {
    /// Stored and wire form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }
}

impl std::fmt::Display for RecipeFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A recipe entry's quality: MP3 bitrate bands, FLAC resolutions, or
/// `custom` for either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum RecipeQuality {
    #[serde(rename = "below_192")]
    Below192,
    #[serde(rename = "192_255")]
    Kbps192To255,
    #[serde(rename = "256_319")]
    Kbps256To319,
    #[serde(rename = "320_plus")]
    Kbps320Plus,
    #[serde(rename = "cd")]
    Cd,
    #[serde(rename = "24_48")]
    Bit24Khz48,
    #[serde(rename = "24_96")]
    Bit24Khz96,
    #[serde(rename = "24_192")]
    Bit24Khz192,
    #[serde(rename = "hi_res")]
    HiRes,
    #[serde(rename = "custom")]
    Custom,
}

impl RecipeQuality {
    /// Stored and wire form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Below192 => "below_192",
            Self::Kbps192To255 => "192_255",
            Self::Kbps256To319 => "256_319",
            Self::Kbps320Plus => "320_plus",
            Self::Cd => "cd",
            Self::Bit24Khz48 => "24_48",
            Self::Bit24Khz96 => "24_96",
            Self::Bit24Khz192 => "24_192",
            Self::HiRes => "hi_res",
            Self::Custom => "custom",
        }
    }
}

impl std::fmt::Display for RecipeQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical MP3 standard bounds: (min, max).
fn mp3_standard_bounds(quality: RecipeQuality) -> Option<(i64, Option<i64>)> {
    match quality {
        RecipeQuality::Below192 => Some((16, Some(191))),
        RecipeQuality::Kbps192To255 => Some((192, Some(255))),
        RecipeQuality::Kbps256To319 => Some((256, Some(319))),
        RecipeQuality::Kbps320Plus => Some((320, None)),
        RecipeQuality::Cd
        | RecipeQuality::Bit24Khz48
        | RecipeQuality::Bit24Khz96
        | RecipeQuality::Bit24Khz192
        | RecipeQuality::HiRes
        | RecipeQuality::Custom => None,
    }
}

fn is_mp3_quality(quality: RecipeQuality) -> bool {
    mp3_standard_bounds(quality).is_some() || quality == RecipeQuality::Custom
}

fn is_flac_quality(quality: RecipeQuality) -> bool {
    matches!(
        quality,
        RecipeQuality::Cd
            | RecipeQuality::Bit24Khz48
            | RecipeQuality::Bit24Khz96
            | RecipeQuality::Bit24Khz192
            | RecipeQuality::HiRes
            | RecipeQuality::Custom
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
        if self.format == RecipeFormat::Mp3 {
            if !is_mp3_quality(self.quality) {
                return Err(recipe_error(format!(
                    "unsupported mp3 quality recipe value: {:?}",
                    self.quality.as_str()
                )));
            }
            if self.bit_depth.is_some() || self.sample_rate_hz.is_some() {
                return Err(recipe_error(
                    "MP3 recipe entries cannot define FLAC resolution".to_owned(),
                ));
            }
            if self.quality == RecipeQuality::Custom {
                let (Some(minimum), Some(maximum)) = (self.min_bitrate_kbps, self.max_bitrate_kbps)
                else {
                    return Err(recipe_error(
                        "custom MP3 recipe requires min and max bitrates".to_owned(),
                    ));
                };
                for (name, value) in [("minimum bitrate", minimum), ("maximum bitrate", maximum)] {
                    if !(QUALITY_KBPS_MIN..=QUALITY_KBPS_MAX).contains(&value) {
                        return Err(recipe_error(format!(
                            "{name} must be between {QUALITY_KBPS_MIN} and {QUALITY_KBPS_MAX} kbps"
                        )));
                    }
                }
                if minimum > maximum {
                    return Err(recipe_error(
                        "custom MP3 needs a minimum bitrate no higher than its maximum".to_owned(),
                    ));
                }
                return Ok(());
            }
            let Some((exp_min, exp_max)) = mp3_standard_bounds(self.quality) else {
                return Err(recipe_error(format!(
                    "unsupported mp3 quality recipe value: {:?}",
                    self.quality.as_str()
                )));
            };
            let actual = (self.min_bitrate_kbps, self.max_bitrate_kbps);
            if actual != (None, None) && actual != (Some(exp_min), exp_max) {
                return Err(recipe_error(
                    "standard MP3 recipe fields must use their canonical bounds".to_owned(),
                ));
            }
            return Ok(());
        }
        if !is_flac_quality(self.quality) {
            return Err(recipe_error(format!(
                "unsupported flac quality recipe value: {:?}",
                self.quality.as_str()
            )));
        }
        if self.min_bitrate_kbps.is_some() || self.max_bitrate_kbps.is_some() {
            return Err(recipe_error(
                "FLAC recipe entries cannot define a bitrate".to_owned(),
            ));
        }
        if self.quality == RecipeQuality::Custom {
            let (Some(depth), Some(rate)) = (self.bit_depth, self.sample_rate_hz) else {
                return Err(recipe_error(
                    "custom FLAC needs a bit depth and a sample rate".to_owned(),
                ));
            };
            if !(1..=64).contains(&depth) {
                return Err(recipe_error(
                    "bit depth must be between 1 and 64".to_owned(),
                ));
            }
            if !(8000..=768000).contains(&rate) {
                return Err(recipe_error(
                    "sample rate must be between 8000 and 768000 Hz".to_owned(),
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
        if self.format == RecipeFormat::Mp3
            && self.quality != RecipeQuality::Custom
            && let Some((min, max)) = mp3_standard_bounds(self.quality)
        {
            self.min_bitrate_kbps = Some(min);
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

/// List-level recipe validation (v2 `validate_quality_recipe`): an empty
/// recipe, standard duplicates, MP3 range overlaps, and custom FLAC
/// duplicates rejected.
pub fn validate_quality_recipe(entries: &[QualityRecipeEntry]) -> Result<(), ConfigError> {
    if entries.is_empty() {
        return Err(recipe_error("needs at least one quality".to_owned()));
    }
    for entry in entries {
        entry.validate_entry()?;
    }
    let mut seen_standard = Vec::new();
    let mut mp3_ranges: Vec<(i64, Option<i64>)> = Vec::new();
    let mut flac_custom_pairs = Vec::new();
    for entry in entries {
        if entry.quality != RecipeQuality::Custom {
            let key = (entry.format, entry.quality);
            if seen_standard.contains(&key) {
                return Err(recipe_error(format!(
                    "lists {}/{} twice",
                    entry.format, entry.quality
                )));
            }
            seen_standard.push(key);
        }
        if entry.format == RecipeFormat::Mp3 {
            let mut bounds = (entry.min_bitrate_kbps, entry.max_bitrate_kbps);
            if entry.quality != RecipeQuality::Custom
                && bounds == (None, None)
                && let Some((min, max)) = mp3_standard_bounds(entry.quality)
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
                    if entry.quality == RecipeQuality::Custom {
                        return Err(recipe_error(
                            "custom MP3 quality recipe overlaps another MP3 range".to_owned(),
                        ));
                    }
                    return Err(recipe_error("MP3 quality recipe ranges overlap".to_owned()));
                }
            }
            mp3_ranges.push((minimum, maximum));
        } else if entry.quality == RecipeQuality::Custom
            && let (Some(depth), Some(rate)) = (entry.bit_depth, entry.sample_rate_hz)
        {
            if flac_custom_pairs.contains(&(depth, rate)) {
                return Err(recipe_error(
                    "lists the same custom FLAC resolution twice".to_owned(),
                ));
            }
            flac_custom_pairs.push((depth, rate));
        }
    }
    Ok(())
}

/// The tiers one recipe entry touches. A custom MP3 range can touch several.
fn entry_tiers(entry: &QualityRecipeEntry) -> Vec<&'static str> {
    if entry.format == RecipeFormat::Flac {
        return vec!["lossless"];
    }
    match entry.quality {
        RecipeQuality::Below192 => return vec!["low"],
        RecipeQuality::Kbps192To255 => return vec!["mp3_192"],
        RecipeQuality::Kbps256To319 => return vec!["mp3_256"],
        RecipeQuality::Kbps320Plus => return vec!["mp3_320"],
        _ => {}
    }
    let minimum = entry.min_bitrate_kbps.unwrap_or(QUALITY_KBPS_MIN);
    let upper = entry.max_bitrate_kbps.unwrap_or(QUALITY_KBPS_MAX);
    let mut touched = Vec::new();
    if minimum <= 191 && upper >= QUALITY_KBPS_MIN {
        touched.push("low");
    }
    if minimum <= 255 && upper >= 192 {
        touched.push("mp3_192");
    }
    if minimum <= 319 && upper >= 256 {
        touched.push("mp3_256");
    }
    if upper >= 320 {
        touched.push("mp3_320");
    }
    if touched.is_empty() {
        touched.push("low");
    }
    touched
}

/// The closest contiguous tier range, (worst, best), that covers a recipe.
/// `None` for an empty recipe, which has no range.
fn range_from_recipe(entries: &[QualityRecipeEntry]) -> Option<(&'static str, &'static str)> {
    let ranks: Vec<usize> = entries
        .iter()
        .flat_map(entry_tiers)
        .filter_map(tier_rank)
        .collect();
    let worst = *ranks.iter().min()?;
    let best = *ranks.iter().max()?;
    Some((TIER_KEYS_WORST_FIRST[worst], TIER_KEYS_WORST_FIRST[best]))
}

/// The recipe entry one tier converts to. `lossless` takes every FLAC
/// resolution, as the tier itself does.
fn tier_entry(tier: &str) -> Option<QualityRecipeEntry> {
    let (format, quality) = match tier {
        "lossless" => (RecipeFormat::Flac, RecipeQuality::HiRes),
        "mp3_320" => (RecipeFormat::Mp3, RecipeQuality::Kbps320Plus),
        "mp3_256" => (RecipeFormat::Mp3, RecipeQuality::Kbps256To319),
        "mp3_192" => (RecipeFormat::Mp3, RecipeQuality::Kbps192To255),
        "low" => (RecipeFormat::Mp3, RecipeQuality::Below192),
        _ => return None,
    };
    let mut entry = QualityRecipeEntry {
        format,
        quality,
        min_bitrate_kbps: None,
        max_bitrate_kbps: None,
        bit_depth: None,
        sample_rate_hz: None,
    };
    entry.canonicalize();
    Some(entry)
}

/// Whether a preference order lists exactly the accepted tiers, once each.
#[must_use]
pub fn order_fits(preference: &[String], accepted: &[String]) -> bool {
    preference.len() == accepted.len() && accepted.iter().all(|tier| preference.contains(tier))
}

/// The one conversion of a tier range to a recipe: one entry per accepted
/// tier, in `preference` when that fits the range, else best first. The
/// recipe's range reads back as the range it came from.
#[must_use]
pub fn recipe_for_range(
    quality_min: &str,
    quality_max: &str,
    preference: &[String],
) -> Vec<QualityRecipeEntry> {
    let accepted = derive_default_order(quality_min, quality_max);
    let order = if order_fits(preference, &accepted) {
        preference
    } else {
        &accepted
    };
    order.iter().filter_map(|tier| tier_entry(tier)).collect()
}

/// Download policy: quality tiers, recipe v2, timeouts, retry, retention,
/// quotas, and the upgrade scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DownloadPolicy {
    /// Worst accepted tier label, set from the recipe.
    pub quality_min: String,
    /// Best accepted tier label, set from the recipe.
    pub quality_max: String,
    /// Always on. Other formats are still accepted, but rank after FLAC and
    /// MP3 of the same completeness.
    pub flac_mp3_only: bool,
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
    // A row or request without the key decodes as empty, never as the default
    // policy's recipe: a stored row is then converted from its own range, and
    // a save is refused.
    /// Ordered quality recipe, earlier entries preferred. A save needs at
    /// least one entry.
    #[serde(default)]
    #[schema(default)]
    pub quality_recipe: Vec<QualityRecipeEntry>,
    /// When an automatic download waits for a hand pick: `when_unsure`
    /// (no candidate looks like the whole album in an accepted quality),
    /// `always`, or `off` (grab the best guess).
    pub search_review: String,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            quality_min: "mp3_320".to_owned(),
            quality_max: "lossless".to_owned(),
            flac_mp3_only: true,
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
            max_size_mb: 0,
            usenet_retention_days: 0,
            ignored_terms: Vec::new(),
            required_terms: Vec::new(),
            quality_cutoff: "lossless".to_owned(),
            upgrade_allowed: false,
            max_library_size_gb: 0,
            default_request_quota_count: 0,
            default_request_quota_days: 7,
            default_storage_quota_gb: 0,
            background_upgrade_scan_enabled: false,
            background_upgrade_scan_interval_hours: 12,
            background_upgrade_max_per_run: 3,
            quality_recipe: recipe_for_range("mp3_320", "lossless", &[]),
            search_review: "when_unsure".to_owned(),
        }
    }
}

impl DownloadPolicy {
    /// Replace the recipe with the conversion of the healed tier range.
    pub fn convert_range(&mut self, preference: &[String]) {
        self.heal_tiers();
        self.quality_recipe = recipe_for_range(&self.quality_min, &self.quality_max, preference);
    }

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
        check_range(
            key,
            "preflight_score_auto_accept",
            self.preflight_score_auto_accept,
            0.0,
            1.0,
        )?;
        check_range(
            key,
            "preflight_score_manual_min",
            self.preflight_score_manual_min,
            0.0,
            1.0,
        )?;
        if self.preflight_score_manual_min > self.preflight_score_auto_accept {
            return Err(validation(
                key,
                "preflight_score_manual_min",
                format!(
                    "manual-review score must not be above the auto-accept score ({}), got {}",
                    self.preflight_score_auto_accept, self.preflight_score_manual_min
                ),
            ));
        }
        if !self.flac_mp3_only {
            return Err(validation(
                key,
                "quality_recipe",
                "only works with FLAC and MP3 only on".to_owned(),
            ));
        }
        validate_quality_recipe(&self.quality_recipe)?;
        if !matches!(
            self.search_review.as_str(),
            "when_unsure" | "always" | "off"
        ) {
            return Err(validation(
                key,
                "search_review",
                format!("unknown choice {:?}", self.search_review),
            ));
        }
        Ok(())
    }

    fn normalize(&mut self) {
        if self.quality_recipe.is_empty() {
            self.convert_range(&[]);
        }
        if !matches!(
            self.search_review.as_str(),
            "when_unsure" | "always" | "off"
        ) {
            self.search_review = "when_unsure".to_owned();
        }
        for entry in &mut self.quality_recipe {
            entry.canonicalize();
        }
        // A recipe that does not validate keeps the stored range: the
        // settings screen reports it, and it says nothing about the range.
        if validate_quality_recipe(&self.quality_recipe).is_ok()
            && let Some((worst, best)) = range_from_recipe(&self.quality_recipe)
        {
            worst.clone_into(&mut self.quality_min);
            best.clone_into(&mut self.quality_max);
        }
        self.heal_tiers();
        self.flac_mp3_only = true;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Worst tier first, what each one converts to.
    const CONVERTED: [(RecipeFormat, RecipeQuality); 5] = [
        (RecipeFormat::Mp3, RecipeQuality::Below192),
        (RecipeFormat::Mp3, RecipeQuality::Kbps192To255),
        (RecipeFormat::Mp3, RecipeQuality::Kbps256To319),
        (RecipeFormat::Mp3, RecipeQuality::Kbps320Plus),
        (RecipeFormat::Flac, RecipeQuality::HiRes),
    ];

    fn steps(policy: &DownloadPolicy) -> Vec<(RecipeFormat, RecipeQuality)> {
        policy
            .quality_recipe
            .iter()
            .map(|entry| (entry.format, entry.quality))
            .collect()
    }

    // The range is what the landing, Usenet and plugin sources read, so a
    // conversion that moved it would change what gets downloaded. A stored
    // row may hold an empty recipe or no recipe key at all.
    #[test]
    fn a_v1_range_converts_to_a_recipe_with_the_same_range() {
        let mut ranges = 0;
        for (low, quality_min) in TIER_KEYS_WORST_FIRST.iter().enumerate() {
            for (high, quality_max) in TIER_KEYS_WORST_FIRST.iter().enumerate().skip(low) {
                ranges += 1;
                let row = json!({
                    "quality_min": quality_min,
                    "quality_max": quality_max,
                    "quality_cutoff": quality_min,
                });
                let mut with_empty = row.clone();
                with_empty["quality_recipe"] = json!([]);
                for stored in [row, with_empty] {
                    let mut policy: DownloadPolicy =
                        serde_json::from_value(stored).expect("the row decodes");
                    policy.normalize();
                    let label = format!("{quality_min} to {quality_max}");
                    assert!(!policy.quality_recipe.is_empty(), "{label}");
                    validate_quality_recipe(&policy.quality_recipe).expect(&label);
                    assert_eq!(policy.quality_min, *quality_min, "{label}");
                    assert_eq!(policy.quality_max, *quality_max, "{label}");
                    assert_eq!(policy.quality_cutoff, *quality_min, "{label}");
                    let best_first: Vec<_> = CONVERTED[low..=high].iter().rev().copied().collect();
                    assert_eq!(steps(&policy), best_first, "{label}");
                }
            }
        }
        assert_eq!(ranges, 15);

        let fresh = DownloadPolicy::default();
        assert_eq!(
            (fresh.quality_min.as_str(), fresh.quality_max.as_str()),
            ("mp3_320", "lossless")
        );
        assert_eq!(steps(&fresh), [CONVERTED[4], CONVERTED[3]]);

        let mut no_recipe = fresh;
        no_recipe.quality_recipe.clear();
        assert!(no_recipe.validate().is_err(), "a save needs a recipe");
    }
}
