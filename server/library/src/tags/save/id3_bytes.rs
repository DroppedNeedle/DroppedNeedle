//! ID3 byte inventory: version, frame ids, text truth, raw payloads, and
//! the frame codecs the writer builds its own frames with.

use super::Refusal;
use super::id3_upgrade::V24_ONLY_IDS;
use crate::tags::fields::{INVOLVED_ROLES, Role};

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
pub(super) struct Id3FrameTruth {
    pub(super) id: String,
    pub(super) payload: Vec<u8>,
    pub(super) text: Option<Id3TextTruth>,
}

#[derive(Clone)]
pub(super) struct Id3TextTruth {
    /// Description for `TXXX`, `None` for plain text frames.
    pub(super) desc: Option<String>,
    pub(super) values: Vec<String>,
}

#[derive(Clone)]
pub(super) struct DeepId3 {
    pub(super) major: u8,
    pub(super) minor: u8,
    pub(super) frames: Vec<Id3FrameTruth>,
}

/// Key-value list ids, which are not text despite the `T` prefix.
pub(super) fn is_key_value_id(id: &str) -> bool {
    matches!(id, "TIPL" | "TMCL")
}

/// Text frames: the `T` family minus the key-value lists, plus iTunes'
/// movement frames (`MVNM`, `MVIN`).
fn is_text_id(id: &str) -> bool {
    (id.starts_with('T') && !is_key_value_id(id)) || matches!(id, "MVNM" | "MVIN")
}

pub(super) fn parse_id3_deep(data: &[u8]) -> Result<Option<DeepId3>, Refusal> {
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
pub(super) fn decode_uslt(payload: &[u8]) -> Result<String, String> {
    decode_lang_text(payload).map(|(_, _, text)| text)
}

/// A `USLT` or `COMM` payload: encoding, three-byte language, a
/// terminated description, then the text.
pub(super) fn decode_lang_text(payload: &[u8]) -> Result<([u8; 3], String, String), String> {
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
pub(super) fn comment_owned(payload: &[u8]) -> bool {
    decode_lang_text(payload).is_ok_and(|(_, description, _)| description.is_empty())
}

/// Strings for a frame this wrapper builds, each terminated: UTF-8 in
/// v2.4, UTF-16 with a byte-order mark in v2.3 (which has no UTF-8).
/// Returns the encoding byte and the body.
pub(super) fn encode_id3_strings(v23: bool, parts: &[&str]) -> (u8, Vec<u8>) {
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
pub(super) fn comment_payload(v23: bool, language: [u8; 3], text: &str) -> Vec<u8> {
    let (encoding, body) = encode_id3_strings(v23, &["", text]);
    let mut payload = vec![encoding];
    payload.extend_from_slice(&language);
    payload.extend_from_slice(&body);
    payload
}

/// The role and name pairs of a people frame, in order.
pub(super) type People = Vec<(String, String)>;

/// The role and name pairs of a people frame.
pub(super) fn decode_people(payload: &[u8]) -> Result<Vec<(String, String)>, String> {
    let values = decode_id3_strings(payload)?;
    if values.len() % 2 != 0 {
        return Err("a role without a name".to_owned());
    }
    Ok(values
        .chunks_exact(2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect())
}

pub(super) fn people_payload(v23: bool, pairs: &[(String, String)]) -> Vec<u8> {
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
pub(super) fn people_pairs(tag: &DeepId3, id: &str) -> Result<Vec<(String, String)>, String> {
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
pub(super) fn people_sources(id: &'static str, role: Role) -> Vec<&'static str> {
    if id == "TMCL" && role == Role::Performer {
        vec!["TMCL", "TIPL"]
    } else {
        vec![id]
    }
}

/// Whether `role` owns a pair whose role is `pair_role` in frame `id`.
pub(super) fn role_owns(id: &str, role: Role, pair_role: &str) -> bool {
    match role {
        Role::Named(name) => pair_role == name,
        Role::Performer => id == "TMCL" || !INVOLVED_ROLES.contains(&pair_role),
    }
}

/// A musician credit as shown: `name (instrument)`, or the name alone.
pub(super) fn performer_text(instrument: &str, name: &str) -> String {
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
pub(super) fn performer_pair(value: &str) -> (String, String) {
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
pub(super) fn replace_people(
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
