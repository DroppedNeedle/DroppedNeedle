//! What a save keeps of a file's tag: whether it can keep the tag whole
//! at all, and the items it leaves as they are because undo could not
//! put them back exactly.

use std::fs;
use std::path::Path;

use super::id3::Id3;
use super::mp4::Mp4;
use super::vorbis::Vorbis;
use super::{Refusal, TagFormat};
use crate::tags::{AudioFormat, TagsError, format_for_path};

/// Why a save leaves a tag item as it is: a scrub does not remove it,
/// and no edit may replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LeftAlone {
    /// An MP4 item holding a value that is not text, or an empty one.
    NotText,
    /// Two MP4 atoms of one code.
    HeldTwice,
    /// An ID3 frame over 1 MB, more than a tag document copies.
    OverSizeLimit,
}

/// What any tag write can keep of a file's tag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagsKept {
    /// Why no tag write can keep the tag whole, in plain words. A save
    /// would refuse the file; the preview says so first.
    pub refusal: Option<String>,
    /// Items every write leaves as they are, by their key without the
    /// format prefix (`cnID`, `----:iTunSMPB`, `GEOB`), one per frame.
    pub left_alone: Vec<(String, LeftAlone)>,
}

impl TagsKept {
    pub(super) fn refused(reason: String) -> Self {
        Self {
            refusal: Some(reason),
            left_alone: Vec::new(),
        }
    }
}

/// What a tag write can keep of this file's tag. Read-only containers
/// and untagged files report nothing.
pub fn tags_kept(path: &Path) -> Result<TagsKept, TagsError> {
    let format = format_for_path(path)?;
    let kept: KeptFn = match format {
        AudioFormat::Mp3 => kept_as::<Id3>,
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => kept_as::<Vorbis>,
        AudioFormat::M4a => kept_as::<Mp4>,
        AudioFormat::Aac | AudioFormat::Wav => return Ok(TagsKept::default()),
    };
    let bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    kept(format, &bytes).map_err(|refusal| TagsError::TagRead {
        path: path.display().to_string(),
        reason: refusal.to_string(),
    })
}

type KeptFn = fn(AudioFormat, &[u8]) -> Result<TagsKept, Refusal>;

fn kept_as<F: TagFormat>(format: AudioFormat, bytes: &[u8]) -> Result<TagsKept, Refusal> {
    match F::parse(format, bytes)? {
        Some(truth) => F::kept(truth),
        None => Ok(TagsKept::default()),
    }
}
