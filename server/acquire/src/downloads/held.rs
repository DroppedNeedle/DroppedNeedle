//! The held-import review: files the landing would not import on its own
//! wait here for a person (v2 "import anyway").
//!
//! A person can listen to a held file, import it, discard it, or (for an
//! AcoustID hold) ask for a fresh fingerprint check. An import goes through
//! the one intake as a person's choice, with the plan of the download the
//! file came from: a file fetched for a copy joins it, a version replaces
//! yours, otherwise it goes into the edition the library chose for the
//! album. The check that held the file is skipped; the comparison with
//! your copy still runs, and a file no better than it is kept, never
//! deleted, until a second import says to use it anyway. A replaced copy
//! goes to the recycle bin once the new file is in.
//!
//! Holds the library side caused (a taken destination, an old copy that
//! could not be recycled) can also be retried or discarded for a whole
//! download at once. Every refusal is one plain sentence saying what to do.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::held_rows::HeldRow;
use super::journal::Journal;
use super::plan::PlannedAlbum;
use super::queue_rows::Viewer;
use super::state::TaskStatus;
use crate::intake::{self, Gate, IntakeBatch, IntakeFile, Origin, Outcome, Pairing};
use crate::landing::ports::LandingLibrary;
use crate::landing::probe::{self, LandedFile};
use crate::landing::specs;
use crate::reason::{Reason, ReasonKind};
use crate::worker::DownloadWorker;
use droppedneedle_library::matching::Release;
use droppedneedle_providers::RequestPriority;

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
    journal: Arc<Journal>,
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

impl HeldImports {
    /// The review over `journal`; `worker` lands the files and wakes.
    pub fn new(journal: Arc<Journal>, worker: Arc<DownloadWorker>) -> Self {
        Self {
            journal,
            worker,
            busy: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Held files the viewer may see, newest first.
    pub async fn list(
        &self,
        viewer: &Viewer,
        release_group_mbid: Option<&str>,
    ) -> Result<Vec<HeldRow>, HeldError> {
        self.journal
            .read_held(viewer, release_group_mbid, None)
            .await
            .map_err(unavailable)
    }

    /// One held file the viewer may see. Someone else's file answers
    /// not found, as in v2.
    pub async fn get(&self, viewer: &Viewer, id: i64) -> Result<HeldRow, HeldError> {
        let row = self
            .journal
            .read_held_row(id)
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
            .and_then(|landing| landing.library().cloned())
            .ok_or_else(|| {
                HeldError::Refused(
                    "Imports are not set up yet. Add a library folder in settings, then try again."
                        .to_owned(),
                )
            })
    }

    /// Import one held file, skipping the check that held it. A file no
    /// better than your copy is kept and the answer says so; importing it
    /// again then replaces yours.
    pub async fn import(&self, viewer: &Viewer, id: i64, actor: &str) -> Result<Placed, HeldError> {
        let _claim = self.claim(id)?;
        let row = self.get(viewer, id).await?;
        self.import_one(row, actor).await
    }

    async fn import_one(&self, row: HeldRow, actor: &str) -> Result<Placed, HeldError> {
        let placed = self.place_rows(vec![row], actor).await?;
        match placed.held.first() {
            Some(reason) => Err(refused(*reason)),
            None => Ok(Placed::Imported(
                placed.paths.into_iter().next().unwrap_or_default(),
            )),
        }
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
        if row.reason != Reason::FingerprintMismatch {
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
        let rows = self
            .journal
            .read_held(viewer, None, None)
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
            if row.reason != Reason::FingerprintMismatch {
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
        self.place_rows(rows, &viewer.user_id).await?.imported()
    }

    /// Import several held files in one go: one batch per download they
    /// came from, each with that download's plan. Answers files imported;
    /// when none went in, why the first stays held.
    pub async fn import_files(
        &self,
        viewer: &Viewer,
        ids: &[i64],
        actor: &str,
    ) -> Result<usize, HeldError> {
        let mut claims = Vec::with_capacity(ids.len());
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            claims.push(self.claim(*id)?);
            rows.push(self.get(viewer, *id).await?);
        }
        self.place_rows(rows, actor).await?.imported()
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
        self.journal
            .run_foreground("downloads.held_verdict_clear", move |store| {
                store.clear_wrong_product_verdict(&task)
            })
            .await
            .map_err(unavailable)?;
        self.worker.wake();
        Ok(count)
    }

    /// A download's held files of one kind: held by the library side
    /// (`local`, the Library kind) or by the checks.
    async fn task_rows(
        &self,
        viewer: &Viewer,
        task_id: &str,
        local: bool,
    ) -> Result<Vec<HeldRow>, HeldError> {
        let rows: Vec<HeldRow> = self
            .journal
            .read_held(viewer, None, Some(task_id))
            .await
            .map_err(unavailable)?
            .into_iter()
            .filter(|row| (row.reason.kind() == ReasonKind::Library) == local)
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
        self.import_one(row.clone(), actor)
            .await
            .map(Reverified::Imported)
    }

    /// A held row whose file vanished is resolved as discarded, so it
    /// stops showing.
    async fn ensure_on_disk(&self, row: &HeldRow, path: &Path) -> Result<(), HeldError> {
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Ok(());
        }
        let id = row.id;
        let now = self.journal.db().now_unix_f64();
        self.journal
            .run_foreground("downloads.held_vanished", move |store| {
                store.resolve_held(&[id], "discarded", now)
            })
            .await
            .map_err(unavailable)?;
        Err(HeldError::Refused(
            "The held file is no longer on disk. Download the album again.".to_owned(),
        ))
    }

    /// Place held files as a person's choice, through the one intake: one
    /// batch per download they came from, each with that download's plan,
    /// so a file fetched for a copy joins it and a version replaces yours.
    /// A file the intake holds again keeps its copy and takes its new
    /// reason; a whole batch the library could not take answers why.
    async fn place_rows(&self, rows: Vec<HeldRow>, actor: &str) -> Result<RowsPlaced, HeldError> {
        let mut groups: Vec<Vec<HeldRow>> = Vec::new();
        for row in rows {
            match groups
                .iter_mut()
                .find(|group| group[0].source_task_id == row.source_task_id)
            {
                Some(group) => group.push(row),
                None => groups.push(vec![row]),
            }
        }
        let mut placed = RowsPlaced::default();
        for group in groups {
            self.place_group(group, actor, &mut placed).await?;
        }
        Ok(placed)
    }

    async fn place_group(
        &self,
        rows: Vec<HeldRow>,
        actor: &str,
        placed: &mut RowsPlaced,
    ) -> Result<(), HeldError> {
        let library = self.library()?;
        for row in &rows {
            self.ensure_on_disk(row, Path::new(&row.held_path)).await?;
        }
        let first = &rows[0];
        let target = self.task_target(first).await?;
        let (release, join) = self.release_for(&library, first, target.as_ref()).await?;
        let mut files = Vec::with_capacity(rows.len());
        let mut pairing = Pairing::default();
        for (index, row) in rows.iter().enumerate() {
            // A file whose quality cannot be read is never judged against
            // your copy: that could replace it with something unplayable.
            let Some(file) = probe_held(Path::new(&row.held_path)).await else {
                return Err(HeldError::Refused(
                    "The held file could not be read to compare its quality with your copy, \
                     so nothing was changed. Play it to check it, or discard it."
                        .to_owned(),
                ));
            };
            let track = track_on(&release, row).ok_or_else(|| {
                HeldError::Refused(
                    "This track is not on the edition chosen for the album. Choose the edition \
                     it belongs to on the album page, or discard the file."
                        .to_owned(),
                )
            })?;
            files.push(IntakeFile {
                file,
                held_for: Some(row.reason),
            });
            // Every file is compared with the library's copies of its
            // track; with none there, it is placed as it is.
            pairing.owned.push((index, track));
        }
        let batch = IntakeBatch {
            files,
            target: intake::Target {
                release: release.clone(),
                join,
                asked: target.map(|target| target.album_wanted).unwrap_or_default(),
            },
            origin: Origin::Download {
                task_id: first
                    .source_task_id
                    .clone()
                    .unwrap_or_else(|| format!("held-{}", first.id)),
                upgrade: first.origin == "upgrade",
                staging: Some(format!("held-{}", first.id)),
            },
            gate: Gate::PersonChoseFile,
            owner: actor.to_owned(),
        };
        let placement = match intake::place(library.as_ref(), &batch, pairing, None).await {
            Ok(placement) => placement,
            Err(deferred) => {
                tracing::warn!(
                    held_id = first.id,
                    detail = %deferred.detail,
                    code = deferred.reason.code(),
                    "held import refused"
                );
                if deferred.reason != Reason::LibraryChanged {
                    return Err(refused(deferred.reason));
                }
                // The library changed under the import twice: the files
                // stay held with the publisher's words, to try again.
                for row in &rows {
                    self.rehold(row, Reason::LibraryChanged, &deferred.detail)
                        .await?;
                    placed.held.push(Reason::LibraryChanged);
                }
                return Ok(());
            }
        };
        let mut imported = false;
        for (row, outcome) in rows.iter().zip(&placement.outcomes) {
            match outcome {
                Outcome::Imported { .. } => {
                    self.finish(row, "imported").await?;
                    remove_held_copy(Path::new(&row.held_path)).await;
                    imported = true;
                    placed.imported += 1;
                }
                Outcome::Held { reason, detail, .. } => {
                    tracing::info!(
                        held_id = row.id,
                        code = reason.code(),
                        detail,
                        "held file stays held"
                    );
                    self.rehold(row, *reason, detail).await?;
                    placed.held.push(*reason);
                }
                // A person's import is never dropped as owned; the file
                // stays as it was.
                Outcome::AlreadyOwned { .. } | Outcome::Untouched => placed.held.push(row.reason),
            }
        }
        placed.paths.extend(placement.paths);
        if imported {
            tracing::info!(
                held_id = first.id,
                files = placed.imported,
                "held files imported"
            );
            self.settle_task(&library, first, &release).await;
        }
        Ok(())
    }

    /// What the download a held file came from was for: its row, details
    /// and plan. `None` when the file names no download, or the download
    /// is gone. A plan that does not read is an error, never an empty
    /// plan: that would lose the copy the file joins.
    async fn task_target(&self, row: &HeldRow) -> Result<Option<specs::Target>, HeldError> {
        let Some(task_id) = row.source_task_id.clone() else {
            return Ok(None);
        };
        let Some(task) = self
            .journal
            .read_task(&task_id)
            .await
            .map_err(unavailable)?
        else {
            return Ok(None);
        };
        let (details, plan) = self
            .journal
            .read("downloads.held_plan", move |store| {
                Ok((store.task_details(&task_id)?, store.task_plan(&task_id)?))
            })
            .await
            .map_err(unavailable)?;
        Ok(Some(crate::landing::target_for(&task, &details, &plan)))
    }

    /// The release the files go into and the copy they join: the planned
    /// copy's release when the download was fetched for one, else the
    /// album's chosen edition, else the one the file was matched to.
    async fn release_for(
        &self,
        library: &Arc<dyn LandingLibrary>,
        row: &HeldRow,
        target: Option<&specs::Target>,
    ) -> Result<(Release, Option<PlannedAlbum>), HeldError> {
        let unmatched = || {
            HeldError::Refused(
                "This file was never matched to an album, so it cannot be imported as it is. \
                 Discard it and download the album again."
                    .to_owned(),
            )
        };
        let planned = target.and_then(|target| target.planned_album.clone());
        let release_mbid = match &planned {
            Some(planned) => planned.release_mbid.clone(),
            None => {
                let group = row
                    .release_group_mbid
                    .as_deref()
                    .filter(|mbid| !mbid.is_empty())
                    .ok_or_else(unmatched)?;
                library
                    .chosen_edition(group)
                    .await
                    .or_else(|| row.release_mbid.clone())
                    .filter(|mbid| !mbid.is_empty())
                    .ok_or_else(unmatched)?
            }
        };
        let release = match library
            .release(&release_mbid, RequestPriority::UserInitiated)
            .await
        {
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
        Ok((release, planned))
    }

    /// Keep a held file held for `reason`, with the intake's words.
    async fn rehold(&self, row: &HeldRow, reason: Reason, detail: &str) -> Result<(), HeldError> {
        if row.reason == reason && row.reason_detail.as_deref() == Some(detail) {
            return Ok(());
        }
        let id = row.id;
        let detail = Some(detail.to_owned()).filter(|detail| !detail.is_empty());
        self.journal
            .run_foreground("downloads.held_rehold", move |store| {
                store.rehold(id, reason, detail.as_deref())
            })
            .await
            .map(|_| ())
            .map_err(unavailable)
    }

    async fn finish(&self, row: &HeldRow, status: &'static str) -> Result<(), HeldError> {
        let id = row.id;
        let now = self.journal.db().now_unix_f64();
        self.journal
            .run_foreground("downloads.held_resolve", move |store| {
                store.resolve_held(&[id], status, now)
            })
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn discard_rows(&self, rows: Vec<HeldRow>) -> Result<(), HeldError> {
        let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        let now = self.journal.db().now_unix_f64();
        self.journal
            .run_foreground("downloads.held_discard", move |store| {
                store.resolve_held(&ids, "discarded", now)
            })
            .await
            .map_err(unavailable)?;
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
        let task = match self.journal.read_task(&task_id).await {
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
        match self.journal.read_held(&viewer, None, Some(&task_id)).await {
            Ok(rows) if rows.is_empty() => {}
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(%task_id, %error, "held rows unreadable after a held import");
                return;
            }
        }
        if task.download_type != "track" {
            let owned = library.owned(&release.release_group_id, None).await;
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
        let now = self.journal.db().now_unix_f64();
        let id = task_id.clone();
        match self
            .journal
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

/// What placing held files did.
#[derive(Debug, Default)]
struct RowsPlaced {
    imported: usize,
    /// Where the imported files now live.
    paths: Vec<PathBuf>,
    /// The reason each file still held stays held for.
    held: Vec<Reason>,
}

impl RowsPlaced {
    /// Files imported; when none went in, why the first stays held.
    fn imported(self) -> Result<usize, HeldError> {
        match self.held.first() {
            Some(reason) if self.imported == 0 => Err(refused(*reason)),
            _ => Ok(self.imported),
        }
    }
}

/// The answer for a file that stays held.
fn refused(reason: Reason) -> HeldError {
    HeldError::Refused(format!("{} {}", reason.message(), reason.action()))
}

/// A held file as the intake reads it. `None` when it cannot be read as
/// audio.
async fn probe_held(path: &Path) -> Option<LandedFile> {
    let paths = vec![path.to_path_buf()];
    match tokio::task::spawn_blocking(move || probe::probe(&paths, &[])).await {
        Ok(landing) => landing.audio.into_iter().next(),
        Err(error) => {
            tracing::warn!(%error, "held file probe failed");
            None
        }
    }
}

/// The track of `release` a held file was paired to: by release track,
/// else by recording.
fn track_on(release: &Release, row: &HeldRow) -> Option<usize> {
    let same = |left: &str, right: Option<&str>| {
        right.is_some_and(|right| !right.is_empty() && left.eq_ignore_ascii_case(right))
    };
    release
        .tracks
        .iter()
        .position(|track| same(&track.id, row.release_track_mbid.as_deref()))
        .or_else(|| {
            release
                .tracks
                .iter()
                .position(|track| same(&track.recording_id, row.recording_mbid.as_deref()))
        })
}

/// Remove a held copy. A copy already gone is fine; any other failure
/// is logged and leaves a stray file in the held folder, nothing worse.
pub(crate) async fn remove_held_copy(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "held copy not removed");
    }
}
