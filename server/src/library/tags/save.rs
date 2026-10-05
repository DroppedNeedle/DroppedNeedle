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
//! Preserved across one unrelated title edit: `TXXX:WORK`, unknown ID3
//! frames, multi-valued tags, `TXXX` description case, unknown Vorbis
//! fields, `TOTALTRACKS`/`TOTALDISCS` spelling, the vendor string, MP4
//! freeforms, pictures, `CUSTOM_KEEP` everywhere, and byte-identical audio.
//! Refused: mixed v2.3 tags, present empty Vorbis values, unencodable
//! items (see [`Refusal`]), unparseable tag bytes, and any post-save delta
//! outside the edit.
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
};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::items::Timestamp;
use lofty::tag::{ItemKey, TagExt as _, TagType};
use thiserror::Error;

use super::{AudioFormat, TagsError, format_for_path};

/// One requested mutation: an [`ItemKey`] with its replacement values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagEdit {
    pub key: ItemKey,
    pub values: Vec<String>,
}

impl TagEdit {
    #[must_use]
    pub fn new(key: ItemKey, values: Vec<String>) -> Self {
        Self { key, values }
    }

    #[must_use]
    pub fn set_title(title: impl Into<String>) -> Self {
        Self {
            key: ItemKey::TrackTitle,
            values: vec![title.into()],
        }
    }
}

/// Why a save was refused. Every variant leaves the original untouched.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    #[error("refusing to write {format}: {reason}")]
    ReadOnlyFormat { format: String, reason: String },
    #[error("unencodable {key:?}: {reason}")]
    UnencodableItem { key: ItemKey, reason: String },
    #[error("empty Vorbis value present under '{key}'; lofty drops empties on save")]
    EmptyVorbisValue { key: String },
    #[error("mixed ID3v2.3 tag: {detail}")]
    MixedId3v23 { detail: String },
    #[error("edit key {key:?} is outside this slice's surface")]
    UnsupportedEdit { key: ItemKey },
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

/// Apply `edits` to `path` through the full wrapper pipeline.
pub fn save_tags(path: &Path, edits: &[TagEdit]) -> Result<SaveReport, TagsError> {
    let format = format_for_path(path)?;
    match format {
        AudioFormat::Aac => {
            return Err(refused(
                path,
                Refusal::ReadOnlyFormat {
                    format: format.as_str().to_owned(),
                    reason: "APE tags on ADTS never persist; read-only".to_owned(),
                },
            ));
        }
        AudioFormat::Wav => {
            return Err(refused(
                path,
                Refusal::ReadOnlyFormat {
                    format: format.as_str().to_owned(),
                    reason:
                        "WAV writes fork a second ID3 chunk and RIFF INFO is unwritten; read-only"
                            .to_owned(),
                },
            ));
        }
        AudioFormat::Flac
        | AudioFormat::Mp3
        | AudioFormat::Ogg
        | AudioFormat::Opus
        | AudioFormat::M4a => {}
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
    presave_scan(format, &snapshot, edits).map_err(|refusal| refused(path, refusal))?;

    let temp = TempCopy::create(path)?;
    apply_edits(format, &snapshot, &tagged, edits, temp.path())
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
        .verify_against(&after, edits)
        .map_err(|refusal| refused(path, refusal))?;
    temp.commit()?;
    Ok(SaveReport {
        format,
        id3_version: snapshot.id3_version_label(),
        edits_applied: edits.len(),
    })
}

// ---------------------------------------------------------------------------
// Pre-save scan: the measured unencodable catalog.
// ---------------------------------------------------------------------------

/// Frame ids that only exist in ID3v2.4. Any of them inside a v2.3 tag
/// means a mixed tag lofty would silently rewrite.
const V24_ONLY_IDS: &[&str] = &[
    "ASPI", "EQU2", "POSS", "RVA2", "SEEK", "SIGN", "TDOR", "TDRC", "TDRL", "TIPL", "TMCL", "TMOO",
    "TPRO", "TRSN", "TSO2", "TSOA", "TSOP", "TSOT", "TSST",
];

fn presave_scan(
    format: AudioFormat,
    snapshot: &Snapshot,
    edits: &[TagEdit],
) -> Result<(), Refusal> {
    snapshot.check_file_state()?;
    let id3_major = match snapshot {
        Snapshot::Id3 { tag, .. } => tag.as_ref().map(|tag| tag.major),
        _ => None,
    };
    for edit in edits {
        check_edit(format, id3_major, edit)?;
    }
    Ok(())
}

fn check_edit(format: AudioFormat, id3_major: Option<u8>, edit: &TagEdit) -> Result<(), Refusal> {
    if matches!(
        edit.key,
        ItemKey::TrackTitle
            | ItemKey::TrackArtist
            | ItemKey::AlbumTitle
            | ItemKey::AlbumArtist
            | ItemKey::Genre
    ) {
        if edit.values.iter().any(|value| value.is_empty()) {
            return Err(Refusal::UnencodableItem {
                key: edit.key,
                reason: "empty values are not expressible".to_owned(),
            });
        }
        // v2.3 joins multi-values with `/`, which does not round-trip
        // through a NUL-based verify; refuse rather than corrupt.
        if format == AudioFormat::Mp3 && id3_major == Some(3) && edit.values.len() > 1 {
            return Err(Refusal::UnencodableItem {
                key: edit.key,
                reason: "v2.3 cannot express multi-valued edits".to_owned(),
            });
        }
        return Ok(());
    }
    let is_id3 = format == AudioFormat::Mp3;
    match edit.key {
        // Upstream lofty-rs#732: `Work` maps to an invalid `WORK` frame and
        // aborts the whole save on ID3v2.
        ItemKey::Work if is_id3 => Err(Refusal::UnencodableItem {
            key: edit.key,
            reason: "Work maps to invalid frame WORK on ID3v2 (lofty-rs#732)".to_owned(),
        }),
        // Same class: PCST aborts on 0/1 and vanishes otherwise.
        ItemKey::FlagPodcast if is_id3 => Err(Refusal::UnencodableItem {
            key: edit.key,
            reason: "FlagPodcast aborts or vanishes on ID3v2".to_owned(),
        }),
        ItemKey::RecordingDate
        | ItemKey::ReleaseDate
        | ItemKey::OriginalReleaseDate
        | ItemKey::TaggingTime
        | ItemKey::EncodingTime => {
            for value in &edit.values {
                if value.parse::<Timestamp>().is_err() {
                    return Err(Refusal::UnencodableItem {
                        key: edit.key,
                        reason: format!("unparseable timestamp '{value}' would poison ID3v2 reads"),
                    });
                }
            }
            Err(Refusal::UnsupportedEdit { key: edit.key })
        }
        ItemKey::TrackNumber | ItemKey::TrackTotal | ItemKey::DiscNumber | ItemKey::DiscTotal => {
            for value in &edit.values {
                if !is_track_count(value) {
                    return Err(Refusal::UnencodableItem {
                        key: edit.key,
                        reason: format!("non-numeric count '{value}' would be dropped"),
                    });
                }
            }
            Err(Refusal::UnsupportedEdit { key: edit.key })
        }
        ItemKey::MovementNumber => {
            for value in &edit.values {
                if !is_track_count(value) {
                    return Err(Refusal::UnencodableItem {
                        key: edit.key,
                        reason: format!(
                            "movement '{value}' is not n or n/m and would read back wrong"
                        ),
                    });
                }
            }
            Err(Refusal::UnsupportedEdit { key: edit.key })
        }
        _ => Err(Refusal::UnsupportedEdit { key: edit.key }),
    }
}

fn is_track_count(value: &str) -> bool {
    let (head, tail) = match value.split_once('/') {
        Some((number, total)) => (number, Some(total)),
        None => (value, None),
    };
    if head.trim().parse::<u32>().is_err() {
        return false;
    }
    tail.is_none_or(|total| total.trim().parse::<u32>().is_ok())
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

    fn verify_against(&self, after: &Snapshot, edits: &[TagEdit]) -> Result<(), Refusal> {
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

/// The last requested values per edited native key.
fn expected_edits(
    edits: &[TagEdit],
    key_of: impl Fn(ItemKey) -> Option<String>,
) -> Vec<(String, Vec<String>)> {
    let mut expected: Vec<(String, Vec<String>)> = Vec::new();
    for edit in edits {
        if let Some(key) = key_of(edit.key) {
            if let Some(slot) = expected.iter_mut().find(|(slot_key, _)| slot_key == &key) {
                slot.1 = edit.values.clone();
            } else {
                expected.push((key, edit.values.clone()));
            }
        }
    }
    expected
}

fn verify_id3(
    had_tag: bool,
    before: Option<&DeepId3>,
    after: Option<&DeepId3>,
    edits: &[TagEdit],
) -> Result<(), Refusal> {
    let edited: Vec<String> = edits
        .iter()
        .filter_map(|edit| id3_frame_id(edit.key))
        .collect();
    match (had_tag, before, after) {
        (false, _, after) => {
            // A brand-new tag: every frame present must be an edited one
            // carrying its requested values.
            let after = after.ok_or_else(|| Refusal::VerifyMismatch {
                detail: "edited tag missing after save".to_owned(),
            })?;
            for frame in &after.frames {
                if !edited.contains(&frame.id) {
                    return mismatch(format!("unexpected new frame {}", frame.id));
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
            let mut before_ids = id_multiset(&before.frames);
            let mut after_ids = id_multiset(&after.frames);
            for id in &edited {
                before_ids.remove(id);
                after_ids.remove(id);
            }
            if before_ids != after_ids {
                return mismatch(format!("frame ids changed outside {edited:?}"));
            }
            // Text truth, compared per (id, description).
            let mut before_text = text_map(&before.frames);
            let mut after_text = text_map(&after.frames);
            for id in &edited {
                before_text.remove(id);
                after_text.remove(id);
            }
            if before_text != after_text {
                return mismatch("untouched text changed".to_owned());
            }
            // Every other payload byte-identical.
            let mut before_raw = raw_map(&before.frames);
            let mut after_raw = raw_map(&after.frames);
            for id in &edited {
                before_raw.remove(id);
                after_raw.remove(id);
            }
            if before_raw != after_raw {
                return mismatch("untouched frame payload changed".to_owned());
            }
        }
        (true, _, _) => {
            return mismatch("tag appeared or vanished under the save".to_owned());
        }
    }
    // Edited frames carry exactly the requested values.
    if let Some(after) = after {
        let expected = expected_edits(edits, id3_frame_id);
        for (id, values) in &expected {
            let seen: Vec<&Vec<String>> = after
                .frames
                .iter()
                .filter(|frame| &frame.id == id)
                .filter_map(|frame| frame.text.as_ref().map(|text| &text.values))
                .collect();
            if seen.as_slice() != [values] {
                return mismatch(format!("edited frame '{id}' did not land"));
            }
        }
    }
    Ok(())
}

fn text_map(frames: &[Id3FrameTruth]) -> BTreeMap<String, Vec<String>> {
    let mut map = BTreeMap::new();
    for frame in frames {
        if frame.text.is_none() {
            continue;
        }
        // The deep parse refuses duplicates, so each key is unique.
        let text = frame.text.as_ref().map(|text| text.values.clone());
        if let Some(values) = text {
            let key = match &frame.text {
                Some(Id3TextTruth {
                    desc: Some(desc), ..
                }) => format!("TXXX:{desc}"),
                _ => frame.id.clone(),
            };
            map.insert(key, values);
        }
    }
    map
}

fn raw_map(frames: &[Id3FrameTruth]) -> BTreeMap<String, Vec<Vec<u8>>> {
    let mut map: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for frame in frames {
        if frame.text.is_some() {
            continue;
        }
        map.entry(frame.id.clone())
            .or_default()
            .push(frame.payload.clone());
    }
    for payloads in map.values_mut() {
        payloads.sort();
    }
    map
}

fn id_multiset(frames: &[Id3FrameTruth]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for frame in frames {
        *counts.entry(frame.id.clone()).or_insert(0) += 1;
    }
    counts
}

fn verify_vorbis(
    had_tag: bool,
    before: Option<&VorbisTruth>,
    after: Option<&VorbisTruth>,
    edits: &[TagEdit],
) -> Result<(), Refusal> {
    let expected = expected_edits(edits, vorbis_entry_key);
    let edited_keys: Vec<&str> = expected.iter().map(|(key, _)| key.as_str()).collect();
    match (had_tag, before, after) {
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
                return mismatch("untouched fields changed".to_owned());
            }
        }
        (true, _, _) => {
            return mismatch("tag appeared or vanished under the save".to_owned());
        }
    }
    if let Some(after) = after {
        let grouped = grouped_pairs_all(&after.pairs);
        for (key, values) in &expected {
            if grouped.get(key) != Some(values) {
                return mismatch(format!("edited field '{key}' did not land"));
            }
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
    edits: &[TagEdit],
) -> Result<(), Refusal> {
    let edited: Vec<[u8; 4]> = edits
        .iter()
        .filter_map(|edit| mp4_fourcc(edit.key))
        .collect();
    if let Some(before) = before {
        let after = after.ok_or_else(|| Refusal::VerifyMismatch {
            detail: "edited tag missing after save".to_owned(),
        })?;
        let mut before_items = atom_items(before);
        let mut after_items = atom_items(after);
        for code in &edited {
            before_items.remove(code);
            after_items.remove(code);
        }
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
    if let Some(after) = after {
        let expected = expected_edits(edits, mp4_entry_key);
        for (key, values) in &expected {
            let Some(code) = mp4_entry_code(key) else {
                continue;
            };
            let seen: Vec<String> = after
                .iter()
                .filter(|atom| atom.kind == code)
                .flat_map(|atom| {
                    atom.items.iter().filter_map(|item| match item {
                        Mp4Item::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                })
                .collect();
            if seen != *values {
                return mismatch(format!("edited atom '{key}' did not land"));
            }
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
// Edit targets per format.
// ---------------------------------------------------------------------------

fn id3_frame_id(key: ItemKey) -> Option<String> {
    match key {
        ItemKey::TrackTitle => Some("TIT2".to_owned()),
        ItemKey::TrackArtist => Some("TPE1".to_owned()),
        ItemKey::AlbumTitle => Some("TALB".to_owned()),
        ItemKey::AlbumArtist => Some("TPE2".to_owned()),
        ItemKey::Genre => Some("TCON".to_owned()),
        _ => None,
    }
}

fn vorbis_entry_key(key: ItemKey) -> Option<String> {
    match key {
        ItemKey::TrackTitle => Some("TITLE".to_owned()),
        ItemKey::TrackArtist => Some("ARTIST".to_owned()),
        ItemKey::AlbumTitle => Some("ALBUM".to_owned()),
        ItemKey::AlbumArtist => Some("ALBUMARTIST".to_owned()),
        ItemKey::Genre => Some("GENRE".to_owned()),
        _ => None,
    }
}

fn mp4_fourcc(key: ItemKey) -> Option<[u8; 4]> {
    match key {
        ItemKey::TrackTitle => Some(*b"\xa9nam"),
        ItemKey::TrackArtist => Some(*b"\xa9ART"),
        ItemKey::AlbumTitle => Some(*b"\xa9alb"),
        ItemKey::AlbumArtist => Some(*b"aART"),
        ItemKey::Genre => Some(*b"\xa9gen"),
        _ => None,
    }
}

fn mp4_entry_key(key: ItemKey) -> Option<String> {
    mp4_fourcc(key).map(|code| {
        format!(
            "{:02x}{:02x}{:02x}{:02x}",
            code[0], code[1], code[2], code[3]
        )
    })
}

fn mp4_entry_code(key: &str) -> Option<[u8; 4]> {
    if key.len() != 8 {
        return None;
    }
    let mut code = [0u8; 4];
    for (index, cell) in code.iter_mut().enumerate() {
        *cell = u8::from_str_radix(&key[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(code)
}

// ---------------------------------------------------------------------------
// Native application on the temp copy, repaired from byte truth.
// ---------------------------------------------------------------------------

fn apply_edits(
    format: AudioFormat,
    snapshot: &Snapshot,
    tagged: &lofty::file::TaggedFile,
    edits: &[TagEdit],
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
                let Some(id) = id3_frame_id(edit.key) else {
                    continue;
                };
                let _ = native.remove(&FrameId::Valid(std::borrow::Cow::Borrowed(id3_static_id(
                    edit.key,
                ))));
                native.insert(Frame::Text(TextInformationFrame::new(
                    FrameId::Valid(std::borrow::Cow::Borrowed(id3_static_id(edit.key))),
                    TextEncoding::UTF8,
                    edit.values.join("\0"),
                )));
                let _ = id;
            }
            let pin_v23 = truth.is_some_and(|tag| tag.major == 3)
                || matches!(native.original_version(), Id3v2Version::V3);
            native
                .save_to_path(temp, WriteOptions::new().use_id3v23(pin_v23))
                .map_err(|error| Refusal::SaveFailed {
                    reason: error_chain(&error),
                })?;
            Ok(())
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
            // re-push the raw truth in file order so unknown fields that
            // the generic round-trip dropped come back.
            let keys: Vec<String> = native.items().map(|(key, _)| key.to_owned()).collect();
            for key in &keys {
                native.remove(key).for_each(drop);
            }
            if let Some(truth) = truth {
                for (key, value) in &truth.pairs {
                    if !is_picture_field(key) {
                        native.push(key.clone(), value.clone());
                    }
                }
                native.set_vendor(truth.vendor.clone());
            }
            for edit in edits {
                let Some(field) = vorbis_entry_key(edit.key) else {
                    continue;
                };
                native.remove(&field).for_each(drop);
                for value in &edit.values {
                    native.push(field.clone(), value.clone());
                }
            }
            native
                .save_to_path(temp, WriteOptions::new())
                .map_err(|error| Refusal::SaveFailed {
                    reason: error_chain(&error),
                })?;
            Ok(())
        }
        AudioFormat::M4a => {
            let mut native = tagged
                .tag(TagType::Mp4Ilst)
                .cloned()
                .map(Ilst::from)
                .unwrap_or_default();
            for edit in edits {
                let Some(code) = mp4_fourcc(edit.key) else {
                    continue;
                };
                let mut values = edit.values.iter();
                let Some(first) = values.next() else {
                    continue;
                };
                let mut atom = Atom::new(AtomIdent::Fourcc(code), AtomData::UTF8(first.clone()));
                for value in values {
                    atom.push_data(AtomData::UTF8(value.clone()));
                }
                native.replace_atom(atom);
            }
            native
                .save_to_path(temp, WriteOptions::new())
                .map_err(|error| Refusal::SaveFailed {
                    reason: error_chain(&error),
                })?;
            Ok(())
        }
        AudioFormat::Aac | AudioFormat::Wav => Err(Refusal::ReadOnlyFormat {
            format: format.as_str().to_owned(),
            reason: "read-only container".to_owned(),
        }),
    }
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

fn id3_static_id(key: ItemKey) -> &'static str {
    match key {
        ItemKey::TrackTitle => "TIT2",
        ItemKey::TrackArtist => "TPE1",
        ItemKey::AlbumTitle => "TALB",
        ItemKey::AlbumArtist => "TPE2",
        ItemKey::Genre => "TCON",
        _ => "TXXX",
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
