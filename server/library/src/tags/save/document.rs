//! A file's writable fields and pictures as a [`FieldDocument`], read
//! from the native homes the writer uses.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use lofty::file::TaggedFileExt as _;
use lofty::tag::TagType;

use super::id3::Id3;
use super::mp4::Mp4;
use super::vorbis::Vorbis;
use super::{PictureSpec, Refusal, TagFormat, error_chain};
use crate::tags::fields::TagField;
use crate::tags::{AudioFormat, TagsError, format_for_path};

/// Every writable field a file carries, read from the native homes the
/// writer uses and kept exactly as written, so writing them back
/// verbatim restores the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldDocument {
    pub values: BTreeMap<TagField, Vec<String>>,
    /// Fields present in a shape that is not text (an MP4 atom holding a
    /// number, a non-UTF-8 identifier, a v2.3 date split oddly): they
    /// cannot be written back, so nothing may overwrite or remove them.
    pub opaque: std::collections::BTreeSet<TagField>,
    /// Each Vorbis spelling's own values, for fields the file spells more
    /// than one way (`TOTALTRACKS` and `TRACKTOTAL`).
    pub spellings: BTreeMap<(TagField, &'static str), Vec<String>>,
    /// Text items outside the writable fields (a mood, an encoder
    /// setting, a custom `TXXX`), keyed the way [`TagEdit::native`] takes
    /// them, so a scrub can remove them and a restore put them back.
    /// ID3 comment, URL, private and object frames are listed whole
    /// (`id3:COMM`, one base64 payload per frame), except the comment the
    /// comment field owns. Pictures and other non-text items are not
    /// listed: they are never scrubbed.
    pub native: BTreeMap<String, Vec<String>>,
    /// Embedded pictures in file order.
    pub pictures: Vec<PictureSpec>,
}

/// Read a file's [`FieldDocument`]. Read-only containers report nothing:
/// there is nothing to write back.
pub fn read_document(path: &Path) -> Result<FieldDocument, TagsError> {
    let format = format_for_path(path)?;
    let mut document = FieldDocument::default();
    let list: ListFn = match format {
        AudioFormat::Mp3 => list_as::<Id3>,
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => list_as::<Vorbis>,
        AudioFormat::M4a => list_as::<Mp4>,
        AudioFormat::Aac | AudioFormat::Wav => return Ok(document),
    };
    let bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let unreadable = |refusal: Refusal| TagsError::TagRead {
        path: path.display().to_string(),
        reason: refusal.to_string(),
    };
    list(format, &bytes, &mut document).map_err(unreadable)?;
    document.pictures = read_pictures(&bytes, format).map_err(unreadable)?;
    Ok(document)
}

type ListFn = fn(AudioFormat, &[u8], &mut FieldDocument) -> Result<(), Refusal>;

fn list_as<F: TagFormat>(
    format: AudioFormat,
    bytes: &[u8],
    document: &mut FieldDocument,
) -> Result<(), Refusal> {
    match F::parse(format, bytes)? {
        Some(truth) => F::list(truth, document),
        None => Ok(()),
    }
}

/// A file's embedded pictures in file order. A file lofty cannot read is
/// an error, never "no pictures": a plan built on that would remove
/// them.
pub(super) fn read_pictures(
    bytes: &[u8],
    format: AudioFormat,
) -> Result<Vec<PictureSpec>, Refusal> {
    use lofty::file::AudioFile as _;
    use lofty::ogg::OggPictureStorage as _;
    fn failed<E: std::error::Error>(error: E) -> Refusal {
        Refusal::SaveFailed {
            reason: format!("pictures unreadable: {}", error_chain(&error)),
        }
    }
    let spec = |picture: &lofty::picture::Picture, info: Option<[u32; 4]>| PictureSpec {
        kind: picture.pic_type().as_u8(),
        mime: picture
            .mime_type()
            .map(|mime| mime.as_str().to_owned())
            .unwrap_or_default(),
        description: picture.description().unwrap_or_default().to_owned(),
        data: picture.data().to_vec(),
        info,
    };
    let with_info = |list: &[(lofty::picture::Picture, lofty::picture::PictureInformation)]| {
        list.iter()
            .map(|(picture, info)| {
                spec(
                    picture,
                    Some([info.width, info.height, info.color_depth, info.num_colors]),
                )
            })
            .collect::<Vec<_>>()
    };
    let options = lofty::config::ParseOptions::new().read_properties(false);
    let mut reader = std::io::Cursor::new(bytes);
    match format {
        AudioFormat::Flac => {
            let file = lofty::flac::FlacFile::read_from(&mut reader, options).map_err(failed)?;
            // Pictures sit in their own blocks; any inside the comment
            // block come after them, as lofty writes them back.
            let mut out = with_info(file.pictures());
            if let Some(comments) = file.vorbis_comments() {
                out.extend(with_info(comments.pictures()));
            }
            Ok(out)
        }
        AudioFormat::Ogg => {
            let file = lofty::ogg::VorbisFile::read_from(&mut reader, options).map_err(failed)?;
            Ok(with_info(file.vorbis_comments().pictures()))
        }
        AudioFormat::Opus => {
            let file = lofty::ogg::OpusFile::read_from(&mut reader, options).map_err(failed)?;
            Ok(with_info(file.vorbis_comments().pictures()))
        }
        AudioFormat::Mp3 | AudioFormat::M4a => {
            let tag_type = if format == AudioFormat::Mp3 {
                TagType::Id3v2
            } else {
                TagType::Mp4Ilst
            };
            let tagged = lofty::probe::Probe::new(&mut reader)
                .options(options)
                .guess_file_type()
                .map_err(failed)?
                .read()
                .map_err(failed)?;
            let mut out: Vec<PictureSpec> = tagged
                .tag(tag_type)
                .map(|tag| {
                    tag.pictures()
                        .iter()
                        .map(|picture| spec(picture, None))
                        .collect()
                })
                .unwrap_or_default();
            // MP4 stores no picture type; the first `covr` is the front
            // cover by convention (iTunes, Picard).
            if format == AudioFormat::M4a {
                for (index, picture) in out.iter_mut().enumerate() {
                    picture.kind = if index == 0 { 3 } else { 0 };
                }
            }
            Ok(out)
        }
        AudioFormat::Aac | AudioFormat::Wav => Ok(Vec::new()),
    }
}

/// File each field read into the document.
pub(super) fn collect_reads(document: &mut FieldDocument, reads: Vec<(TagField, FieldRead)>) {
    for (field, read) in reads {
        match read {
            FieldRead::Absent => {}
            FieldRead::Values(values) => {
                document.values.insert(field, values);
            }
            FieldRead::Opaque => {
                document.opaque.insert(field);
            }
        }
    }
}

/// The writable fields a file carries as text (see [`read_document`]).
pub fn read_fields(path: &Path) -> Result<BTreeMap<TagField, Vec<String>>, TagsError> {
    Ok(read_document(path)?.values)
}

/// What a file holds for one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FieldRead {
    Absent,
    Values(Vec<String>),
    /// Present, but not as text this code can write back unchanged.
    Opaque,
}

pub(super) fn values_or_absent(values: Vec<String>) -> FieldRead {
    if values.is_empty() {
        FieldRead::Absent
    } else {
        FieldRead::Values(values)
    }
}
