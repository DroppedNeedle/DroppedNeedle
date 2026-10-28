//! Journal rows behind the download queue view.
//!
//! Writes run on the writer lane through [`DownloadStore`]: live progress,
//! cancel and stop-retrying, retry successors, clearing finished rows and
//! removing quarantine entries. Reads run on the read pool through
//! [`super::journal::Journal`]: the scoped task list, the per-task attempt
//! and decision summaries the cards show, and the activity summary.
//!
//! "Scoped" means an admin sees every user's tasks, anyone
//! else only their own.

use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::{OptionalExtension as _, Row, params};

use super::sql;
use super::state::TaskStatus;
use super::store::{DownloadStore, NewTask, StoreError, TaskRow};
use crate::db::{ms_from_secs_f64 as ms, secs_f64_from_ms};
use crate::held::store::{FileState, HeldRows};
use crate::reason::Reason;
use crate::requests::settle::relink;

/// Who is looking at the queue.
#[derive(Debug, Clone)]
pub struct Viewer {
    pub user_id: String,
    /// Admins see and act on every user's tasks.
    pub admin: bool,
}

impl Viewer {
    /// `None` for admins (no owner filter), the user id otherwise.
    pub fn owner_filter(&self) -> Option<&str> {
        (!self.admin).then_some(self.user_id.as_str())
    }

    /// Whether this viewer may see and act on a task owned by `owner`.
    pub fn may_touch(&self, owner: &str) -> bool {
        self.admin || self.user_id == owner
    }
}

/// One poll's progress, as the task row stores it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressColumns {
    pub downloaded_bytes: i64,
    /// `None` keeps the stored total.
    pub total_bytes: Option<i64>,
    pub progress_percent: i64,
    pub files_total: i64,
    pub files_completed: i64,
    pub files_failed: i64,
    pub queue_position_start: Option<i64>,
    pub queue_position_end: Option<i64>,
    pub remote_queued: bool,
}

/// What stopping a task did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// A live task was cancelled; its transfer is aborted by the cleanup pass.
    Cancelled,
    /// A failed or short task will not be retried any more.
    RetriesStopped,
    /// Completed or already cancelled: nothing to stop.
    AlreadySettled,
}

/// What an album purge took out of the download journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumPurge {
    /// Failed or short tasks that will no longer retry.
    pub retries_cancelled: usize,
    /// Held downloads discarded.
    pub held_removed: usize,
}

impl DownloadStore<'_> {
    /// Download-side cleanup after an album left the library at
    /// `removed_at` (unix seconds): its failed
    /// or short tasks stop retrying, its held downloads are discarded and
    /// its blocklist entries go. Only rows made at or before the removal
    /// are touched, so a request made since keeps its own; held files go
    /// by when each was held, and uploads are never touched. Live tasks are
    /// left alone; they are cancelled per task so their transfers are torn
    /// down. The caller deletes the held copies first
    /// ([`HeldRows::removed_album_files`]).
    pub fn purge_album(
        &self,
        release_group_mbid: &str,
        removed_at: f64,
        now: f64,
    ) -> Result<AlbumPurge, StoreError> {
        let cancelled: Vec<String> = self
            .conn
            .prepare(sql::PURGE_ALBUM_TASKS.text())?
            .query_map(
                params![release_group_mbid, ms(now), ms(removed_at)],
                |row| row.get(0),
            )?
            .collect::<Result<_, _>>()?;
        // Files kept for "Retry import" go with the tasks that kept them.
        for task_id in &cancelled {
            self.release_task_kept(task_id, now)?;
        }
        let retries_cancelled = cancelled.len();
        let held = HeldRows::new(self.conn);
        let removed_ms = ms(removed_at);
        let now_ms = ms(now);
        let mut held_removed = 0;
        for (file, entry, _) in held.removed_album_files(release_group_mbid, removed_ms)? {
            held_removed += held.resolve(entry, &[file], FileState::Discarded, now_ms)?;
        }
        self.conn.execute(
            sql::PURGE_ALBUM_QUARANTINE.text(),
            params![release_group_mbid, ms(removed_at)],
        )?;
        Ok(AlbumPurge {
            retries_cancelled,
            held_removed,
        })
    }

    /// Store one poll's progress on a live task and stamp the poll.
    pub fn record_progress(
        &self,
        task_id: &str,
        progress: &ProgressColumns,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            sql::RECORD_PROGRESS.text(),
            params![
                task_id,
                progress.downloaded_bytes,
                progress.total_bytes,
                progress.progress_percent,
                progress.files_total,
                progress.files_completed,
                progress.files_failed,
                progress.queue_position_start,
                progress.queue_position_end,
                progress.remote_queued,
                ms(now),
            ],
        )?;
        Ok(())
    }

    /// Stop one task. A live task is cancelled (its attempts go to cleanup
    /// with a discard disposition). A failed or short task is marked
    /// cancelled so the retry sweep skips it, and files kept for "Retry
    /// import" go to cleanup. Either way the owner's
    /// wanted watch on the album stops too, so it cannot start the task
    /// again (issue #255: one action, no hidden second switch).
    pub fn stop_task(&self, task: &TaskRow, now: f64) -> Result<Stopped, StoreError> {
        let outcome = match task.status {
            TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Processing => {
                if self.cancel_task(&task.id, now)? {
                    Stopped::Cancelled
                } else {
                    Stopped::AlreadySettled
                }
            }
            TaskStatus::Failed | TaskStatus::Partial => {
                let changed = self
                    .conn
                    .execute(sql::STOP_RETRIES.text(), params![task.id, ms(now)])?;
                if changed == 0 {
                    Stopped::AlreadySettled
                } else {
                    self.release_task_kept(&task.id, now)?;
                    Stopped::RetriesStopped
                }
            }
            TaskStatus::Completed | TaskStatus::Cancelled => Stopped::AlreadySettled,
        };
        if outcome != Stopped::AlreadySettled && !task.release_group_mbid.is_empty() {
            self.conn.execute(
                sql::STOP_WATCH.text(),
                params![task.release_group_mbid, task.user_id],
            )?;
        }
        Ok(outcome)
    }

    /// Insert a retry successor of `source` unless it already exists. The
    /// edition pin, track identity and the plan's release, wanted tracks,
    /// planned copy and "Get it from the album" ride along, so a retry
    /// fetches and files the same, and the requests that followed `source`
    /// follow the successor and read downloading again. Answers whether a
    /// row was created.
    pub fn spawn_successor(
        &self,
        source: &TaskRow,
        successor: &NewTask,
        now: f64,
    ) -> Result<bool, StoreError> {
        if self.get_task(&successor.id)?.is_some() {
            return Ok(false);
        }
        let details = self.task_details(&source.id)?;
        let plan = self.task_plan(&source.id)?.for_successor();
        if source.download_type == "track" {
            let recording = source.recording_mbid.clone().unwrap_or_default();
            self.insert_track_task(successor, &recording, now)?;
        } else {
            self.insert_task(successor, now)?;
        }
        self.set_task_details(&successor.id, &details, now)?;
        self.set_task_plan(&successor.id, &plan, now)?;
        relink(self.conn, &source.id, &successor.id)?;
        Ok(true)
    }

    /// Delete finished (completed and cancelled) tasks, for one owner or,
    /// with `None`, for everyone. Attempt rows stay: their cleanup debt
    /// outlives the queue row by design, and files still kept for a
    /// removed task go to cleanup at the next hourly sweep.
    pub fn clear_finished(&self, owner: Option<&str>) -> Result<i64, StoreError> {
        let removed = self
            .conn
            .execute(sql::CLEAR_FINISHED.text(), params![owner])?;
        Ok(removed as i64)
    }

    /// Remove one quarantine entry, so its release may be tried again.
    /// Answers false when no such entry exists.
    pub fn delete_quarantine(&self, id: i64) -> Result<bool, StoreError> {
        let removed = self
            .conn
            .execute(sql::DELETE_QUARANTINE.text(), params![id])?;
        Ok(removed > 0)
    }
}

/// Columns of a task the queue view shows beyond [`TaskRow`].
#[derive(Debug, Clone, Default)]
pub struct TaskExtras {
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub artist_mbid: Option<String>,
    pub track_title: Option<String>,
    pub year: Option<i64>,
    pub files_total: i64,
    pub files_completed: i64,
    pub files_failed: i64,
    pub queue_position_start: Option<i64>,
    pub queue_position_end: Option<i64>,
    pub remote_queued: bool,
    pub quality_bitrate: Option<i64>,
    pub quality_bit_depth: Option<i64>,
    pub quality_sample_rate: Option<i64>,
    pub advertised_queue_depth: Option<i64>,
    pub final_path: Option<String>,
    pub wrong_product_verdict_at: Option<f64>,
    pub wrong_product_detail: Option<String>,
}

/// A task row as the queue view reads it.
#[derive(Debug, Clone)]
pub struct QueueTask {
    pub task: TaskRow,
    pub extras: TaskExtras,
}

impl QueueTask {
    /// Decode one `download_tasks` row with the queue view's extras, over
    /// the table's one mapper.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            task: TaskRow::from_row(row)?,
            extras: TaskExtras {
                release_mbid: row.get("release_mbid")?,
                release_track_mbid: row.get("release_track_mbid")?,
                artist_mbid: row.get("artist_mbid")?,
                track_title: row.get("track_title")?,
                year: row.get("year")?,
                files_total: row.get("files_total")?,
                files_completed: row.get("files_completed")?,
                files_failed: row.get("files_failed")?,
                queue_position_start: row.get("queue_position_start")?,
                queue_position_end: row.get("queue_position_end")?,
                remote_queued: row.get::<_, i64>("remote_queued")? != 0,
                quality_bitrate: row.get("quality_bitrate")?,
                quality_bit_depth: row.get("quality_bit_depth")?,
                quality_sample_rate: row.get("quality_sample_rate")?,
                advertised_queue_depth: row.get("advertised_queue_depth")?,
                final_path: row.get("final_path")?,
                wrong_product_verdict_at: row
                    .get::<_, Option<i64>>("wrong_product_verdict_at")?
                    .map(secs_f64_from_ms),
                wrong_product_detail: row.get("wrong_product_detail")?,
            },
        })
    }
}

/// Filters for the queue list.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub status: Option<TaskStatus>,
    pub release_group_mbid: Option<String>,
    /// 1-based page.
    pub page: i64,
    pub page_size: i64,
}

/// Per-task facts drawn from the attempt journal, held files and import
/// decisions.
#[derive(Debug, Clone, Default)]
pub struct TaskFacts {
    /// Attempts that reached a download client.
    pub attempts: i64,
    /// The cleanup-state summary of the attempts.
    pub cleanup_state: Option<String>,
    /// A file of this task waits in the held list.
    pub held_for_review: bool,
    /// The newest import decision.
    pub decision: Option<DecisionFacts>,
}

/// The newest import decision of a task.
#[derive(Debug, Clone, Default)]
pub struct DecisionFacts {
    pub outcome: String,
    pub reason_code: Option<Reason>,
    pub detail: Option<String>,
    pub files_total: i64,
    pub files_imported: i64,
    pub files_held: i64,
    pub decided_at: f64,
}

/// Queue counters for the nav badge and the activity refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivitySummary {
    pub revision: i64,
    pub active_count: i64,
    pub held_count: i64,
    pub failed_count: i64,
    pub landed_release_group_mbids: Vec<String>,
}

impl DownloadStore<'_> {
    /// The viewer's tasks, newest first.
    pub fn queue_page(
        &self,
        viewer: &Viewer,
        filter: &ListFilter,
    ) -> Result<Vec<QueueTask>, StoreError> {
        let offset = (filter.page.max(1) - 1) * filter.page_size;
        let mut stmt = self.conn.prepare_cached(sql::QUEUE_PAGE.text())?;
        let rows = stmt.query_map(
            params![
                viewer.owner_filter(),
                filter.status.map(TaskStatus::as_str),
                filter.release_group_mbid.as_deref(),
                filter.page_size,
                offset,
            ],
            QueueTask::from_row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// One task as the queue view reads it, unscoped (the caller checks
    /// ownership).
    pub fn queue_task(&self, task_id: &str) -> Result<Option<QueueTask>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::TASK_BY_ID.text(),
                params![task_id],
                QueueTask::from_row,
            )
            .optional()?)
    }

    /// Facts for every task id given, keyed by task id.
    pub fn task_facts(
        &self,
        task_ids: &[String],
    ) -> Result<std::collections::HashMap<String, TaskFacts>, StoreError> {
        let mut facts: std::collections::HashMap<String, TaskFacts> = task_ids
            .iter()
            .map(|id| (id.clone(), TaskFacts::default()))
            .collect();
        if task_ids.is_empty() {
            return Ok(facts);
        }
        let len = ListLen::new(task_ids.len());
        let ids = || rusqlite::params_from_iter(task_ids.iter());

        let mut stmt = self.conn.prepare_cached(&sql::attempt_facts(len))?;
        let mut rows = stmt.query(ids())?;
        while let Some(row) = rows.next()? {
            let task_id: String = row.get(0)?;
            if let Some(entry) = facts.get_mut(&task_id) {
                entry.attempts = row.get::<_, Option<i64>>(1)?.unwrap_or(0);
                entry.cleanup_state = row.get(2)?;
            }
        }

        for task_id in HeldRows::new(self.conn).tasks_holding(task_ids)? {
            if let Some(entry) = facts.get_mut(&task_id) {
                entry.held_for_review = true;
            }
        }

        let mut stmt = self.conn.prepare_cached(&sql::decision_facts(len))?;
        let mut rows = stmt.query(ids())?;
        while let Some(row) = rows.next()? {
            let task_id: String = row.get(0)?;
            if let Some(entry) = facts.get_mut(&task_id) {
                entry.decision = Some(DecisionFacts {
                    outcome: row.get(1)?,
                    reason_code: row
                        .get::<_, Option<String>>(2)?
                        .map(|code| Reason::from_stored(&code)),
                    detail: row.get(3)?,
                    files_total: row.get(4)?,
                    files_imported: row.get(5)?,
                    files_held: row.get(6)?,
                    decided_at: secs_f64_from_ms(row.get(7)?),
                });
            }
        }
        Ok(facts)
    }

    /// The viewer's activity summary: the revision the triggers keep (global
    /// for admins, per user otherwise), live, held and failed counts, and the
    /// 20 albums that landed most recently.
    pub fn activity_summary(&self, viewer: &Viewer) -> Result<ActivitySummary, StoreError> {
        let owner = viewer.owner_filter();
        let revision: Option<i64> = if viewer.admin {
            self.conn
                .query_row(sql::GLOBAL_REVISION.text(), [], |row| row.get(0))
                .optional()?
        } else {
            self.conn
                .query_row(sql::USER_REVISION.text(), params![viewer.user_id], |row| {
                    row.get(0)
                })
                .optional()?
        };
        let (active_count, failed_count): (i64, i64) =
            self.conn
                .query_row(sql::ACTIVITY_COUNTS.text(), params![owner], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
        let held_count = HeldRows::new(self.conn).count_open(owner)?;
        let mut stmt = self.conn.prepare_cached(sql::LANDED_ALBUMS.text())?;
        let landed = stmt
            .query_map(params![owner], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ActivitySummary {
            revision: revision.unwrap_or(0),
            active_count,
            held_count,
            failed_count,
            landed_release_group_mbids: landed,
        })
    }

    /// The viewer's failed and short tasks that are still the newest task
    /// for their album or track, oldest first. Feeds both bulk actions:
    /// "stop all retries" and "retry all failed" decide per task from its
    /// schedule.
    pub fn newest_unsettled(&self, viewer: &Viewer) -> Result<Vec<TaskRow>, StoreError> {
        let mut stmt = self.conn.prepare_cached(sql::NEWEST_UNSETTLED.text())?;
        let rows = stmt.query_map(params![viewer.owner_filter()], TaskRow::from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
