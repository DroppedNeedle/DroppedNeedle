//! Vorbis comments (FLAC, Ogg, Opus) as a [`TagFormat`]: the raw
//! inventory, field reads, the keys a write updates, the save, and the
//! post-save verify.

use std::collections::BTreeMap;
use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::{TaggedFile, TaggedFileExt as _};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::{TagExt as _, TagType};

use super::document::{FieldDocument, FieldRead, collect_reads};
use super::edit::{foreign, supersede};
use super::kept::TagsKept;
use super::{PictureSpec, Refusal, TagEdit, TagFormat, lofty_picture, mismatch, save_failed};
use crate::tags::AudioFormat;
use crate::tags::fields::TagField;

/// The Vorbis comment of a FLAC, Ogg or Opus file.
pub(super) struct Vorbis;

/// One edit at its home in a Vorbis comment: every value of one key (no
/// values removes it). Keys match without case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VorbisEdit {
    key: String,
    values: Vec<String>,
}

impl TagFormat for Vorbis {
    type Truth = VorbisTruth;
    type Edit = VorbisEdit;

    fn parse(format: AudioFormat, bytes: &[u8]) -> Result<Option<VorbisTruth>, Refusal> {
        parse_vorbis_truth(format, bytes)
    }

    fn list(truth: VorbisTruth, document: &mut FieldDocument) -> Result<(), Refusal> {
        document.spellings = vorbis_spellings(&truth);
        document.native = vorbis_native(&truth);
        let reads = TagField::ALL
            .into_iter()
            .map(|field| (field, vorbis_read(&truth, field)))
            .collect();
        collect_reads(document, reads);
        Ok(())
    }

    fn prepare(truth: VorbisTruth) -> Result<VorbisTruth, Refusal> {
        if let Some(key) = empty_key(&truth) {
            return Err(Refusal::EmptyVorbisValue {
                key: key.to_owned(),
            });
        }
        if let Some((upper, variants)) = mixed_spelling(&truth) {
            return Err(Refusal::NoByteInventory {
                detail: format!("comment key '{upper}' has mixed spellings {variants:?}"),
            });
        }
        Ok(truth)
    }

    /// Every item is listed or owned by a field, so nothing is left
    /// alone; a comment the save would refuse is named instead.
    fn kept(truth: VorbisTruth) -> Result<TagsKept, Refusal> {
        if let Some(key) = empty_key(&truth) {
            return Ok(TagsKept::refused(format!(
                "The file's {key} tag is empty, and the tag writer cannot keep an empty tag."
            )));
        }
        if let Some((upper, variants)) = mixed_spelling(&truth) {
            return Ok(TagsKept::refused(format!(
                "The file spells its {upper} tag more than one way ({}), so saving its tags would merge them.",
                variants.join(", ")
            )));
        }
        Ok(TagsKept::default())
    }

    fn resolve_fields(
        truth: Option<&VorbisTruth>,
        edits: &[TagEdit],
    ) -> Result<Vec<VorbisEdit>, Refusal> {
        let mut resolved: Vec<VorbisEdit> = Vec::new();
        for edit in edits {
            if let Some(key) = edit.spelling {
                supersede::<Self>(
                    &mut resolved,
                    VorbisEdit {
                        key: key.to_owned(),
                        values: edit.values.clone(),
                    },
                );
                continue;
            }
            for key in vorbis_write_keys(edit.field, truth) {
                supersede::<Self>(
                    &mut resolved,
                    VorbisEdit {
                        key: key.to_owned(),
                        values: edit.values.clone(),
                    },
                );
            }
        }
        Ok(resolved)
    }

    fn resolve_native(
        truth: Option<&VorbisTruth>,
        key: &str,
        values: &[String],
        _resolved: &[VorbisEdit],
    ) -> Result<VorbisEdit, Refusal> {
        let rest = match key.split_once(':') {
            Some(("vorbis", rest)) if valid_vorbis_key(rest) => rest,
            _ => return Err(foreign(key)),
        };
        // Spellings are matched without case on write, so a key the file
        // also spells another way, or holds empty, is refused rather than
        // touched.
        let blocked = truth.is_some_and(|truth| {
            truth.pairs.iter().any(|(held, value)| {
                held.eq_ignore_ascii_case(rest) && (held != rest || value.is_empty())
            })
        });
        if blocked {
            return Err(Refusal::NoByteInventory {
                detail: format!("tag '{key}' is spelled more than one way or holds no value"),
            });
        }
        Ok(VorbisEdit {
            key: rest.to_owned(),
            values: values.to_vec(),
        })
    }

    fn same_target(a: &VorbisEdit, b: &VorbisEdit) -> bool {
        a.key.eq_ignore_ascii_case(&b.key)
    }

    fn apply(
        truth: Option<&VorbisTruth>,
        tagged: &TaggedFile,
        edits: &[VorbisEdit],
        pictures: Option<&[PictureSpec]>,
        temp: &Path,
    ) -> Result<(), Refusal> {
        let mut native = tagged
            .tag(TagType::VorbisComments)
            .cloned()
            .map(VorbisComments::from)
            .unwrap_or_default();
        // Empty every pair (pictures stay in the pictures vec), then push
        // the raw truth in file order, minus the edited keys, so unknown
        // fields the generic round-trip dropped come back, then the
        // edits. Nothing is removed after a push: removal reorders what
        // is left.
        let keys: Vec<String> = native.items().map(|(key, _)| key.to_owned()).collect();
        for key in &keys {
            native.remove(key).for_each(drop);
        }
        if let Some(truth) = truth {
            for (key, value) in &truth.pairs {
                let replaced = edits.iter().any(|edit| edit.key.eq_ignore_ascii_case(key));
                if !is_picture_field(key) && !replaced {
                    native.push(key.clone(), value.clone());
                }
            }
            native.set_vendor(truth.vendor.clone());
        }
        for edit in edits {
            for value in &edit.values {
                native.push(edit.key.clone(), value.clone());
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

    fn verify(
        before: Option<&VorbisTruth>,
        after: Option<&VorbisTruth>,
        edits: &[VorbisEdit],
        _pictures_edited: bool,
    ) -> Result<(), Refusal> {
        verify_vorbis(before.is_some(), before, after, edits)
    }
}

/// The first key holding an empty value, which lofty drops on save.
fn empty_key(truth: &VorbisTruth) -> Option<&str> {
    truth
        .pairs
        .iter()
        .find(|(_, value)| value.is_empty())
        .map(|(key, _)| key.as_str())
}

/// The first key spelled more than one way (`title` plus `TITLE`), with
/// its spellings: it cannot be edited without merging them. Repeated
/// keys with one spelling are ordinary multi-values.
fn mixed_spelling(truth: &VorbisTruth) -> Option<(String, Vec<String>)> {
    let mut spellings: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, _) in &truth.pairs {
        let entry = spellings.entry(key.to_ascii_uppercase()).or_default();
        if !entry.contains(key) {
            entry.push(key.clone());
        }
    }
    spellings
        .into_iter()
        .find(|(_, variants)| variants.len() > 1)
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

// ---------------------------------------------------------------------------
// Vorbis raw inventory: the comment payload lofty will not show us.
// ---------------------------------------------------------------------------

pub(super) struct VorbisTruth {
    pub(super) vendor: String,
    pub(super) pairs: Vec<(String, String)>,
}

/// Vorbis field rule, mirroring lofty's own check: ASCII 0x20-0x7D
/// except `=`, case-insensitive.
pub(super) fn valid_vorbis_key(key: &str) -> bool {
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

/// A field's values as the file's Vorbis comment holds them (Picard's
/// spelling first when the file uses several).
pub(super) fn vorbis_read(truth: &VorbisTruth, field: TagField) -> FieldRead {
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
pub(super) fn vorbis_spellings(
    truth: &VorbisTruth,
) -> BTreeMap<(TagField, &'static str), Vec<String>> {
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

fn verify_vorbis(
    had_tag: bool,
    before: Option<&VorbisTruth>,
    after: Option<&VorbisTruth>,
    edits: &[VorbisEdit],
) -> Result<(), Refusal> {
    // Keys compare without case, as the pairs are grouped.
    let expected: Vec<(String, &Vec<String>)> = edits
        .iter()
        .map(|edit| (edit.key.to_ascii_uppercase(), &edit.values))
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
