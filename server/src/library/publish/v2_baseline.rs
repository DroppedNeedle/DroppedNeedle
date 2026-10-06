//! v2 original-file baselines in v3's form.
//!
//! v2 recorded the state a file had before its first managed write as a
//! JSON tag snapshot (`SemanticTagSnapshot`, written with msgspec): the
//! file's native tags exactly as they were (the whole ID3 tag for MP3,
//! the Vorbis comments for FLAC, Ogg and Opus, the `ilst` items for M4A),
//! plus its artwork and technical facts. v3's baseline is a
//! [`BeforeState`] around a [`TagDocument`].
//!
//! The translation runs v2's native tags through the same readers v3 uses
//! on a file ([`document_from_captured`]), so the document is exactly the
//! one v3 would have recorded had it read the original file itself, and a
//! restore puts every field v3 writes back the way it was, including
//! removing fields v2 added later. The original place is v2's original
//! root and relative path; root ids carry over from v2 unchanged.
//!
//! What v3's restore does not write (embedded artwork, tags outside the
//! fields it manages) stays in v2's snapshot, which the import keeps
//! byte for byte beside the translated baseline.

use base64::Engine as _;
use serde::Deserialize;
use thiserror::Error;

use super::staging::document_from_fields;
use super::undo::BeforeState;
use crate::library::tags::{
    AudioFormat, CapturedAtom, CapturedTags, CapturedValue, Refusal, document_from_captured,
};

/// The only snapshot shape v2 ever wrote.
const SNAPSHOT_VERSION: u64 = 1;

/// Why one v2 baseline cannot become a v3 baseline. Messages name the
/// problem, never tag values.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum V2BaselineError {
    /// The snapshot is not v2's snapshot JSON.
    #[error("the tag snapshot cannot be read: {0}")]
    Unreadable(String),
    /// A snapshot shape this version does not know.
    #[error("tag snapshot version {0} is unknown")]
    Version(u64),
    /// A container v3 does not handle (WMA).
    #[error("v3 does not handle {0} files")]
    Format(String),
    /// The native tags do not read back.
    #[error("the original tags do not read back: {0}")]
    Tags(Refusal),
}

#[derive(Deserialize)]
struct Snapshot {
    snapshot_version: u64,
    probe: Probe,
    native_tags: NativeTags,
}

#[derive(Deserialize)]
struct Probe {
    detected_format: Option<String>,
}

#[derive(Deserialize)]
struct NativeTags {
    #[serde(default)]
    entries: Vec<Entry>,
    #[serde(default)]
    encoded_id3: Option<String>,
}

#[derive(Deserialize)]
struct Entry {
    key: String,
    #[serde(default)]
    values: Vec<Value>,
}

#[derive(Deserialize)]
struct Value {
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    integer: Option<i64>,
    #[serde(default)]
    boolean: Option<bool>,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    integer_pair: Vec<i64>,
}

fn unreadable(detail: impl std::fmt::Display) -> V2BaselineError {
    V2BaselineError::Unreadable(detail.to_string())
}

/// msgspec writes bytes as standard base64.
fn decode_bytes(text: &str) -> Result<Vec<u8>, V2BaselineError> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(unreadable)
}

fn container(name: &str) -> Result<AudioFormat, V2BaselineError> {
    match name.to_ascii_lowercase().as_str() {
        "flac" => Ok(AudioFormat::Flac),
        "mp3" => Ok(AudioFormat::Mp3),
        "ogg" => Ok(AudioFormat::Ogg),
        "opus" => Ok(AudioFormat::Opus),
        "m4a" => Ok(AudioFormat::M4a),
        "aac" => Ok(AudioFormat::Aac),
        "wav" => Ok(AudioFormat::Wav),
        other => Err(V2BaselineError::Format(other.to_owned())),
    }
}

/// Vorbis comments: one pair per value. v2 read the keys through mutagen,
/// which lowercases them; Vorbis keys are case-insensitive.
fn vorbis_pairs(entries: &[Entry]) -> Result<Vec<(String, String)>, V2BaselineError> {
    let mut pairs = Vec::new();
    for entry in entries {
        for value in &entry.values {
            match (value.kind.as_str(), &value.text) {
                ("text", Some(text)) => pairs.push((entry.key.to_ascii_uppercase(), text.clone())),
                _ => {
                    return Err(unreadable(format!(
                        "comment {} holds a {} value",
                        entry.key, value.kind
                    )));
                }
            }
        }
    }
    Ok(pairs)
}

/// An MP4 item kind: the key's four characters as Latin-1 bytes (mutagen
/// spells `\xa9nam` as `©nam`).
fn atom_kind(key: &str) -> Option<[u8; 4]> {
    let bytes: Vec<u8> = key
        .chars()
        .map(|ch| u8::try_from(u32::from(ch)).ok())
        .collect::<Option<_>>()?;
    bytes.try_into().ok()
}

fn mp4_value(value: &Value) -> Result<CapturedValue, V2BaselineError> {
    let bad = || unreadable(format!("an MP4 {} value is incomplete", value.kind));
    Ok(match value.kind.as_str() {
        "text" => CapturedValue::Text(value.text.clone().ok_or_else(bad)?),
        "integer" => CapturedValue::Integer(
            u64::try_from(value.integer.ok_or_else(bad)?).map_err(|_| bad())?,
        ),
        "boolean" => CapturedValue::Bool(value.boolean.ok_or_else(bad)?),
        "integer_pair" => {
            let half = |index: usize| {
                value
                    .integer_pair
                    .get(index)
                    .map_or(Ok(0), |part| u32::try_from(*part))
                    .map_err(|_| bad())
            };
            CapturedValue::Pair(half(0)?, half(1)?)
        }
        "binary" => CapturedValue::Bytes(decode_bytes(value.binary.as_deref().ok_or_else(bad)?)?),
        _ => CapturedValue::Bytes(value.text.clone().unwrap_or_default().into_bytes()),
    })
}

fn mp4_atoms(entries: &[Entry]) -> Result<Vec<CapturedAtom>, V2BaselineError> {
    let mut atoms = Vec::new();
    for entry in entries {
        if let Some(rest) = entry.key.strip_prefix("----:") {
            let (mean, name) = rest
                .split_once(':')
                .ok_or_else(|| unreadable(format!("freeform {} has no name", entry.key)))?;
            let mut values = Vec::new();
            for value in &entry.values {
                let data = match (&value.binary, &value.text) {
                    (Some(binary), _) => decode_bytes(binary)?,
                    (None, Some(text)) => text.clone().into_bytes(),
                    (None, None) => return Err(unreadable("a freeform value is empty")),
                };
                values.push(CapturedValue::Freeform {
                    mean: mean.to_owned(),
                    name: name.to_owned(),
                    data,
                });
            }
            atoms.push(CapturedAtom {
                kind: *b"----",
                values,
            });
            continue;
        }
        let kind = atom_kind(&entry.key)
            .ok_or_else(|| unreadable(format!("MP4 item {} is not a four-byte kind", entry.key)))?;
        let values = entry
            .values
            .iter()
            .map(mp4_value)
            .collect::<Result<_, _>>()?;
        atoms.push(CapturedAtom { kind, values });
    }
    Ok(atoms)
}

/// Translate one v2 baseline: its tag snapshot blob, the container v2
/// recorded for it, and the place the file had before v2 first managed it.
pub fn translate(
    snapshot_json: &[u8],
    format: &str,
    original_root: &str,
    original_rel: &str,
) -> Result<BeforeState, V2BaselineError> {
    let snapshot: Snapshot = serde_json::from_slice(snapshot_json).map_err(unreadable)?;
    if snapshot.snapshot_version != SNAPSHOT_VERSION {
        return Err(V2BaselineError::Version(snapshot.snapshot_version));
    }
    let format = container(snapshot.probe.detected_format.as_deref().unwrap_or(format))?;
    let native = &snapshot.native_tags;
    let captured = match format {
        AudioFormat::Mp3 => CapturedTags::Id3(match &native.encoded_id3 {
            Some(encoded) => decode_bytes(encoded)?,
            None => Vec::new(),
        }),
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => {
            CapturedTags::Vorbis(vorbis_pairs(&native.entries)?)
        }
        AudioFormat::M4a => CapturedTags::Mp4(mp4_atoms(&native.entries)?),
        // v3 writes no tags to these, so their documents stay empty, the
        // same as v3's own baselines of such files.
        AudioFormat::Aac | AudioFormat::Wav => CapturedTags::Id3(Vec::new()),
    };
    let fields = document_from_captured(format, &captured).map_err(V2BaselineError::Tags)?;
    Ok(BeforeState {
        doc: document_from_fields(fields),
        source_root: original_root.to_owned(),
        source_rel: original_rel.to_owned(),
        // v2 kept the original's stat and tag revisions, not a content
        // hash; restore never reads this for a baseline.
        source_sha256: String::new(),
        mgmt_state_before: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A FLAC snapshot as v2 wrote it (mutagen's lowercase keys, both
    /// track-total spellings) reads as v3 reads the same file.
    #[test]
    fn vorbis_snapshot_reads_like_the_file() {
        let json = br#"{"snapshot_version":1,"adapter_version":"1",
            "probe":{"extension":".flac","admitted":true,"detected_format":"flac",
                     "detected_class":"FLAC","extension_matches":true},
            "native_tags":{"storage_kind":"vorbis_comments","entries":[
                {"key":"artist","values":[{"kind":"text","text":"A"},{"kind":"text","text":"B"}],
                 "container":"list"},
                {"key":"title","values":[{"kind":"text","text":"Song"}],"container":"list"},
                {"key":"totaltracks","values":[{"kind":"text","text":"9"}],"container":"list"},
                {"key":"tracktotal","values":[{"kind":"text","text":"09"}],"container":"list"}
            ]}}"#;
        let before = translate(json, "flac", "music", "In/song.flac").expect("translates");
        let managed = &before.doc.managed;
        assert_eq!(managed["artist"], vec!["A".to_owned(), "B".to_owned()]);
        assert_eq!(managed["title"], vec!["Song".to_owned()]);
        assert_eq!(managed["total_tracks:TOTALTRACKS"], vec!["9".to_owned()]);
        assert_eq!(managed["total_tracks:TRACKTOTAL"], vec!["09".to_owned()]);
        assert_eq!(
            before.doc.version,
            crate::library::publish::tags_seam::TAG_DOCUMENT_VERSION
        );
        assert_eq!(
            (before.source_root.as_str(), before.source_rel.as_str()),
            ("music", "In/song.flac")
        );
    }

    /// MP4 items keep their kinds (`©nam` is byte 0xA9), pairs and
    /// freeforms; a WMA snapshot is refused by name.
    #[test]
    fn mp4_snapshot_and_wma_refusal() {
        let json = br#"{"snapshot_version":1,"probe":{"detected_format":"m4a"},
            "native_tags":{"storage_kind":"mp4_atoms","entries":[
                {"key":"\u00a9nam","values":[{"kind":"text","text":"Song"}]},
                {"key":"trkn","values":[{"kind":"integer_pair","integer_pair":[3,12]}]},
                {"key":"----:com.apple.iTunes:MusicBrainz Album Id",
                 "values":[{"kind":"binary","binary":"cmVsLTE="}]}
            ]}}"#;
        let before = translate(json, "m4a", "music", "a.m4a").expect("translates");
        let managed = &before.doc.managed;
        assert_eq!(managed["title"], vec!["Song".to_owned()]);
        assert_eq!(managed["track_number"], vec!["3".to_owned()]);
        assert_eq!(managed["total_tracks"], vec!["12".to_owned()]);
        assert_eq!(managed["musicbrainz_release_id"], vec!["rel-1".to_owned()]);

        let wma = br#"{"snapshot_version":1,"probe":{"detected_format":"wma"},
            "native_tags":{"storage_kind":"asf_attributes","entries":[]}}"#;
        assert_eq!(
            translate(wma, "wma", "music", "a.wma").map(|_| ()),
            Err(V2BaselineError::Format("wma".to_owned()))
        );
    }
}
