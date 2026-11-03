//! Quality tiers for landed files, judged from the files themselves.
//!
//! Search results only promise a quality; the files on disk are the truth.
//! One linear axis, worst to best: `low`, `mp3_192`,
//! `mp3_256`, `mp3_320`, `lossless`. Lossy codecs share the bitrate bands;
//! an MP4-family file counts as lossless only when the header reports a
//! bit depth (ALAC).

use droppedneedle_kernel::runtime_config::sections::{
    QualityRecipeEntry, RecipeFormat, RecipeQuality, tier_rank,
};

/// Containers that are always lossless.
const LOSSLESS: [&str; 4] = ["flac", "wav", "ape", "wv"];
/// Containers that are lossless only with a bit depth (ALAC).
const MP4_FAMILY: [&str; 3] = ["m4a", "mp4", "m4b"];

/// The tier one file reaches.
pub fn tier_for(format: &str, bitrate_kbps: Option<u32>, bit_depth: Option<u8>) -> &'static str {
    let format = format.to_ascii_lowercase();
    if LOSSLESS.contains(&format.as_str()) {
        return "lossless";
    }
    if MP4_FAMILY.contains(&format.as_str()) && bit_depth.is_some() {
        return "lossless";
    }
    match bitrate_kbps.unwrap_or(0) {
        rate if rate >= 320 => "mp3_320",
        rate if rate >= 256 => "mp3_256",
        rate if rate >= 192 => "mp3_192",
        _ => "low",
    }
}

/// A folder is only as good as its worst file.
pub fn worst<'a>(tiers: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    tiers
        .into_iter()
        .min_by_key(|tier| tier_rank(tier).unwrap_or(0))
}

/// Whether `tier` sits inside the accepted `[min, max]` band. An unknown
/// band bound accepts, so a broken setting never blocks every import.
pub fn in_range(tier: &str, min: &str, max: &str) -> bool {
    let rank = tier_rank(tier).unwrap_or(0);
    let low = tier_rank(min).unwrap_or(0);
    let high = tier_rank(max).unwrap_or(usize::MAX);
    low <= rank && rank <= high
}

/// What the quality recipe says about one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeFit {
    /// This recipe entry admits it. Earlier entries are preferred.
    Step(usize),
    /// The format is listed but the bitrate is not shown, so only the file
    /// itself can tell.
    Unshown,
    /// The recipe does not list it.
    Outside,
}

/// The depth and rate a FLAC entry admits at most, narrowest first. `None`
/// for an exact custom entry and for MP3 entries.
fn flac_ceiling(quality: RecipeQuality) -> Option<(usize, i64, i64)> {
    match quality {
        RecipeQuality::Cd => Some((1, 16, 48_000)),
        RecipeQuality::Bit24Khz48 => Some((2, 24, 48_000)),
        RecipeQuality::Bit24Khz96 => Some((3, 24, 96_000)),
        RecipeQuality::Bit24Khz192 => Some((4, 24, 192_000)),
        RecipeQuality::HiRes => Some((5, i64::MAX, i64::MAX)),
        _ => None,
    }
}

/// Whether the recipe has any entry of `format`, the name a landed file or
/// a search result reads by. Anything but FLAC and MP3 never has one.
pub fn names_format(recipe: &[QualityRecipeEntry], format: &str) -> bool {
    let wanted = if format.eq_ignore_ascii_case("flac") {
        RecipeFormat::Flac
    } else if format.eq_ignore_ascii_case("mp3") {
        RecipeFormat::Mp3
    } else {
        return false;
    };
    recipe.iter().any(|entry| entry.format == wanted)
}

/// Judge one file against the recipe, from what is known of it. A value that
/// is not shown never refuses a file.
///
/// A FLAC takes the narrowest entry that admits it, so `hi_res, 24_192, cd`
/// prefers higher resolutions in that order, where the first match would
/// rank every FLAC on `hi_res`. A hidden resolution reads as 16-bit/44.1 kHz,
/// so it never beats a shown higher one.
pub fn recipe_fit(
    recipe: &[QualityRecipeEntry],
    format: &str,
    bitrate_kbps: Option<i32>,
    bit_depth: Option<i32>,
    sample_rate_hz: Option<i32>,
) -> RecipeFit {
    if !names_format(recipe, format) {
        return RecipeFit::Outside;
    }
    if format.eq_ignore_ascii_case("mp3") {
        let Some(bitrate) = bitrate_kbps.map(i64::from) else {
            return RecipeFit::Unshown;
        };
        return recipe
            .iter()
            .position(|entry| {
                entry.format == RecipeFormat::Mp3
                    && bitrate >= entry.min_bitrate_kbps.unwrap_or(16)
                    && entry.max_bitrate_kbps.is_none_or(|max| bitrate <= max)
            })
            .map_or(RecipeFit::Outside, RecipeFit::Step);
    }
    let depth = bit_depth.map(i64::from);
    let rate = sample_rate_hz.map(i64::from);
    let flac = || {
        recipe
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.format == RecipeFormat::Flac)
    };
    let exact = flac().find(|(_, entry)| {
        entry.quality == RecipeQuality::Custom
            && depth.is_some()
            && rate.is_some()
            && entry.bit_depth == depth
            && entry.sample_rate_hz == rate
    });
    if let Some((index, _)) = exact {
        return RecipeFit::Step(index);
    }
    let (seen_depth, seen_rate) = (depth.unwrap_or(16), rate.unwrap_or(44_100));
    let narrowest = flac()
        .filter_map(|(index, entry)| {
            let (width, most_depth, most_rate) = flac_ceiling(entry.quality)?;
            (seen_depth <= most_depth && seen_rate <= most_rate).then_some((width, index))
        })
        .min();
    if let Some((_, index)) = narrowest {
        return RecipeFit::Step(index);
    }
    // Only exact entries left: a hidden axis cannot contradict one.
    if depth.is_some() && rate.is_some() {
        return RecipeFit::Outside;
    }
    flac()
        .find(|(_, entry)| {
            entry.quality == RecipeQuality::Custom
                && depth.is_none_or(|depth| entry.bit_depth == Some(depth))
                && rate.is_none_or(|rate| entry.sample_rate_hz == Some(rate))
        })
        .map_or(RecipeFit::Outside, |(index, _)| RecipeFit::Step(index))
}

/// Whether `candidate` is strictly better than `held` (equal never
/// replaces).
pub fn beats(candidate: &str, held: &str) -> bool {
    tier_rank(candidate).unwrap_or(0) > tier_rank(held).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use droppedneedle_kernel::runtime_config::sections::recipe_for_range;

    use super::*;

    fn flac(quality: RecipeQuality) -> QualityRecipeEntry {
        QualityRecipeEntry {
            format: RecipeFormat::Flac,
            quality,
            min_bitrate_kbps: None,
            max_bitrate_kbps: None,
            bit_depth: None,
            sample_rate_hz: None,
        }
    }

    fn mp3(min: i64, max: Option<i64>) -> QualityRecipeEntry {
        QualityRecipeEntry {
            format: RecipeFormat::Mp3,
            quality: RecipeQuality::Custom,
            min_bitrate_kbps: Some(min),
            max_bitrate_kbps: max,
            bit_depth: None,
            sample_rate_hz: None,
        }
    }

    fn exact(depth: i64, rate: i64) -> QualityRecipeEntry {
        QualityRecipeEntry {
            bit_depth: Some(depth),
            sample_rate_hz: Some(rate),
            ..flac(RecipeQuality::Custom)
        }
    }

    // What automatic downloads may take: each row is one recipe and one file.
    #[test]
    fn the_recipe_decides_what_a_file_may_be() {
        use RecipeFit::{Outside, Step, Unshown};
        let default = recipe_for_range("mp3_320", "lossless", &[]);
        let capped = vec![flac(RecipeQuality::Cd), mp3(320, None)];
        let gap = vec![mp3(320, None), mp3(192, Some(255))];
        let custom = vec![exact(16, 44_100)];
        let best_available = vec![
            flac(RecipeQuality::HiRes),
            flac(RecipeQuality::Bit24Khz192),
            flac(RecipeQuality::Bit24Khz96),
            flac(RecipeQuality::Bit24Khz48),
            flac(RecipeQuality::Cd),
            mp3(320, None),
        ];
        type Row<'a> = (
            &'a [QualityRecipeEntry],
            &'a str,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            RecipeFit,
        );
        let rows: &[Row<'_>] = &[
            (&default, "ogg", Some(320), None, None, Outside),
            (&default, "opus", Some(320), None, None, Outside),
            (&default, "m4a", None, Some(16), None, Outside),
            (&default, "wav", None, Some(16), Some(44_100), Outside),
            (&default, "dsf", None, Some(1), Some(2_822_400), Outside),
            (&default, "flac", None, Some(24), Some(192_000), Step(0)),
            (&default, "FLAC", None, None, None, Step(0)),
            (&default, "mp3", Some(320), None, None, Step(1)),
            (&default, "mp3", Some(256), None, None, Outside),
            (&default, "mp3", None, None, None, Unshown),
            (&capped, "flac", None, Some(16), Some(44_100), Step(0)),
            (&capped, "flac", None, Some(24), Some(44_100), Outside),
            (&capped, "flac", None, Some(16), Some(96_000), Outside),
            (&capped, "flac", None, Some(24), None, Outside),
            (&capped, "flac", None, None, None, Step(0)),
            (&gap, "mp3", Some(256), None, None, Outside),
            (&gap, "mp3", Some(192), None, None, Step(1)),
            (&gap, "flac", None, None, None, Outside),
            (&custom, "flac", None, Some(16), Some(44_100), Step(0)),
            (&custom, "flac", None, Some(16), Some(48_000), Outside),
            (&custom, "flac", None, Some(24), None, Outside),
            (&custom, "flac", None, None, None, Step(0)),
            (
                &best_available,
                "flac",
                None,
                Some(32),
                Some(384_000),
                Step(0),
            ),
            (
                &best_available,
                "flac",
                None,
                Some(24),
                Some(192_000),
                Step(1),
            ),
            (
                &best_available,
                "flac",
                None,
                Some(24),
                Some(96_000),
                Step(2),
            ),
            (
                &best_available,
                "flac",
                None,
                Some(24),
                Some(48_000),
                Step(3),
            ),
            (
                &best_available,
                "flac",
                None,
                Some(16),
                Some(44_100),
                Step(4),
            ),
            (&best_available, "flac", None, None, None, Step(4)),
            (&best_available, "mp3", Some(320), None, None, Step(5)),
        ];
        for (recipe, format, bitrate, depth, rate, want) in rows {
            assert_eq!(
                recipe_fit(recipe, format, *bitrate, *depth, *rate),
                *want,
                "{format} {bitrate:?} {depth:?} {rate:?} on {recipe:?}"
            );
        }
    }

    #[test]
    fn tiers_follow_v2() {
        assert_eq!(tier_for("FLAC", None, None), "lossless");
        assert_eq!(tier_for("m4a", Some(256), None), "mp3_256");
        assert_eq!(tier_for("m4a", Some(900), Some(16)), "lossless");
        assert_eq!(tier_for("mp3", Some(320), None), "mp3_320");
        assert_eq!(tier_for("ogg", Some(160), None), "low");
        assert_eq!(worst(["lossless", "mp3_256", "mp3_320"]), Some("mp3_256"));
        assert!(in_range("mp3_320", "mp3_320", "lossless"));
        assert!(!in_range("mp3_256", "mp3_320", "lossless"));
        assert!(beats("lossless", "mp3_320"));
        assert!(!beats("mp3_320", "mp3_320"));
    }
}
