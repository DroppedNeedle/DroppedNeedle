//! Audio tags: read tags, probe streams, fingerprint, and save safely.
//!
//! Tag reads go through lofty, technical
//! probe and decode through symphonia (with the libopus adapter for Opus),
//! and AcoustID-style fingerprints through rusty-chromaprint with the
//! measured pipeline (f64 downmix, rubato cubic-256 to 11025 Hz, test2
//! preset, URL-safe unpadded base64). Every mutation goes through the
//! save wrapper, which refuses loudly rather than silently dropping data;
//! `fields` lists what it writes (Picard's tag set) and `picard` fills
//! that set from an identified release.
//!
//! `lyrics` reads the lyrics a file carries (a `.lrc` beside it, or its
//! lyrics tag) and holds the one lyrics parser.
//!
//! WMA is unrecognized everywhere here: there is no ASF code,
//! and every entry point rejects `.wma` before touching the file.

pub mod fields;
pub mod fingerprint;
pub mod loudness;
pub mod lyrics;
pub mod picard;
pub mod probe;
pub mod read;
pub mod save;

pub use fields::TagField;
pub use fingerprint::{Fingerprint, generate_fingerprint};
pub use probe::{AudioInfo, probe};
pub use read::{AudioArtistCredit, AudioTag, read_cover_art, read_tags};
pub use save::{
    CapturedAtom, CapturedTags, CapturedValue, FieldDocument, Refusal, SaveReport, TagEdit,
    document_from_captured, read_document, read_fields, save_tags,
};

use std::path::Path;

use thiserror::Error;

/// Audio container the tag code understands, routed by file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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
    format_for_extension(&extension).ok_or(TagsError::UnrecognizedExtension { extension })
}

/// The container a lowercase extension, without its dot, is read as, or
/// `None` when no reader takes it. Soulseek folders count a file as audio
/// exactly when this returns a container.
#[must_use]
pub fn format_for_extension(extension: &str) -> Option<AudioFormat> {
    match extension {
        "flac" => Some(AudioFormat::Flac),
        "mp3" => Some(AudioFormat::Mp3),
        "ogg" | "oga" => Some(AudioFormat::Ogg),
        "opus" => Some(AudioFormat::Opus),
        "m4a" | "m4b" | "mp4" => Some(AudioFormat::M4a),
        "aac" => Some(AudioFormat::Aac),
        "wav" | "wave" => Some(AudioFormat::Wav),
        _ => None,
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
