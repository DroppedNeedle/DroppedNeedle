//! AcoustID wire shapes, read by the lookups in `acoustid.rs`.
//!
//! Tolerant of unknown fields: serde skips what is not declared. Identity is
//! required: a result, a recording or a release group without its id, or with
//! a blank one, fails decoding as a payload error. A recording's title,
//! duration and artist names read as absent when the answer sends another
//! JSON type there, as v2 read them. Every other field is an `Option`, or a
//! default that cannot pass for a real answer, except an answer's `status`,
//! which must be present.
//!
//! Seen on 2026-10-29 (`tests/fixtures/acoustid/`):
//!
//! - A recording's `duration` is a float on the single lookup (`219.0`), so
//!   it is read as a number and cut to whole seconds.
//! - A result may carry no release groups of its own; the recording's list
//!   has them (`single_lookup.json`).
//! - No match is `{"results": [], "status": "ok"}` (`single_no_match.json`).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

/// Reads an identity: a string that is not blank once trimmed. A missing,
/// null or blank one fails decoding.
fn identity<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    let id = raw.trim();
    if id.is_empty() {
        return Err(D::Error::custom("an identity is blank"));
    }
    Ok(id.to_owned())
}

/// Reads a display field: the value when it has the type the field needs,
/// `None` when it has another. v2 read such a field as absent.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).ok())
}

/// The single-print answer: the status, then the matches, best first.
///
/// Verified against api.acoustid.org/v2/lookup on 2026-10-29: tests/fixtures/acoustid/single_lookup.json.
/// Verified against api.acoustid.org/v2/lookup on 2026-10-29: tests/fixtures/acoustid/single_no_match.json.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LookupAnswer {
    /// `ok` on success. Anything else is malformed.
    pub status: String,
    /// The matches, best first. Absent or `null` is no match.
    #[serde(default)]
    pub results: Option<Vec<LookupResult>>,
}

/// One AcoustID track that matched the print.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LookupResult {
    /// AcoustID's track id. The client does not read it; decoding still requires it.
    #[allow(dead_code)]
    #[serde(deserialize_with = "identity")]
    pub id: String,
    /// The match score. Absent reads as 0, below the match threshold.
    #[serde(default)]
    pub score: Option<f64>,
    /// The recordings linked to the track.
    #[serde(default)]
    pub recordings: Option<Vec<LookupRecording>>,
    /// Release groups listed for the track itself.
    #[serde(default)]
    pub releasegroups: Option<Vec<ReleaseGroupRef>>,
}

/// A MusicBrainz recording linked to the track.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LookupRecording {
    /// The MusicBrainz recording id. Required.
    #[serde(deserialize_with = "identity")]
    pub id: String,
    /// The recording title.
    #[serde(default, deserialize_with = "lenient")]
    pub title: Option<String>,
    /// The length in seconds, as sent (a float on the single lookup).
    #[serde(default, deserialize_with = "lenient")]
    pub duration: Option<f64>,
    /// The artist credits, in order.
    #[serde(default, deserialize_with = "lenient")]
    pub artists: Option<Vec<ArtistCredit>>,
    /// Release groups the recording appears in.
    #[serde(default)]
    pub releasegroups: Option<Vec<ReleaseGroupRef>>,
}

/// One artist credit. Only the name is read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArtistCredit {
    /// The name as credited.
    #[serde(default, deserialize_with = "lenient")]
    pub name: Option<String>,
}

/// A MusicBrainz release group. The release-group check compares its id.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReleaseGroupRef {
    /// The MusicBrainz release-group id. Required.
    #[serde(deserialize_with = "identity")]
    pub id: String,
}

/// The fields of any AcoustID answer that the status check reads.
///
/// Seen on 2026-10-15 per tests/fixtures/acoustid/README.md: `invalid_key.json`,
/// a bad key's HTTP 400 with error code 4.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    /// `ok` on a usable answer.
    #[serde(default)]
    pub status: Option<String>,
    /// The error object, on a refusal.
    #[serde(default)]
    pub error: Option<Refusal>,
}

/// AcoustID's error object. Code 4 is an unknown key.
#[derive(Debug, Clone, Deserialize)]
pub struct Refusal {
    /// AcoustID's error code.
    #[serde(default)]
    pub code: Option<i64>,
}

// The batch answer's shape, seen on 2026-10-15 per tests/fixtures/acoustid/README.md
// (`batch_lookup.json`, a batch of one print): `{"status": "ok", "fingerprints":
// [{"index": 0, "results": [{"id", "score", "recordings": [{"id", "releases":
// [{"id"}]}]}]}]}`.

/// A batch answer.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchAnswer {
    pub status: String,
    #[serde(default)]
    pub fingerprints: Vec<BatchEntry>,
}

/// The results for one query of a batch.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchEntry {
    pub index: BatchIndex,
    #[serde(default)]
    pub results: Option<Vec<BatchResult>>,
}

/// The query index, which AcoustID echoes as a number or a numeric
/// string (seen in v2's answers; the 2026-10-15 probe had a number).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BatchIndex {
    Number(u64),
    Text(String),
}

impl BatchIndex {
    /// The query's position in the batch, when AcoustID gave a usable one.
    pub fn value(&self) -> Option<usize> {
        match self {
            BatchIndex::Number(number) => usize::try_from(*number).ok(),
            BatchIndex::Text(text) => text.trim().parse().ok(),
        }
    }
}

/// One AcoustID track that matched a print.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BatchResult {
    /// AcoustID's own id for the track.
    pub id: String,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub recordings: Option<Vec<BatchRecording>>,
}

impl BatchResult {
    /// The score, 0 when AcoustID gave none.
    pub fn score(&self) -> f64 {
        self.score.unwrap_or(0.0)
    }

    /// The MusicBrainz recordings AcoustID links to the track.
    pub fn recordings(&self) -> &[BatchRecording] {
        self.recordings.as_deref().unwrap_or_default()
    }
}

/// A MusicBrainz recording linked to an AcoustID track.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BatchRecording {
    pub id: String,
    #[serde(default)]
    pub releases: Option<Vec<BatchRelease>>,
}

/// A MusicBrainz release a recording appears on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BatchRelease {
    pub id: String,
}
