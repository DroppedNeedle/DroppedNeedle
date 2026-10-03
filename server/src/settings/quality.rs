//! Acquisition-quality snapshot and summary sentence (pure).
//!
//! Ports the v2 `build_snapshot`/`compose_summary` contract used by the
//! policy summary and impact endpoints: the canonical five tiers, the
//! default order derivation, the v2 recipe legacy projection, and the
//! user-facing summary sentence. No I/O: the caller supplies the policy.

use droppedneedle::runtime_config::sections::{QualityRecipeEntry, derive_default_order};
use serde::{Deserialize, Serialize};

/// Canonical five tiers, best first.
pub const TIER_KEYS_BEST_FIRST: [&str; 5] = ["lossless", "mp3_320", "mp3_256", "mp3_192", "low"];

/// Lossy band floors, best first.
const LOSSY_BANDS: [(i64, &str); 3] = [(320, "mp3_320"), (256, "mp3_256"), (192, "mp3_192")];

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
            "MP3 {}-{}-{} kbps",
            entry.min_bitrate_kbps.unwrap_or(0),
            entry.target_bitrate_kbps.unwrap_or(0),
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

/// Project every tier in the derived contiguous range exactly once.
/// Explicit recipe entries establish ordering constraints; missing tiers
/// slot into canonical positions between them.
pub fn legacy_recipe_order(entries: &[QualityRecipeEntry]) -> Vec<String> {
    let mut touched = std::collections::BTreeSet::new();
    for entry in entries {
        touched.extend(recipe_entry_legacy_tiers(entry));
    }
    let positions: Vec<usize> = touched
        .iter()
        .map(|tier| {
            TIER_KEYS_BEST_FIRST
                .iter()
                .position(|t| t == tier)
                .unwrap_or(0)
        })
        .collect();
    let first = positions.iter().min().copied().unwrap_or(0);
    let last = positions.iter().max().copied().unwrap_or(0);
    let contiguous: Vec<&str> = TIER_KEYS_BEST_FIRST[first..=last].to_vec();

    let mut order: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let entry_tiers = recipe_entry_legacy_tiers(entry);
        for tier in TIER_KEYS_BEST_FIRST {
            if entry_tiers.contains(&tier) && !seen.contains(tier) {
                order.push(tier.to_owned());
                seen.insert(tier);
            }
        }
    }
    for tier in contiguous {
        if seen.contains(tier) {
            continue;
        }
        let position = TIER_KEYS_BEST_FIRST
            .iter()
            .position(|t| *t == tier)
            .unwrap_or(0);
        let insertion = order
            .iter()
            .position(|existing| {
                TIER_KEYS_BEST_FIRST
                    .iter()
                    .position(|t| t == existing)
                    .unwrap_or(0)
                    > position
            })
            .unwrap_or(order.len());
        order.insert(insertion, tier.to_owned());
        seen.insert(tier);
    }
    order
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
    /// Submitted preference order (v1).
    pub quality_preference_order: Vec<String>,
    /// Canonical recipe (v2).
    pub quality_recipe: Vec<QualityRecipeEntry>,
    /// FLAC/MP3-only gate.
    pub flac_mp3_only: bool,
    /// Within-lossless target.
    pub lossless_preference: String,
    /// Lossless bit-depth ceiling.
    pub lossless_max_bit_depth: Option<i64>,
    /// Lossless sample-rate ceiling.
    pub lossless_max_sample_rate_hz: Option<i64>,
    /// Unknown-quality handling.
    pub unknown_quality_behavior: String,
}

/// Compose the saved product contract shown to users.
pub fn compose_summary(inputs: &SummaryInputs) -> String {
    let mut sentence = if !inputs.quality_recipe.is_empty() && inputs.flac_mp3_only {
        let mut sentence = format!("Try {}", recipe_entry_label(&inputs.quality_recipe[0]));
        for entry in &inputs.quality_recipe[1..] {
            sentence += &format!(", then {}", recipe_entry_label(entry));
        }
        sentence += ".";
        sentence
    } else {
        let order = &inputs.quality_preference_order;
        if order.is_empty() {
            return "No quality preference configured.".to_owned();
        }
        let mut sentence = format!("Try {}", tier_label(&order[0]));
        for tier in &order[1..] {
            sentence += &format!(", then {}", tier_label(tier));
        }
        sentence += ".";
        let pref = inputs.lossless_preference.as_str();
        if pref != "highest"
            && order.iter().any(|t| t == "lossless")
            && matches!(pref, "cd" | "24_48" | "24_96" | "24_192")
        {
            let detail = match pref {
                "cd" => "CD-quality (16-bit/48 kHz)",
                "24_48" => "up-to-24-bit/48 kHz",
                "24_96" => "up-to-24-bit/96 kHz",
                _ => "up-to-24-bit/192 kHz",
            };
            sentence += &format!(" Lossless prefers {detail} copies.");
        }
        sentence
    };
    let mut cap_bits = Vec::new();
    if let Some(depth) = inputs.lossless_max_bit_depth {
        cap_bits.push(format!("{depth}-bit maximum bit depth"));
    }
    if let Some(rate) = inputs.lossless_max_sample_rate_hz {
        cap_bits.push(format!(
            "{} kHz maximum sample rate",
            (rate as f64 / 1000.0).round() as i64
        ));
    }
    if !cap_bits.is_empty() {
        sentence += &format!(" Never acquire above {}.", cap_bits.join(" or "));
    }
    match inputs.unknown_quality_behavior.as_str() {
        "review" => sentence += " Never acquire unknown-quality audio automatically.",
        "reject" => sentence += " Unknown-quality copies are excluded.",
        _ => sentence += " Unknown-quality copies are a last resort.",
    }
    sentence
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

/// Whether the candidate order reproduces the legacy default shape
/// (summary `legacy_rollback_compatible` / impact `legacy_representable`).
#[allow(clippy::too_many_arguments)]
pub fn is_legacy_default_shape(
    order: &[String],
    quality_min: &str,
    quality_max: &str,
    lossless_preference: &str,
    lossless_max_bit_depth: Option<i64>,
    lossless_max_sample_rate_hz: Option<i64>,
    preferred_lossy_bitrate_kbps: Option<i64>,
    lossy_min_bitrate_kbps: Option<i64>,
    lossy_max_bitrate_kbps: Option<i64>,
    unknown_quality_behavior: &str,
) -> bool {
    let derived = derive_default_order(quality_min, quality_max);
    order == derived.as_slice()
        && lossless_preference == "highest"
        && lossless_max_bit_depth.is_none()
        && lossless_max_sample_rate_hz.is_none()
        && preferred_lossy_bitrate_kbps.is_none()
        && lossy_min_bitrate_kbps.is_none()
        && lossy_max_bitrate_kbps.is_none()
        && unknown_quality_behavior == "allow_as_fallback"
}

/// Lossy band for a bitrate (best-first scan).
pub fn band_for_bitrate(rate: i64) -> &'static str {
    for (floor, key) in LOSSY_BANDS {
        if rate >= floor {
            return key;
        }
    }
    "low"
}
