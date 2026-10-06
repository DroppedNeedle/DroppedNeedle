//! Journal rows behind the download queue view.
//!
//! Writes run on the writer lane through [`DownloadStore`]: live progress,
//! cancel and stop-retrying, retry successors, clearing finished rows and
//! removing quarantine entries. Reads run on the reader pool: the scoped
//! task list, the per-task attempt and decision summaries the cards show,
//! the activity summary and the quarantine list.
//!
//! "Scoped" means what v2 did: an admin sees every user's tasks, anyone
//! else only their own.

use rusqlite::params;
use sqlx::{Row as _, SqlitePool, sqlite::SqliteRow};

use super::state::TaskStatus;
use super::store::{DownloadStore, NewTask, QuarantineRow, StoreError, TaskRow, task_from_sqlx};

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

impl DownloadStore<'_> {
    /// Store one poll's progress on a live task and stamp the poll.
    pub fn record_progress(
        &self,
        task_id: &str,
        progress: &ProgressColumns,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE download_tasks SET downloaded_bytes = ?2, \
                 total_size_bytes = COALESCE(?3, total_size_bytes), progress_percent = ?4, \
                 files_total = ?5, files_completed = ?6, files_failed = ?7, \
                 queue_position_start = ?8, queue_position_end = ?9, remote_queued = ?10, \
                 last_polled_at = ?11 \
             WHERE id = ?1 AND status IN ('downloading', 'processing')",
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
                now,
            ],
        )?;
        Ok(())
    }

    /// Stop one task. A live task is cancelled (its attempts go to cleanup
    /// with a discard disposition). A failed or short task is marked
    /// cancelled so the retry sweep skips it; its kept files stay for the
    /// cleanup policy that already owns them. Either way the owner's
    /// wanted watch on the album stops too, so it cannot start the task
    /// again (v2 #255: one action, no hidden second switch).
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
                let changed = self.conn.execute(
                    "UPDATE download_tasks SET status = 'cancelled', cancelled_at = ?2, \
                         remote_queued = 0, updated_at = ?2 \
                     WHERE id = ?1 AND status IN ('failed', 'partial')",
                    params![task.id, now],
                )?;
                if changed == 0 {
                    Stopped::AlreadySettled
                } else {
                    Stopped::RetriesStopped
                }
            }
            TaskStatus::Completed | TaskStatus::Cancelled => Stopped::AlreadySettled,
        };
        if outcome != Stopped::AlreadySettled && !task.release_group_mbid.is_empty() {
            self.conn.execute(
                "UPDATE wanted_watches SET state = 'stopped' \
                 WHERE release_group_mbid_lower = lower(?1) AND user_id = ?2 \
                   AND state = 'watching'",
                params![task.release_group_mbid, task.user_id],
            )?;
        }
        Ok(outcome)
    }

    /// Insert a retry successor of `source` unless it already exists. The
    /// edition pin and track identity ride along, so a retried edition
    /// keeps its pinned release. Answers whether a row was created.
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
        if source.download_type == "track" {
            let recording = source.recording_mbid.clone().unwrap_or_default();
            self.insert_track_task(successor, &recording, now)?;
        } else {
            self.insert_task(successor, now)?;
        }
        self.set_task_details(&successor.id, &details, now)?;
        Ok(true)
    }

    /// Point the request linked to `from` at `to`, so the request card
    /// follows the retry instead of showing the old failure.
    pub fn relink_request(&self, from: &str, to: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE request_history SET download_task_id = ?2, generation = generation + 1 \
             WHERE download_task_id = ?1",
            params![from, to],
        )?;
        Ok(())
    }

    /// Delete finished (completed and cancelled) tasks, for one owner or,
    /// with `None`, for everyone. Attempt rows stay: their cleanup debt
    /// outlives the queue row by design.
    pub fn clear_finished(&self, owner: Option<&str>) -> Result<i64, StoreError> {
        let removed = self.conn.execute(
            "DELETE FROM download_tasks WHERE status IN ('completed', 'cancelled') \
             AND (?1 IS NULL OR user_id = ?1)",
            params![owner],
        )?;
        Ok(removed as i64)
    }

    /// Remove one quarantine entry, so its release may be tried again.
    /// Answers false when no such entry exists.
    pub fn delete_quarantine(&self, id: i64) -> Result<bool, StoreError> {
        let removed = self
            .conn
            .execute("DELETE FROM download_quarantine WHERE id = ?1", params![id])?;
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

fn queue_task(row: &SqliteRow) -> Result<QueueTask, sqlx::Error> {
    Ok(QueueTask {
        task: task_from_sqlx(row)?,
        extras: TaskExtras {
            release_mbid: row.try_get("release_mbid")?,
            release_track_mbid: row.try_get("release_track_mbid")?,
            artist_mbid: row.try_get("artist_mbid")?,
            track_title: row.try_get("track_title")?,
            year: row.try_get("year")?,
            files_total: row.try_get("files_total")?,
            files_completed: row.try_get("files_completed")?,
            files_failed: row.try_get("files_failed")?,
            queue_position_start: row.try_get("queue_position_start")?,
            queue_position_end: row.try_get("queue_position_end")?,
            remote_queued: row.try_get::<i64, _>("remote_queued")? != 0,
            quality_bitrate: row.try_get("quality_bitrate")?,
            quality_bit_depth: row.try_get("quality_bit_depth")?,
            quality_sample_rate: row.try_get("quality_sample_rate")?,
            advertised_queue_depth: row.try_get("advertised_queue_depth")?,
            final_path: row.try_get("final_path")?,
            wrong_product_verdict_at: row.try_get("wrong_product_verdict_at")?,
            wrong_product_detail: row.try_get("wrong_product_detail")?,
        },
    })
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

/// The viewer's tasks, newest first.
pub async fn list_tasks(
    pool: &SqlitePool,
    viewer: &Viewer,
    filter: &ListFilter,
) -> Result<Vec<QueueTask>, sqlx::Error> {
    let offset = (filter.page.max(1) - 1) * filter.page_size;
    let rows = sqlx::query(
        "SELECT * FROM download_tasks \
         WHERE (?1 IS NULL OR user_id = ?1) AND (?2 IS NULL OR status = ?2) \
           AND (?3 IS NULL OR release_group_mbid = ?3) \
         ORDER BY created_at DESC, rowid DESC LIMIT ?4 OFFSET ?5",
    )
    .bind(viewer.owner_filter())
    .bind(filter.status.map(TaskStatus::as_str))
    .bind(filter.release_group_mbid.as_deref())
    .bind(filter.page_size)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    rows.iter().map(queue_task).collect()
}

/// One task, unscoped (the caller checks ownership).
pub async fn get_task(pool: &SqlitePool, task_id: &str) -> Result<Option<QueueTask>, sqlx::Error> {
    sqlx::query("SELECT * FROM download_tasks WHERE id = ?1")
        .bind(task_id)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(queue_task)
        .transpose()
}

/// Per-task facts drawn from the attempt journal, held files and import
/// decisions.
#[derive(Debug, Clone, Default)]
pub struct TaskFacts {
    /// Attempts that reached a download client.
    pub attempts: i64,
    /// v2's `acquisition_cleanup_state` summary of the attempts.
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
    pub reason_code: Option<String>,
    pub reason_text: Option<String>,
    pub reason_action: Option<String>,
    pub detail: Option<String>,
    pub files_total: i64,
    pub files_imported: i64,
    pub files_held: i64,
    pub decided_at: f64,
}

/// `?,?,?` for `count` binds.
fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

/// Facts for every task id given, keyed by task id.
pub async fn task_facts(
    pool: &SqlitePool,
    task_ids: &[String],
) -> Result<std::collections::HashMap<String, TaskFacts>, sqlx::Error> {
    let mut facts: std::collections::HashMap<String, TaskFacts> = task_ids
        .iter()
        .map(|id| (id.clone(), TaskFacts::default()))
        .collect();
    if task_ids.is_empty() {
        return Ok(facts);
    }
    let marks = placeholders(task_ids.len());

    let sql = format!(
        "SELECT task_id, SUM(handle_json != '') AS handled, \
           CASE \
             WHEN MAX(state = 'needs_attention') THEN 'needs_attention' \
             WHEN MAX(state = 'preserved') THEN 'preserved' \
             WHEN MAX(state IN ('cleanup_pending', 'workspace_removed')) THEN 'pending' \
             WHEN MAX(state IN ('acquiring', 'in_use')) THEN 'in_use' \
             WHEN MAX(state = 'complete') THEN 'complete' \
             ELSE 'not_tracked' \
           END AS cleanup_state \
         FROM download_attempts WHERE task_id IN ({marks}) GROUP BY task_id"
    );
    let mut query = sqlx::query(&sql);
    for id in task_ids {
        query = query.bind(id);
    }
    for row in query.fetch_all(pool).await? {
        let task_id: String = row.try_get("task_id")?;
        if let Some(entry) = facts.get_mut(&task_id) {
            entry.attempts = row.try_get::<Option<i64>, _>("handled")?.unwrap_or(0);
            entry.cleanup_state = row.try_get("cleanup_state")?;
        }
    }

    let sql = format!(
        "SELECT DISTINCT source_task_id FROM held_imports \
         WHERE status = 'held' AND source_task_id IN ({marks})"
    );
    let mut query = sqlx::query_scalar::<_, String>(&sql);
    for id in task_ids {
        query = query.bind(id);
    }
    for task_id in query.fetch_all(pool).await? {
        if let Some(entry) = facts.get_mut(&task_id) {
            entry.held_for_review = true;
        }
    }

    let sql = format!(
        "SELECT d.task_id, d.outcome, d.reason_code, d.reason_text, d.reason_action, d.detail, \
                d.files_total, d.files_imported, d.files_held, d.decided_at \
         FROM download_import_decisions d \
         WHERE d.task_id IN ({marks}) AND d.id = ( \
           SELECT n.id FROM download_import_decisions n WHERE n.task_id = d.task_id \
           ORDER BY n.decided_at DESC, n.id DESC LIMIT 1)"
    );
    let mut query = sqlx::query(&sql);
    for id in task_ids {
        query = query.bind(id);
    }
    for row in query.fetch_all(pool).await? {
        let task_id: String = row.try_get("task_id")?;
        if let Some(entry) = facts.get_mut(&task_id) {
            entry.decision = Some(DecisionFacts {
                outcome: row.try_get("outcome")?,
                reason_code: row.try_get("reason_code")?,
                reason_text: row.try_get("reason_text")?,
                reason_action: row.try_get("reason_action")?,
                detail: row.try_get("detail")?,
                files_total: row.try_get("files_total")?,
                files_imported: row.try_get("files_imported")?,
                files_held: row.try_get("files_held")?,
                decided_at: row.try_get("decided_at")?,
            });
        }
    }
    Ok(facts)
}

/// The newest client handle a task's attempts recorded, as stored JSON.
pub async fn newest_handle(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT handle_json FROM download_attempts \
         WHERE task_id = ?1 AND handle_json != '' \
         ORDER BY candidate_index DESC, created_at DESC LIMIT 1",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await
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

/// The viewer's activity summary: the revision the triggers keep (global
/// for admins, per user otherwise), live, held and failed counts, and the
/// 20 albums that landed most recently.
pub async fn activity_summary(
    pool: &SqlitePool,
    viewer: &Viewer,
) -> Result<ActivitySummary, sqlx::Error> {
    let owner = viewer.owner_filter();
    let revision: Option<i64> = if viewer.admin {
        sqlx::query_scalar(
            "SELECT revision FROM download_activity_global_revision WHERE singleton = 1",
        )
        .fetch_optional(pool)
        .await?
    } else {
        sqlx::query_scalar(
            "SELECT revision FROM download_activity_user_revisions WHERE user_id = ?1",
        )
        .bind(&viewer.user_id)
        .fetch_optional(pool)
        .await?
    };
    let counts = sqlx::query(
        "SELECT \
           COALESCE(SUM(status IN ('queued', 'downloading', 'processing')), 0) AS active, \
           COALESCE(SUM(status = 'failed'), 0) AS failed \
         FROM download_tasks WHERE (?1 IS NULL OR user_id = ?1)",
    )
    .bind(owner)
    .fetch_one(pool)
    .await?;
    let held: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM held_imports WHERE status = 'held' AND (?1 IS NULL OR user_id = ?1)",
    )
    .bind(owner)
    .fetch_one(pool)
    .await?;
    let landed: Vec<String> = sqlx::query_scalar(
        "SELECT release_group_mbid FROM download_tasks \
         WHERE status IN ('completed', 'partial') AND release_group_mbid != '' \
           AND (?1 IS NULL OR user_id = ?1) \
         GROUP BY release_group_mbid \
         ORDER BY MAX(COALESCE(completed_at, updated_at)) DESC, release_group_mbid ASC LIMIT 20",
    )
    .bind(owner)
    .fetch_all(pool)
    .await?;
    Ok(ActivitySummary {
        revision: revision.unwrap_or(0),
        active_count: counts.try_get("active")?,
        held_count: held,
        failed_count: counts.try_get("failed")?,
        landed_release_group_mbids: landed,
    })
}

/// The viewer's failed and short tasks that are still the newest task for
/// their album or track, oldest first. Feeds both bulk actions: "stop all
/// retries" and "retry all failed" decide per task from its schedule.
pub async fn newest_unsettled(
    pool: &SqlitePool,
    viewer: &Viewer,
) -> Result<Vec<TaskRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT * FROM download_tasks t \
         WHERE t.status IN ('failed', 'partial') AND (?1 IS NULL OR t.user_id = ?1) \
           AND NOT EXISTS ( \
             SELECT 1 FROM download_tasks n \
             WHERE n.user_id = t.user_id AND n.download_type = t.download_type \
               AND n.release_group_mbid = t.release_group_mbid \
               AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '') \
               AND (n.created_at > t.created_at \
                    OR (n.created_at = t.created_at AND n.rowid > t.rowid))) \
         ORDER BY t.created_at ASC",
    )
    .bind(viewer.owner_filter())
    .fetch_all(pool)
    .await?;
    rows.iter().map(task_from_sqlx).collect()
}

/// One page of quarantine entries, newest first.
pub async fn list_quarantine(
    pool: &SqlitePool,
    page: i64,
    page_size: i64,
) -> Result<Vec<QuarantineRow>, sqlx::Error> {
    let offset = (page.max(1) - 1) * page_size;
    let rows = sqlx::query(
        "SELECT id, source, identity, release_group_mbid, reason, quarantined_at \
         FROM download_quarantine ORDER BY quarantined_at DESC, id DESC LIMIT ?1 OFFSET ?2",
    )
    .bind(page_size)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(QuarantineRow {
                id: row.try_get("id")?,
                source: row.try_get("source")?,
                identity: row.try_get("identity")?,
                release_group_mbid: row.try_get("release_group_mbid")?,
                reason: row.try_get("reason")?,
                quarantined_at: row.try_get("quarantined_at")?,
            })
        })
        .collect()
}
