//! Why a download sits where it does, in words a person can act on.
//!
//! Each task in the queue view carries at most one reason: a stable code,
//! one plain sentence and one suggested action. The import's own decision
//! comes first (it already stores its sentence and action, see
//! `landing::reasons`); otherwise the worker's last outcome on the task is
//! translated here, so the queue never shows internal error text.

use super::state::TaskStatus;
use crate::acquire::landing::reasons::explain;
use crate::acquire::target::reasons::TrackReason;

/// What a task that was moved on to its next source by hand records as
/// its last outcome.
pub const NEXT_SOURCE_NOTE: &str = "moved to the next source at your request";

/// A reason as the queue view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueReason {
    pub code: String,
    pub text: String,
    pub action: String,
}

impl QueueReason {
    fn of(code: &str, text: &str, action: &str) -> Self {
        Self {
            code: code.to_owned(),
            text: text.to_owned(),
            action: action.to_owned(),
        }
    }
}

/// The newest import decision, as far as the reason needs it.
#[derive(Debug, Clone, Default)]
pub struct DecisionReason<'a> {
    pub outcome: &'a str,
    pub code: Option<&'a str>,
    pub text: Option<&'a str>,
    pub action: Option<&'a str>,
}

/// Worker outcomes the queue explains, matched on the start of the
/// task's last message.
const WORKER_OUTCOMES: &[(&str, &str, &str, &str)] = &[
    (
        "transfer stalled",
        "stalled",
        "The transfer stopped moving and timed out.",
        "Nothing to do: DroppedNeedle retries later, or retry it now.",
    ),
    (
        "stuck in the remote queue",
        "queue_timeout",
        "The peer kept the files in its upload queue for too long.",
        "Nothing to do: DroppedNeedle retries later, or retry it now.",
    ),
    (
        "no transfer materialized after enqueue",
        "never_started",
        "The download client accepted the files but never started the transfer.",
        "Check that the download client is running, then retry.",
    ),
    (
        "poll deadline hit",
        "deadline",
        "The download took longer than the time allowed for one source.",
        "Nothing to do: DroppedNeedle retries later, or retry it now.",
    ),
    (
        "source batch terminal",
        "source_failed",
        "The source stopped sending before every file arrived.",
        "Nothing to do: DroppedNeedle retries later, or retry it now.",
    ),
    (
        "no source could serve this release",
        "no_source",
        "No download source had this release.",
        "DroppedNeedle retries later. You can also search by hand or pick another edition.",
    ),
    (
        "imported what was found; no source had the rest",
        "tracks_missing",
        "Some tracks were imported, but no source had the rest.",
        "DroppedNeedle retries the missing tracks later. You can also search for them yourself.",
    ),
    (
        NEXT_SOURCE_NOTE,
        "moved_on",
        "You moved this download on from a slow source.",
        "Nothing to do: it starts on the next source shortly.",
    ),
];

/// The one reason to show for a task, if it needs one. Live tasks have
/// none (their progress speaks), except a task moved on by hand.
pub fn task_reason(
    status: TaskStatus,
    error_message: Option<&str>,
    held_for_review: bool,
    decision: Option<&DecisionReason<'_>>,
) -> Option<QueueReason> {
    if status == TaskStatus::Completed {
        return None;
    }
    if status == TaskStatus::Cancelled {
        return Some(QueueReason::of(
            "cancelled",
            "This download was stopped, so it will not be retried.",
            "Retry it to start again.",
        ));
    }
    let settled = status.is_terminal();
    if let Some(decision) = decision
        && (held_for_review || (settled && decision.outcome != "imported"))
        && let Some(code) = decision.code
    {
        let known = explain(code);
        return Some(QueueReason {
            code: code.to_owned(),
            text: decision.text.unwrap_or(known.message).to_owned(),
            action: decision.action.unwrap_or(known.action).to_owned(),
        });
    }
    if held_for_review {
        return Some(QueueReason::of(
            "held_for_review",
            "Some files of this download are held for you to check before they reach the library.",
            "Open the held list and import or discard them.",
        ));
    }
    let message = error_message.unwrap_or_default().trim();
    // The note stays on the row after the next source starts; it only
    // explains the wait before that.
    if !settled && !(status == TaskStatus::Queued && message.starts_with(NEXT_SOURCE_NOTE)) {
        return None;
    }
    if let Some(reason) = TrackReason::in_text(message) {
        return Some(QueueReason::of(
            reason.code(),
            reason.message(),
            reason.action(),
        ));
    }
    if let Some((_, code, text, action)) = WORKER_OUTCOMES
        .iter()
        .find(|(prefix, ..)| message.starts_with(prefix))
    {
        return Some(QueueReason::of(code, text, action));
    }
    settled.then(|| {
        QueueReason::of(
            "download_failed",
            "The download did not finish.",
            "Nothing to do: DroppedNeedle retries later, or retry it now.",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_text_never_reaches_the_view() {
        let stalled = task_reason(TaskStatus::Failed, Some("transfer stalled"), false, None);
        assert_eq!(stalled.map(|r| r.code), Some("stalled".to_owned()));
        let odd = task_reason(TaskStatus::Failed, Some("sqlite busy at 0x12"), false, None);
        assert_eq!(odd.map(|r| r.code), Some("download_failed".to_owned()));
        assert!(
            task_reason(
                TaskStatus::Downloading,
                Some("transfer stalled"),
                false,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn the_import_decision_wins() {
        let decision = DecisionReason {
            outcome: "held",
            code: Some("weak_match"),
            text: None,
            action: None,
        };
        let reason = task_reason(
            TaskStatus::Failed,
            Some("Held for review: x"),
            true,
            Some(&decision),
        );
        let reason = reason.unwrap_or_else(|| QueueReason::of("", "", ""));
        assert_eq!(reason.code, "weak_match");
        assert_eq!(reason.text, explain("weak_match").message);
    }
}
