//! Quality tiers for landed files, judged from the files themselves.
//!
//! Search results only promise a quality; the files on disk are the truth
//! (v2 `local_probe`). One linear axis, worst to best: `low`, `mp3_192`,
//! `mp3_256`, `mp3_320`, `lossless`. Lossy codecs share the bitrate bands;
//! an MP4-family file counts as lossless only when the header reports a
//! bit depth (ALAC), as in v2.

use crate::runtime_config::sections::tier_rank;

/// Containers that are always lossless.
const LOSSLESS: [&str; 4] = ["flac", "wav", "ape", "wv"];
/// Containers that are lossless only with a bit depth (ALAC).
const MP4_FAMILY: [&str; 3] = ["m4a", "mp4", "m4b"];

/// The tier one file reaches (v2 `tier_for`).
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

/// Whether `candidate` is strictly better than `held` (equal never
/// replaces).
pub fn beats(candidate: &str, held: &str) -> bool {
    tier_rank(candidate).unwrap_or(0) > tier_rank(held).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
