//! Startup recovery and orchestrator failover.
//!
//! Classifies the journal's rows at startup: queued tasks re-dispatch (they
//! never started, so failing them would be spurious); a task that was landing
//! resumes its landing from the files its attempt names; a task with a live
//! attempt whose client handle is journaled resumes polling the transfers the
//! client kept across the restart; anything else restarts cleanly from scratch.
//! Recovery never blocks startup and never repeats a fetch destructively:
//! resume re-correlates before polling, and restart reuses the idempotency key
//! so a double resume cannot double-enqueue.
//!
//! Failover between orchestrator instances rides the attempt-lease
//! mechanism in [`super::store`]: a successor claims only rows whose
//! lease lapsed, so the old owner's in-flight work is never disturbed.

use super::state::{AttemptState, TaskStatus};
use super::store::AttemptRow;

/// What startup recovery should do with one active task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    /// Created but never enqueued: dispatch it now.
    Redispatch,
    /// A landing was running: it resumes on the first poll pass from the
    /// files its attempt names. Never a fresh download.
    ResumeLanding,
    /// A live attempt with a journaled handle: poll the client's kept
    /// transfers instead of failing them (the old "transfer lost during
    /// restart" bug); a genuinely dead transfer ages out via the stall
    /// watchdog and failover re-pulls from another peer.
    ResumePoll,
    /// No live attempt the client can be asked about: run the task from
    /// scratch under the same idempotency key.
    RestartClean,
    /// Already settled: leave the row alone. Terminal tasks never reach
    /// the classifier (the caller only feeds active rows); this arm keeps
    /// a misfed row a no-op instead of resurrecting it.
    Noop,
}

/// Classify one task for startup recovery from its row's status and its
/// attempt rows.
///
/// The caller only feeds active rows; a terminal row fed by mistake maps
/// to [`StartupAction::Noop`] so recovery never resurrects a task that
/// already settled (the store guard would refuse the write anyway).
pub fn classify_startup(status: TaskStatus, attempts: &[AttemptRow]) -> StartupAction {
    match status {
        TaskStatus::Queued => StartupAction::Redispatch,
        TaskStatus::Processing
            if attempts.iter().any(|attempt| {
                matches!(
                    attempt.state,
                    AttemptState::Acquiring
                        | AttemptState::InUse
                        | AttemptState::Preserved
                        | AttemptState::CleanupPending
                )
            }) =>
        {
            StartupAction::ResumeLanding
        }
        TaskStatus::Downloading | TaskStatus::Processing => {
            let live = attempts.iter().any(|attempt| {
                attempt.handle_journaled
                    && matches!(attempt.state, AttemptState::Acquiring | AttemptState::InUse)
            });
            if live {
                StartupAction::ResumePoll
            } else {
                StartupAction::RestartClean
            }
        }
        TaskStatus::Completed
        | TaskStatus::Partial
        | TaskStatus::Failed
        | TaskStatus::Cancelled => StartupAction::Noop,
    }
}

/// Planned successor for a terminal task: the original stays terminal for audit
/// while a fresh queued task carries `retry_count + 1`. An upgrade's retry
/// stays an upgrade (keeps the origin-aware gate); everything else becomes
/// `retry` so quota counts ignore it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrySpawn {
    /// Successor task id.
    pub task_id: String,
    /// Successor origin.
    pub origin: String,
    /// Successor retry generation.
    pub retry_count: i64,
}

/// Plan a retry successor. Returns `None` for non-terminal statuses: a
/// retry is only ever spawned from a settled task.
pub fn plan_retry(
    task_id: &str,
    status: TaskStatus,
    origin: &str,
    retry_count: i64,
) -> Option<RetrySpawn> {
    if !status.is_terminal() {
        return None;
    }
    Some(RetrySpawn {
        task_id: format!("{task_id}-r{}", retry_count + 1),
        origin: if origin == "upgrade" {
            "upgrade".to_string()
        } else {
            "retry".to_string()
        },
        retry_count: retry_count + 1,
    })
}

/// Failover claim window: a successor orchestrator may claim attempt rows
/// whose lease expired this long ago or more. Matches the 300-second
/// cleanup lease.
pub const FAILOVER_LEASE_SECONDS: f64 = 300.0;

/// Maximum cleanup rows claimed per worker pass.
pub const FAILOVER_CLAIM_LIMIT: i64 = 25;
