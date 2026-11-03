//! Usenet policy: quality tiers, timeouts, retention.
//!
//! [`UsenetPolicy`] holds the subset of the download policy the Usenet
//! clients consume, with the shipped defaults; the acquisition clients fill it
//! from the download policy settings ([`UsenetPolicy::from_saved`]).

use std::time::Duration;

use crate::landing::quality::{RecipeFit, names_format, recipe_fit};
use crate::settings::{clamp_u32, clamp_u64};
use droppedneedle_kernel::runtime_config::sections::{DownloadPolicy, QualityRecipeEntry};

/// Words in a release title that name a format a recipe cannot list.
const OTHER_FORMAT_WORDS: [&str; 7] = ["ogg", "vorbis", "opus", "aac", "m4a", "alac", "wav"];

/// A text's words, lowercased.
fn words(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

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
/// the default recipe, 30-minute stall / 2-hour queued watchdogs,
/// 30-minute minimum release age, unbounded retention/size.
#[derive(Debug, Clone)]
pub struct UsenetPolicy {
    /// Floor of the accepted contiguous tier band.
    pub quality_min: QualityTier,
    /// Ceiling of the accepted contiguous tier band.
    pub quality_max: QualityTier,
    /// The saved recipe: what automatic picks may take.
    pub quality_recipe: Vec<QualityRecipeEntry>,
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
            quality_recipe: DownloadPolicy::default().quality_recipe,
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
        out.quality_recipe.clone_from(&policy.quality_recipe);
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

    /// Whether a release's Newznab categories leave it inside the settings:
    /// lossless (3040) needs a band that reaches lossless and a FLAC entry,
    /// MP3 (3010) a band that reaches below it and an MP3 entry. Other
    /// categories, and none, say nothing.
    pub fn accepts_categories(&self, category_ids: &[i32]) -> bool {
        let lossless_out = category_ids.contains(&3040)
            && (self.quality_max != QualityTier::Lossless
                || !names_format(&self.quality_recipe, "flac"));
        let lossy_out = category_ids.contains(&3010)
            && (self.quality_min == QualityTier::Lossless
                || !names_format(&self.quality_recipe, "mp3"));
        !(lossless_out || lossy_out)
    }

    /// Whether a release's title leaves it inside the recipe. It is refused
    /// when it names another format and neither FLAC nor MP3, or says 24-bit
    /// or hi-res where no FLAC entry admits 24-bit. Words of `names` (the
    /// artist and title asked for) say nothing, so an album called Opus
    /// stands. Whatever the title does not say, the landing decides.
    pub fn accepts_title(&self, title: &str, names: &[&str]) -> bool {
        let asked: Vec<String> = names.iter().flat_map(|name| words(name)).collect();
        let said: Vec<String> = words(title)
            .into_iter()
            .filter(|word| !asked.contains(word))
            .collect();
        let says = |word: &str| said.iter().any(|known| known == word);
        let says_pair = |first: &str, second: &str| {
            said.windows(2)
                .any(|pair| pair[0] == first && pair[1] == second)
        };
        let other_format = OTHER_FORMAT_WORDS.iter().any(|word| says(word));
        if other_format && !says("flac") && !says("mp3") {
            return false;
        }
        let hi_res =
            says("24bit") || says_pair("24", "bit") || says("hires") || says_pair("hi", "res");
        !hi_res
            || recipe_fit(&self.quality_recipe, "flac", None, Some(24), None) != RecipeFit::Outside
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

#[cfg(test)]
mod tests {
    use super::*;

    // A word in a title is only evidence when it names a format, and never
    // when it is the album or artist asked for.
    #[test]
    fn a_title_refuses_only_what_it_says() {
        let default = UsenetPolicy::v2_defaults();
        let capped = UsenetPolicy {
            quality_recipe: serde_json::from_value(serde_json::json!([
                {"format": "flac", "quality": "cd"},
            ]))
            .expect("recipe"),
            ..UsenetPolicy::v2_defaults()
        };
        let album: &[&str] = &["Portishead", "Dummy"];
        let opus: &[&str] = &["Schiller", "Opus"];
        let rows: &[(&UsenetPolicy, &str, &[&str], bool)] = &[
            (&default, "Portishead - Dummy (1994) [OGG]", album, false),
            (&default, "Portishead-Dummy-WEB-2022-AAC", album, false),
            (&default, "Portishead - Dummy FLAC + OGG", album, true),
            (&default, "Portishead - Dummy (1994)", album, true),
            (&default, "Schiller - Opus (2013)", opus, true),
            (&default, "Schiller - Opus [OPUS]", album, false),
            (&default, "Enigma - Opus Dei Waves", album, false),
            (&default, "Artist - Wave of Mutilation", album, true),
            (&default, "Portishead - Dummy 24bit FLAC", album, true),
            (&capped, "Portishead - Dummy 24bit FLAC", album, false),
            (&capped, "Portishead - Dummy [24 bit]", album, false),
            (&capped, "Portishead - Dummy Hi-Res", album, false),
            (&capped, "Portishead - Dummy FLAC", album, true),
        ];
        for (policy, title, names, want) in rows {
            assert_eq!(policy.accepts_title(title, names), *want, "{title}");
        }
    }
}
