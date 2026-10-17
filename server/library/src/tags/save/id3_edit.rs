//! ID3 edits: the homes edits resolve to, and their save on lofty's
//! native tag, repaired from byte truth.

use lofty::TextEncoding;
use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt as _;
use lofty::id3::v2::{
    ExtendedTextFrame, Frame, FrameId, Id3v2Tag, Id3v2Version, TextInformationFrame,
    UniqueFileIdentifierFrame, UnsynchronizedTextFrame,
};
use lofty::tag::{TagExt as _, TagType};

use super::document::FieldRead;
use super::edit::{foreign, supersede};
use super::id3::{
    Id3, existing_txxx, id3_pair, id3_read, is_raw_native_id, raw_after, split_v23_date, year_of,
};
use super::id3_bytes::{
    DeepId3, People, comment_owned, comment_payload, decode_lang_text, decode_people,
    is_key_value_id, people_pairs, people_payload, people_sources, performer_pair, performer_text,
    replace_people, role_owns,
};
use super::{PictureSpec, Refusal, TagEdit, lofty_picture, save_failed};
use crate::tags::fields::{INVOLVED_ROLES, Id3Target, Role, Slot, TagField};

/// One edit at its home in an ID3 tag. No values removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Id3Edit {
    Text {
        id: String,
        values: Vec<String>,
    },
    User {
        desc: String,
        values: Vec<String>,
    },
    Ufid {
        owner: String,
        value: Option<String>,
    },
    /// The one `USLT` frame: its text, or none to remove it.
    Lyrics {
        text: Option<String>,
    },
    /// Every frame with this id after the save, as raw payloads (none
    /// removes them).
    Raw {
        id: String,
        payloads: Vec<Vec<u8>>,
    },
}

impl Id3Edit {
    /// The frame key verification excludes from the untouched set.
    pub(super) fn key(&self) -> String {
        match self {
            Id3Edit::Text { id, .. } | Id3Edit::Raw { id, .. } => id.clone(),
            Id3Edit::User { desc, .. } => format!("TXXX:{desc}"),
            Id3Edit::Ufid { owner, .. } => format!("UFID:{owner}"),
            Id3Edit::Lyrics { .. } => "USLT".to_owned(),
        }
    }
}

/// Field edits at their ID3 homes.
pub(super) fn resolve_fields(
    tag: Option<&DeepId3>,
    edits: &[TagEdit],
) -> Result<Vec<Id3Edit>, Refusal> {
    let mut resolved: Vec<Id3Edit> = Vec::new();
    let mut push = |edit: Id3Edit| supersede::<Id3>(&mut resolved, edit);
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
                        if index > 0 && !pairs.iter().any(|pair| role_owns(source, role, &pair.0)) {
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
                    "the file holds several comments; they cannot be written as one".to_owned(),
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
                if decode_lang_text(&payload).ok() != Some((language, String::new(), text.clone()))
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
                push(Id3Edit::Text {
                    id: id.to_owned(),
                    values,
                });
            }
            Id3Target::User(desc) => push(Id3Edit::User {
                desc: tag
                    .and_then(|tag| existing_txxx(tag, desc))
                    .unwrap_or_else(|| desc.to_owned()),
                values,
            }),
            Id3Target::Ufid(owner) => push(Id3Edit::Ufid {
                owner: owner.to_owned(),
                value: values.into_iter().next(),
            }),
            Id3Target::Lyrics => push(Id3Edit::Lyrics {
                text: (!edit.values.is_empty()).then(|| edit.values.join("\n")),
            }),
            Id3Target::Pair(id, slot) => {
                let index = match pairs.iter().position(|(known, _, _)| *known == id) {
                    Some(index) => index,
                    None => {
                        let (number, total) = tag.map_or((None, None), |tag| id3_pair(tag, id));
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
                    push(Id3Edit::Text {
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
        push(Id3Edit::Raw {
            id: id.to_owned(),
            payloads,
        });
    }
    if let Some(payloads) = comments {
        push(Id3Edit::Raw {
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
        push(Id3Edit::Text {
            id: id.to_owned(),
            values,
        });
    }
    Ok(resolved)
}

/// The edit an `id3:` native key names. A raw frame id keeps the frames
/// of that id the edit leaves alone, as resolved so far or as the file
/// holds them.
pub(super) fn resolve_native(
    tag: Option<&DeepId3>,
    key: &str,
    values: &[String],
    resolved: &[Id3Edit],
) -> Result<Id3Edit, Refusal> {
    let Some(("id3", rest)) = key.split_once(':') else {
        return Err(foreign(key));
    };
    let values = values.to_vec();
    if let Some(desc) = rest.strip_prefix("TXXX:") {
        return Ok(Id3Edit::User {
            desc: desc.to_owned(),
            values,
        });
    }
    if rest.len() == 4 && rest.starts_with('T') && !is_key_value_id(rest) {
        return Ok(Id3Edit::Text {
            id: rest.to_owned(),
            values,
        });
    }
    if !is_raw_native_id(rest) {
        return Err(foreign(key));
    }
    let payloads = values
        .iter()
        .map(|value| {
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, value).map_err(
                |_| Refusal::NoByteInventory {
                    detail: format!("tag '{key}' holds a value that is not base64"),
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let current: Vec<Vec<u8>> = match resolved.iter().find_map(|known| match known {
        Id3Edit::Raw { id, payloads } if id == rest => Some(payloads),
        _ => None,
    }) {
        Some(payloads) => payloads.clone(),
        None => tag
            .map(|tag| {
                tag.frames
                    .iter()
                    .filter(|frame| frame.text.is_none() && frame.id == rest)
                    .map(|frame| frame.payload.clone())
                    .collect()
            })
            .unwrap_or_default(),
    };
    Ok(Id3Edit::Raw {
        id: rest.to_owned(),
        payloads: raw_after(&current, rest, &payloads),
    })
}

/// Save the edits to `temp` on lofty's native tag, rebuilt from byte
/// truth first.
pub(super) fn apply(
    truth: Option<&DeepId3>,
    tagged: &lofty::file::TaggedFile,
    edits: &[Id3Edit],
    pictures: Option<&[PictureSpec]>,
    temp: &std::path::Path,
) -> Result<(), Refusal> {
    let mut native = tagged
        .tag(TagType::Id3v2)
        .cloned()
        .map(Id3v2Tag::from)
        .unwrap_or_default();
    repair_id3_text(&mut native, truth);
    for edit in edits {
        match edit {
            Id3Edit::Text { id, values } => {
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
            Id3Edit::User { desc, values } => {
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
            Id3Edit::Ufid { owner, value } => {
                native.retain(|frame| {
                    !matches!(frame, Frame::UniqueFileIdentifier(ufid)
                        if ufid.owner == owner.as_str())
                });
                if let Some(value) = value {
                    native.insert(Frame::UniqueFileIdentifier(UniqueFileIdentifierFrame::new(
                        owner.clone(),
                        value.as_bytes().to_vec(),
                    )));
                }
            }
            Id3Edit::Raw { id, payloads } => {
                let frame_id = FrameId::Valid(std::borrow::Cow::Owned(id.clone()));
                let _ = native.remove(&frame_id).count();
                for payload in payloads {
                    native.insert(Frame::Binary(lofty::id3::v2::BinaryFrame::new(
                        frame_id.clone(),
                        payload.clone(),
                    )));
                }
            }
            Id3Edit::Lyrics { text } => {
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
