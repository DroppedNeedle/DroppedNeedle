//! Startup recovery: every active task is classified from its rows
//! before traffic is served, and the clean ones restart.

use crate::downloads::journal::Journal;
use crate::downloads::recovery::{StartupAction, classify_startup};
use crate::downloads::state::TaskStatus;
use crate::downloads::store::StoreError;
use crate::reason::log_unknown_spellings;

/// What startup recovery did, for the boot log and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Queued rows the first worker pass will dispatch.
    pub redispatched: usize,
    /// Rows resuming their landing or their polling.
    pub resumed: usize,
    /// Rows restarted clean (back to queued, same keys).
    pub restarted: usize,
}

/// Classify every active task from its rows and restart the clean ones.
/// Never fails a task: queued rows stay queued for the first pass, a
/// landing or a live attempt resumes, and anything else returns to queued
/// under its existing idempotency keys. Re-running after a clean shutdown
/// is a no-op.
pub async fn run_startup_recovery(journal: &Journal) -> Result<RecoveryReport, StoreError> {
    match journal
        .read("downloads.reason_spellings", |store| {
            store.stored_reason_spellings()
        })
        .await
    {
        Ok(spellings) => log_unknown_spellings(&spellings),
        Err(error) => tracing::warn!(%error, "stored reasons could not be read"),
    }
    let tasks = journal
        .read_tasks_in(&[
            TaskStatus::Queued,
            TaskStatus::Downloading,
            TaskStatus::Processing,
        ])
        .await?;
    let mut report = RecoveryReport::default();
    for task in tasks {
        let attempts = match journal.read_attempts(&task.id).await {
            Ok(attempts) => attempts,
            Err(error) => {
                // Without its attempts a task is never restarted clean: it
                // keeps its state and the first pass looks at it again.
                tracing::warn!(task_id = %task.id, %error, "startup recovery could not read the task's attempts");
                report.resumed += 1;
                continue;
            }
        };
        match classify_startup(task.status, &attempts) {
            StartupAction::Redispatch => report.redispatched += 1,
            StartupAction::ResumeLanding | StartupAction::ResumePoll => report.resumed += 1,
            StartupAction::RestartClean => {
                let task_id = task.id.clone();
                let clock = journal.db().clock().clone();
                journal
                    .run("downloads.recovery.restart", move |store| {
                        let now = crate::db::unix_secs_f64(clock.as_ref());
                        store.transition_task(&task_id, TaskStatus::Queued, now, None)
                    })
                    .await?;
                report.restarted += 1;
            }
            StartupAction::Noop => {}
        }
    }
    Ok(report)
}
