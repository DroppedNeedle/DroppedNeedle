//! The download queue: what a person sees of their downloads and the
//! actions they take on them (cancel, retry, move to the next source,
//! clear, stop every retry, retry every failure), plus the admin
//! blocklist (quarantine) view.
//!
//! Ports the queue half of v2's `download_service.py` and the matching
//! orchestrator actions. Ownership follows v2: admins see and act on every
//! task, everyone else on their own; acting on someone else's task is
//! refused. Retry timing comes from the live worker tuning on each call.

use std::collections::HashMap;
use std::sync::Arc;

use super::queue_rows::{self, ActivitySummary, ListFilter, QueueTask, Stopped, TaskFacts, Viewer};
use super::reasons::{DecisionReason, QueueReason, task_reason};
use super::sources::SourceHandle;
use super::state::TaskStatus;
use super::store::{QuarantineRow, TaskRow};
use super::watchdog::RetryPolicy;
use crate::acquire::worker::{DownloadWorker, NextSourceError, RetryError};

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
    pub reason: Option<QueueReason>,
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
    pub fn new(worker: Arc<DownloadWorker>) -> Self {
        Self { worker }
    }

    fn pool(&self) -> &sqlx::SqlitePool {
        self.worker.journal().db().pool()
    }

    /// The viewer's tasks, newest first, with their reasons and schedule.
    pub async fn list(
        &self,
        viewer: &Viewer,
        filter: &ListFilter,
    ) -> Result<Vec<TaskView>, QueueError> {
        let rows = queue_rows::list_tasks(self.pool(), viewer, filter)
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
        let handle = queue_rows::newest_handle(self.pool(), task_id)
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
        let stopped = self
            .worker
            .journal()
            .run_foreground("downloads.stop", move |store| {
                store.stop_task(&task, now_unix_f64())
            })
            .await
            .map_err(QueueError::Unavailable)?;
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
            .worker
            .journal()
            .run_foreground("downloads.clear", move |store| {
                store.clear_finished(owner.as_deref())
            })
            .await
            .map_err(QueueError::Unavailable)?;
        if cleared > 0 {
            tracing::info!(user_id = %viewer.user_id, cleared, "finished downloads cleared");
        }
        Ok(cleared)
    }

    /// Stop every retry still scheduled for the viewer's failed or short
    /// downloads. Failures that will not retry by themselves stay, for
    /// "retry all failed".
    pub async fn stop_all_retries(&self, viewer: &Viewer) -> Result<i64, QueueError> {
        let policy = self.worker.current_config().retry;
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
        let policy = self.worker.current_config().retry;
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
        queue_rows::activity_summary(self.pool(), viewer)
            .await
            .map_err(unavailable)
    }

    /// One page of the blocklist (admin view).
    pub async fn quarantine(
        &self,
        page: i64,
        page_size: i64,
    ) -> Result<Vec<QuarantineRow>, QueueError> {
        queue_rows::list_quarantine(self.pool(), page, page_size)
            .await
            .map_err(unavailable)
    }

    /// Remove one blocklist entry so its release may be tried again.
    pub async fn delete_quarantine(&self, id: i64) -> Result<(), QueueError> {
        let removed = self
            .worker
            .journal()
            .run_foreground("downloads.quarantine_delete", move |store| {
                store.delete_quarantine(id)
            })
            .await
            .map_err(QueueError::Unavailable)?;
        if removed {
            Ok(())
        } else {
            Err(QueueError::NotFound)
        }
    }

    /// One task, refused when the viewer may not touch it.
    async fn owned(&self, viewer: &Viewer, task_id: &str) -> Result<QueueTask, QueueError> {
        let row = queue_rows::get_task(self.pool(), task_id)
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
        let tasks = queue_rows::newest_unsettled(self.pool(), viewer)
            .await
            .map_err(unavailable)?;
        let ids: Vec<String> = tasks.iter().map(|task| task.id.clone()).collect();
        let facts = queue_rows::task_facts(self.pool(), &ids)
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
        let mut facts: HashMap<String, TaskFacts> = queue_rows::task_facts(self.pool(), &ids)
            .await
            .map_err(unavailable)?;
        let config = self.worker.current_config();
        let retry_max = i64::from(config.retry.auto_retry_max());
        let ladder = config.retry.ladder_minutes();
        Ok(rows
            .into_iter()
            .map(|row| {
                let facts = facts.remove(&row.task.id).unwrap_or_default();
                let decision = facts.decision.as_ref().map(|decision| DecisionReason {
                    outcome: decision.outcome.as_str(),
                    code: decision.reason_code.as_deref(),
                    text: decision.reason_text.as_deref(),
                    action: decision.reason_action.as_deref(),
                });
                let reason = task_reason(
                    row.task.status,
                    row.task.error_message.as_deref(),
                    facts.held_for_review,
                    decision.as_ref(),
                );
                // Tasks paused on a held-file review show no countdown: it
                // would never fire.
                let next_retry_at =
                    will_auto_retry(&config.retry, &row.task, facts.held_for_review);
                TaskView {
                    row,
                    facts,
                    next_retry_at,
                    retry_max,
                    retry_ladder_minutes: ladder.clone(),
                    attempt_total: config.max_failover_attempts,
                    reason,
                }
            })
            .collect())
    }
}

fn now_unix_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
