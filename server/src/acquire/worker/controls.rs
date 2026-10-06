//! Queue actions that need the worker's sources and tuning: storing a
//! poll's progress (and telling the owner's tabs), retrying a settled task
//! right away, and moving a transfer stuck in a peer's queue to the next
//! source.

use std::sync::Arc;

use super::{DownloadWorker, Pass, handled_count, live_attempt, now_unix_f64, successor_row};
use crate::acquire::downloads::queue_rows::ProgressColumns;
use crate::acquire::downloads::reasons::NEXT_SOURCE_NOTE;
use crate::acquire::downloads::sources::TransferProgress;
use crate::acquire::downloads::state::TaskStatus;
use crate::acquire::downloads::store::TaskRow;
use crate::acquire::downloads::watchdog::Watchdog;
use crate::events::{DownloadProgress, UserNotice};

/// Why the next-source action was refused. The sentences are shown to
/// the person who clicked.
#[derive(Debug, thiserror::Error)]
pub enum NextSourceError {
    /// No such task.
    #[error("download task not found")]
    NotFound,
    /// The task is not in a state where switching makes sense.
    #[error("{0}")]
    Conflict(&'static str),
    /// The journal or the download client could not be read.
    #[error("{0}")]
    Unavailable(String),
}

/// Why a manual retry was refused.
#[derive(Debug, thiserror::Error)]
pub enum RetryError {
    /// Only finished-unsuccessfully tasks can be retried.
    #[error("{0}")]
    Conflict(&'static str),
    /// The journal write failed.
    #[error("{0}")]
    Journal(String),
}

/// Byte percentage when the total is known, else the file ratio.
fn percent(progress: &TransferProgress) -> i64 {
    let ratio = match progress.total_bytes {
        Some(total) if total > 0 => progress.downloaded_bytes as f64 / total as f64,
        _ if progress.files_total > 0 => {
            f64::from(progress.files_completed) / f64::from(progress.files_total)
        }
        _ => 0.0,
    };
    (ratio * 100.0).clamp(0.0, 100.0).round() as i64
}

impl DownloadWorker {
    /// Store one healthy poll's progress on the task and send it to the
    /// owner's open tabs as `download_progress`.
    pub(super) async fn record_progress(
        &self,
        pass: &Pass,
        task: &TaskRow,
        progress: &TransferProgress,
    ) {
        let now = pass.now;
        let columns = ProgressColumns {
            downloaded_bytes: i64::try_from(progress.downloaded_bytes).unwrap_or(i64::MAX),
            total_bytes: progress
                .total_bytes
                .map(|total| i64::try_from(total).unwrap_or(i64::MAX)),
            progress_percent: percent(progress),
            files_total: i64::from(progress.files_total),
            files_completed: i64::from(progress.files_completed),
            files_failed: i64::from(progress.files_failed),
            queue_position_start: progress.queue_position_start,
            queue_position_end: progress.queue_position_end,
            remote_queued: progress.downloaded_bytes == 0
                && !progress.has_active_transfer
                && !progress.all_terminal,
        };
        let task_id = task.id.clone();
        let stored = columns.clone();
        let attempts = self
            .step("downloads.progress", move |store| {
                store.record_progress(&task_id, &stored, now)?;
                Ok(store.list_attempts(&task_id)?.len() as i64)
            })
            .await;
        let Some(attempts) = attempts else {
            return;
        };
        let attempt_total = pass.config.max_failover_attempts;
        self.events.notify(
            &task.user_id,
            UserNotice::DownloadProgress(DownloadProgress {
                task_id: task.id.clone(),
                status: task.status.as_str().to_owned(),
                bytes_downloaded: columns.downloaded_bytes,
                bytes_total: columns.total_bytes.or(task.total_size_bytes).unwrap_or(0),
                files_completed: columns.files_completed,
                files_total: columns.files_total,
                progress_percent: columns.progress_percent,
                source: task.source.clone(),
                candidate_index: task.candidate_index,
                quality_format: task.quality_format.clone(),
                queue_position_start: columns.queue_position_start,
                queue_position_end: columns.queue_position_end,
                remote_queued: columns.remote_queued,
                attempt_number: attempts,
                attempt_total,
                has_next_source: attempts < attempt_total,
            }),
        );
    }

    /// Retry a settled task now: spawn its successor (or reuse the one a
    /// double click already spawned), point its request at it, and wake
    /// the loop so the search starts right away. An album retry clears the
    /// album's blocklist first: an explicit "try again" reconsiders the
    /// releases the failed attempt quarantined (a track retry leaves the
    /// album's blocklist alone, and auto-retry never clears it). Answers
    /// the successor's id.
    pub async fn retry_now(&self, task: &TaskRow) -> Result<String, RetryError> {
        if !matches!(
            task.status,
            TaskStatus::Failed | TaskStatus::Partial | TaskStatus::Cancelled
        ) {
            return Err(RetryError::Conflict(
                "Only failed, cancelled or partial downloads can be retried.",
            ));
        }
        let Some(row) = successor_row(task) else {
            return Err(RetryError::Conflict(
                "Only failed, cancelled or partial downloads can be retried.",
            ));
        };
        let source = task.clone();
        let now = now_unix_f64();
        let outcome = self
            .journal
            .run_foreground("downloads.manual_retry", move |store| {
                if let Some(existing) = store.get_task(&row.id)? {
                    // A second click on the same task: answer the retry it
                    // already started, unless that retry settled too.
                    return Ok(if existing.status.is_terminal() {
                        None
                    } else {
                        Some(existing.id)
                    });
                }
                if source.download_type == "album" && !source.release_group_mbid.is_empty() {
                    store.delete_quarantine_for_album(&source.release_group_mbid)?;
                }
                store.spawn_successor(&source, &row, now)?;
                store.relink_request(&source.id, &row.id)?;
                Ok(Some(row.id))
            })
            .await
            .map_err(RetryError::Journal)?;
        let Some(successor) = outcome else {
            return Err(RetryError::Conflict(
                "This download was already retried. Retry the newer attempt instead.",
            ));
        };
        tracing::info!(task_id = %task.id, %successor, "download retried by hand");
        self.wake();
        Ok(successor)
    }

    /// Give up on a transfer that waits in a Soulseek peer's queue and
    /// move to the next ranked source. `expected_candidate_index` makes a
    /// double click or a stale page harmless: once the task moved on, the
    /// old index no longer matches and nothing happens. The live client
    /// status is checked first, so a transfer that started moving bytes
    /// since the page rendered is left alone. The peer is not blocklisted:
    /// it was only slow, and the next search skips it because this task
    /// already tried it.
    pub async fn next_source(
        self: &Arc<Self>,
        task_id: &str,
        expected_candidate_index: i64,
    ) -> Result<TaskRow, NextSourceError> {
        let task = self
            .journal
            .read_task(task_id)
            .await
            .map_err(NextSourceError::Unavailable)?
            .ok_or(NextSourceError::NotFound)?;
        if task.candidate_index != Some(expected_candidate_index) {
            return Err(NextSourceError::Conflict(
                "The download has already moved to another source.",
            ));
        }
        if task.source != "soulseek" || task.status != TaskStatus::Downloading {
            return Err(NextSourceError::Conflict(
                "The download is not waiting in a Soulseek queue.",
            ));
        }
        if self.landing_in_flight(&task.id) {
            return Err(NextSourceError::Conflict(
                "The download finished and is being imported.",
            ));
        }
        let config = (self.config)();
        let pass = Pass {
            watchdog: Watchdog::new(config.watchdog.clone()),
            sources: (self.sources)(),
            config,
            now: now_unix_f64(),
        };
        let attempts = self.attempts(&task.id).await.ok_or_else(|| {
            NextSourceError::Unavailable("download attempts unreadable".to_owned())
        })?;
        let Some(attempt) = live_attempt(&attempts).cloned() else {
            return Err(NextSourceError::Conflict(
                "The download is no longer waiting in a remote queue.",
            ));
        };
        let (Some(handle), Some(source)) =
            (attempt.handle.clone(), pass.source_for(&attempt.row.source))
        else {
            return Err(NextSourceError::Conflict(
                "The download is no longer waiting in a remote queue.",
            ));
        };
        if handled_count(&attempts, None) >= pass.config.max_failover_attempts {
            return Err(NextSourceError::Conflict(
                "No other eligible source is available.",
            ));
        }
        let progress = source
            .poll(&handle)
            .await
            .map_err(|error| NextSourceError::Unavailable(error.to_string()))?;
        if progress.downloaded_bytes > 0 {
            return Err(NextSourceError::Conflict(
                "The transfer has already started.",
            ));
        }
        if progress.has_active_transfer || progress.all_terminal {
            return Err(NextSourceError::Conflict(
                "The download is no longer waiting in a remote queue.",
            ));
        }
        self.fail_over(
            pass.now,
            &task,
            &attempt.row,
            source,
            &handle,
            NEXT_SOURCE_NOTE,
            None,
        )
        .await;
        tracing::info!(task_id = %task.id, "download moved to the next source by hand");
        self.wake();
        self.journal
            .read_task(&task.id)
            .await
            .map_err(NextSourceError::Unavailable)?
            .ok_or(NextSourceError::NotFound)
    }
}
