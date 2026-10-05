//! The download journal: tasks, attempts, keys, and sweeps on SQLite.
//!
//! Ports the durable half of `download_store.py` plus the lifecycle bits
//! the orchestrator leans on: task transitions, the attempt journal with
//! revision-CAS and leases, idempotency keys, quarantine rows, held-import
//! retry, the newest-per-target retryable sweep, and the stale-task query.
//! Table shapes mirror migrations 0001/0002; production boots through the
//! sqlx migrator while tests build scratch databases with
//! [`apply_test_schema`], which runs the same SQL files.

use rusqlite::{Connection, OptionalExtension, Row};

use super::state::{AttemptState, TaskStatus};

/// Journal failures: SQLite errors plus unknown wire values.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The underlying SQLite call failed.
    #[error("download store failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A status or state string from the database is unknown.
    #[error("unknown download status value: {0}")]
    UnknownStatus(String),
    /// A lifecycle write the journal refuses: out of a terminal status, or
    /// finalizing into a non-terminal one.
    #[error("invalid download transition: {0}")]
    InvalidTransition(String),
}

/// Run the real migration SQL against a scratch connection.
///
/// Tests only: production applies migrations at boot through the sqlx
/// migrator. Running the same files here keeps the tests on the real
/// schema.
#[cfg(any(test, feature = "test-support"))]
pub fn apply_test_schema(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(include_str!("../../../migrations/0001_baseline.sql"))?;
    conn.execute_batch(include_str!(
        "../../../migrations/0002_download_idempotency.sql"
    ))?;
    Ok(())
}

/// Task row fields the durable core reads and writes.
#[derive(Debug, Clone)]
pub struct TaskRow {
    /// Task id (`droppedneedle` job prefix source for usenet).
    pub id: String,
    /// Owning user id.
    pub user_id: String,
    /// Artist name the task searches for.
    pub artist_name: String,
    /// Album (or track) title the task searches for.
    pub album_title: String,
    /// Current persisted status.
    pub status: TaskStatus,
    /// Why the task exists: `user`, `retry`, or `upgrade`.
    pub origin: String,
    /// `album` or `track`.
    pub download_type: String,
    /// Target release-group MBID.
    pub release_group_mbid: String,
    /// Target recording MBID for track retries.
    pub recording_mbid: Option<String>,
    /// How many retries this task already is.
    pub retry_count: i64,
    /// Selected candidate index, once picked.
    pub candidate_index: Option<i64>,
    /// Progress percent (0-100) the worker last reported.
    pub progress_percent: i64,
    /// Total transfer bytes, once known.
    pub total_size_bytes: Option<i64>,
    /// Bytes transferred so far.
    pub downloaded_bytes: i64,
    /// Picked candidate quality (format label), once picked.
    pub quality_format: Option<String>,
    /// Fetch source: `soulseek`, `usenet`, or `plugin:<key>`.
    pub source: String,
    /// Download client that owns the transfer (`slskd`, `sabnzbd`).
    pub download_client: String,
    /// Source username (soulseek) or linked marker (usenet persists `""`,
    /// never NULL, per the v2 #245 note; NULL means never linked).
    pub source_username: Option<String>,
    /// Search job id that produced the picked candidate, once linked.
    pub search_job_id: Option<String>,
    /// Last outcome text.
    pub error_message: Option<String>,
    /// Creation time (unix seconds).
    pub created_at: f64,
    /// First enqueue time, once started.
    pub started_at: Option<f64>,
    /// Last watchdog poll time.
    pub last_polled_at: Option<f64>,
    /// Terminal time, once terminal.
    pub completed_at: Option<f64>,
    /// Last write time.
    pub updated_at: f64,
}

fn unknown_value(column: &str, value: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("unknown {column} value: {value}").into(),
    )
}

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<TaskRow> {
    let status_text: String = row.get("status")?;
    let status =
        TaskStatus::parse(&status_text).ok_or_else(|| unknown_value("status", status_text))?;
    Ok(TaskRow {
        id: row.get("id")?,
        user_id: row.get("user_id")?,
        artist_name: row.get("artist_name")?,
        album_title: row.get("album_title")?,
        status,
        origin: row.get("origin")?,
        download_type: row.get("download_type")?,
        release_group_mbid: row.get("release_group_mbid")?,
        recording_mbid: row.get("recording_mbid")?,
        retry_count: row.get("retry_count")?,
        candidate_index: row.get("candidate_index")?,
        progress_percent: row.get("progress_percent")?,
        total_size_bytes: row.get("total_size_bytes")?,
        downloaded_bytes: row.get("downloaded_bytes")?,
        quality_format: row.get("quality_format")?,
        source: row.get("source")?,
        download_client: row.get("download_client")?,
        source_username: row.get("source_username")?,
        search_job_id: row.get("search_job_id")?,
        error_message: row.get("error_message")?,
        created_at: row.get("created_at")?,
        started_at: row.get("started_at")?,
        last_polled_at: row.get("last_polled_at")?,
        completed_at: row.get("completed_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// Optional task detail columns, stamped after insert.
#[derive(Debug, Clone, Default)]
pub struct TaskDetails {
    /// Pinned edition release MBID, when the ask pinned one.
    pub release_mbid: Option<String>,
    /// Artist MBID, when known.
    pub artist_mbid: Option<String>,
    /// Release year, when known.
    pub year: Option<i32>,
    /// Track title for exact-track rows.
    pub track_title: Option<String>,
}

/// Fields for a brand-new task row.
#[derive(Debug, Clone)]
pub struct NewTask {
    /// Task id.
    pub id: String,
    /// Owning user id (must exist in `auth_users`).
    pub user_id: String,
    /// Artist display name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Target release-group MBID.
    pub release_group_mbid: String,
    /// Why the task exists: `user`, `retry`, or `upgrade`.
    pub origin: String,
    /// Retry generation (0 for a fresh request).
    pub retry_count: i64,
}

/// Attempt-journal row fields the cleanup worker reads and writes.
#[derive(Debug, Clone)]
pub struct AttemptRow {
    /// Attempt id.
    pub id: String,
    /// Owning task id (no FK by design: deleting queue rows must not
    /// erase cleanup debt).
    pub task_id: String,
    /// `soulseek`, `usenet`, or `plugin:<key>`.
    pub source: String,
    /// Candidate position within the task.
    pub candidate_index: i64,
    /// Client job name (unsuffixed; SABnzbd may add `.<N>` on disk).
    pub job_name: String,
    /// Journal state.
    pub state: AttemptState,
    /// Cleanup disposition: `undecided`, `discard`, or `preserve`.
    pub disposition: String,
    /// Consecutive cleanup failures.
    pub cleanup_failures: i64,
    /// Earliest next cleanup attempt (unix seconds).
    pub next_retry_at: f64,
    /// Current lease holder, while claimed.
    pub lease_owner: Option<String>,
    /// Lease expiry (unix seconds), while claimed.
    pub lease_expires_at: Option<f64>,
    /// Last error code.
    pub error_code: Option<String>,
    /// Optimistic-concurrency revision; every transition bumps it.
    pub row_revision: i64,
}

fn attempt_from_row(row: &Row<'_>) -> rusqlite::Result<AttemptRow> {
    let state_text: String = row.get("state")?;
    let state =
        AttemptState::parse(&state_text).ok_or_else(|| unknown_value("state", state_text))?;
    Ok(AttemptRow {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        source: row.get("source")?,
        candidate_index: row.get("candidate_index")?,
        job_name: row.get("job_name")?,
        state,
        disposition: row.get("disposition")?,
        cleanup_failures: row.get("cleanup_failures")?,
        next_retry_at: row.get("next_retry_at")?,
        lease_owner: row.get("lease_owner")?,
        lease_expires_at: row.get("lease_expires_at")?,
        error_code: row.get("error_code")?,
        row_revision: row.get("row_revision")?,
    })
}

/// Quarantine (blocklist) row.
#[derive(Debug, Clone)]
pub struct QuarantineRow {
    /// Row id.
    pub id: i64,
    /// `soulseek`, `usenet`, or `plugin:<key>`.
    pub source: String,
    /// Canonical source identity.
    pub identity: String,
    /// Album scope for retry-clearing, when set.
    pub release_group_mbid: Option<String>,
    /// Why it was blocklisted.
    pub reason: String,
    /// Block time (unix seconds).
    pub quarantined_at: f64,
}

/// The download journal over one SQLite connection.
pub struct DownloadStore<'conn> {
    conn: &'conn Connection,
}

impl<'conn> DownloadStore<'conn> {
    /// Borrow a connection as a download journal.
    pub fn new(conn: &'conn Connection) -> Self {
        Self { conn }
    }

    /// Insert a queued task row.
    pub fn insert_task(&self, task: &NewTask, now: f64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO download_tasks \
             (id, user_id, artist_name, album_title, release_group_mbid, origin, \
              retry_count, status, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?)",
            rusqlite::params![
                task.id,
                task.user_id,
                task.artist_name,
                task.album_title,
                task.release_group_mbid,
                task.origin,
                task.retry_count,
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Insert a queued exact-track task row. The release-group column keeps
    /// the containing group when known (else empty); the recording MBID is
    /// the lookup key and `download_type` reads `track`.
    pub fn insert_track_task(
        &self,
        task: &NewTask,
        recording_mbid: &str,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO download_tasks \
             (id, user_id, artist_name, album_title, release_group_mbid, recording_mbid, \
              download_type, origin, retry_count, status, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'track', ?, ?, 'queued', ?, ?)",
            rusqlite::params![
                task.id,
                task.user_id,
                task.artist_name,
                task.album_title,
                task.release_group_mbid,
                recording_mbid,
                task.origin,
                task.retry_count,
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Newest still-active task for one album, for the status-sync album-row
    /// fallback. Queued rows count as active: a queued task is a live
    /// download-in-progress from the request's view.
    pub fn newest_active_for_album(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<TaskRow>, StoreError> {
        self.conn
            .query_row(
                "SELECT * FROM download_tasks \
                 WHERE release_group_mbid = ? \
                   AND status IN ('queued', 'downloading', 'processing') \
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                rusqlite::params![release_group_mbid],
                task_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Stamp optional task detail columns after insert (edition pins,
    /// artist identity, track titles). `None` leaves the column as-is.
    pub fn set_task_details(
        &self,
        task_id: &str,
        details: &TaskDetails,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE download_tasks \
             SET release_mbid = COALESCE(?, release_mbid), \
                 artist_mbid = COALESCE(?, artist_mbid), \
                 year = COALESCE(?, year), \
                 track_title = COALESCE(?, track_title), \
                 updated_at = ? \
             WHERE id = ?",
            rusqlite::params![
                details.release_mbid,
                details.artist_mbid,
                details.year,
                details.track_title,
                now,
                task_id,
            ],
        )?;
        Ok(())
    }

    /// Optional detail columns for one task (edition pins, artist
    /// identity, track titles). Missing rows read as all-`None` so a retry
    /// successor copies whatever the original carried.
    pub fn task_details(&self, task_id: &str) -> Result<TaskDetails, StoreError> {
        type DetailColumns = (Option<String>, Option<String>, Option<i32>, Option<String>);
        let row: Option<DetailColumns> = self
            .conn
            .query_row(
                "SELECT release_mbid, artist_mbid, year, track_title \
                 FROM download_tasks WHERE id = ?",
                rusqlite::params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let (release_mbid, artist_mbid, year, track_title) =
            row.unwrap_or((None, None, None, None));
        Ok(TaskDetails {
            release_mbid,
            artist_mbid,
            year,
            track_title,
        })
    }

    /// Fetch one task row, if it exists.
    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskRow>, StoreError> {
        self.conn
            .query_row(
                "SELECT * FROM download_tasks WHERE id = ?",
                rusqlite::params![task_id],
                task_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Every task currently in one of the given statuses.
    pub fn list_active(&self, statuses: &[TaskStatus]) -> Result<Vec<TaskRow>, StoreError> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = statuses.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT * FROM download_tasks WHERE status IN ({placeholders})");
        let wires: Vec<&str> = statuses.iter().map(|status| status.as_str()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(wires), task_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Move a task to a new status, stamping poll and terminal times.
    ///
    /// Terminal statuses also set `completed_at`; every transition bumps
    /// `updated_at` so the landed projection and activity revisions fire.
    pub fn transition_task(
        &self,
        task_id: &str,
        status: TaskStatus,
        now: f64,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        let terminal = status.is_terminal();
        // Terminal rows never move: a retry spawns a successor task while
        // the original stays settled for audit. The guard lives in the
        // WHERE clause so a concurrent settle cannot slip between a read
        // and this write.
        let changed = self.conn.execute(
            "UPDATE download_tasks \
             SET status = ?, error_message = COALESCE(?, error_message), \
                 last_polled_at = ?, \
                 completed_at = CASE WHEN ? THEN ? ELSE completed_at END, \
                 updated_at = ? \
             WHERE id = ? AND status NOT IN ('completed', 'partial', 'failed', 'cancelled')",
            rusqlite::params![
                status.as_str(),
                error,
                now,
                terminal as i64,
                now,
                now,
                task_id,
            ],
        )?;
        if changed == 0
            && self
                .get_task(task_id)?
                .is_some_and(|row| row.status.is_terminal())
        {
            return Err(StoreError::InvalidTransition(format!(
                "task {task_id} is terminal"
            )));
        }
        Ok(())
    }

    /// Record a watchdog poll without changing status.
    pub fn touch_poll(&self, task_id: &str, now: f64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE download_tasks SET last_polled_at = ?, updated_at = ? WHERE id = ?",
            rusqlite::params![now, now, task_id],
        )?;
        Ok(())
    }

    /// Link one task to its picked candidate: the source identity, the
    /// search job that produced it, and its position. The worker calls
    /// this when it enqueues a candidate; the reimport guard reads these
    /// columns back to tell linked tasks from never-started ones.
    pub fn link_candidate(
        &self,
        task_id: &str,
        source_username: &str,
        search_job_id: &str,
        candidate_index: i64,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE download_tasks SET source_username = ?, search_job_id = ?, \
                 candidate_index = ?, updated_at = ? WHERE id = ?",
            rusqlite::params![
                source_username,
                search_job_id,
                candidate_index,
                now,
                task_id
            ],
        )?;
        Ok(())
    }

    /// Whether one task can be reimported: failed or short-landed, with a
    /// picked candidate still linked (ports v2
    /// `download_store.get_reimportable_task_ids`, including the #245 note:
    /// usenet persists `source_username = ""`, so the NULL check only
    /// excludes never-linked tasks of either source).
    pub fn is_reimportable(&self, task_id: &str) -> Result<bool, StoreError> {
        let found: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM download_tasks WHERE id = ? \
                 AND status IN ('failed', 'partial') \
                 AND source_username IS NOT NULL \
                 AND search_job_id IS NOT NULL \
                 AND candidate_index IS NOT NULL",
                rusqlite::params![task_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Requeue one failed or short-landed task for import without
    /// re-searching: the picked candidate, source link, and search job stay
    /// on the row, so the worker resumes from the linked files instead of
    /// starting over. Answers None when the task is missing or the
    /// reimport guard above fails. This is the one intended write out of
    /// a terminal status; `transition_task` still refuses all others.
    pub fn reimport_task(&self, task_id: &str, now: f64) -> Result<Option<TaskRow>, StoreError> {
        let changed = self.conn.execute(
            "UPDATE download_tasks SET status = 'queued', error_message = NULL, \
                 last_polled_at = ?, updated_at = ? WHERE id = ? \
             AND status IN ('failed', 'partial') \
             AND source_username IS NOT NULL \
             AND search_job_id IS NOT NULL \
             AND candidate_index IS NOT NULL",
            rusqlite::params![now, now, task_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_task(task_id)
    }

    /// Claim an idempotency key. Returns true on first claim; a repeat
    /// claim returns false and must not trigger a second fetch.
    pub fn claim_key(
        &self,
        key: &str,
        task_id: &str,
        operation: &str,
        now: f64,
    ) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO download_idempotency_keys \
             (key, task_id, operation, created_at) VALUES (?, ?, ?, ?)",
            rusqlite::params![key, task_id, operation, now],
        )?;
        Ok(changed == 1)
    }

    /// Task id behind an idempotency key, if the key was claimed before.
    /// Lets a repeat dispatch answer the original task instead of minting
    /// a duplicate.
    pub fn task_id_for_key(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.conn
            .query_row(
                "SELECT task_id FROM download_idempotency_keys WHERE key = ?",
                rusqlite::params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Release an idempotency key claimed by a dispatch whose insert then
    /// failed. Without this the key would dangle, pointing a repeat at a
    /// task id that was never written.
    pub fn release_key(&self, key: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM download_idempotency_keys WHERE key = ?",
            rusqlite::params![key],
        )?;
        Ok(())
    }

    /// Insert an attempt-journal row for one candidate hand-off.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_attempt(
        &self,
        attempt_id: &str,
        task_id: &str,
        source: &str,
        candidate_index: i64,
        job_name: &str,
        handle_json: &str,
        state: AttemptState,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO download_attempts \
             (id, task_id, source, candidate_index, job_name, handle_json, \
              state, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                attempt_id,
                task_id,
                source,
                candidate_index,
                job_name,
                handle_json,
                state.as_str(),
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Fetch one attempt row, if it exists.
    pub fn get_attempt(&self, attempt_id: &str) -> Result<Option<AttemptRow>, StoreError> {
        self.conn
            .query_row(
                "SELECT * FROM download_attempts WHERE id = ?",
                rusqlite::params![attempt_id],
                attempt_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// The journaled client handle for one attempt, if any. Empty means
    /// the attempt never reached the client (a crash between journaling
    /// and enqueue); the worker re-enqueues rather than polling it.
    pub fn attempt_handle_json(&self, attempt_id: &str) -> Result<Option<String>, StoreError> {
        self.conn
            .query_row(
                "SELECT handle_json FROM download_attempts WHERE id = ?",
                rusqlite::params![attempt_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|value| value.filter(|text| !text.is_empty()))
            .map_err(StoreError::from)
    }

    /// Every attempt for a task, oldest first.
    pub fn list_attempts(&self, task_id: &str) -> Result<Vec<AttemptRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM download_attempts WHERE task_id = ? \
             ORDER BY candidate_index, created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], attempt_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Transition an attempt guarded by its revision (compare-and-swap).
    ///
    /// Returns the refreshed row, or `None` when the revision moved under
    /// the caller (another worker owns it now). The revision always bumps
    /// on success so a lost update is impossible.
    #[allow(clippy::too_many_arguments)]
    pub fn transition_attempt(
        &self,
        attempt_id: &str,
        expected_revision: i64,
        state: AttemptState,
        now: f64,
        disposition: Option<&str>,
        error_code: Option<&str>,
        clear_lease: bool,
    ) -> Result<Option<AttemptRow>, StoreError> {
        let changed = self.conn.execute(
            "UPDATE download_attempts \
             SET state = ?, \
                 disposition = COALESCE(?, disposition), \
                 error_code = ?, \
                 lease_owner = CASE WHEN ? THEN NULL ELSE lease_owner END, \
                 lease_expires_at = CASE WHEN ? THEN NULL ELSE lease_expires_at END, \
                 completed_at = CASE WHEN ? IN ('complete', 'preserved', 'needs_attention') \
                                     THEN ? ELSE completed_at END, \
                 updated_at = ?, row_revision = row_revision + 1 \
             WHERE id = ? AND row_revision = ?",
            rusqlite::params![
                state.as_str(),
                disposition,
                error_code,
                clear_lease as i64,
                clear_lease as i64,
                state.as_str(),
                now,
                now,
                attempt_id,
                expected_revision,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_attempt(attempt_id)
    }

    /// Claim due cleanup attempts for one worker.
    ///
    /// Ports `claim_download_cleanup_attempts`: claimable rows are
    /// `cleanup_pending`/`workspace_removed` (plus due `needs_attention`
    /// rechecks) whose retry time passed and whose lease is absent or
    /// expired. Claiming stamps owner plus expiry so a second worker -
    /// or a second orchestrator after failover - cannot take the same
    /// row until the lease lapses.
    pub fn claim_cleanup_attempts(
        &self,
        worker_id: &str,
        now: f64,
        limit: i64,
        lease_seconds: f64,
    ) -> Result<Vec<AttemptRow>, StoreError> {
        self.conn.execute(
            "UPDATE download_attempts \
             SET lease_owner = ?, lease_expires_at = ?, updated_at = ?, \
                 row_revision = row_revision + 1 \
             WHERE id IN ( \
               SELECT id FROM download_attempts \
               WHERE state IN ('cleanup_pending', 'workspace_removed', 'needs_attention') \
                 AND next_retry_at <= ? \
                 AND (lease_owner IS NULL OR lease_expires_at IS NULL \
                      OR lease_expires_at <= ?) \
               ORDER BY next_retry_at, created_at, id LIMIT ?)",
            rusqlite::params![worker_id, now + lease_seconds, now, now, now, limit],
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT * FROM download_attempts \
             WHERE lease_owner = ? AND lease_expires_at = ? \
             ORDER BY next_retry_at, created_at, id",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![worker_id, now + lease_seconds],
            attempt_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Record a deferred cleanup: bump failures, stamp the error, and push
    /// the next retry out with the same growth the v2 worker uses.
    pub fn record_cleanup_failure(
        &self,
        attempt_id: &str,
        expected_revision: i64,
        error_code: &str,
        now: f64,
    ) -> Result<Option<AttemptRow>, StoreError> {
        let current = self.get_attempt(attempt_id)?;
        let Some(row) = current else {
            return Ok(None);
        };
        if row.row_revision != expected_revision {
            return Ok(None);
        }
        let backoff = 300.0 * 2_f64.powi(row.cleanup_failures.min(6) as i32);
        let changed = self.conn.execute(
            "UPDATE download_attempts \
             SET cleanup_failures = cleanup_failures + 1, error_code = ?, \
                 next_retry_at = ?, lease_owner = NULL, lease_expires_at = NULL, \
                 updated_at = ?, row_revision = row_revision + 1 \
             WHERE id = ? AND row_revision = ?",
            rusqlite::params![
                error_code,
                now + backoff,
                now,
                attempt_id,
                expected_revision,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_attempt(attempt_id)
    }

    /// True when any non-terminal journal row still references this job.
    /// The orphan reconciler treats "any row" as owned; only a clean
    /// lookup with zero rows lets a folder be considered debris.
    pub fn has_cleanup_debt(
        &self,
        source: &str,
        task_id: &str,
        job_name: &str,
    ) -> Result<bool, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM download_attempts \
             WHERE source = ? AND task_id = ? AND job_name = ? \
               AND state NOT IN ('complete', 'preserved')",
            rusqlite::params![source, task_id, job_name],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Settle a task terminal while keeping its attempt for audit.
    ///
    /// Ports `_fail_task_preserving_attempt`: the task goes terminal but
    /// the attempt row survives (marked preserved or cleanup-pending), so
    /// a reaped or failed download never loses its cleanup evidence.
    pub fn finalize_task_and_attempt(
        &self,
        task_id: &str,
        status: TaskStatus,
        now: f64,
        error: Option<&str>,
        attempt_id: Option<&str>,
        preserve_attempt: bool,
    ) -> Result<(), StoreError> {
        if !status.is_terminal() {
            return Err(StoreError::InvalidTransition(format!(
                "finalize of {task_id} needs a terminal status, got {}",
                status.as_str()
            )));
        }
        self.transition_task(task_id, status, now, error)?;
        if let Some(id) = attempt_id
            && let Some(row) = self.get_attempt(id)?
        {
            let state = if preserve_attempt {
                AttemptState::Preserved
            } else {
                AttemptState::CleanupPending
            };
            self.transition_attempt(
                id,
                row.row_revision,
                state,
                now,
                Some(if preserve_attempt {
                    "preserve"
                } else {
                    "discard"
                }),
                None,
                true,
            )?;
        }
        Ok(())
    }

    /// Blocklist a release by source identity, pruning expired rows first.
    ///
    /// Ports `record_quarantine`: the prune-on-write keeps the table small
    /// and the TTL self-heal lands on disk, not just filtered on read.
    pub fn record_quarantine(
        &self,
        source: &str,
        identity: &str,
        reason: &str,
        release_group_mbid: Option<&str>,
        now: f64,
        ttl_seconds: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM download_quarantine WHERE quarantined_at < ?",
            rusqlite::params![now - ttl_seconds],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO download_quarantine \
             (source, identity, release_group_mbid, reason, quarantined_at) \
             VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![source, identity, release_group_mbid, reason, now],
        )?;
        Ok(())
    }

    /// Live `(source, identity)` blocklist pairs for fast scorer lookup.
    pub fn load_quarantine_set(
        &self,
        now: f64,
        ttl_seconds: f64,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source, identity FROM download_quarantine WHERE quarantined_at >= ?",
        )?;
        let rows = stmt.query_map(rusqlite::params![now - ttl_seconds], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Every quarantine row, newest first (for the review surface).
    pub fn list_quarantine(&self) -> Result<Vec<QuarantineRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, identity, release_group_mbid, reason, quarantined_at \
             FROM download_quarantine ORDER BY quarantined_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(QuarantineRow {
                id: row.get(0)?,
                source: row.get(1)?,
                identity: row.get(2)?,
                release_group_mbid: row.get(3)?,
                reason: row.get(4)?,
                quarantined_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Clear an album's blocklist on manual retry: an explicit "try again"
    /// reconsiders releases the failed attempt quarantined. Album scope
    /// only - a per-track retry must not wipe the album's blocklist, and
    /// auto-retry never clears it.
    pub fn delete_quarantine_for_album(&self, release_group_mbid: &str) -> Result<i64, StoreError> {
        let removed = self.conn.execute(
            "DELETE FROM download_quarantine WHERE release_group_mbid = ?",
            rusqlite::params![release_group_mbid],
        )?;
        Ok(removed as i64)
    }

    /// Insert a held-import row.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_held(
        &self,
        user_id: &str,
        held_path: &str,
        reason: &str,
        source_task_id: Option<&str>,
        release_group_mbid: Option<&str>,
        now: f64,
    ) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO held_imports \
             (user_id, held_path, reason, source_task_id, release_group_mbid, \
              status, created_at) \
             VALUES (?, ?, ?, ?, ?, 'held', ?)",
            rusqlite::params![
                user_id,
                held_path,
                reason,
                source_task_id,
                release_group_mbid,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// True when the task left a track held for review. Auto-retry pauses
    /// while this holds: re-downloading a held track would loop, and
    /// discarding the held track clears the gate.
    pub fn has_unresolved_held_for_task(&self, source_task_id: &str) -> Result<bool, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM held_imports \
             WHERE source_task_id = ? AND status = 'held'",
            rusqlite::params![source_task_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Due management-hold units: `(task_id, user_id)` pairs whose
    /// `management:*` rows have a retry time at or before now, oldest
    /// first. Ports `list_due_management_hold_units`.
    pub fn list_due_held_units(
        &self,
        now: f64,
        limit: i64,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_task_id, user_id FROM held_imports \
             WHERE status = 'held' AND source_task_id IS NOT NULL \
               AND reason LIKE 'management:%' \
               AND management_next_retry_at IS NOT NULL \
               AND management_next_retry_at <= ? \
             GROUP BY source_task_id, user_id \
             ORDER BY MIN(management_next_retry_at), MIN(id) LIMIT ?",
        )?;
        let rows = stmt.query_map(rusqlite::params![now, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Schedule the next management retry for held rows. Only `held`
    /// `management:*` rows move; anything else keeps its values.
    pub fn schedule_held_retry(
        &self,
        held_ids: &[i64],
        retry_count: i64,
        next_retry_at: Option<f64>,
    ) -> Result<(), StoreError> {
        if held_ids.is_empty() {
            return Ok(());
        }
        let placeholders = held_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE held_imports SET management_retry_count = ?, \
             management_next_retry_at = ? WHERE id IN ({placeholders}) \
             AND status = 'held' AND reason LIKE 'management:%'"
        );
        let mut params: Vec<rusqlite::types::Value> =
            vec![retry_count.into(), next_retry_at.into()];
        params.extend(held_ids.iter().map(|id| (*id).into()));
        self.conn
            .execute(&sql, rusqlite::params_from_iter(params))?;
        Ok(())
    }

    /// Retryable tasks: the newest task per target (album, or track plus
    /// user) when that newest task is terminal `failed`/`partial` under
    /// the retry ceiling. Oldest first so the most overdue retry goes
    /// first. Ports `list_retryable_tasks`, including the upgrade-origin
    /// exclusion on both sides: a failed upgrade never auto-retries, and
    /// a newer upgrade cannot suppress a user task's legitimate retry.
    pub fn list_retryable(&self, max_retry_count: i64) -> Result<Vec<TaskRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM download_tasks t \
             WHERE t.status IN ('failed', 'partial') \
               AND t.origin != 'upgrade' \
               AND t.retry_count < ? \
               AND NOT EXISTS ( \
                 SELECT 1 FROM download_tasks n \
                 WHERE n.user_id = t.user_id \
                   AND n.download_type = t.download_type \
                   AND n.release_group_mbid = t.release_group_mbid \
                   AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '') \
                   AND n.origin != 'upgrade' \
                   AND (n.created_at > t.created_at \
                        OR (n.created_at = t.created_at AND n.rowid > t.rowid))) \
             ORDER BY t.completed_at ASC NULLS LAST",
        )?;
        let rows = stmt.query_map(rusqlite::params![max_retry_count], task_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Active (`downloading`/`processing`) tasks no poller has touched
    /// within `threshold_seconds`. The reap sweep fails these; tasks with
    /// a live owner are filtered by the caller before reaping.
    pub fn list_unpolled_active(
        &self,
        now: f64,
        threshold_seconds: f64,
    ) -> Result<Vec<TaskRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM download_tasks \
             WHERE status IN ('downloading', 'processing') \
               AND COALESCE(last_polled_at, started_at, created_at, 0) < ?",
        )?;
        let rows = stmt.query_map(rusqlite::params![now - threshold_seconds], task_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}
