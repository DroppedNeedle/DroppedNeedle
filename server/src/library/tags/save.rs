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
//! credits, track and disc numbers with totals, dates, release facts, and
//! the MusicBrainz ids, each under Picard's native name for the format.
//! An edit with no values removes the field.
//!
//! Preserved across one unrelated title edit: `TXXX:WORK`, unknown ID3
//! frames, multi-valued tags, `TXXX` description case, unknown Vorbis
//! fields, `TOTALTRACKS`/`TOTALDISCS` spelling, the vendor string, MP4
//! freeforms, pictures, `CUSTOM_KEEP` everywhere, and byte-identical audio.
//! Refused: mixed v2.3 tags, present empty Vorbis values, values a field
//! cannot hold (see [`Refusal`]), unparseable tag bytes, and any
//! post-save delta outside the edit.
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
    UniqueFileIdentifierFrame,
};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::items::Timestamp;
use lofty::tag::{Accessor as _, TagExt as _, TagType};
use thiserror::Error;

use super::fields::{FieldKind, Id3Target, MP4_MEAN, Mp4Target, Slot, TagField};
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
}

impl TagEdit {
    #[must_use]
    pub fn new(field: TagField, values: Vec<String>) -> Self {
        Self {
            field,
            values,
            verbatim: false,
            spelling: None,
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
    #[error("mixed ID3v2.3 tag: {detail}")]
    MixedId3v23 { detail: String },
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
    let format = format_for_path(path)?;
    if let Some(refusal) = read_only(format) {
        return Err(refused(path, refusal));
    }
    if edits.is_empty() {
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
    let snapshot = Snapshot::capture(format, &original_bytes, &tagged)
        .map_err(|refusal| refused(path, refusal))?;
    snapshot
        .check_file_state()
        .map_err(|refusal| refused(path, refusal))?;
    for edit in edits {
        check_edit(edit).map_err(|refusal| refused(path, refusal))?;
    }
    let native = resolve_edits(&snapshot, edits).map_err(|refusal| refused(path, refusal))?;

    let temp = TempCopy::create(path)?;
    apply_edits(format, &snapshot, &tagged, &native, temp.path())
        .map_err(|refusal| refused(temp.path(), refusal))?;
    let temp_bytes = fs::read(temp.path()).map_err(|source| TagsError::Io {
        path: temp.path().display().to_string(),
        source,
    })?;
    let temp_tagged = lofty::read_from_path(temp.path()).map_err(|error| TagsError::TagRead {
        path: temp.path().display().to_string(),
        reason: error.to_string(),
    })?;
    let after = Snapshot::capture(format, &temp_bytes, &temp_tagged)
        .map_err(|refusal| refused(temp.path(), refusal))?;
    snapshot
        .verify_against(&after, &native)
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
            Some(tag) => TagField::ALL
                .into_iter()
                .map(|field| (field, id3_read(&tag, field)))
                .collect(),
            None => Vec::new(),
        },
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => {
            match parse_vorbis_truth(format, &bytes).map_err(unreadable)? {
                Some(truth) => {
                    document.spellings = vorbis_spellings(&truth);
                    TagField::ALL
                        .into_iter()
                        .map(|field| (field, vorbis_read(&truth, field)))
                        .collect()
                }
                None => Vec::new(),
            }
        }
        AudioFormat::M4a => match parse_mp4_atoms(&bytes).map_err(unreadable)? {
            Some(atoms) => TagField::ALL
                .into_iter()
                .map(|field| (field, mp4_read(&atoms, field)))
                .collect(),
            None => Vec::new(),
        },
        AudioFormat::Aac | AudioFormat::Wav => Vec::new(),
    };
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
    Ok(document)
}

/// The writable fields a file carries as text (see [`read_document`]).
pub fn read_fields(path: &Path) -> Result<BTreeMap<TagField, Vec<String>>, TagsError> {
    Ok(read_document(path)?.values)
}

// ---------------------------------------------------------------------------
// Pre-save scan: values each field can hold.
// ---------------------------------------------------------------------------

/// Frame ids that only exist in ID3v2.4. Any of them inside a v2.3 tag
/// means a mixed tag lofty would silently rewrite. `TSO2` is not on the
/// list: Picard writes it in v2.3 tags too and lofty's v2.3 writer keeps
/// it. lofty drops `TSOA`, `TSOP`, `TSOT`, and `TSST` from v2.3 tags, so
/// a v2.3 file carrying them is refused before anything is written.
const V24_ONLY_IDS: &[&str] = &[
    "ASPI", "EQU2", "POSS", "RVA2", "SEEK", "SIGN", "TDOR", "TDRC", "TDRL", "TIPL", "TMCL", "TMOO",
    "TPRO", "TRSN", "TSOA", "TSOP", "TSOT", "TSST",
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
struct Id3FrameTruth {
    id: String,
    payload: Vec<u8>,
    text: Option<Id3TextTruth>,
}

struct Id3TextTruth {
    /// Description for `TXXX`, `None` for plain text frames.
    desc: Option<String>,
    values: Vec<String>,
}

struct DeepId3 {
    major: u8,
    minor: u8,
    frames: Vec<Id3FrameTruth>,
}

/// Key-value list ids, which are not text despite the `T` prefix.
fn is_key_value_id(id: &str) -> bool {
    matches!(id, "TIPL" | "TMCL")
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
        } else if id.starts_with('T') && !is_key_value_id(&id) {
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
    for part in parts {
        // A trailing terminator yields a trailing empty part; drop those,
        // keep interior empties (they are real values).
        values.push(decode_id3_part(*encoding, part)?);
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

fn decode_id3_part(encoding: u8, part: &[u8]) -> Result<String, String> {
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
            let (units, swap) = if encoding == 1 {
                match units.first() {
                    Some(0xFEFF) => (&units[1..], false),
                    Some(0xFFFE) => (&units[1..], true),
                    // Missing BOM: assume big-endian like most readers.
                    _ => (units.as_slice(), false),
                }
            } else {
                (units.as_slice(), false)
            };
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
        atoms.push(Mp4AtomTruth {
            kind: kid.kind,
            items: mp4_child_items(bytes, kid),
        });
    }
    Ok(Some(atoms))
}

/// Data payloads of one `ilst` child, interpreted by kind.
fn mp4_child_items(bytes: &[u8], kid: &Mp4Box) -> Vec<Mp4Item> {
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
            .map(|(mean, name, grandkid)| Mp4Item::Freeform {
                mean: mean.unwrap_or_default(),
                name: name.unwrap_or_default(),
                data: bytes[grandkid.content + 8..grandkid.end].to_vec(),
            })
            .collect();
    }
    let mut items = Vec::new();
    for grandkid in grandkids {
        if grandkid.kind != *b"data" || grandkid.content + 8 > grandkid.end {
            continue;
        }
        let payload = &bytes[grandkid.content + 8..grandkid.end];
        items.push(mp4_data_item(kid.kind, payload));
    }
    if items.is_empty() {
        items.push(Mp4Item::Bytes(bytes[kid.start..kid.end].to_vec()));
    }
    items
}

fn mp4_data_item(kind: [u8; 4], payload: &[u8]) -> Mp4Item {
    if (kind == *b"trkn" || kind == *b"disk") && payload.len() >= 6 {
        return Mp4Item::Pair(
            u32::from(u16::from_be_bytes([payload[2], payload[3]])),
            u32::from(u16::from_be_bytes([payload[4], payload[5]])),
        );
    }
    if kind == *b"cpil" && payload.len() == 1 {
        return Mp4Item::Bool(payload[0] != 0);
    }
    if let Ok(text) = std::str::from_utf8(payload)
        && !text.is_empty()
        && text.chars().all(|c| !c.is_control())
    {
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
// Snapshots and verify.
// ---------------------------------------------------------------------------

enum Snapshot {
    Id3 {
        had_tag: bool,
        tag: Option<DeepId3>,
        pictures: Vec<Vec<u8>>,
    },
    Vorbis {
        had_tag: bool,
        truth: Option<VorbisTruth>,
        pictures: Vec<Vec<u8>>,
    },
    Mp4 {
        atoms: Option<Vec<Mp4AtomTruth>>,
        pictures: Vec<Vec<u8>>,
    },
}

impl Snapshot {
    fn capture(
        format: AudioFormat,
        file_bytes: &[u8],
        tagged: &lofty::file::TaggedFile,
    ) -> Result<Self, Refusal> {
        let pictures_of = |tag_type: TagType| -> Vec<Vec<u8>> {
            tagged.tag(tag_type).map_or_else(Vec::new, |tag| {
                tag.pictures()
                    .iter()
                    .map(|pic| pic.data().to_vec())
                    .collect()
            })
        };
        match format {
            AudioFormat::Mp3 => {
                let tag = parse_id3_deep(file_bytes)?;
                Ok(Snapshot::Id3 {
                    had_tag: tag.is_some(),
                    tag,
                    pictures: pictures_of(TagType::Id3v2),
                })
            }
            AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => {
                let truth = parse_vorbis_truth(format, file_bytes)?;
                Ok(Snapshot::Vorbis {
                    had_tag: truth.is_some(),
                    truth,
                    pictures: pictures_of(TagType::VorbisComments),
                })
            }
            AudioFormat::M4a => Ok(Snapshot::Mp4 {
                atoms: parse_mp4_atoms(file_bytes)?,
                pictures: pictures_of(TagType::Mp4Ilst),
            }),
            AudioFormat::Aac | AudioFormat::Wav => Err(Refusal::ReadOnlyFormat {
                format: format.as_str().to_owned(),
                reason: "read-only container".to_owned(),
            }),
        }
    }

    fn check_file_state(&self) -> Result<(), Refusal> {
        match self {
            Snapshot::Id3 { tag, .. } => {
                let Some(tag) = tag else {
                    return Ok(());
                };
                let report = Id3ByteReport {
                    version_major: tag.major,
                    version_minor: tag.minor,
                    frame_ids: tag.frames.iter().map(|frame| frame.id.clone()).collect(),
                    unsynchronised: false,
                    extended_header: false,
                    truncated: false,
                };
                if let Some(detail) = mixed_v23_detail(&report) {
                    return Err(Refusal::MixedId3v23 { detail });
                }
                Ok(())
            }
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
            Snapshot::Mp4 { .. } => Ok(()),
        }
    }

    fn verify_against(&self, after: &Snapshot, edits: &[NativeEdit]) -> Result<(), Refusal> {
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
                verify_id3(*had_tag, before.as_ref(), after.as_ref(), edits)?;
                if pictures != after_pictures {
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
                if pictures != after_pictures {
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
                verify_mp4(before.as_deref(), after.as_deref(), edits)?;
                if pictures != after_pictures {
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
}

impl NativeEdit {
    /// The frame key verification excludes from the untouched set.
    fn id3_key(&self) -> Option<String> {
        match self {
            NativeEdit::Id3Text { id, .. } => Some(id.clone()),
            NativeEdit::Id3User { desc, .. } => Some(format!("TXXX:{desc}")),
            NativeEdit::Id3Ufid { owner, .. } => Some(format!("UFID:{owner}")),
            _ => None,
        }
    }

    fn same_target(&self, other: &NativeEdit) -> bool {
        match (self, other) {
            (NativeEdit::Vorbis { key: a, .. }, NativeEdit::Vorbis { key: b, .. }) => a == b,
            (NativeEdit::Mp4Text { code: a, .. }, NativeEdit::Mp4Text { code: b, .. })
            | (NativeEdit::Mp4Pair { code: a, .. }, NativeEdit::Mp4Pair { code: b, .. })
            | (NativeEdit::Mp4Flag { code: a, .. }, NativeEdit::Mp4Flag { code: b, .. }) => a == b,
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
            for edit in edits.iter().filter(|edit| edit.spelling.is_none()) {
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
                }
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
                match edit.field.mp4() {
                    Mp4Target::Text(code) => push(NativeEdit::Mp4Text {
                        code,
                        values: edit.values.clone(),
                    }),
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
    let v23 = tag.major == 3;
    match field.id3(v23) {
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
    match field.mp4() {
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
) -> Result<(), Refusal> {
    let edited: Vec<String> = edits.iter().filter_map(NativeEdit::id3_key).collect();
    let removal_only = edits.iter().all(|edit| match edit {
        NativeEdit::Id3Text { values, .. } | NativeEdit::Id3User { values, .. } => {
            values.is_empty()
        }
        NativeEdit::Id3Ufid { value, .. } => value.is_none(),
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
            if strip(raw_map(&before.frames)) != strip(raw_map(&after.frames)) {
                return mismatch("untouched frame payload changed".to_owned());
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
    let expected: Vec<(&str, &Vec<String>)> = edits
        .iter()
        .filter_map(|edit| match edit {
            NativeEdit::Vorbis { key, values } => Some((key.as_str(), values)),
            _ => None,
        })
        .collect();
    let edited_keys: Vec<&str> = expected.iter().map(|(key, _)| *key).collect();
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
        let landed = match grouped.get(*key) {
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
) -> Result<(), Refusal> {
    let edited_codes: Vec<[u8; 4]> = edits
        .iter()
        .filter_map(|edit| match edit {
            NativeEdit::Mp4Text { code, .. }
            | NativeEdit::Mp4Pair { code, .. }
            | NativeEdit::Mp4Flag { code, .. } => Some(*code),
            _ => None,
        })
        .collect();
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
            freeforms.retain(|item| {
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
            _ => continue,
        };
        if !landed {
            return mismatch(format!("edited atom '{label}' did not land"));
        }
    }
    Ok(())
}

fn describe_atom_diff(kind: &[u8; 4], before: &[Mp4Item], after: &[Mp4Item]) -> String {
    let label = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        kind[0], kind[1], kind[2], kind[3]
    );
    if *kind != *b"----" {
        return format!("{label} {}->{} items", before.len(), after.len());
    }
    let grouped = |items: &[Mp4Item]| {
        let mut map: BTreeMap<(String, String), Vec<Vec<u8>>> = BTreeMap::new();
        for item in items {
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

/// Semantic items per atom kind, flattened across same-kind atoms in
/// file order (lofty splits multi-data atoms on save). Freeforms sort
/// stably by (mean, name): order across names is meaningless, order
/// within a name is a multi-value order and stays put.
fn atom_items(atoms: &[Mp4AtomTruth]) -> BTreeMap<[u8; 4], Vec<Mp4Item>> {
    let mut map: BTreeMap<[u8; 4], Vec<Mp4Item>> = BTreeMap::new();
    for atom in atoms {
        map.entry(atom.kind)
            .or_default()
            .extend(atom.items.iter().cloned());
    }
    if let Some(freeforms) = map.get_mut(b"----") {
        freeforms.sort_by(|a, b| match (a, b) {
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
    temp: &Path,
) -> Result<(), Refusal> {
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
                    _ => {}
                }
            }
            let pin_v23 = truth.is_some_and(|tag| tag.major == 3)
                || matches!(native.original_version(), Id3v2Version::V3);
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
        let stem = original
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("audio");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|span| span.as_nanos())
            .unwrap_or(0);
        for _ in 0..100 {
            let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                ".{stem}.dn-tagtmp-{}-{nanos}-{count}.{extension}",
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
