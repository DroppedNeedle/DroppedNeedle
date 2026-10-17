//! A v2.3 tag holding v2.4 frames, saved whole as ID3v2.4.

use std::fs;
use std::path::Path;

use super::Refusal;
use super::document::FieldRead;
use super::id3::{date_parts, frame_key, id3_read_as, id3_text, key_multiset, raw_map, text_map};
use super::id3_bytes::{
    DeepId3, Id3FrameTruth, Id3TextTruth, People, decode_people, encode_id3_strings,
    parse_id3_deep, people_payload,
};
use crate::tags::fields::{Id3Target, TagField};
use crate::tags::{AudioFormat, TagsError, format_for_path};

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
pub(super) const V24_ONLY_IDS: &[&str] = &[
    "ASPI", "EQU2", "POSS", "RVA2", "SEEK", "SIGN", "TDEN", "TDOR", "TDRC", "TDRL", "TDTG", "TIPL",
    "TMCL", "TMOO", "TPRO", "TRSN", "TSOA", "TSOP", "TSOT", "TSST",
];

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
const PEOPLE_FIELDS: &[TagField] = &[
    TagField::Performer,
    TagField::Arranger,
    TagField::Producer,
    TagField::Engineer,
    TagField::Mixer,
    TagField::DjMixer,
];

pub(super) fn is_mixed_v23(tag: &DeepId3) -> bool {
    tag.major == 3
        && tag
            .frames
            .iter()
            .any(|frame| V24_ONLY_IDS.contains(&frame.id.as_str()))
}

/// The tag as a save writes it: a mixed v2.3 tag as its ID3v2.4 form,
/// any other as it is.
pub(super) fn as_saved(tag: DeepId3) -> Result<DeepId3, Refusal> {
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

/// Why a save cannot move this tag to ID3v2.4 whole: a mixed v2.3 tag
/// whose chapter or `LINK` frames cannot move unchanged. In plain words.
pub(super) fn unmovable_frames(tag: &DeepId3) -> Option<String> {
    if is_mixed_v23(tag) {
        v24_layouts(tag).err()
    } else {
        None
    }
}

/// Why a save cannot keep this tag (as saved) whole, in plain words, or
/// `None` when it can. The writer keeps one of two identical frames,
/// one `PCNT` and the like, and one `UFID` per owner, so a tag holding
/// more would lose a frame, and undo could put back only one.
pub(super) fn frames_kept_once(tag: &DeepId3) -> Option<String> {
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
            return Some(format!(
                "The file's ID3 tag holds two {} frames where the tag writer keeps only one, so saving its tags would lose one.",
                frame.id
            ));
        }
    }
    None
}

/// Whether a save of this file writes ID3v2.3: an MP3 whose tag is v2.3
/// and holds no v2.4 frames (a mixed tag is saved as v2.4). Such a save
/// joins several values of one text frame with "/", and a read gives
/// them back as one value.
pub fn id3_lands_as_v23(path: &Path) -> Result<bool, TagsError> {
    if format_for_path(path)? != AudioFormat::Mp3 {
        return Ok(false);
    }
    let bytes = fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let tag = parse_id3_deep(&bytes).map_err(|refusal| TagsError::TagRead {
        path: path.display().to_string(),
        reason: refusal.to_string(),
    })?;
    Ok(tag.is_some_and(|tag| tag.major == 3 && !is_mixed_v23(&tag)))
}

/// A mixed tag as ID3v2.4, and its conflict notes.
pub(super) struct Upgrade {
    pub(super) tag: DeepId3,
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
pub(super) fn upgrade_to_v24(tag: &DeepId3) -> Result<Upgrade, Refusal> {
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
