//! Unified download dispatch: one production journal writer behind both
//! dispatch spellings.
//!
//! Requests (`requests::dispatch::DownloadDispatch`) and flows
//! (`flows::seams::DownloadDispatch`) each define the narrow surface they
//! need from downloads. Production unifies both behind [`UnifiedDispatch`]
//! over the durable [`Journal`]: one task-id mint, one insert path, one
//! status vocabulary mapping.
//!
//! Task ids are 32 lowercase hex chars (a UUID without dashes): the
//! orphan reconciler's `job_name_parts` only recognises that shape, so
//! anything else would make debris invisible to the sweep.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use sqlx::Row;

use super::db::AcquireDb;
use super::downloads::manifest::{DownloadManifest, ManifestCodec};
use super::downloads::state::TaskStatus;
use super::downloads::store::{
    CLEANUP_DEBT_SQL, DownloadStore, NewTask, QUARANTINE_SET_SQL, REIMPORTABLE_SQL, RETRYABLE_SQL,
    StoreError, TaskDetails, TaskRow, task_from_sqlx,
};
use super::downloads::watchdog::RetryPolicy;
use super::flows::seams as flows;
use super::requests::dispatch as requests;
use crate::db::{DbError, Lane, OpError};
use crate::ids::IdGenerator;

/// Current unix time as the float seconds the journal stores.
fn now_unix_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// The download journal. Every operation is one transaction on the shared
/// writer lane, so multi-statement steps (claim a key and insert its task,
/// settle a task and its attempt) commit together or not at all.
pub struct Journal {
    db: AcquireDb,
}

impl Journal {
    /// Journal over the application database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Database handle, for the read-only queries that use the pool.
    pub fn db(&self) -> &AcquireDb {
        &self.db
    }

    /// Run one closure against the download store inside one background
    /// write transaction. A store error rolls the whole closure back.
    pub async fn run<R, F>(&self, name: &'static str, op: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.run_on(Lane::Background, name, op).await
    }

    /// Same as [`Self::run`] on the request-path lane.
    pub async fn run_foreground<R, F>(&self, name: &'static str, op: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.run_on(Lane::Foreground, name, op).await
    }

    /// Read one task through the reader pool.
    pub async fn read_task(&self, task_id: &str) -> Result<Option<TaskRow>, String> {
        let row = sqlx::query("SELECT * FROM download_tasks WHERE id = ?")
            .bind(task_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| format!("read download task: {error}"))?;
        row.as_ref()
            .map(task_from_sqlx)
            .transpose()
            .map_err(|error| format!("decode download task: {error}"))
    }

    /// Retryable tasks (see [`DownloadStore::list_retryable`]) through the
    /// reader pool.
    pub async fn read_retryable(&self, max_retry_count: i64) -> Result<Vec<TaskRow>, String> {
        let rows = sqlx::query(RETRYABLE_SQL)
            .bind(max_retry_count)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| format!("read retryable tasks: {error}"))?;
        rows.iter()
            .map(task_from_sqlx)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode retryable task: {error}"))
    }

    /// One attempt's stored client handle, if any, through the reader pool.
    pub async fn read_attempt_handle(&self, attempt_id: &str) -> Result<Option<String>, String> {
        let json: Option<String> =
            sqlx::query_scalar("SELECT handle_json FROM download_attempts WHERE id = ?")
                .bind(attempt_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(|error| format!("read attempt handle: {error}"))?;
        Ok(json.filter(|text| !text.is_empty()))
    }

    /// Stored handles of a task's attempts on one source.
    pub async fn read_source_handles(
        &self,
        task_id: &str,
        source: &str,
    ) -> Result<Vec<String>, String> {
        sqlx::query_scalar(
            "SELECT handle_json FROM download_attempts \
             WHERE task_id = ? AND source = ? AND handle_json != ''",
        )
        .bind(task_id)
        .bind(source)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| format!("read attempt handles: {error}"))
    }

    /// Whether a client job still carries cleanup debt.
    pub async fn read_cleanup_debt(
        &self,
        source: &str,
        task_id: &str,
        job_name: &str,
    ) -> Result<bool, String> {
        let count: i64 = sqlx::query_scalar(CLEANUP_DEBT_SQL)
            .bind(source)
            .bind(task_id)
            .bind(job_name)
            .fetch_one(self.db.pool())
            .await
            .map_err(|error| format!("read cleanup debt: {error}"))?;
        Ok(count > 0)
    }

    /// Live `(source, identity)` quarantine pairs.
    pub async fn read_quarantine_set(
        &self,
        now: f64,
        ttl_seconds: f64,
    ) -> Result<Vec<(String, String)>, String> {
        sqlx::query_as(QUARANTINE_SET_SQL)
            .bind(now - ttl_seconds)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| format!("read quarantine: {error}"))
    }

    async fn run_on<R, F>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.db
            .lane()
            .write(lane, name, move |tx| {
                let store = DownloadStore::new(tx);
                op(&store).map_err(|error| match error {
                    StoreError::Sqlite(error) => OpError::Sql(error),
                    other => OpError::Abort(other.to_string()),
                })
            })
            .await
            .map_err(|error: DbError| error.to_string())
    }
}

/// Live retry policy, read on every use.
pub type RetryPolicySource = Arc<dyn Fn() -> RetryPolicy + Send + Sync>;

/// One production dispatch behind both trait spellings.
pub struct UnifiedDispatch {
    journal: Arc<Journal>,
    ids: Arc<dyn IdGenerator>,
    staging_root: PathBuf,
    retry: RetryPolicySource,
}

/// The task columns the request-path reads need.
struct TaskSnapshot {
    status: TaskStatus,
    progress_percent: i64,
    total_size_bytes: Option<i64>,
    downloaded_bytes: i64,
    error_message: Option<String>,
    quality_format: Option<String>,
    source: String,
}

impl UnifiedDispatch {
    /// Wire dispatch over a shared journal. `staging_root` holds the
    /// per-task manifest skeletons written at dispatch time; `retry` reads
    /// the auto-retry policy for the wanted view's retrying rows.
    pub fn new(
        journal: Arc<Journal>,
        ids: Arc<dyn IdGenerator>,
        staging_root: PathBuf,
        retry: RetryPolicySource,
    ) -> Self {
        Self {
            journal,
            ids,
            staging_root,
            retry,
        }
    }

    /// Shared journal, for the worker and the source adapters.
    pub fn journal(&self) -> &Arc<Journal> {
        &self.journal
    }

    /// Mint a 32-hex task id.
    fn mint_task_id(&self) -> String {
        self.ids.new_id().replace('-', "").to_lowercase()
    }

    /// Insert one queued task row plus its manifest skeleton. Track rows
    /// key on the recording MBID; album and edition rows on the
    /// release-group MBID (editions carry the pinned release MBID in the
    /// row and the manifest for the library importer). The idempotency
    /// claim and the insert share one transaction: a repeat dispatch either
    /// finds the original task or nothing at all, never a key without a
    /// task.
    #[allow(clippy::too_many_arguments)]
    async fn insert(
        &self,
        user_id: &str,
        artist: &str,
        title: &str,
        is_track: bool,
        key: &str,
        origin: &str,
        release_mbid: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<String, String> {
        let task_id = self.mint_task_id();
        let now = now_unix_f64();
        let (release_group_mbid, recording_mbid) = if is_track {
            (String::new(), key.to_owned())
        } else {
            (key.to_owned(), String::new())
        };
        let task = NewTask {
            id: task_id.clone(),
            user_id: user_id.to_owned(),
            artist_name: artist.to_owned(),
            album_title: title.to_owned(),
            release_group_mbid: release_group_mbid.clone(),
            origin: origin.to_owned(),
            retry_count: 0,
        };
        let details = TaskDetails {
            release_mbid: release_mbid.map(str::to_owned),
            track_title: is_track.then(|| title.to_owned()),
            ..TaskDetails::default()
        };
        let namespaced = idempotency_key.map(|caller_key| format!("dispatch:{caller_key}"));
        let inserted = self
            .journal
            .run_foreground("downloads.dispatch", move |store| {
                if let Some(key) = &namespaced
                    && !store.claim_key(key, &task.id, "dispatch", now)?
                {
                    // Repeat dispatch: answer the original task, never a twin.
                    return Ok((store.task_id_for_key(key)?, false));
                }
                if is_track {
                    store.insert_track_task(&task, &recording_mbid, now)?;
                } else {
                    store.insert_task(&task, now)?;
                }
                store.set_task_details(&task.id, &details, now)?;
                Ok((Some(task.id.clone()), true))
            })
            .await?;
        match inserted {
            (Some(existing), false) => Ok(existing),
            (None, _) => Err("duplicate dispatch; original task unknown".to_owned()),
            (Some(task_id), true) => {
                self.write_manifest_skeleton(
                    &task_id,
                    &release_group_mbid,
                    artist,
                    title,
                    is_track,
                    origin,
                    release_mbid,
                )
                .await;
                Ok(task_id)
            }
        }
    }

    /// Best-effort manifest skeleton, written atomically (temp file, then
    /// rename). A staging failure must not fail the dispatch (the task
    /// still queues and the worker still polls); the missing manifest only
    /// steers startup recovery toward a clean restart, which is the safe
    /// direction.
    #[allow(clippy::too_many_arguments)]
    async fn write_manifest_skeleton(
        &self,
        task_id: &str,
        release_group_mbid: &str,
        artist: &str,
        title: &str,
        is_track: bool,
        origin: &str,
        release_mbid: Option<&str>,
    ) {
        let manifest = DownloadManifest {
            task_id: task_id.to_owned(),
            release_group_mbid: release_group_mbid.to_owned(),
            artist_name: artist.to_owned(),
            album_title: title.to_owned(),
            naming_template: String::new(),
            target_files: Vec::new(),
            source_username: None,
            handle: None,
            expected_tracks: Vec::new(),
            release_mbid: release_mbid.map(str::to_owned),
            artist_mbid: None,
            year: None,
            is_track,
            hold_on_wrong_track: false,
            origin: origin.to_owned(),
            requested_by_user_id: None,
            attempt_id: None,
        };
        let staging_root = self.staging_root.clone();
        let task_id = task_id.to_owned();
        let written = tokio::task::spawn_blocking(move || {
            ManifestCodec
                .write(&staging_root, &manifest)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result);
        if let Err(error) = written {
            tracing::warn!(task_id, %error, "dispatch manifest write failed");
        }
    }

    /// Request-path read of one task's state and progress columns.
    async fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, String> {
        let row = sqlx::query(
            "SELECT status, progress_percent, total_size_bytes, downloaded_bytes, \
             error_message, quality_format, source FROM download_tasks WHERE id = ?1",
        )
        .bind(task_id)
        .fetch_optional(self.journal.db().pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some(row) = row else {
            return Ok(None);
        };
        let decode = |error: sqlx::Error| error.to_string();
        let status: String = row.try_get(0).map_err(decode)?;
        let status =
            TaskStatus::parse(&status).ok_or_else(|| format!("unknown task status {status}"))?;
        Ok(Some(TaskSnapshot {
            status,
            progress_percent: row.try_get(1).map_err(decode)?,
            total_size_bytes: row.try_get(2).map_err(decode)?,
            downloaded_bytes: row.try_get(3).map_err(decode)?,
            error_message: row.try_get(4).map_err(decode)?,
            quality_format: row.try_get(5).map_err(decode)?,
            source: row.try_get(6).map_err(decode)?,
        }))
    }

    /// Cancel one task: it goes terminal and its live attempts move to
    /// cleanup with a discard disposition in the same transaction, so the
    /// worker's cleanup pass aborts the transfer and discards the client
    /// record.
    async fn cancel(&self, task_id: &str) -> Result<(), String> {
        let task_id = task_id.to_owned();
        self.journal
            .run_foreground("downloads.cancel", move |store| {
                store.cancel_task(&task_id, now_unix_f64()).map(|_| ())
            })
            .await
    }
}

/// Map a journal failure onto the requests seam.
fn failed(error: String) -> requests::DispatchError {
    requests::DispatchError::Failed(error)
}

impl requests::DownloadDispatch for UnifiedDispatch {
    fn dispatch<'a>(
        &'a self,
        request: &'a requests::DispatchRequest,
    ) -> BoxFuture<'a, Result<requests::DispatchOutcome, requests::DispatchError>> {
        Box::pin(async move {
            // Editions dispatch as album fetches; the pinned release MBID
            // rides in the row and the manifest for the library importer.
            let is_track = request.kind == "track";
            let origin = match request.origin {
                requests::DispatchOrigin::User
                | requests::DispatchOrigin::Approval
                | requests::DispatchOrigin::Edition => "user",
                requests::DispatchOrigin::Retry | requests::DispatchOrigin::Wanted => "retry",
                requests::DispatchOrigin::Upgrade => "upgrade",
            };
            self.insert(
                &request.user_id,
                &request.artist_name,
                &request.title,
                is_track,
                &request.key,
                origin,
                request.release_mbid.as_deref(),
                request.idempotency_key.as_deref(),
            )
            .await
            .map(|task_id| requests::DispatchOutcome::Dispatched { task_id })
            .map_err(failed)
        })
    }

    fn cancel_task<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<(), requests::DispatchError>> {
        Box::pin(async move { self.cancel(task_id).await.map_err(failed) })
    }

    fn task_state<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<requests::DispatchTaskState, requests::DispatchError>> {
        Box::pin(async move {
            use requests::DispatchTaskState as State;
            let snapshot = self.snapshot(task_id).await.map_err(failed)?;
            Ok(match snapshot.map(|row| row.status) {
                None => State::Missing,
                Some(TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Processing) => {
                    State::Active
                }
                Some(TaskStatus::Completed) => State::Imported,
                Some(TaskStatus::Partial) => State::Incomplete,
                Some(TaskStatus::Failed) => State::Failed,
                Some(TaskStatus::Cancelled) => State::Cancelled,
            })
        })
    }

    fn task_progress<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<requests::TaskProgress>, requests::DispatchError>> {
        Box::pin(async move {
            let snapshot = self.snapshot(task_id).await.map_err(failed)?;
            Ok(snapshot.map(|row| requests::TaskProgress {
                status: row.status.as_str().to_owned(),
                progress_percent: row.progress_percent,
                total_size_bytes: row.total_size_bytes,
                downloaded_bytes: row.downloaded_bytes,
                error_message: row.error_message,
                quality: row.quality_format,
                protocol: row.source,
            }))
        })
    }

    fn reimportable<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, requests::DispatchError>> {
        Box::pin(async move {
            sqlx::query_scalar::<_, bool>(REIMPORTABLE_SQL)
                .bind(task_id)
                .fetch_one(self.journal.db().pool())
                .await
                .map_err(|error| failed(error.to_string()))
        })
    }

    fn retry_schedule<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<requests::RetrySchedule>, requests::DispatchError>> {
        Box::pin(async move {
            let policy = (self.retry)();
            if policy.auto_retry_max() == 0 {
                return Ok(None);
            }
            // The newest task for its target is the one the retry sweep
            // acts on; once a successor exists this task is no longer
            // waiting on a retry.
            let row: Option<(String, i64, Option<f64>, f64)> = sqlx::query_as(
                "SELECT t.status, t.retry_count, t.completed_at, t.updated_at \
                 FROM download_tasks t WHERE t.id = ?1 AND t.origin != 'upgrade' \
                 AND NOT EXISTS (SELECT 1 FROM download_tasks n \
                   WHERE n.user_id = t.user_id AND n.download_type = t.download_type \
                     AND n.release_group_mbid = t.release_group_mbid \
                     AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '') \
                     AND n.origin != 'upgrade' \
                     AND (n.created_at > t.created_at \
                          OR (n.created_at = t.created_at AND n.rowid > t.rowid)))",
            )
            .bind(task_id)
            .fetch_optional(self.journal.db().pool())
            .await
            .map_err(|error| failed(error.to_string()))?;
            let Some((status, retry_count, completed_at, updated_at)) = row else {
                return Ok(None);
            };
            let retry_count = u32::try_from(retry_count).unwrap_or(0);
            let anchor = completed_at.unwrap_or(updated_at);
            Ok(policy
                .next_retry_at(retry_count, anchor, &status)
                .map(|at| requests::RetrySchedule {
                    retry_count,
                    max_attempts: policy.max_attempts,
                    next_retry_at: at.max(0.0) as u64,
                }))
        })
    }

    fn find_task_since<'a>(
        &'a self,
        owner: &'a str,
        kind: &'a str,
        key: &'a str,
        since: u64,
    ) -> BoxFuture<'a, Result<Option<String>, requests::DispatchError>> {
        Box::pin(async move {
            UnifiedDispatch::find_task_since(self, owner, kind, key, since)
                .await
                .map_err(failed)
        })
    }
}

impl UnifiedDispatch {
    /// Newest task one owner started for an album or recording since a
    /// time (epoch seconds).
    async fn find_task_since(
        &self,
        owner: &str,
        kind: &str,
        key: &str,
        since: u64,
    ) -> Result<Option<String>, String> {
        sqlx::query_scalar(
            "SELECT id FROM download_tasks WHERE user_id = ?1 \
             AND ((?2 = 'track' AND recording_mbid = ?3) \
                  OR (?2 != 'track' AND release_group_mbid = ?3)) \
             AND created_at >= ?4 ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )
        .bind(owner)
        .bind(kind)
        .bind(key)
        .bind(since as f64)
        .fetch_optional(self.journal.db().pool())
        .await
        .map_err(|error| error.to_string())
    }
}

impl flows::DownloadDispatch for UnifiedDispatch {
    fn dispatch<'a>(
        &'a self,
        request: &'a flows::DispatchRequest,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let origin = match request.origin.as_str() {
                "upgrade" => "upgrade",
                "wanted" => "retry",
                _ => "user",
            };
            self.insert(
                &request.user_id,
                &request.artist,
                &request.title,
                request.kind == flows::DispatchKind::Track,
                &request.mbid,
                origin,
                None,
                request.idempotency_key.as_deref(),
            )
            .await
        })
    }

    fn task_status<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            Ok(self.snapshot(task_id).await?.map(|row| {
                match row.status {
                    // v2's download_task.status has no queued state; a
                    // queued task is a live download-in-progress to its
                    // requester.
                    TaskStatus::Queued | TaskStatus::Downloading => "downloading",
                    TaskStatus::Processing => "processing",
                    TaskStatus::Completed => "completed",
                    TaskStatus::Partial => "partial",
                    TaskStatus::Failed => "failed",
                    TaskStatus::Cancelled => "cancelled",
                }
                .to_owned()
            }))
        })
    }

    fn active_task_for_album<'a>(
        &'a self,
        rg_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<flows::DownloadTaskView>, String>> {
        Box::pin(async move {
            let row: Option<(String, String, String)> = sqlx::query_as(
                "SELECT id, status, release_group_mbid FROM download_tasks \
                 WHERE release_group_mbid = ?1 \
                   AND status IN ('queued', 'downloading', 'processing') \
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            )
            .bind(rg_mbid)
            .fetch_optional(self.journal.db().pool())
            .await
            .map_err(|error| error.to_string())?;
            Ok(row.map(|(task_id, status, album)| flows::DownloadTaskView {
                task_id,
                status,
                album_mbid: Some(album),
            }))
        })
    }

    fn dispatch_upgrade<'a>(
        &'a self,
        request: &'a flows::DispatchRequest,
    ) -> BoxFuture<'a, Result<flows::UpgradeDispatch, String>> {
        Box::pin(async move {
            // Active-task dedup: an album already fetching gets nothing new.
            // The seam spells "nothing queued" as AlreadyInLibrary (the
            // sweep only needs to not count it); a true library-cutoff check
            // waits on a library catalog port.
            if flows::DownloadDispatch::active_task_for_album(self, &request.mbid)
                .await?
                .is_some()
            {
                return Ok(flows::UpgradeDispatch::AlreadyInLibrary);
            }
            self.insert(
                &request.user_id,
                &request.artist,
                &request.title,
                request.kind == flows::DispatchKind::Track,
                &request.mbid,
                "upgrade",
                None,
                request.idempotency_key.as_deref(),
            )
            .await
            .map(flows::UpgradeDispatch::Enqueued)
        })
    }
}
