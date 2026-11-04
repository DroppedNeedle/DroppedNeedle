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
pub mod mpeg_length;
pub mod picard;
pub mod probe;
pub mod read;
pub mod save;

pub use fields::TagField;
pub use fingerprint::{Fingerprint, generate_fingerprint};
pub use probe::{AudioInfo, probe};
pub use read::{AudioArtistCredit, AudioTag, read_cover_art, read_cover_art_from, read_tags};
pub use save::{
    CapturedAtom, CapturedTags, CapturedValue, FieldDocument, Refusal, SaveReport, TagEdit,
    document_from_captured, read_document, read_fields, save_tags,
};

use std::fs::File;
use std::path::Path;

use droppedneedle_kernel::beneath::{RootLinks, open_file_beneath};
use thiserror::Error;

/// A track or sidecar file and the folder it is read beneath: the library
/// folder it lies in, which is an admin's setting and may itself be a
/// link, or the downloads or scratch folder holding it. Whoever writes
/// that folder can swap any file or folder below it for a link or a FIFO,
/// so every read of a file's bytes here opens it from the folder one name
/// at a time without following a link (`kernel::beneath`): a link met on
/// the way fails the open, and a FIFO fails rather than hanging it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackFile<'a> {
    root: &'a Path,
    path: &'a Path,
}

impl<'a> TrackFile<'a> {
    /// The file at `path`, read beneath `root`.
    #[must_use]
    pub fn new(root: &'a Path, path: &'a Path) -> Self {
        Self { root, path }
    }

    /// The file at `path` that lies at `relative` below its library
    /// folder: the folder is `path` less as many components as `relative`
    /// has (the two may be spelled in different Unicode forms, so their
    /// lengths say nothing). `None` when `relative` is empty or longer
    /// than `path`.
    #[must_use]
    pub fn in_library(path: &'a Path, relative: &str) -> Option<Self> {
        let depth = Path::new(relative).components().count();
        let root = path.ancestors().nth(depth).filter(|_| depth > 0)?;
        Some(Self { root, path })
    }

    /// Another file below the same folder, such as a track's `.lrc`.
    #[must_use]
    pub fn beside(self, path: &'a Path) -> Self {
        Self {
            root: self.root,
            path,
        }
    }

    /// The file's path.
    #[must_use]
    pub fn path(self) -> &'a Path {
        self.path
    }

    /// The folder the file is read beneath.
    #[must_use]
    pub fn root(self) -> &'a Path {
        self.root
    }

    /// Open the regular file for reading. Blocking.
    pub fn open(self) -> std::io::Result<File> {
        open_file_beneath(self.root, self.path, RootLinks::Follow)
    }

    /// The whole file's bytes. Blocking.
    pub fn read(self) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut self.open()?, &mut bytes)?;
        Ok(bytes)
    }
}

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
