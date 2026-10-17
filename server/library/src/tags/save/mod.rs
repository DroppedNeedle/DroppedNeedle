//! The save wrapper: every tag mutation passes through here.
//!
//! Rule zero: never round-trip music through the
//! generic `Tag` API. The generic round-trip is lossy in ways that matter
//! here: known `TXXX` descriptions are rewritten to map-canonical spelling
//! (`Artists` becomes `ARTISTS`), `Work` becomes an invalid `WORK` frame
//! that aborts the whole save (lofty-rs#732), several text frames keep
//! only their first value, and unknown Vorbis fields vanish entirely. So
//! this wrapper parses the tag bytes itself, rebuilds the losable frames
//! and fields from that truth on top of lofty's native tag, saves to a
//! temp copy with the ID3 version pinned, and verifies the re-read bytes
//! against the snapshot. Anything it cannot preserve it refuses loudly,
//! leaving the original untouched.
//!
//! The writable fields are Picard's tag set ([`TagField`]): titles and
//! credits, track and disc numbers with totals, dates, release facts,
//! work and movement, comment, copyright, encoder, and the MusicBrainz
//! and AcoustID ids, each under Picard's native name for the format.
//! An edit with no values removes the field. Fields sharing an ID3 frame
//! own only their part of it: arranger and producer their `TIPL` (v2.3
//! `IPLS`) pairs, the comment the `COMM` frame with no description; the
//! other pairs and comments stay as they were, in their places.
//!
//! Preserved across one unrelated title edit: unknown ID3 frames,
//! multi-valued tags, `TXXX` description case, unknown Vorbis fields,
//! `TOTALTRACKS`/`TOTALDISCS` spelling, the vendor string, MP4 freeforms,
//! the order of MP4 items and of their values, each MP4 item's data
//! type, pictures, `CUSTOM_KEEP` everywhere,
//! and byte-identical audio. Refused: present empty
//! Vorbis values, values a field cannot hold (see [`Refusal`]), a
//! performer or arranger in MP4 (which has no home for either), a NUL
//! in a credit or comment, unparseable tag bytes, and any post-save
//! delta outside the edit.
//!
//! An MP3 keeps its ID3 version, except an ID3v2.3 tag that also holds
//! ID3v2.4 frames: lofty's v2.3 writer would drop those, so such a tag is
//! read and saved whole as ID3v2.4 (see [`upgrade_to_v24`]).
//!
//! `WAV` and `AAC` are read-only (the ID3-chunk fork and the never-persist
//! APE tag, per the measured format matrix), and WMA is unrecognized like everywhere.

mod document;
mod edit;
mod id3;
mod id3_bytes;
mod id3_upgrade;
mod mp4;
mod v2_captured;
mod vorbis;

pub use document::{FieldDocument, read_document, read_fields};
pub use edit::accepts;
pub use id3_bytes::{Id3ByteReport, inspect_id3_bytes, mixed_v23_detail};
pub use id3_upgrade::{id3_lands_as_v23, id3_unkept_frames, id3_upgrade_notes};
pub use v2_captured::{CapturedAtom, CapturedTags, CapturedValue, document_from_captured};

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::TextEncoding;
use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt as _;
use lofty::id3::v2::{
    ExtendedTextFrame, Frame, FrameId, Id3v2Tag, Id3v2Version, TextInformationFrame,
    UniqueFileIdentifierFrame, UnsynchronizedTextFrame,
};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::{Accessor as _, TagExt as _, TagType};
use thiserror::Error;

use super::fields::{MP4_MEAN, TagField};
use super::{AudioFormat, TagsError, format_for_path};
use document::read_pictures;
use edit::{NativeEdit, check_edit, resolve_edits};
use id3::{repair_id3_text, verify_id3};
use id3_bytes::{DeepId3, parse_id3_deep};
use id3_upgrade::{is_mixed_v23, upgrade_to_v24};
use mp4::{
    Mp4AtomTruth, keep_mp4_order, parse_mp4_atoms, repair_mp4_text, replace_mp4, verify_mp4,
};
use vorbis::{VorbisTruth, is_picture_field, parse_vorbis_truth, verify_vorbis};

/// One requested mutation: a field with its replacement values. No
/// values removes the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagEdit {
    pub field: TagField,
    pub values: Vec<String>,
    /// Values read back from a file (undo, baseline restore): written as
    /// they were, without the checks a new value gets. The post-save
    /// verify still refuses anything that does not land exactly.
    pub verbatim: bool,
    /// One Vorbis spelling to write (a file holding `TOTALTRACKS` and
    /// `TRACKTOTAL` with different values gets each back); `None` writes
    /// every spelling the file uses.
    pub spelling: Option<&'static str>,
    /// A native item outside the writable fields (`vorbis:COMPOSER`,
    /// `id3:TCOM`, `id3:TXXX:<description>`, `mp4:<atom>`,
    /// `mp4:----:<name>`), as [`FieldDocument::native`] lists them. Its
    /// values go back verbatim; none removes it. `field` is unused then.
    pub native: Option<String>,
}

impl TagEdit {
    #[must_use]
    pub fn new(field: TagField, values: Vec<String>) -> Self {
        Self {
            field,
            values,
            verbatim: false,
            spelling: None,
            native: None,
        }
    }

    /// A native item outside the writable fields, written back verbatim
    /// (or removed, with no values).
    #[must_use]
    pub fn native(key: impl Into<String>, values: Vec<String>) -> Self {
        Self {
            field: TagField::Title,
            values,
            verbatim: true,
            spelling: None,
            native: Some(key.into()),
        }
    }

    /// Values a file held, to be written back unchanged.
    #[must_use]
    pub fn verbatim(field: TagField, values: Vec<String>) -> Self {
        Self {
            field,
            values,
            verbatim: true,
            spelling: None,
            native: None,
        }
    }

    /// One Vorbis spelling's values, written back unchanged. Other
    /// formats have one home per field and ignore this edit.
    #[must_use]
    pub fn verbatim_spelling(field: TagField, spelling: &'static str, values: Vec<String>) -> Self {
        Self {
            field,
            values,
            verbatim: true,
            spelling: Some(spelling),
            native: None,
        }
    }

    #[must_use]
    pub fn set_title(title: impl Into<String>) -> Self {
        Self::new(TagField::Title, vec![title.into()])
    }
}

/// Why a save was refused. Every variant leaves the original untouched.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    #[error("refusing to write {format}: {reason}")]
    ReadOnlyFormat { format: String, reason: String },
    #[error("unencodable {field:?}: {reason}")]
    UnencodableItem { field: TagField, reason: String },
    #[error("empty Vorbis value present under '{key}'; lofty drops empties on save")]
    EmptyVorbisValue { key: String },
    #[error("post-save verify found an out-of-edit delta: {detail}")]
    VerifyMismatch { detail: String },
    #[error("no byte inventory possible: {detail}")]
    NoByteInventory { detail: String },
    #[error("native save failed (original untouched): {reason}")]
    SaveFailed { reason: String },
}

/// What a successful save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    pub format: AudioFormat,
    /// ID3 version the tag was pinned to, when applicable (`"2.3"`, `"2.4"`).
    pub id3_version: Option<String>,
    pub edits_applied: usize,
}

fn refused(path: &Path, refusal: Refusal) -> TagsError {
    TagsError::SaveRefused {
        path: path.display().to_string(),
        refusal,
    }
}

/// lofty's save errors hide the cause behind `..` Debug; unfold it.
fn error_chain(error: &impl std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = std::error::Error::source(cause);
    }
    parts.join(": caused by: ")
}

fn read_only(format: AudioFormat) -> Option<Refusal> {
    let reason = match format {
        AudioFormat::Aac => "APE tags on ADTS never persist; read-only",
        AudioFormat::Wav => {
            "WAV writes fork a second ID3 chunk and RIFF INFO is unwritten; read-only"
        }
        AudioFormat::Flac
        | AudioFormat::Mp3
        | AudioFormat::Ogg
        | AudioFormat::Opus
        | AudioFormat::M4a => return None,
    };
    Some(Refusal::ReadOnlyFormat {
        format: format.as_str().to_owned(),
        reason: reason.to_owned(),
    })
}

/// True when the save wrapper can write this container at all.
#[must_use]
pub fn writable(format: AudioFormat) -> bool {
    read_only(format).is_none()
}

/// Apply `edits` to `path` through the full wrapper pipeline.
pub fn save_tags(path: &Path, edits: &[TagEdit]) -> Result<SaveReport, TagsError> {
    save_tags_with_pictures(path, edits, None)
}

/// One embedded picture, as written and as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureSpec {
    /// ID3 / FLAC picture type (3 is the front cover). MP4 stores none:
    /// its first `covr` reads as the front cover, the rest as 0.
    pub kind: u8,
    pub mime: String,
    pub description: String,
    pub data: Vec<u8>,
    /// FLAC and Ogg picture-block facts (width, height, colour depth,
    /// colours used) as the file held them, so a restore writes them back
    /// unchanged. `None` lets the writer work them out from the image.
    pub info: Option<[u32; 4]>,
}

/// [`save_tags`], also replacing the file's embedded pictures with
/// `pictures` (in order) when given; `Some(&[])` removes them all. The
/// verify then expects exactly those picture bytes.
pub fn save_tags_with_pictures(
    path: &Path,
    edits: &[TagEdit],
    pictures: Option<&[PictureSpec]>,
) -> Result<SaveReport, TagsError> {
    let format = format_for_path(path)?;
    if let Some(refusal) = read_only(format) {
        return Err(refused(path, refusal));
    }
    if edits.is_empty() && pictures.is_none() {
        return Ok(SaveReport {
            format,
            id3_version: None,
            edits_applied: 0,
        });
    }

    let original_bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let tagged = lofty::read_from_path(path).map_err(|error| TagsError::TagRead {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let mut snapshot =
        Snapshot::capture(format, &original_bytes).map_err(|refusal| refused(path, refusal))?;
    snapshot
        .upgrade_mixed_id3()
        .map_err(|refusal| refused(path, refusal))?;
    snapshot
        .check_file_state()
        .map_err(|refusal| refused(path, refusal))?;
    for edit in edits {
        check_edit(edit).map_err(|refusal| refused(path, refusal))?;
    }
    let native = resolve_edits(&snapshot, edits).map_err(|refusal| refused(path, refusal))?;

    let temp = TempCopy::create(path)?;
    apply_edits(format, &snapshot, &tagged, &native, pictures, temp.path())
        .map_err(|refusal| refused(temp.path(), refusal))?;
    let temp_bytes = fs::read(temp.path()).map_err(|source| TagsError::Io {
        path: temp.path().display().to_string(),
        source,
    })?;
    // The written copy must still read as a whole.
    lofty::read_from_path(temp.path()).map_err(|error| TagsError::TagRead {
        path: temp.path().display().to_string(),
        reason: error.to_string(),
    })?;
    let after =
        Snapshot::capture(format, &temp_bytes).map_err(|refusal| refused(temp.path(), refusal))?;
    snapshot
        .verify_against(&after, &native, pictures)
        .map_err(|refusal| refused(path, refusal))?;
    temp.commit()?;
    Ok(SaveReport {
        format,
        id3_version: snapshot.id3_version_label(),
        edits_applied: edits.len(),
    })
}

// ---------------------------------------------------------------------------
// Snapshots and verify.
// ---------------------------------------------------------------------------

enum Snapshot {
    Id3 {
        had_tag: bool,
        tag: Option<DeepId3>,
        pictures: Vec<PictureSpec>,
    },
    Vorbis {
        had_tag: bool,
        truth: Option<VorbisTruth>,
        pictures: Vec<PictureSpec>,
    },
    Mp4 {
        atoms: Option<Vec<Mp4AtomTruth>>,
        pictures: Vec<PictureSpec>,
    },
}

impl Snapshot {
    fn capture(format: AudioFormat, file_bytes: &[u8]) -> Result<Self, Refusal> {
        match format {
            AudioFormat::Mp3 => {
                let tag = parse_id3_deep(file_bytes)?;
                Ok(Snapshot::Id3 {
                    had_tag: tag.is_some(),
                    tag,
                    pictures: read_pictures(file_bytes, format)?,
                })
            }
            AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => {
                let truth = parse_vorbis_truth(format, file_bytes)?;
                Ok(Snapshot::Vorbis {
                    had_tag: truth.is_some(),
                    truth,
                    pictures: read_pictures(file_bytes, format)?,
                })
            }
            AudioFormat::M4a => Ok(Snapshot::Mp4 {
                atoms: parse_mp4_atoms(file_bytes)?,
                pictures: read_pictures(file_bytes, format)?,
            }),
            AudioFormat::Aac | AudioFormat::Wav => Err(Refusal::ReadOnlyFormat {
                format: format.as_str().to_owned(),
                reason: "read-only container".to_owned(),
            }),
        }
    }

    /// A mixed v2.3 tag's truth becomes the ID3v2.4 tag it is saved as:
    /// edits resolve to v2.4 homes, and the verify expects version 2.4.
    fn upgrade_mixed_id3(&mut self) -> Result<(), Refusal> {
        if let Snapshot::Id3 { tag: Some(tag), .. } = self
            && is_mixed_v23(tag)
        {
            *tag = upgrade_to_v24(tag)?.tag;
        }
        Ok(())
    }

    fn check_file_state(&self) -> Result<(), Refusal> {
        match self {
            Snapshot::Vorbis { truth, .. } => {
                let Some(truth) = truth else {
                    return Ok(());
                };
                if let Some((key, _)) = truth.pairs.iter().find(|(_, value)| value.is_empty()) {
                    return Err(Refusal::EmptyVorbisValue { key: key.clone() });
                }
                // Keys with mixed spellings (`title` plus `TITLE`)
                // cannot be edited without merging them; repeated keys
                // with one spelling are ordinary multi-values.
                let mut spellings: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for (key, _) in &truth.pairs {
                    let entry = spellings.entry(key.to_ascii_uppercase()).or_default();
                    if !entry.contains(key) {
                        entry.push(key.clone());
                    }
                }
                if let Some((upper, variants)) =
                    spellings.iter().find(|(_, variants)| variants.len() > 1)
                {
                    return Err(Refusal::NoByteInventory {
                        detail: format!("comment key '{upper}' has mixed spellings {variants:?}"),
                    });
                }
                Ok(())
            }
            Snapshot::Id3 { .. } | Snapshot::Mp4 { .. } => Ok(()),
        }
    }

    fn verify_against(
        &self,
        after: &Snapshot,
        edits: &[NativeEdit],
        pictures: Option<&[PictureSpec]>,
    ) -> Result<(), Refusal> {
        let expected = pictures;
        let mp4 = matches!(self, Snapshot::Mp4 { .. });
        // MP4 keeps only the bytes; ID3 and FLAC/Ogg keep the type, MIME
        // type and description too, and picture-block facts when given.
        let same = |want: &PictureSpec, got: &PictureSpec, facts: bool| {
            want.data == got.data
                && (mp4
                    || (want.kind == got.kind
                        && want.mime == got.mime
                        && want.description == got.description
                        && (!facts || want.info.is_none() || want.info == got.info)))
        };
        let pictures_hold = |before: &Vec<PictureSpec>, after: &Vec<PictureSpec>| {
            let (want, facts): (&[PictureSpec], bool) = match expected {
                Some(expected) => (expected, true),
                None => (before.as_slice(), false),
            };
            want.len() == after.len()
                && want
                    .iter()
                    .zip(after)
                    .all(|(want, got)| same(want, got, facts))
        };
        match (self, after) {
            (
                Snapshot::Id3 {
                    had_tag,
                    tag: before,
                    pictures,
                },
                Snapshot::Id3 {
                    tag: after,
                    pictures: after_pictures,
                    ..
                },
            ) => {
                verify_id3(
                    *had_tag,
                    before.as_ref(),
                    after.as_ref(),
                    edits,
                    expected.is_some(),
                )?;
                if !pictures_hold(pictures, after_pictures) {
                    return mismatch("pictures changed".to_owned());
                }
                Ok(())
            }
            (
                Snapshot::Vorbis {
                    had_tag,
                    truth: before,
                    pictures,
                },
                Snapshot::Vorbis {
                    truth: after,
                    pictures: after_pictures,
                    ..
                },
            ) => {
                verify_vorbis(*had_tag, before.as_ref(), after.as_ref(), edits)?;
                if !pictures_hold(pictures, after_pictures) {
                    return mismatch("pictures changed".to_owned());
                }
                Ok(())
            }
            (
                Snapshot::Mp4 {
                    atoms: before,
                    pictures,
                    ..
                },
                Snapshot::Mp4 {
                    atoms: after,
                    pictures: after_pictures,
                    ..
                },
            ) => {
                verify_mp4(
                    before.as_deref(),
                    after.as_deref(),
                    edits,
                    expected.is_some(),
                )?;
                if !pictures_hold(pictures, after_pictures) {
                    return mismatch("pictures changed".to_owned());
                }
                Ok(())
            }
            _ => mismatch("format changed under the save".to_owned()),
        }
    }

    fn id3_version_label(&self) -> Option<String> {
        match self {
            Snapshot::Id3 { tag, .. } => tag.as_ref().map(|tag| format!("2.{}", tag.major)),
            _ => None,
        }
    }
}

fn mismatch(detail: String) -> Result<(), Refusal> {
    Err(Refusal::VerifyMismatch { detail })
}

// ---------------------------------------------------------------------------
// Native application on the temp copy, repaired from byte truth.
// ---------------------------------------------------------------------------

fn apply_edits(
    format: AudioFormat,
    snapshot: &Snapshot,
    tagged: &lofty::file::TaggedFile,
    edits: &[NativeEdit],
    pictures: Option<&[PictureSpec]>,
    temp: &Path,
) -> Result<(), Refusal> {
    let lofty_picture = |spec: &PictureSpec| {
        lofty::picture::Picture::unchecked(spec.data.clone())
            .pic_type(lofty::picture::PictureType::from_u8(spec.kind))
            .mime_type(lofty::picture::MimeType::from_str(&spec.mime))
            .description(spec.description.clone())
            .build()
    };
    match format {
        AudioFormat::Mp3 => {
            let truth = match snapshot {
                Snapshot::Id3 { tag, .. } => tag.as_ref(),
                _ => None,
            };
            let mut native = tagged
                .tag(TagType::Id3v2)
                .cloned()
                .map(Id3v2Tag::from)
                .unwrap_or_default();
            repair_id3_text(&mut native, truth);
            for edit in edits {
                match edit {
                    NativeEdit::Id3Text { id, values } => {
                        let frame_id = FrameId::Valid(std::borrow::Cow::Owned(id.clone()));
                        let _ = native.remove(&frame_id);
                        if !values.is_empty() {
                            native.insert(Frame::Text(TextInformationFrame::new(
                                frame_id,
                                TextEncoding::UTF8,
                                values.join("\0"),
                            )));
                        }
                    }
                    NativeEdit::Id3User { desc, values } => {
                        native.retain(|frame| {
                            !matches!(frame, Frame::UserText(text)
                                if text.description.eq_ignore_ascii_case(desc))
                        });
                        if !values.is_empty() {
                            native.insert(Frame::UserText(ExtendedTextFrame::new(
                                TextEncoding::UTF8,
                                desc.clone(),
                                values.join("\0"),
                            )));
                        }
                    }
                    NativeEdit::Id3Ufid { owner, value } => {
                        native.retain(|frame| {
                            !matches!(frame, Frame::UniqueFileIdentifier(ufid)
                                if ufid.owner == owner.as_str())
                        });
                        if let Some(value) = value {
                            native.insert(Frame::UniqueFileIdentifier(
                                UniqueFileIdentifierFrame::new(
                                    owner.clone(),
                                    value.as_bytes().to_vec(),
                                ),
                            ));
                        }
                    }
                    NativeEdit::Id3Raw { id, payloads } => {
                        let frame_id = FrameId::Valid(std::borrow::Cow::Owned(id.clone()));
                        let _ = native.remove(&frame_id).count();
                        for payload in payloads {
                            native.insert(Frame::Binary(lofty::id3::v2::BinaryFrame::new(
                                frame_id.clone(),
                                payload.clone(),
                            )));
                        }
                    }
                    NativeEdit::Id3Lyrics { text } => {
                        // By id: the frame may be one put back raw.
                        let _ = native
                            .remove(&FrameId::Valid(std::borrow::Cow::Borrowed("USLT")))
                            .count();
                        if let Some(text) = text {
                            native.insert(Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
                                TextEncoding::UTF8,
                                *b"XXX",
                                "",
                                text.clone(),
                            )));
                        }
                    }
                    _ => {}
                }
            }
            if let Some(pictures) = pictures {
                let _ = native
                    .remove(&FrameId::Valid(std::borrow::Cow::Borrowed("APIC")))
                    .count();
                for spec in pictures {
                    native.insert(Frame::Picture(lofty::id3::v2::AttachedPictureFrame::new(
                        TextEncoding::UTF8,
                        lofty_picture(spec),
                    )));
                }
            }
            // The truth decides: an upgraded mixed tag is saved as 2.4.
            let pin_v23 = match truth {
                Some(tag) => tag.major == 3,
                None => matches!(native.original_version(), Id3v2Version::V3),
            };
            native
                .save_to_path(temp, WriteOptions::new().use_id3v23(pin_v23))
                .map_err(save_failed)
        }
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => {
            let truth = match snapshot {
                Snapshot::Vorbis { truth, .. } => truth.as_ref(),
                _ => None,
            };
            let mut native = tagged
                .tag(TagType::VorbisComments)
                .cloned()
                .map(VorbisComments::from)
                .unwrap_or_default();
            // Empty every pair (pictures stay in the pictures vec), then
            // push the raw truth in file order, minus the edited keys, so
            // unknown fields the generic round-trip dropped come back,
            // then the edits. Nothing is removed after a push: removal
            // reorders what is left.
            let keys: Vec<String> = native.items().map(|(key, _)| key.to_owned()).collect();
            for key in &keys {
                native.remove(key).for_each(drop);
            }
            let edited: Vec<(&String, &Vec<String>)> = edits
                .iter()
                .filter_map(|edit| match edit {
                    NativeEdit::Vorbis { key, values } => Some((key, values)),
                    _ => None,
                })
                .collect();
            if let Some(truth) = truth {
                for (key, value) in &truth.pairs {
                    let replaced = edited
                        .iter()
                        .any(|(edited_key, _)| edited_key.eq_ignore_ascii_case(key));
                    if !is_picture_field(key) && !replaced {
                        native.push(key.clone(), value.clone());
                    }
                }
                native.set_vendor(truth.vendor.clone());
            }
            for (key, values) in edited {
                for value in values {
                    native.push(key.clone(), value.clone());
                }
            }
            if let Some(pictures) = pictures {
                use lofty::ogg::OggPictureStorage as _;
                native.remove_pictures();
                for spec in pictures {
                    let info = spec.info.map(|[width, height, color_depth, num_colors]| {
                        lofty::picture::PictureInformation {
                            width,
                            height,
                            color_depth,
                            num_colors,
                        }
                    });
                    native
                        .insert_picture(lofty_picture(spec), info)
                        .map_err(save_failed)?;
                }
            }
            native
                .save_to_path(temp, WriteOptions::new())
                .map_err(save_failed)
        }
        AudioFormat::M4a => {
            let mut native = tagged
                .tag(TagType::Mp4Ilst)
                .cloned()
                .map(Ilst::from)
                .unwrap_or_default();
            if let Snapshot::Mp4 {
                atoms: Some(atoms), ..
            } = snapshot
            {
                repair_mp4_text(&mut native, atoms);
            }
            for edit in edits {
                match edit {
                    NativeEdit::Mp4Text { code, values } => {
                        replace_mp4(&mut native, AtomIdent::Fourcc(*code), values);
                    }
                    NativeEdit::Mp4Freeform { name, values } => {
                        let ident = AtomIdent::Freeform {
                            mean: std::borrow::Cow::Borrowed(MP4_MEAN),
                            name: std::borrow::Cow::Owned(name.clone()),
                        };
                        replace_mp4(&mut native, ident, values);
                    }
                    NativeEdit::Mp4Flag { code, value } => {
                        let ident = AtomIdent::Fourcc(*code);
                        let _ = native.remove(&ident).count();
                        if let Some(value) = value {
                            native.insert(Atom::new(ident, AtomData::Bool(*value)));
                        }
                    }
                    NativeEdit::Mp4Integer { code, value } => {
                        let ident = AtomIdent::Fourcc(*code);
                        let _ = native.remove(&ident).count();
                        if let Some(value) = value {
                            native.insert(Atom::new(ident, AtomData::SignedInteger(*value)));
                        }
                    }
                    NativeEdit::Mp4Pair {
                        code,
                        number,
                        total,
                    } => {
                        let disk = code == b"disk";
                        if disk {
                            native.remove_disk();
                            native.remove_disk_total();
                        } else {
                            native.remove_track();
                            native.remove_track_total();
                        }
                        if *number > 0 || *total > 0 {
                            match (disk, *total > 0) {
                                (true, true) => {
                                    native.set_disk(*number);
                                    native.set_disk_total(*total);
                                }
                                (true, false) => native.set_disk(*number),
                                (false, true) => {
                                    native.set_track(*number);
                                    native.set_track_total(*total);
                                }
                                (false, false) => native.set_track(*number),
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let Some(pictures) = pictures {
                native.remove_pictures();
                for spec in pictures {
                    native.insert_picture(lofty_picture(spec));
                }
            }
            if let Snapshot::Mp4 {
                atoms: Some(atoms), ..
            } = snapshot
            {
                keep_mp4_order(&mut native, atoms);
            }
            native
                .save_to_path(temp, WriteOptions::new())
                .map_err(save_failed)
        }
        AudioFormat::Aac | AudioFormat::Wav => Err(Refusal::ReadOnlyFormat {
            format: format.as_str().to_owned(),
            reason: "read-only container".to_owned(),
        }),
    }
}

fn save_failed<E: std::error::Error>(error: E) -> Refusal {
    Refusal::SaveFailed {
        reason: error_chain(&error),
    }
}

// ---------------------------------------------------------------------------
// Temp copy in the target directory: same filesystem, same extension.
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempCopy {
    temp: PathBuf,
    original: PathBuf,
}

impl TempCopy {
    fn create(original: &Path) -> Result<Self, TagsError> {
        let dir = original.parent().filter(|dir| !dir.as_os_str().is_empty());
        let dir = dir.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let extension = original
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("tmp");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|span| span.as_nanos())
            .unwrap_or(0);
        for _ in 0..100 {
            let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                ".dn-tagtmp-{}-{nanos}-{count}.{extension}",
                std::process::id()
            );
            let temp = dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(_) => {
                    fs::copy(original, &temp).map_err(|source| TagsError::Io {
                        path: temp.display().to_string(),
                        source,
                    })?;
                    return Ok(Self {
                        temp,
                        original: original.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(TagsError::Io {
                        path: temp.display().to_string(),
                        source,
                    });
                }
            }
        }
        Err(TagsError::Io {
            path: original.display().to_string(),
            source: std::io::Error::new(ErrorKind::AlreadyExists, "could not pick a temp name"),
        })
    }

    fn path(&self) -> &Path {
        &self.temp
    }

    fn commit(&self) -> Result<(), TagsError> {
        File::open(&self.temp)
            .and_then(|file| file.sync_all())
            .map_err(|source| TagsError::Io {
                path: self.temp.display().to_string(),
                source,
            })?;
        fs::rename(&self.temp, &self.original).map_err(|source| TagsError::Io {
            path: self.original.display().to_string(),
            source,
        })?;
        if let Some(dir) = self.original.parent()
            && let Ok(handle) = File::open(dir)
        {
            let _ = handle.sync_all();
        }
        Ok(())
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temp);
    }
}
