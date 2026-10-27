//! The closed sets a library operation job row holds: its kind, its
//! state, the control its worker has not seen yet, each work item's state,
//! and a management job's phase. Each converts to and from SQL in one
//! place, so no caller writes one as a literal.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

/// One SQL conversion for a closed set stored as text: the value is its
/// `as_str`, and text outside the set fails decoding.
macro_rules! sql_text {
    ($type:ty) => {
        impl ToSql for $type {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $type {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    FromSqlError::Other(
                        format!(concat!("not a ", stringify!($type), ": {}"), text).into(),
                    )
                })
            }
        }
    };
}

/// What kind of work a job carries. Re-identification, repairs and
/// management have workers; bulk review apply comes from v2 data and only
/// answers to reads and controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    BulkReviewApply,
    Repair,
    ExplicitReidentification,
    LibraryManagement,
}

impl OperationKind {
    pub const fn as_str(self) -> &'static str {
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

sql_text!(OperationKind);

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

sql_text!(OperationState);

/// A control the running worker has not acted on yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
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

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "none" => Some(Self::None),
            "pause" => Some(Self::Pause),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }
}

sql_text!(ControlRequest);

/// Where one work item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl WorkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

sql_text!(WorkState);

/// Where a management job is: planning its files, a preview waiting to be
/// applied, writing (applying, undoing or restoring originals), or done.
/// The worker takes a queued job in any phase but `ready` and `complete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagementPhase {
    Planning,
    Ready,
    Applying,
    Undoing,
    Restoring,
    Complete,
}

impl ManagementPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Ready => "ready",
            Self::Applying => "applying",
            Self::Undoing => "undoing",
            Self::Restoring => "restoring",
            Self::Complete => "complete",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "planning" => Some(Self::Planning),
            "ready" => Some(Self::Ready),
            "applying" => Some(Self::Applying),
            "undoing" => Some(Self::Undoing),
            "restoring" => Some(Self::Restoring),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }
}

sql_text!(ManagementPhase);
