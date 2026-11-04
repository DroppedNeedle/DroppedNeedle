//! AcoustID wire shapes, read by the batch lookup in `acoustid.rs`.
//!
//! Tolerant of unknown fields: serde skips what is not declared. Identity is
//! required: a result, a recording or a release without its id fails
//! decoding. Every other field is an `Option`, or a default that cannot pass
//! for a real answer, except an answer's `status`, which must be present.

use serde::Deserialize;

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
