//! Usenet policy: quality tiers, search recipe, timeouts, retention.
//!
//! Ported from v2's `DownloadPolicySettings` and quality tiers. This struct
//! holds the subset the Usenet clients consume, with v2's defaults; the
//! acquisition wiring fills it from the download policy settings.

use std::time::Duration;

/// Audio quality tiers, best first.
///
/// Ported from v2 `TIER_KEYS` (`quality_tiers.py`): a single linear axis,
/// lossless at the top, then codec-agnostic lossy bitrate bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityTier {
    /// Below 192kbps equivalent.
    Low,
    /// 192-255kbps band.
    Mp3_192,
    /// 256-319kbps band.
    Mp3_256,
    /// 320kbps+ band (V0 included, like v2).
    Mp3_320,
    /// FLAC/ALAC/WAV/APE/WavPack.
    Lossless,
}

impl QualityTier {
    /// v2 wire key (`quality_min`/`quality_max`/`quality_cutoff` values).
    pub fn key(self) -> &'static str {
        match self {
            QualityTier::Low => "low",
            QualityTier::Mp3_192 => "mp3_192",
            QualityTier::Mp3_256 => "mp3_256",
            QualityTier::Mp3_320 => "mp3_320",
            QualityTier::Lossless => "lossless",
        }
    }

    /// Parse a v2 wire key. Unknown keys are the caller's bug; `None` lets
    /// the caller fall back to a default instead of panicking.
    pub fn parse(key: &str) -> Option<QualityTier> {
        match key.trim().to_ascii_lowercase().as_str() {
            "low" => Some(QualityTier::Low),
            "mp3_192" | "mp3-192" | "192" => Some(QualityTier::Mp3_192),
            "mp3_256" | "mp3-256" | "256" => Some(QualityTier::Mp3_256),
            "mp3_320" | "mp3-320" | "320" => Some(QualityTier::Mp3_320),
            "lossless" | "flac" => Some(QualityTier::Lossless),
            _ => None,
        }
    }

    /// Higher is better, mirroring v2 `tier_rank` (low=0 .. lossless=4).
    pub fn rank(self) -> u8 {
        match self {
            QualityTier::Low => 0,
            QualityTier::Mp3_192 => 1,
            QualityTier::Mp3_256 => 2,
            QualityTier::Mp3_320 => 3,
            QualityTier::Lossless => 4,
        }
    }

    /// Newznab category recipe for this tier: 3040 (Lossless) for lossless,
    /// 3010 (MP3) for the lossy bands. v2's scorer treats category as the
    /// primary quality signal (3040 ⇒ lossless).
    pub fn category_id(self) -> i32 {
        match self {
            QualityTier::Lossless => 3040,
            _ => 3010,
        }
    }
}

/// Accepted quality range plus the Usenet timeout/retention knobs.
///
/// Defaults are v2's: `quality_min "mp3_320"`, `quality_max "lossless"`,
/// `flac_mp3_only` on, 30-minute stall / 2-hour queued watchdogs,
/// 30-minute minimum release age, unbounded retention/size.
#[derive(Debug, Clone)]
pub struct UsenetPolicy {
    /// Floor of the accepted contiguous tier band.
    pub quality_min: QualityTier,
    /// Ceiling of the accepted contiguous tier band.
    pub quality_max: QualityTier,
    /// Restrict acceptable formats to FLAC + MP3 (v2 default on).
    pub flac_mp3_only: bool,
    /// Stop upgrading once the worst held track reaches this tier.
    pub quality_cutoff: QualityTier,
    /// Per-indexer HTTP call budget (v2 `per_indexer_timeout`).
    pub indexer_timeout: Duration,
    /// NZB fetch + SABnzbd add budget (v2 `fetch_nzb`/add 60s).
    pub enqueue_timeout: Duration,
    /// Queue/history poll budget (v2 30s).
    pub poll_timeout: Duration,
    /// Watchdog: an active transfer moving no bytes this long fails over.
    pub stall_timeout: Duration,
    /// Watchdog: a job queued (never active) this long fails over.
    pub queued_timeout: Duration,
    /// Reject a release older than this many days (0 = no limit). Past a
    /// provider's retention the articles are gone, so the grab would only
    /// partially download and fail (v2 `usenet_retention_days`).
    pub retention_days: u32,
    /// Never permanently blocklist a release younger than this; it may
    /// still be propagating (v2 `usenet_min_release_age_minutes`).
    pub min_release_age: Duration,
    /// Hard upper bound on one album's total size in MB (0 = unbounded).
    pub max_size_mb: u64,
}

impl Default for UsenetPolicy {
    fn default() -> Self {
        UsenetPolicy {
            quality_min: QualityTier::Mp3_320,
            quality_max: QualityTier::Mp3_320,
            flac_mp3_only: true,
            quality_cutoff: QualityTier::Lossless,
            indexer_timeout: Duration::from_secs(30),
            enqueue_timeout: Duration::from_secs(60),
            poll_timeout: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(30 * 60),
            queued_timeout: Duration::from_secs(120 * 60),
            retention_days: 0,
            min_release_age: Duration::from_secs(30 * 60),
            max_size_mb: 0,
        }
    }
}

impl UsenetPolicy {
    /// v2's shipped default band is `mp3_320..=lossless`; `Default` keeps
    /// min==max until the coordinator binds real settings, so call this
    /// for the v2 behavior.
    pub fn v2_defaults() -> Self {
        UsenetPolicy {
            quality_max: QualityTier::Lossless,
            ..UsenetPolicy::default()
        }
    }

    /// Whether `tier` sits inside the accepted contiguous band.
    pub fn accepts_tier(&self, tier: QualityTier) -> bool {
        tier.rank() >= self.quality_min.rank() && tier.rank() <= self.quality_max.rank()
    }

    /// Whether an extension passes the `flac_mp3_only` gate (v2 `_FLAC_MP3_EXT`).
    pub fn accepts_extension(&self, ext: &str) -> bool {
        if !self.flac_mp3_only {
            return true;
        }
        matches!(ext.trim().to_ascii_lowercase().as_str(), "flac" | "mp3")
    }

    /// Category recipe for the accepted band: the distinct category ids of
    /// every tier in range, lossless first. Callers append the indexer's
    /// own Audio/Other id (read from caps, never hardcoded) when they
    /// want promos and oddities too.
    pub fn recipe_categories(&self) -> Vec<i32> {
        let mut out = Vec::new();
        for tier in [
            QualityTier::Lossless,
            QualityTier::Mp3_320,
            QualityTier::Mp3_256,
            QualityTier::Mp3_192,
            QualityTier::Low,
        ] {
            if self.accepts_tier(tier) {
                let id = tier.category_id();
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// Retention gate: `None` age is unknown, never rejected; otherwise the
    /// release must be younger than `retention_days` (0 disables the gate).
    pub fn within_retention(&self, usenet_date_unix: Option<f64>, now_unix: f64) -> bool {
        let Some(posted) = usenet_date_unix else {
            return true;
        };
        if self.retention_days == 0 {
            return true;
        }
        now_unix - posted <= f64::from(self.retention_days) * 86_400.0
    }

    /// Propagation guard: a release younger than `min_release_age` must not
    /// be permanently blocklisted yet (indexers propagate slowly).
    pub fn old_enough_to_blocklist(&self, usenet_date_unix: Option<f64>, now_unix: f64) -> bool {
        let Some(posted) = usenet_date_unix else {
            return true;
        };
        now_unix - posted >= self.min_release_age.as_secs_f64()
    }

    /// Size gate: 0 disables; otherwise the album must fit `max_size_mb`.
    pub fn within_size_cap(&self, size_bytes: u64) -> bool {
        if self.max_size_mb == 0 {
            return true;
        }
        size_bytes <= self.max_size_mb * 1024 * 1024
    }
}
