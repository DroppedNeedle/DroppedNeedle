//! ID3 as a [`TagFormat`]: items no field owns, field reads, and the
//! post-save verify. Edits and their save are in `id3_edit`.

use std::collections::BTreeMap;
use std::path::Path;

use lofty::file::TaggedFile;

use super::document::{FieldDocument, FieldRead, collect_reads, values_or_absent};
use super::id3_bytes::{
    DeepId3, Id3FrameTruth, Id3TextTruth, comment_owned, decode_lang_text, decode_uslt,
    parse_id3_deep, people_pairs, people_sources, performer_pair, performer_text, role_owns,
};
use super::id3_edit::{self, Id3Edit};
use super::id3_upgrade::{as_saved, frames_kept_once, unmovable_frames};
use super::kept::{LeftAlone, TagsKept};
use super::{PictureSpec, Refusal, TagEdit, TagFormat, mismatch};
use crate::tags::AudioFormat;
use crate::tags::fields::{Id3Target, Role, Slot, TagField};

/// The ID3v2 tag of an MP3.
pub(super) struct Id3;

impl TagFormat for Id3 {
    type Truth = DeepId3;
    type Edit = Id3Edit;

    fn parse(_format: AudioFormat, bytes: &[u8]) -> Result<Option<DeepId3>, Refusal> {
        parse_id3_deep(bytes)
    }

    fn list(tag: DeepId3, document: &mut FieldDocument) -> Result<(), Refusal> {
        let tag = as_saved(tag)?;
        document.native = id3_native(&tag);
        let reads = TagField::ALL
            .into_iter()
            .map(|field| (field, id3_read(&tag, field)))
            .collect();
        collect_reads(document, reads);
        Ok(())
    }

    /// A mixed v2.3 tag's truth becomes the ID3v2.4 tag it is saved as:
    /// edits resolve to v2.4 homes, and the verify expects version 2.4.
    fn prepare(tag: DeepId3) -> Result<DeepId3, Refusal> {
        as_saved(tag)
    }

    /// A tag the save cannot keep whole is refused; otherwise raw frames
    /// over [`MAX_RAW_NATIVE_BYTES`] are left as they are.
    fn kept(tag: DeepId3) -> Result<TagsKept, Refusal> {
        if let Some(reason) = unmovable_frames(&tag) {
            return Ok(TagsKept::refused(reason));
        }
        let tag = as_saved(tag)?;
        if let Some(reason) = frames_kept_once(&tag) {
            return Ok(TagsKept::refused(reason));
        }
        let left_alone = tag
            .frames
            .iter()
            .filter(|frame| frame.text.is_none() && is_raw_native_id(&frame.id))
            .filter(|frame| frame.id != "COMM" || !comment_owned(&frame.payload))
            .filter(|frame| frame.payload.len() > MAX_RAW_NATIVE_BYTES)
            .map(|frame| (frame.id.clone(), LeftAlone::OverSizeLimit))
            .collect();
        Ok(TagsKept {
            refusal: None,
            left_alone,
        })
    }

    fn resolve_fields(tag: Option<&DeepId3>, edits: &[TagEdit]) -> Result<Vec<Id3Edit>, Refusal> {
        id3_edit::resolve_fields(tag, edits)
    }

    fn resolve_native(
        tag: Option<&DeepId3>,
        key: &str,
        values: &[String],
        resolved: &[Id3Edit],
    ) -> Result<Id3Edit, Refusal> {
        id3_edit::resolve_native(tag, key, values, resolved)
    }

    fn same_target(a: &Id3Edit, b: &Id3Edit) -> bool {
        a.key() == b.key()
    }

    fn apply(
        tag: Option<&DeepId3>,
        tagged: &TaggedFile,
        edits: &[Id3Edit],
        pictures: Option<&[PictureSpec]>,
        temp: &Path,
    ) -> Result<(), Refusal> {
        id3_edit::apply(tag, tagged, edits, pictures, temp)
    }

    fn verify(
        before: Option<&DeepId3>,
        after: Option<&DeepId3>,
        edits: &[Id3Edit],
        pictures_edited: bool,
    ) -> Result<(), Refusal> {
        verify_id3(before.is_some(), before, after, edits, pictures_edited)
    }

    fn version(tag: &DeepId3) -> Option<String> {
        Some(format!("2.{}", tag.major))
    }
}

/// The date the tag editor reads from an ID3v2.3 tag that also holds the
/// v2.4 `TDRC` frame (`tag_bytes` from the start of the file). `None` for
/// any other tag, or one the editor cannot read.
pub(crate) fn mixed_v23_date(tag_bytes: &[u8]) -> Option<String> {
    let tag = parse_id3_deep(tag_bytes).ok()??;
    if tag.major != 3 || id3_text(&tag, "TDRC").is_none() {
        return None;
    }
    match id3_read(&as_saved(tag).ok()?, TagField::Date) {
        FieldRead::Values(values) => values.into_iter().next(),
        FieldRead::Absent | FieldRead::Opaque => None,
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
pub(super) fn raw_after(current: &[Vec<u8>], id: &str, payloads: &[Vec<u8>]) -> Vec<Vec<u8>> {
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
pub(super) fn is_raw_native_id(id: &str) -> bool {
    matches!(id, "COMM" | "PRIV" | "GEOB")
        || (id.len() == 4 && id.starts_with('W') && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// A date's parts: year, then month and day, then hour and minute.
type DateParts<'a> = (
    &'a str,
    Option<(&'a str, &'a str)>,
    Option<(&'a str, &'a str)>,
);

/// Split `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM[:SS]`;
/// anything else is not a date this code reshapes.
pub(super) fn date_parts(value: &str) -> Option<DateParts<'_>> {
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
pub(super) fn year_of(value: &str) -> String {
    match date_parts(value) {
        Some((year, _, _)) => year.to_owned(),
        None => value.to_owned(),
    }
}

/// v2.3 date frames for one date: `TYER`, `TDAT` (`DDMM`), `TIME`
/// (`HHMM`). A value that is not a date goes to `TYER` as it is.
pub(super) fn split_v23_date(value: Option<&str>) -> (Vec<String>, Vec<String>, Vec<String>) {
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

pub(super) fn existing_txxx(tag: &DeepId3, desc: &str) -> Option<String> {
    tag.frames.iter().find_map(|frame| {
        let existing = frame.text.as_ref()?.desc.as_ref()?;
        existing
            .eq_ignore_ascii_case(desc)
            .then(|| existing.clone())
    })
}

pub(super) fn id3_text<'a>(tag: &'a DeepId3, id: &str) -> Option<&'a Vec<String>> {
    tag.frames
        .iter()
        .find(|frame| frame.id == id)
        .and_then(|frame| frame.text.as_ref())
        .map(|text| &text.values)
}

/// The `n/m` halves of an ID3 pair frame, exactly as written.
pub(super) fn id3_pair(tag: &DeepId3, id: &str) -> (Option<String>, Option<String>) {
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

/// A field's values as the file's ID3 tag holds them.
pub(super) fn id3_read(tag: &DeepId3, field: TagField) -> FieldRead {
    id3_read_as(tag, field, tag.major == 3)
}

/// A field's values read from its v2.3 home (`v23`) or its v2.4 one,
/// whatever the tag's own version.
pub(super) fn id3_read_as(tag: &DeepId3, field: TagField, v23: bool) -> FieldRead {
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
pub(super) fn frame_key(frame: &Id3FrameTruth) -> String {
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
    edits: &[Id3Edit],
    pictures_edited: bool,
) -> Result<(), Refusal> {
    let mut edited: Vec<String> = edits.iter().map(Id3Edit::key).collect();
    // Picture bytes are checked on their own.
    if pictures_edited {
        edited.push("APIC".to_owned());
    }
    let removal_only = !pictures_edited
        && edits.iter().all(|edit| match edit {
            Id3Edit::Text { values, .. } | Id3Edit::User { values, .. } => values.is_empty(),
            Id3Edit::Ufid { value, .. } => value.is_none(),
            Id3Edit::Lyrics { text } => text.is_none(),
            Id3Edit::Raw { payloads, .. } => payloads.is_empty(),
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
        let key = edit.key();
        let landed: Vec<&Id3FrameTruth> = frames
            .iter()
            .filter(|frame| frame_key(frame) == key)
            .collect();
        let ok = match edit {
            Id3Edit::Text { values, .. } | Id3Edit::User { values, .. } => {
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
            Id3Edit::Ufid { owner, value } => match value {
                None => landed.is_empty(),
                Some(value) => {
                    let mut payload = owner.as_bytes().to_vec();
                    payload.push(0);
                    payload.extend_from_slice(value.as_bytes());
                    landed.len() == 1 && landed[0].payload == payload
                }
            },
            Id3Edit::Lyrics { text } => match text {
                None => landed.is_empty(),
                Some(text) => {
                    landed.len() == 1
                        && decode_uslt(&landed[0].payload).ok().as_deref() == Some(text.as_str())
                }
            },
            Id3Edit::Raw { payloads, .. } => {
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
        };
        if !ok {
            return mismatch(format!("edited frame '{key}' did not land"));
        }
    }
    Ok(())
}

pub(super) fn text_map(frames: &[Id3FrameTruth]) -> BTreeMap<String, Vec<String>> {
    // The deep parse refuses duplicate text keys, so each key is unique.
    frames
        .iter()
        .filter_map(|frame| {
            let text = frame.text.as_ref()?;
            Some((frame_key(frame), text.values.clone()))
        })
        .collect()
}

pub(super) fn raw_map(frames: &[Id3FrameTruth]) -> BTreeMap<String, Vec<Vec<u8>>> {
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

pub(super) fn key_multiset(frames: &[Id3FrameTruth]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for frame in frames {
        *counts.entry(frame_key(frame)).or_insert(0) += 1;
    }
    counts
}
