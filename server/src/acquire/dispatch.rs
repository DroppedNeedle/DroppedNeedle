//! Unified download dispatch: one production journal writer behind both
//! dispatch spellings.
//!
//! Requests (`requests::dispatch::DownloadDispatch`) and flows
//! (`flows::seams::DownloadDispatch`) each define the narrow surface they
//! need from downloads. Both stay (their tests pin them); production
//! unifies behind [`UnifiedDispatch`], which
//! implements both over the durable [`Journal`]. One struct, one task-id
//! mint, one insert path, one status vocabulary mapping.
//!
//! Task ids are 32 lowercase hex chars (a UUID without dashes): the
//! orphan reconciler's `job_name_parts` only recognises that shape, so
//! anything else would make debris invisible to the sweep.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::downloads::manifest::{DownloadManifest, ManifestCodec};
use super::downloads::store::{DownloadStore, NewTask, StoreError, TaskDetails, TaskRow};
use super::flows::seams as flows;
use super::requests::dispatch as requests;
use crate::ids::IdGenerator;

/// Current unix time as the float seconds the journal stores.
fn now_unix_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Sync journal over one rusqlite connection. The app also holds a sqlx
/// pool on the same file; the busy timeout absorbs lock contention and
/// every borrow is short.
pub struct Journal {
    conn: Mutex<Connection>,
}

impl Journal {
    /// Open the journal on a database file. Migrations already applied at
    /// boot; this only tunes the connection.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let conn =
            Connection::open(db_path).map_err(|error| format!("acquire journal: {error}"))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|error| format!("acquire journal: {error}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Scratch journal over `:memory:` with the real migration SQL applied.
    /// Foreign keys stay off, so dispatches for not-yet-created users
    /// still insert in unit-style tests. The pragma is explicit: the
    /// bundled SQLite enables enforcement by default.
    #[cfg(any(test, feature = "test-support"))]
    pub fn memory() -> Result<Self, String> {
        let conn =
            Connection::open_in_memory().map_err(|error| format!("acquire journal: {error}"))?;
        conn.execute_batch("PRAGMA foreign_keys = OFF")
            .map_err(|error| format!("acquire journal: {error}"))?;
        super::downloads::store::apply_test_schema(&conn)
            .map_err(|error| format!("acquire journal: {error}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Run one closure against the download store.
    pub fn with_store<R>(
        &self,
        op: impl FnOnce(&DownloadStore<'_>) -> Result<R, StoreError>,
    ) -> Result<R, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "acquire journal lock lost".to_owned())?;
        let store = DownloadStore::new(&conn);
        op(&store).map_err(|error| error.to_string())
    }

    /// Run one closure against the download store off the async runtime.
    /// Async callers must use this (never [`Self::with_store`]): rusqlite
    /// is synchronous, so running it inline would stall the executor.
    pub async fn with_store_async<R, F>(journal: &Arc<Journal>, op: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        let journal = Arc::clone(journal);
        tokio::task::spawn_blocking(move || journal.with_store(op))
            .await
            .map_err(|error| format!("acquire journal join failed: {error}"))?
    }
}

/// One production dispatch behind both trait spellings.
pub struct UnifiedDispatch {
    journal: Arc<Journal>,
    ids: Arc<dyn IdGenerator>,
    staging_root: PathBuf,
}

impl UnifiedDispatch {
    /// Wire dispatch over a shared journal. `staging_root` holds the
    /// per-task manifest skeletons written at dispatch time.
    pub fn new(journal: Arc<Journal>, ids: Arc<dyn IdGenerator>, staging_root: PathBuf) -> Self {
        Self {
            journal,
            ids,
            staging_root,
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
    /// row and the manifest for the library importer).
    #[allow(clippy::too_many_arguments)]
    fn insert(
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
        let claimed_key = if let Some(caller_key) = idempotency_key {
            let namespaced = format!("dispatch:{caller_key}");
            let claimed = self
                .journal
                .with_store(|store| store.claim_key(&namespaced, &task_id, "dispatch", now))?;
            if !claimed {
                // Repeat dispatch: answer the original task, never a twin.
                return self
                    .journal
                    .with_store(|store| store.task_id_for_key(&namespaced))?
                    .ok_or_else(|| "duplicate dispatch; original task unknown".to_owned());
            }
            Some(namespaced)
        } else {
            None
        };
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
        let inserted = self.journal.with_store(|store| {
            if is_track {
                store.insert_track_task(&task, &recording_mbid, now)?;
            } else {
                store.insert_task(&task, now)?;
            }
            store.set_task_details(
                &task.id,
                &TaskDetails {
                    release_mbid: release_mbid.map(str::to_owned),
                    track_title: is_track.then(|| title.to_owned()),
                    ..TaskDetails::default()
                },
                now,
            )
        });
        if let Err(error) = inserted {
            // The insert failed after the claim: release the key so a
            // repeat dispatches fresh instead of pointing at a task id
            // that was never written.
            if let Some(namespaced) = &claimed_key {
                let _ = self
                    .journal
                    .with_store(|store| store.release_key(namespaced));
            }
            return Err(error);
        }
        self.write_manifest_skeleton(
            &task_id,
            &release_group_mbid,
            artist,
            title,
            is_track,
            origin,
            release_mbid,
        );
        Ok(task_id)
    }

    /// Best-effort manifest skeleton. A staging failure must not fail the
    /// dispatch (the task still queues and the worker still polls); the
    /// missing manifest only steers startup recovery toward a clean
    /// restart, which is the safe direction.
    #[allow(clippy::too_many_arguments)]
    fn write_manifest_skeleton(
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
        let path = ManifestCodec::path(&self.staging_root, task_id);
        let bytes = match ManifestCodec.encode(&manifest) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(task_id, %error, "dispatch manifest encode failed");
                return;
            }
        };
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(task_id, %error, "dispatch staging dir failed");
            return;
        }
        if let Err(error) = std::fs::write(&path, bytes) {
            tracing::warn!(task_id, %error, "dispatch manifest write failed");
        }
    }

    /// Read one task row, if it exists.
    fn get_task(&self, task_id: &str) -> Option<TaskRow> {
        self.journal
            .with_store(|store| store.get_task(task_id))
            .unwrap_or(None)
    }
}

impl requests::DownloadDispatch for UnifiedDispatch {
    fn dispatch(
        &self,
        request: &requests::DispatchRequest,
    ) -> Result<requests::DispatchOutcome, requests::DispatchError> {
        // Editions dispatch as album fetches; the pinned release MBID rides
        // in the row and the manifest for the library importer.
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
        .map(|task_id| requests::DispatchOutcome::Dispatched { task_id })
        .map_err(requests::DispatchError::Failed)
    }

    fn cancel_task(&self, task_id: &str) {
        let active = self
            .get_task(task_id)
            .is_some_and(|row| !row.status.is_terminal());
        if !active {
            return;
        }
        let _ = self.journal.with_store(|store| {
            store.transition_task(
                task_id,
                super::downloads::state::TaskStatus::Cancelled,
                now_unix_f64(),
                None,
            )
        });
    }

    fn task_state(&self, task_id: &str) -> requests::DispatchTaskState {
        use super::downloads::state::TaskStatus as Db;
        match self.get_task(task_id).map(|row| row.status) {
            None => requests::DispatchTaskState::Missing,
            Some(Db::Queued | Db::Downloading | Db::Processing) => {
                requests::DispatchTaskState::Active
            }
            Some(Db::Completed) => requests::DispatchTaskState::Imported,
            // The requests seam has no partial state; a short landing reads
            // as failed here while the flows sync maps it precisely.
            Some(Db::Partial | Db::Failed) => requests::DispatchTaskState::Failed,
            Some(Db::Cancelled) => requests::DispatchTaskState::Cancelled,
        }
    }

    fn task_progress(&self, task_id: &str) -> Option<requests::TaskProgress> {
        self.get_task(task_id).map(|row| requests::TaskProgress {
            status: row.status.as_str().to_owned(),
            progress_percent: row.progress_percent,
            total_size_bytes: row.total_size_bytes,
            downloaded_bytes: row.downloaded_bytes,
            error_message: row.error_message,
            quality: row.quality_format,
            protocol: row.source,
        })
    }

    fn reimportable(&self, task_id: &str) -> bool {
        self.journal
            .with_store(|store| store.is_reimportable(task_id))
            .unwrap_or(false)
    }
}

impl flows::DownloadDispatch for UnifiedDispatch {
    fn dispatch(&self, request: &flows::DispatchRequest) -> Result<String, String> {
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
    }

    fn task_status(&self, task_id: &str) -> Option<String> {
        use super::downloads::state::TaskStatus as Db;
        self.get_task(task_id).map(|row| {
            match row.status {
                // v2's download_task.status has no queued state; a queued
                // task is a live download-in-progress to its requester.
                Db::Queued | Db::Downloading => "downloading",
                Db::Processing => "processing",
                Db::Completed => "completed",
                Db::Partial => "partial",
                Db::Failed => "failed",
                Db::Cancelled => "cancelled",
            }
            .to_owned()
        })
    }

    fn active_task_for_album(&self, rg_mbid: &str) -> Option<flows::DownloadTaskView> {
        self.journal
            .with_store(|store| store.newest_active_for_album(rg_mbid))
            .unwrap_or(None)
            .map(|row| flows::DownloadTaskView {
                task_id: row.id,
                status: row.status.as_str().to_owned(),
                album_mbid: Some(row.release_group_mbid),
            })
    }

    fn dispatch_upgrade(
        &self,
        request: &flows::DispatchRequest,
    ) -> Result<flows::UpgradeDispatch, String> {
        // Active-task dedup: an album already fetching gets nothing new.
        // The seam spells "nothing queued" as AlreadyInLibrary (the sweep
        // only needs to not count it); a true library-cutoff check waits
        // on a library catalog port.
        if self.active_task_for_album(&request.mbid).is_some() {
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
        .map(flows::UpgradeDispatch::Enqueued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UuidGenerator;

    fn dispatch() -> UnifiedDispatch {
        let staging = std::env::temp_dir().join(format!(
            "dn-dispatch-test-{}-{}",
            std::process::id(),
            now_unix_f64().to_bits()
        ));
        UnifiedDispatch::new(
            Arc::new(Journal::memory().expect("memory journal")),
            Arc::new(UuidGenerator),
            staging,
        )
    }

    fn album_request(key: Option<&str>) -> requests::DispatchRequest {
        requests::DispatchRequest {
            user_id: "u1".to_owned(),
            kind: "album".to_owned(),
            key: "rg-1".to_owned(),
            artist_name: "artist".to_owned(),
            title: "album".to_owned(),
            origin: requests::DispatchOrigin::User,
            release_mbid: None,
            idempotency_key: key.map(str::to_owned),
        }
    }

    fn task_of(outcome: requests::DispatchOutcome) -> String {
        match outcome {
            requests::DispatchOutcome::Dispatched { task_id } => task_id,
            requests::DispatchOutcome::AlreadyInLibrary => panic!("unexpected dedup"),
        }
    }

    #[test]
    fn repeat_key_answers_the_original_task() {
        use requests::DownloadDispatch as _;
        let dispatch = dispatch();
        let first = task_of(dispatch.dispatch(&album_request(Some("k1"))).unwrap());
        let second = task_of(dispatch.dispatch(&album_request(Some("k1"))).unwrap());
        assert_eq!(first, second);
        let other = task_of(dispatch.dispatch(&album_request(Some("k2"))).unwrap());
        assert_ne!(first, other);
    }

    #[test]
    fn missing_key_mints_fresh() {
        use requests::DownloadDispatch as _;
        let dispatch = dispatch();
        let first = task_of(dispatch.dispatch(&album_request(None)).unwrap());
        let second = task_of(dispatch.dispatch(&album_request(None)).unwrap());
        assert_ne!(first, second);
    }

    #[test]
    fn flows_seam_honors_keys() {
        use flows::DownloadDispatch as _;
        let dispatch = dispatch();
        let request = flows::DispatchRequest {
            user_id: "u1".to_owned(),
            kind: flows::DispatchKind::Album,
            mbid: "rg-1".to_owned(),
            artist: "artist".to_owned(),
            title: "album".to_owned(),
            origin: "wanted".to_owned(),
            idempotency_key: Some("w1".to_owned()),
        };
        let first = dispatch.dispatch(&request).unwrap();
        let second = dispatch.dispatch(&request).unwrap();
        assert_eq!(first, second);
    }
}
