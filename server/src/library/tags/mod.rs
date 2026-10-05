//! Audio tags: read tags, probe streams, fingerprint, and save safely.
//!
//! Tag reads go through lofty, technical
//! probe and decode through symphonia (with the libopus adapter for Opus),
//! and AcoustID-style fingerprints through rusty-chromaprint with the
//! measured pipeline (f64 downmix, rubato cubic-256 to 11025 Hz, test2
//! preset, URL-safe unpadded base64). Every mutation goes through the
//! save wrapper, which refuses loudly rather than silently dropping data.
//!
//! WMA is unrecognized everywhere here: there is no ASF code,
//! and every entry point rejects `.wma` before touching the file.

pub mod fingerprint;
pub mod probe;
pub mod read;
pub mod save;

pub use fingerprint::{Fingerprint, generate_fingerprint};
pub use probe::{AudioInfo, probe};
pub use read::{AudioArtistCredit, AudioTag, read_cover_art, read_tags};
pub use save::{Refusal, SaveReport, TagEdit, save_tags};

use std::path::Path;

use thiserror::Error;

/// Audio container the tag code understands, routed by file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Flac,
    Mp3,
    Ogg,
    Opus,
    M4a,
    Aac,
    Wav,
}

impl AudioFormat {
    /// Canonical lowercase name, matching v2's `file_format` values.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
            Self::Ogg => "ogg",
            Self::Opus => "opus",
            Self::M4a => "m4a",
            Self::Aac => "aac",
            Self::Wav => "wav",
        }
    }
}

/// Route a path to its container. WMA and anything else unknown are
/// rejected here so no reader ever sees them.
pub fn format_for_path(path: &Path) -> Result<AudioFormat, TagsError> {
    let extension = path
        .extension()
        .and_then(|raw| raw.to_str())
        .unwrap_or("")
        .to_lowercase();
    match extension.as_str() {
        "flac" => Ok(AudioFormat::Flac),
        "mp3" => Ok(AudioFormat::Mp3),
        "ogg" | "oga" => Ok(AudioFormat::Ogg),
        "opus" => Ok(AudioFormat::Opus),
        "m4a" | "m4b" | "mp4" => Ok(AudioFormat::M4a),
        "aac" => Ok(AudioFormat::Aac),
        "wav" | "wave" => Ok(AudioFormat::Wav),
        other => Err(TagsError::UnrecognizedExtension {
            extension: other.to_owned(),
        }),
    }
}

/// Every tag failure. Save refusals carry their own rich reason.
#[derive(Debug, Error)]
pub enum TagsError {
    #[error("unrecognized audio extension '.{extension}' (WMA is cut)")]
    UnrecognizedExtension { extension: String },
    #[error("io error on '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("tag read failed for '{path}': {reason}")]
    TagRead { path: String, reason: String },
    #[error("probe failed for '{path}': {reason}")]
    Probe { path: String, reason: String },
    #[error("decode failed for '{path}': {reason}")]
    Decode { path: String, reason: String },
    #[error("fingerprint failed for '{path}': {reason}")]
    Fingerprint { path: String, reason: String },
    #[error("save refused for '{path}': {refusal}")]
    SaveRefused { path: String, refusal: Refusal },
}

impl TagsError {
    #[must_use]
    pub fn is_unrecognized(&self) -> bool {
        matches!(self, Self::UnrecognizedExtension { .. })
    }
}
