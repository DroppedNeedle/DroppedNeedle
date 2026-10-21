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

use super::downloads::journal::Journal;
use super::downloads::manifest::{DownloadManifest, ManifestCodec, PlannedAlbum, TrackPosition};
use super::downloads::state::TaskStatus;
use super::downloads::store::{NewTask, TaskDetails, TaskRow};
use super::downloads::watchdog::RetryPolicy;
use super::edition::chosen_edition;
use super::flows::seams as flows;
use super::requests::dispatch as requests;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

/// Current unix time as the float seconds the journal stores.
fn now_unix_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// One production dispatch behind both trait spellings.
pub struct UnifiedDispatch {
    journal: Arc<Journal>,
    ids: Arc<dyn IdGenerator>,
    staging_root: PathBuf,
    policy: Live<DownloadPolicy>,
}

impl UnifiedDispatch {
    /// Wire dispatch over a shared journal. `staging_root` holds the
    /// per-task manifest skeletons written at dispatch time; `policy`
    /// holds the auto-retry policy for the wanted view's retrying rows.
    pub fn new(
        journal: Arc<Journal>,
        ids: Arc<dyn IdGenerator>,
        staging_root: PathBuf,
        policy: Live<DownloadPolicy>,
    ) -> Self {
        Self {
            journal,
            ids,
            staging_root,
            policy,
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
        wanted: &[(u32, u32)],
        planned_album: Option<PlannedAlbum>,
        idempotency_key: Option<&str>,
    ) -> Result<String, String> {
        // Wanted positions and the copy fetched for belong to the caller's
        // release; they mean nothing on an edition picked here.
        let on_release = release_mbid.is_some() && !is_track;
        let wanted: Vec<TrackPosition> = if on_release {
            wanted
                .iter()
                .map(|&(disc, track)| TrackPosition { disc, track })
                .collect()
        } else {
            Vec::new()
        };
        let planned_album = planned_album.filter(|_| on_release);
        let task_id = self.mint_task_id();
        let now = now_unix_f64();
        // An album ask without a release fetches the edition the library
        // has chosen for that album (a single track resolves its album
        // when it is searched for).
        let chosen = match release_mbid {
            None if !is_track => match chosen_edition(self.journal.db().pool(), key).await {
                Ok(chosen) => chosen.map(|edition| edition.release_mbid),
                Err(error) => {
                    tracing::warn!(key, %error, "chosen edition unreadable; dispatching the album");
                    None
                }
            },
            _ => None,
        };
        let release_mbid = release_mbid.or(chosen.as_deref());
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
            .await
            .map_err(|error| error.to_string())?;
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
                    wanted,
                    planned_album,
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
        album_wanted: Vec<TrackPosition>,
        planned_album: Option<PlannedAlbum>,
    ) {
        let manifest = DownloadManifest {
            task_id: task_id.to_owned(),
            release_group_mbid: release_group_mbid.to_owned(),
            artist_name: artist.to_owned(),
            album_title: title.to_owned(),
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
            track_album: None,
            album_wanted,
            planned_album,
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

    /// Request-path read of one task, on the read pool.
    async fn snapshot(&self, task_id: &str) -> Result<Option<TaskRow>, String> {
        self.journal
            .read_task(task_id)
            .await
            .map_err(|error| error.to_string())
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
            .map_err(|error| error.to_string())
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
                &request.wanted,
                request.planned_album.clone(),
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
            self.journal
                .read_reimportable(task_id)
                .await
                .map_err(|error| failed(error.to_string()))
        })
    }

    fn retry_schedule<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<requests::RetrySchedule>, requests::DispatchError>> {
        Box::pin(async move {
            let Some(policy) = self.policy.read() else {
                return Ok(None);
            };
            let policy = RetryPolicy::from_policy(&policy);
            if policy.auto_retry_max() == 0 {
                return Ok(None);
            }
            // The newest task for its target is the one the retry sweep
            // acts on; once a successor exists this task is no longer
            // waiting on a retry.
            let row = self
                .journal
                .read_retry_anchor(task_id)
                .await
                .map_err(|error| failed(error.to_string()))?;
            let Some(row) = row else {
                return Ok(None);
            };
            let retry_count = u32::try_from(row.retry_count).unwrap_or(0);
            let anchor = row.completed_at.unwrap_or(row.updated_at);
            Ok(policy
                .next_retry_at(retry_count, anchor, &row.status)
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
            self.journal
                .read_task_since(owner, kind, key, since as f64)
                .await
                .map_err(|error| failed(error.to_string()))
        })
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
                &[],
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
            let Some(row) = self.snapshot(task_id).await? else {
                return Ok(None);
            };
            let status = match row.status {
                // v2's download_task.status has no queued state; a queued
                // task is a live download-in-progress to its requester.
                TaskStatus::Queued | TaskStatus::Downloading => "downloading",
                TaskStatus::Processing => "processing",
                TaskStatus::Completed => "completed",
                TaskStatus::Partial => "partial",
                TaskStatus::Failed => "failed",
                TaskStatus::Cancelled => "cancelled",
            };
            Ok(Some(status.to_owned()))
        })
    }

    fn active_task_for_album<'a>(
        &'a self,
        rg_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<flows::DownloadTaskView>, String>> {
        Box::pin(async move {
            let row = self
                .journal
                .read_active_for_album(rg_mbid)
                .await
                .map_err(|error| error.to_string())?;
            Ok(row.map(|row| flows::DownloadTaskView {
                task_id: row.id,
                status: row.status.as_str().to_owned(),
                album_mbid: Some(row.release_group_mbid),
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
                &[],
                None,
                request.idempotency_key.as_deref(),
            )
            .await
            .map(flows::UpgradeDispatch::Enqueued)
        })
    }
}
