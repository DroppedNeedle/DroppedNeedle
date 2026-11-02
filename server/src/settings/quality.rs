//! Acquisition-quality tiers and recipe status (pure).
//!
//! The canonical five tiers, the default order derivation and the v2 recipe
//! legacy projection. No I/O: the caller supplies the policy.

use super::models::QualityRecipeStatus;
use droppedneedle_kernel::runtime_config::sections::{QualityRecipeEntry, RecipeFormat};

/// Canonical five tiers, best first.
pub const TIER_KEYS_BEST_FIRST: [&str; 5] = ["lossless", "mp3_320", "mp3_256", "mp3_192", "low"];

/// Canonical v1 tiers touched by one recipe entry (rollback projection).
pub fn recipe_entry_legacy_tiers(entry: &QualityRecipeEntry) -> Vec<&'static str> {
    if entry.format == RecipeFormat::Flac {
        return vec!["lossless"];
    }
    match entry.quality.as_str() {
        "below_192" => return vec!["low"],
        "192_255" => return vec!["mp3_192"],
        "256_319" => return vec!["mp3_256"],
        "320_plus" => return vec!["mp3_320"],
        _ => {}
    }
    let minimum = entry.min_bitrate_kbps.unwrap_or(16);
    let upper = entry.max_bitrate_kbps.unwrap_or(2048);
    let mut touched = Vec::new();
    if minimum <= 191 && upper >= 16 {
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

/// Closest contiguous v1 range covering all recipe entries.
pub fn legacy_range_from_recipe(entries: &[QualityRecipeEntry]) -> (String, String) {
    let mut tiers = std::collections::BTreeSet::new();
    for entry in entries {
        tiers.extend(recipe_entry_legacy_tiers(entry));
    }
    let ordered: Vec<&str> = TIER_KEYS_BEST_FIRST.iter().rev().copied().collect();
    let ranks: Vec<usize> = tiers
        .iter()
        .map(|tier| ordered.iter().position(|t| t == tier).unwrap_or(0))
        .collect();
    let lo = ranks.iter().min().copied().unwrap_or(0);
    let hi = ranks.iter().max().copied().unwrap_or(0);
    (ordered[lo].to_owned(), ordered[hi].to_owned())
}

/// Rank of a tier (0 = worst). Unknown tiers rank below everything.
pub fn tier_rank_worst_zero(tier: &str) -> usize {
    TIER_KEYS_BEST_FIRST
        .iter()
        .rev()
        .position(|t| *t == tier)
        .unwrap_or(0)
}

/// Clamp a cutoff tier into the [min, max] rank range.
pub fn clamp_cutoff(cutoff: &str, quality_min: &str, quality_max: &str) -> String {
    let rank = tier_rank_worst_zero(cutoff)
        .max(tier_rank_worst_zero(quality_min))
        .min(tier_rank_worst_zero(quality_max));
    TIER_KEYS_BEST_FIRST[TIER_KEYS_BEST_FIRST.len() - 1 - rank].to_owned()
}

/// Read-only recipe verdict.
pub fn recipe_status(
    recipe: &[QualityRecipeEntry],
    flac_mp3_only: bool,
    recipe_valid: bool,
    recipe_error: Option<&str>,
) -> (QualityRecipeStatus, Option<String>) {
    if !recipe.is_empty() {
        if !recipe_valid {
            return (
                QualityRecipeStatus::Invalid,
                Some(recipe_error.unwrap_or("invalid quality recipe").to_owned()),
            );
        }
        if !flac_mp3_only {
            return (
                QualityRecipeStatus::NonConvertible,
                Some("v2 recipes require FLAC/MP3-only mode".to_owned()),
            );
        }
        return (QualityRecipeStatus::V2, None);
    }
    if flac_mp3_only {
        (QualityRecipeStatus::V1, None)
    } else {
        (
            QualityRecipeStatus::NonConvertible,
            Some("legacy policy allows codecs outside FLAC/MP3".to_owned()),
        )
    }
}
