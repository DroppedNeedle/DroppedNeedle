//! Download policy: quality tiers, the closed quality recipe, and timeouts.
//!
//! Ported from `backend/models/acquisition_quality.py` (semantics governed
//! by the owner-signed spec; field names mirror the frontend hand-mirrored
//! types). The recipe is deliberately closed over formats and quality
//! identifiers: adding a codec later is an explicit schema change rather
//! than accepting arbitrary codec strings.

use std::time::Duration;

/// The closed v2 recipe admits only these two containers.
pub const RECIPE_FORMATS: [&str; 2] = ["flac", "mp3"];

/// Closed MP3 quality identifiers (v2 `MP3_RECIPE_QUALITIES`).
pub const MP3_RECIPE_QUALITIES: [&str; 5] =
    ["below_192", "192_255", "256_319", "320_plus", "custom"];

/// Closed FLAC quality identifiers (v2 `FLAC_RECIPE_QUALITIES`).
pub const FLAC_RECIPE_QUALITIES: [&str; 6] = ["cd", "24_48", "24_96", "24_192", "hi_res", "custom"];

/// Canonical `(min, target, max)` kbps bounds for the standard MP3 recipe
/// qualities (v2 `_MP3_STANDARD_BOUNDS`).
pub fn mp3_standard_bounds(quality: &str) -> Option<(i32, i32, Option<i32>)> {
    match quality {
        "below_192" => Some((16, 128, Some(191))),
        "192_255" => Some((192, 192, Some(255))),
        "256_319" => Some((256, 256, Some(319))),
        "320_plus" => Some((320, 320, None)),
        _ => None,
    }
}

/// Known-unimportable audio containers (DSD families; v2
/// `NOT_IMPORTABLE_EXTENSIONS`). A declared DSD release must not spend
/// bandwidth even when a noisy title claims "lossless".
pub const NOT_IMPORTABLE_EXTENSIONS: [&str; 3] = ["dsd", "dsf", "dff"];

/// Fixed 6-step lossless detail ladder INSIDE the canonical `lossless` tier
/// (v2 `LOSSLESS_DETAIL_STEPS`; never new canonical tiers):
/// 0 cd (at most 16-bit / 48 kHz), 1 24_48, 2 24_96, 3 24_192,
/// 4 hi_res (above 24-bit or 192 kHz), 5 partial (resolution partial/unknown).
pub const LOSSLESS_DETAIL_STEPS: [&str; 6] =
    ["cd", "24_48", "24_96", "24_192", "hi_res", "partial"];

/// CD bit depth: the ceiling of the `cd` step.
pub const CD_BIT_DEPTH: i32 = 16;

/// Map trusted depth/rate onto the fixed ladder (v2
/// `lossless_detail_step`). Both axes present -> steps 0-4 by ascending
/// order match; either axis absent -> the `partial` step. Requires
/// Hz-normalised integers.
#[must_use]
pub fn lossless_detail_step(bit_depth: Option<i32>, sample_rate_hz: Option<i32>) -> usize {
    let (Some(depth), Some(rate)) = (bit_depth, sample_rate_hz) else {
        return 5;
    };
    if depth <= CD_BIT_DEPTH && rate <= 48_000 {
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

/// One ordered, closed format-quality recipe entry (v2
/// `QualityRecipeEntry`). Standard entries carry their canonical bounds;
/// custom entries carry only the fields relevant to their format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityRecipeEntry {
    pub format: String,
    pub quality: String,
    pub min_bitrate_kbps: Option<i32>,
    pub target_bitrate_kbps: Option<i32>,
    pub max_bitrate_kbps: Option<i32>,
    pub bit_depth: Option<i32>,
    pub sample_rate_hz: Option<i32>,
}

/// Why a recipe entry or list was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RecipeError(pub String);

impl QualityRecipeEntry {
    /// Build and validate one entry (v2 `_validate_recipe_entry_fields`).
    /// Standard MP3 entries with omitted bounds are canonicalised to their
    /// fixed bounds.
    pub fn new(
        format: &str,
        quality: &str,
        min_bitrate_kbps: Option<i32>,
        target_bitrate_kbps: Option<i32>,
        max_bitrate_kbps: Option<i32>,
        bit_depth: Option<i32>,
        sample_rate_hz: Option<i32>,
    ) -> Result<Self, RecipeError> {
        if !RECIPE_FORMATS.contains(&format) {
            return Err(RecipeError(format!(
                "unsupported quality recipe format: {format:?}"
            )));
        }
        let allowed = if format == "mp3" {
            &MP3_RECIPE_QUALITIES[..]
        } else {
            &FLAC_RECIPE_QUALITIES[..]
        };
        if !allowed.contains(&quality) {
            return Err(RecipeError(format!(
                "unsupported {format} quality recipe value: {quality:?}"
            )));
        }
        if format == "mp3" {
            if bit_depth.is_some() || sample_rate_hz.is_some() {
                return Err(RecipeError(
                    "MP3 recipe entries cannot define FLAC resolution".to_owned(),
                ));
            }
            let (minimum, target, maximum) = if quality == "custom" {
                let minimum = strict_int(min_bitrate_kbps, "min_bitrate_kbps", 16, 2048)?;
                let target = strict_int(target_bitrate_kbps, "target_bitrate_kbps", 16, 2048)?;
                let maximum = strict_int(max_bitrate_kbps, "max_bitrate_kbps", 16, 2048)?;
                if !(minimum <= target && target <= maximum) {
                    return Err(RecipeError(
                        "custom MP3 recipe requires min_bitrate_kbps <= \
                         target_bitrate_kbps <= max_bitrate_kbps"
                            .to_owned(),
                    ));
                }
                (minimum, target, maximum)
            } else {
                let (exp_min, exp_target, exp_max) = mp3_standard_bounds(quality)
                    .ok_or_else(|| RecipeError(format!("unknown MP3 recipe quality: {quality}")))?;
                let given = (min_bitrate_kbps, target_bitrate_kbps, max_bitrate_kbps);
                if given != (None, None, None)
                    && given != (Some(exp_min), Some(exp_target), exp_max)
                {
                    return Err(RecipeError(
                        "standard MP3 recipe fields must use their canonical bounds".to_owned(),
                    ));
                }
                (exp_min, exp_target, exp_max.unwrap_or(i32::MAX))
            };
            return Ok(Self {
                format: format.to_owned(),
                quality: quality.to_owned(),
                min_bitrate_kbps: Some(minimum),
                target_bitrate_kbps: Some(target),
                max_bitrate_kbps: (maximum != i32::MAX).then_some(maximum),
                bit_depth: None,
                sample_rate_hz: None,
            });
        }
        if min_bitrate_kbps.is_some() || target_bitrate_kbps.is_some() || max_bitrate_kbps.is_some()
        {
            return Err(RecipeError(
                "FLAC recipe entries cannot define a bitrate".to_owned(),
            ));
        }
        if quality == "custom" {
            let depth = strict_int(bit_depth, "bit_depth", 1, 64)?;
            let rate = strict_int(sample_rate_hz, "sample_rate_hz", 8_000, 768_000)?;
            return Ok(Self {
                format: format.to_owned(),
                quality: quality.to_owned(),
                min_bitrate_kbps: None,
                target_bitrate_kbps: None,
                max_bitrate_kbps: None,
                bit_depth: Some(depth),
                sample_rate_hz: Some(rate),
            });
        }
        if bit_depth.is_some() || sample_rate_hz.is_some() {
            return Err(RecipeError(
                "standard FLAC recipe entries cannot define exact resolution".to_owned(),
            ));
        }
        Ok(Self {
            format: format.to_owned(),
            quality: quality.to_owned(),
            min_bitrate_kbps: None,
            target_bitrate_kbps: None,
            max_bitrate_kbps: None,
            bit_depth: None,
            sample_rate_hz: None,
        })
    }

    /// The lossy bounds this entry enforces, or `None` for FLAC entries.
    #[must_use]
    pub fn mp3_bounds(&self) -> Option<(i32, Option<i32>)> {
        if self.format != "mp3" {
            return None;
        }
        Some((self.min_bitrate_kbps.unwrap_or(16), self.max_bitrate_kbps))
    }
}

fn strict_int(value: Option<i32>, field: &str, low: i32, high: i32) -> Result<i32, RecipeError> {
    match value {
        Some(v) if (low..=high).contains(&v) => Ok(v),
        _ => Err(RecipeError(format!(
            "{field} must be an integer between {low} and {high}"
        ))),
    }
}

fn intervals_overlap(
    left_min: i32,
    left_max: Option<i32>,
    right_min: i32,
    right_max: Option<i32>,
) -> bool {
    (left_max.is_none_or(|max| right_min <= max)) && (right_max.is_none_or(|max| left_min <= max))
}

/// Validate recipe uniqueness/overlap (v2 `validate_quality_recipe`):
/// duplicate standard entries rejected, MP3 ranges (including custom) must
/// not overlap, custom FLAC resolutions must be unique.
pub fn validate_quality_recipe(entries: &[QualityRecipeEntry]) -> Result<(), RecipeError> {
    if entries.is_empty() {
        return Err(RecipeError(
            "quality_recipe must contain at least one entry".to_owned(),
        ));
    }
    let mut seen_standard = std::collections::HashSet::new();
    let mut mp3_ranges: Vec<(i32, Option<i32>)> = Vec::new();
    let mut flac_custom_pairs = std::collections::HashSet::new();
    for entry in entries {
        if entry.quality != "custom" {
            let key = (entry.format.clone(), entry.quality.clone());
            if !seen_standard.insert(key) {
                return Err(RecipeError(format!(
                    "quality_recipe contains duplicate {}/{}",
                    entry.format, entry.quality
                )));
            }
        }
        if entry.format == "mp3" {
            let minimum = entry.min_bitrate_kbps.unwrap_or(16);
            let maximum = entry.max_bitrate_kbps;
            for (existing_min, existing_max) in &mp3_ranges {
                if intervals_overlap(minimum, maximum, *existing_min, *existing_max) {
                    if entry.quality == "custom" {
                        return Err(RecipeError(
                            "custom MP3 quality recipe overlaps another MP3 range".to_owned(),
                        ));
                    }
                    return Err(RecipeError("MP3 quality recipe ranges overlap".to_owned()));
                }
            }
            mp3_ranges.push((minimum, maximum));
        } else if entry.quality == "custom" {
            let pair = (
                entry.bit_depth.unwrap_or(0),
                entry.sample_rate_hz.unwrap_or(0),
            );
            if !flac_custom_pairs.insert(pair) {
                return Err(RecipeError(
                    "quality_recipe contains duplicate custom FLAC resolution".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Immutable quality policy pinned to one acquisition (v2
/// `AcquisitionQualitySnapshot`, recipe-bearing shape).
#[derive(Debug, Clone)]
pub struct DownloadPolicy {
    /// Ordered recipe; earlier entries are more preferred.
    pub quality_recipe: Vec<QualityRecipeEntry>,
    /// How unknown-quality evidence is treated when nothing better exists.
    pub allow_unknown_as_fallback: bool,
    /// Search window handed to slskd per query rung (v2 default 30s).
    pub search_timeout: Duration,
    /// Extra poll time past the search window: slskd fills
    /// GET /searches/{id}/responses only after the search completes, which
    /// lands after the searchTimeout window (v2 observed ~12s later on
    /// 0.25.1; grace is 30s or every search returns 0 candidates).
    pub completion_grace: Duration,
    /// Pause between search-state polls (v2: 0.5s).
    pub poll_interval: Duration,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            // v2's canonical preference shape: FLAC first, then 320 MP3.
            // Literals, not `new`: Default cannot fail, and these are the
            // exact canonical values the constructor would produce.
            quality_recipe: vec![
                QualityRecipeEntry {
                    format: "flac".to_owned(),
                    quality: "cd".to_owned(),
                    min_bitrate_kbps: None,
                    target_bitrate_kbps: None,
                    max_bitrate_kbps: None,
                    bit_depth: None,
                    sample_rate_hz: None,
                },
                QualityRecipeEntry {
                    format: "mp3".to_owned(),
                    quality: "320_plus".to_owned(),
                    min_bitrate_kbps: Some(320),
                    target_bitrate_kbps: Some(320),
                    max_bitrate_kbps: None,
                    bit_depth: None,
                    sample_rate_hz: None,
                },
            ],
            allow_unknown_as_fallback: true,
            search_timeout: Duration::from_secs(30),
            completion_grace: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
        }
    }
}

impl DownloadPolicy {
    /// Rank one candidate file against the recipe: the index of the first
    /// entry it satisfies, or `None` when no entry admits it (v2 evaluator
    /// shape — eligibility plus preference step; structured reasons stay
    /// with the orchestrator).
    ///
    /// `bitrate_kbps` is meaningful for lossy only — FLAC bitrate varies
    /// with compression and is NOT a fidelity axis (v2
    /// `AudioQualityEvidence`). A lossless file with partial/unknown
    /// resolution still matches a standard FLAC entry (v2 detail step
    /// `partial`), because the legacy floor preserves migrated Soulseek FLAC
    /// ordering.
    #[must_use]
    pub fn recipe_rank(
        &self,
        extension: &str,
        bitrate_kbps: Option<i32>,
        bit_depth: Option<i32>,
        sample_rate_hz: Option<i32>,
    ) -> Option<usize> {
        if NOT_IMPORTABLE_EXTENSIONS.contains(&extension) {
            return None;
        }
        self.quality_recipe.iter().position(|entry| {
            if entry.format == "mp3" {
                if extension != "mp3" {
                    return false;
                }
                let Some(bitrate) = bitrate_kbps else {
                    // Unknown lossy bitrate: only a custom entry with no
                    // minimum... v2 has none, so unknown bitrates match no
                    // MP3 entry and fall to unknown-quality handling.
                    return false;
                };
                let (minimum, maximum) = entry.mp3_bounds().unwrap_or((16, None));
                bitrate >= minimum && maximum.is_none_or(|max| bitrate <= max)
            } else {
                // FLAC entry: any lossless container the recipe admits.
                // slskd advertises flac/alac/wav/ape/wv (v2 `_LOSSLESS_EXT`);
                // only flac satisfies the closed recipe.
                if extension != "flac" {
                    return false;
                }
                if entry.quality == "custom" {
                    // Custom FLAC pins an exact resolution; partial evidence
                    // cannot prove it (v2: a known axis may prove a cap
                    // rejection but cannot prove a positive step).
                    match (
                        entry.bit_depth,
                        entry.sample_rate_hz,
                        bit_depth,
                        sample_rate_hz,
                    ) {
                        (Some(want_depth), Some(want_rate), Some(depth), Some(rate)) => {
                            depth == want_depth && rate == want_rate
                        }
                        _ => false,
                    }
                } else {
                    // Standard FLAC entry: the file's detail step must not
                    // exceed the entry's step; partial evidence (step 5) only
                    // matches... nothing standard, so fall back to the legacy
                    // comparator: files with absent axes keep their migrated
                    // ordering and stay eligible (v2 `detail_comparator_axes`).
                    let want_step = LOSSLESS_DETAIL_STEPS
                        .iter()
                        .position(|step| *step == entry.quality)
                        .unwrap_or(0);
                    let have_step = lossless_detail_step(bit_depth, sample_rate_hz);
                    have_step <= want_step || have_step == 5
                }
            }
        })
    }
}
