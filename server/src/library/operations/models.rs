//! Operation job types: what `library_operation_jobs` rows hold, the
//! controls an administrator sends, and the re-identification candidates a
//! job offers.

use serde::{Deserialize, Serialize};

use super::reasons::Reason;
use crate::library::identify::models::{CandidateEvidence, EvidenceClass};

/// What kind of work a job carries. Only explicit re-identification has a
/// worker today; the other kinds come from v2 data and still answer to
/// reads and controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    BulkReviewApply,
    Repair,
    ExplicitReidentification,
    LibraryManagement,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BulkReviewApply => "bulk_review_apply",
            Self::Repair => "repair",
            Self::ExplicitReidentification => "explicit_reidentification",
            Self::LibraryManagement => "library_management",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "bulk_review_apply" => Some(Self::BulkReviewApply),
            "repair" => Some(Self::Repair),
            "explicit_reidentification" => Some(Self::ExplicitReidentification),
            "library_management" => Some(Self::LibraryManagement),
            _ => None,
        }
    }
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Queued,
    Running,
    Paused,
    /// Evaluated and waiting on an administrator's choice.
    Ready,
    Succeeded,
    Failed,
    Cancelled,
    Stopped,
}

impl OperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Ready => "ready",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Stopped => "stopped",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "paused" => Some(Self::Paused),
            "ready" => Some(Self::Ready),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// A control the running worker has not acted on yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRequest {
    None,
    Pause,
    Stop,
}

impl ControlRequest {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pause => "pause",
            Self::Stop => "stop",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "pause" => Self::Pause,
            "stop" => Self::Stop,
            _ => Self::None,
        }
    }
}

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

/// One `library_operation_jobs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationJob {
    pub id: String,
    /// The stored kind, kept as text so a row from a newer kind still reads.
    pub kind: String,
    pub state: OperationState,
    pub requested_by_user_id: Option<String>,
    pub expected_work_count: i64,
    pub completed_count: i64,
    pub succeeded_count: i64,
    pub failed_count: i64,
    pub skipped_count: i64,
    pub control_request: ControlRequest,
    pub terminal_code: Option<String>,
    pub reidentification_attempt_count: i64,
    pub row_revision: i64,
    pub event_revision: i64,
    pub created_at: f64,
    pub updated_at: f64,
}

impl OperationJob {
    pub fn is_reidentification(&self) -> bool {
        self.kind == OperationKind::ExplicitReidentification.as_str()
    }
}

/// One work item's outcome, as the job detail lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkResult {
    pub ordinal: i64,
    pub local_album_id: Option<String>,
    pub local_track_id: Option<String>,
    pub action: String,
    pub state: String,
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

/// How an administrator settles a re-identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    /// Accept the candidate's exact release; every file must map to it.
    ExactRelease,
    /// Keep the release group and seal the files as they are.
    CustomEdition,
    /// Keep the files out of Library Management.
    LeaveUnmanaged,
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

/// Start an explicit re-identification.
#[derive(Debug, Clone, Default)]
pub struct ReidentifyInput {
    pub expected_album_revision: Option<i64>,
    pub expected_input_revision: Option<String>,
    pub idempotency_key: Option<String>,
    pub one_off_local_metadata: bool,
    pub release_mbid: Option<String>,
}

/// Settle a re-identification.
#[derive(Debug, Clone)]
pub struct CandidateChoice {
    pub expected_row_revision: i64,
    pub candidate_key: String,
    pub confirmation: bool,
    pub decision_mode: DecisionMode,
}

/// The result of undoing an automatic edition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    pub local_album_id: String,
    /// `restored` or `cleared_to_review`.
    pub outcome: String,
    pub review_id: Option<String>,
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
    pub page: crate::library::identify::sources::EditionPage,
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
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(reason) | Self::Invalid(reason) | Self::Conflict(reason) => {
                write!(f, "{}: {}", reason.code, reason.message)
            }
            Self::Unavailable(cause) | Self::Store(cause) => write!(f, "{cause}"),
        }
    }
}

impl std::error::Error for OperationError {}

impl From<rusqlite::Error> for OperationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}
