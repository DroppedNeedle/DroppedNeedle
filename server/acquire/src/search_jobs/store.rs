//! `search_jobs` rows. Reads go through the reader pool, writes through
//! the writer lane. The candidate list is one JSON document per job
//! (`candidates_blob`), written once when the search finishes.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::candidates::Candidate;
use crate::db::AcquireDb;
use crate::downloads::store::{DownloadStore, NewTask, StoreError, TaskDetails};
use droppedneedle_kernel::db::{DbError, Lane};

/// Job statuses.
pub const SEARCHING: &str = "searching";
/// The search finished; candidates can be picked.
pub const COMPLETED: &str = "completed";
/// A candidate was picked and a download started.
pub const MATCHED: &str = "matched";
/// The search could not run.
pub const FAILED: &str = "failed";
/// The person cancelled or dismissed it.
pub const CANCELLED: &str = "cancelled";

/// What a job stores besides its columns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobPayload {
    /// The edition searched for, when known.
    pub release_mbid: Option<String>,
    /// Tracks on that edition, once read.
    pub tracks_total: Option<usize>,
    /// Ranked candidates, Soulseek first, then Usenet, then plugins.
    pub candidates: Vec<Candidate>,
}

impl JobPayload {
    /// Decode a stored document. A v2 job stored a bare list in another
    /// shape; it reads as `None` and the job as expired.
    pub fn decode(blob: &str) -> Option<Self> {
        serde_json::from_str(blob).ok()
    }

    fn encode(&self) -> Result<String, DbError> {
        Ok(serde_json::to_string(self)?)
    }
}

/// One job row.
#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: String,
    pub user_id: String,
    pub artist_name: String,
    pub album_title: String,
    pub year: Option<i32>,
    pub release_group_mbid: Option<String>,
    pub status: String,
    pub blob: String,
    /// Reason code when the job failed.
    pub error_message: Option<String>,
}

/// A new job.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub id: String,
    pub user_id: String,
    pub artist_name: String,
    pub album_title: String,
    pub year: Option<i32>,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
}

/// The download a pick starts.
#[derive(Debug, Clone)]
pub struct PickedTask {
    pub task: NewTask,
    pub details: TaskDetails,
    pub source: String,
    pub username: Option<String>,
    pub candidate_index: i64,
}

/// Why a pick was refused inside its transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickRefusal {
    /// The job is no longer open for picks; carries its status.
    NotOpen(String),
    /// The album is already downloading.
    AlbumBusy,
}

/// What the pick transaction fails with. A refusal rolls back like any
/// other failure, and the caller answers it.
#[derive(Debug, thiserror::Error)]
enum PickError {
    #[error("pick refused: {0:?}")]
    Refused(PickRefusal),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for PickError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(error.into())
    }
}

/// A download-store failure inside a job write. The text goes to the log.
fn store_failed(operation: &str, error: StoreError) -> DbError {
    DbError::WriteFailed {
        operation: operation.to_owned(),
        cause: error.to_string(),
    }
}

/// The download parked on a job: queued, linked to the job, no candidate
/// picked yet.
const PARKED_TASK_SQL: &str = "SELECT id FROM download_tasks WHERE search_job_id = ?1 \
     AND candidate_index IS NULL AND status = 'queued' ORDER BY created_at LIMIT 1";

/// What the automatic downloader decided about its search.
#[derive(Debug, Clone)]
pub enum ReviewOutcome {
    /// Take this candidate: `(index, source, username)`.
    Take(i64, String, Option<String>),
    /// Wait for a pick.
    Park,
}

/// How long an unpicked job is kept after it last changed: a week.
pub const PRUNE_AFTER_SECONDS: f64 = 7.0 * 86_400.0;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// Search jobs over the acquisition database.
#[derive(Clone)]
pub struct JobStore {
    db: AcquireDb,
}

impl JobStore {
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// The database handle (for the wanted store).
    pub fn db(&self) -> &AcquireDb {
        &self.db
    }

    /// Insert a job in `searching`.
    pub async fn insert(&self, job: NewJob) -> Result<(), String> {
        let payload = JobPayload {
            release_mbid: job.release_mbid.clone(),
            ..JobPayload::default()
        };
        self.db
            .write("search_jobs.insert", move |tx| {
                let at = now();
                tx.execute(
                    "INSERT INTO search_jobs (id, user_id, artist_name, album_title, year, \
                     release_group_mbid, search_query, status, candidates_blob, created_at, \
                     updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                    params![
                        job.id,
                        job.user_id,
                        job.artist_name,
                        job.album_title,
                        job.year,
                        job.release_group_mbid,
                        format!("{} - {}", job.artist_name, job.album_title),
                        SEARCHING,
                        payload.encode()?,
                        at,
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// One job.
    pub async fn get(&self, job_id: &str) -> Result<Option<JobRow>, String> {
        let row = sqlx::query(
            "SELECT id, user_id, artist_name, album_title, year, release_group_mbid, status, \
             candidates_blob, error_message FROM search_jobs WHERE id = ?1",
        )
        .bind(job_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some(row) = row else {
            return Ok(None);
        };
        let decode = |error: sqlx::Error| error.to_string();
        Ok(Some(JobRow {
            id: row.try_get(0).map_err(decode)?,
            user_id: row.try_get(1).map_err(decode)?,
            artist_name: row.try_get(2).map_err(decode)?,
            album_title: row.try_get(3).map_err(decode)?,
            year: row
                .try_get::<Option<i64>, _>(4)
                .map_err(decode)?
                .and_then(|year| i32::try_from(year).ok()),
            release_group_mbid: row
                .try_get::<Option<String>, _>(5)
                .map_err(decode)?
                .filter(|mbid| !mbid.is_empty()),
            status: row.try_get(6).map_err(decode)?,
            blob: row.try_get(7).map_err(decode)?,
            error_message: row.try_get(8).map_err(decode)?,
        }))
    }

    /// The download a pick started from this job, if any.
    pub async fn picked_task(&self, job_id: &str) -> Result<Option<String>, String> {
        sqlx::query_scalar(
            "SELECT id FROM download_tasks WHERE search_job_id = ?1 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(job_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())
    }

    /// Store the outcome of a search that is still running. Answers false
    /// when the job moved meanwhile (cancelled), so nothing is overwritten.
    pub async fn finish(
        &self,
        job_id: &str,
        status: &'static str,
        payload: JobPayload,
        reason: Option<&'static str>,
    ) -> Result<bool, String> {
        let job_id = job_id.to_owned();
        self.db
            .write_background("search_jobs.finish", move |tx| {
                let at = now();
                let changed = tx.execute(
                    "UPDATE search_jobs SET status = ?2, candidates_blob = ?3, \
                     error_message = ?4, completed_at = ?5, updated_at = ?5 \
                     WHERE id = ?1 AND status = ?6",
                    params![job_id, status, payload.encode()?, reason, at, SEARCHING],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Record the automatic downloader's search for one queued task, in
    /// one transaction: the finished job with its candidates, then either
    /// the taken candidate linked to the task (the job reads `matched`) or
    /// the task parked on the job until someone picks. Answers false, and
    /// writes nothing, when the task is no longer queued and unlinked
    /// (cancelled or picked meanwhile).
    pub async fn record_review(
        &self,
        job: NewJob,
        payload: JobPayload,
        task_id: &str,
        outcome: ReviewOutcome,
    ) -> Result<bool, String> {
        let task_id = task_id.to_owned();
        self.db
            .write("search_jobs.review", move |tx| {
                let at = now();
                let open: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM download_tasks WHERE id = ?1 \
                     AND status = 'queued' AND search_job_id IS NULL)",
                    params![task_id],
                    |row| row.get(0),
                )?;
                if !open {
                    return Ok(false);
                }
                let status = match outcome {
                    ReviewOutcome::Take(..) => MATCHED,
                    ReviewOutcome::Park => COMPLETED,
                };
                tx.execute(
                    "INSERT INTO search_jobs (id, user_id, artist_name, album_title, year, \
                     release_group_mbid, search_query, status, candidates_blob, created_at, \
                     updated_at, completed_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)",
                    params![
                        job.id,
                        job.user_id,
                        job.artist_name,
                        job.album_title,
                        job.year,
                        job.release_group_mbid,
                        format!("{} - {}", job.artist_name, job.album_title),
                        status,
                        payload.encode()?,
                        at,
                    ],
                )?;
                match outcome {
                    ReviewOutcome::Take(index, source, username) => DownloadStore::new(tx)
                        .link_candidate(&task_id, &source, username.as_deref(), &job.id, index, at)
                        .map_err(|error| store_failed("search_jobs.review", error))?,
                    ReviewOutcome::Park => {
                        tx.execute(
                            "UPDATE download_tasks SET search_job_id = ?2, updated_at = ?3 \
                             WHERE id = ?1",
                            params![task_id, job.id, at],
                        )?;
                    }
                }
                Ok(true)
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Close a job (cancel or dismiss): it moves to `cancelled` from
    /// searching, completed or failed, and a download parked on it waiting
    /// for a pick is cancelled with it, so it never waits forever. Answers
    /// whether the job moved.
    pub async fn close(&self, job_id: &str) -> Result<bool, String> {
        let job_id = job_id.to_owned();
        self.db
            .write("search_jobs.close", move |tx| {
                let at = now();
                let moved = tx.execute(
                    "UPDATE search_jobs SET status = ?2, updated_at = ?3 \
                     WHERE id = ?1 AND status IN (?4, ?5, ?6)",
                    params![job_id, CANCELLED, at, SEARCHING, COMPLETED, FAILED],
                )?;
                let parked: Option<String> = tx
                    .query_row(PARKED_TASK_SQL, params![job_id], |row| row.get(0))
                    .optional()?;
                if let Some(task_id) = parked {
                    DownloadStore::new(tx)
                        .cancel_task(&task_id, at)
                        .map_err(|error| store_failed("search_jobs.close", error))?;
                }
                Ok(moved > 0)
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Move a job to `to` when its status is one of `from`. Answers
    /// whether it moved.
    pub async fn transition(
        &self,
        job_id: &str,
        to: &'static str,
        from: &'static [&'static str],
        reason: Option<&'static str>,
    ) -> Result<bool, String> {
        let job_id = job_id.to_owned();
        self.db
            .write("search_jobs.transition", move |tx| {
                let at = now();
                let mut moved = 0;
                for status in from {
                    moved += tx.execute(
                        "UPDATE search_jobs SET status = ?2, error_message = COALESCE(?3, \
                         error_message), updated_at = ?4 WHERE id = ?1 AND status = ?5",
                        params![job_id, to, reason, at, status],
                    )?;
                }
                Ok(moved > 0)
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Delete jobs nobody needs any more: unpicked ones (searching,
    /// completed, failed, cancelled) last touched more than
    /// [`PRUNE_AFTER_SECONDS`] before `now`, and picked ones whose download
    /// finished or was removed. A job a download is parked on stays for as
    /// long as the download waits. A picked task that outlives its job just
    /// searches as usual on failover. Answers how many rows went.
    pub async fn prune(&self, now: f64) -> Result<u64, String> {
        let cutoff = now - PRUNE_AFTER_SECONDS;
        self.db
            .write_background("search_jobs.prune", move |tx| {
                let stale = tx.execute(
                    "DELETE FROM search_jobs WHERE status IN (?1, ?2, ?3, ?4) \
                     AND updated_at < ?5 AND NOT EXISTS ( \
                     SELECT 1 FROM download_tasks t WHERE t.search_job_id = search_jobs.id \
                     AND t.status = 'queued')",
                    params![SEARCHING, COMPLETED, FAILED, CANCELLED, cutoff],
                )?;
                let picked = tx.execute(
                    "DELETE FROM search_jobs WHERE status = ?1 AND NOT EXISTS ( \
                     SELECT 1 FROM download_tasks t WHERE t.search_job_id = search_jobs.id \
                     AND t.status IN ('queued', 'downloading', 'processing'))",
                    params![MATCHED],
                )?;
                Ok(u64::try_from(stale + picked).unwrap_or(u64::MAX))
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Start the picked download in one transaction: the job must still be
    /// `completed`. A download parked on this job waiting for a pick is
    /// linked to the candidate and resumes; otherwise no download of the
    /// album may be running and a new task is queued, linked to the
    /// candidate. The job becomes `matched`. Answers the task's id.
    pub async fn pick(
        &self,
        job_id: &str,
        picked: PickedTask,
    ) -> Result<Result<String, PickRefusal>, String> {
        let job_id = job_id.to_owned();
        let outcome = self
            .db
            .db()
            .write(Lane::Foreground, "search_jobs.pick", move |tx| {
                let at = now();
                let status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM search_jobs WHERE id = ?1",
                        params![job_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let status = status.unwrap_or_default();
                if status != COMPLETED {
                    return Err(PickError::Refused(PickRefusal::NotOpen(status)));
                }
                let parked: Option<String> = tx
                    .query_row(PARKED_TASK_SQL, params![job_id], |row| row.get(0))
                    .optional()?;
                let store = DownloadStore::new(tx);
                let store_error = |error| PickError::Db(store_failed("search_jobs.pick", error));
                if let Some(task_id) = parked {
                    store
                        .link_candidate(
                            &task_id,
                            &picked.source,
                            picked.username.as_deref(),
                            &job_id,
                            picked.candidate_index,
                            at,
                        )
                        .map_err(store_error)?;
                    tx.execute(
                        "UPDATE search_jobs SET status = ?2, updated_at = ?3 WHERE id = ?1",
                        params![job_id, MATCHED, at],
                    )?;
                    return Ok(task_id);
                }
                if !picked.task.release_group_mbid.is_empty()
                    && store
                        .newest_active_for_album(&picked.task.release_group_mbid)
                        .map_err(store_error)?
                        .is_some()
                {
                    return Err(PickError::Refused(PickRefusal::AlbumBusy));
                }
                store.insert_task(&picked.task, at).map_err(store_error)?;
                store
                    .set_task_details(&picked.task.id, &picked.details, at)
                    .map_err(store_error)?;
                store
                    .link_candidate(
                        &picked.task.id,
                        &picked.source,
                        picked.username.as_deref(),
                        &job_id,
                        picked.candidate_index,
                        at,
                    )
                    .map_err(store_error)?;
                tx.execute(
                    "UPDATE search_jobs SET status = ?2, updated_at = ?3 WHERE id = ?1",
                    params![job_id, MATCHED, at],
                )?;
                Ok(picked.task.id.clone())
            })
            .await;
        match outcome {
            Ok(task_id) => Ok(Ok(task_id)),
            Err(PickError::Refused(refusal)) => Ok(Err(refusal)),
            Err(PickError::Db(error)) => Err(error.to_string()),
        }
    }
}
