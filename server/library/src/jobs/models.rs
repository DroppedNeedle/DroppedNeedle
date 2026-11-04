//! Operation job types: the controls an administrator sends, what a job's
//! work items report, and the error every operation request answers with.
//! The job row and its states live in [`crate::jobs`].

use crate::jobs::{OperationJob, WorkState};
use crate::reasons::Reason;
use crate::reidentify::models::ReidentificationCandidate;

/// What an administrator asks of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    Stop,
}

droppedneedle_kernel::spelled!(Control {
    Pause => "pause",
    Resume => "resume",
    Stop => "stop",
});

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
    pub bundle_id: Option<String>,
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
    /// The database was too busy to answer, a write ran past its time
    /// budget, or the database is shutting down; the cause is for the log.
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
            DbError::Busy { .. } | DbError::TxTimeout { .. } | DbError::LaneClosed => {
                Self::Busy(error.to_string())
            }
            other => Self::Store(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for OperationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}
