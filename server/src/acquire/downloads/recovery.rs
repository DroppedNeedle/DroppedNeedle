//! Startup recovery and orchestrator failover.
//!
//! Ports `startup_resume` / `_resume_single_task` classification: queued
//! tasks re-dispatch (they never started, so failing them would be
//! spurious), tasks with a staging manifest resume polling against the
//! transfers the client kept across the restart, and tasks without a
//! manifest - or whose manifest no longer matches the journaled attempt -
//! restart cleanly from scratch. Recovery never blocks startup and never
//! repeats a fetch destructively: resume re-correlates before polling,
//! and restart reuses the idempotency key so a double resume cannot
//! double-enqueue.
//!
//! Failover between orchestrator instances rides the attempt-lease
//! mechanism in [`super::store`]: a successor claims only rows whose
//! lease lapsed, so the old owner's in-flight work is never disturbed.

use super::state::TaskStatus;

/// What startup recovery should do with one active task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    /// Created but never enqueued: dispatch it now.
    Redispatch,
    /// Manifest on disk matches the journal: poll the client's kept
    /// transfers instead of failing them (the old "transfer lost during
    /// restart" bug); a genuinely dead transfer ages out via the stall
    /// watchdog and failover re-pulls from another peer.
    ResumePoll,
    /// No manifest, or the journal moved past it: run the task from
    /// scratch under the same idempotency key.
    RestartClean,
    /// Already settled: leave the row alone. Terminal tasks never reach
    /// the classifier (the caller only feeds active rows); this arm keeps
    /// a misfed row a no-op instead of resurrecting it.
    Noop,
}

/// Inputs the classifier needs for one task.
#[derive(Debug, Clone, Copy)]
pub struct StartupCtx {
    /// Current persisted status.
    pub status: TaskStatus,
    /// Whether `staging/{task_id}/manifest.json` exists.
    pub manifest_present: bool,
    /// Whether the manifest's attempt still matches the task's selected
    /// candidate (`None` when either side is unknown).
    pub manifest_matches_attempt: Option<bool>,
}

/// Classify one active task for startup recovery.
///
/// The caller only feeds active rows; a terminal row fed by mistake maps
/// to [`StartupAction::Noop`] so recovery never resurrects a task that
/// already settled (the store guard would refuse the write anyway).
pub fn classify_startup(ctx: StartupCtx) -> StartupAction {
    match ctx.status {
        TaskStatus::Queued => StartupAction::Redispatch,
        TaskStatus::Downloading | TaskStatus::Processing => {
            if !ctx.manifest_present {
                return StartupAction::RestartClean;
            }
            match ctx.manifest_matches_attempt {
                Some(false) => StartupAction::RestartClean,
                _ => StartupAction::ResumePoll,
            }
        }
        // A terminal row fed by mistake is left alone; recovery must
        // never resurrect a task that already settled (the store guard
        // would refuse the write anyway).
        TaskStatus::Completed
        | TaskStatus::Partial
        | TaskStatus::Failed
        | TaskStatus::Cancelled => StartupAction::Noop,
    }
}

/// Planned successor for a terminal task. Ports `_create_retry_task`:
/// the original stays terminal for audit while a fresh queued task
/// carries `retry_count + 1`. An upgrade's retry stays an upgrade (keeps
/// the origin-aware gate); everything else becomes `retry` so quota
/// counts ignore it.
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
/// whose lease expired this long ago or more. Mirrors the 300-second
/// cleanup lease in `AcquisitionCleanupService.run_once`.
pub const FAILOVER_LEASE_SECONDS: f64 = 300.0;

/// Maximum cleanup rows claimed per worker pass (v2: 25).
pub const FAILOVER_CLAIM_LIMIT: i64 = 25;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_redispatches() {
        let ctx = StartupCtx {
            status: TaskStatus::Queued,
            manifest_present: false,
            manifest_matches_attempt: None,
        };
        assert_eq!(classify_startup(ctx), StartupAction::Redispatch);
    }

    #[test]
    fn manifest_absent_restarts_clean() {
        let ctx = StartupCtx {
            status: TaskStatus::Downloading,
            manifest_present: false,
            manifest_matches_attempt: None,
        };
        assert_eq!(classify_startup(ctx), StartupAction::RestartClean);
    }

    #[test]
    fn candidate_mismatch_restarts_clean() {
        let ctx = StartupCtx {
            status: TaskStatus::Processing,
            manifest_present: true,
            manifest_matches_attempt: Some(false),
        };
        assert_eq!(classify_startup(ctx), StartupAction::RestartClean);
    }

    #[test]
    fn matching_manifest_resumes_poll() {
        let ctx = StartupCtx {
            status: TaskStatus::Downloading,
            manifest_present: true,
            manifest_matches_attempt: Some(true),
        };
        assert_eq!(classify_startup(ctx), StartupAction::ResumePoll);
    }
}
