//! Task and attempt states, ported from `acquisition/status.py`.
//!
//! The persisted vocabulary mirrors the `download_tasks.status` CHECK in
//! migration 0001 exactly; `retrying` and `awaiting_review` stay SSE-only
//! signals and are never written. Terminal states have no outgoing edges:
//! a retry always spawns a fresh task carrying `retry_count + 1` while the
//! original stays terminal for audit.

/// Persisted download-task status. Wire strings match v2 byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    /// Created but never enqueued; startup re-dispatches these.
    Queued,
    /// Transfers are polling against the download client.
    Downloading,
    /// Files landed; import and verification own the task now.
    Processing,
    /// Every transfer landed and imported.
    Completed,
    /// Landed short of the full release; still counts as landed.
    Partial,
    /// Terminal failure; auto-retry may spawn a successor task.
    Failed,
    /// Cancelled by operator or request withdrawal.
    Cancelled,
}

impl TaskStatus {
    /// All seven persisted statuses in lifecycle order.
    pub const ALL: [TaskStatus; 7] = [
        TaskStatus::Queued,
        TaskStatus::Downloading,
        TaskStatus::Processing,
        TaskStatus::Completed,
        TaskStatus::Partial,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ];

    /// Wire string stored in `download_tasks.status`.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Downloading => "downloading",
            TaskStatus::Processing => "processing",
            TaskStatus::Completed => "completed",
            TaskStatus::Partial => "partial",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// Parse a status read back from SQLite.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(TaskStatus::Queued),
            "downloading" => Some(TaskStatus::Downloading),
            "processing" => Some(TaskStatus::Processing),
            "completed" => Some(TaskStatus::Completed),
            "partial" => Some(TaskStatus::Partial),
            "failed" => Some(TaskStatus::Failed),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }

    /// True once the task has reached a final state.
    pub fn is_terminal(self) -> bool {
        is_terminal(self.as_str())
    }
}

/// Attempt-journal state. Mirrors the `download_attempts.state` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptState {
    /// Candidate handed off; the client has not confirmed materialization.
    Acquiring,
    /// Source files exist and the acquisition still needs them.
    InUse,
    /// Terminal result settled; source cleanup is owed.
    CleanupPending,
    /// Workspace gone; client-side records may remain.
    WorkspaceRemoved,
    /// Cleanup debt fully paid.
    Complete,
    /// Kept deliberately (held for review, or preserved on failure).
    Preserved,
    /// Needs an operator: unsafe to clean automatically.
    NeedsAttention,
}

impl AttemptState {
    /// Wire string stored in `download_attempts.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Acquiring => "acquiring",
            AttemptState::InUse => "in_use",
            AttemptState::CleanupPending => "cleanup_pending",
            AttemptState::WorkspaceRemoved => "workspace_removed",
            AttemptState::Complete => "complete",
            AttemptState::Preserved => "preserved",
            AttemptState::NeedsAttention => "needs_attention",
        }
    }

    /// Parse a state read back from SQLite.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "acquiring" => Some(AttemptState::Acquiring),
            "in_use" => Some(AttemptState::InUse),
            "cleanup_pending" => Some(AttemptState::CleanupPending),
            "workspace_removed" => Some(AttemptState::WorkspaceRemoved),
            "complete" => Some(AttemptState::Complete),
            "preserved" => Some(AttemptState::Preserved),
            "needs_attention" => Some(AttemptState::NeedsAttention),
            _ => None,
        }
    }
}

/// True for `completed`, `partial`, `failed`, `cancelled`.
///
/// Accepts the wire string so bare database values work without parsing,
/// mirroring v2's `is_terminal`, which takes either form.
pub fn is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "partial" | "failed" | "cancelled")
}

/// Whether `from -> to` is a defined transition.
///
/// Advisory only, exactly like v2: the store never rejects a write for a
/// missing edge, because a wrongly narrow map would block a legitimate
/// flow. Tests and reviewers use this to assert legality instead.
pub fn can_transition(from: TaskStatus, to: TaskStatus) -> bool {
    match from {
        TaskStatus::Queued => matches!(
            to,
            TaskStatus::Downloading | TaskStatus::Failed | TaskStatus::Cancelled
        ),
        // Downloading/Processing fall back to Queued when the poll pass
        // finds no pollable attempt or fails an attempt over; Queued fails
        // directly when no source can serve the release.
        TaskStatus::Downloading => matches!(
            to,
            TaskStatus::Queued
                | TaskStatus::Processing
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        ),
        TaskStatus::Processing => matches!(
            to,
            TaskStatus::Queued
                | TaskStatus::Completed
                | TaskStatus::Partial
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        ),
        // Terminal states have no outgoing transitions; a retry creates a
        // new task. (The transient SSE-only `retrying` / `awaiting_review`
        // signals are not representable here by design.)
        TaskStatus::Completed
        | TaskStatus::Partial
        | TaskStatus::Failed
        | TaskStatus::Cancelled => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_have_no_outgoing_edges() {
        for from in TaskStatus::ALL {
            if from.is_terminal() {
                for to in TaskStatus::ALL {
                    assert!(!can_transition(from, to), "{from:?} -> {to:?}");
                }
            }
        }
    }

    #[test]
    fn happy_path_edges_exist() {
        assert!(can_transition(TaskStatus::Queued, TaskStatus::Downloading));
        assert!(can_transition(
            TaskStatus::Downloading,
            TaskStatus::Processing
        ));
        assert!(can_transition(
            TaskStatus::Processing,
            TaskStatus::Completed
        ));
    }
}
