//! MP4 edits: the `ilst` homes edits resolve to, and their save on
//! lofty's native tag, repaired from byte truth.

use lofty::config::WriteOptions;
use lofty::file::{TaggedFile, TaggedFileExt as _};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::tag::{Accessor as _, TagExt as _, TagType};

use super::edit::{foreign, supersede};
use super::mp4::{Mp4, Mp4AtomTruth, Mp4Item, existing_freeform, mp4_native, mp4_pair};
use super::{PictureSpec, Refusal, TagEdit, lofty_picture, save_failed};
use crate::tags::fields::{MP4_MEAN, Mp4Target, Slot};

/// One edit at its home in an MP4 `ilst`. No values (or a zero pair, or
/// no flag) removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Mp4Edit {
    Text {
        code: [u8; 4],
        values: Vec<String>,
    },
    Freeform {
        name: String,
        values: Vec<String>,
    },
    Pair {
        code: [u8; 4],
        number: u32,
        total: u32,
    },
    Flag {
        code: [u8; 4],
        value: Option<bool>,
    },
    Integer {
        code: [u8; 4],
        value: Option<i32>,
    },
}

impl Mp4Edit {
    /// The atom code an edit writes; `None` for a freeform.
    pub(super) fn code(&self) -> Option<[u8; 4]> {
        match self {
            Mp4Edit::Text { code, .. }
            | Mp4Edit::Pair { code, .. }
            | Mp4Edit::Flag { code, .. }
            | Mp4Edit::Integer { code, .. } => Some(*code),
            Mp4Edit::Freeform { .. } => None,
        }
    }

    pub(super) fn same_target(&self, other: &Mp4Edit) -> bool {
        match (self, other) {
            (Mp4Edit::Text { code: a, .. }, Mp4Edit::Text { code: b, .. })
            | (Mp4Edit::Pair { code: a, .. }, Mp4Edit::Pair { code: b, .. })
            | (Mp4Edit::Flag { code: a, .. }, Mp4Edit::Flag { code: b, .. })
            | (Mp4Edit::Integer { code: a, .. }, Mp4Edit::Integer { code: b, .. }) => a == b,
            (Mp4Edit::Freeform { name: a, .. }, Mp4Edit::Freeform { name: b, .. }) => a == b,
            // Different kinds of item are different homes.
            _ => false,
        }
    }
}

/// Field edits at their MP4 homes.
pub(super) fn resolve_fields(
    atoms: Option<&[Mp4AtomTruth]>,
    edits: &[TagEdit],
) -> Result<Vec<Mp4Edit>, Refusal> {
    let mut resolved: Vec<Mp4Edit> = Vec::new();
    let mut push = |edit: Mp4Edit| supersede::<Mp4>(&mut resolved, edit);
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
            Mp4Target::Text(code) => push(Mp4Edit::Text {
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
                            .ok_or_else(|| unencodable("MP4 numbers must be whole numbers"))?,
                    ),
                };
                push(Mp4Edit::Integer { code, value });
            }
            Mp4Target::Freeform(name) => push(Mp4Edit::Freeform {
                name: atoms
                    .and_then(|atoms| existing_freeform(atoms, name))
                    .unwrap_or_else(|| name.to_owned()),
                values: edit.values.clone(),
            }),
            Mp4Target::Pair(code, slot) => {
                let index = match pairs.iter().position(|(known, _, _)| *known == code) {
                    Some(index) => index,
                    None => {
                        let (number, total) = atoms
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
                push(Mp4Edit::Flag { code, value });
            }
        }
    }
    for (code, number, total) in pairs {
        push(Mp4Edit::Pair {
            code,
            number,
            total,
        });
    }
    Ok(resolved)
}

/// The edit an `mp4:` native key names.
pub(super) fn resolve_native(
    atoms: Option<&[Mp4AtomTruth]>,
    key: &str,
    values: &[String],
) -> Result<Mp4Edit, Refusal> {
    let Some(("mp4", rest)) = key.split_once(':') else {
        return Err(foreign(key));
    };
    // The item is replaced whole: one holding a value that is not text is
    // refused rather than lose that value.
    let listed = atoms
        .map(|atoms| mp4_native(atoms, true))
        .unwrap_or_default()
        .contains_key(key);
    let held = atoms.is_some_and(|atoms| {
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
    let values = values.to_vec();
    match rest.strip_prefix("----:") {
        Some(name) => Ok(Mp4Edit::Freeform {
            name: name.to_owned(),
            values,
        }),
        None => {
            let code: Vec<u8> = rest
                .chars()
                .map(|c| u8::try_from(u32::from(c)).map_err(|_| foreign(key)))
                .collect::<Result<_, _>>()?;
            let code: [u8; 4] = code.try_into().map_err(|_| foreign(key))?;
            Ok(Mp4Edit::Text { code, values })
        }
    }
}

/// Save the edits to `temp` on lofty's native tag, repaired from byte
/// truth first and put back in the file's item order last.
pub(super) fn apply(
    atoms: Option<&[Mp4AtomTruth]>,
    tagged: &TaggedFile,
    edits: &[Mp4Edit],
    pictures: Option<&[PictureSpec]>,
    temp: &std::path::Path,
) -> Result<(), Refusal> {
    let mut native = tagged
        .tag(TagType::Mp4Ilst)
        .cloned()
        .map(Ilst::from)
        .unwrap_or_default();
    if let Some(atoms) = atoms {
        repair_mp4_text(&mut native, atoms);
    }
    for edit in edits {
        match edit {
            Mp4Edit::Text { code, values } => {
                replace_mp4(&mut native, AtomIdent::Fourcc(*code), values);
            }
            Mp4Edit::Freeform { name, values } => {
                let ident = AtomIdent::Freeform {
                    mean: std::borrow::Cow::Borrowed(MP4_MEAN),
                    name: std::borrow::Cow::Owned(name.clone()),
                };
                replace_mp4(&mut native, ident, values);
            }
            Mp4Edit::Flag { code, value } => {
                let ident = AtomIdent::Fourcc(*code);
                let _ = native.remove(&ident).count();
                if let Some(value) = value {
                    native.insert(Atom::new(ident, AtomData::Bool(*value)));
                }
            }
            Mp4Edit::Integer { code, value } => {
                let ident = AtomIdent::Fourcc(*code);
                let _ = native.remove(&ident).count();
                if let Some(value) = value {
                    native.insert(Atom::new(ident, AtomData::SignedInteger(*value)));
                }
            }
            Mp4Edit::Pair {
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
        }
    }
    if let Some(pictures) = pictures {
        native.remove_pictures();
        for spec in pictures {
            native.insert_picture(lofty_picture(spec));
        }
    }
    if let Some(atoms) = atoms {
        keep_mp4_order(&mut native, atoms);
    }
    native
        .save_to_path(temp, WriteOptions::new())
        .map_err(save_failed)
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
