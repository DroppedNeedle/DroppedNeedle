//! The closed sets a library operation job row holds: its kind, its
//! state, the control its worker has not seen yet, each work item's state,
//! and a management job's phase. Each converts to and from SQL in one
//! place, so no caller writes one as a literal.

use droppedneedle_kernel::spelled;
use serde::{Deserialize, Serialize};

/// What kind of work a job carries. A bulk review apply settles or
/// retries many identification reviews, one work item per review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    BulkReviewApply,
    Repair,
    ExplicitReidentification,
    LibraryManagement,
}

spelled!(OperationKind {
    BulkReviewApply => "bulk_review_apply",
    Repair => "repair",
    ExplicitReidentification => "explicit_reidentification",
    LibraryManagement => "library_management",
});

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

spelled!(OperationState {
    Queued => "queued",
    Running => "running",
    Paused => "paused",
    Ready => "ready",
    Succeeded => "succeeded",
    Failed => "failed",
    Cancelled => "cancelled",
    Stopped => "stopped",
});

/// A control the running worker has not acted on yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlRequest {
    None,
    Pause,
    Stop,
}

spelled!(ControlRequest {
    None => "none",
    Pause => "pause",
    Stop => "stop",
});

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

spelled!(WorkState {
    Pending => "pending",
    Running => "running",
    Succeeded => "succeeded",
    Failed => "failed",
    Skipped => "skipped",
});

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

spelled!(ManagementPhase {
    Planning => "planning",
    Ready => "ready",
    Applying => "applying",
    Undoing => "undoing",
    Restoring => "restoring",
    Complete => "complete",
});
