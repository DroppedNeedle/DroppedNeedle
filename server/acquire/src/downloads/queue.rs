//! The download queue: what a person sees of their downloads and the
//! actions they take on them (cancel, retry, move to the next source,
//! clear, stop every retry, retry every failure), plus the admin
//! blocklist (quarantine) view.
//!
//! Admins see and act on every
//! task, everyone else on their own; acting on someone else's task is
//! refused. Retry timing comes from the live worker tuning on each call.

use std::collections::HashMap;
use std::sync::Arc;

use super::journal::Journal;
use super::queue_rows::{ActivitySummary, ListFilter, QueueTask, Stopped, TaskFacts, Viewer};
use super::reasons::{DecisionReason, task_reason};
use super::sources::SourceHandle;
use super::state::TaskStatus;
use super::store::{QuarantineRow, TaskRow};
use super::watchdog::RetryPolicy;
use crate::downloads::worker::{DownloadWorker, NextSourceError, RetryError};
use crate::reason::Reason;

/// Queue failures, mapped to HTTP statuses by the handlers.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// No such task or entry.
    #[error("not found")]
    NotFound,
    /// The task belongs to someone else.
    #[error("{0}")]
    Forbidden(&'static str),
    /// The action does not fit the task's state. The sentence is shown.
    #[error("{0}")]
    Conflict(String),
    /// The journal could not be read or written.
    #[error("download queue unavailable: {0}")]
    Unavailable(String),
}

/// What a queue action answers while the download settings do not read.
pub const SETTINGS_UNREADABLE: &str =
    "Download settings can't be read. Save Settings, Downloads again.";

fn unavailable(error: impl std::fmt::Display) -> QueueError {
    QueueError::Unavailable(error.to_string())
}

/// One task as the queue shows it.
#[derive(Debug, Clone)]
pub struct TaskView {
    pub row: QueueTask,
    pub facts: TaskFacts,
    /// When the next automatic retry is due, if one will run.
    pub next_retry_at: Option<f64>,
    /// Automatic retries allowed in all (0 when auto-retry is off).
    pub retry_max: i64,
    /// The whole retry backoff schedule in minutes.
    pub retry_ladder_minutes: Vec<i64>,
    /// Sources a task may try in all.
    pub attempt_total: i64,
    /// Why the task sits where it does, when it needs saying.
    pub reason: Option<Reason>,
    /// The reason's sentence when it names more than the reason can (the
    /// library folder a download waits for).
    pub reason_text: Option<String>,
}

/// One file of a task's current source.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskFile {
    pub filename: String,
    pub size: Option<i64>,
}

/// The queue over the download worker and its journal.
#[derive(Clone)]
pub struct DownloadQueue {
    journal: Arc<Journal>,
    worker: Arc<DownloadWorker>,
}

/// Whether the retry sweep will pick the task up again by itself.
fn will_auto_retry(policy: &RetryPolicy, task: &TaskRow, held: bool) -> Option<f64> {
    if held || task.origin == "upgrade" {
        return None;
    }
    policy.next_retry_at(
        u32::try_from(task.retry_count.max(0)).unwrap_or(u32::MAX),
        task.completed_at.unwrap_or(task.updated_at),
        task.status.as_str(),
    )
}

impl DownloadQueue {
    /// The queue over `journal`; `worker` runs the actions that wake it.
    pub fn new(journal: Arc<Journal>, worker: Arc<DownloadWorker>) -> Self {
        Self { journal, worker }
    }

    /// The viewer's tasks, newest first, with their reasons and schedule.
    pub async fn list(
        &self,
        viewer: &Viewer,
        filter: &ListFilter,
    ) -> Result<Vec<TaskView>, QueueError> {
        let rows = self
            .journal
            .read_queue(viewer, filter)
            .await
            .map_err(unavailable)?;
        self.views(rows).await
    }

    /// One task the viewer may see.
    pub async fn get(&self, viewer: &Viewer, task_id: &str) -> Result<TaskView, QueueError> {
        let row = self.owned(viewer, task_id).await?;
        let mut views = self.views(vec![row]).await?;
        views.pop().ok_or(QueueError::NotFound)
    }

    /// The files of a task's current (newest) source, as its download
    /// client was asked for them.
    pub async fn files(
        &self,
        viewer: &Viewer,
        task_id: &str,
    ) -> Result<(QueueTask, Vec<TaskFile>), QueueError> {
        let row = self.owned(viewer, task_id).await?;
        let handle = self
            .journal
            .read_newest_handle(task_id)
            .await
            .map_err(unavailable)?
            .and_then(|json| serde_json::from_str::<SourceHandle>(&json).ok());
        let files = match handle {
            Some(handle) if !handle.filenames.is_empty() => handle
                .filenames
                .iter()
                .enumerate()
                .map(|(index, name)| TaskFile {
                    filename: name.clone(),
                    size: handle.sizes.get(index).copied(),
                })
                .collect(),
            // A usenet job is one release folder.
            Some(handle) if !handle.job_name.is_empty() => vec![TaskFile {
                filename: handle.job_name,
                size: row.task.total_size_bytes,
            }],
            _ => Vec::new(),
        };
        Ok((row, files))
    }

    /// Stop one task: cancel it if it is live, or stop its retries if it
    /// failed. A task that already finished answers a conflict.
    pub async fn cancel(&self, viewer: &Viewer, task_id: &str) -> Result<(), QueueError> {
        let row = self.owned(viewer, task_id).await?;
        match self.stop(row.task).await? {
            Stopped::AlreadySettled => Err(QueueError::Conflict(
                "This download already finished.".to_owned(),
            )),
            Stopped::Cancelled | Stopped::RetriesStopped => Ok(()),
        }
    }

    async fn stop(&self, task: TaskRow) -> Result<Stopped, QueueError> {
        let task_id = task.id.clone();
        let clock = self.journal.db().clock().clone();
        let stopped = self
            .journal
            .run_foreground("downloads.stop", move |store| {
                store.stop_task(&task, crate::db::unix_secs_f64(clock.as_ref()))
            })
            .await
            .map_err(unavailable)?;
        if stopped != Stopped::AlreadySettled {
            tracing::info!(task_id, ?stopped, "download stopped by hand");
        }
        Ok(stopped)
    }

    /// Move a transfer waiting in a peer's queue to the next source.
    pub async fn next_source(
        &self,
        viewer: &Viewer,
        task_id: &str,
        expected_candidate_index: i64,
    ) -> Result<TaskRow, QueueError> {
        self.owned(viewer, task_id).await?;
        self.worker
            .next_source(task_id, expected_candidate_index)
            .await
            .map_err(|error| match error {
                NextSourceError::NotFound => QueueError::NotFound,
                NextSourceError::Conflict(message) => QueueError::Conflict(message.to_owned()),
                NextSourceError::Unavailable(cause) => QueueError::Unavailable(cause),
            })
    }

    /// Retry one settled task now. Answers the new task's id.
    pub async fn retry(&self, viewer: &Viewer, task_id: &str) -> Result<String, QueueError> {
        let row = self.owned(viewer, task_id).await?;
        self.retry_task(&row.task).await
    }

    async fn retry_task(&self, task: &TaskRow) -> Result<String, QueueError> {
        self.worker
            .retry_now(task)
            .await
            .map_err(|error| match error {
                RetryError::Conflict(message) => QueueError::Conflict(message.to_owned()),
                RetryError::Journal(cause) => QueueError::Unavailable(cause),
            })
    }

    /// Delete the viewer's finished (completed and cancelled) tasks.
    pub async fn clear(&self, viewer: &Viewer) -> Result<i64, QueueError> {
        let owner = viewer.owner_filter().map(str::to_owned);
        let cleared = self
            .journal
            .run_foreground("downloads.clear", move |store| {
                store.clear_finished(owner.as_deref())
            })
            .await
            .map_err(unavailable)?;
        if cleared > 0 {
            tracing::info!(user_id = %viewer.user_id, cleared, "finished downloads cleared");
        }
        Ok(cleared)
    }

    /// Stop every retry still scheduled for the viewer's failed or short
    /// downloads. Failures that will not retry by themselves stay, for
    /// "retry all failed".
    pub async fn stop_all_retries(&self, viewer: &Viewer) -> Result<i64, QueueError> {
        let policy = self
            .worker
            .current_config()
            .ok_or_else(|| QueueError::Conflict(SETTINGS_UNREADABLE.to_owned()))?
            .retry;
        let (tasks, held) = self.unsettled(viewer).await?;
        let mut stopped = 0;
        for task in tasks {
            if will_auto_retry(&policy, &task, held.contains(&task.id)).is_none() {
                continue;
            }
            if self.stop(task).await? != Stopped::AlreadySettled {
                stopped += 1;
            }
        }
        Ok(stopped)
    }

    /// Retry each of the viewer's failed downloads that will not retry by
    /// itself (auto-retry off or attempts spent),
    /// once per album or track: older failures superseded by a newer task
    /// for the same target are skipped.
    pub async fn retry_all_failed(&self, viewer: &Viewer) -> Result<i64, QueueError> {
        let policy = self
            .worker
            .current_config()
            .ok_or_else(|| QueueError::Conflict(SETTINGS_UNREADABLE.to_owned()))?
            .retry;
        let (tasks, held) = self.unsettled(viewer).await?;
        let mut retried = 0;
        for task in tasks {
            // Held files wait for a person's decision; downloading the
            // album again would only hold the same files again.
            let waiting = held.contains(&task.id);
            if task.status != TaskStatus::Failed
                || waiting
                || will_auto_retry(&policy, &task, waiting).is_some()
            {
                continue;
            }
            match self.retry_task(&task).await {
                Ok(_) => retried += 1,
                Err(QueueError::Conflict(message)) => {
                    tracing::info!(task_id = %task.id, %message, "bulk retry skipped a task");
                }
                Err(error) => return Err(error),
            }
        }
        Ok(retried)
    }

    /// The viewer's queue counters.
    pub async fn activity(&self, viewer: &Viewer) -> Result<ActivitySummary, QueueError> {
        self.journal
            .read_activity_summary(viewer)
            .await
            .map_err(unavailable)
    }

    /// One page of the blocklist (admin view).
    pub async fn quarantine(
        &self,
        page: i64,
        page_size: i64,
    ) -> Result<Vec<QuarantineRow>, QueueError> {
        self.journal
            .read_quarantine_page(page, page_size)
            .await
            .map_err(unavailable)
    }

    /// Remove one blocklist entry so its release may be tried again.
    pub async fn delete_quarantine(&self, id: i64) -> Result<(), QueueError> {
        let removed = self
            .journal
            .run_foreground("downloads.quarantine_delete", move |store| {
                store.delete_quarantine(id)
            })
            .await
            .map_err(unavailable)?;
        if removed {
            Ok(())
        } else {
            Err(QueueError::NotFound)
        }
    }

    /// One task, refused when the viewer may not touch it.
    async fn owned(&self, viewer: &Viewer, task_id: &str) -> Result<QueueTask, QueueError> {
        let row = self
            .journal
            .read_queue_task(task_id)
            .await
            .map_err(unavailable)?
            .ok_or(QueueError::NotFound)?;
        if !viewer.may_touch(&row.task.user_id) {
            return Err(QueueError::Forbidden(
                "This download belongs to another user.",
            ));
        }
        Ok(row)
    }

    /// The viewer's newest failed or short tasks, plus which of them wait
    /// on held files.
    async fn unsettled(
        &self,
        viewer: &Viewer,
    ) -> Result<(Vec<TaskRow>, std::collections::HashSet<String>), QueueError> {
        let tasks = self
            .journal
            .read_newest_unsettled(viewer)
            .await
            .map_err(unavailable)?;
        let ids: Vec<String> = tasks.iter().map(|task| task.id.clone()).collect();
        let facts = self
            .journal
            .read_task_facts(&ids)
            .await
            .map_err(unavailable)?;
        let held = facts
            .into_iter()
            .filter(|(_, facts)| facts.held_for_review)
            .map(|(id, _)| id)
            .collect();
        Ok((tasks, held))
    }

    /// Attach facts, schedule and reason to task rows.
    async fn views(&self, rows: Vec<QueueTask>) -> Result<Vec<TaskView>, QueueError> {
        let ids: Vec<String> = rows.iter().map(|row| row.task.id.clone()).collect();
        let mut facts: HashMap<String, TaskFacts> = self
            .journal
            .read_task_facts(&ids)
            .await
            .map_err(unavailable)?;
        // While the download settings do not read, the views leave the
        // retry schedule out instead of failing the page.
        let config = self.worker.current_config();
        let retry_max = config
            .as_ref()
            .map_or(0, |config| i64::from(config.retry.auto_retry_max()));
        let ladder = config
            .as_ref()
            .map(|config| config.retry.ladder_minutes())
            .unwrap_or_default();
        let attempt_total = config
            .as_ref()
            .map(|config| config.max_failover_attempts)
            .unwrap_or_default();
        let no_source = !self.worker.readiness().can_acquire();
        let library_wait = self.worker.library_wait();
        Ok(rows
            .into_iter()
            .map(|row| {
                let facts = facts.remove(&row.task.id).unwrap_or_default();
                let decision = facts.decision.as_ref().map(|decision| DecisionReason {
                    outcome: decision.outcome.as_str(),
                    code: decision.reason_code,
                });
                let mut reason_text = None;
                let reason = task_reason(
                    row.task.status,
                    row.task.outcome_code,
                    facts.held_for_review,
                    decision.as_ref(),
                )
                // A library wait says more than a copy that gave way.
                .filter(|reason| *reason != Reason::QueueGaveWay || library_wait.is_none())
                .or_else(|| {
                    if row.task.status == TaskStatus::Queued && no_source {
                        return Some(Reason::NoDownloadSource);
                    }
                    if row.task.status.is_terminal() {
                        return None;
                    }
                    // Nothing starts or lands while the library waits.
                    let wait = library_wait.as_ref()?;
                    reason_text = Some(wait.sentence());
                    Some(wait.reason())
                });
                // Tasks paused on a held-file review show no countdown: it
                // would never fire.
                let next_retry_at = config.as_ref().and_then(|config| {
                    will_auto_retry(&config.retry, &row.task, facts.held_for_review)
                });
                TaskView {
                    row,
                    facts,
                    next_retry_at,
                    retry_max,
                    retry_ladder_minutes: ladder.clone(),
                    attempt_total,
                    reason,
                    reason_text,
                }
            })
            .collect())
    }
}
