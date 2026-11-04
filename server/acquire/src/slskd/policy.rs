//! What a Soulseek search takes from the saved download policy: the
//! quality band the landing keeps, the order of preference inside it, and
//! the search timings.
//!
//! The band and the recipe are the gate at the pick, so a folder the landing
//! would refuse or hold is never fetched. The recipe orders what is left.

use std::time::Duration;

use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy as SavedPolicy, QualityRecipeEntry,
};

use crate::landing::quality::{RecipeFit, in_range, recipe_fit, tier_for};

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
    /// The saved recipe, earlier entries preferred.
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

    /// Rank one file by preference: the recipe entry it satisfies, or `None`
    /// when nothing admits it or the search does not show enough to say.
    #[must_use]
    pub fn recipe_rank(
        &self,
        extension: &str,
        bitrate_kbps: Option<i32>,
        bit_depth: Option<i32>,
        sample_rate_hz: Option<i32>,
    ) -> Option<usize> {
        match self.recipe_fit(extension, bitrate_kbps, bit_depth, sample_rate_hz) {
            RecipeFit::Step(index) => Some(index),
            RecipeFit::Unshown | RecipeFit::Outside => None,
        }
    }

    /// What the recipe says about one search result. Bitrate counts for
    /// lossy files only: FLAC bitrate varies with compression.
    #[must_use]
    pub fn recipe_fit(
        &self,
        extension: &str,
        bitrate_kbps: Option<i32>,
        bit_depth: Option<i32>,
        sample_rate_hz: Option<i32>,
    ) -> RecipeFit {
        recipe_fit(
            &self.quality_recipe,
            extension,
            bitrate_kbps,
            bit_depth,
            sample_rate_hz,
        )
    }
}
