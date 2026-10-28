//! Usenet policy: quality tiers, timeouts, retention.
//!
//! [`UsenetPolicy`] holds the subset of the download policy the Usenet
//! clients consume, with the shipped defaults; the acquisition clients fill it
//! from the download policy settings ([`UsenetPolicy::from_saved`]).

use std::time::Duration;

use crate::settings::{clamp_u32, clamp_u64};
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

/// Audio quality tiers, best first.
///
/// A single linear axis,
/// lossless at the top, then codec-agnostic lossy bitrate bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityTier {
    /// Below 192kbps equivalent.
    Low,
    /// 192-255kbps band.
    Mp3_192,
    /// 256-319kbps band.
    Mp3_256,
    /// 320kbps+ band (V0 included).
    Mp3_320,
    /// FLAC/ALAC/WAV/APE/WavPack.
    Lossless,
}

impl QualityTier {
    /// Wire key (`quality_min`/`quality_max`/`quality_cutoff` values).
    pub fn key(self) -> &'static str {
        match self {
            QualityTier::Low => "low",
            QualityTier::Mp3_192 => "mp3_192",
            QualityTier::Mp3_256 => "mp3_256",
            QualityTier::Mp3_320 => "mp3_320",
            QualityTier::Lossless => "lossless",
        }
    }

    /// Parse a wire key. Unknown keys are the caller's bug; `None` lets
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

    /// Higher is better (low=0 .. lossless=4).
    pub fn rank(self) -> u8 {
        match self {
            QualityTier::Low => 0,
            QualityTier::Mp3_192 => 1,
            QualityTier::Mp3_256 => 2,
            QualityTier::Mp3_320 => 3,
            QualityTier::Lossless => 4,
        }
    }
}

/// Accepted quality range plus the Usenet timeout/retention knobs.
///
/// Shipped defaults: `quality_min "mp3_320"`, `quality_max "lossless"`,
/// `flac_mp3_only` on, 30-minute stall / 2-hour queued watchdogs,
/// 30-minute minimum release age, unbounded retention/size.
#[derive(Debug, Clone)]
pub struct UsenetPolicy {
    /// Floor of the accepted contiguous tier band.
    pub quality_min: QualityTier,
    /// Ceiling of the accepted contiguous tier band.
    pub quality_max: QualityTier,
    /// Restrict acceptable formats to FLAC + MP3 (default on).
    pub flac_mp3_only: bool,
    /// Stop upgrading once the worst held track reaches this tier.
    pub quality_cutoff: QualityTier,
    /// Per-indexer HTTP call budget.
    pub indexer_timeout: Duration,
    /// NZB fetch + SABnzbd add budget.
    pub enqueue_timeout: Duration,
    /// Queue/history poll budget.
    pub poll_timeout: Duration,
    /// Watchdog: an active transfer moving no bytes this long fails over.
    pub stall_timeout: Duration,
    /// Watchdog: a job queued (never active) this long fails over.
    pub queued_timeout: Duration,
    /// Reject a release older than this many days (0 = no limit). Past a
    /// provider's retention the articles are gone, so the grab would only
    /// partially download and fail.
    pub retention_days: u32,
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
            max_size_mb: 0,
        }
    }
}

impl UsenetPolicy {
    /// The shipped default band, `mp3_320..=lossless`. `Default` keeps
    /// min==max until real settings are bound, so the saved policy starts
    /// from this.
    pub fn v2_defaults() -> Self {
        UsenetPolicy {
            quality_max: QualityTier::Lossless,
            ..UsenetPolicy::default()
        }
    }

    /// Usenet policy bound from the download-policy section.
    pub(crate) fn from_saved(policy: &DownloadPolicy) -> Self {
        let mut out = UsenetPolicy::v2_defaults();
        if let Some(tier) = QualityTier::parse(&policy.quality_min) {
            out.quality_min = tier;
        }
        if let Some(tier) = QualityTier::parse(&policy.quality_max) {
            out.quality_max = tier;
        }
        if let Some(tier) = QualityTier::parse(&policy.quality_cutoff) {
            out.quality_cutoff = tier;
        }
        out.flac_mp3_only = policy.flac_mp3_only;
        out.retention_days = clamp_u32(policy.usenet_retention_days);
        out.max_size_mb = clamp_u64(policy.max_size_mb);
        out.stall_timeout =
            Duration::from_secs(clamp_u64(policy.download_stall_timeout_minutes).max(1) * 60);
        out.queued_timeout =
            Duration::from_secs(clamp_u64(policy.download_queued_timeout_minutes).max(1) * 60);
        out
    }

    /// Whether `tier` sits inside the accepted contiguous band.
    pub fn accepts_tier(&self, tier: QualityTier) -> bool {
        tier.rank() >= self.quality_min.rank() && tier.rank() <= self.quality_max.rank()
    }

    /// Whether a release's Newznab categories leave it inside the band:
    /// lossless (3040) needs a band that reaches lossless, MP3 (3010) one
    /// that reaches below it. Other categories, and none, say nothing.
    pub fn accepts_categories(&self, category_ids: &[i32]) -> bool {
        let lossless_out =
            category_ids.contains(&3040) && self.quality_max != QualityTier::Lossless;
        let lossy_out = category_ids.contains(&3010) && self.quality_min == QualityTier::Lossless;
        !(lossless_out || lossy_out)
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

    /// Size gate: 0 disables; otherwise the album must fit `max_size_mb`.
    pub fn within_size_cap(&self, size_bytes: u64) -> bool {
        if self.max_size_mb == 0 {
            return true;
        }
        size_bytes <= self.max_size_mb * 1024 * 1024
    }
}
