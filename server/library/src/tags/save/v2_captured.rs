//! Tags an earlier DroppedNeedle captured away from their file, read as
//! the [`FieldDocument`] the file would give.

use super::document::{FieldDocument, FieldRead, collect_reads};
use super::id3::id3_read;
use super::id3_bytes::parse_id3_deep;
use super::id3_upgrade::as_saved;
use super::mp4::{Mp4AtomTruth, Mp4Item, mp4_read};
use super::vorbis::{VorbisTruth, valid_vorbis_key, vorbis_read, vorbis_spellings};
use super::{Refusal, writable};
use crate::tags::AudioFormat;
use crate::tags::fields::TagField;

/// Tags captured away from their file (an earlier DroppedNeedle's
/// original-file snapshot), in the native shapes the readers here take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturedTags {
    /// A whole ID3v2 tag, header included (MP3).
    Id3(Vec<u8>),
    /// Vorbis comment pairs (FLAC, Ogg, Opus).
    Vorbis(Vec<(String, String)>),
    /// The items of an MP4 `ilst` (M4A).
    Mp4(Vec<CapturedAtom>),
}

/// One MP4 `ilst` item: its four-byte kind and its data values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedAtom {
    pub kind: [u8; 4],
    pub values: Vec<CapturedValue>,
}

/// One MP4 data value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturedValue {
    Text(String),
    Integer(u64),
    Bool(bool),
    /// `trkn`/`disk`: number and total.
    Pair(u32, u32),
    /// A `----` freeform item.
    Freeform {
        mean: String,
        name: String,
        data: Vec<u8>,
    },
    Bytes(Vec<u8>),
}

/// The [`FieldDocument`] a file of `format` holding `captured` would
/// read as: the same native homes, spellings and opaque rules as
/// [`read_document`], so a document built here restores exactly like one
/// read from the file. Read-only containers report nothing.
pub fn document_from_captured(
    format: AudioFormat,
    captured: &CapturedTags,
) -> Result<FieldDocument, Refusal> {
    let mut document = FieldDocument::default();
    if !writable(format) {
        return Ok(document);
    }
    let reads: Vec<(TagField, FieldRead)> = match (format, captured) {
        (AudioFormat::Mp3, CapturedTags::Id3(bytes)) => match parse_id3_deep(bytes)? {
            Some(tag) => {
                let tag = as_saved(tag)?;
                TagField::ALL
                    .into_iter()
                    .map(|field| (field, id3_read(&tag, field)))
                    .collect()
            }
            None => Vec::new(),
        },
        (AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus, CapturedTags::Vorbis(pairs)) => {
            if let Some((key, _)) = pairs.iter().find(|(key, _)| !valid_vorbis_key(key)) {
                return Err(Refusal::NoByteInventory {
                    detail: format!("invalid comment key '{key}'"),
                });
            }
            let truth = VorbisTruth {
                vendor: String::new(),
                pairs: pairs.clone(),
            };
            document.spellings = vorbis_spellings(&truth);
            TagField::ALL
                .into_iter()
                .map(|field| (field, vorbis_read(&truth, field)))
                .collect()
        }
        (AudioFormat::M4a, CapturedTags::Mp4(atoms)) => {
            let atoms: Vec<Mp4AtomTruth> = atoms
                .iter()
                .map(|atom| Mp4AtomTruth {
                    kind: atom.kind,
                    items: atom.values.iter().map(mp4_item).collect(),
                    types: vec![None; atom.values.len()],
                })
                .collect();
            TagField::ALL
                .into_iter()
                .map(|field| (field, mp4_read(&atoms, field)))
                .collect()
        }
        _ => {
            return Err(Refusal::NoByteInventory {
                detail: format!("the captured tags are not {} tags", format.as_str()),
            });
        }
    };
    collect_reads(&mut document, reads);
    Ok(document)
}

fn mp4_item(value: &CapturedValue) -> Mp4Item {
    match value {
        CapturedValue::Text(text) => Mp4Item::Text(text.clone()),
        CapturedValue::Integer(value) => Mp4Item::Integer(*value),
        CapturedValue::Bool(value) => Mp4Item::Bool(*value),
        CapturedValue::Pair(number, total) => Mp4Item::Pair(*number, *total),
        CapturedValue::Freeform { mean, name, data } => Mp4Item::Freeform {
            mean: mean.clone(),
            name: name.clone(),
            data: data.clone(),
        },
        CapturedValue::Bytes(bytes) => Mp4Item::Bytes(bytes.clone()),
    }
}
