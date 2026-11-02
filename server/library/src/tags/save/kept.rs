//! What a save keeps of a file's tag: whether it can keep the tag whole
//! at all, and the items it leaves as they are because undo could not
//! put them back exactly.

use std::fs;
use std::path::Path;

use super::document::read_pictures;
use super::id3::Id3;
use super::mp4::Mp4;
use super::vorbis::Vorbis;
use super::{Refusal, TagFormat};
use crate::tags::{AudioFormat, TagsError, format_for_path};

/// Why a save leaves a tag item as it is: a scrub does not remove it,
/// and no edit may replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LeftAlone {
    /// An MP4 item holding a value that is not text.
    NotText,
    /// An MP4 item holding an empty value.
    Empty,
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
    /// Items every write leaves as they are, by their native key
    /// (`mp4:cnID`, `mp4:----:iTunSMPB`, `id3:GEOB`), one per frame.
    pub left_alone: Vec<(String, LeftAlone)>,
    /// What a write does to the file's ID3 version, when it changes it.
    pub upgrade_notes: Vec<String>,
}

/// What a planned write does to the file's embedded pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PicturesPlan {
    /// They stay as they are.
    Untouched,
    /// They are replaced as a set.
    Written,
}

impl TagsKept {
    pub(super) fn refused(reason: String) -> Self {
        Self {
            refusal: Some(reason),
            left_alone: Vec::new(),
            upgrade_notes: Vec::new(),
        }
    }
}

/// What a tag write that does `pictures` can keep of this file's tag.
/// Read-only containers report nothing, as untagged files do unless a
/// picture write would lose one of their pictures.
pub fn tags_kept(path: &Path, pictures: PicturesPlan) -> Result<TagsKept, TagsError> {
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
    kept(format, &bytes, pictures).map_err(|refusal| TagsError::TagRead {
        path: path.display().to_string(),
        reason: refusal.to_string(),
    })
}

type KeptFn = fn(AudioFormat, &[u8], PicturesPlan) -> Result<TagsKept, Refusal>;

fn kept_as<F: TagFormat>(
    format: AudioFormat,
    bytes: &[u8],
    pictures: PicturesPlan,
) -> Result<TagsKept, Refusal> {
    let kept = match F::parse(format, bytes)? {
        Some(truth) => F::kept(truth, pictures)?,
        None => TagsKept::default(),
    };
    if kept.refusal.is_none() && pictures == PicturesPlan::Written {
        let held = read_pictures(bytes, format)?;
        if let Some(reason) = F::pictures_refusal(&held) {
            return Ok(TagsKept::refused(reason));
        }
    }
    Ok(kept)
}
