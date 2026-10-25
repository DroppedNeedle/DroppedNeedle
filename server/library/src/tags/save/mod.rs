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
//! Each tag format (ID3, Vorbis comments, MP4) implements [`TagFormat`]
//! with its own edit type: the container is matched to its format once,
//! and an edit resolved for one format cannot reach another's writer.
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
//! read and saved whole as ID3v2.4 (see [`id3_upgrade`]).
//!
//! `WAV` and `AAC` are read-only (the ID3-chunk fork and the never-persist
//! APE tag, per the measured format matrix), and WMA is unrecognized like everywhere.

mod document;
mod edit;
mod id3;
mod id3_bytes;
mod id3_edit;
mod id3_upgrade;
mod kept;
mod mp4;
mod mp4_edit;
mod v2_captured;
mod vorbis;

pub use document::{FieldDocument, read_document, read_fields};
pub use edit::accepts;
pub use id3_bytes::{Id3ByteReport, inspect_id3_bytes, mixed_v23_detail};
pub use id3_upgrade::{id3_lands_as_v23, id3_upgrade_notes};
pub use kept::{LeftAlone, TagsKept, tags_kept};
pub use v2_captured::{CapturedAtom, CapturedTags, CapturedValue, document_from_captured};

pub(crate) use id3::mixed_v23_date;

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lofty::file::TaggedFile;
use thiserror::Error;

use super::fields::TagField;
use super::{AudioFormat, TagsError, format_for_path};
use document::read_pictures;
use edit::{check_edit, resolve_edits};
use id3::Id3;
use mp4::Mp4;
use vorbis::Vorbis;

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

type SaveFn =
    fn(&Path, AudioFormat, &[TagEdit], Option<&[PictureSpec]>) -> Result<SavedInPlace, TagsError>;

/// The save for this container's tag format, or why it is read-only.
fn writer(format: AudioFormat) -> Result<SaveFn, Refusal> {
    let reason = match format {
        AudioFormat::Mp3 => return Ok(save_as::<Id3>),
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => return Ok(save_as::<Vorbis>),
        AudioFormat::M4a => return Ok(save_as::<Mp4>),
        AudioFormat::Aac => "APE tags on ADTS never persist; read-only",
        AudioFormat::Wav => {
            "WAV writes fork a second ID3 chunk and RIFF INFO is unwritten; read-only"
        }
    };
    Err(Refusal::ReadOnlyFormat {
        format: format.as_str().to_owned(),
        reason: reason.to_owned(),
    })
}

/// True when the save wrapper can write this container at all.
#[must_use]
pub fn writable(format: AudioFormat) -> bool {
    writer(format).is_ok()
}

/// Why the save wrapper cannot write this container, if it cannot.
pub fn check_writable(format: AudioFormat) -> Result<(), Refusal> {
    writer(format).map(|_| ())
}

/// Apply `edits` to `path` through the full wrapper pipeline, on a copy
/// beside it that replaces it only once verified.
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
    let save = writer(format).map_err(|refusal| refused(path, refusal))?;
    if edits.is_empty() && pictures.is_none() {
        return Ok(SaveReport {
            format,
            id3_version: None,
            edits_applied: 0,
        });
    }
    let temp = sibling_copy(path)?;
    let saved = save(&temp, format, edits, pictures).and_then(|saved| {
        fs::rename(&temp, path).map_err(|source| TagsError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if let Some(dir) = path.parent()
            && let Ok(handle) = File::open(dir)
        {
            let _ = handle.sync_all();
        }
        Ok(saved.report)
    });
    if saved.is_err() {
        let _ = fs::remove_file(&temp);
    }
    saved
}

/// What a save in place left in the file: the report, and the SHA-256
/// and size of the bytes saved, from the read that verified them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedInPlace {
    pub report: SaveReport,
    pub sha256: String,
    pub size: u64,
}

/// Apply `edits` (and `pictures`, as [`save_tags_with_pictures`] takes
/// them) to `path` itself, through the full wrapper pipeline: for a file
/// that is already a private copy, such as the publisher's journal temp.
/// A refusal may leave `path` part written. Its extension names the
/// container.
pub fn save_in_place(
    path: &Path,
    edits: &[TagEdit],
    pictures: Option<&[PictureSpec]>,
) -> Result<SavedInPlace, TagsError> {
    let format = format_for_path(path)?;
    let save = writer(format).map_err(|refusal| refused(path, refusal))?;
    save(path, format, edits, pictures)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A hidden copy of `original` in its own folder, with its extension, for
/// [`save_tags_with_pictures`] to save in and rename over it.
fn sibling_copy(original: &Path) -> Result<PathBuf, TagsError> {
    let dir = original.parent().filter(|dir| !dir.as_os_str().is_empty());
    let dir = dir.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let extension = original
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("tmp");
    let io = |path: &Path, source| TagsError::Io {
        path: path.display().to_string(),
        source,
    };
    for _ in 0..100 {
        let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(
            ".dn-tagtmp-{}-{count}.{extension}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(_) => {
                return match fs::copy(original, &temp) {
                    Ok(_) => Ok(temp),
                    Err(source) => {
                        let _ = fs::remove_file(&temp);
                        Err(io(&temp, source))
                    }
                };
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io(&temp, source)),
        }
    }
    Err(io(
        original,
        std::io::Error::new(ErrorKind::AlreadyExists, "could not pick a temp name"),
    ))
}

/// The whole file, with the SHA-256 of what was read.
fn read_hashed(path: &Path) -> Result<(Vec<u8>, String), TagsError> {
    use sha2::{Digest as _, Sha256};
    let bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    Ok((bytes, sha256))
}

// ---------------------------------------------------------------------------
// One tag format, and the save every format goes through.
// ---------------------------------------------------------------------------

/// One tag format the writer handles: its own byte parser (the check on
/// lofty), its own edit type, and each step of a save.
trait TagFormat {
    /// The tag as the file's bytes hold it.
    type Truth;
    /// One edit at its native home in this format's tag.
    type Edit;

    /// Pictures compare by their bytes alone: the format keeps no
    /// picture type, MIME type or description.
    const PICTURE_BYTES_ONLY: bool = false;

    /// The file's tag, or `None` when it has none.
    fn parse(format: AudioFormat, bytes: &[u8]) -> Result<Option<Self::Truth>, Refusal>;

    /// The writable fields and native items the tag holds.
    fn list(truth: Self::Truth, document: &mut FieldDocument) -> Result<(), Refusal>;

    /// The tag a save starts from, or why this file cannot be edited.
    fn prepare(truth: Self::Truth) -> Result<Self::Truth, Refusal> {
        Ok(truth)
    }

    /// Whether a save can keep this tag whole, in words, and the items
    /// it leaves as they are.
    fn kept(truth: Self::Truth) -> Result<TagsKept, Refusal>;

    /// Field edits at their homes in this tag.
    fn resolve_fields(
        truth: Option<&Self::Truth>,
        edits: &[TagEdit],
    ) -> Result<Vec<Self::Edit>, Refusal>;

    /// The edit a [`TagEdit::native`] key names, given the edits resolved
    /// before it.
    fn resolve_native(
        truth: Option<&Self::Truth>,
        key: &str,
        values: &[String],
        resolved: &[Self::Edit],
    ) -> Result<Self::Edit, Refusal>;

    /// True when both edits write one home, so the later one wins.
    fn same_target(a: &Self::Edit, b: &Self::Edit) -> bool;

    /// Write the edits, and the pictures when given, to `temp`.
    fn apply(
        truth: Option<&Self::Truth>,
        tagged: &TaggedFile,
        edits: &[Self::Edit],
        pictures: Option<&[PictureSpec]>,
        temp: &Path,
    ) -> Result<(), Refusal>;

    /// Refuse any change outside the edits and any edit that did not
    /// land. Picture bytes are checked on their own.
    fn verify(
        before: Option<&Self::Truth>,
        after: Option<&Self::Truth>,
        edits: &[Self::Edit],
        pictures_edited: bool,
    ) -> Result<(), Refusal>;

    /// The tag version a save keeps (`"2.4"`), for formats that have one.
    fn version(_truth: &Self::Truth) -> Option<String> {
        None
    }
}

fn save_as<F: TagFormat>(
    path: &Path,
    format: AudioFormat,
    edits: &[TagEdit],
    pictures: Option<&[PictureSpec]>,
) -> Result<SavedInPlace, TagsError> {
    // The file's bytes are held only while they are parsed, never across
    // the save.
    let original_bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let tagged = lofty::read_from_path(path).map_err(|error| TagsError::TagRead {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let mut snapshot = Snapshot::<F>::capture(format, &original_bytes)
        .map_err(|refusal| refused(path, refusal))?;
    drop(original_bytes);

    snapshot.truth = snapshot
        .truth
        .take()
        .map(F::prepare)
        .transpose()
        .map_err(|refusal| refused(path, refusal))?;
    for edit in edits {
        check_edit(edit).map_err(|refusal| refused(path, refusal))?;
    }
    let native = resolve_edits::<F>(snapshot.truth.as_ref(), edits)
        .map_err(|refusal| refused(path, refusal))?;

    F::apply(snapshot.truth.as_ref(), &tagged, &native, pictures, path)
        .map_err(|refusal| refused(path, refusal))?;
    drop(tagged);
    // The verify read: its hash is the saved file's.
    let (after, sha256, size) = {
        let (saved_bytes, sha256) = read_hashed(path)?;
        let after = Snapshot::<F>::capture(format, &saved_bytes)
            .map_err(|refusal| refused(path, refusal))?;
        (after, sha256, saved_bytes.len() as u64)
    };
    // The written file must still read as a whole.
    lofty::read_from_path(path).map_err(|error| TagsError::TagRead {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    snapshot
        .verify_against(&after, &native, pictures)
        .map_err(|refusal| refused(path, refusal))?;
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| TagsError::Io {
            path: path.display().to_string(),
            source,
        })?;
    Ok(SavedInPlace {
        report: SaveReport {
            format,
            id3_version: snapshot.truth.as_ref().and_then(F::version),
            edits_applied: edits.len(),
        },
        sha256,
        size,
    })
}

/// A file's tag and pictures, before or after a save.
struct Snapshot<F: TagFormat> {
    truth: Option<F::Truth>,
    pictures: Vec<PictureSpec>,
}

impl<F: TagFormat> Snapshot<F> {
    fn capture(format: AudioFormat, file_bytes: &[u8]) -> Result<Self, Refusal> {
        let truth = F::parse(format, file_bytes)?;
        Ok(Self {
            truth,
            pictures: read_pictures(file_bytes, format)?,
        })
    }

    fn verify_against(
        &self,
        after: &Self,
        edits: &[F::Edit],
        pictures: Option<&[PictureSpec]>,
    ) -> Result<(), Refusal> {
        F::verify(
            self.truth.as_ref(),
            after.truth.as_ref(),
            edits,
            pictures.is_some(),
        )?;
        // MP4 keeps only the bytes; ID3 and FLAC/Ogg keep the type, MIME
        // type and description too, and picture-block facts when given.
        let same = |want: &PictureSpec, got: &PictureSpec, facts: bool| {
            want.data == got.data
                && (F::PICTURE_BYTES_ONLY
                    || (want.kind == got.kind
                        && want.mime == got.mime
                        && want.description == got.description
                        && (!facts || want.info.is_none() || want.info == got.info)))
        };
        let (want, facts): (&[PictureSpec], bool) = match pictures {
            Some(expected) => (expected, true),
            None => (self.pictures.as_slice(), false),
        };
        let hold = want.len() == after.pictures.len()
            && want
                .iter()
                .zip(&after.pictures)
                .all(|(want, got)| same(want, got, facts));
        if !hold {
            return mismatch("pictures changed".to_owned());
        }
        Ok(())
    }
}

fn mismatch(detail: String) -> Result<(), Refusal> {
    Err(Refusal::VerifyMismatch { detail })
}

/// lofty's picture for one spec.
fn lofty_picture(spec: &PictureSpec) -> lofty::picture::Picture {
    lofty::picture::Picture::unchecked(spec.data.clone())
        .pic_type(lofty::picture::PictureType::from_u8(spec.kind))
        .mime_type(lofty::picture::MimeType::from_str(&spec.mime))
        .description(spec.description.clone())
        .build()
}

fn save_failed<E: std::error::Error>(error: E) -> Refusal {
    Refusal::SaveFailed {
        reason: error_chain(&error),
    }
}
