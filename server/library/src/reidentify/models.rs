//! The candidates a re-identification offers and what its evaluation found.

use serde::{Deserialize, Serialize};

use crate::identify::models::{CandidateEvidence, EvidenceClass, IdentificationOutcome};

/// Where one local file lands on a candidate release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateTrack {
    pub local_track_id: String,
    pub title: Option<String>,
    pub disc_number: Option<u32>,
    pub position: Option<u32>,
}

/// One candidate release a re-identification offers, with what the
/// review panel shows about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReidentificationCandidate {
    pub candidate_key: String,
    /// The matcher would have accepted this release on its own.
    pub automatic_safe: bool,
    pub evidence: CandidateEvidence,
    pub artist_mbid: Option<String>,
    pub release_type: Option<String>,
    pub release_date: Option<String>,
    pub local_album_title: String,
    pub local_album_artist_name: String,
    pub album_title_classification: EvidenceClass,
    pub album_artist_classification: EvidenceClass,
    pub tracks: Vec<CandidateTrack>,
    /// Release track titles no local file took.
    pub unmatched_expected_tracks: Vec<String>,
    /// The same release tracks by id, in the same order. A candidate
    /// stored before they were kept has none.
    #[serde(default)]
    pub unmatched_expected_track_ids: Vec<String>,
}

/// What a re-identification evaluation found, kept on the job snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub outcome: IdentificationOutcome,
    pub reason_code: String,
    pub candidates: Vec<ReidentificationCandidate>,
    #[serde(default)]
    pub selected_candidate_key: Option<String>,
    #[serde(default)]
    pub custom_manifest_id: Option<String>,
}
