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
use lofty::tag::items::Timestamp;
use lofty::tag::{Accessor as _, TagExt as _, TagType};
use thiserror::Error;

use super::fields::{
    FieldKind, INVOLVED_ROLES, Id3Target, MP4_MEAN, Mp4Target, Role, Slot, TagField,
};
use super::{AudioFormat, TagsError, format_for_path};

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
    if !writable(format) {
        return Ok(document);
    }
    let bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let unreadable = |refusal: Refusal| TagsError::TagRead {
        path: path.display().to_string(),
        reason: refusal.to_string(),
    };
    let reads: Vec<(TagField, FieldRead)> = match format {
        AudioFormat::Mp3 => match parse_id3_deep(&bytes).map_err(unreadable)? {
            Some(tag) => {
                let tag = as_saved(tag).map_err(unreadable)?;
                document.native = id3_native(&tag);
                TagField::ALL
                    .into_iter()
                    .map(|field| (field, id3_read(&tag, field)))
                    .collect()
            }
            None => Vec::new(),
        },
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => {
            match parse_vorbis_truth(format, &bytes).map_err(unreadable)? {
                Some(truth) => {
                    document.spellings = vorbis_spellings(&truth);
                    document.native = vorbis_native(&truth);
                    TagField::ALL
                        .into_iter()
                        .map(|field| (field, vorbis_read(&truth, field)))
                        .collect()
                }
                None => Vec::new(),
            }
        }
        AudioFormat::M4a => match parse_mp4_atoms(&bytes).map_err(unreadable)? {
            Some(atoms) => {
                document.native = mp4_native(&atoms, false);
                TagField::ALL
                    .into_iter()
                    .map(|field| (field, mp4_read(&atoms, field)))
                    .collect()
            }
            None => Vec::new(),
        },
        AudioFormat::Aac | AudioFormat::Wav => Vec::new(),
    };
    collect_reads(&mut document, reads);
    document.pictures = read_pictures(&bytes, format).map_err(unreadable)?;
    Ok(document)
}

/// A file's embedded pictures in file order. A file lofty cannot read is
/// an error, never "no pictures": a plan built on that would remove
/// them.
fn read_pictures(bytes: &[u8], format: AudioFormat) -> Result<Vec<PictureSpec>, Refusal> {
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

/// ID3 text frames no writable field owns, in either tag version.
fn id3_native(tag: &DeepId3) -> BTreeMap<String, Vec<String>> {
    let mut owned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for field in TagField::ALL {
        for v23 in [true, false] {
            match field.id3(v23) {
                Id3Target::Text(id) | Id3Target::Pair(id, _) => {
                    owned.insert(id.to_owned());
                }
                Id3Target::User(desc) => {
                    owned.insert(format!("TXXX:{}", desc.to_ascii_uppercase()));
                }
                Id3Target::SplitDate => {
                    owned.extend(["TYER", "TDAT", "TIME"].map(str::to_owned));
                }
                // Key-value frames are never listed; owned comments are
                // left out of the `COMM` listing below.
                Id3Target::Ufid(_)
                | Id3Target::Lyrics
                | Id3Target::People(..)
                | Id3Target::Comment => {}
            }
        }
    }
    let mut out = BTreeMap::new();
    for frame in &tag.frames {
        let Some(text) = &frame.text else {
            continue;
        };
        let key = match &text.desc {
            Some(desc) => format!("TXXX:{desc}"),
            None => frame.id.clone(),
        };
        let upper = match &text.desc {
            Some(desc) => format!("TXXX:{}", desc.to_ascii_uppercase()),
            None => frame.id.clone(),
        };
        if owned.contains(&upper) || text.values.iter().all(String::is_empty) {
            continue;
        }
        out.insert(format!("id3:{key}"), text.values.clone());
    }
    // Comment, link, private and object frames, as v2's scrub saw them:
    // each value is one frame's raw payload (base64), so a restore writes
    // the same bytes back. Frames left out of the list are never touched
    // (see `raw_listing`).
    let mut ids: Vec<&str> = tag
        .frames
        .iter()
        .filter(|frame| frame.text.is_none() && is_raw_native_id(&frame.id))
        .map(|frame| frame.id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let (listed, _) = raw_listing(&native_pool(&tag.frames, id));
        if listed.is_empty() {
            continue;
        }
        out.insert(
            format!("id3:{id}"),
            listed
                .into_iter()
                .map(|payload| {
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, payload)
                })
                .collect(),
        );
    }
    out
}

/// Largest frame listed as a native item (a bigger object stays as it
/// is: never scrubbed, never copied into a tag document).
const MAX_RAW_NATIVE_BYTES: usize = 1 << 20;

/// Raw payloads split into the ones listed as a native item and the ones
/// left alone. Frames over [`MAX_RAW_NATIVE_BYTES`] are left alone, and
/// so is every frame of a set that holds exact duplicates (the writer
/// cannot keep two identical frames apart). An edit of the id rewrites
/// the listed frames only and writes the others back unchanged.
fn raw_listing<'a>(all: &[&'a [u8]]) -> (Vec<&'a [u8]>, Vec<&'a [u8]>) {
    let duplicated = all
        .iter()
        .enumerate()
        .any(|(index, payload)| all[..index].contains(payload));
    if duplicated {
        return (Vec::new(), all.to_vec());
    }
    all.iter()
        .copied()
        .partition(|payload| payload.len() <= MAX_RAW_NATIVE_BYTES)
}

/// The raw frames of one id a native item covers: all of them, except
/// the `COMM` frames the comment field owns.
fn native_pool<'a>(frames: &'a [Id3FrameTruth], id: &str) -> Vec<&'a [u8]> {
    frames
        .iter()
        .filter(|frame| frame.text.is_none() && frame.id == id)
        .map(|frame| frame.payload.as_slice())
        .filter(|payload| id != "COMM" || !comment_owned(payload))
        .collect()
}

/// Every frame of a raw id after a native edit of it, starting from
/// `current`: the listed frames give way to `payloads`, the rest stay.
/// A `COMM` edit leaves the comment field's frames alone unless it
/// carries one itself: a native item recorded before the comment was a
/// field listed those frames too, and replaying it puts them back.
fn raw_after(current: &[Vec<u8>], id: &str, payloads: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let replaces_owned = id == "COMM" && payloads.iter().any(|payload| comment_owned(payload));
    let (pool, mut kept): (Vec<&[u8]>, Vec<Vec<u8>>) = if id == "COMM" && !replaces_owned {
        let (owned, rest): (Vec<&Vec<u8>>, Vec<&Vec<u8>>) =
            current.iter().partition(|payload| comment_owned(payload));
        (
            rest.into_iter().map(Vec::as_slice).collect(),
            owned.into_iter().cloned().collect(),
        )
    } else {
        (current.iter().map(Vec::as_slice).collect(), Vec::new())
    };
    let (_, left) = raw_listing(&pool);
    kept.extend(left.into_iter().map(<[u8]>::to_vec));
    kept.extend(payloads.iter().cloned());
    kept
}

/// ID3 frames listed whole as native items: comments, URL links,
/// private and encapsulated-object frames.
fn is_raw_native_id(id: &str) -> bool {
    matches!(id, "COMM" | "PRIV" | "GEOB")
        || (id.len() == 4 && id.starts_with('W') && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Vorbis comments no writable field owns (pictures excluded). A key
/// the file spells more than one way, or that holds an empty value, is
/// not listed: an edit of one spelling would touch the others, and an
/// empty value cannot be written back.
fn vorbis_native(truth: &VorbisTruth) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unlisted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut spelling: BTreeMap<String, &str> = BTreeMap::new();
    for (key, value) in &truth.pairs {
        let owned = TagField::ALL.into_iter().any(|field| {
            field
                .vorbis_keys()
                .iter()
                .any(|known| known.eq_ignore_ascii_case(key))
        });
        if owned || is_picture_field(key) {
            continue;
        }
        let folded = key.to_ascii_uppercase();
        if *spelling.entry(folded.clone()).or_insert(key.as_str()) != key.as_str()
            || value.is_empty()
        {
            unlisted.insert(folded);
            continue;
        }
        out.entry(format!("vorbis:{key}"))
            .or_default()
            .push(value.clone());
    }
    out.retain(|key, _| !unlisted.contains(&key["vorbis:".len()..].to_ascii_uppercase()));
    out
}

/// MP4 text atoms and iTunes freeforms no writable field owns, or with
/// `owned_too`, every one that holds text (what a native edit may
/// replace: an undo recorded before a field owned the item names it).
fn mp4_native(atoms: &[Mp4AtomTruth], owned_too: bool) -> BTreeMap<String, Vec<String>> {
    let mut owned_codes: Vec<[u8; 4]> = vec![*b"covr"];
    let mut owned_names: Vec<&'static str> = Vec::new();
    let fields: &[TagField] = if owned_too { &[] } else { &TagField::ALL };
    for field in fields.iter().copied() {
        match field.mp4() {
            Some(
                Mp4Target::Text(code)
                | Mp4Target::Pair(code, _)
                | Mp4Target::Flag(code)
                | Mp4Target::Integer(code),
            ) => owned_codes.push(code),
            Some(Mp4Target::Freeform(name)) => owned_names.push(name),
            None => {}
        }
    }
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // A freeform holding any value that is not text (or is empty) is not
    // listed: an edit replaces the whole item, and that value could not
    // be written back.
    let mut unlisted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for atom in atoms {
        if atom.kind == *b"----" {
            for item in &atom.items {
                if let Mp4Item::Freeform { mean, name, data } = item
                    && mean == MP4_MEAN
                    && !owned_names
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(name))
                {
                    let key = format!("mp4:----:{name}");
                    match std::str::from_utf8(data) {
                        Ok(text) if !text.is_empty() => {
                            out.entry(key).or_default().push(text.to_owned());
                        }
                        _ => {
                            unlisted.insert(key);
                        }
                    }
                }
            }
            continue;
        }
        if owned_codes.contains(&atom.kind) {
            continue;
        }
        let texts: Option<Vec<String>> = atom
            .items
            .iter()
            .map(|item| match item {
                Mp4Item::Text(text) if !text.is_empty() => Some(text.clone()),
                _ => None,
            })
            .collect();
        let code: String = atom.kind.iter().map(|byte| char::from(*byte)).collect();
        let key = format!("mp4:{code}");
        match texts.filter(|texts| !texts.is_empty()) {
            Some(texts) if !out.contains_key(&key) => {
                out.insert(key, texts);
            }
            // Two atoms of one code, or a value that is not text: left
            // alone.
            _ => {
                unlisted.insert(key);
            }
        }
    }
    out.retain(|key, _| !unlisted.contains(key));
    out
}

/// The native edit a [`TagEdit::native`] key names in this file.
fn native_edit(snapshot: &Snapshot, key: &str, values: &[String]) -> Result<NativeEdit, Refusal> {
    let foreign = || Refusal::NoByteInventory {
        detail: format!("tag '{key}' does not belong to this file's format"),
    };
    let values = values.to_vec();
    match (snapshot, key.split_once(':')) {
        (Snapshot::Id3 { .. }, Some(("id3", rest))) => match rest.strip_prefix("TXXX:") {
            Some(desc) => Ok(NativeEdit::Id3User {
                desc: desc.to_owned(),
                values,
            }),
            None if rest.len() == 4 && rest.starts_with('T') && !is_key_value_id(rest) => {
                Ok(NativeEdit::Id3Text {
                    id: rest.to_owned(),
                    values,
                })
            }
            None if is_raw_native_id(rest) => {
                let payloads = values
                    .iter()
                    .map(|value| {
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, value)
                            .map_err(|_| Refusal::NoByteInventory {
                                detail: format!("tag '{key}' holds a value that is not base64"),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(NativeEdit::Id3Raw {
                    id: rest.to_owned(),
                    payloads,
                })
            }
            None => Err(foreign()),
        },
        (Snapshot::Vorbis { truth, .. }, Some(("vorbis", rest))) if valid_vorbis_key(rest) => {
            // Spellings are matched without case on write, so a key the
            // file also spells another way, or holds empty, is refused
            // rather than touched.
            let blocked = truth.as_ref().is_some_and(|truth| {
                truth.pairs.iter().any(|(held, value)| {
                    held.eq_ignore_ascii_case(rest) && (held != rest || value.is_empty())
                })
            });
            if blocked {
                return Err(Refusal::NoByteInventory {
                    detail: format!("tag '{key}' is spelled more than one way or holds no value"),
                });
            }
            Ok(NativeEdit::Vorbis {
                key: rest.to_owned(),
                values,
            })
        }
        (Snapshot::Mp4 { atoms, .. }, Some(("mp4", rest))) => {
            // The item is replaced whole: one holding a value that is not
            // text is refused rather than lose that value.
            let listed = atoms
                .as_deref()
                .map(|atoms| mp4_native(atoms, true))
                .unwrap_or_default()
                .contains_key(key);
            let held = atoms.as_deref().is_some_and(|atoms| {
                atoms.iter().any(|atom| match rest.strip_prefix("----:") {
                    Some(name) => atom.items.iter().any(|item| {
                        matches!(item, Mp4Item::Freeform { mean, name: held, .. }
                            if mean == MP4_MEAN && held == name)
                    }),
                    None => {
                        let code: String = atom.kind.iter().map(|byte| char::from(*byte)).collect();
                        code == rest
                    }
                })
            });
            if held && !listed {
                return Err(Refusal::NoByteInventory {
                    detail: format!("tag '{key}' holds values that cannot be written back"),
                });
            }
            match rest.strip_prefix("----:") {
                Some(name) => Ok(NativeEdit::Mp4Freeform {
                    name: name.to_owned(),
                    values,
                }),
                None => {
                    let code: Vec<u8> = rest
                        .chars()
                        .map(|c| u8::try_from(u32::from(c)).map_err(|_| foreign()))
                        .collect::<Result<_, _>>()?;
                    let code: [u8; 4] = code.try_into().map_err(|_| foreign())?;
                    Ok(NativeEdit::Mp4Text { code, values })
                }
            }
        }
        _ => Err(foreign()),
    }
}

/// File each field read into the document.
fn collect_reads(document: &mut FieldDocument, reads: Vec<(TagField, FieldRead)>) {
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

/// Tags captured away from their file (an earlier DroppedNeedle's
/// original-file snapshot), in the native shapes the readers here take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturedTags {
    /// A whole ID3v2 tag, header included (MP3).
    Id3(Vec<u8>),
    /// Vorbis comment pairs (FLAC, Ogg, Opus).
    Vorbis(Vec<(String, String)>),
    /// The items of an MP4 `ilst` (M4A).
    Mp4(Vec<CapturedAtom>),
}

/// One MP4 `ilst` item: its four-byte kind and its data values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedAtom {
    pub kind: [u8; 4],
    pub values: Vec<CapturedValue>,
}

/// One MP4 data value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturedValue {
    Text(String),
    Integer(u64),
    Bool(bool),
    /// `trkn`/`disk`: number and total.
    Pair(u32, u32),
    /// A `----` freeform item.
    Freeform {
        mean: String,
        name: String,
        data: Vec<u8>,
    },
    Bytes(Vec<u8>),
}

/// The [`FieldDocument`] a file of `format` holding `captured` would
/// read as: the same native homes, spellings and opaque rules as
/// [`read_document`], so a document built here restores exactly like one
/// read from the file. Read-only containers report nothing.
pub fn document_from_captured(
    format: AudioFormat,
    captured: &CapturedTags,
) -> Result<FieldDocument, Refusal> {
    let mut document = FieldDocument::default();
    if !writable(format) {
        return Ok(document);
    }
    let reads: Vec<(TagField, FieldRead)> = match (format, captured) {
        (AudioFormat::Mp3, CapturedTags::Id3(bytes)) => match parse_id3_deep(bytes)? {
            Some(tag) => {
                let tag = as_saved(tag)?;
                TagField::ALL
                    .into_iter()
                    .map(|field| (field, id3_read(&tag, field)))
                    .collect()
            }
            None => Vec::new(),
        },
        (AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus, CapturedTags::Vorbis(pairs)) => {
            if let Some((key, _)) = pairs.iter().find(|(key, _)| !valid_vorbis_key(key)) {
                return Err(Refusal::NoByteInventory {
                    detail: format!("invalid comment key '{key}'"),
                });
            }
            let truth = VorbisTruth {
                vendor: String::new(),
                pairs: pairs.clone(),
            };
            document.spellings = vorbis_spellings(&truth);
            TagField::ALL
                .into_iter()
                .map(|field| (field, vorbis_read(&truth, field)))
                .collect()
        }
        (AudioFormat::M4a, CapturedTags::Mp4(atoms)) => {
            let atoms: Vec<Mp4AtomTruth> = atoms
                .iter()
                .map(|atom| Mp4AtomTruth {
                    kind: atom.kind,
                    items: atom.values.iter().map(mp4_item).collect(),
                    types: vec![None; atom.values.len()],
                })
                .collect();
            TagField::ALL
                .into_iter()
                .map(|field| (field, mp4_read(&atoms, field)))
                .collect()
        }
        _ => {
            return Err(Refusal::NoByteInventory {
                detail: format!("the captured tags are not {} tags", format.as_str()),
            });
        }
    };
    collect_reads(&mut document, reads);
    Ok(document)
}

fn mp4_item(value: &CapturedValue) -> Mp4Item {
    match value {
        CapturedValue::Text(text) => Mp4Item::Text(text.clone()),
        CapturedValue::Integer(value) => Mp4Item::Integer(*value),
        CapturedValue::Bool(value) => Mp4Item::Bool(*value),
        CapturedValue::Pair(number, total) => Mp4Item::Pair(*number, *total),
        CapturedValue::Freeform { mean, name, data } => Mp4Item::Freeform {
            mean: mean.clone(),
            name: name.clone(),
            data: data.clone(),
        },
        CapturedValue::Bytes(bytes) => Mp4Item::Bytes(bytes.clone()),
    }
}

/// The writable fields a file carries as text (see [`read_document`]).
pub fn read_fields(path: &Path) -> Result<BTreeMap<TagField, Vec<String>>, TagsError> {
    Ok(read_document(path)?.values)
}

// ---------------------------------------------------------------------------
// Pre-save scan: values each field can hold.
// ---------------------------------------------------------------------------

/// Frame ids that only exist in ID3v2.4. Any of them inside a v2.3 tag
/// makes it a mixed tag, saved whole as ID3v2.4 ([`upgrade_to_v24`]).
/// Probed against lofty 0.25.4 (2026-10-14): its v2.3 writer drops
/// `ASPI`, `EQU2`, `RVA2`, `SEEK`, `SIGN`, `TDEN`, `TDRL`, `TDTG`,
/// `TMOO`, `TPRO`, `TSOA`, `TSOP`, `TSOT` and `TSST`, shortens `TDOR` to
/// a year, and drops the `TDRC`, `TIPL` and `TMCL` frames the wrapper
/// puts back itself; its v2.4 writer keeps every frame, the v2.3-only
/// ones included. `POSS` and `TRSN` are ID3v2.4 frames too. `TSO2` is not on the list:
/// Picard and iTunes write it in v2.3 tags, and lofty's v2.3 writer
/// keeps it.
const V24_ONLY_IDS: &[&str] = &[
    "ASPI", "EQU2", "POSS", "RVA2", "SEEK", "SIGN", "TDEN", "TDOR", "TDRC", "TDRL", "TDTG", "TIPL",
    "TMCL", "TMOO", "TPRO", "TRSN", "TSOA", "TSOP", "TSOT", "TSST",
];

/// True when the field can hold these values (dates parse, counts are
/// numbers, single-valued fields have one value, nothing is blank).
#[must_use]
pub fn accepts(edit: &TagEdit) -> bool {
    check_edit(edit).is_ok()
}

fn check_edit(edit: &TagEdit) -> Result<(), Refusal> {
    if edit.verbatim {
        return Ok(());
    }
    let unencodable = |reason: String| Refusal::UnencodableItem {
        field: edit.field,
        reason,
    };
    if edit.values.iter().any(|value| value.is_empty()) {
        return Err(unencodable("empty values are not expressible".to_owned()));
    }
    let kind = edit.field.kind();
    if kind != FieldKind::Text && edit.values.len() > 1 {
        return Err(unencodable("the field holds one value".to_owned()));
    }
    for value in &edit.values {
        match kind {
            FieldKind::Count if value.trim().parse::<u32>().is_err() => {
                return Err(unencodable(format!(
                    "non-numeric count '{value}' would be dropped"
                )));
            }
            FieldKind::Flag if value != "0" && value != "1" => {
                return Err(unencodable(format!("flag '{value}' is not 1 or 0")));
            }
            FieldKind::Date if value.parse::<Timestamp>().is_err() => {
                return Err(unencodable(format!(
                    "unparseable timestamp '{value}' would poison ID3v2 reads"
                )));
            }
            FieldKind::Gain
                if value
                    .strip_suffix(" dB")
                    .and_then(|number| number.parse::<f64>().ok())
                    .is_none_or(|number| !number.is_finite()) =>
            {
                return Err(unencodable(format!("gain '{value}' is not '<number> dB'")));
            }
            FieldKind::Peak
                if value
                    .parse::<f64>()
                    .ok()
                    .is_none_or(|number| !number.is_finite() || number < 0.0) =>
            {
                return Err(unencodable(format!("peak '{value}' is not a number")));
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ID3 byte inventory: version, frame ids, text truth, raw payloads.
// ---------------------------------------------------------------------------

/// What the byte walker saw. Public so the mixed-v2.3 detector stays
/// unit-testable on synthetic headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Id3ByteReport {
    pub version_major: u8,
    pub version_minor: u8,
    pub frame_ids: Vec<String>,
    pub unsynchronised: bool,
    pub extended_header: bool,
    /// The tag could not be walked to the end (v2.2, truncation, garbage).
    pub truncated: bool,
}

/// Walk the ID3v2 tag at the start of `data`. `None` when no tag is there.
#[must_use]
pub fn inspect_id3_bytes(data: &[u8]) -> Option<Id3ByteReport> {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return None;
    }
    match parse_id3_deep(data) {
        Ok(Some(tag)) => Some(Id3ByteReport {
            version_major: tag.major,
            version_minor: tag.minor,
            frame_ids: tag.frames.iter().map(|frame| frame.id.clone()).collect(),
            unsynchronised: false,
            extended_header: false,
            truncated: false,
        }),
        Ok(None) => None,
        Err(_) => Some(Id3ByteReport {
            version_major: data[3],
            version_minor: data[4],
            frame_ids: Vec::new(),
            unsynchronised: data[5] & 0x80 != 0,
            extended_header: data[5] & 0x40 != 0,
            truncated: true,
        }),
    }
}

/// The mixed-v2.3 detector: v2.4-only frame ids inside a v2.3 tag.
#[must_use]
pub fn mixed_v23_detail(report: &Id3ByteReport) -> Option<String> {
    if report.version_major != 3 {
        return None;
    }
    let mixed: Vec<&str> = report
        .frame_ids
        .iter()
        .filter(|id| V24_ONLY_IDS.contains(&id.as_str()))
        .map(String::as_str)
        .collect();
    if mixed.is_empty() {
        None
    } else {
        Some(format!(
            "v2.4-only frames in v2.3 tag: {}",
            mixed.join(", ")
        ))
    }
}

fn syncsafe_u32(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |acc, byte| (acc << 7) | u32::from(byte & 0x7F))
}

/// One frame's truth: raw payload bytes plus decoded text when the frame
/// is a text kind this wrapper rebuilds.
#[derive(Clone)]
struct Id3FrameTruth {
    id: String,
    payload: Vec<u8>,
    text: Option<Id3TextTruth>,
}

#[derive(Clone)]
struct Id3TextTruth {
    /// Description for `TXXX`, `None` for plain text frames.
    desc: Option<String>,
    values: Vec<String>,
}

#[derive(Clone)]
struct DeepId3 {
    major: u8,
    minor: u8,
    frames: Vec<Id3FrameTruth>,
}

/// Key-value list ids, which are not text despite the `T` prefix.
fn is_key_value_id(id: &str) -> bool {
    matches!(id, "TIPL" | "TMCL")
}

/// Text frames: the `T` family minus the key-value lists, plus iTunes'
/// movement frames (`MVNM`, `MVIN`).
fn is_text_id(id: &str) -> bool {
    (id.starts_with('T') && !is_key_value_id(id)) || matches!(id, "MVNM" | "MVIN")
}

fn parse_id3_deep(data: &[u8]) -> Result<Option<DeepId3>, Refusal> {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return Ok(None);
    }
    let major = data[3];
    let minor = data[4];
    let flags = data[5];
    let no_inventory = |detail: String| Refusal::NoByteInventory { detail };
    if major == 2 || major > 4 {
        return Err(no_inventory(format!(
            "ID3v2.{major} upgrades on read and would not round-trip"
        )));
    }
    if flags & 0x80 != 0 {
        return Err(no_inventory(
            "unsynchronised tag cannot be inventoried".to_owned(),
        ));
    }
    if flags & 0x40 != 0 {
        return Err(no_inventory(
            "extended header cannot be inventoried".to_owned(),
        ));
    }
    let size = syncsafe_u32(&data[6..10]) as usize;
    if 10usize.saturating_add(size) > data.len() {
        return Err(no_inventory("tag overruns the file".to_owned()));
    }
    let tag_end = 10 + size;
    let mut frames = Vec::new();
    let mut offset = 10usize;
    while offset + 10 <= tag_end {
        if data[offset] == 0 {
            break; // Padding.
        }
        let id = &data[offset..offset + 4];
        if !id.iter().all(|byte| byte.is_ascii_alphanumeric()) {
            break; // Padding or garbage; stop rather than guess.
        }
        let frame_size = if major == 4 {
            syncsafe_u32(&data[offset + 4..offset + 8]) as usize
        } else {
            u32::from_be_bytes([
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]) as usize
        };
        let flag0 = data[offset + 8];
        let flag1 = data[offset + 9];
        // Any alteration, compression, encryption, grouping, or
        // unsynchronisation flag means reconstruction would not be
        // faithful; refuse rather than clear or misread bits.
        let risky = if major == 4 {
            flag0 != 0 || flag1 & 0x4F != 0
        } else {
            flag0 != 0 || flag1 & 0xE0 != 0
        };
        if risky {
            return Err(no_inventory(format!(
                "frame {} carries flags this wrapper cannot preserve",
                String::from_utf8_lossy(id)
            )));
        }
        let payload_end = offset.saturating_add(10).saturating_add(frame_size);
        if payload_end > tag_end {
            return Err(no_inventory("tag bytes end mid-frame".to_owned()));
        }
        let id = String::from_utf8_lossy(id).into_owned();
        let payload = data[offset + 10..payload_end].to_vec();
        let text = if id == "TXXX" {
            Some(decode_txxx(&payload).map_err(no_inventory)?)
        } else if is_text_id(&id) {
            Some(Id3TextTruth {
                desc: None,
                values: decode_id3_strings(&payload).map_err(no_inventory)?,
            })
        } else {
            None
        };
        frames.push(Id3FrameTruth { id, payload, text });
        offset = payload_end;
    }
    // A sized tag that yields zero frames is corrupt, not empty (an
    // emptied tag is all padding); refuse rather than verify against air.
    if frames.is_empty() && size > 0 && !data[10..tag_end].iter().all(|byte| *byte == 0) {
        return Err(no_inventory(
            "tag has size but no parseable frames".to_owned(),
        ));
    }
    // Duplicate text ids or TXXX descriptions cannot be patched back
    // unambiguously; refuse the pathology loudly.
    let mut seen_text = std::collections::HashSet::new();
    let mut seen_txxx = std::collections::HashSet::new();
    for frame in &frames {
        if let Some(text) = &frame.text {
            let novel = match &text.desc {
                Some(desc) => seen_txxx.insert(desc.clone()),
                None => seen_text.insert(frame.id.clone()),
            };
            if !novel {
                return Err(no_inventory(format!(
                    "duplicate {} frame cannot be patched back",
                    frame.id
                )));
            }
        }
    }
    Ok(Some(DeepId3 {
        major,
        minor,
        frames,
    }))
}

/// Decode a text-frame payload: encoding byte plus NUL-separated strings.
fn decode_id3_strings(payload: &[u8]) -> Result<Vec<String>, String> {
    let Some((encoding, body)) = payload.split_first() else {
        return Ok(Vec::new());
    };
    let parts: Vec<&[u8]> = match encoding {
        0 | 3 => body.split(|byte| *byte == 0).collect(),
        1 | 2 => split_utf16_units(body),
        _ => return Err(format!("unknown text encoding {encoding}")),
    };
    let mut values = Vec::new();
    let mut swap = false;
    for part in parts {
        // A trailing terminator yields a trailing empty part; drop those,
        // keep interior empties (they are real values).
        values.push(decode_id3_part_after(*encoding, part, &mut swap)?);
    }
    while values.last().is_some_and(String::is_empty) {
        values.pop();
    }
    Ok(values)
}

fn decode_txxx(payload: &[u8]) -> Result<Id3TextTruth, String> {
    let mut values = decode_id3_strings(payload)?;
    if values.is_empty() {
        return Ok(Id3TextTruth {
            desc: Some(String::new()),
            values: Vec::new(),
        });
    }
    let desc = values.remove(0);
    Ok(Id3TextTruth {
        desc: Some(desc),
        values,
    })
}

/// Split UTF-16 payloads on aligned double-NUL terminators.
fn split_utf16_units(body: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index + 1 < body.len() {
        if body[index] == 0 && body[index + 1] == 0 {
            parts.push(&body[start..index]);
            index += 2;
            start = index;
        } else {
            index += 2;
        }
    }
    parts.push(&body[start..]);
    parts
}

/// The text of a `USLT` frame.
fn decode_uslt(payload: &[u8]) -> Result<String, String> {
    decode_lang_text(payload).map(|(_, _, text)| text)
}

/// A `USLT` or `COMM` payload: encoding, three-byte language, a
/// terminated description, then the text.
fn decode_lang_text(payload: &[u8]) -> Result<([u8; 3], String, String), String> {
    let (&encoding, rest) = payload
        .split_first()
        .ok_or_else(|| "empty frame".to_owned())?;
    let language: [u8; 3] = rest
        .get(..3)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| "short frame".to_owned())?;
    let body = &rest[3..];
    let (desc_end, text_start) = if matches!(encoding, 1 | 2) {
        body.chunks_exact(2)
            .position(|pair| pair == [0, 0])
            .map(|index| (index * 2, index * 2 + 2))
    } else {
        body.iter()
            .position(|byte| *byte == 0)
            .map(|index| (index, index + 1))
    }
    .ok_or_else(|| "description not terminated".to_owned())?;
    let mut swap = false;
    let description = decode_id3_part_after(encoding, &body[..desc_end], &mut swap)?;
    let text = decode_id3_part_after(encoding, &body[text_start..], &mut swap)?;
    Ok((
        language,
        description,
        text.trim_end_matches('\0').to_owned(),
    ))
}

/// A `COMM` frame the comment field owns: one with an empty description.
fn comment_owned(payload: &[u8]) -> bool {
    decode_lang_text(payload).is_ok_and(|(_, description, _)| description.is_empty())
}

/// Strings for a frame this wrapper builds, each terminated: UTF-8 in
/// v2.4, UTF-16 with a byte-order mark in v2.3 (which has no UTF-8).
/// Returns the encoding byte and the body.
fn encode_id3_strings(v23: bool, parts: &[&str]) -> (u8, Vec<u8>) {
    let mut body = Vec::new();
    for part in parts {
        if v23 {
            body.extend_from_slice(&[0xFE, 0xFF]);
            for unit in part.encode_utf16() {
                body.extend_from_slice(&unit.to_be_bytes());
            }
            body.extend_from_slice(&[0, 0]);
        } else {
            body.extend_from_slice(part.as_bytes());
            body.push(0);
        }
    }
    (if v23 { 1 } else { 3 }, body)
}

/// A `COMM` payload with an empty description.
fn comment_payload(v23: bool, language: [u8; 3], text: &str) -> Vec<u8> {
    let (encoding, body) = encode_id3_strings(v23, &["", text]);
    let mut payload = vec![encoding];
    payload.extend_from_slice(&language);
    payload.extend_from_slice(&body);
    payload
}

/// The role and name pairs of a people frame, in order.
type People = Vec<(String, String)>;

/// The role and name pairs of a people frame.
fn decode_people(payload: &[u8]) -> Result<Vec<(String, String)>, String> {
    let values = decode_id3_strings(payload)?;
    if values.len() % 2 != 0 {
        return Err("a role without a name".to_owned());
    }
    Ok(values
        .chunks_exact(2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect())
}

fn people_payload(v23: bool, pairs: &[(String, String)]) -> Vec<u8> {
    let parts: Vec<&str> = pairs
        .iter()
        .flat_map(|(role, name)| [role.as_str(), name.as_str()])
        .collect();
    let (encoding, body) = encode_id3_strings(v23, &parts);
    let mut payload = vec![encoding];
    payload.extend_from_slice(&body);
    payload
}

/// The pairs of the one people frame `id`, none when it is absent. More
/// than one such frame, or one that does not decode, is an error.
fn people_pairs(tag: &DeepId3, id: &str) -> Result<Vec<(String, String)>, String> {
    let frames: Vec<&Id3FrameTruth> = tag.frames.iter().filter(|frame| frame.id == id).collect();
    match frames.as_slice() {
        [] => Ok(Vec::new()),
        [one] => decode_people(&one.payload),
        _ => Err(format!("several {id} frames")),
    }
}

/// The frames a role's pairs are read from. Performers on v2.4 are the
/// `TMCL` pairs, then the `TIPL` pairs outside [`INVOLVED_ROLES`]: an
/// ID3v2.3 tag upgraded to v2.4 (mutagen's `update_to_v24`) holds its
/// musician credits there, and Picard reads them the same way. A write
/// moves them to `TMCL`.
fn people_sources(id: &'static str, role: Role) -> Vec<&'static str> {
    if id == "TMCL" && role == Role::Performer {
        vec!["TMCL", "TIPL"]
    } else {
        vec![id]
    }
}

/// Whether `role` owns a pair whose role is `pair_role` in frame `id`.
fn role_owns(id: &str, role: Role, pair_role: &str) -> bool {
    match role {
        Role::Named(name) => pair_role == name,
        Role::Performer => id == "TMCL" || !INVOLVED_ROLES.contains(&pair_role),
    }
}

/// A musician credit as shown: `name (instrument)`, or the name alone.
fn performer_text(instrument: &str, name: &str) -> String {
    if instrument.is_empty() {
        name.to_owned()
    } else {
        format!("{name} ({instrument})")
    }
}

/// A musician credit split back into instrument and name. The instrument
/// is the balanced bracket group that ends the value, after a space, so
/// `A (drums (drum set))` keeps its inner brackets and `Sunn O)))` stays
/// a name. A name with no instrument that itself ends in a bracket group
/// is split too; MusicBrainz keeps disambiguation out of names.
fn performer_pair(value: &str) -> (String, String) {
    if value.ends_with(')') {
        let mut depth = 0usize;
        for (index, c) in value.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        if let Some(name) = value[..index].strip_suffix(' ')
                            && !name.is_empty()
                        {
                            let instrument = &value[index + 1..value.len() - 1];
                            return (instrument.to_owned(), name.to_owned());
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    (String::new(), value.to_owned())
}

/// `pairs` with the ones `role` owns replaced by `values`: the new pairs
/// take the owned pairs' places in order, any extra ones follow the last
/// owned place (the end when there was none), and owned places left over
/// go. Every other pair keeps its place, so a write of the same number
/// of values (an undo) puts interleaved pairs back where they were.
fn replace_people(
    id: &str,
    role: Role,
    pairs: &[(String, String)],
    values: &[String],
) -> Vec<(String, String)> {
    let mut new = values.iter().map(|value| match role {
        Role::Named(name) => (name.to_owned(), value.clone()),
        Role::Performer => performer_pair(value),
    });
    let owned = pairs
        .iter()
        .filter(|pair| role_owns(id, role, &pair.0))
        .count();
    let mut seen = 0;
    let mut out = Vec::new();
    for pair in pairs {
        if role_owns(id, role, &pair.0) {
            seen += 1;
            out.extend(new.next());
            if seen == owned {
                out.extend(new.by_ref());
            }
        } else {
            out.push(pair.clone());
        }
    }
    out.extend(new);
    out
}

/// One string of a frame. `swap` carries the byte order from the string
/// before: a UTF-16 string with no byte-order mark takes it. lofty writes
/// a v2.3 multi-string frame with one mark at the very start.
fn decode_id3_part_after(encoding: u8, part: &[u8], swap: &mut bool) -> Result<String, String> {
    match encoding {
        0 => Ok(part.iter().map(|byte| *byte as char).collect()),
        3 => std::str::from_utf8(part)
            .map(str::to_owned)
            .map_err(|_| "invalid UTF-8 in text frame".to_owned()),
        1 | 2 => {
            if part.len() % 2 != 0 {
                return Err("odd-length UTF-16 in text frame".to_owned());
            }
            let units: Vec<u16> = part
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            let units = if encoding == 1 {
                match units.first() {
                    Some(0xFEFF) => {
                        *swap = false;
                        &units[1..]
                    }
                    Some(0xFFFE) => {
                        *swap = true;
                        &units[1..]
                    }
                    // No mark: the order of the string before, big-endian
                    // for the first.
                    _ => units.as_slice(),
                }
            } else {
                *swap = false;
                units.as_slice()
            };
            let swap = *swap;
            char::decode_utf16(units.iter().map(
                |unit| {
                    if swap { unit.swap_bytes() } else { *unit }
                },
            ))
            .map(|decoded| decoded.map_err(|_| "invalid UTF-16 in text frame".to_owned()))
            .collect()
        }
        _ => Err(format!("unknown text encoding {encoding}")),
    }
}

// ---------------------------------------------------------------------------
// Vorbis raw inventory: the comment payload lofty will not show us.
// ---------------------------------------------------------------------------

struct VorbisTruth {
    vendor: String,
    pairs: Vec<(String, String)>,
}

/// Vorbis field rule, mirroring lofty's own check: ASCII 0x20-0x7D
/// except `=`, case-insensitive.
fn valid_vorbis_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| (0x20..=0x7D).contains(&byte) && byte != 0x3D)
}

/// Picture fields ride the pictures path, never the pairs path, so they
/// are neither pushed nor compared as pairs.
fn is_picture_field(key: &str) -> bool {
    key.eq_ignore_ascii_case("METADATA_BLOCK_PICTURE") || key.eq_ignore_ascii_case("COVERART")
}

fn parse_vorbis_truth(format: AudioFormat, bytes: &[u8]) -> Result<Option<VorbisTruth>, Refusal> {
    let no_inventory = |detail: String| Refusal::NoByteInventory { detail };
    let payload = match format {
        AudioFormat::Flac => flac_comment_block(bytes).map_err(no_inventory)?,
        AudioFormat::Ogg | AudioFormat::Opus => {
            ogg_comment_packet(format, bytes).map_err(no_inventory)?
        }
        _ => {
            return Err(no_inventory("not a Vorbis container".to_owned()));
        }
    };
    let Some(payload) = payload else {
        return Ok(None);
    };
    parse_vorbis_payload(&payload)
        .map(Some)
        .map_err(no_inventory)
}

fn flac_comment_block(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if bytes.len() < 4 || &bytes[..4] != b"fLaC" {
        return Err("not a FLAC stream".to_owned());
    }
    let mut offset = 4;
    loop {
        if offset + 4 > bytes.len() {
            return Err("FLAC metadata ends mid-block".to_owned());
        }
        let header = bytes[offset];
        let len = u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
            as usize;
        if offset + 4 + len > bytes.len() {
            return Err("FLAC block overruns the file".to_owned());
        }
        if header & 0x7F == 4 {
            return Ok(Some(bytes[offset + 4..offset + 4 + len].to_vec()));
        }
        offset += 4 + len;
        if header & 0x80 != 0 {
            return Ok(None);
        }
    }
}

/// Walk OGG pages, assemble the first stream's packets, and return the
/// comment packet (index 1). Anything chained or multiplexed is refused.
fn ogg_comment_packet(format: AudioFormat, bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut offset = 0;
    let mut serial: Option<u32> = None;
    let mut assembly: Vec<u8> = Vec::new();
    let mut packets: Vec<Vec<u8>> = Vec::new();
    while offset + 27 <= bytes.len() {
        if &bytes[offset..offset + 4] != b"OggS" {
            return Err("broken OGG page header".to_owned());
        }
        if bytes[offset + 4] != 0 {
            return Err("unsupported OGG version".to_owned());
        }
        let page_serial = u32::from_le_bytes(
            bytes[offset + 14..offset + 18]
                .try_into()
                .map_err(|_| "broken OGG page".to_owned())?,
        );
        let is_bos = bytes[offset + 5] & 0x02 != 0;
        match serial {
            None => serial = Some(page_serial),
            Some(first) if first == page_serial => {}
            Some(_) if is_bos => {
                return Err("chained OGG streams cannot be inventoried".to_owned());
            }
            Some(_) => {
                // Another multiplexed stream; skip its pages.
                let segments = bytes[offset + 26] as usize;
                if offset + 27 + segments > bytes.len() {
                    return Err("broken OGG page".to_owned());
                }
                let data: usize = bytes[offset + 27..offset + 27 + segments]
                    .iter()
                    .map(|segment| *segment as usize)
                    .sum();
                offset += 27 + segments + data;
                continue;
            }
        }
        let segments = bytes[offset + 26] as usize;
        if offset + 27 + segments > bytes.len() {
            return Err("broken OGG page".to_owned());
        }
        let table = &bytes[offset + 27..offset + 27 + segments];
        let mut data_at = offset + 27 + segments;
        for segment in table {
            let len = *segment as usize;
            if data_at + len > bytes.len() {
                return Err("broken OGG page".to_owned());
            }
            assembly.extend_from_slice(&bytes[data_at..data_at + len]);
            data_at += len;
            if len < 255 {
                packets.push(std::mem::take(&mut assembly));
            }
        }
        offset = data_at;
        if packets.len() >= 2 {
            break;
        }
    }
    if packets.len() < 2 {
        return Err("comment packet missing".to_owned());
    }
    let packet = &packets[1];
    let magic_len = match format {
        AudioFormat::Opus => {
            if packet.len() < 8 || &packet[..8] != b"OpusTags" {
                return Err("second Opus packet is not OpusTags".to_owned());
            }
            8
        }
        _ => {
            if packet.len() < 7 || &packet[..7] != b"\x03vorbis" {
                return Err("second Vorbis packet is not the comments".to_owned());
            }
            7
        }
    };
    Ok(Some(packet[magic_len..].to_vec()))
}

fn parse_vorbis_payload(payload: &[u8]) -> Result<VorbisTruth, String> {
    let mut cursor = 0;
    let take = |cursor: &mut usize, len: usize| -> Result<&[u8], String> {
        if *cursor + len > payload.len() {
            return Err("comment payload ends mid-field".to_owned());
        }
        let slice = &payload[*cursor..*cursor + len];
        *cursor += len;
        Ok(slice)
    };
    let take_u32 = |cursor: &mut usize| -> Result<usize, String> {
        Ok(u32::from_le_bytes(
            take(cursor, 4)?
                .try_into()
                .map_err(|_| "unreachable".to_owned())?,
        ) as usize)
    };
    let vendor_len = take_u32(&mut cursor)?;
    let vendor = std::str::from_utf8(take(&mut cursor, vendor_len)?)
        .map_err(|_| "vendor is not UTF-8".to_owned())?
        .to_owned();
    let count = take_u32(&mut cursor)?;
    if count > 4096 {
        return Err("absurd comment count".to_owned());
    }
    let mut pairs = Vec::with_capacity(count.min(512));
    for _ in 0..count {
        let len = take_u32(&mut cursor)?;
        let raw = std::str::from_utf8(take(&mut cursor, len)?)
            .map_err(|_| "comment is not UTF-8".to_owned())?;
        let Some((key, value)) = raw.split_once('=') else {
            return Err("comment without '=' cannot be inventoried".to_owned());
        };
        if !valid_vorbis_key(key) {
            return Err(format!("invalid comment key '{key}'"));
        }
        pairs.push((key.to_owned(), value.to_owned()));
    }
    // A framing bit plus zero padding may follow Vorbis comments
    // (mutagen pads OGG comment packets); anything else is garbage.
    let mut rest = &payload[cursor..];
    if rest.first() == Some(&1) {
        rest = &rest[1..];
    }
    if !rest.iter().all(|byte| *byte == 0) {
        return Err("trailing bytes after comments".to_owned());
    }
    Ok(VorbisTruth { vendor, pairs })
}

// ---------------------------------------------------------------------------
// MP4 atom inventory: type plus a hash of each `ilst` child's bytes.
// ---------------------------------------------------------------------------

/// One `ilst` child's semantic content. lofty normalizes encodings on
/// save (multi-data atoms split apart, integers shrink to minimal width,
/// tuples re-encoded), so atoms compare by meaning, not by bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mp4Item {
    Text(String),
    Integer(u64),
    Bool(bool),
    Pair(u32, u32),
    Freeform {
        mean: String,
        name: String,
        data: Vec<u8>,
    },
    Bytes(Vec<u8>),
}

struct Mp4AtomTruth {
    kind: [u8; 4],
    items: Vec<Mp4Item>,
    /// Each item's data type (1 is UTF-8, 21 a signed integer), `None`
    /// where the item has no data box or was not read from bytes.
    types: Vec<Option<u32>>,
}

/// Walk `moov/udta/meta/ilst` and hash every child atom. `Ok(None)` when
/// the file simply has no `ilst` (untagged); structural trouble is an error.
fn parse_mp4_atoms(bytes: &[u8]) -> Result<Option<Vec<Mp4AtomTruth>>, Refusal> {
    let no_inventory = |detail: String| Refusal::NoByteInventory { detail };
    let top = read_boxes(bytes, 0, bytes.len()).map_err(no_inventory)?;
    let Some(moov) = find_box(&top, b"moov") else {
        return Ok(None);
    };
    let moov_kids = read_boxes(bytes, moov.content, moov.end).map_err(no_inventory)?;
    let Some(udta) = find_box(&moov_kids, b"udta") else {
        return Ok(None);
    };
    let udta_kids = read_boxes(bytes, udta.content, udta.end).map_err(no_inventory)?;
    let Some(meta) = find_box(&udta_kids, b"meta") else {
        return Ok(None);
    };
    if meta.content + 4 > meta.end {
        return Err(no_inventory("truncated meta box".to_owned()));
    }
    let meta_kids = read_boxes(bytes, meta.content + 4, meta.end).map_err(no_inventory)?;
    let Some(ilst) = find_box(&meta_kids, b"ilst") else {
        return Ok(None);
    };
    let kids = read_boxes(bytes, ilst.content, ilst.end).map_err(no_inventory)?;
    let mut atoms = Vec::with_capacity(kids.len());
    for kid in &kids {
        let (items, types) = mp4_child_items(bytes, kid).into_iter().unzip();
        atoms.push(Mp4AtomTruth {
            kind: kid.kind,
            items,
            types,
        });
    }
    Ok(Some(atoms))
}

/// Data payloads of one `ilst` child, interpreted by kind and data type,
/// each with its data type.
fn mp4_child_items(bytes: &[u8], kid: &Mp4Box) -> Vec<(Mp4Item, Option<u32>)> {
    let data_type = |grandkid: &Mp4Box| {
        Some(u32::from_be_bytes([
            0,
            bytes[grandkid.content + 1],
            bytes[grandkid.content + 2],
            bytes[grandkid.content + 3],
        ]))
    };
    let grandkids = read_boxes(bytes, kid.content, kid.end).unwrap_or_default();
    if kid.kind == *b"----" {
        let mut mean = None;
        let mut name = None;
        let mut items = Vec::new();
        for grandkid in &grandkids {
            if grandkid.kind == *b"mean" && grandkid.content + 4 <= grandkid.end {
                mean = std::str::from_utf8(&bytes[grandkid.content + 4..grandkid.end])
                    .ok()
                    .map(str::to_owned);
            } else if grandkid.kind == *b"name" && grandkid.content + 4 <= grandkid.end {
                name = std::str::from_utf8(&bytes[grandkid.content + 4..grandkid.end])
                    .ok()
                    .map(str::to_owned);
            } else if grandkid.kind == *b"data" && grandkid.content + 8 <= grandkid.end {
                items.push((mean.clone(), name.clone(), grandkid));
            }
        }
        // Mean/name boxes precede the data boxes; pair positionally.
        return items
            .into_iter()
            .map(|(mean, name, grandkid)| {
                (
                    Mp4Item::Freeform {
                        mean: mean.unwrap_or_default(),
                        name: name.unwrap_or_default(),
                        data: bytes[grandkid.content + 8..grandkid.end].to_vec(),
                    },
                    data_type(grandkid),
                )
            })
            .collect();
    }
    let mut items = Vec::new();
    for grandkid in grandkids {
        if grandkid.kind != *b"data" || grandkid.content + 8 > grandkid.end {
            continue;
        }
        let payload = &bytes[grandkid.content + 8..grandkid.end];
        let data_type = data_type(&grandkid);
        items.push((mp4_data_item(kid.kind, data_type, payload), data_type));
    }
    if items.is_empty() {
        items.push((Mp4Item::Bytes(bytes[kid.start..kid.end].to_vec()), None));
    }
    items
}

/// One data payload by meaning. Only UTF-8 data (type 1), or data of the
/// implicit type 0, can be text: an integer whose bytes happen to be
/// printable stays an integer.
fn mp4_data_item(kind: [u8; 4], data_type: Option<u32>, payload: &[u8]) -> Mp4Item {
    if (kind == *b"trkn" || kind == *b"disk") && payload.len() >= 6 {
        return Mp4Item::Pair(
            u32::from(u16::from_be_bytes([payload[2], payload[3]])),
            u32::from(u16::from_be_bytes([payload[4], payload[5]])),
        );
    }
    // Movement number and count are integers, even when their bytes
    // happen to be printable.
    if kind == *b"\xa9mvi" || kind == *b"\xa9mvc" {
        match payload.len() {
            1 => return Mp4Item::Integer(u64::from(payload[0])),
            2 => return Mp4Item::Integer(u64::from(u16::from_be_bytes([payload[0], payload[1]]))),
            4 => {
                return Mp4Item::Integer(u64::from(u32::from_be_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                ])));
            }
            _ => {}
        }
    }
    if kind == *b"cpil" && payload.len() == 1 {
        return Mp4Item::Bool(payload[0] != 0);
    }
    if let Ok(text) = std::str::from_utf8(payload)
        && matches!(data_type, None | Some(0 | 1))
        && !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
    {
        // Line breaks are text (lyrics, comments), not binary.
        return Mp4Item::Text(text.to_owned());
    }
    match payload.len() {
        1 => Mp4Item::Integer(u64::from(payload[0])),
        2 => Mp4Item::Integer(u64::from(u16::from_be_bytes([payload[0], payload[1]]))),
        4 => Mp4Item::Integer(u64::from(u32::from_be_bytes(
            payload.try_into().unwrap_or([0, 0, 0, 0]),
        ))),
        8 => Mp4Item::Integer(u64::from_be_bytes(payload.try_into().unwrap_or([0; 8]))),
        _ => Mp4Item::Bytes(payload.to_vec()),
    }
}

struct Mp4Box {
    kind: [u8; 4],
    start: usize,
    content: usize,
    end: usize,
}

fn find_box<'a>(boxes: &'a [Mp4Box], kind: &[u8; 4]) -> Option<&'a Mp4Box> {
    boxes.iter().find(|candidate| &candidate.kind == kind)
}

fn read_boxes(bytes: &[u8], mut offset: usize, end: usize) -> Result<Vec<Mp4Box>, String> {
    let mut out = Vec::new();
    while offset + 8 <= end {
        let mut size = u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| "broken box".to_owned())?,
        ) as usize;
        let kind: [u8; 4] = bytes[offset + 4..offset + 8]
            .try_into()
            .map_err(|_| "broken box".to_owned())?;
        let mut content = offset + 8;
        if size == 1 {
            if offset + 16 > end {
                return Err("broken large box".to_owned());
            }
            size = u64::from_be_bytes(
                bytes[offset + 8..offset + 16]
                    .try_into()
                    .map_err(|_| "broken box".to_owned())?,
            ) as usize;
            content = offset + 16;
        } else if size == 0 {
            size = end - offset;
        }
        if size < content - offset || offset + size > end {
            return Err("box overruns its parent".to_owned());
        }
        out.push(Mp4Box {
            kind,
            start: offset,
            content,
            end: offset + size,
        });
        offset += size;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// A v2.3 tag holding v2.4 frames, saved whole as ID3v2.4.
// ---------------------------------------------------------------------------

/// The first note on a mixed tag, as the preview shows it.
const UPGRADE_NOTE: &str =
    "Its ID3v2.3 tag also holds newer ID3v2.4 tags, so it is saved as ID3v2.4.";

/// The fields held as text whose v2.3 home differs from their v2.4 one,
/// with the words a conflict note uses for two of their values.
const MOVED_FIELDS: &[(TagField, &str)] = &[
    (TagField::Date, "recording dates"),
    (TagField::OriginalDate, "original release dates"),
    (TagField::TitleSort, "title sort names"),
    (TagField::ArtistSort, "artist sort names"),
    (TagField::AlbumSort, "album sort names"),
    (TagField::DiscSubtitle, "disc subtitles"),
];

/// The credits read from people frames, checked pair by pair instead.
const PEOPLE_FIELDS: &[TagField] = &[TagField::Performer, TagField::Arranger, TagField::Producer];

fn is_mixed_v23(tag: &DeepId3) -> bool {
    tag.major == 3
        && tag
            .frames
            .iter()
            .any(|frame| V24_ONLY_IDS.contains(&frame.id.as_str()))
}

/// The tag as a save writes it: a mixed v2.3 tag as its ID3v2.4 form,
/// any other as it is.
fn as_saved(tag: DeepId3) -> Result<DeepId3, Refusal> {
    if is_mixed_v23(&tag) {
        Ok(upgrade_to_v24(&tag)?.tag)
    } else {
        Ok(tag)
    }
}

/// What a save does to the file's ID3 version: nothing unless its v2.3
/// tag also holds v2.4 frames. Then the first note says it is saved as
/// ID3v2.4, and one more names each value the tag holds twice, in a
/// v2.3 and a v2.4 frame, differently.
pub fn id3_upgrade_notes(path: &Path) -> Result<Vec<String>, TagsError> {
    if format_for_path(path)? != AudioFormat::Mp3 {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let unreadable = |refusal: Refusal| TagsError::TagRead {
        path: path.display().to_string(),
        reason: refusal.to_string(),
    };
    let Some(tag) = parse_id3_deep(&bytes).map_err(unreadable)? else {
        return Ok(Vec::new());
    };
    if !is_mixed_v23(&tag) {
        return Ok(Vec::new());
    }
    let upgrade = upgrade_to_v24(&tag).map_err(unreadable)?;
    let mut notes = vec![UPGRADE_NOTE.to_owned()];
    notes.extend(upgrade.conflicts);
    Ok(notes)
}

/// Frames lofty 0.25.4 keeps only once per tag: a second one replaces
/// the first on insert.
const ONE_PER_TAG_IDS: &[&str] = &[
    "MCDI", "ETCO", "MLLT", "SYTC", "RVRB", "PCNT", "RBUF", "POSS", "OWNE", "SEEK", "ASPI",
];

/// Why a save cannot keep this MP3's ID3 tag whole, in plain words, or
/// `None` when it can. The writer keeps one of two identical frames,
/// one `PCNT` and the like, and one `UFID` per owner, so a tag holding
/// more would lose a frame. A mixed v2.3 tag whose chapter or `LINK`
/// frames cannot move to ID3v2.4 is named too. The save would refuse
/// either; the preview says so first.
pub fn id3_unkept_frames(path: &Path) -> Result<Option<String>, TagsError> {
    if format_for_path(path)? != AudioFormat::Mp3 {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let unreadable = |refusal: Refusal| TagsError::TagRead {
        path: path.display().to_string(),
        reason: refusal.to_string(),
    };
    let Some(tag) = parse_id3_deep(&bytes).map_err(unreadable)? else {
        return Ok(None);
    };
    if is_mixed_v23(&tag)
        && let Err(reason) = v24_layouts(&tag)
    {
        return Ok(Some(reason));
    }
    let tag = as_saved(tag).map_err(unreadable)?;
    let mut seen: std::collections::HashSet<(&str, &[u8])> = std::collections::HashSet::new();
    for frame in tag.frames.iter().filter(|frame| frame.text.is_none()) {
        let key: &[u8] = if ONE_PER_TAG_IDS.contains(&frame.id.as_str()) {
            &[]
        } else if frame.id == "UFID" {
            frame
                .payload
                .split(|byte| *byte == 0)
                .next()
                .unwrap_or_default()
        } else {
            &frame.payload
        };
        if !seen.insert((frame.id.as_str(), key)) {
            return Ok(Some(format!(
                "The file's ID3 tag holds two {} frames where the tag writer keeps only one, so saving its tags would lose one.",
                frame.id
            )));
        }
    }
    Ok(None)
}

/// A mixed tag as ID3v2.4, and its conflict notes.
struct Upgrade {
    tag: DeepId3,
    conflicts: Vec<String>,
}

/// The frames a field's home holds in `tag`, in file order: the first
/// `TXXX` with the description (the one the field reads), the date's
/// `TYER`, `TDAT` and `TIME`, or the text frame.
fn home_frames(tag: &DeepId3, target: Id3Target) -> Vec<usize> {
    let position = |id: &str| tag.frames.iter().position(|frame| frame.id == id);
    match target {
        Id3Target::Text(id) => position(id).into_iter().collect(),
        Id3Target::SplitDate => ["TYER", "TDAT", "TIME"]
            .into_iter()
            .filter_map(position)
            .collect(),
        Id3Target::User(desc) => tag
            .frames
            .iter()
            .position(|frame| {
                frame
                    .text
                    .as_ref()
                    .and_then(|text| text.desc.as_ref())
                    .is_some_and(|held| held.eq_ignore_ascii_case(desc))
            })
            .into_iter()
            .collect(),
        Id3Target::Ufid(_)
        | Id3Target::Lyrics
        | Id3Target::Pair(..)
        | Id3Target::People(..)
        | Id3Target::Comment => Vec::new(),
    }
}

/// Whether a v2.3 value says the same as the v2.4 one: equal, or for a
/// date the same date with less detail (a year, or a day without its
/// time).
fn values_agree(field: TagField, old: &[String], new: &[String]) -> bool {
    if old == new {
        return true;
    }
    let dated = matches!(field, TagField::Date | TagField::OriginalDate);
    match (old, new) {
        ([old], [new]) if dated && date_parts(new).is_some() => {
            let rest = new.strip_prefix(old.as_str());
            match date_parts(old) {
                Some((_, None, None)) => rest.is_some_and(|rest| rest.starts_with('-')),
                Some((_, Some(_), None)) => rest.is_some_and(|rest| rest.starts_with('T')),
                _ => false,
            }
        }
        _ => false,
    }
}

/// A text frame holding `values`, as this writer builds one in v2.4.
fn v24_text_frame(id: &str, values: Vec<String>) -> Id3FrameTruth {
    let parts: Vec<&str> = values.iter().map(String::as_str).collect();
    let (encoding, body) = encode_id3_strings(false, &parts);
    let mut payload = vec![encoding];
    payload.extend_from_slice(&body);
    Id3FrameTruth {
        id: id.to_owned(),
        payload,
        text: Some(Id3TextTruth { desc: None, values }),
    }
}

/// The people pairs `TIPL` holds after `IPLS` joins it: its own pairs,
/// then each `IPLS` pair not already among them.
fn merged_people(held: &[(String, String)], extra: &[(String, String)]) -> People {
    let mut out = held.to_vec();
    for pair in extra {
        if !out.contains(pair) {
            out.push(pair.clone());
        }
    }
    out
}

/// Where an upgrade moves the credits: the `IPLS` frame that goes, the
/// `TIPL` frame it joins (none when the tag has none), and their pairs.
struct PeopleMove {
    ipls: usize,
    tipl: Option<usize>,
    held: People,
    pairs: People,
}

/// The credit move for `tag`, when there is one: a single `IPLS` that
/// decodes to pairs, and at most one `TIPL`, which decodes. Otherwise
/// both stay as they are.
fn people_move(tag: &DeepId3) -> Option<PeopleMove> {
    let positions = |id: &str| -> Vec<usize> {
        tag.frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.id == id)
            .map(|(index, _)| index)
            .collect()
    };
    let [ipls] = positions("IPLS")[..] else {
        return None;
    };
    let extra = decode_people(&tag.frames[ipls].payload).ok()?;
    if extra.is_empty() {
        return None;
    }
    let (tipl, held) = match positions("TIPL")[..] {
        [] => (None, Vec::new()),
        [tipl] => (Some(tipl), decode_people(&tag.frames[tipl].payload).ok()?),
        _ => return None,
    };
    Some(PeopleMove {
        ipls,
        tipl,
        pairs: merged_people(&held, &extra),
        held,
    })
}

/// A mixed v2.3 tag as the ID3v2.4 tag it is saved as. Every frame stays
/// as it was, except that a v2.3 frame whose value has a v2.4 home moves
/// there:
/// - `TXXX` sort names and disc subtitle go to `TSOT`, `TSOP`, `TSOA` and
///   `TSST`, `TYER`/`TDAT`/`TIME` to one `TDRC`, `TORY` to `TDOR`, filling
///   an empty v2.4 frame in place or adding one. When the v2.4 frame
///   already says the same, the v2.3 frames just go.
/// - When the two differ, both stay, the field reads the v2.4 frame as
///   v2.4 readers do, and a conflict note says so. Which is newer cannot
///   be known, so neither value is dropped.
/// - The `IPLS` pairs join `TIPL` after its own, and `IPLS` goes.
/// - A `TSRC` joined with '/' splits into separate values.
///
/// The result checks itself: everything that did not move is unchanged,
/// and every field reads what the rules above promise. A failure is a bug
/// here, refused as [`Refusal::VerifyMismatch`] before any write.
fn upgrade_to_v24(tag: &DeepId3) -> Result<Upgrade, Refusal> {
    let count = tag.frames.len();
    let mut frames: Vec<Option<Id3FrameTruth>> = tag.frames.iter().cloned().map(Some).collect();
    // Frames added before the frame at an index, in order.
    let mut added: Vec<(usize, Id3FrameTruth)> = Vec::new();
    let mut moved: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut conflicts = Vec::new();

    for (field, noun) in MOVED_FIELDS.iter().copied() {
        let FieldRead::Values(old) = id3_read_as(tag, field, true) else {
            continue;
        };
        let Id3Target::Text(home) = field.id3(false) else {
            continue;
        };
        let sources = home_frames(tag, field.id3(true));
        if let FieldRead::Values(new) = id3_read_as(tag, field, false) {
            if !values_agree(field, &old, &new) {
                let label: Vec<String> = sources
                    .iter()
                    .map(|index| frame_key(&tag.frames[*index]))
                    .collect();
                let now = new.join("; ");
                conflicts.push(format!(
                    "It holds two different {noun} ({home} {now}, {} {}). It now reads as \
                     {now}; the other value stays in its own frame, which most players ignore.",
                    label.join("/"),
                    old.join("; "),
                ));
                continue;
            }
        } else {
            let frame = v24_text_frame(home, old);
            match tag.frames.iter().position(|frame| frame.id == home) {
                Some(index) => frames[index] = Some(frame),
                None => added.push((sources.first().copied().unwrap_or(count), frame)),
            }
            moved.insert(home.to_owned());
        }
        for index in sources {
            moved.insert(frame_key(&tag.frames[index]));
            frames[index] = None;
        }
    }

    if let FieldRead::Values(split) = id3_read_as(tag, TagField::Isrc, true)
        && let Some(index) = tag.frames.iter().position(|frame| frame.id == "TSRC")
        && id3_text(tag, "TSRC") != Some(&split)
    {
        frames[index] = Some(v24_text_frame("TSRC", split));
        moved.insert("TSRC".to_owned());
    }

    let people = people_move(tag).filter(|people| {
        decode_people(&people_payload(false, &people.pairs))
            .ok()
            .as_ref()
            == Some(&people.pairs)
    });
    if let Some(people) = &people {
        let frame = Id3FrameTruth {
            id: "TIPL".to_owned(),
            payload: people_payload(false, &people.pairs),
            text: None,
        };
        match people.tipl {
            // Every IPLS pair was there already: TIPL keeps its bytes.
            Some(_) if people.pairs == people.held => {}
            Some(index) => frames[index] = Some(frame),
            None => added.push((people.ipls, frame)),
        }
        frames[people.ipls] = None;
        moved.extend(["IPLS".to_owned(), "TIPL".to_owned()]);
    }

    for (index, payload) in v24_layouts(tag).map_err(|reason| Refusal::SaveFailed { reason })? {
        if let Some(frame) = frames[index].as_mut() {
            frame.payload = payload;
            moved.insert(frame_key(frame));
        }
    }

    let mut out = Vec::with_capacity(count + added.len());
    for (index, frame) in frames.into_iter().enumerate() {
        out.extend(
            added
                .iter()
                .filter(|(at, _)| *at == index)
                .map(|(_, frame)| frame.clone()),
        );
        out.extend(frame);
    }
    out.extend(
        added
            .iter()
            .filter(|(at, _)| *at >= count)
            .map(|(_, frame)| frame.clone()),
    );
    let upgraded = DeepId3 {
        major: 4,
        minor: 0,
        frames: out,
    };
    check_upgrade(tag, &upgraded, &moved, people.as_ref())?;
    Ok(Upgrade {
        tag: upgraded,
        conflicts,
    })
}

/// The frames of a v2.3 tag whose payload is laid out by the tag's
/// version, rewritten for ID3v2.4: each index whose payload changes, with
/// its new payload. `CHAP` and `CTOC` embed whole frames, whose sizes
/// become syncsafe; nothing else in them changes. `LINK` names a frame
/// in three bytes in v2.3 and four in v2.4, so it cannot move unchanged.
/// The error is a plain sentence.
fn v24_layouts(tag: &DeepId3) -> Result<Vec<(usize, Vec<u8>)>, String> {
    let mut out = Vec::new();
    for (index, frame) in tag.frames.iter().enumerate() {
        let id = frame.id.as_str();
        let unmovable = || {
            format!(
                "The file's ID3v2.3 tag must be saved as ID3v2.4, and its {id} frame cannot \
                 move to ID3v2.4 unchanged."
            )
        };
        match id {
            "LINK" => return Err(unmovable()),
            "CHAP" | "CTOC" => {
                let payload = chapter_payload_v24(id, &frame.payload).ok_or_else(unmovable)?;
                if payload != frame.payload {
                    out.push((index, payload));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// A v2.3 `CHAP` or `CTOC` payload with its embedded frame headers in
/// the v2.4 layout, or `None` when it cannot be read or holds a frame
/// with flags.
fn chapter_payload_v24(id: &str, payload: &[u8]) -> Option<Vec<u8>> {
    let after_string = |at: usize| -> Option<usize> {
        Some(at + payload.get(at..)?.iter().position(|byte| *byte == 0)? + 1)
    };
    let mut at = after_string(0)?;
    if id == "CHAP" {
        at += 16;
    } else {
        let count = *payload.get(at + 1)?;
        at += 2;
        for _ in 0..count {
            at = after_string(at)?;
        }
    }
    let mut out = payload.get(..at)?.to_vec();
    while at < payload.len() {
        if payload[at] == 0 {
            out.extend_from_slice(&payload[at..]);
            break;
        }
        let header = payload.get(at..at + 10)?;
        let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        if header[8] != 0 || header[9] != 0 || size >= 1 << 28 {
            return None;
        }
        let end = (at + 10).checked_add(size as usize)?;
        out.extend_from_slice(&header[..4]);
        out.extend_from_slice(&[
            (size >> 21) as u8 & 0x7F,
            (size >> 14) as u8 & 0x7F,
            (size >> 7) as u8 & 0x7F,
            size as u8 & 0x7F,
        ]);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(payload.get(at + 10..end)?);
        at = end;
    }
    Some(out)
}

/// Whether lofty reads a `CHAP` or `CTOC` the same from its v2.3 and its
/// v2.4 payload: element id, times or entries, and every embedded frame.
fn same_chapter(id: &str, v23: &[u8], v24: &[u8]) -> bool {
    use lofty::config::ParseOptions;
    use lofty::id3::v2::{ChapterFrame, ChapterTableOfContentsFrame, FrameFlags, Id3v2Version};
    let options = ParseOptions::new();
    let flags = FrameFlags::default();
    if id == "CHAP" {
        let old = ChapterFrame::parse(&mut &v23[..], options, Id3v2Version::V3, flags);
        let new = ChapterFrame::parse(&mut &v24[..], options, Id3v2Version::V4, flags);
        matches!((old, new), (Ok(old), Ok(new))
            if old.id == new.id
                && old.times == new.times
                && old.offsets == new.offsets
                && old.children == new.children)
    } else {
        let old =
            ChapterTableOfContentsFrame::parse(&mut &v23[..], options, Id3v2Version::V3, flags);
        let new =
            ChapterTableOfContentsFrame::parse(&mut &v24[..], options, Id3v2Version::V4, flags);
        matches!((old, new), (Ok(old), Ok(new))
            if old.id == new.id
                && old.flags == new.flags
                && old.entries == new.entries
                && old.children == new.children)
    }
}

/// The upgrade's promises, checked: frames outside `moved` are as they
/// were, every field reads its v2.4 value or else the moved v2.3 one (an
/// ISRC its split values), the credits are the merged pairs, and each
/// chapter frame reads as it did.
fn check_upgrade(
    original: &DeepId3,
    upgraded: &DeepId3,
    moved: &std::collections::BTreeSet<String>,
    people: Option<&PeopleMove>,
) -> Result<(), Refusal> {
    let fail = |detail: String| Refusal::VerifyMismatch {
        detail: format!("ID3v2.4 upgrade: {detail}"),
    };
    let strip = |frames: &[Id3FrameTruth]| {
        let mut ids = key_multiset(frames);
        let mut text = text_map(frames);
        let mut raw = raw_map(frames);
        for key in moved {
            ids.remove(key);
            text.remove(key);
            raw.remove(key);
        }
        (ids, text, raw)
    };
    if strip(&original.frames) != strip(&upgraded.frames) {
        return Err(fail("frames changed outside the moved ones".to_owned()));
    }
    for field in TagField::ALL {
        if PEOPLE_FIELDS.contains(&field) {
            continue;
        }
        let old = id3_read_as(original, field, true);
        let new = id3_read_as(original, field, false);
        let expected = match (field, &old, &new) {
            (TagField::Isrc, FieldRead::Values(_), _)
            | (_, FieldRead::Values(_), FieldRead::Absent) => old,
            _ => new,
        };
        if id3_read_as(upgraded, field, false) != expected {
            return Err(fail(format!("{field:?} does not read as promised")));
        }
    }
    if let Some(people) = people {
        let tipl: Vec<&Id3FrameTruth> = upgraded
            .frames
            .iter()
            .filter(|frame| frame.id == "TIPL")
            .collect();
        let held = match tipl.as_slice() {
            [one] => decode_people(&one.payload).ok(),
            _ => None,
        };
        let expected = people_move(original).map(|again| again.pairs);
        if held.is_none()
            || held != expected
            || Some(&people.pairs) != expected.as_ref()
            || upgraded.frames.iter().any(|frame| frame.id == "IPLS")
        {
            return Err(fail("the credits did not merge as promised".to_owned()));
        }
    }
    let chapters = |tag: &DeepId3| -> Vec<(String, Vec<u8>)> {
        tag.frames
            .iter()
            .filter(|frame| matches!(frame.id.as_str(), "CHAP" | "CTOC"))
            .map(|frame| (frame.id.clone(), frame.payload.clone()))
            .collect()
    };
    let (old, new) = (chapters(original), chapters(upgraded));
    if old.len() != new.len()
        || old
            .iter()
            .zip(&new)
            .any(|((id, v23), (new_id, v24))| id != new_id || !same_chapter(id, v23, v24))
    {
        return Err(fail("a chapter frame does not read as it did".to_owned()));
    }
    Ok(())
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
// Edits resolved to their native homes in this file's tag.
// ---------------------------------------------------------------------------

/// One edit at its native home. No values (or a zero pair, or no flag)
/// removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NativeEdit {
    Id3Text {
        id: String,
        values: Vec<String>,
    },
    Id3User {
        desc: String,
        values: Vec<String>,
    },
    Id3Ufid {
        owner: String,
        value: Option<String>,
    },
    /// The one `USLT` frame: its text, or none to remove it.
    Id3Lyrics {
        text: Option<String>,
    },
    /// Every frame with this id after the save, as raw payloads (none
    /// removes them).
    Id3Raw {
        id: String,
        payloads: Vec<Vec<u8>>,
    },
    Vorbis {
        key: String,
        values: Vec<String>,
    },
    Mp4Text {
        code: [u8; 4],
        values: Vec<String>,
    },
    Mp4Freeform {
        name: String,
        values: Vec<String>,
    },
    Mp4Pair {
        code: [u8; 4],
        number: u32,
        total: u32,
    },
    Mp4Flag {
        code: [u8; 4],
        value: Option<bool>,
    },
    Mp4Integer {
        code: [u8; 4],
        value: Option<i32>,
    },
}

impl NativeEdit {
    /// The frame key verification excludes from the untouched set.
    fn id3_key(&self) -> Option<String> {
        match self {
            NativeEdit::Id3Text { id, .. } => Some(id.clone()),
            NativeEdit::Id3User { desc, .. } => Some(format!("TXXX:{desc}")),
            NativeEdit::Id3Ufid { owner, .. } => Some(format!("UFID:{owner}")),
            NativeEdit::Id3Lyrics { .. } => Some("USLT".to_owned()),
            NativeEdit::Id3Raw { id, .. } => Some(id.clone()),
            _ => None,
        }
    }

    fn same_target(&self, other: &NativeEdit) -> bool {
        match (self, other) {
            // Vorbis keys are matched without case on write.
            (NativeEdit::Vorbis { key: a, .. }, NativeEdit::Vorbis { key: b, .. }) => {
                a.eq_ignore_ascii_case(b)
            }
            (NativeEdit::Mp4Text { code: a, .. }, NativeEdit::Mp4Text { code: b, .. })
            | (NativeEdit::Mp4Pair { code: a, .. }, NativeEdit::Mp4Pair { code: b, .. })
            | (NativeEdit::Mp4Flag { code: a, .. }, NativeEdit::Mp4Flag { code: b, .. })
            | (NativeEdit::Mp4Integer { code: a, .. }, NativeEdit::Mp4Integer { code: b, .. }) => {
                a == b
            }
            (NativeEdit::Mp4Freeform { name: a, .. }, NativeEdit::Mp4Freeform { name: b, .. }) => {
                a == b
            }
            _ => self.id3_key().is_some() && self.id3_key() == other.id3_key(),
        }
    }
}

/// Resolve edits against the file's own spellings (an existing `TXXX`
/// description, every Vorbis spelling of a total the file uses, a
/// freeform name's case), fold track and disc halves into one pair with
/// the half that was not edited, split a v2.3 date over its three
/// frames, and let a later edit of the same target win.
fn resolve_edits(snapshot: &Snapshot, edits: &[TagEdit]) -> Result<Vec<NativeEdit>, Refusal> {
    let fields: Vec<TagEdit> = edits
        .iter()
        .filter(|edit| edit.native.is_none())
        .cloned()
        .collect();
    let mut resolved = resolve_field_edits(snapshot, &fields)?;
    for edit in edits {
        let Some(key) = &edit.native else {
            continue;
        };
        let mut native = native_edit(snapshot, key, &edit.values)?;
        if let NativeEdit::Id3Raw { id, payloads } = &mut native {
            let current: Vec<Vec<u8>> = match resolved.iter().find_map(|known| match known {
                NativeEdit::Id3Raw {
                    id: known,
                    payloads,
                } if known == id => Some(payloads),
                _ => None,
            }) {
                Some(payloads) => payloads.clone(),
                None => match snapshot {
                    Snapshot::Id3 { tag: Some(tag), .. } => tag
                        .frames
                        .iter()
                        .filter(|frame| frame.text.is_none() && frame.id == *id)
                        .map(|frame| frame.payload.clone())
                        .collect(),
                    _ => Vec::new(),
                },
            };
            *payloads = raw_after(&current, id, payloads);
        }
        resolved.retain(|known| !known.same_target(&native));
        resolved.push(native);
    }
    Ok(resolved)
}

fn resolve_field_edits(snapshot: &Snapshot, edits: &[TagEdit]) -> Result<Vec<NativeEdit>, Refusal> {
    let mut resolved: Vec<NativeEdit> = Vec::new();
    let mut push = |edit: NativeEdit| {
        resolved.retain(|known| !known.same_target(&edit));
        resolved.push(edit);
    };
    match snapshot {
        Snapshot::Id3 { tag, .. } => {
            let tag = tag.as_ref();
            let v23 = tag.is_some_and(|tag| tag.major == 3);
            let mut pairs: Vec<(&'static str, Option<String>, Option<String>)> = Vec::new();
            // Each people frame touched: its pairs, and the field that
            // last changed them.
            let mut people: Vec<(&'static str, People, TagField)> = Vec::new();
            let mut comments: Option<Vec<Vec<u8>>> = None;
            for edit in edits.iter().filter(|edit| edit.spelling.is_none()) {
                let unencodable = |reason: String| Refusal::UnencodableItem {
                    field: edit.field,
                    reason,
                };
                if let Id3Target::People(id, role) = edit.field.id3(v23) {
                    // Values the file already reads as (an undo or restore
                    // replays every field) leave the frames alone: a
                    // rewrite would move v2.4 musicians from TIPL to TMCL.
                    let unchanged = tag.is_some_and(|tag| match id3_read(tag, edit.field) {
                        FieldRead::Values(values) => values == edit.values,
                        FieldRead::Absent => edit.values.is_empty(),
                        FieldRead::Opaque => false,
                    });
                    if unchanged {
                        continue;
                    }
                    if edit.values.iter().any(|value| value.contains('\0')) {
                        return Err(unencodable(
                            "a credit cannot hold a NUL character".to_owned(),
                        ));
                    }
                    if role == Role::Performer {
                        for value in &edit.values {
                            let (instrument, name) = performer_pair(value);
                            if performer_text(&instrument, &name) != *value {
                                return Err(unencodable(format!(
                                    "'{value}' would not read back as written"
                                )));
                            }
                            // v2.3 keeps every credit in `IPLS`, where these
                            // roles are not musicians.
                            if v23 && INVOLVED_ROLES.contains(&instrument.as_str()) {
                                return Err(unencodable(format!(
                                    "'{value}' would read back as a {instrument} credit"
                                )));
                            }
                        }
                    }
                    let held = |id: &str| -> Result<Vec<(String, String)>, Refusal> {
                        Ok(tag
                            .map(|tag| people_pairs(tag, id))
                            .transpose()
                            .map_err(|detail| Refusal::NoByteInventory { detail })?
                            .unwrap_or_default())
                    };
                    for (index, source) in people_sources(id, role).into_iter().enumerate() {
                        let slot = match people.iter().position(|(known, ..)| *known == source) {
                            Some(slot) => slot,
                            None => {
                                let pairs = held(source)?;
                                // A frame the role holds nothing in is
                                // left as it is.
                                if index > 0
                                    && !pairs.iter().any(|pair| role_owns(source, role, &pair.0))
                                {
                                    continue;
                                }
                                people.push((source, pairs, edit.field));
                                people.len() - 1
                            }
                        };
                        // The values go to the first frame; the others
                        // only give up the role's pairs.
                        let values: &[String] = if index == 0 { &edit.values } else { &[] };
                        people[slot].1 = replace_people(source, role, &people[slot].1, values);
                        people[slot].2 = edit.field;
                    }
                    continue;
                }
                if edit.field.id3(v23) == Id3Target::Comment {
                    let current = comments.get_or_insert_with(|| {
                        tag.map(|tag| {
                            tag.frames
                                .iter()
                                .filter(|frame| frame.id == "COMM")
                                .map(|frame| frame.payload.clone())
                                .collect()
                        })
                        .unwrap_or_default()
                    });
                    let owned: Vec<&Vec<u8>> = current
                        .iter()
                        .filter(|payload| comment_owned(payload))
                        .collect();
                    if owned.len() > 1 {
                        return Err(unencodable(
                            "the file holds several comments; they cannot be written as one"
                                .to_owned(),
                        ));
                    }
                    // Keep the comment's language, or use English.
                    let language = owned
                        .first()
                        .and_then(|payload| decode_lang_text(payload).ok())
                        .map_or(*b"eng", |(language, _, _)| language);
                    current.retain(|payload| !comment_owned(payload));
                    if let Some(text) = edit.values.first() {
                        if text.contains('\0') {
                            return Err(unencodable(
                                "a comment cannot hold a NUL character".to_owned(),
                            ));
                        }
                        let payload = comment_payload(v23, language, text);
                        if decode_lang_text(&payload).ok()
                            != Some((language, String::new(), text.clone()))
                        {
                            return Err(unencodable(format!(
                                "'{text}' would not read back as written"
                            )));
                        }
                        current.push(payload);
                    }
                    continue;
                }
                // lofty writes v2.3 multi-values with '/', so say so up
                // front and verify what lands.
                let values = if v23 && edit.values.len() > 1 {
                    vec![edit.values.join("/")]
                } else {
                    edit.values.clone()
                };
                match edit.field.id3(v23) {
                    Id3Target::Text(id) => {
                        let values = if id == "TORY" {
                            values.iter().map(|value| year_of(value)).collect()
                        } else {
                            values
                        };
                        push(NativeEdit::Id3Text {
                            id: id.to_owned(),
                            values,
                        });
                    }
                    Id3Target::User(desc) => push(NativeEdit::Id3User {
                        desc: tag
                            .and_then(|tag| existing_txxx(tag, desc))
                            .unwrap_or_else(|| desc.to_owned()),
                        values,
                    }),
                    Id3Target::Ufid(owner) => push(NativeEdit::Id3Ufid {
                        owner: owner.to_owned(),
                        value: values.into_iter().next(),
                    }),
                    Id3Target::Lyrics => push(NativeEdit::Id3Lyrics {
                        text: (!edit.values.is_empty()).then(|| edit.values.join("\n")),
                    }),
                    Id3Target::Pair(id, slot) => {
                        let index = match pairs.iter().position(|(known, _, _)| *known == id) {
                            Some(index) => index,
                            None => {
                                let (number, total) =
                                    tag.map_or((None, None), |tag| id3_pair(tag, id));
                                pairs.push((id, number, total));
                                pairs.len() - 1
                            }
                        };
                        let value = values
                            .first()
                            .map(|value| value.trim().to_owned())
                            .filter(|value| !value.is_empty());
                        match slot {
                            Slot::Number => pairs[index].1 = value,
                            Slot::Total => pairs[index].2 = value,
                        }
                    }
                    Id3Target::SplitDate => {
                        let (year, day, time) = split_v23_date(values.first().map(String::as_str));
                        for (id, values) in [("TYER", year), ("TDAT", day), ("TIME", time)] {
                            push(NativeEdit::Id3Text {
                                id: id.to_owned(),
                                values,
                            });
                        }
                    }
                    Id3Target::People(..) | Id3Target::Comment => {}
                }
            }
            for (id, pairs, field) in people {
                let payloads = if pairs.is_empty() {
                    Vec::new()
                } else {
                    let payload = people_payload(v23, &pairs);
                    if decode_people(&payload).ok().as_ref() != Some(&pairs) {
                        return Err(Refusal::UnencodableItem {
                            field,
                            reason: format!("the {id} credits would not read back as written"),
                        });
                    }
                    vec![payload]
                };
                push(NativeEdit::Id3Raw {
                    id: id.to_owned(),
                    payloads,
                });
            }
            if let Some(payloads) = comments {
                push(NativeEdit::Id3Raw {
                    id: "COMM".to_owned(),
                    payloads,
                });
            }
            for (id, number, total) in pairs {
                let values = match (number, total) {
                    (None, None) => Vec::new(),
                    (Some(number), None) => vec![number],
                    (number, Some(total)) => {
                        vec![format!("{}/{total}", number.as_deref().unwrap_or("0"))]
                    }
                };
                push(NativeEdit::Id3Text {
                    id: id.to_owned(),
                    values,
                });
            }
        }
        Snapshot::Vorbis { truth, .. } => {
            for edit in edits {
                if let Some(key) = edit.spelling {
                    push(NativeEdit::Vorbis {
                        key: key.to_owned(),
                        values: edit.values.clone(),
                    });
                    continue;
                }
                for key in vorbis_write_keys(edit.field, truth.as_ref()) {
                    push(NativeEdit::Vorbis {
                        key: key.to_owned(),
                        values: edit.values.clone(),
                    });
                }
            }
        }
        Snapshot::Mp4 { atoms, .. } => {
            let mut pairs: Vec<([u8; 4], u32, u32)> = Vec::new();
            for edit in edits.iter().filter(|edit| edit.spelling.is_none()) {
                let unencodable = |reason: &str| Refusal::UnencodableItem {
                    field: edit.field,
                    reason: reason.to_owned(),
                };
                let Some(target) = edit.field.mp4() else {
                    // Nothing to remove where the format has no home.
                    if edit.values.is_empty() {
                        continue;
                    }
                    return Err(unencodable("MP4 has no home for this field"));
                };
                match target {
                    Mp4Target::Text(code) => push(NativeEdit::Mp4Text {
                        code,
                        values: edit.values.clone(),
                    }),
                    Mp4Target::Integer(code) => {
                        let value = match edit.values.first() {
                            None => None,
                            Some(value) => Some(
                                value
                                    .trim()
                                    .parse::<i32>()
                                    .ok()
                                    .filter(|value| *value >= 0)
                                    .ok_or_else(|| {
                                        unencodable("MP4 numbers must be whole numbers")
                                    })?,
                            ),
                        };
                        push(NativeEdit::Mp4Integer { code, value });
                    }
                    Mp4Target::Freeform(name) => push(NativeEdit::Mp4Freeform {
                        name: atoms
                            .as_deref()
                            .and_then(|atoms| existing_freeform(atoms, name))
                            .unwrap_or_else(|| name.to_owned()),
                        values: edit.values.clone(),
                    }),
                    Mp4Target::Pair(code, slot) => {
                        let index = match pairs.iter().position(|(known, _, _)| *known == code) {
                            Some(index) => index,
                            None => {
                                let (number, total) = atoms
                                    .as_deref()
                                    .and_then(|atoms| mp4_pair(atoms, code))
                                    .unwrap_or((0, 0));
                                pairs.push((code, number, total));
                                pairs.len() - 1
                            }
                        };
                        let value = match edit.values.first() {
                            None => 0,
                            Some(value) => value
                                .trim()
                                .parse()
                                .map_err(|_| unencodable("MP4 numbers must be whole numbers"))?,
                        };
                        match slot {
                            Slot::Number => pairs[index].1 = value,
                            Slot::Total => pairs[index].2 = value,
                        }
                    }
                    Mp4Target::Flag(code) => {
                        let value = match edit.values.first().map(|value| value.trim()) {
                            None => None,
                            Some("1") => Some(true),
                            Some("0") => Some(false),
                            Some(_) => return Err(unencodable("MP4 flags are 1 or 0")),
                        };
                        push(NativeEdit::Mp4Flag { code, value });
                    }
                }
            }
            for (code, number, total) in pairs {
                push(NativeEdit::Mp4Pair {
                    code,
                    number,
                    total,
                });
            }
        }
    }
    Ok(resolved)
}

/// A date's parts: year, then month and day, then hour and minute.
type DateParts<'a> = (
    &'a str,
    Option<(&'a str, &'a str)>,
    Option<(&'a str, &'a str)>,
);

/// Split `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM[:SS]`;
/// anything else is not a date this code reshapes.
fn date_parts(value: &str) -> Option<DateParts<'_>> {
    let digits = |part: &str, width: usize| {
        part.len() == width && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    let (date, time) = match value.trim().split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (value.trim(), None),
    };
    let mut parts = date.split('-');
    let year = parts.next().filter(|year| digits(year, 4))?;
    let month = parts.next();
    let day = parts.next();
    if parts.next().is_some() || month.is_some_and(|month| !digits(month, 2)) {
        return None;
    }
    if day.is_some_and(|day| !digits(day, 2)) {
        return None;
    }
    let month_day = match (month, day) {
        (Some(month), Some(day)) => Some((month, day)),
        (Some(_), None) if time.is_none() => None,
        (None, None) if time.is_none() => None,
        _ => return None,
    };
    let clock = match time {
        None => None,
        Some(time) => {
            let mut clock = time.split(':');
            let hour = clock.next().filter(|hour| digits(hour, 2))?;
            let minute = clock.next().filter(|minute| digits(minute, 2))?;
            Some((hour, minute))
        }
    };
    Some((year, month_day, clock))
}

/// The year of a date, or the value as it is when it is not a date.
fn year_of(value: &str) -> String {
    match date_parts(value) {
        Some((year, _, _)) => year.to_owned(),
        None => value.to_owned(),
    }
}

/// v2.3 date frames for one date: `TYER`, `TDAT` (`DDMM`), `TIME`
/// (`HHMM`). A value that is not a date goes to `TYER` as it is.
fn split_v23_date(value: Option<&str>) -> (Vec<String>, Vec<String>, Vec<String>) {
    let Some(value) = value else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    match date_parts(value) {
        None => (vec![value.to_owned()], Vec::new(), Vec::new()),
        Some((year, month_day, clock)) => (
            vec![year.to_owned()],
            month_day
                .map(|(month, day)| vec![format!("{day}{month}")])
                .unwrap_or_default(),
            match (month_day, clock) {
                (Some(_), Some((hour, minute))) => vec![format!("{hour}{minute}")],
                _ => Vec::new(),
            },
        ),
    }
}

fn existing_txxx(tag: &DeepId3, desc: &str) -> Option<String> {
    tag.frames.iter().find_map(|frame| {
        let existing = frame.text.as_ref()?.desc.as_ref()?;
        existing
            .eq_ignore_ascii_case(desc)
            .then(|| existing.clone())
    })
}

fn existing_freeform(atoms: &[Mp4AtomTruth], name: &str) -> Option<String> {
    atoms
        .iter()
        .flat_map(|atom| atom.items.iter())
        .find_map(|item| match item {
            Mp4Item::Freeform {
                mean,
                name: existing,
                ..
            } if mean == MP4_MEAN && existing.eq_ignore_ascii_case(name) => Some(existing.clone()),
            _ => None,
        })
}

fn id3_text<'a>(tag: &'a DeepId3, id: &str) -> Option<&'a Vec<String>> {
    tag.frames
        .iter()
        .find(|frame| frame.id == id)
        .and_then(|frame| frame.text.as_ref())
        .map(|text| &text.values)
}

/// The `n/m` halves of an ID3 pair frame, exactly as written.
fn id3_pair(tag: &DeepId3, id: &str) -> (Option<String>, Option<String>) {
    let Some(text) = id3_text(tag, id).and_then(|values| values.first()) else {
        return (None, None);
    };
    let (number, total) = match text.split_once('/') {
        Some((number, total)) => (number, Some(total)),
        None => (text.as_str(), None),
    };
    let present = |part: &str| (!part.is_empty()).then(|| part.to_owned());
    (present(number), total.and_then(present))
}

fn mp4_pair(atoms: &[Mp4AtomTruth], code: [u8; 4]) -> Option<(u32, u32)> {
    atoms
        .iter()
        .filter(|atom| atom.kind == code)
        .flat_map(|atom| atom.items.iter())
        .find_map(|item| match item {
            Mp4Item::Pair(number, total) => Some((*number, *total)),
            _ => None,
        })
}

fn vorbis_present(truth: Option<&VorbisTruth>, key: &str) -> bool {
    truth.is_some_and(|truth| {
        truth
            .pairs
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(key))
    })
}

/// The Vorbis keys a write of `field` updates: every spelling the file
/// already uses, or Picard's when it uses none.
fn vorbis_write_keys(field: TagField, truth: Option<&VorbisTruth>) -> Vec<&'static str> {
    let keys = field.vorbis_keys();
    let present: Vec<&'static str> = keys
        .iter()
        .copied()
        .filter(|key| vorbis_present(truth, key))
        .collect();
    if present.is_empty() {
        keys[..1].to_vec()
    } else {
        present
    }
}

/// What a file holds for one field.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldRead {
    Absent,
    Values(Vec<String>),
    /// Present, but not as text this code can write back unchanged.
    Opaque,
}

fn values_or_absent(values: Vec<String>) -> FieldRead {
    if values.is_empty() {
        FieldRead::Absent
    } else {
        FieldRead::Values(values)
    }
}

/// A field's values as the file's ID3 tag holds them.
fn id3_read(tag: &DeepId3, field: TagField) -> FieldRead {
    id3_read_as(tag, field, tag.major == 3)
}

/// A field's values read from its v2.3 home (`v23`) or its v2.4 one,
/// whatever the tag's own version.
fn id3_read_as(tag: &DeepId3, field: TagField, v23: bool) -> FieldRead {
    match field.id3(v23) {
        // ID3v2.3 has one text value per frame, so several ISRCs are
        // written joined with '/'; no ISRC holds one, so split it back.
        Id3Target::Text(id) if v23 && field == TagField::Isrc => values_or_absent(
            id3_text(tag, id)
                .cloned()
                .unwrap_or_default()
                .iter()
                .flat_map(|value| value.split('/'))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        Id3Target::Text(id) => values_or_absent(id3_text(tag, id).cloned().unwrap_or_default()),
        Id3Target::User(desc) => values_or_absent(
            tag.frames
                .iter()
                .find_map(|frame| {
                    let text = frame.text.as_ref()?;
                    text.desc
                        .as_ref()?
                        .eq_ignore_ascii_case(desc)
                        .then(|| text.values.clone())
                })
                .unwrap_or_default(),
        ),
        Id3Target::Ufid(owner) => tag
            .frames
            .iter()
            .filter(|frame| frame.id == "UFID")
            .find_map(|frame| {
                let (frame_owner, identifier) = split_ufid(&frame.payload);
                (frame_owner == owner).then(|| match std::str::from_utf8(identifier) {
                    Ok(text) => FieldRead::Values(vec![text.to_owned()]),
                    Err(_) => FieldRead::Opaque,
                })
            })
            .unwrap_or(FieldRead::Absent),
        Id3Target::Lyrics => {
            let frames: Vec<&Id3FrameTruth> = tag
                .frames
                .iter()
                .filter(|frame| frame.id == "USLT")
                .collect();
            match frames.as_slice() {
                [] => FieldRead::Absent,
                [one] => match decode_uslt(&one.payload) {
                    Ok(text) if !text.is_empty() => FieldRead::Values(vec![text]),
                    Ok(_) => FieldRead::Absent,
                    Err(_) => FieldRead::Opaque,
                },
                // Several lyrics frames (languages, descriptions) cannot
                // be written back as one value.
                _ => FieldRead::Opaque,
            }
        }
        Id3Target::Pair(id, slot) => {
            let (number, total) = id3_pair(tag, id);
            match slot {
                Slot::Number => number,
                Slot::Total => total,
            }
            .map_or(FieldRead::Absent, |value| FieldRead::Values(vec![value]))
        }
        Id3Target::SplitDate => {
            let first = |id: &str| id3_text(tag, id).and_then(|values| values.first()).cloned();
            let four_digits =
                |value: &str| value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_digit());
            match (first("TYER"), first("TDAT"), first("TIME")) {
                (None, None, None) => FieldRead::Absent,
                (Some(year), None, None) => FieldRead::Values(vec![year]),
                (Some(year), Some(day), time)
                    if four_digits(&year)
                        && four_digits(&day)
                        && time.as_deref().is_none_or(four_digits) =>
                {
                    let mut date = format!("{year}-{}-{}", &day[2..], &day[..2]);
                    if let Some(time) = time {
                        date.push_str(&format!("T{}:{}", &time[..2], &time[2..]));
                    }
                    FieldRead::Values(vec![date])
                }
                _ => FieldRead::Opaque,
            }
        }
        Id3Target::People(id, role) => {
            let mut owned: Vec<(String, String)> = Vec::new();
            for source in people_sources(id, role) {
                match people_pairs(tag, source) {
                    Ok(pairs) => owned.extend(
                        pairs
                            .into_iter()
                            .filter(|(pair_role, _)| role_owns(source, role, pair_role)),
                    ),
                    Err(_) => return FieldRead::Opaque,
                }
            }
            let values: Vec<String> = owned
                .iter()
                .map(|(instrument, name)| match role {
                    Role::Named(_) => name.clone(),
                    Role::Performer => performer_text(instrument, name),
                })
                .collect();
            // A pair that would not come back from its text (no name, a
            // name ending in parentheses) cannot be written back.
            let faithful = owned.iter().zip(&values).all(|(pair, value)| {
                !pair.1.is_empty()
                    && match role {
                        Role::Named(_) => true,
                        Role::Performer => performer_pair(value) == *pair,
                    }
            });
            if faithful {
                values_or_absent(values)
            } else {
                FieldRead::Opaque
            }
        }
        Id3Target::Comment => {
            let owned: Vec<String> = tag
                .frames
                .iter()
                .filter(|frame| frame.id == "COMM")
                .filter_map(|frame| decode_lang_text(&frame.payload).ok())
                .filter(|(_, description, _)| description.is_empty())
                .map(|(_, _, text)| text)
                .collect();
            match owned.as_slice() {
                [] => FieldRead::Absent,
                [text] if !text.is_empty() && !text.contains('\0') => FieldRead::Values(owned),
                // Several comments, an empty one, or one holding a NUL
                // cannot be written back as one value.
                _ => FieldRead::Opaque,
            }
        }
    }
}

/// A field's values as the file's Vorbis comment holds them (Picard's
/// spelling first when the file uses several).
fn vorbis_read(truth: &VorbisTruth, field: TagField) -> FieldRead {
    for key in field.vorbis_keys() {
        let values: Vec<String> = truth
            .pairs
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.clone())
            .collect();
        if !values.is_empty() {
            return FieldRead::Values(values);
        }
    }
    FieldRead::Absent
}

/// Every spelling's values for fields the file spells more than one way.
fn vorbis_spellings(truth: &VorbisTruth) -> BTreeMap<(TagField, &'static str), Vec<String>> {
    let mut spellings = BTreeMap::new();
    for field in TagField::ALL {
        let present: Vec<(&'static str, Vec<String>)> = field
            .vorbis_keys()
            .iter()
            .map(|key| {
                let values: Vec<String> = truth
                    .pairs
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(key))
                    .map(|(_, value)| value.clone())
                    .collect();
                (*key, values)
            })
            .filter(|(_, values)| !values.is_empty())
            .collect();
        if present.len() > 1 {
            for (key, values) in present {
                spellings.insert((field, key), values);
            }
        }
    }
    spellings
}

/// A field's values as the file's MP4 atoms hold them.
fn mp4_read(atoms: &[Mp4AtomTruth], field: TagField) -> FieldRead {
    let items_of = |code: [u8; 4]| -> Vec<&Mp4Item> {
        atoms
            .iter()
            .filter(|atom| atom.kind == code)
            .flat_map(|atom| atom.items.iter())
            .collect()
    };
    let Some(target) = field.mp4() else {
        return FieldRead::Absent;
    };
    match target {
        Mp4Target::Text(code) => {
            let mut values = Vec::new();
            for item in items_of(code) {
                match item {
                    Mp4Item::Text(text) => values.push(text.clone()),
                    _ => return FieldRead::Opaque,
                }
            }
            values_or_absent(values)
        }
        Mp4Target::Freeform(name) => {
            let mut values = Vec::new();
            for item in atoms.iter().flat_map(|atom| atom.items.iter()) {
                if let Mp4Item::Freeform {
                    mean,
                    name: existing,
                    data,
                } = item
                    && mean == MP4_MEAN
                    && existing.eq_ignore_ascii_case(name)
                {
                    match std::str::from_utf8(data) {
                        Ok(text) => values.push(text.to_owned()),
                        Err(_) => return FieldRead::Opaque,
                    }
                }
            }
            values_or_absent(values)
        }
        Mp4Target::Pair(code, slot) => match mp4_pair(atoms, code) {
            None => FieldRead::Absent,
            Some((number, total)) => {
                let value = match slot {
                    Slot::Number => number,
                    Slot::Total => total,
                };
                if value > 0 {
                    FieldRead::Values(vec![value.to_string()])
                } else {
                    FieldRead::Absent
                }
            }
        },
        Mp4Target::Flag(code) => match items_of(code).as_slice() {
            [] => FieldRead::Absent,
            [Mp4Item::Bool(value)] => {
                FieldRead::Values(vec![if *value { "1" } else { "0" }.to_owned()])
            }
            _ => FieldRead::Opaque,
        },
        Mp4Target::Integer(code) => match items_of(code).as_slice() {
            [] => FieldRead::Absent,
            [Mp4Item::Integer(value)] if i32::try_from(*value).is_ok() => {
                FieldRead::Values(vec![value.to_string()])
            }
            _ => FieldRead::Opaque,
        },
    }
}

/// UFID payload: owner, NUL, identifier bytes.
fn split_ufid(payload: &[u8]) -> (&str, &[u8]) {
    match payload.iter().position(|byte| *byte == 0) {
        Some(end) => (
            std::str::from_utf8(&payload[..end]).unwrap_or(""),
            &payload[end + 1..],
        ),
        None => ("", payload),
    }
}

// ---------------------------------------------------------------------------
// Verify: untouched stays byte-identical, edits land exactly.
// ---------------------------------------------------------------------------

/// A frame's identity for comparison: `TXXX:<description>`,
/// `UFID:<owner>`, or the frame id.
fn frame_key(frame: &Id3FrameTruth) -> String {
    if let Some(Id3TextTruth {
        desc: Some(desc), ..
    }) = &frame.text
    {
        return format!("TXXX:{desc}");
    }
    if frame.id == "UFID" {
        return format!("UFID:{}", split_ufid(&frame.payload).0);
    }
    frame.id.clone()
}

fn verify_id3(
    had_tag: bool,
    before: Option<&DeepId3>,
    after: Option<&DeepId3>,
    edits: &[NativeEdit],
    pictures_edited: bool,
) -> Result<(), Refusal> {
    let mut edited: Vec<String> = edits.iter().filter_map(NativeEdit::id3_key).collect();
    // Picture bytes are checked on their own.
    if pictures_edited {
        edited.push("APIC".to_owned());
    }
    let removal_only = !pictures_edited
        && edits.iter().all(|edit| match edit {
            NativeEdit::Id3Text { values, .. } | NativeEdit::Id3User { values, .. } => {
                values.is_empty()
            }
            NativeEdit::Id3Ufid { value, .. } => value.is_none(),
            NativeEdit::Id3Lyrics { text } => text.is_none(),
            NativeEdit::Id3Raw { payloads, .. } => payloads.is_empty(),
            _ => true,
        });
    match (had_tag, before, after) {
        (false, _, None) if removal_only => return Ok(()),
        (false, _, after) => {
            // A brand-new tag: every frame present must be an edited one
            // carrying its requested values.
            let after = after.ok_or_else(|| Refusal::VerifyMismatch {
                detail: "edited tag missing after save".to_owned(),
            })?;
            for frame in &after.frames {
                let key = frame_key(frame);
                if !edited.contains(&key) {
                    return mismatch(format!("unexpected new frame {key}"));
                }
            }
        }
        (true, Some(before), Some(after)) => {
            if before.major != after.major {
                return mismatch(format!(
                    "ID3 version moved 2.{} -> 2.{}",
                    before.major, after.major
                ));
            }
            let strip = |mut map: BTreeMap<String, Vec<Vec<u8>>>| {
                for key in &edited {
                    map.remove(key);
                }
                map
            };
            let mut before_ids = key_multiset(&before.frames);
            let mut after_ids = key_multiset(&after.frames);
            for key in &edited {
                before_ids.remove(key);
                after_ids.remove(key);
            }
            if before_ids != after_ids {
                return mismatch(format!("frames changed outside {edited:?}"));
            }
            let mut before_text = text_map(&before.frames);
            let mut after_text = text_map(&after.frames);
            for key in &edited {
                before_text.remove(key);
                after_text.remove(key);
            }
            if before_text != after_text {
                return mismatch("untouched text changed".to_owned());
            }
            // Every other payload byte-identical.
            let (before_raw, after_raw) = (
                strip(raw_map(&before.frames)),
                strip(raw_map(&after.frames)),
            );
            if before_raw != after_raw {
                let changed: Vec<&String> = before_raw
                    .iter()
                    .filter(|(key, payloads)| after_raw.get(*key) != Some(*payloads))
                    .map(|(key, _)| key)
                    .collect();
                return mismatch(format!("untouched frame payload changed: {changed:?}"));
            }
        }
        (true, _, None) if removal_only => return Ok(()),
        (true, _, _) => {
            return mismatch("tag appeared or vanished under the save".to_owned());
        }
    }
    let frames: &[Id3FrameTruth] = after.map_or(&[], |after| after.frames.as_slice());
    for edit in edits {
        let Some(key) = edit.id3_key() else {
            continue;
        };
        let landed: Vec<&Id3FrameTruth> = frames
            .iter()
            .filter(|frame| frame_key(frame) == key)
            .collect();
        let ok = match edit {
            NativeEdit::Id3Text { values, .. } | NativeEdit::Id3User { values, .. } => {
                if values.is_empty() {
                    landed.is_empty()
                } else {
                    let seen: Vec<&Vec<String>> = landed
                        .iter()
                        .filter_map(|frame| frame.text.as_ref().map(|text| &text.values))
                        .collect();
                    seen.as_slice() == [values]
                }
            }
            NativeEdit::Id3Ufid { owner, value } => match value {
                None => landed.is_empty(),
                Some(value) => {
                    let mut payload = owner.as_bytes().to_vec();
                    payload.push(0);
                    payload.extend_from_slice(value.as_bytes());
                    landed.len() == 1 && landed[0].payload == payload
                }
            },
            NativeEdit::Id3Lyrics { text } => match text {
                None => landed.is_empty(),
                Some(text) => {
                    landed.len() == 1
                        && decode_uslt(&landed[0].payload).ok().as_deref() == Some(text.as_str())
                }
            },
            NativeEdit::Id3Raw { payloads, .. } => {
                // Every frame of the id, byte for byte, the ones the edit
                // left alone included.
                let mut want: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
                let mut got: Vec<&[u8]> = landed
                    .iter()
                    .map(|frame| frame.payload.as_slice())
                    .collect();
                want.sort();
                got.sort();
                want == got
            }
            _ => true,
        };
        if !ok {
            return mismatch(format!("edited frame '{key}' did not land"));
        }
    }
    Ok(())
}

fn text_map(frames: &[Id3FrameTruth]) -> BTreeMap<String, Vec<String>> {
    // The deep parse refuses duplicate text keys, so each key is unique.
    frames
        .iter()
        .filter_map(|frame| {
            let text = frame.text.as_ref()?;
            Some((frame_key(frame), text.values.clone()))
        })
        .collect()
}

fn raw_map(frames: &[Id3FrameTruth]) -> BTreeMap<String, Vec<Vec<u8>>> {
    let mut map: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for frame in frames {
        if frame.text.is_some() {
            continue;
        }
        map.entry(frame_key(frame))
            .or_default()
            .push(frame.payload.clone());
    }
    for payloads in map.values_mut() {
        payloads.sort();
    }
    map
}

fn key_multiset(frames: &[Id3FrameTruth]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for frame in frames {
        *counts.entry(frame_key(frame)).or_insert(0) += 1;
    }
    counts
}

fn verify_vorbis(
    had_tag: bool,
    before: Option<&VorbisTruth>,
    after: Option<&VorbisTruth>,
    edits: &[NativeEdit],
) -> Result<(), Refusal> {
    // Keys compare without case, as the pairs are grouped.
    let expected: Vec<(String, &Vec<String>)> = edits
        .iter()
        .filter_map(|edit| match edit {
            NativeEdit::Vorbis { key, values } => Some((key.to_ascii_uppercase(), values)),
            _ => None,
        })
        .collect();
    let edited_keys: Vec<&str> = expected.iter().map(|(key, _)| key.as_str()).collect();
    match (had_tag, before, after) {
        (false, _, None) if expected.iter().all(|(_, values)| values.is_empty()) => {
            return Ok(());
        }
        (false, _, after) => {
            let after = after.ok_or_else(|| Refusal::VerifyMismatch {
                detail: "edited tag missing after save".to_owned(),
            })?;
            for (key, _) in &after.pairs {
                if is_picture_field(key) {
                    continue;
                }
                if !edited_keys.contains(&key.to_ascii_uppercase().as_str()) {
                    return mismatch(format!("unexpected new field '{key}'"));
                }
            }
        }
        (true, Some(before), Some(after)) => {
            if before.vendor != after.vendor {
                return mismatch("vendor string changed".to_owned());
            }
            let rest_before = grouped_pairs(&before.pairs, &edited_keys);
            let rest_after = grouped_pairs(&after.pairs, &edited_keys);
            if rest_before != rest_after {
                let mut changed: Vec<&String> = rest_before
                    .keys()
                    .chain(rest_after.keys())
                    .filter(|key| rest_before.get(*key) != rest_after.get(*key))
                    .collect();
                changed.sort();
                changed.dedup();
                return mismatch(format!("untouched fields changed: {changed:?}"));
            }
        }
        (true, _, _) => {
            return mismatch("tag appeared or vanished under the save".to_owned());
        }
    }
    let grouped = after
        .map(|after| grouped_pairs_all(&after.pairs))
        .unwrap_or_default();
    for (key, values) in &expected {
        let landed = match grouped.get(key) {
            Some(seen) => seen == *values,
            None => values.is_empty(),
        };
        if !landed {
            return mismatch(format!("edited field '{key}' did not land"));
        }
    }
    Ok(())
}

/// Fields grouped by upper-cased key (order-sensitive within a key,
/// order-insensitive across keys), minus picture fields and edited keys.
fn grouped_pairs(
    pairs: &[(String, String)],
    edited_keys: &[&str],
) -> BTreeMap<String, Vec<String>> {
    let mut grouped = grouped_pairs_all(pairs);
    for key in edited_keys {
        grouped.remove(*key);
    }
    grouped
}

fn grouped_pairs_all(pairs: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, value) in pairs {
        if is_picture_field(key) {
            continue;
        }
        grouped
            .entry(key.to_ascii_uppercase())
            .or_default()
            .push(value.clone());
    }
    grouped
}

fn verify_mp4(
    before: Option<&[Mp4AtomTruth]>,
    after: Option<&[Mp4AtomTruth]>,
    edits: &[NativeEdit],
    pictures_edited: bool,
) -> Result<(), Refusal> {
    let mut edited_codes: Vec<[u8; 4]> = edits
        .iter()
        .filter_map(|edit| match edit {
            NativeEdit::Mp4Text { code, .. }
            | NativeEdit::Mp4Pair { code, .. }
            | NativeEdit::Mp4Flag { code, .. }
            | NativeEdit::Mp4Integer { code, .. } => Some(*code),
            _ => None,
        })
        .collect();
    // Picture bytes are checked on their own.
    if pictures_edited {
        edited_codes.push(*b"covr");
    }
    let edited_names: Vec<&str> = edits
        .iter()
        .filter_map(|edit| match edit {
            NativeEdit::Mp4Freeform { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let untouched = |atoms: &[Mp4AtomTruth]| {
        let mut items = atom_items(atoms);
        for code in &edited_codes {
            items.remove(code);
        }
        if let Some(freeforms) = items.get_mut(b"----") {
            freeforms.retain(|(item, _)| {
                !matches!(item, Mp4Item::Freeform { mean, name, .. }
                    if mean == MP4_MEAN && edited_names.contains(&name.as_str()))
            });
            if freeforms.is_empty() {
                items.remove(b"----");
            }
        }
        items
    };
    if let Some(before) = before {
        let after = after.ok_or_else(|| Refusal::VerifyMismatch {
            detail: "edited tag missing after save".to_owned(),
        })?;
        let before_items = untouched(before);
        let after_items = untouched(after);
        if before_items != after_items {
            let mut kinds: Vec<[u8; 4]> = before_items
                .keys()
                .chain(after_items.keys())
                .copied()
                .collect();
            kinds.sort();
            kinds.dedup();
            let diffs: Vec<String> = kinds
                .iter()
                .filter(|kind| before_items.get(*kind) != after_items.get(*kind))
                .map(|kind| {
                    describe_atom_diff(
                        kind,
                        before_items.get(kind).map(Vec::as_slice).unwrap_or(&[]),
                        after_items.get(kind).map(Vec::as_slice).unwrap_or(&[]),
                    )
                })
                .collect();
            return mismatch(format!("untouched atoms changed: {}", diffs.join(", ")));
        }
    }
    let atoms: &[Mp4AtomTruth] = after.unwrap_or(&[]);
    for edit in edits {
        let (label, landed) = match edit {
            NativeEdit::Mp4Text { code, values } => {
                let seen: Vec<String> = atoms
                    .iter()
                    .filter(|atom| atom.kind == *code)
                    .flat_map(|atom| atom.items.iter())
                    .filter_map(|item| match item {
                        Mp4Item::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
                (String::from_utf8_lossy(code).into_owned(), seen == *values)
            }
            NativeEdit::Mp4Freeform { name, values } => {
                let seen: Vec<String> = atoms
                    .iter()
                    .flat_map(|atom| atom.items.iter())
                    .filter_map(|item| match item {
                        Mp4Item::Freeform {
                            mean,
                            name: landed,
                            data,
                        } if mean == MP4_MEAN && landed == name => {
                            Some(String::from_utf8_lossy(data).into_owned())
                        }
                        _ => None,
                    })
                    .collect();
                (name.clone(), seen == *values)
            }
            NativeEdit::Mp4Pair {
                code,
                number,
                total,
            } => {
                let seen = mp4_pair(atoms, *code);
                let landed = if *number == 0 && *total == 0 {
                    seen.is_none()
                } else {
                    seen == Some((*number, *total))
                };
                (String::from_utf8_lossy(code).into_owned(), landed)
            }
            NativeEdit::Mp4Flag { code, value } => {
                let seen: Vec<&Mp4Item> = atoms
                    .iter()
                    .filter(|atom| atom.kind == *code)
                    .flat_map(|atom| atom.items.iter())
                    .collect();
                let landed = match value {
                    None => seen.is_empty(),
                    Some(value) => seen.as_slice() == [&Mp4Item::Bool(*value)],
                };
                (String::from_utf8_lossy(code).into_owned(), landed)
            }
            NativeEdit::Mp4Integer { code, value } => {
                let seen: Vec<&Mp4Item> = atoms
                    .iter()
                    .filter(|atom| atom.kind == *code)
                    .flat_map(|atom| atom.items.iter())
                    .collect();
                let landed = match value {
                    None => seen.is_empty(),
                    Some(value) => {
                        seen.as_slice() == [&Mp4Item::Integer(u64::from(value.unsigned_abs()))]
                    }
                };
                (String::from_utf8_lossy(code).into_owned(), landed)
            }
            _ => continue,
        };
        if !landed {
            return mismatch(format!("edited atom '{label}' did not land"));
        }
    }
    Ok(())
}

fn describe_atom_diff(kind: &[u8; 4], before: &[TypedItem], after: &[TypedItem]) -> String {
    let label = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        kind[0], kind[1], kind[2], kind[3]
    );
    if *kind != *b"----" {
        return format!("{label} {}->{} items", before.len(), after.len());
    }
    let grouped = |items: &[TypedItem]| {
        let mut map: BTreeMap<(String, String), Vec<Vec<u8>>> = BTreeMap::new();
        for (item, _) in items {
            if let Mp4Item::Freeform { mean, name, data } = item {
                map.entry((mean.clone(), name.clone()))
                    .or_default()
                    .push(data.clone());
            }
        }
        map
    };
    let short = |datas: Option<&Vec<Vec<u8>>>| -> String {
        datas.map_or_else(
            || "-".to_owned(),
            |vec| {
                vec.iter()
                    .map(|data| {
                        let text = String::from_utf8_lossy(data);
                        let mut short: String = text.chars().take(24).collect();
                        if text.len() > 24 {
                            short.push('~');
                        }
                        format!("{}B:{short}", data.len())
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            },
        )
    };
    let (before_map, after_map) = (grouped(before), grouped(after));
    let mut names: Vec<&(String, String)> = before_map.keys().chain(after_map.keys()).collect();
    names.sort();
    names.dedup();
    let diffs: Vec<String> = names
        .iter()
        .filter(|key| before_map.get(*key) != after_map.get(*key))
        .map(|key| {
            format!(
                "{}:{} {}->{}",
                key.0,
                key.1,
                short(before_map.get(*key)),
                short(after_map.get(*key))
            )
        })
        .collect();
    format!("{label} [{}]", diffs.join("; "))
}

/// An item by meaning, with its data type.
type TypedItem = (Mp4Item, Option<u32>);

/// Semantic items and their data types per atom kind, flattened across
/// same-kind atoms in file order (lofty splits multi-data atoms on save).
/// Freeforms sort stably by (mean, name): order across names is
/// meaningless, order within a name is a multi-value order and stays put.
fn atom_items(atoms: &[Mp4AtomTruth]) -> BTreeMap<[u8; 4], Vec<TypedItem>> {
    let mut map: BTreeMap<[u8; 4], Vec<TypedItem>> = BTreeMap::new();
    for atom in atoms {
        map.entry(atom.kind)
            .or_default()
            .extend(atom.items.iter().cloned().zip(atom.types.iter().copied()));
    }
    if let Some(freeforms) = map.get_mut(b"----") {
        freeforms.sort_by(|(a, _), (b, _)| match (a, b) {
            (
                Mp4Item::Freeform {
                    mean: am, name: an, ..
                },
                Mp4Item::Freeform {
                    mean: bm, name: bn, ..
                },
            ) => (am, an).cmp(&(bm, bn)),
            _ => std::cmp::Ordering::Equal,
        });
    }
    map
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

/// Put every UTF-8 atom and freeform back as the file held it: the
/// generic round-trip can reorder an item's values (two genres come back
/// swapped once the tag grows). Items of any other data type, or that
/// hold anything else, stay as lofty has them.
fn repair_mp4_text(native: &mut Ilst, atoms: &[Mp4AtomTruth]) {
    let mut texts: Vec<(AtomIdent<'static>, Vec<String>)> = Vec::new();
    let mut skipped: Vec<AtomIdent<'static>> = Vec::new();
    for atom in atoms {
        let utf8 = |index: usize| atom.types.get(index).copied().flatten() == Some(1);
        let found: Vec<(AtomIdent<'static>, Option<String>)> = if atom.kind == *b"----" {
            atom.items
                .iter()
                .enumerate()
                .map(|(index, item)| match item {
                    Mp4Item::Freeform { mean, name, data } => (
                        AtomIdent::Freeform {
                            mean: std::borrow::Cow::Owned(mean.clone()),
                            name: std::borrow::Cow::Owned(name.clone()),
                        },
                        std::str::from_utf8(data)
                            .ok()
                            .filter(|text| {
                                utf8(index)
                                    && !text.is_empty()
                                    && !mean.is_empty()
                                    && !name.is_empty()
                            })
                            .map(str::to_owned),
                    ),
                    _ => (AtomIdent::Fourcc(atom.kind), None),
                })
                .collect()
        } else {
            atom.items
                .iter()
                .enumerate()
                .map(|(index, item)| match item {
                    Mp4Item::Text(text) if utf8(index) => {
                        (AtomIdent::Fourcc(atom.kind), Some(text.clone()))
                    }
                    _ => (AtomIdent::Fourcc(atom.kind), None),
                })
                .collect()
        };
        for (ident, text) in found {
            match text {
                Some(text) => match texts.iter_mut().find(|(known, _)| *known == ident) {
                    Some((_, values)) => values.push(text),
                    None => texts.push((ident, vec![text])),
                },
                None => skipped.push(ident),
            }
        }
    }
    // An item holding anything that is not text stays as lofty has it.
    texts.retain(|(ident, _)| !skipped.contains(ident));
    for (ident, values) in texts {
        replace_mp4(native, ident, &values);
    }
}

/// Put the `ilst` items back in the file's order: lofty's remove swaps
/// the items left behind, and a replaced item goes to the end. Items the
/// file did not hold follow, in the order they were added.
fn keep_mp4_order(native: &mut Ilst, atoms: &[Mp4AtomTruth]) {
    let mut order: Vec<AtomIdent<'static>> = Vec::new();
    for atom in atoms {
        if atom.kind == *b"----" {
            for item in &atom.items {
                if let Mp4Item::Freeform { mean, name, .. } = item {
                    order.push(AtomIdent::Freeform {
                        mean: std::borrow::Cow::Owned(mean.clone()),
                        name: std::borrow::Cow::Owned(name.clone()),
                    });
                }
            }
        } else {
            order.push(AtomIdent::Fourcc(atom.kind));
        }
    }
    let mut held: Vec<Atom<'static>> = std::mem::take(native).into_iter().collect();
    held.sort_by_key(|atom| {
        order
            .iter()
            .position(|ident| ident == atom.ident())
            .unwrap_or(order.len())
    });
    for atom in held {
        native.insert(atom);
    }
}

/// Swap one `ilst` item's values; no values removes it.
fn replace_mp4(native: &mut Ilst, ident: AtomIdent<'static>, values: &[String]) {
    let _ = native.remove(&ident).count();
    let mut values = values.iter();
    let Some(first) = values.next() else {
        return;
    };
    let mut atom = Atom::new(ident, AtomData::UTF8(first.clone()));
    for value in values {
        atom.push_data(AtomData::UTF8(value.clone()));
    }
    native.insert(atom);
}

/// Timestamp ids that alias across ID3 versions: the merge emits v2.4
/// ids that the writer converts on v2.3 saves, so each family is cleared
/// as a unit before truth goes back.
fn timestamp_family(id: &str) -> Option<&'static [&'static str]> {
    match id {
        "TDRC" | "TYER" | "TDAT" | "TIME" => Some(&["TDRC", "TYER", "TDAT", "TIME"]),
        "TDOR" | "TORY" => Some(&["TDOR", "TORY"]),
        "TDRL" => Some(&["TDRL"]),
        "TDEN" => Some(&["TDEN"]),
        "TDTG" => Some(&["TDTG"]),
        _ => None,
    }
}

/// Rebuild every text/TXXX frame from byte truth, undoing the generic
/// round-trip's mangling (canonicalized descriptions, first-value
/// truncation, the invalid `WORK` frame). Timestamps go back verbatim as
/// text under their original ids, which is byte-faithful in both versions.
fn repair_id3_text(native: &mut Id3v2Tag, truth: Option<&DeepId3>) {
    let Some(truth) = truth else {
        return;
    };
    // Sweep merge-created junk first: the generic round-trip emits
    // frames no byte inventory contains (notably the invalid `WORK`
    // frame from lofty-rs#732), which would otherwise abort the save.
    // Anything not in truth is junk by definition.
    let truth_ids: std::collections::HashSet<&str> =
        truth.frames.iter().map(|frame| frame.id.as_str()).collect();
    let junk: Vec<String> = native
        .iter()
        .map(|frame| frame.id().as_str().to_owned())
        .filter(|id| !truth_ids.contains(id.as_str()))
        .collect();
    let mut junk_sorted = junk.clone();
    junk_sorted.sort();
    junk_sorted.dedup();
    for id in &junk_sorted {
        let _ = native.remove(&FrameId::Valid(std::borrow::Cow::Borrowed(id.as_str())));
    }
    let mut families_cleared: Vec<&[&str]> = Vec::new();
    for frame in &truth.frames {
        if frame.text.is_none() || frame.text.as_ref().is_some_and(|text| text.desc.is_some()) {
            continue;
        }
        if let Some(family) = timestamp_family(&frame.id)
            && !families_cleared.contains(&family)
        {
            for id in family {
                let _ = native.remove(&FrameId::Valid(std::borrow::Cow::Borrowed(id)));
            }
            families_cleared.push(family);
        }
    }
    let mut txxx_seen = false;
    for frame in &truth.frames {
        let Some(text) = &frame.text else {
            continue;
        };
        if let Some(desc) = &text.desc {
            if !txxx_seen {
                let _ = native.remove(&FrameId::Valid(std::borrow::Cow::Borrowed("TXXX")));
                txxx_seen = true;
            }
            native.insert(Frame::UserText(ExtendedTextFrame::new(
                TextEncoding::UTF8,
                desc.clone(),
                text.values.join("\0"),
            )));
        } else {
            let _ = native.remove(&FrameId::Valid(std::borrow::Cow::Owned(frame.id.clone())));
            native.insert(Frame::Text(TextInformationFrame::new(
                FrameId::Valid(std::borrow::Cow::Owned(frame.id.clone())),
                TextEncoding::UTF8,
                text.values.join("\0"),
            )));
        }
    }
    // Every other frame but `UFID` goes back as the bytes it was. The
    // generic round-trip in lofty 0.25.4 changes many of them (seen
    // 2026-10-15 on v2.3 and v2.4 tags): pictures, lyrics and comments
    // are re-encoded, a `WXXX` URL is written as UTF-16, a `POPM` gains
    // a play counter and has its rating rounded to stars (an empty email
    // even becomes "MusicBee"), `ETCO` and `OWNE` cannot be written at
    // all, and `IPLS` is read as a `TIPL` the sweep above removes. `UFID`
    // survives, and its edits find it by owner. An edit of any of these
    // removes them by id after this.
    let mut raw_ids: Vec<&str> = truth
        .frames
        .iter()
        .filter(|frame| frame.text.is_none() && frame.id != "UFID")
        .map(|frame| frame.id.as_str())
        .collect();
    raw_ids.sort_unstable();
    raw_ids.dedup();
    for id in raw_ids {
        let frame_id = FrameId::Valid(std::borrow::Cow::Owned(id.to_owned()));
        let _ = native.remove(&frame_id).count();
        for frame in truth.frames.iter().filter(|frame| frame.id == id) {
            native.insert(Frame::Binary(lofty::id3::v2::BinaryFrame::new(
                frame_id.clone(),
                frame.payload.clone(),
            )));
        }
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
