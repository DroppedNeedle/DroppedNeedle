//! Recognized audio file extensions.
//!
//! The set mirrors v2's scan list with the WMA cut applied: WMA files are
//! unrecognized, and there is no WMA reader anywhere in the tree. Matching is
//! case-insensitive and tolerates a leading dot.

/// Extensions the library engine accepts, lowercase without dots.
pub const RECOGNIZED_AUDIO_EXTENSIONS: &[&str] =
    &["flac", "mp3", "ogg", "m4a", "aac", "wav", "opus"];

/// True when the extension names a supported audio format.
pub fn is_recognized_audio_extension(extension: &str) -> bool {
    let normalized = extension
        .strip_prefix('.')
        .unwrap_or(extension)
        .to_lowercase();
    RECOGNIZED_AUDIO_EXTENSIONS.contains(&normalized.as_str())
}

/// Content type served for a recognized extension, if it is recognized.
pub fn content_type_for_extension(extension: &str) -> Option<&'static str> {
    let normalized = extension
        .strip_prefix('.')
        .unwrap_or(extension)
        .to_lowercase();
    match normalized.as_str() {
        "flac" => Some("audio/flac"),
        "mp3" => Some("audio/mpeg"),
        "ogg" => Some("audio/ogg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "wav" => Some("audio/wav"),
        "opus" => Some("audio/opus"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_set_matches_v2_minus_wma() {
        for extension in ["flac", "mp3", "ogg", "m4a", "aac", "wav", "opus"] {
            assert!(is_recognized_audio_extension(extension), "{extension}");
            assert!(
                is_recognized_audio_extension(&format!(".{extension}")),
                ".{extension}"
            );
            assert!(
                is_recognized_audio_extension(&extension.to_uppercase()),
                "{extension} uppercase"
            );
        }
        assert_eq!(content_type_for_extension("mp3"), Some("audio/mpeg"));
        assert_eq!(content_type_for_extension(".FLAC"), Some("audio/flac"));
    }

    #[test]
    fn wma_is_unrecognized() {
        for extension in ["wma", ".wma", "WMA", ".Wma"] {
            assert!(!is_recognized_audio_extension(extension), "{extension}");
            assert_eq!(content_type_for_extension(extension), None);
        }
        assert!(!is_recognized_audio_extension("txt"));
        assert!(!is_recognized_audio_extension(""));
    }
}
