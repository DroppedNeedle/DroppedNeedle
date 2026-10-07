//! The held-import review: files the landing would not import on its own
//! wait here for a person (v2 "import anyway").
//!
//! A person can listen to a held file, import it as it is, discard it, or
//! (for an AcoustID hold) ask for a fresh fingerprint check. An import goes
//! through the same library import seam as a landing, into the edition the
//! library chose for the album, so a held file lands exactly where an
//! automatic import would have put it. For an upgrade, the library's copy
//! is replaced only by a strictly better file and goes to the recycle bin
//! first, never deleted.
//!
//! Holds the library side caused (a taken destination, an old copy that
//! could not be recycled) can also be retried or discarded for a whole
//! download at once. Every refusal is one plain sentence saying what to do.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::held_rows::{self, HeldRow};
use super::queue_rows::Viewer;
use super::state::TaskStatus;
use crate::acquire::landing::ports::{ImportFailure, ImportFile, ImportRequest, LandingLibrary};
use crate::acquire::landing::reasons::explain;
use crate::acquire::landing::{is_local_hold, probe, quality, specs};
use crate::acquire::worker::DownloadWorker;
use crate::library::matching::Release;

/// Fingerprint checks one bulk re-check runs at most (v2
/// `HELD_REVERIFY_BULK_LIMIT`). Skipped rows do not count.
pub const BULK_REVERIFY_LIMIT: usize = 25;

/// Why a held-file action did not happen. The sentences are shown as is.
#[derive(Debug, thiserror::Error)]
pub enum HeldError {
    /// No such held file (or download) the viewer may see.
    #[error("{0}")]
    NotFound(&'static str),
    /// The action cannot run now: the file, the album or the server is
    /// not in a state for it. The sentence says what to do.
    #[error("{0}")]
    Refused(String),
    /// The journal could not be read or written.
    #[error("held imports unavailable: {0}")]
    Unavailable(String),
}

fn unavailable(error: impl std::fmt::Display) -> HeldError {
    HeldError::Unavailable(error.to_string())
}

const HELD_NOT_FOUND: &str = "That held file was not found. It may have been handled already.";

/// What importing a held file did.
#[derive(Debug, Clone, PartialEq)]
pub enum Placed {
    /// The file is in the library at this path.
    Imported(PathBuf),
    /// An upgrade whose file was no better than the library's copy: the
    /// copy stays and the held file was removed.
    KeptExisting,
}

/// A fingerprint re-check's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Reverified {
    /// AcoustID now agrees; the file was imported.
    Imported(Placed),
    /// Still no confident agreement; the file stays held.
    StillHeld,
}

/// One row of a bulk re-check.
#[derive(Debug, Clone)]
pub struct BulkItem {
    pub held_id: i64,
    pub release_group_mbid: Option<String>,
    pub outcome: Result<Reverified, BulkSkip>,
}

/// Why a bulk re-check left a row alone.
#[derive(Debug, Clone)]
pub enum BulkSkip {
    /// Not a fingerprint hold, so there is nothing to re-check.
    NotFingerprint,
    /// The check or the import failed; the sentence says why.
    Failed(String),
}

/// The held-import review over the download worker.
#[derive(Clone)]
pub struct HeldImports {
    worker: Arc<DownloadWorker>,
    /// Held ids an action runs on right now; a second action on the same
    /// file is refused instead of importing it twice.
    busy: Arc<Mutex<HashSet<i64>>>,
}

/// One held id's place in the busy set, given back when dropped.
struct Claim {
    busy: Arc<Mutex<HashSet<i64>>>,
    id: i64,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Ok(mut busy) = self.busy.lock() {
            busy.remove(&self.id);
        }
    }
}

fn now_unix_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

impl HeldImports {
    pub fn new(worker: Arc<DownloadWorker>) -> Self {
        Self {
            worker,
            busy: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn pool(&self) -> &sqlx::SqlitePool {
        self.worker.journal().db().pool()
    }

    /// Held files the viewer may see, newest first.
    pub async fn list(
        &self,
        viewer: &Viewer,
        release_group_mbid: Option<&str>,
    ) -> Result<Vec<HeldRow>, HeldError> {
        held_rows::list_held(self.pool(), viewer, release_group_mbid, None)
            .await
            .map_err(unavailable)
    }

    /// One held file the viewer may see. Someone else's file answers
    /// not found, as in v2.
    pub async fn get(&self, viewer: &Viewer, id: i64) -> Result<HeldRow, HeldError> {
        let row = held_rows::get_held(self.pool(), id)
            .await
            .map_err(unavailable)?
            .filter(|row| viewer.may_touch(&row.user_id))
            .ok_or(HeldError::NotFound(HELD_NOT_FOUND))?;
        Ok(row)
    }

    fn claim(&self, id: i64) -> Result<Claim, HeldError> {
        let mut busy = self
            .busy
            .lock()
            .map_err(|_| HeldError::Unavailable("held action lock poisoned".to_owned()))?;
        if !busy.insert(id) {
            return Err(HeldError::Refused(
                "This held file is already being handled. Wait a moment and refresh.".to_owned(),
            ));
        }
        Ok(Claim {
            busy: self.busy.clone(),
            id,
        })
    }

    fn library(&self) -> Result<Arc<dyn LandingLibrary>, HeldError> {
        self.worker
            .landing()
            .and_then(|landing| landing.library_slot().get().cloned())
            .ok_or_else(|| {
                HeldError::Refused(
                    "Imports are not set up yet. Add a library folder in settings, then try again."
                        .to_owned(),
                )
            })
    }

    /// Import one held file as it is, skipping the check that held it.
    pub async fn import(&self, viewer: &Viewer, id: i64, actor: &str) -> Result<Placed, HeldError> {
        let _claim = self.claim(id)?;
        let row = self.get(viewer, id).await?;
        self.import_row(&row, actor).await
    }

    /// Delete one held file and let the album's automatic retry resume.
    pub async fn discard(&self, viewer: &Viewer, id: i64) -> Result<(), HeldError> {
        let _claim = self.claim(id)?;
        let row = self.get(viewer, id).await?;
        self.discard_rows(vec![row]).await?;
        self.worker.wake();
        Ok(())
    }

    /// Fingerprint one AcoustID-held file again; import it when AcoustID
    /// now agrees.
    pub async fn reverify(
        &self,
        viewer: &Viewer,
        id: i64,
        actor: &str,
    ) -> Result<Reverified, HeldError> {
        let _claim = self.claim(id)?;
        let row = self.get(viewer, id).await?;
        if row.reason != "fingerprint_mismatch" {
            return Err(HeldError::Refused(
                "Only files held because AcoustID heard a different recording can be re-checked."
                    .to_owned(),
            ));
        }
        self.reverify_row(&row, actor).await
    }

    /// Re-check the viewer's AcoustID holds in one go: the listed ids in
    /// order (duplicates once), or every held file newest first. At most
    /// [`BULK_REVERIFY_LIMIT`] fingerprint checks run; one file's failure
    /// never stops the rest.
    pub async fn reverify_bulk(
        &self,
        viewer: &Viewer,
        ids: Option<Vec<i64>>,
        actor: &str,
    ) -> Result<Vec<BulkItem>, HeldError> {
        let rows = held_rows::list_held(self.pool(), viewer, None, None)
            .await
            .map_err(unavailable)?;
        let candidates: Vec<HeldRow> = match ids {
            None => rows,
            Some(ids) => {
                let mut seen = HashSet::new();
                ids.into_iter()
                    .filter(|id| seen.insert(*id))
                    .filter_map(|id| rows.iter().find(|row| row.id == id).cloned())
                    .collect()
            }
        };
        let mut results = Vec::new();
        let mut checked = 0;
        for row in candidates {
            if row.reason != "fingerprint_mismatch" {
                results.push(BulkItem {
                    held_id: row.id,
                    release_group_mbid: row.release_group_mbid,
                    outcome: Err(BulkSkip::NotFingerprint),
                });
                continue;
            }
            if checked >= BULK_REVERIFY_LIMIT {
                break;
            }
            checked += 1;
            let outcome = match self.claim(row.id) {
                Ok(_claim) => self.reverify_row(&row, actor).await,
                Err(error) => Err(error),
            };
            results.push(BulkItem {
                held_id: row.id,
                release_group_mbid: row.release_group_mbid.clone(),
                outcome: outcome.map_err(|error| {
                    if let HeldError::Unavailable(cause) = &error {
                        tracing::warn!(held_id = row.id, %cause, "held re-check failed");
                    }
                    BulkSkip::Failed(match error {
                        HeldError::Unavailable(_) => "The re-check failed.".to_owned(),
                        other => other.to_string(),
                    })
                }),
            });
        }
        Ok(results)
    }

    /// Import every file of a download that waits on the library side (a
    /// taken destination, an old copy that could not be recycled), as one
    /// unit (v2 management retry). Admin only; answers files imported.
    pub async fn retry_local(&self, viewer: &Viewer, task_id: &str) -> Result<usize, HeldError> {
        let rows = self.task_rows(viewer, task_id, true).await?;
        let mut claims = Vec::with_capacity(rows.len());
        for row in &rows {
            claims.push(self.claim(row.id)?);
        }
        let mut imported = 0;
        for row in &rows {
            self.import_row(row, &viewer.user_id).await?;
            imported += 1;
        }
        Ok(imported)
    }

    /// Delete every file of a download that waits on the library side.
    /// Admin only; answers files discarded.
    pub async fn discard_local(&self, viewer: &Viewer, task_id: &str) -> Result<usize, HeldError> {
        let rows = self.task_rows(viewer, task_id, true).await?;
        let count = rows.len();
        self.discard_rows(rows).await?;
        self.worker.wake();
        Ok(count)
    }

    /// Delete every file a download's checks held and clear its
    /// wrong-product verdict (v2 verdict discard). Answers files discarded.
    pub async fn discard_verdict(
        &self,
        viewer: &Viewer,
        task_id: &str,
    ) -> Result<usize, HeldError> {
        let rows = self.task_rows(viewer, task_id, false).await?;
        let count = rows.len();
        self.discard_rows(rows).await?;
        let task = task_id.to_owned();
        self.worker
            .journal()
            .run_foreground("downloads.held_verdict_clear", move |store| {
                store.clear_wrong_product_verdict(&task)
            })
            .await
            .map_err(HeldError::Unavailable)?;
        self.worker.wake();
        Ok(count)
    }

    /// A download's held files of one kind: held by the library side
    /// (`local`) or by the checks.
    async fn task_rows(
        &self,
        viewer: &Viewer,
        task_id: &str,
        local: bool,
    ) -> Result<Vec<HeldRow>, HeldError> {
        let rows: Vec<HeldRow> = held_rows::list_held(self.pool(), viewer, None, Some(task_id))
            .await
            .map_err(unavailable)?
            .into_iter()
            .filter(|row| is_local_hold(&row.reason) == local)
            .collect();
        if rows.is_empty() {
            return Err(HeldError::NotFound(if local {
                "No held files of this download wait on the library."
            } else {
                "No held files were found for this download."
            }));
        }
        Ok(rows)
    }

    async fn reverify_row(&self, row: &HeldRow, actor: &str) -> Result<Reverified, HeldError> {
        let library = self.library()?;
        let path = PathBuf::from(&row.held_path);
        self.ensure_on_disk(row, &path).await?;
        let heard = library.fingerprints(vec![(String::new(), path)]).await;
        let heard = heard.get("").map(Vec::as_slice).unwrap_or(&[]);
        // No AcoustID answer is no confirmation: the file stays held.
        if heard.is_empty()
            || specs::fingerprint_disagrees(
                heard,
                row.recording_mbid.as_deref().unwrap_or(""),
                row.duration_seconds,
                row.expected_duration_seconds,
            )
        {
            return Ok(Reverified::StillHeld);
        }
        self.import_row(row, actor).await.map(Reverified::Imported)
    }

    /// A held row whose file vanished is resolved as discarded, so it
    /// stops showing.
    async fn ensure_on_disk(&self, row: &HeldRow, path: &Path) -> Result<(), HeldError> {
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Ok(());
        }
        let id = row.id;
        let now = now_unix_f64();
        self.worker
            .journal()
            .run_foreground("downloads.held_vanished", move |store| {
                store.resolve_held(&[id], "discarded", now)
            })
            .await
            .map_err(HeldError::Unavailable)?;
        Err(HeldError::Refused(
            "The held file is no longer on disk. Download the album again.".to_owned(),
        ))
    }

    /// Place one held file through the library's import seam.
    async fn import_row(&self, row: &HeldRow, actor: &str) -> Result<Placed, HeldError> {
        let library = self.library()?;
        let source = PathBuf::from(&row.held_path);
        self.ensure_on_disk(row, &source).await?;
        let (release, track) = self.release_track(&library, row).await?;
        let release_track = &release.tracks[track];

        // An upgrade replaces the library's copy only with a better file;
        // the old copy goes to the recycle bin before the new one lands.
        let mut recycled = None;
        if row.origin == "upgrade" {
            let copies = library
                .owned_copies(
                    &release.release_group_id,
                    &release_track.id,
                    &release_track.recording_id,
                )
                .await;
            if !copies.is_empty() {
                let tier = held_tier(&source).await;
                if !copies.iter().all(|copy| quality::beats(tier, copy.tier)) {
                    self.finish(row, "imported").await?;
                    remove_held_copy(&source).await;
                    self.settle_task(&library, row, &release).await;
                    return Ok(Placed::KeptExisting);
                }
                let moved = library
                    .recycle(
                        copies.into_iter().map(|copy| copy.track_id).collect(),
                        actor.to_owned(),
                    )
                    .await
                    .map_err(|detail| {
                        tracing::warn!(held_id = row.id, %detail, "upgrade recycle failed");
                        let reason = explain("upgrade_blocked");
                        HeldError::Refused(format!("{} {}", reason.message, reason.action))
                    })?;
                recycled = Some(moved);
            }
        }

        let request = ImportRequest {
            task_id: row
                .source_task_id
                .clone()
                .unwrap_or_else(|| format!("held-{}", row.id)),
            staging: Some(format!("held-{}", row.id)),
            release: release.clone(),
            files: vec![ImportFile {
                path: source.clone(),
                track,
            }],
        };
        let outcome = library.import(request).await;
        let failed = match &outcome {
            Ok(receipt) if receipt.skipped.is_empty() => None,
            Ok(receipt) => Some(
                receipt
                    .skipped
                    .first()
                    .map(|(_, detail)| ImportFailure::Occupied(detail.clone()))
                    .unwrap_or_else(|| ImportFailure::Occupied(String::new())),
            ),
            Err(failure) => Some(failure.clone()),
        };
        if let Some(failure) = failed {
            if let Some(moved) = recycled
                && !library.put_back(moved, actor.to_owned()).await
            {
                tracing::error!(held_id = row.id, "recycled copies not all put back");
            }
            tracing::warn!(
                held_id = row.id,
                detail = failure.detail(),
                "held import refused"
            );
            let reason = match failure {
                ImportFailure::Occupied(_) => explain("target_occupied"),
                ImportFailure::LocalFault(_) => explain("local_fault"),
            };
            return Err(HeldError::Refused(format!(
                "{} {}",
                reason.message, reason.action
            )));
        }
        let placed = outcome
            .ok()
            .and_then(|receipt| receipt.paths.into_iter().next())
            .unwrap_or_default();
        self.finish(row, "imported").await?;
        remove_held_copy(&source).await;
        self.settle_task(&library, row, &release).await;
        tracing::info!(held_id = row.id, path = %placed.display(), "held file imported");
        Ok(Placed::Imported(placed))
    }

    /// The release the file goes into (the album's chosen edition, else
    /// the one it was matched to) and its track there.
    async fn release_track(
        &self,
        library: &Arc<dyn LandingLibrary>,
        row: &HeldRow,
    ) -> Result<(Release, usize), HeldError> {
        let unmatched = || {
            HeldError::Refused(
                "This file was never matched to an album, so it cannot be imported as it is. \
                 Discard it and download the album again."
                    .to_owned(),
            )
        };
        let group = row
            .release_group_mbid
            .as_deref()
            .filter(|mbid| !mbid.is_empty())
            .ok_or_else(unmatched)?;
        let chosen = library.chosen_edition(group).await;
        let release_mbid = chosen
            .or_else(|| row.release_mbid.clone())
            .filter(|mbid| !mbid.is_empty())
            .ok_or_else(unmatched)?;
        let release = match library.release(&release_mbid).await {
            Ok(Some(release)) => release,
            Ok(None) => {
                return Err(HeldError::Refused(
                    "The album's edition is no longer on MusicBrainz. Choose another edition \
                     on the album page, then import again."
                        .to_owned(),
                ));
            }
            Err(error) => {
                tracing::warn!(held_id = row.id, ?error, "release lookup failed");
                return Err(HeldError::Refused(
                    "MusicBrainz could not be reached to read the album's tracklist. \
                     Try again in a minute."
                        .to_owned(),
                ));
            }
        };
        let same = |left: &str, right: Option<&str>| {
            right.is_some_and(|right| !right.is_empty() && left.eq_ignore_ascii_case(right))
        };
        let track = release
            .tracks
            .iter()
            .position(|track| same(&track.id, row.release_track_mbid.as_deref()))
            .or_else(|| {
                release
                    .tracks
                    .iter()
                    .position(|track| same(&track.recording_id, row.recording_mbid.as_deref()))
            })
            .ok_or_else(|| {
                HeldError::Refused(
                    "This track is not on the edition chosen for the album. Choose the edition \
                     it belongs to on the album page, or discard the file."
                        .to_owned(),
                )
            })?;
        Ok((release, track))
    }

    async fn finish(&self, row: &HeldRow, status: &'static str) -> Result<(), HeldError> {
        let id = row.id;
        let now = now_unix_f64();
        self.worker
            .journal()
            .run_foreground("downloads.held_resolve", move |store| {
                store.resolve_held(&[id], status, now)
            })
            .await
            .map_err(HeldError::Unavailable)?;
        Ok(())
    }

    async fn discard_rows(&self, rows: Vec<HeldRow>) -> Result<(), HeldError> {
        let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        let now = now_unix_f64();
        self.worker
            .journal()
            .run_foreground("downloads.held_discard", move |store| {
                store.resolve_held(&ids, "discarded", now)
            })
            .await
            .map_err(HeldError::Unavailable)?;
        for row in rows {
            remove_held_copy(Path::new(&row.held_path)).await;
        }
        Ok(())
    }

    /// The import may have made the album whole: then a failed or partial
    /// source task completes, so it stops waiting on a retry it no longer
    /// needs. Best effort: the import already stuck.
    async fn settle_task(
        &self,
        library: &Arc<dyn LandingLibrary>,
        row: &HeldRow,
        release: &Release,
    ) {
        let Some(task_id) = row.source_task_id.clone() else {
            return;
        };
        let task = match self.worker.journal().read_task(&task_id).await {
            Ok(Some(task)) => task,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%task_id, %error, "task unreadable after a held import");
                return;
            }
        };
        if !matches!(task.status, TaskStatus::Failed | TaskStatus::Partial) {
            return;
        }
        let viewer = Viewer {
            user_id: task.user_id.clone(),
            admin: true,
        };
        match held_rows::list_held(self.pool(), &viewer, None, Some(&task_id)).await {
            Ok(rows) if rows.is_empty() => {}
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(%task_id, %error, "held rows unreadable after a held import");
                return;
            }
        }
        if task.download_type != "track" {
            let owned = library.owned(&release.release_group_id).await;
            let whole = release.tracks.iter().all(|track| {
                owned
                    .release_tracks
                    .contains(&track.id.to_ascii_lowercase())
                    || owned
                        .recordings
                        .contains(&track.recording_id.to_ascii_lowercase())
            });
            if !whole {
                return;
            }
        }
        let now = now_unix_f64();
        let id = task_id.clone();
        match self
            .worker
            .journal()
            .run("downloads.held_settle", move |store| {
                store.complete_after_manual_import(&id, now)
            })
            .await
        {
            Ok(true) => self.worker.announce_completed(&task).await,
            Ok(false) => {}
            Err(error) => tracing::warn!(%task_id, %error, "task not settled after a held import"),
        }
    }
}

/// The quality tier of a held file, read from its header.
async fn held_tier(path: &Path) -> &'static str {
    let paths = vec![path.to_path_buf()];
    match tokio::task::spawn_blocking(move || probe::probe(&paths, &[])).await {
        Ok(landing) => landing.audio.first().map_or("low", |file| file.tier()),
        Err(error) => {
            tracing::warn!(%error, "held file probe failed; treated as lowest quality");
            "low"
        }
    }
}

/// Remove a held copy. A copy already gone is fine; any other failure
/// is logged and leaves a stray file in the held folder, nothing worse.
async fn remove_held_copy(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "held copy not removed");
    }
}
