//! Edits checked before a save, then resolved to their native homes in
//! the file's tag.

use lofty::tag::items::Timestamp;

use super::document::FieldRead;
use super::id3::{
    existing_txxx, id3_pair, id3_read, is_raw_native_id, raw_after, split_v23_date, year_of,
};
use super::id3_bytes::{
    People, comment_owned, comment_payload, decode_lang_text, decode_people, is_key_value_id,
    people_pairs, people_payload, people_sources, performer_pair, performer_text, replace_people,
    role_owns,
};
use super::mp4::{Mp4Item, existing_freeform, mp4_native, mp4_pair};
use super::vorbis::{valid_vorbis_key, vorbis_write_keys};
use super::{Refusal, Snapshot, TagEdit};
use crate::tags::fields::{
    FieldKind, INVOLVED_ROLES, Id3Target, MP4_MEAN, Mp4Target, Role, Slot, TagField,
};

// ---------------------------------------------------------------------------
// Pre-save scan: values each field can hold.
// ---------------------------------------------------------------------------

/// True when the field can hold these values (dates parse, counts are
/// numbers, single-valued fields have one value, nothing is blank).
#[must_use]
pub fn accepts(edit: &TagEdit) -> bool {
    check_edit(edit).is_ok()
}

pub(super) fn check_edit(edit: &TagEdit) -> Result<(), Refusal> {
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
// Edits resolved to their native homes in this file's tag.
// ---------------------------------------------------------------------------

/// One edit at its native home. No values (or a zero pair, or no flag)
/// removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NativeEdit {
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
    pub(super) fn id3_key(&self) -> Option<String> {
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
pub(super) fn resolve_edits(
    snapshot: &Snapshot,
    edits: &[TagEdit],
) -> Result<Vec<NativeEdit>, Refusal> {
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
