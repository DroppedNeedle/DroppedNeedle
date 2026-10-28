//! The download journal: tasks, attempts, keys, and sweeps on SQLite.
//!
//! The durable store plus the lifecycle bits the orchestrator leans on: task
//! transitions, the attempt journal with revision-CAS and leases, idempotency
//! keys, quarantine rows, held-import retry, the newest-per-target retryable
//! sweep, and the stale-task query. Table shapes mirror migrations 0001/0002.
//!
//! [`DownloadStore`] runs over whatever connection it is given: a writer
//! transaction for writes, a read-pool connection for reads (see
//! [`super::journal::Journal`]). Its statements live in [`super::sql`].

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::db::statement::ListLen;
use std::cell::RefCell;

use rusqlite::{Connection, OptionalExtension, Row};

use super::plan::TaskPlan;
use super::source_tag::SourceTag;
use super::sql;
use super::state::{AttemptState, TaskStatus};
use crate::db::{ms_from_secs_f64 as ms, secs_f64_from_ms as secs};
use crate::reason::Reason;
use crate::requests::settle::{Fulfilled, on_task_settled};

/// Journal failures: the database's own, or a lifecycle write the journal
/// refuses. A write closure over the journal fails with this type, so a
/// refusal rolls the whole transaction back.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database failed, or a stored value did not decode.
    #[error("download journal: {0}")]
    Db(#[from] DbError),
    /// Out of a terminal status, or finalizing into a non-terminal one.
    #[error("invalid download transition: {0}")]
    InvalidTransition(String),
    /// A task's stored plan does not decode, or a plan does not encode.
    #[error("download plan of task {task_id}: {detail}")]
    Plan {
        /// The task the plan belongs to.
        task_id: String,
        /// What serde said.
        detail: String,
    },
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
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
    /// Why the task exists: a `DispatchOrigin` as stored text.
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
    /// never NULL, see issue #245; NULL means never linked).
    pub source_username: Option<String>,
    /// Search job id that produced the picked candidate, once linked.
    pub search_job_id: Option<String>,
    /// A person picked the linked candidate (not the automatic review).
    pub picked_by_person: bool,
    /// Last outcome, as the worker logged it.
    pub error_message: Option<String>,
    /// Last outcome, typed: what the queue explains.
    pub outcome_code: Option<Reason>,
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

/// What the worker records when a task moves for a reason: the typed
/// reason the queue explains, and the text its log keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskOutcome<'a> {
    pub reason: Reason,
    pub text: &'a str,
}

impl<'a> TaskOutcome<'a> {
    pub fn new(reason: Reason, text: &'a str) -> Self {
        Self { reason, text }
    }
}

fn unknown_value(column: &str, value: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("unknown {column} value: {value}").into(),
    )
}

impl TaskRow {
    /// Decode one `download_tasks` row: the one mapper for the table.
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let status_text: String = row.get("status")?;
        let status =
            TaskStatus::parse(&status_text).ok_or_else(|| unknown_value("status", status_text))?;
        Ok(Self {
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
            picked_by_person: row.get("picked_by_person")?,
            error_message: row.get("error_message")?,
            outcome_code: row
                .get::<_, Option<String>>("outcome_code")?
                .map(|code| Reason::from_stored(&code)),
            created_at: secs(row.get("created_at")?),
            started_at: row.get::<_, Option<i64>>("started_at")?.map(secs),
            last_polled_at: row.get::<_, Option<i64>>("last_polled_at")?.map(secs),
            completed_at: row.get::<_, Option<i64>>("completed_at")?.map(secs),
            updated_at: secs(row.get("updated_at")?),
        })
    }
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

/// The album a single-track task resolved to, as task columns.
#[derive(Debug, Clone, Default)]
pub struct TrackAlbumColumns {
    /// Release group of the album.
    pub release_group_mbid: String,
    /// The edition the track is taken from.
    pub release_mbid: String,
    /// Album title.
    pub album_title: String,
    /// Release-track MBID of the wanted track.
    pub release_track_mbid: Option<String>,
    /// Position of the wanted track on its disc.
    pub track_number: i64,
    /// Disc of the wanted track.
    pub disc_number: i64,
    /// Tracks on the edition.
    pub track_count: i64,
    /// Canonical length of the wanted track in seconds.
    pub duration_seconds: Option<f64>,
    /// Album year.
    pub year: Option<i32>,
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
    /// Why the task exists: a `DispatchOrigin` as stored text.
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
    /// The source the attempt ran on.
    pub source: SourceTag,
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
    /// Whether the client handle was journaled with the attempt.
    pub handle_journaled: bool,
    /// When the attempt was queued (unix seconds).
    pub created_at: f64,
}

impl AttemptRow {
    /// Decode one `download_attempts` row: the one mapper for the table.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let state_text: String = row.get("state")?;
        let state =
            AttemptState::parse(&state_text).ok_or_else(|| unknown_value("state", state_text))?;
        Ok(Self {
            id: row.get("id")?,
            task_id: row.get("task_id")?,
            source: row.get("source")?,
            candidate_index: row.get("candidate_index")?,
            job_name: row.get("job_name")?,
            state,
            disposition: row.get("disposition")?,
            cleanup_failures: row.get("cleanup_failures")?,
            next_retry_at: secs(row.get("next_retry_at")?),
            lease_owner: row.get("lease_owner")?,
            lease_expires_at: row.get::<_, Option<i64>>("lease_expires_at")?.map(secs),
            error_code: row.get("error_code")?,
            row_revision: row.get("row_revision")?,
            handle_journaled: !row.get::<_, String>("handle_json")?.is_empty(),
            created_at: secs(row.get("created_at")?),
        })
    }
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
    /// Block time (unix seconds; stored as milliseconds).
    pub quarantined_at: f64,
}

impl QuarantineRow {
    /// Decode one `download_quarantine` row: the one mapper for the table.
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            source: row.get("source")?,
            identity: row.get("identity")?,
            release_group_mbid: row.get("release_group_mbid")?,
            reason: row.get("reason")?,
            quarantined_at: secs(row.get("quarantined_at")?),
        })
    }
}

/// When a failed task's next automatic retry is anchored.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryAnchor {
    /// The task's status wire string.
    pub status: String,
    /// Retries the task already is.
    pub retry_count: i64,
    /// Terminal time, once terminal.
    pub completed_at: Option<f64>,
    /// Last write time.
    pub updated_at: f64,
}

/// What starting a reimport did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReimportStart {
    /// The task is processing and its attempt's files are kept for it.
    Begun,
    /// The task is missing, or not failed or partial.
    NotReimportable,
    /// Cleanup took the attempt's files first.
    FilesGone,
}

/// The download journal over one SQLite connection.
///
/// A write that settles a task also settles the requests linked to it, in
/// the same transaction; the requests that reached the library are kept
/// here until the transaction commits (see [`Self::take_fulfilled`]).
pub struct DownloadStore<'conn> {
    pub(super) conn: &'conn Connection,
    fulfilled: RefCell<Vec<Fulfilled>>,
}

impl<'conn> DownloadStore<'conn> {
    /// The connection the store runs on, for the held list's rows in the
    /// same transaction.
    pub(crate) fn conn(&self) -> &'conn Connection {
        self.conn
    }

    /// Borrow a connection as a download journal.
    pub fn new(conn: &'conn Connection) -> Self {
        Self {
            conn,
            fulfilled: RefCell::new(Vec::new()),
        }
    }

    /// The requests this store's writes brought to the library, to be
    /// announced once the transaction commits.
    pub(crate) fn take_fulfilled(&self) -> Vec<Fulfilled> {
        self.fulfilled.take()
    }

    /// Settle the requests linked to `task_id`, which settled as `status`
    /// at `now` (epoch seconds). The journal's own terminal writes call
    /// this; the status sweep calls it for a task whose requests did not
    /// follow it.
    pub fn settle_requests(
        &self,
        task_id: &str,
        status: TaskStatus,
        now: f64,
    ) -> Result<(), StoreError> {
        let at_ms = (now * 1000.0).round() as i64;
        let moved = on_task_settled(self.conn, task_id, status, at_ms)?;
        self.fulfilled.borrow_mut().extend(moved);
        Ok(())
    }

    /// Rows of one prepared statement, decoded by `map`.
    fn rows<T>(
        &self,
        text: &str,
        params: impl rusqlite::Params,
        map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, StoreError> {
        let mut stmt = self.conn.prepare_cached(text)?;
        let rows = stmt.query_map(params, map)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Insert a queued task row.
    pub fn insert_task(&self, task: &NewTask, now: f64) -> Result<(), StoreError> {
        self.conn.execute(
            sql::INSERT_TASK.text(),
            rusqlite::params![
                task.id,
                task.user_id,
                task.artist_name,
                task.album_title,
                task.release_group_mbid,
                task.origin,
                task.retry_count,
                ms(now),
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
            sql::INSERT_TRACK_TASK.text(),
            rusqlite::params![
                task.id,
                task.user_id,
                task.artist_name,
                task.album_title,
                task.release_group_mbid,
                recording_mbid,
                task.origin,
                task.retry_count,
                ms(now),
            ],
        )?;
        Ok(())
    }

    /// Newest still-active task for one album. Queued rows count as active: a queued task is a live
    /// download-in-progress from the request's view.
    pub fn newest_active_for_album(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<TaskRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::NEWEST_ACTIVE_FOR_ALBUM.text(),
                rusqlite::params![release_group_mbid],
                TaskRow::from_row,
            )
            .optional()?)
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
            sql::SET_TASK_DETAILS.text(),
            rusqlite::params![
                details.release_mbid,
                details.artist_mbid,
                details.year,
                details.track_title,
                ms(now),
                task_id,
            ],
        )?;
        Ok(())
    }

    /// Record the album a single-track task was resolved to: the release
    /// group and edition it is fetched as part of, the album title, and
    /// the track's place on that edition. The landing reads these to check
    /// the file against the album's tracklist.
    pub fn set_track_album(
        &self,
        task_id: &str,
        album: &TrackAlbumColumns,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            sql::SET_TRACK_ALBUM.text(),
            rusqlite::params![
                album.release_group_mbid,
                album.release_mbid,
                album.album_title,
                album.release_track_mbid,
                album.track_number,
                album.disc_number,
                album.track_count,
                album.duration_seconds,
                album.year,
                ms(now),
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
                sql::TASK_DETAILS.text(),
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

    /// One task's plan. A task with no row reads as the empty plan; a
    /// stored plan that does not decode is an error, never an empty plan.
    pub fn task_plan(&self, task_id: &str) -> Result<TaskPlan, StoreError> {
        let stored: Option<String> = self
            .conn
            .query_row(sql::TASK_PLAN.text(), rusqlite::params![task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(json) = stored else {
            return Ok(TaskPlan::default());
        };
        TaskPlan::decode(&json).map_err(|error| StoreError::Plan {
            task_id: task_id.to_owned(),
            detail: error.to_string(),
        })
    }

    /// Replace one task's plan.
    pub fn set_task_plan(
        &self,
        task_id: &str,
        plan: &TaskPlan,
        now: f64,
    ) -> Result<(), StoreError> {
        let json = plan.encode().map_err(|error| StoreError::Plan {
            task_id: task_id.to_owned(),
            detail: error.to_string(),
        })?;
        self.conn.execute(
            sql::SET_TASK_PLAN.text(),
            rusqlite::params![json, ms(now), task_id],
        )?;
        Ok(())
    }

    /// Fetch one task row, if it exists.
    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::TASK_BY_ID.text(),
                rusqlite::params![task_id],
                TaskRow::from_row,
            )
            .optional()?)
    }

    /// Every task currently in one of the given statuses.
    pub fn list_active(&self, statuses: &[TaskStatus]) -> Result<Vec<TaskRow>, StoreError> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let text = sql::tasks_in(ListLen::new(statuses.len()));
        let wires = statuses.iter().map(|status| status.as_str());
        self.rows(&text, rusqlite::params_from_iter(wires), TaskRow::from_row)
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
        outcome: Option<TaskOutcome<'_>>,
    ) -> Result<(), StoreError> {
        // Terminal rows never move: a retry spawns a successor task while
        // the original stays settled for audit.
        let changed = self.conn.execute(
            sql::TRANSITION_TASK.text(),
            rusqlite::params![
                status.as_str(),
                outcome.map(|outcome| outcome.text),
                ms(now),
                status.is_terminal(),
                task_id,
                outcome.map(|outcome| outcome.reason.code()),
            ],
        )?;
        if changed > 0 && status.is_terminal() {
            self.settle_requests(task_id, status, now)?;
        }
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
        self.conn
            .execute(sql::TOUCH_POLL.text(), rusqlite::params![ms(now), task_id])?;
        Ok(())
    }

    /// Link one task to the candidate picked from a search, by a person or
    /// by the automatic search review (`picked_by_person` says which): the
    /// source, the peer, the search job and the position in its list. The
    /// worker fetches exactly that candidate first; the reimport guard
    /// reads these columns back to tell linked tasks from never-started
    /// ones.
    #[allow(clippy::too_many_arguments)]
    pub fn link_candidate(
        &self,
        task_id: &str,
        source: &str,
        source_username: Option<&str>,
        search_job_id: &str,
        candidate_index: i64,
        picked_by_person: bool,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            sql::LINK_CANDIDATE.text(),
            rusqlite::params![
                source,
                source_username,
                search_job_id,
                candidate_index,
                ms(now),
                task_id,
                picked_by_person
            ],
        )?;
        Ok(())
    }

    /// Whether one task can be reimported: failed or short-landed, with
    /// an attempt whose client handle still names its files (kept on a
    /// failure, or settled without removing the files). Keyed on the
    /// attempt journal, not on the never-written candidate link.
    pub fn is_reimportable(&self, task_id: &str) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            sql::REIMPORTABLE.text(),
            rusqlite::params![task_id],
            |row| row.get(0),
        )?)
    }

    /// Start a reimport: the attempt whose files it reads (`attempt_id`)
    /// is taken back from cleanup, then a failed or
    /// short-landed task goes back to `processing` with its error and
    /// completion cleared, so the import can run against the files already
    /// on disk. This is the one write out of a terminal status;
    /// `transition_task` still refuses all others. Both moves compare and
    /// set in one transaction, so cleanup and a reimport never both act
    /// on one attempt.
    pub fn begin_reimport(
        &self,
        task_id: &str,
        attempt_id: &str,
        now: f64,
    ) -> Result<ReimportStart, StoreError> {
        let reimportable = self
            .get_task(task_id)?
            .is_some_and(|task| matches!(task.status, TaskStatus::Failed | TaskStatus::Partial));
        if !reimportable {
            return Ok(ReimportStart::NotReimportable);
        }
        let claimed = self.conn.execute(
            sql::CLAIM_REIMPORT.text(),
            rusqlite::params![ms(now), attempt_id],
        )?;
        if claimed == 0 {
            return Ok(ReimportStart::FilesGone);
        }
        self.conn.execute(
            sql::BEGIN_REIMPORT.text(),
            rusqlite::params![ms(now), task_id],
        )?;
        Ok(ReimportStart::Begun)
    }

    /// Hand one task's kept files to cleanup (discard): the task was
    /// stopped or its album removed.
    pub fn release_task_kept(&self, task_id: &str, now: f64) -> Result<(), StoreError> {
        self.conn.execute(
            sql::RELEASE_TASK_KEPT.text(),
            rusqlite::params![ms(now), task_id],
        )?;
        Ok(())
    }

    /// Hand kept files to cleanup (discard): those of cancelled or removed
    /// tasks, and those kept since before `kept_before` whose task is not
    /// running. Answers each moved attempt as (attempt id, task id).
    pub fn release_kept(
        &self,
        now: f64,
        kept_before: f64,
    ) -> Result<Vec<(String, String)>, StoreError> {
        self.rows(
            sql::RELEASE_KEPT.text(),
            rusqlite::params![ms(now), ms(kept_before)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
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
            sql::CLAIM_KEY.text(),
            rusqlite::params![key, task_id, operation, ms(now)],
        )?;
        Ok(changed == 1)
    }

    /// Task id behind an idempotency key, if the key was claimed before.
    /// Lets a repeat dispatch answer the original task instead of minting
    /// a duplicate.
    pub fn task_id_for_key(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(sql::TASK_FOR_KEY.text(), rusqlite::params![key], |row| {
                row.get::<_, String>(0)
            })
            .optional()?)
    }

    /// Release an idempotency key claimed by a dispatch whose insert then
    /// failed. Without this the key would dangle, pointing a repeat at a
    /// task id that was never written.
    pub fn release_key(&self, key: &str) -> Result<(), StoreError> {
        self.conn
            .execute(sql::RELEASE_KEY.text(), rusqlite::params![key])?;
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
            sql::INSERT_ATTEMPT.text(),
            rusqlite::params![
                attempt_id,
                task_id,
                source,
                candidate_index,
                job_name,
                handle_json,
                state.as_str(),
                ms(now),
            ],
        )?;
        Ok(())
    }

    /// Fetch one attempt row, if it exists.
    pub fn get_attempt(&self, attempt_id: &str) -> Result<Option<AttemptRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::ATTEMPT_BY_ID.text(),
                rusqlite::params![attempt_id],
                AttemptRow::from_row,
            )
            .optional()?)
    }

    /// The journaled client handle for one attempt, if any. Empty means
    /// the attempt never reached the client (a crash between journaling
    /// and enqueue); the worker re-enqueues rather than polling it.
    pub fn attempt_handle_json(&self, attempt_id: &str) -> Result<Option<String>, StoreError> {
        let handle = self
            .conn
            .query_row(
                sql::ATTEMPT_HANDLE.text(),
                rusqlite::params![attempt_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(handle.filter(|text| !text.is_empty()))
    }

    /// Stored handles of a task's attempts on one source.
    pub fn source_handles(&self, task_id: &str, source: &str) -> Result<Vec<String>, StoreError> {
        self.rows(
            sql::SOURCE_HANDLES.text(),
            rusqlite::params![task_id, source],
            |row| row.get(0),
        )
    }

    /// The newest client handle a task's attempts recorded, as stored JSON.
    pub fn newest_handle(&self, task_id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::NEWEST_HANDLE.text(),
                rusqlite::params![task_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Every attempt for a task, oldest first.
    pub fn list_attempts(&self, task_id: &str) -> Result<Vec<AttemptRow>, StoreError> {
        self.rows(
            sql::ATTEMPTS_FOR_TASK.text(),
            rusqlite::params![task_id],
            AttemptRow::from_row,
        )
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
            sql::TRANSITION_ATTEMPT.text(),
            rusqlite::params![
                state.as_str(),
                disposition,
                error_code,
                clear_lease,
                ms(now),
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
    /// Claimable rows are
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
        let expires = now + lease_seconds;
        self.conn.execute(
            sql::CLAIM_CLEANUP.text(),
            rusqlite::params![worker_id, ms(expires), ms(now), limit],
        )?;
        self.rows(
            sql::CLAIMED_ATTEMPTS.text(),
            rusqlite::params![worker_id, ms(expires)],
            AttemptRow::from_row,
        )
    }

    /// Record a deferred cleanup: bump failures, stamp the error, and push
    /// the next retry out with the cleanup backoff's growth.
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
            sql::RECORD_CLEANUP_FAILURE.text(),
            rusqlite::params![
                error_code,
                ms(now + backoff),
                ms(now),
                attempt_id,
                expected_revision,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_attempt(attempt_id)
    }

    /// Downloads whose source files cleanup could not remove: tasks with a
    /// row needing attention or a retried cleanup that failed three times.
    /// Fewer failures are a blip the next retry usually clears.
    pub fn cleanup_warning_count(&self) -> Result<u64, StoreError> {
        let count: i64 = self
            .conn
            .query_row(sql::CLEANUP_WARNING_COUNT.text(), [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// True when any journal row other than a completed one still
    /// references this job, kept files included. The orphan reconciler
    /// treats "any row" as owned; only a clean lookup with zero rows lets
    /// a folder be considered debris.
    pub fn has_cleanup_debt(
        &self,
        source: &str,
        task_id: &str,
        job_name: &str,
    ) -> Result<bool, StoreError> {
        let count: i64 = self.conn.query_row(
            sql::CLEANUP_DEBT.text(),
            rusqlite::params![source, task_id, job_name],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Settle a task terminal while keeping its attempt for audit.
    ///
    /// The task goes terminal but
    /// the attempt row survives (marked preserved or cleanup-pending), so
    /// a reaped or failed download never loses its cleanup evidence.
    pub fn finalize_task_and_attempt(
        &self,
        task_id: &str,
        status: TaskStatus,
        now: f64,
        outcome: Option<TaskOutcome<'_>>,
        attempt_id: Option<&str>,
        preserve_attempt: bool,
    ) -> Result<(), StoreError> {
        if !status.is_terminal() {
            return Err(StoreError::InvalidTransition(format!(
                "finalize of {task_id} needs a terminal status, got {}",
                status.as_str()
            )));
        }
        self.transition_task(task_id, status, now, outcome)?;
        if let Some(id) = attempt_id
            && let Some(row) = self.get_attempt(id)?
        {
            let state = if preserve_attempt {
                AttemptState::Preserved
            } else {
                AttemptState::CleanupPending
            };
            let moved = self.transition_attempt(
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
            if moved.is_none() {
                // Another writer moved the attempt between the read and the
                // write. Refuse, so the caller's transaction rolls back the
                // task settle too instead of leaving a terminal task with an
                // in-use attempt.
                return Err(StoreError::InvalidTransition(format!(
                    "attempt {id} changed while settling task {task_id}"
                )));
            }
        }
        Ok(())
    }

    /// Cancel one task: a live task goes `cancelled` and every attempt still
    /// acquiring or in use moves to `cleanup_pending` with a discard
    /// disposition, so the cleanup pass aborts the client transfer and
    /// drops its records. Answers false when the task is missing or already
    /// terminal (nothing to stop).
    pub fn cancel_task(&self, task_id: &str, now: f64) -> Result<bool, StoreError> {
        let changed = self
            .conn
            .execute(sql::CANCEL_TASK.text(), rusqlite::params![ms(now), task_id])?;
        if changed == 0 {
            return Ok(false);
        }
        self.conn.execute(
            sql::CANCEL_ATTEMPTS.text(),
            rusqlite::params![ms(now), task_id],
        )?;
        self.settle_requests(task_id, TaskStatus::Cancelled, now)?;
        Ok(true)
    }

    /// Blocklist a release by source identity, pruning expired rows first.
    ///
    /// The prune-on-write keeps the table small
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
            sql::PRUNE_QUARANTINE.text(),
            rusqlite::params![ms(now - ttl_seconds)],
        )?;
        self.conn.execute(
            sql::INSERT_QUARANTINE.text(),
            rusqlite::params![source, identity, release_group_mbid, reason, ms(now)],
        )?;
        Ok(())
    }

    /// Live `(source, identity)` blocklist pairs for fast scorer lookup.
    pub fn load_quarantine_set(
        &self,
        now: f64,
        ttl_seconds: f64,
    ) -> Result<Vec<(String, String)>, StoreError> {
        self.rows(
            sql::QUARANTINE_SET.text(),
            rusqlite::params![ms(now - ttl_seconds)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
    }

    /// Every quarantine row, newest first (for the review surface).
    pub fn list_quarantine(&self) -> Result<Vec<QuarantineRow>, StoreError> {
        self.rows(sql::QUARANTINE_ALL.text(), [], QuarantineRow::from_row)
    }

    /// One page of quarantine entries, newest first.
    pub fn quarantine_page(
        &self,
        page: i64,
        page_size: i64,
    ) -> Result<Vec<QuarantineRow>, StoreError> {
        let offset = (page.max(1) - 1) * page_size;
        self.rows(
            sql::QUARANTINE_PAGE.text(),
            rusqlite::params![page_size, offset],
            QuarantineRow::from_row,
        )
    }

    /// Clear an album's blocklist on manual retry: an explicit "try again"
    /// reconsiders releases the failed attempt quarantined. Album scope
    /// only - a per-track retry must not wipe the album's blocklist, and
    /// auto-retry never clears it.
    pub fn delete_quarantine_for_album(&self, release_group_mbid: &str) -> Result<i64, StoreError> {
        let removed = self.conn.execute(
            sql::DELETE_QUARANTINE_FOR_ALBUM.text(),
            rusqlite::params![release_group_mbid],
        )?;
        Ok(removed as i64)
    }

    /// Every distinct reason spelling the journal stores.
    pub fn stored_reason_spellings(&self) -> Result<Vec<String>, StoreError> {
        self.rows(sql::STORED_REASONS.text(), [], |row| row.get(0))
    }

    /// Retryable tasks: the newest task per target (album, or track plus
    /// user) when that newest task is terminal `failed`/`partial` under
    /// the retry ceiling. Oldest first so the most overdue retry goes
    /// first. Upgrades are excluded on both sides: a failed upgrade never
    /// auto-retries, and a newer upgrade cannot suppress a user task's
    /// legitimate retry.
    pub fn list_retryable(&self, max_retry_count: i64) -> Result<Vec<TaskRow>, StoreError> {
        self.rows(
            sql::RETRYABLE.text(),
            rusqlite::params![max_retry_count],
            TaskRow::from_row,
        )
    }

    /// The retry anchor of one task while it is still the newest task for
    /// its target, so the wanted view can show when its retry is due.
    /// `None` once a successor exists or for an upgrade.
    pub fn retry_anchor(&self, task_id: &str) -> Result<Option<RetryAnchor>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::RETRY_ANCHOR.text(),
                rusqlite::params![task_id],
                |row| {
                    Ok(RetryAnchor {
                        status: row.get(0)?,
                        retry_count: row.get(1)?,
                        completed_at: row.get::<_, Option<i64>>(2)?.map(secs),
                        updated_at: secs(row.get(3)?),
                    })
                },
            )
            .optional()?)
    }

    /// Newest task one owner started for an album or recording since a
    /// time (epoch seconds).
    pub fn task_since(
        &self,
        owner: &str,
        kind: &str,
        key: &str,
        since: f64,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::TASK_SINCE.text(),
                rusqlite::params![owner, kind, key, ms(since)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Active (`downloading`/`processing`) tasks no poller has touched
    /// within `threshold_seconds`. The reap sweep fails these; tasks with
    /// a live owner are filtered by the caller before reaping.
    pub fn list_unpolled_active(
        &self,
        now: f64,
        threshold_seconds: f64,
    ) -> Result<Vec<TaskRow>, StoreError> {
        self.rows(
            sql::UNPOLLED_ACTIVE.text(),
            rusqlite::params![ms(now - threshold_seconds)],
            TaskRow::from_row,
        )
    }
}
