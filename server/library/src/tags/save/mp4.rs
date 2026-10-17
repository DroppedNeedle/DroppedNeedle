//! MP4 `ilst` items as a [`TagFormat`]: the atom inventory, field reads,
//! and the post-save verify. Edits and their save are in `mp4_edit`.

use std::collections::BTreeMap;
use std::path::Path;

use lofty::file::TaggedFile;

use super::document::{FieldDocument, FieldRead, collect_reads, values_or_absent};
use super::kept::{LeftAlone, TagsKept};
use super::mp4_edit::{self, Mp4Edit};
use super::{PictureSpec, Refusal, TagEdit, TagFormat, mismatch};
use crate::tags::AudioFormat;
use crate::tags::fields::{MP4_MEAN, Mp4Target, Slot, TagField};

/// The `ilst` items of an M4A file.
pub(super) struct Mp4;

impl TagFormat for Mp4 {
    type Truth = Vec<Mp4AtomTruth>;
    type Edit = Mp4Edit;

    const PICTURE_BYTES_ONLY: bool = true;

    fn parse(_format: AudioFormat, bytes: &[u8]) -> Result<Option<Vec<Mp4AtomTruth>>, Refusal> {
        parse_mp4_atoms(bytes)
    }

    fn list(atoms: Vec<Mp4AtomTruth>, document: &mut FieldDocument) -> Result<(), Refusal> {
        document.native = mp4_native(&atoms, false);
        let reads = TagField::ALL
            .into_iter()
            .map(|field| (field, mp4_read(&atoms, field)))
            .collect();
        collect_reads(document, reads);
        Ok(())
    }

    /// Items holding a value that is not text, and atoms of one code
    /// held twice, are left as they are.
    fn kept(atoms: Vec<Mp4AtomTruth>) -> Result<TagsKept, Refusal> {
        let left_alone = mp4_listing(&atoms, false)
            .1
            .into_iter()
            .map(|(key, why)| (key["mp4:".len()..].to_owned(), why))
            .collect();
        Ok(TagsKept {
            refusal: None,
            left_alone,
        })
    }

    fn resolve_fields(
        atoms: Option<&Vec<Mp4AtomTruth>>,
        edits: &[TagEdit],
    ) -> Result<Vec<Mp4Edit>, Refusal> {
        mp4_edit::resolve_fields(atoms.map(Vec::as_slice), edits)
    }

    fn resolve_native(
        atoms: Option<&Vec<Mp4AtomTruth>>,
        key: &str,
        values: &[String],
        _resolved: &[Mp4Edit],
    ) -> Result<Mp4Edit, Refusal> {
        mp4_edit::resolve_native(atoms.map(Vec::as_slice), key, values)
    }

    fn same_target(a: &Mp4Edit, b: &Mp4Edit) -> bool {
        a.same_target(b)
    }

    fn apply(
        atoms: Option<&Vec<Mp4AtomTruth>>,
        tagged: &TaggedFile,
        edits: &[Mp4Edit],
        pictures: Option<&[PictureSpec]>,
        temp: &Path,
    ) -> Result<(), Refusal> {
        mp4_edit::apply(atoms.map(Vec::as_slice), tagged, edits, pictures, temp)
    }

    fn verify(
        before: Option<&Vec<Mp4AtomTruth>>,
        after: Option<&Vec<Mp4AtomTruth>>,
        edits: &[Mp4Edit],
        pictures_edited: bool,
    ) -> Result<(), Refusal> {
        verify_mp4(
            before.map(Vec::as_slice),
            after.map(Vec::as_slice),
            edits,
            pictures_edited,
        )
    }
}

/// MP4 text atoms and iTunes freeforms no writable field owns, or with
/// `owned_too`, every one that holds text (what a native edit may
/// replace: an undo recorded before a field owned the item names it).
pub(super) fn mp4_native(atoms: &[Mp4AtomTruth], owned_too: bool) -> BTreeMap<String, Vec<String>> {
    mp4_listing(atoms, owned_too).0
}

/// [`mp4_native`], and the items it leaves out, each with why.
fn mp4_listing(
    atoms: &[Mp4AtomTruth],
    owned_too: bool,
) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, LeftAlone>) {
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
    let mut unlisted: BTreeMap<String, LeftAlone> = BTreeMap::new();
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
                            unlisted.insert(key, LeftAlone::NotText);
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
        // Two atoms of one code, or a value that is not text: left alone.
        match texts.filter(|texts| !texts.is_empty()) {
            None => {
                unlisted.insert(key, LeftAlone::NotText);
            }
            Some(_) if out.contains_key(&key) || unlisted.contains_key(&key) => {
                unlisted.entry(key).or_insert(LeftAlone::HeldTwice);
            }
            Some(texts) => {
                out.insert(key, texts);
            }
        }
    }
    out.retain(|key, _| !unlisted.contains_key(key));
    (out, unlisted)
}

// ---------------------------------------------------------------------------
// MP4 atom inventory: type plus a hash of each `ilst` child's bytes.
// ---------------------------------------------------------------------------

/// One `ilst` child's semantic content. lofty normalizes encodings on
/// save (multi-data atoms split apart, integers shrink to minimal width,
/// tuples re-encoded), so atoms compare by meaning, not by bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Mp4Item {
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

pub(super) struct Mp4AtomTruth {
    pub(super) kind: [u8; 4],
    pub(super) items: Vec<Mp4Item>,
    /// Each item's data type (1 is UTF-8, 21 a signed integer), `None`
    /// where the item has no data box or was not read from bytes.
    pub(super) types: Vec<Option<u32>>,
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

pub(super) fn existing_freeform(atoms: &[Mp4AtomTruth], name: &str) -> Option<String> {
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

pub(super) fn mp4_pair(atoms: &[Mp4AtomTruth], code: [u8; 4]) -> Option<(u32, u32)> {
    atoms
        .iter()
        .filter(|atom| atom.kind == code)
        .flat_map(|atom| atom.items.iter())
        .find_map(|item| match item {
            Mp4Item::Pair(number, total) => Some((*number, *total)),
            _ => None,
        })
}

/// A field's values as the file's MP4 atoms hold them.
pub(super) fn mp4_read(atoms: &[Mp4AtomTruth], field: TagField) -> FieldRead {
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

fn verify_mp4(
    before: Option<&[Mp4AtomTruth]>,
    after: Option<&[Mp4AtomTruth]>,
    edits: &[Mp4Edit],
    pictures_edited: bool,
) -> Result<(), Refusal> {
    let mut edited_codes: Vec<[u8; 4]> = edits.iter().filter_map(Mp4Edit::code).collect();
    // Picture bytes are checked on their own.
    if pictures_edited {
        edited_codes.push(*b"covr");
    }
    let edited_names: Vec<&str> = edits
        .iter()
        .filter_map(|edit| match edit {
            Mp4Edit::Freeform { name, .. } => Some(name.as_str()),
            Mp4Edit::Text { .. }
            | Mp4Edit::Pair { .. }
            | Mp4Edit::Flag { .. }
            | Mp4Edit::Integer { .. } => None,
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
            Mp4Edit::Text { code, values } => {
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
            Mp4Edit::Freeform { name, values } => {
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
            Mp4Edit::Pair {
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
            Mp4Edit::Flag { code, value } => {
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
            Mp4Edit::Integer { code, value } => {
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
