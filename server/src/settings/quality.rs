//! Acquisition-quality snapshot and summary sentence (pure).
//!
//! Ports the v2 `build_snapshot`/`compose_summary` contract used by the
//! policy summary and impact endpoints: the canonical five tiers, the
//! default order derivation, the v2 recipe legacy projection, and the
//! user-facing summary sentence. No I/O: the caller supplies the policy.

use droppedneedle_kernel::runtime_config::sections::QualityRecipeEntry;
use serde::{Deserialize, Serialize};

/// Canonical five tiers, best first.
pub const TIER_KEYS_BEST_FIRST: [&str; 5] = ["lossless", "mp3_320", "mp3_256", "mp3_192", "low"];

/// Tier label for the summary sentence.
fn tier_label(key: &str) -> &str {
    match key {
        "lossless" => "lossless",
        "mp3_320" => "lossy 320 kbps",
        "mp3_256" => "lossy 256-319 kbps",
        "mp3_192" => "lossy 192-255 kbps",
        _ => "lossy below 192 kbps",
    }
}

/// Recipe entry label for the summary sentence.
fn recipe_entry_label(entry: &QualityRecipeEntry) -> String {
    if entry.format == "flac" {
        if entry.quality == "custom" {
            let khz = entry.sample_rate_hz.unwrap_or(0) as f64 / 1000.0;
            return format!(
                "FLAC {}-bit/{} kHz",
                entry.bit_depth.unwrap_or(0),
                khz.round() as i64
            );
        }
        return format!("FLAC {}", entry.quality.replace('_', "/"));
    }
    if entry.quality == "custom" {
        return format!(
            "MP3 {}-{} kbps",
            entry.min_bitrate_kbps.unwrap_or(0),
            entry.max_bitrate_kbps.unwrap_or(0)
        );
    }
    format!("MP3 {} kbps", entry.quality.replace('_', "-"))
}

/// Canonical v1 tiers touched by one recipe entry (rollback projection).
pub fn recipe_entry_legacy_tiers(entry: &QualityRecipeEntry) -> Vec<&'static str> {
    if entry.format == "flac" {
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

/// Inputs the summary sentence needs (duck-typed policy projection).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SummaryInputs {
    /// Accepted tiers, best first (v1).
    pub quality_preference_order: Vec<String>,
    /// Canonical recipe (v2).
    pub quality_recipe: Vec<QualityRecipeEntry>,
    /// FLAC/MP3-only gate.
    pub flac_mp3_only: bool,
}

/// Compose the saved product contract shown to users.
pub fn compose_summary(inputs: &SummaryInputs) -> String {
    let (first, rest) = if !inputs.quality_recipe.is_empty() && inputs.flac_mp3_only {
        let mut labels = inputs.quality_recipe.iter().map(recipe_entry_label);
        (labels.next(), labels.collect::<Vec<_>>())
    } else {
        let mut labels = inputs
            .quality_preference_order
            .iter()
            .map(|tier| tier_label(tier).to_owned());
        (labels.next(), labels.collect::<Vec<_>>())
    };
    let Some(first) = first else {
        return "No quality preference configured.".to_owned();
    };
    let mut sentence = format!("Try {first}");
    for label in rest {
        sentence += &format!(", then {label}");
    }
    sentence + "."
}

/// Read-only recipe verdict: `v1`, `v2`, `non_convertible`, `invalid`.
pub fn recipe_status(
    recipe: &[QualityRecipeEntry],
    flac_mp3_only: bool,
    recipe_valid: bool,
    recipe_error: Option<&str>,
) -> (String, Option<String>) {
    if !recipe.is_empty() {
        if !recipe_valid {
            return (
                "invalid".to_owned(),
                Some(recipe_error.unwrap_or("invalid quality recipe").to_owned()),
            );
        }
        if !flac_mp3_only {
            return (
                "non_convertible".to_owned(),
                Some("v2 recipes require FLAC/MP3-only mode".to_owned()),
            );
        }
        return ("v2".to_owned(), None);
    }
    if flac_mp3_only {
        ("v1".to_owned(), None)
    } else {
        (
            "non_convertible".to_owned(),
            Some("legacy policy allows codecs outside FLAC/MP3".to_owned()),
        )
    }
}
