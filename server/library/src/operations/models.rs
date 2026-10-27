//! Operation job types: the controls an administrator sends, what a job's
//! work items report, and the re-identification candidates a job offers.
//! The job row and its states live in [`crate::jobs`].

use serde::{Deserialize, Serialize};

use super::reasons::Reason;
use crate::identify::models::{CandidateEvidence, EvidenceClass};
pub use crate::identity::AutomaticUndone;
pub use crate::jobs::{ControlRequest, OperationJob, OperationKind, OperationState, WorkState};

/// What an administrator asks of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    Stop,
}

impl Control {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Stop => "stop",
        }
    }
}

/// One work item's outcome, as the job detail lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkResult {
    pub ordinal: i64,
    pub local_album_id: Option<String>,
    pub local_track_id: Option<String>,
    pub action: String,
    pub state: WorkState,
    pub failure_code: Option<String>,
    pub result: serde_json::Value,
}

/// A job with its work results and, for re-identification, the
/// candidates waiting on a choice.
#[derive(Debug, Clone)]
pub struct OperationDetail {
    pub job: OperationJob,
    pub results: Vec<WorkResult>,
    pub results_truncated: bool,
    pub candidates: Vec<ReidentificationCandidate>,
    pub selected_candidate_key: Option<String>,
}

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
}

/// What a re-identification evaluation found, kept on the job snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub outcome: String,
    pub reason_code: String,
    pub candidates: Vec<ReidentificationCandidate>,
    #[serde(default)]
    pub selected_candidate_key: Option<String>,
    #[serde(default)]
    pub custom_manifest_id: Option<String>,
}

/// The result of undoing an automatic edition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    pub local_album_id: String,
    pub outcome: AutomaticUndone,
    pub review_id: Option<String>,
}

/// One file an edition choice placed on the release: what a retag would
/// rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFile {
    pub local_track_id: String,
    pub root_id: String,
    pub relative_path: String,
}

/// What choosing an edition did to one album.
#[derive(Debug, Clone, PartialEq)]
pub struct EditionChoice {
    pub local_album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: String,
    /// Files placed on a track of the edition.
    pub placed: Vec<PlacedFile>,
    /// Files the edition has no track for. They keep their recording.
    pub extra_track_ids: Vec<String>,
    /// Titles of the edition's tracks no file holds.
    pub missing_titles: Vec<String>,
}

/// One edition-finder page with the album's current identity marked.
#[derive(Debug, Clone)]
pub struct ReleaseSearch {
    pub title_query: String,
    pub artist_query: String,
    /// Set when the page lists one release group's releases.
    pub release_group_query: Option<String>,
    pub current_release_group_mbid: Option<String>,
    pub current_release_mbid: Option<String>,
    pub page: crate::identify::sources::EditionPage,
    pub limit: u32,
}

/// Why an operation request failed. Handlers map each to a status; every
/// refusal carries its [`Reason`]: a code, a sentence, and what to do.
#[derive(Debug)]
pub enum OperationError {
    NotFound(Reason),
    /// Bad input, or a choice these files cannot take.
    Invalid(Reason),
    /// A valid request against the wrong state, or against a job or album
    /// that moved since the caller read it.
    Conflict(Reason),
    /// MusicBrainz could not answer; the cause is for the log.
    Unavailable(String),
    /// A store fault; the cause is for the log.
    Store(String),
    /// The database was too busy to answer, or is shutting down; the
    /// cause is for the log.
    Busy(String),
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(reason) | Self::Invalid(reason) | Self::Conflict(reason) => {
                write!(f, "{}: {}", reason.code, reason.message)
            }
            Self::Unavailable(cause) | Self::Store(cause) | Self::Busy(cause) => {
                write!(f, "{cause}")
            }
        }
    }
}

impl std::error::Error for OperationError {}

impl From<droppedneedle_kernel::db::DbError> for OperationError {
    fn from(error: droppedneedle_kernel::db::DbError) -> Self {
        use droppedneedle_kernel::db::DbError;
        match error {
            DbError::Busy { .. } | DbError::LaneClosed => Self::Busy(error.to_string()),
            other => Self::Store(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for OperationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}
