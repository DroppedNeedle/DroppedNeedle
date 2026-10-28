//! What a Soulseek search takes from the saved download policy: the
//! quality band the landing keeps, the order of preference inside it, and
//! the search timings.
//!
//! The band is the gate at the pick, so a folder the landing would refuse
//! is never fetched. The saved recipe orders the folders that are left;
//! a policy saved without one prefers the band's tiers, best first.

use std::time::Duration;

use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy as SavedPolicy, QualityRecipeEntry, derive_default_order,
};

use crate::landing::quality::{in_range, tier_for};

/// Known-unimportable audio containers (DSD families). A
/// declared DSD release must not spend
/// bandwidth even when a noisy title claims "lossless".
pub const NOT_IMPORTABLE_EXTENSIONS: [&str; 3] = ["dsd", "dsf", "dff"];

/// Fixed lossless detail ladder: 0 cd (at
/// most 16-bit / 48 kHz), 1 24_48, 2 24_96, 3 24_192, 4 hi_res, 5 partial
/// (resolution unknown).
const LOSSLESS_DETAIL_STEPS: [&str; 6] = ["cd", "24_48", "24_96", "24_192", "hi_res", "partial"];

/// Where trusted depth and rate sit on the ladder; either axis absent is
/// the `partial` step.
fn lossless_detail_step(bit_depth: Option<i32>, sample_rate_hz: Option<i32>) -> usize {
    let (Some(depth), Some(rate)) = (bit_depth, sample_rate_hz) else {
        return 5;
    };
    if depth <= 16 && rate <= 48_000 {
        0
    } else if depth <= 24 && rate <= 48_000 {
        1
    } else if depth <= 24 && rate <= 96_000 {
        2
    } else if depth <= 24 && rate <= 192_000 {
        3
    } else {
        4
    }
}

/// The tier a search result shows, by the landing's rules, or `None` when
/// the search does not show it: a lossy file with no bitrate, or an MP4
/// file with neither bit depth nor bitrate.
#[must_use]
pub fn search_tier(
    extension: &str,
    bitrate_kbps: Option<i32>,
    bit_depth: Option<i32>,
) -> Option<&'static str> {
    let tier = tier_for(
        extension,
        bitrate_kbps.and_then(|rate| u32::try_from(rate).ok()),
        bit_depth.and_then(|depth| u8::try_from(depth).ok()),
    );
    (tier == "lossless" || bitrate_kbps.is_some()).then_some(tier)
}

/// The quality rules and timings of every Soulseek search.
#[derive(Debug, Clone)]
pub struct DownloadPolicy {
    /// Lowest tier the landing keeps.
    pub quality_min: String,
    /// Highest tier the landing keeps.
    pub quality_max: String,
    /// The saved recipe, earlier entries preferred. Empty means the band's
    /// tiers, best first.
    pub quality_recipe: Vec<QualityRecipeEntry>,
    /// Search window handed to slskd per query rung.
    pub search_timeout: Duration,
    /// Extra poll time past the search window: slskd fills
    /// GET /searches/{id}/responses only after the search completes, which
    /// lands after the searchTimeout window (v2 observed ~12s later on
    /// 0.25.1; grace is 30s or every search returns 0 candidates).
    pub completion_grace: Duration,
    /// Pause between search-state polls.
    pub poll_interval: Duration,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self::from_saved(&SavedPolicy::default())
    }
}

impl DownloadPolicy {
    /// The search rules for a saved download policy.
    #[must_use]
    pub fn from_saved(saved: &SavedPolicy) -> Self {
        Self {
            quality_min: saved.quality_min.clone(),
            quality_max: saved.quality_max.clone(),
            quality_recipe: saved.quality_recipe.clone(),
            search_timeout: Duration::from_secs(30),
            completion_grace: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
        }
    }

    /// Whether `tier` sits inside the band, as the landing checks it.
    #[must_use]
    pub fn in_band(&self, tier: &str) -> bool {
        in_range(tier, &self.quality_min, &self.quality_max)
    }

    /// Rank one file by preference: the index of the first recipe entry it
    /// satisfies (or of its tier in the band when there is no recipe), or
    /// `None` when nothing admits it.
    ///
    /// `bitrate_kbps` counts for lossy files only: FLAC bitrate varies with
    /// compression. A FLAC file whose resolution the search does not show
    /// still matches a standard FLAC entry.
    #[must_use]
    pub fn recipe_rank(
        &self,
        extension: &str,
        bitrate_kbps: Option<i32>,
        bit_depth: Option<i32>,
        sample_rate_hz: Option<i32>,
    ) -> Option<usize> {
        let extension = extension.to_ascii_lowercase();
        if NOT_IMPORTABLE_EXTENSIONS.contains(&extension.as_str()) {
            return None;
        }
        if self.quality_recipe.is_empty() {
            let tier = search_tier(&extension, bitrate_kbps, bit_depth)?;
            return derive_default_order(&self.quality_min, &self.quality_max)
                .iter()
                .position(|key| key == tier);
        }
        self.quality_recipe.iter().position(|entry| {
            if entry.format == "mp3" {
                let Some(bitrate) = bitrate_kbps.filter(|_| extension == "mp3") else {
                    return false;
                };
                let bitrate = i64::from(bitrate);
                let minimum = entry.min_bitrate_kbps.unwrap_or(16);
                bitrate >= minimum && entry.max_bitrate_kbps.is_none_or(|max| bitrate <= max)
            } else if extension != "flac" {
                false
            } else if entry.quality == "custom" {
                // An exact resolution needs both axes shown.
                match (
                    entry.bit_depth,
                    entry.sample_rate_hz,
                    bit_depth,
                    sample_rate_hz,
                ) {
                    (Some(want_depth), Some(want_rate), Some(depth), Some(rate)) => {
                        i64::from(depth) == want_depth && i64::from(rate) == want_rate
                    }
                    _ => false,
                }
            } else {
                let want_step = LOSSLESS_DETAIL_STEPS
                    .iter()
                    .position(|step| *step == entry.quality)
                    .unwrap_or(0);
                let have_step = lossless_detail_step(bit_depth, sample_rate_hz);
                have_step <= want_step || have_step == 5
            }
        })
    }
}
