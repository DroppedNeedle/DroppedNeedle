//! Why a download sits where it does.
//!
//! Each task in the queue view carries at most one reason. The import's own
//! decision comes first; otherwise the worker's last outcome on the task,
//! stored as a typed code beside its log text, says it. The queue never
//! shows internal error text.

use super::state::TaskStatus;
use crate::reason::Reason;

/// What a task that was moved on to its next source by hand logs as its
/// last outcome.
pub const NEXT_SOURCE_NOTE: &str = "moved to the next source at your request";

/// The newest import decision, as far as the reason needs it.
#[derive(Debug, Clone, Default)]
pub struct DecisionReason<'a> {
    pub outcome: &'a str,
    pub code: Option<Reason>,
}

/// The one reason to show for a task, if it needs one. Live tasks have
/// none (their progress speaks), except a task moved on by hand and one
/// whose queued copy gave way to the next.
pub fn task_reason(
    status: TaskStatus,
    outcome: Option<Reason>,
    held_for_review: bool,
    decision: Option<&DecisionReason<'_>>,
) -> Option<Reason> {
    if status == TaskStatus::Completed {
        return None;
    }
    if status == TaskStatus::Cancelled {
        return Some(Reason::Cancelled);
    }
    let settled = status.is_terminal();
    if let Some(decision) = decision
        && (held_for_review || (settled && decision.outcome != "imported"))
        && let Some(code) = decision.code
    {
        return Some(code);
    }
    if held_for_review {
        return Some(Reason::HeldForReview);
    }
    // The outcome stays on the row after the next source starts; it only
    // explains the wait before that, or the copy a slow peer gave way to.
    let moved_on = (status == TaskStatus::Queued && outcome == Some(Reason::MovedOn))
        || (status == TaskStatus::Downloading && outcome == Some(Reason::QueueGaveWay));
    if !(settled || moved_on) {
        return None;
    }
    Some(outcome.unwrap_or(Reason::DownloadFailed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_outcome_explains_a_settled_task() {
        let stalled = task_reason(TaskStatus::Failed, Some(Reason::Stalled), false, None);
        assert_eq!(stalled, Some(Reason::Stalled));
        let unknown = task_reason(TaskStatus::Failed, None, false, None);
        assert_eq!(unknown, Some(Reason::DownloadFailed));
        assert!(task_reason(TaskStatus::Downloading, Some(Reason::Stalled), false, None).is_none());
        let moved = task_reason(TaskStatus::Queued, Some(Reason::MovedOn), false, None);
        assert_eq!(moved, Some(Reason::MovedOn));
    }

    #[test]
    fn the_import_decision_wins() {
        let decision = DecisionReason {
            outcome: "held",
            code: Some(Reason::WeakMatch),
        };
        let reason = task_reason(
            TaskStatus::Failed,
            Some(Reason::WeakMatch),
            true,
            Some(&decision),
        );
        assert_eq!(reason, Some(Reason::WeakMatch));
    }
}
