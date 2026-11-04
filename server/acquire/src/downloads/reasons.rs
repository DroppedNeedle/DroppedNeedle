//! Why a download sits where it does.
//!
//! Each task in the queue view carries at most one reason. The import's own
//! decision comes first; otherwise the worker's last outcome on the task,
//! stored as a typed code beside its log text, says it. The queue never
//! shows internal error text.

use super::landing_rows::ImportOutcome;
use super::state::TaskStatus;
use crate::reason::{Reason, ReasonKind};

/// What a task that was moved on to its next source by hand logs as its
/// last outcome.
pub const NEXT_SOURCE_NOTE: &str = "moved to the next source at your request";

/// The newest import decision, as far as the reason needs it.
#[derive(Debug, Clone, Default)]
pub struct DecisionReason {
    pub outcome: ImportOutcome,
    pub code: Option<Reason>,
}

/// The one reason to show for a task, if it needs one. Live tasks have
/// none (their progress speaks), except a task moved on by hand and one
/// whose queued copy gave way to the next. A cancelled task shows a
/// settled reason it was stopped for, else plain cancelled.
pub fn task_reason(
    status: TaskStatus,
    outcome: Option<Reason>,
    held_for_review: bool,
    decision: Option<&DecisionReason>,
) -> Option<Reason> {
    if status == TaskStatus::Completed {
        return None;
    }
    if status == TaskStatus::Cancelled {
        // A task the worker stopped on its own keeps the settled reason it
        // stopped for; one a person stopped reads as cancelled.
        let settled = outcome.filter(|reason| reason.kind() == ReasonKind::Settled);
        return Some(settled.unwrap_or(Reason::Cancelled));
    }
    let settled = status.is_terminal();
    if let Some(decision) = decision
        && (held_for_review || (settled && decision.outcome != ImportOutcome::Imported))
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

    /// The worker's outcome explains a settled task, a cancelled task shows
    /// only a settled reason it stopped for, and the import decision wins.
    #[test]
    fn task_reason_table() {
        let held = DecisionReason {
            outcome: ImportOutcome::Held,
            code: Some(Reason::WeakMatch),
        };
        let cases = [
            (
                "failed with an outcome",
                TaskStatus::Failed,
                Some(Reason::Stalled),
                false,
                None,
                Some(Reason::Stalled),
            ),
            (
                "failed without one",
                TaskStatus::Failed,
                None,
                false,
                None,
                Some(Reason::DownloadFailed),
            ),
            (
                "still downloading",
                TaskStatus::Downloading,
                Some(Reason::Stalled),
                false,
                None,
                None,
            ),
            (
                "moved on by hand",
                TaskStatus::Queued,
                Some(Reason::MovedOn),
                false,
                None,
                Some(Reason::MovedOn),
            ),
            (
                "cancelled for a settled reason",
                TaskStatus::Cancelled,
                Some(Reason::NoBetterCopy),
                false,
                None,
                Some(Reason::NoBetterCopy),
            ),
            (
                "cancelled after moving on",
                TaskStatus::Cancelled,
                Some(Reason::MovedOn),
                false,
                None,
                Some(Reason::Cancelled),
            ),
            (
                "cancelled while stalled",
                TaskStatus::Cancelled,
                Some(Reason::Stalled),
                false,
                None,
                Some(Reason::Cancelled),
            ),
            (
                "held by the import",
                TaskStatus::Failed,
                Some(Reason::WeakMatch),
                true,
                Some(&held),
                Some(Reason::WeakMatch),
            ),
        ];
        for (label, status, outcome, held_for_review, decision, expected) in cases {
            assert_eq!(
                task_reason(status, outcome, held_for_review, decision),
                expected,
                "{label}"
            );
        }
    }
}
