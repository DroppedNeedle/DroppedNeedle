//! Drop import: music a person bought elsewhere, uploaded straight into
//! the library.
//!
//! An upload (loose files, folders, zips) becomes one job. The job is
//! unpacked and split into album-shaped items ([`extract`]), each item is
//! identified the way the scanner would ([`identify`]), and the files that
//! match go into the library through the same intake a finished download
//! uses ([`crate::intake`]): an upload better than the library's copy
//! replaces it (the old file goes to the recycle bin), and files the
//! release does not list go alongside the album. Files that did not go
//! in wait as `needs_review`, in the item's entry of the held list
//! ([`crate::held`]), where a person picks the album (Match), imports a
//! paired file, or throws them away.
//!
//! Every item that stops short carries one reason. A fully imported album
//! resolves its open request and wanted watch and tells everyone who asked
//! for it. Progress reaches the person's open tabs as
//! `drop_import_updated`.
//!
//! Jobs and items live in SQLite ([`store`]); a restart fails the jobs it
//! cut off (their staged files wait in the held list) instead of importing
//! anything twice.
//!
//! The import folder ([`crate::import_folder`]) runs its albums through the
//! same steps, one at a time, reading them where they are: its jobs belong
//! to no user, a file the library already holds as well waits for a person
//! rather than being consumed, the album's sidecars go with it, and what
//! went in leaves the folder only once the library holds its exact bytes.

pub mod extract;
pub mod http;
pub mod identify;
pub(crate) mod sql;
pub mod store;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::identify::Outcome;
use self::store::{
    DropItemStatus, DropJobSource, DropJobStatus, DropStore, ItemRecord, JobRecord, StoreError,
};
use crate::flows::seams::{FlowEvent, TickSink};
use crate::held::store::{
    Entry, EntryOrigin, FileState, GroupKey, HeldFile, HeldRows, NewEntry, NewFile,
    Pairing as HeldPairing,
};
use crate::import_folder::site::ImportSite;
use crate::intake;
use crate::landing::canonical_dirs;
use crate::landing::confine;
use crate::landing::matching::{FilePlan, FoundRelease, MatchSummary, plan};
use crate::landing::ports::LandingLibrary;
use crate::landing::probe::{Landing, probe_beneath};
use crate::landing::specs::Target;
use crate::reason::Reason;
use crate::requests::models::RequestKind;
use crate::requests::settle::announce_fulfilled;
use crate::requests::sqlite::RequestStore;
use droppedneedle_kernel::events::{
    DropImportUpdated, EventHub, RequestImported, UserNotice, new_event_id,
};

/// Most files one upload may carry.
pub const MAX_FILES_PER_DROP: usize = 25;
/// More audio than this in one folder is not one album.
const MAX_FILES_PER_UNIT: usize = 60;
/// Jobs listed per page.
const LIST_LIMIT: i64 = 50;
/// Where uploads stream before a job exists.
const INCOMING_DIR: &str = "_incoming";

/// How one item's run ended.
enum Ran {
    Done,
    /// The database closed for a shutdown; the job goes no further.
    Stopped,
}

/// How one album of the import folder's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FolderRun {
    /// Its outcome is recorded.
    Done,
    /// It waits on something of its own (an earlier publish of it still
    /// being recovered): it stays queued and the albums after it go on.
    PassOver,
    /// Something every album needs is not there now (MusicBrainz, AcoustID,
    /// the library, the database): nothing was recorded and it stays
    /// queued, spending no try.
    Waits,
    /// The database closed for a shutdown.
    Stopped,
}

/// Who is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub user_id: String,
    pub admin: bool,
}

/// Why a drop-import call was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropError {
    /// Bad input; the message is safe to show.
    Invalid(String),
    /// No such job or item for this person.
    NotFound,
    /// The item is not in a state that allows this.
    Conflict(String),
    /// Storage or the library failed; the cause is logged, not shown.
    Unavailable(String),
    /// The database was busy during the operation; nothing changed and
    /// the call can run again.
    Busy(String),
}

impl From<StoreError> for DropError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy { operation } => Self::Busy(operation),
            StoreError::Failed(_) => Self::Unavailable(error.to_string()),
        }
    }
}

/// How the startup sweep ends after a failed step, already logged: a busy
/// database is handed back so the sweep runs again, anything else stops
/// it for the next start.
fn sweep_stopped(error: StoreError) -> Result<(), StoreError> {
    match error {
        StoreError::Busy { .. } => Err(error),
        StoreError::Failed(_) => Ok(()),
    }
}

/// One grouped folder as a stored item: its name and its files' paths.
fn item_paths(unit: &extract::Unit) -> (String, Vec<String>) {
    let paths = unit
        .files
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    (unit.folder_name.clone(), paths)
}

/// What became of one file of an item.
#[derive(Debug, Clone)]
struct Fate {
    path: PathBuf,
    /// The release track it was paired to, when it was.
    pairing: Option<HeldPairing>,
    outcome: FileFate,
}

#[derive(Debug, Clone)]
enum FileFate {
    /// In the library now.
    Imported,
    /// The library already holds it as well or better.
    Owned,
    /// Waits for a person, for this reason and detail.
    Held(Reason, String),
}

impl Fate {
    fn held(path: PathBuf, reason: Reason, detail: &str) -> Self {
        Self {
            path,
            pairing: None,
            outcome: FileFate::Held(reason, detail.to_owned()),
        }
    }
}

/// What a hand match of an upload's held files did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Matched {
    /// Files now in the library.
    pub imported: usize,
    /// Files the library already held.
    pub owned: usize,
    /// Files still waiting.
    pub held: usize,
}

/// What organising one item counted.
#[derive(Debug, Default)]
struct Counts {
    imported: usize,
    /// Of `imported`, files that replaced a worse library copy.
    upgraded: usize,
    /// Extra files kept alongside the album.
    bonus: usize,
    owned: usize,
    occupied: usize,
    /// Better copies kept for review because the library's copy could not
    /// go to the recycle bin.
    blocked: usize,
    /// Of `imported` and `bonus`, files placed in a folder that keeps its
    /// own tags, as they came.
    tags_kept: usize,
    /// Files that wait for review.
    waiting: usize,
    /// Release tracks now in the library, out of the edition's tracks.
    covered: usize,
    expected: usize,
}

/// One item organised: its settled row and each file's fate, written
/// together by [`DropImports::record`].
struct Organised {
    item: ItemRecord,
    fates: Vec<Fate>,
    /// Files to remove once the record is written.
    consumed: Vec<PathBuf>,
    release: droppedneedle_library::matching::Release,
    placed: usize,
    fulfills: bool,
    /// The library put the whole import off, so nothing ran: why.
    deferred: Option<Reason>,
}

/// The drop-import service.
pub struct DropImports {
    store: DropStore,
    library: Arc<dyn LandingLibrary>,
    root: PathBuf,
    events: EventHub,
    ticks: Arc<dyn TickSink>,
    ledger: RequestStore,
    site: Arc<ImportSite>,
}

impl DropImports {
    /// Service over its store, the library port, a staging folder of its
    /// own, the event sink, the request stores it resolves, and the import
    /// folder its folder jobs read.
    pub fn new(
        store: DropStore,
        library: Arc<dyn LandingLibrary>,
        root: PathBuf,
        events: EventHub,
        ticks: Arc<dyn TickSink>,
        ledger: RequestStore,
        site: Arc<ImportSite>,
    ) -> Self {
        Self {
            store,
            library,
            root,
            events,
            ticks,
            ledger,
            site,
        }
    }

    /// The jobs and items.
    pub(crate) fn store(&self) -> &DropStore {
        &self.store
    }

    /// Where uploads stream before a job exists: the same disk as the job
    /// folders, so adopting them is a rename.
    pub fn incoming_dir(&self) -> PathBuf {
        self.root.join(INCOMING_DIR)
    }

    /// Register an upload as a job and start working on it in the
    /// background. `uploads` are (original name, file already on disk)
    /// pairs; they move into the job's staging folder.
    pub async fn create_job(
        self: &Arc<Self>,
        user_id: &str,
        user_name: &str,
        uploads: Vec<(String, PathBuf)>,
    ) -> Result<JobRecord, DropError> {
        if uploads.is_empty() {
            return Err(DropError::Invalid("No files were uploaded.".into()));
        }
        if self.library.library_dirs().is_empty() {
            return Err(DropError::Invalid(
                "Set a library folder before importing.".into(),
            ));
        }
        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let staging_dir = self.root.join(&job_id);
        let upload_name = match uploads.as_slice() {
            [only] => only.0.clone(),
            [first, rest @ ..] => format!("{} +{} more", first.0, rest.len()),
            [] => String::new(),
        };
        let dir = staging_dir.clone();
        tokio::task::spawn_blocking(move || stage(&dir, &uploads))
            .await
            .map_err(|error| DropError::Unavailable(format!("staging did not run: {error}")))?
            .map_err(|error| DropError::Unavailable(format!("staging failed: {error}")))?;
        self.store
            .create_job(
                &job_id,
                user_id,
                user_name,
                &upload_name,
                &staging_dir.to_string_lossy(),
            )
            .await?;
        let runner = self.clone();
        let spawned = job_id.clone();
        tokio::spawn(async move { runner.run_job(&spawned).await });
        self.publish(Some(user_id), &job_id);
        self.store
            .get_job(&job_id)
            .await?
            .ok_or(DropError::NotFound)
    }

    /// The person's newest jobs; admins may ask for everyone's.
    pub async fn list_jobs(&self, viewer: &Viewer, all: bool) -> Result<Vec<JobRecord>, DropError> {
        let user = (!(all && viewer.admin)).then_some(viewer.user_id.as_str());
        Ok(self.store.list_jobs(user, LIST_LIMIT).await?)
    }

    /// The staging folder uploads wait in: the held list's `drop` root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A curator picked the album for an upload's held files: they are
    /// paired with that album's tracks without the closeness check and
    /// placed, a file that would replace a library copy only once AcoustID
    /// confirms it. The caller holds the entry's claim; files that do not
    /// go in stay in the entry with why.
    pub(crate) async fn match_entry(
        &self,
        entry: &Entry,
        files: &[HeldFile],
        release_group_mbid: &str,
    ) -> Result<Matched, DropError> {
        let item_id = entry.drop_item_id.ok_or(DropError::NotFound)?;
        let item = self
            .store
            .get_item(item_id)
            .await?
            .ok_or(DropError::NotFound)?;
        let job = self
            .store
            .get_job(&item.job_id)
            .await?
            .ok_or(DropError::NotFound)?;
        if item.status != DropItemStatus::NeedsReview {
            return Err(DropError::Conflict(
                "Only files awaiting review can be matched.".into(),
            ));
        }
        let group = release_group_mbid.trim();
        if group.is_empty() {
            return Err(DropError::Invalid("Pick an album to match.".into()));
        }
        let library = self.library.clone();
        let beneath = self.folder_of(&job).await?;
        let mut paths: Vec<PathBuf> = Vec::with_capacity(files.len());
        for file in files {
            let path = match &beneath {
                Some(folder) => crate::held::below(folder, &file.path),
                None => Some(self.root.join(&file.path)),
            };
            let Some(path) = path else { continue };
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            // A file of the import folder that changed since it was listed
            // is not the one the person matched.
            if beneath.is_some() && !crate::held::as_listed(file, &meta) {
                return Err(DropError::Conflict(
                    "A file in the import folder changed since it was listed. \
                     DroppedNeedle looks at it again by itself."
                        .into(),
                ));
            }
            paths.push(path);
        }
        if paths.is_empty() {
            return Err(DropError::Invalid(match job.source {
                DropJobSource::Upload => {
                    "The uploaded files are no longer on disk. Drop them in again.".into()
                }
                DropJobSource::ImportFolder => {
                    "These files are no longer in the import folder.".into()
                }
            }));
        }
        if !self.store.claim_review(item.id).await? {
            return Err(DropError::Conflict(
                "This item is already being handled.".into(),
            ));
        }
        let landing = self
            .probe(library.as_ref(), paths, beneath.as_deref())
            .await;
        let summary = match identify::assigned(library.as_ref(), &landing, group).await {
            Outcome::Matched(summary) => summary,
            Outcome::Busy | Outcome::Closed => {
                let mut back = item.clone();
                back.status = DropItemStatus::NeedsReview;
                self.store
                    .save_item(&back, DropItemStatus::Processing)
                    .await?;
                return Err(DropError::Busy("drop match edition read".to_owned()));
            }
            other => {
                let reason = match other {
                    Outcome::Unavailable(detail) => {
                        tracing::warn!(item_id, %detail, "drop match: MusicBrainz unavailable");
                        Reason::MusicbrainzUnavailable
                    }
                    Outcome::AcoustidUnavailable => Reason::AcoustidUnavailable,
                    Outcome::Review { reason, .. } => reason,
                    Outcome::Matched(_) | Outcome::Busy | Outcome::Closed => Reason::ImportFailed,
                };
                let mut back = item.clone();
                back.status = DropItemStatus::NeedsReview;
                self.store
                    .save_item(&back, DropItemStatus::Processing)
                    .await?;
                return Err(DropError::Invalid(format!(
                    "{} {}",
                    reason.message(),
                    reason.action()
                )));
            }
        };
        let mut working = item.clone();
        working.status = DropItemStatus::Processing;
        let organised = self
            .organise(&job, working, library.as_ref(), &landing, &summary, true)
            .await;
        // The library put the import off: the files stay held as they
        // were, with their reason and pairing, so nothing retried later
        // goes onto the album this Match replaced.
        if let Some(reason) = organised.deferred {
            let mut back = item.clone();
            back.status = DropItemStatus::NeedsReview;
            self.store
                .save_item(&back, DropItemStatus::Processing)
                .await?;
            if reason == Reason::JournalUnreadable {
                return Err(DropError::Busy("drop match import".to_owned()));
            }
            return Err(DropError::Conflict(format!(
                "{} {}",
                reason.message(),
                reason.action()
            )));
        }
        let mut matched = Matched::default();
        for fate in &organised.fates {
            match fate.outcome {
                FileFate::Imported => matched.imported += 1,
                FileFate::Owned => matched.owned += 1,
                FileFate::Held(..) => matched.held += 1,
            }
        }
        self.finish(&job, organised, Some(entry)).await;
        Ok(matched)
    }

    /// The import folder's real path for an import-folder job, checked now;
    /// `None` for an upload. Refused while the job's folder is no longer the
    /// import folder, or cannot be used.
    async fn folder_of(&self, job: &JobRecord) -> Result<Option<PathBuf>, DropError> {
        if job.source != DropJobSource::ImportFolder {
            return Ok(None);
        }
        let site = self.site.clone();
        let dir = job.staging_dir.clone();
        let found = tokio::task::spawn_blocking(move || site.folder_of(&dir))
            .await
            .map_err(|error| {
                DropError::Unavailable(format!("folder check did not run: {error}"))
            })?;
        match found {
            Ok(folder) => Ok(Some(folder)),
            Err(crate::import_folder::site::Unusable::Off) => Err(DropError::Conflict(
                "These files are in an import folder that is no longer set.".into(),
            )),
            Err(_) => Err(DropError::Conflict(
                "The import folder can't be used right now. Check its status in Settings.".into(),
            )),
        }
    }

    /// Remove empty folders left in a drop item's job folder.
    pub(crate) async fn tidy_item(&self, item_id: i64) {
        match self.store.get_item(item_id).await {
            Ok(Some(item)) => match self.store.get_job(&item.job_id).await {
                Ok(Some(job)) => {
                    self.tidy(&job).await;
                    self.publish(job.user_id.as_deref(), &job.id);
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(item_id, %error, "drop job unreadable for tidying"),
            },
            Ok(None) => {}
            Err(error) => tracing::warn!(item_id, %error, "drop item unreadable for tidying"),
        }
    }

    /// Files of an upload or the import folder a person imported from the
    /// held list reached the library: tell plugins.
    pub(crate) fn announce_import(
        &self,
        release_group_mbid: &str,
        imported: usize,
        source: DropJobSource,
    ) {
        self.ticks.announce(FlowEvent::ImportFinished {
            release_group_mbid: release_group_mbid.to_owned(),
            track_count: i64::try_from(imported).unwrap_or(i64::MAX),
            source: plugin_source(source).to_owned(),
        });
    }

    /// Startup housekeeping: jobs a restart cut off fail and their files
    /// wait in the held list (a hand match cut off goes back to review),
    /// and staging folders with no file left waiting are removed, orphans
    /// and partial uploads included. When a cut-off upload cannot be read
    /// or kept, the sweep stops there: its job stays processing with its
    /// files, for a later sweep to group. A busy database stops it with an
    /// error, so the caller can run it again; any other failure is logged.
    pub async fn sweep_stale(&self) -> Result<(), StoreError> {
        // A job cut off before its upload became items: unpack and group
        // it now, so its files wait for review instead of being swept.
        match self.store.processing_jobs_without_items().await {
            Ok(jobs) => {
                for (job_id, dir) in jobs {
                    let folder = PathBuf::from(&dir);
                    let grouped =
                        tokio::task::spawn_blocking(move || extract::extract_and_group(&folder))
                            .await;
                    let units = match grouped {
                        Ok(Ok((units, _))) => units,
                        Ok(Err(error)) => {
                            tracing::warn!(%job_id, %error, "cut-off drop upload unreadable");
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(%job_id, %error, "cut-off drop upload not grouped");
                            continue;
                        }
                    };
                    let units = units.iter().map(item_paths).collect();
                    if let Err(error) = self.store.add_items(&job_id, units).await {
                        tracing::warn!(%job_id, %error, "cut-off drop upload not kept; staging left as is");
                        return sweep_stopped(error);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cut-off drop jobs unreadable; staging left as is");
                return sweep_stopped(error);
            }
        }
        match self
            .store
            .fail_stale_processing(Reason::Interrupted, &self.root)
            .await
        {
            Ok(0) => {}
            Ok(failed) => tracing::info!(jobs = failed, "drop imports cut off by a restart failed"),
            Err(error) => {
                tracing::warn!(%error, "drop import restart sweep failed; staging left as is");
                return sweep_stopped(error);
            }
        }
        // Every folder under the drop staging root goes, except those of
        // jobs with a file still waiting in the held list. That covers
        // finished jobs, orphaned folders no job names, half-written
        // uploads, and the uploads of a deleted user.
        let keep: HashSet<std::ffi::OsString> = match self.store.staging_dirs().await {
            Ok(dirs) => dirs
                .into_iter()
                .filter(|(_, keep)| *keep)
                .filter_map(|(dir, _)| PathBuf::from(dir).file_name().map(ToOwned::to_owned))
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "drop import jobs unreadable; staging left as is");
                return sweep_stopped(error);
            }
        };
        let root = self.root.clone();
        let swept = tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&root) else {
                return;
            };
            for entry in entries.flatten() {
                if keep.contains(&entry.file_name()) {
                    continue;
                }
                let dir = entry.path();
                let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    std::fs::remove_dir_all(&dir)
                } else {
                    std::fs::remove_file(&dir)
                };
                if let Err(error) = removed {
                    tracing::warn!(dir = %dir.display(), %error, "drop staging not removed");
                }
            }
        })
        .await;
        if let Err(error) = swept {
            tracing::warn!(%error, "drop staging sweep did not run");
        }
        Ok(())
    }

    /// Run one job to the end; a crash records a failed job instead of
    /// vanishing.
    async fn run_job(&self, job_id: &str) {
        let Ok(Some(job)) = self.store.get_job(job_id).await else {
            tracing::warn!(job_id, "drop import job vanished before it ran");
            return;
        };
        tracing::info!(job_id, "drop import job started");
        let cause = match self.process_job(&job).await {
            Ok(Some((status, items))) => {
                tracing::info!(
                    job_id,
                    status = status.as_str(),
                    items,
                    "drop import job finished"
                );
                return;
            }
            // The server is stopping: the job stays processing, and the
            // sweep at the next start settles it.
            Ok(None) => {
                tracing::info!(job_id, "drop import job stopped for a shutdown");
                return;
            }
            Err(cause) => cause,
        };
        tracing::error!(job_id, %cause, "drop import job failed");
        if let Err(error) = self
            .store
            .set_job_status(
                job_id,
                DropJobStatus::Failed,
                Some("The import didn't finish. Check the server log.".to_owned()),
            )
            .await
        {
            tracing::warn!(job_id, %error, "drop import failure not recorded");
        }
        self.publish(job.user_id.as_deref(), job_id);
    }

    /// Answers the job's final status and how many items it made, or
    /// `None` when a shutdown stopped it.
    async fn process_job(&self, job: &JobRecord) -> Result<Option<(DropJobStatus, usize)>, String> {
        let dir = PathBuf::from(&job.staging_dir);
        let (units, notes) = tokio::task::spawn_blocking(move || extract::extract_and_group(&dir))
            .await
            .map_err(|error| format!("unpacking did not run: {error}"))?
            .map_err(|error| format!("unpacking failed: {error}"))?;
        if units.is_empty() {
            let error = notes
                .first()
                .cloned()
                .unwrap_or_else(|| "No audio files were found in the upload.".to_owned());
            self.store
                .set_job_status(&job.id, DropJobStatus::Failed, Some(error))
                .await
                .map_err(|error| error.to_string())?;
            self.publish(job.user_id.as_deref(), &job.id);
            return Ok(Some((DropJobStatus::Failed, 0)));
        }
        let ids = self
            .store
            .add_items(&job.id, units.iter().map(item_paths).collect())
            .await
            .map_err(|error| error.to_string())?;
        let items: Vec<_> = ids.into_iter().zip(units).collect();
        self.publish(job.user_id.as_deref(), &job.id);
        let count = items.len();
        for (id, unit) in items {
            match self
                .process_item(job, id, &unit.folder_name, unit.files)
                .await
            {
                Ok(Ran::Done) => {}
                // Nothing more is written and nothing is tidied: the files
                // stay staged for the next start's sweep.
                Ok(Ran::Stopped) => return Ok(None),
                Err(cause) => {
                    tracing::error!(item_id = id, %cause, "drop import item failed");
                    if let Ok(Some(item)) = self.store.get_item(id).await {
                        let failed =
                            settled(item, DropItemStatus::Failed, Reason::ImportFailed, None);
                        if let Err(error) = self
                            .store
                            .save_item(&failed, DropItemStatus::Processing)
                            .await
                        {
                            tracing::warn!(item_id = id, %error, "drop item failure not recorded");
                        }
                    }
                }
            }
            self.publish(job.user_id.as_deref(), &job.id);
        }
        // A damaged archive next to good ones is still reported: the job
        // completes and the notes say what was skipped.
        let note = (!notes.is_empty()).then(|| notes.join(" "));
        self.store
            .set_job_status(&job.id, DropJobStatus::Completed, note)
            .await
            .map_err(|error| error.to_string())?;
        self.tidy(job).await;
        self.publish(job.user_id.as_deref(), &job.id);
        Ok(Some((DropJobStatus::Completed, count)))
    }

    async fn process_item(
        &self,
        job: &JobRecord,
        item_id: i64,
        folder: &str,
        paths: Vec<PathBuf>,
    ) -> Result<Ran, String> {
        let item = self
            .store
            .get_item(item_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("item vanished")?;
        let library = self.library.clone();
        let landing = self.probe(library.as_ref(), paths, None).await;
        let unreadable = landing.unreadable.len();
        if landing.audio.is_empty() {
            let mut failed = settled(item, DropItemStatus::Failed, Reason::Corrupt, None);
            failed.staging_paths = Vec::new();
            self.save(&failed).await;
            return Ok(Ran::Done);
        }
        if landing.audio.len() > MAX_FILES_PER_UNIT {
            let specifics = format!("It has {} audio files.", landing.audio.len());
            self.wait_whole(job, item, Reason::TooManyFiles, Some(specifics))
                .await;
            return Ok(Ran::Done);
        }
        match identify::identify(library.as_ref(), &landing, folder).await {
            Outcome::Matched(summary) => {
                let organised = self
                    .organise(job, item, library.as_ref(), &landing, &summary, false)
                    .await;
                self.finish(job, organised, None).await;
            }
            Outcome::Review { reason, detail } => {
                let detail = with_unreadable(detail, unreadable);
                self.wait_whole(job, item, reason, detail).await;
            }
            Outcome::Unavailable(cause) => {
                tracing::warn!(item_id, %cause, "drop import: MusicBrainz unavailable");
                self.wait_whole(job, item, Reason::MusicbrainzUnavailable, None)
                    .await;
            }
            Outcome::AcoustidUnavailable => {
                tracing::info!(item_id, "drop import: AcoustID unavailable");
                self.wait_whole(job, item, Reason::AcoustidUnavailable, None)
                    .await;
            }
            // Background identification reads a busy edition until it
            // answers, so busy should never reach here.
            Outcome::Busy => {
                tracing::warn!(
                    item_id,
                    "drop import: identification answered busy; item left as it is"
                );
            }
            Outcome::Closed => return Ok(Ran::Stopped),
        }
        Ok(Ran::Done)
    }

    /// Run one album of the import folder `folder` (its real path, checked
    /// just now): identify it, file it with its sidecars, and record what
    /// became of each file. A wait on something every album needs records
    /// nothing, so the album stays queued. Files a publish of this album
    /// already put in the library (a restart cut it off before its record)
    /// are not imported again: they only wait to leave the folder.
    pub(crate) async fn run_folder_item(&self, folder: &Path, item: ItemRecord) -> FolderRun {
        let job = match self.store.get_job(&item.job_id).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                tracing::warn!(item_id = item.id, "import folder: the album's job vanished");
                return FolderRun::PassOver;
            }
            Err(error) => {
                tracing::warn!(item_id = item.id, %error, "import folder: the album's job does not read");
                return FolderRun::Waits;
            }
        };
        let library = self.library.clone();
        let placed = match library.placed_sources(&format!("drop-{}", item.id)).await {
            Ok(Some(placed)) => placed,
            Ok(None) => {
                tracing::info!(
                    item_id = item.id,
                    "import folder: an earlier publish of the album waits for recovery"
                );
                return FolderRun::PassOver;
            }
            Err(error) => {
                tracing::warn!(item_id = item.id, %error, "import folder: the publish journal does not read");
                return FolderRun::Waits;
            }
        };
        let mut paths: Vec<PathBuf> = Vec::with_capacity(item.staging_paths.len());
        for rel in &item.staging_paths {
            match crate::held::below(folder, rel) {
                Some(path) => paths.push(path),
                None => tracing::warn!(rel, "import folder: a path that climbs out is left alone"),
            }
        }
        let mut already: Vec<String> = Vec::new();
        if !placed.is_empty() {
            let folders = vec![folder.to_path_buf()];
            let checked = paths.clone();
            let in_library = tokio::task::spawn_blocking(move || {
                checked
                    .iter()
                    .map(|path| {
                        confine::hash_beneath(&folders, path)
                            .is_ok_and(|hashed| placed.contains(&hashed.sha256))
                    })
                    .collect::<Vec<bool>>()
            })
            .await
            .unwrap_or_default();
            let mut rest = Vec::with_capacity(paths.len());
            for (index, path) in paths.into_iter().enumerate() {
                if in_library.get(index).copied().unwrap_or(false) {
                    already.push(rel_below(folder, &path));
                } else {
                    rest.push(path);
                }
            }
            paths = rest;
        }
        let landing = self.probe(library.as_ref(), paths, Some(folder)).await;
        let name = match item.folder_name.rsplit('/').next() {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => extract::LOOSE_UNIT_NAME.to_owned(),
        };
        // Everything readable went in before a restart cut the album off.
        if landing.audio.is_empty()
            && landing.unreadable.is_empty()
            && landing.samples.is_empty()
            && !already.is_empty()
        {
            return self.settle_placed(&job, item, folder, already, &name).await;
        }
        if landing.audio.is_empty() {
            self.folder_wait(&job, item, &landing, Reason::Corrupt, None, already)
                .await;
            return FolderRun::Done;
        }
        if landing.audio.len() > MAX_FILES_PER_UNIT {
            let specifics = format!("It has {} audio files.", landing.audio.len());
            self.folder_wait(
                &job,
                item,
                &landing,
                Reason::TooManyFiles,
                Some(specifics),
                already,
            )
            .await;
            return FolderRun::Done;
        }
        match identify::identify(library.as_ref(), &landing, &name).await {
            Outcome::Matched(summary) => {
                let mut organised = self
                    .organise(&job, item, library.as_ref(), &landing, &summary, false)
                    .await;
                // A wait every album shares leaves this one queued; one of
                // its own (a naming script, a fault of this import) holds it
                // as a leftover, which the held list retries by itself.
                if let Some(
                    reason @ (Reason::LibraryOff
                    | Reason::LibraryFolderMissing
                    | Reason::JournalUnreadable
                    | Reason::AcoustidUnavailable),
                ) = organised.deferred
                {
                    tracing::info!(
                        item_id = organised.item.id,
                        code = reason.code(),
                        "import folder: the library put the album off; it waits"
                    );
                    return FolderRun::Waits;
                }
                // Sidecars an earlier publish placed leave with its files.
                if !already.is_empty() {
                    let (root, album) = (folder.to_path_buf(), organised.item.folder_name.clone());
                    let sidecars = tokio::task::spawn_blocking(move || {
                        crate::import_folder::walk::sidecars(&root, &album)
                    })
                    .await
                    .unwrap_or_default();
                    for (below, _) in sidecars {
                        if !organised.item.staging_paths.contains(&below) {
                            organised.item.staging_paths.push(below);
                        }
                    }
                }
                organised.item.staging_paths.extend(already);
                self.finish(&job, organised, None).await;
            }
            Outcome::Review { reason, detail } => {
                self.folder_wait(&job, item, &landing, reason, detail, already)
                    .await;
            }
            Outcome::Unavailable(cause) => {
                tracing::warn!(item_id = item.id, %cause, "import folder: MusicBrainz unavailable; the album waits");
                return FolderRun::Waits;
            }
            Outcome::AcoustidUnavailable => {
                tracing::info!(
                    item_id = item.id,
                    "import folder: AcoustID unavailable; the album waits"
                );
                return FolderRun::Waits;
            }
            Outcome::Busy => return FolderRun::Waits,
            Outcome::Closed => return FolderRun::Stopped,
        }
        FolderRun::Done
    }

    /// An import-folder album all of whose files a publish already put in
    /// the library before a restart cut it off: it settles as imported, the
    /// album's requests and plugins hear of it, and its files and sidecars
    /// wait to leave the folder.
    async fn settle_placed(
        &self,
        job: &JobRecord,
        item: ItemRecord,
        folder: &Path,
        already: Vec<String>,
        name: &str,
    ) -> FolderRun {
        let library = self.library.clone();
        let all: Vec<PathBuf> = already
            .iter()
            .filter_map(|rel| crate::held::below(folder, rel))
            .collect();
        let landing = self.probe(library.as_ref(), all, Some(folder)).await;
        let release = match identify::identify(library.as_ref(), &landing, name).await {
            Outcome::Matched(summary) => summary.best().map(|found| {
                let fulfills = found.release.tracks.iter().all(|track| summary.owns(track));
                (found.release.clone(), fulfills)
            }),
            Outcome::Unavailable(_) | Outcome::AcoustidUnavailable | Outcome::Busy => {
                return FolderRun::Waits;
            }
            Outcome::Closed => return FolderRun::Stopped,
            Outcome::Review { .. } => None,
        };
        let (root, album) = (folder.to_path_buf(), item.folder_name.clone());
        let sidecars = tokio::task::spawn_blocking(move || {
            crate::import_folder::walk::sidecars(&root, &album)
        })
        .await
        .unwrap_or_default();
        let count = already.len();
        let mut leaving = already;
        leaving.extend(sidecars.into_iter().map(|(below, _)| below));
        let imported = ItemRecord {
            status: DropItemStatus::Imported,
            detail: Some(format!(
                "Imported {count} {}.",
                if count == 1 { "file" } else { "files" }
            )),
            reason_code: None,
            release_group_mbid: release
                .as_ref()
                .map(|(release, _)| release.release_group_id.clone())
                .or(item.release_group_mbid.clone()),
            album_title: release
                .as_ref()
                .map(|(release, _)| release.title.clone())
                .or(item.album_title.clone()),
            artist_name: release
                .as_ref()
                .map(|(release, _)| release.artist_text())
                .or(item.artist_name.clone()),
            files_imported: item
                .files_imported
                .saturating_add(i64::try_from(count).unwrap_or(i64::MAX)),
            staging_paths: leaving,
            ..item
        };
        if !self.record(job, &imported, &[], None).await {
            return FolderRun::PassOver;
        }
        if let Some((release, fulfills)) = release {
            self.after_import(job, &release, count, fulfills).await;
        }
        FolderRun::Done
    }

    /// Every file of an import-folder album waits for a person: its audio
    /// for `reason`, unreadable files as unreadable, samples as samples.
    /// `done` are files already in the library, which only wait to leave
    /// the folder.
    async fn folder_wait(
        &self,
        job: &JobRecord,
        item: ItemRecord,
        landing: &Landing,
        reason: Reason,
        specifics: Option<String>,
        done: Vec<String>,
    ) {
        let detail = specifics.clone().unwrap_or_default();
        let fates: Vec<Fate> = landing
            .audio
            .iter()
            .map(|file| Fate::held(file.path.clone(), reason, &detail))
            .chain(
                landing
                    .unreadable
                    .iter()
                    .map(|path| Fate::held(path.clone(), Reason::Corrupt, "")),
            )
            .chain(
                landing
                    .samples
                    .iter()
                    .map(|path| Fate::held(path.clone(), Reason::Samples, "")),
            )
            .collect();
        let status = if fates.is_empty() {
            DropItemStatus::Skipped
        } else {
            DropItemStatus::NeedsReview
        };
        let mut settled = settled(item, status, reason, specifics);
        settled.staging_paths = done;
        self.record(job, &settled, &fates, None).await;
    }

    /// Every staged file of an item waits in the held list for `reason`.
    async fn wait_whole(
        &self,
        job: &JobRecord,
        item: ItemRecord,
        reason: Reason,
        specifics: Option<String>,
    ) {
        let detail = specifics.clone().unwrap_or_default();
        let fates: Vec<Fate> = item
            .staging_paths
            .iter()
            .map(|path| Fate::held(PathBuf::from(path), reason, &detail))
            .collect();
        let mut review = settled(item, DropItemStatus::NeedsReview, reason, specifics);
        review.staging_paths = Vec::new();
        self.record(job, &review, &fates, None).await;
    }

    /// Put an identified item's files in the library and work out what
    /// became of each file and of the item. Nothing is written here:
    /// [`Self::finish`] records it.
    async fn organise(
        &self,
        job: &JobRecord,
        mut item: ItemRecord,
        library: &dyn LandingLibrary,
        landing: &Landing,
        summary: &MatchSummary,
        manual: bool,
    ) -> Organised {
        let from_folder = job.source == DropJobSource::ImportFolder;
        // An upload ignores files that do not read; in the import folder
        // they are the person's own and wait for them.
        let unreadable_fates = || -> Vec<Fate> {
            if !from_folder {
                return Vec::new();
            }
            landing
                .unreadable
                .iter()
                .map(|path| Fate::held(path.clone(), Reason::Corrupt, ""))
                .collect()
        };
        let all_wait = |item: ItemRecord, reason: Reason, specifics: Option<String>| {
            let detail = specifics.clone().unwrap_or_default();
            let fates: Vec<Fate> = landing
                .audio
                .iter()
                .map(|file| Fate::held(file.path.clone(), reason, &detail))
                .chain(
                    landing
                        .samples
                        .iter()
                        .map(|path| Fate::held(path.clone(), Reason::Samples, "")),
                )
                .chain(unreadable_fates())
                .collect();
            let mut review = settled(item, DropItemStatus::NeedsReview, reason, specifics);
            review.staging_paths = Vec::new();
            (review, fates)
        };
        let Some(found) = summary.best() else {
            let (item, fates) = all_wait(item, Reason::NoAlbumMatch, None);
            return Organised {
                item,
                fates,
                consumed: Vec::new(),
                release: droppedneedle_library::matching::Release::default(),
                placed: 0,
                fulfills: false,
                deferred: None,
            };
        };
        let release = &found.release;
        let Some((_, file_plan, pairing)) =
            album_pairing(summary, landing.audio.len(), manual, true)
        else {
            let (item, fates) = all_wait(item, Reason::NoAlbumMatch, None);
            return Organised {
                item,
                fates,
                consumed: Vec::new(),
                release: release.clone(),
                placed: 0,
                fulfills: false,
                deferred: None,
            };
        };
        let folder = PathBuf::from(&job.staging_dir);
        let (origin, sidecars) = if from_folder {
            let (root, album) = (folder.clone(), item.folder_name.clone());
            let found = tokio::task::spawn_blocking(move || {
                crate::import_folder::walk::sidecars(&root, &album)
            })
            .await
            .unwrap_or_default();
            (
                intake::Origin::Folder {
                    drop_item_id: item.id,
                    folders: vec![folder.clone()],
                },
                found
                    .into_iter()
                    .map(|(below, rel)| intake::SidecarFile {
                        path: folder.join(below),
                        rel,
                    })
                    .collect(),
            )
        } else {
            (
                intake::Origin::Upload {
                    drop_item_id: item.id,
                },
                Vec::new(),
            )
        };
        let batch = intake::IntakeBatch {
            files: landing
                .audio
                .iter()
                .map(|file| intake::IntakeFile {
                    file: file.clone(),
                    held_for: None,
                })
                .collect(),
            target: intake::Target {
                release: release.clone(),
                join: None,
                asked: Vec::new(),
            },
            origin,
            gate: if manual {
                intake::Gate::PersonChoseAlbum
            } else {
                intake::Gate::Automatic
            },
            owner: job.user_id.clone(),
            sidecars,
        };
        let pairing_of = |file: usize, track: Option<usize>| {
            let track = track.or_else(|| found.scored.pair_for(file).map(|pair| pair.track))?;
            let on = release.tracks.get(track)?;
            Some(HeldPairing {
                release_mbid: Some(release.id.clone()),
                release_track_mbid: Some(on.id.clone()),
                recording_mbid: Some(on.recording_id.clone()),
                disc_number: Some(i64::from(on.disc.max(1))),
                track_number: Some(i64::from(on.position)),
                track_title: Some(on.title.clone()),
                expected_duration_seconds: on.length_ms.map(|ms| ms as f64 / 1000.0),
            })
        };
        let placed = match intake::place(library, &batch, pairing, None).await {
            Ok(placed) => placed,
            Err(deferred) => {
                tracing::warn!(item_id = item.id, detail = %deferred.detail, "library could not take a drop import");
                // Nothing ran: every file waits with the item. A library that
                // changed under the import twice keeps the publisher's words.
                let (reason, specifics) = match deferred.reason {
                    Reason::LibraryChanged => (Reason::LibraryChanged, Some(deferred.detail)),
                    Reason::NamingScriptFailed => (Reason::NamingScriptFailed, None),
                    Reason::LibraryFolderMissing => {
                        (Reason::LibraryFolderMissing, Some(deferred.detail))
                    }
                    Reason::LibraryOff => (Reason::LibraryOff, None),
                    Reason::AcoustidUnavailable => (Reason::AcoustidUnavailable, None),
                    _ => (Reason::LibraryUnavailable, None),
                };
                // An automatic upload is held paired, so the held list retries
                // it by itself, a wait for AcoustID included (checked as an
                // automatic import is); a person's Match answers busy and
                // changes nothing.
                let put_off = match deferred.reason {
                    Reason::JournalUnreadable => Reason::JournalUnreadable,
                    _ => reason,
                };
                let (item, mut fates) = all_wait(item, reason, specifics);
                for (file, fate) in fates.iter_mut().take(landing.audio.len()).enumerate() {
                    fate.pairing = pairing_of(file, None);
                }
                return Organised {
                    item,
                    fates,
                    consumed: Vec::new(),
                    release: release.clone(),
                    placed: 0,
                    fulfills: false,
                    deferred: Some(put_off),
                };
            }
        };

        let mut done = Counts {
            expected: release.tracks.len(),
            tags_kept: placed.tags_kept,
            ..Counts::default()
        };
        let mut imported_tracks: HashSet<usize> = HashSet::new();
        // Imported and already owned uploads are consumed; the rest wait
        // for a person, never deleted.
        let mut consumed: Vec<PathBuf> = Vec::new();
        let mut fates: Vec<Fate> = Vec::new();
        let mut first_held: Option<Reason> = None;
        for (file, outcome) in placed.outcomes.iter().enumerate() {
            let path = landing.audio[file].path.clone();
            let (outcome, track) = match outcome {
                intake::Outcome::Imported {
                    track: Some(track),
                    upgraded,
                } => {
                    done.imported += 1;
                    done.upgraded += usize::from(*upgraded);
                    imported_tracks.insert(*track);
                    consumed.push(path.clone());
                    (FileFate::Imported, Some(*track))
                }
                intake::Outcome::Imported { track: None, .. } => {
                    done.bonus += 1;
                    consumed.push(path.clone());
                    (FileFate::Imported, None)
                }
                // The import folder's file is a person's own copy: one no
                // better than the library's waits for them, never removed.
                intake::Outcome::AlreadyOwned { track } if from_folder => {
                    first_held.get_or_insert(Reason::NoBetterThanYours);
                    (
                        FileFate::Held(Reason::NoBetterThanYours, String::new()),
                        Some(*track),
                    )
                }
                intake::Outcome::AlreadyOwned { track } => {
                    done.owned += 1;
                    consumed.push(path.clone());
                    (FileFate::Owned, Some(*track))
                }
                intake::Outcome::Held {
                    reason,
                    detail,
                    track,
                } => {
                    match reason {
                        Reason::TargetOccupied => done.occupied += 1,
                        Reason::UpgradeBlocked => {
                            tracing::warn!(
                                item_id = item.id,
                                "library copy not taken away; the upgrade waits for review"
                            );
                            done.blocked += 1;
                        }
                        _ => {}
                    }
                    first_held.get_or_insert(*reason);
                    (FileFate::Held(*reason, detail.clone()), *track)
                }
                // Not paired with any track: it waits, unpaired.
                intake::Outcome::Untouched => {
                    (FileFate::Held(Reason::NoAlbumMatch, String::new()), None)
                }
            };
            let pairing = match outcome {
                FileFate::Held(..) => pairing_of(file, track),
                _ => None,
            };
            fates.push(Fate {
                path,
                pairing,
                outcome,
            });
        }
        fates.extend(
            landing
                .samples
                .iter()
                .map(|path| Fate::held(path.clone(), Reason::Samples, "")),
        );
        fates.extend(unreadable_fates());
        done.waiting = fates
            .iter()
            .filter(|fate| matches!(fate.outcome, FileFate::Held(..)))
            .count();
        let owned_tracks: HashSet<usize> = found
            .scored
            .pairs
            .iter()
            .filter(|pair| file_plan.owned.contains(&pair.local))
            .map(|pair| pair.track)
            .collect();
        done.covered = release
            .tracks
            .iter()
            .enumerate()
            .filter(|(index, track)| {
                imported_tracks.contains(index)
                    || owned_tracks.contains(index)
                    || summary.owns(track)
            })
            .count();

        let unreadable = landing.unreadable.len();
        let placed = done.imported + done.bonus;
        let fulfills = done.imported > 0
            && done.covered == done.expected
            && done.expected > 0
            && unreadable == 0
            && done.occupied == 0
            && done.blocked == 0;
        item.release_group_mbid = Some(release.release_group_id.clone());
        item.album_title = Some(release.title.clone());
        item.artist_name = Some(release.artist_text());
        item.files_imported = item
            .files_imported
            .saturating_add(i64::try_from(placed).unwrap_or(i64::MAX));
        // What went in leaves the import folder once the library holds its
        // bytes; the sidecars with it, those the library took.
        item.staging_paths = if from_folder {
            let mut leaving: Vec<String> = consumed
                .iter()
                .map(|path| rel_below(&folder, path))
                .collect();
            if placed > 0 {
                leaving.extend(
                    batch
                        .sidecars
                        .iter()
                        .map(|sidecar| rel_below(&folder, &sidecar.path)),
                );
            }
            leaving
        } else {
            Vec::new()
        };
        let summary_text = describe(&done, unreadable, fulfills);
        // Files still waiting keep the item waiting, with why the first one
        // was held; a file nothing paired waits as an album not worked out.
        let item = if done.waiting > 0 {
            let reason = fates
                .iter()
                .filter_map(|fate| match fate.outcome {
                    FileFate::Held(reason, _) if reason != Reason::Samples => Some(reason),
                    _ => None,
                })
                .next()
                .or(first_held)
                .unwrap_or(Reason::NoAlbumMatch);
            ItemRecord {
                status: DropItemStatus::NeedsReview,
                detail: Some(if placed > 0 || done.owned > 0 {
                    summary_text
                } else {
                    reason.message().to_owned()
                }),
                reason_code: Some(reason),
                ..item
            }
        } else if placed > 0 {
            ItemRecord {
                status: DropItemStatus::Imported,
                detail: Some(summary_text),
                reason_code: None,
                ..item
            }
        } else {
            ItemRecord {
                status: DropItemStatus::Skipped,
                detail: Some(format!("{} {summary_text}", Reason::AlreadyOwned.message())),
                reason_code: Some(Reason::AlreadyOwned),
                ..item
            }
        };
        Organised {
            item,
            fates,
            consumed,
            release: release.clone(),
            placed,
            fulfills,
            deferred: None,
        }
    }

    /// Write what organising an item did, then remove the files it
    /// consumed and settle requests. A hand match passes the entry it
    /// claimed.
    async fn finish(&self, job: &JobRecord, organised: Organised, entry: Option<&Entry>) {
        let Organised {
            item,
            fates,
            consumed,
            release,
            placed,
            fulfills,
            ..
        } = organised;
        let recorded = self.record(job, &item, &fates, entry).await;
        // The import folder's files leave it only by its own check, once
        // the library holds their exact bytes.
        if recorded && job.source == DropJobSource::Upload {
            intake::dispose::remove_sources(consumed, vec![PathBuf::from(&job.staging_dir)]).await;
        }
        self.tidy(job).await;
        if placed > 0 {
            self.after_import(job, &release, placed, fulfills).await;
        }
        self.publish(job.user_id.as_deref(), &job.id);
    }

    /// Settle a processing item and its files in one transaction: files
    /// still waiting join the item's entry in the held list (made for it,
    /// or the one a hand match claimed), and the claimed entry's files that
    /// went in or were already in the library are decided. Answers whether
    /// it was written; a lost race or a failure is logged.
    async fn record(
        &self,
        job: &JobRecord,
        item: &ItemRecord,
        fates: &[Fate],
        entry: Option<&Entry>,
    ) -> bool {
        let from_folder = job.source == DropJobSource::ImportFolder;
        let root = if from_folder {
            PathBuf::from(&job.staging_dir)
        } else {
            self.root.clone()
        };
        let relative = |path: &Path| {
            path.strip_prefix(&root)
                .ok()
                .map(|rel| rel.to_string_lossy().into_owned())
        };
        // A file left in the import folder is listed as it is now, so one
        // that changes later is never acted on as this file.
        let listed: std::collections::HashMap<PathBuf, (i64, i64)> = if from_folder {
            let paths: Vec<PathBuf> = fates.iter().map(|fate| fate.path.clone()).collect();
            tokio::task::spawn_blocking(move || {
                paths
                    .into_iter()
                    .filter_map(|path| {
                        let meta = std::fs::symlink_metadata(&path).ok()?;
                        Some((path, crate::held::listed_identity(&meta)))
                    })
                    .collect()
            })
            .await
            .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        let mut waiting: Vec<NewFile> = Vec::new();
        let mut decided: Vec<(String, FileFate, Option<HeldPairing>)> = Vec::new();
        for fate in fates {
            let Some(rel) = relative(&fate.path) else {
                tracing::warn!(path = %fate.path.display(), "drop file outside the staging folder left out of the held list");
                continue;
            };
            if let (None, FileFate::Held(reason, detail)) = (entry, &fate.outcome) {
                let file = if from_folder {
                    NewFile::import_folder(&rel, *reason)
                } else {
                    NewFile::upload(&rel, *reason)
                };
                let identity = listed.get(&fate.path);
                waiting.push(NewFile {
                    pairing: fate.pairing.clone().unwrap_or_default(),
                    reason_detail: Some(detail.clone()).filter(|detail| !detail.is_empty()),
                    file_size: identity.map(|identity| identity.0),
                    file_modified_at: identity.map(|identity| identity.1),
                    ..file
                });
            } else {
                decided.push((rel, fate.outcome.clone(), fate.pairing.clone()));
            }
        }
        let new_entry = NewEntry {
            user_id: job.user_id.clone(),
            origin: if from_folder {
                EntryOrigin::ImportFolder
            } else {
                EntryOrigin::Upload
            },
            key: GroupKey::Upload(item.id),
            release_group_mbid: item.release_group_mbid.clone(),
            recording_mbid: None,
            drop_item_id: Some(item.id),
            artist_name: item.artist_name.clone(),
            album_title: item.album_title.clone(),
        };
        let claimed = entry.map(|entry| {
            (
                entry.id,
                entry
                    .files
                    .iter()
                    .map(|file| (file.path.clone(), file.id))
                    .collect::<std::collections::HashMap<_, _>>(),
            )
        });
        let item_id = item.id;
        let (settled_folder, settled_status, settled_reason) =
            (item.folder_name.clone(), item.status, item.reason_code);
        let item = item.clone();
        let clock = self.store.db().clock().clone();
        let written = self
            .store
            .db()
            .write_background("drop_import.record", move |tx| {
                let at = crate::db::unix_secs_f64(clock.as_ref());
                let now = clock.now_ms();
                if !store::write_item(tx, &item, DropItemStatus::Processing, at)? {
                    return Ok(false);
                }
                let rows = HeldRows::new(tx);
                if !waiting.is_empty() {
                    rows.hold(&new_entry, &waiting, now)?;
                }
                if let Some((entry_id, by_path)) = claimed {
                    for (rel, outcome, pairing) in &decided {
                        let Some(id) = by_path.get(rel) else {
                            continue;
                        };
                        match outcome {
                            FileFate::Imported => {
                                rows.resolve(entry_id, &[*id], FileState::Imported, now)?;
                            }
                            FileFate::Owned => {
                                rows.resolve(entry_id, &[*id], FileState::AlreadyOwned, now)?;
                            }
                            FileFate::Held(reason, detail) => {
                                rows.rehold(*id, *reason, Some(detail))?;
                                // A file the Match left unpaired loses the
                                // pairing it had, so nothing retried later
                                // places it on the album this Match replaced.
                                rows.pair(*id, &pairing.clone().unwrap_or_default())?;
                            }
                        }
                    }
                    rows.release(entry_id, now)?;
                }
                Ok(true)
            })
            .await;
        match written {
            Ok(true) => {
                if from_folder {
                    tracing::info!(
                        item_id,
                        folder = %settled_folder,
                        status = settled_status.as_str(),
                        reason = settled_reason.map(Reason::code),
                        "import folder: album settled"
                    );
                }
                true
            }
            Ok(false) => {
                tracing::warn!(item_id, "drop item changed under the import");
                false
            }
            Err(error) => {
                tracing::warn!(%error, "drop item and its held files not saved");
                false
            }
        }
    }

    /// After files reached the library: tell plugins, and when the album
    /// is now complete, resolve its request and wanted watch and tell
    /// everyone else who asked for it.
    async fn after_import(
        &self,
        job: &JobRecord,
        release: &droppedneedle_library::matching::Release,
        imported: usize,
        fulfills: bool,
    ) {
        let group = release.release_group_id.clone();
        self.ticks.announce(FlowEvent::ImportFinished {
            release_group_mbid: group.clone(),
            track_count: i64::try_from(imported).unwrap_or(i64::MAX),
            source: plugin_source(job.source).to_owned(),
        });
        // A partial import keeps the request and watch open for the normal
        // recovery paths.
        if !fulfills {
            return;
        }
        let at = self.store.db().now_epoch();
        let record = match self.ledger.get(RequestKind::Album, &group).await {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(%group, ?error, "drop import could not read the request");
                None
            }
        };
        match self
            .ledger
            .settle_in_library(&group, crate::requests::ledger::WatchOutcome::Imported, at)
            .await
        {
            Ok(fulfilled) => {
                // A request with no owner left is credited to the uploader.
                let fulfilled = fulfilled
                    .into_iter()
                    .map(|mut row| {
                        if row.user_id.is_none() {
                            row.user_id.clone_from(&job.user_id);
                        }
                        row
                    })
                    .collect();
                announce_fulfilled(self.ticks.as_ref(), fulfilled, crate::db::to_i64(at));
            }
            Err(error) => {
                tracing::warn!(%group, ?error, "drop import could not settle the request");
            }
        }
        let Some(record) = record else {
            return;
        };
        let mut askers: Vec<&str> = record
            .requesters
            .iter()
            .map(|requester| requester.user_id.as_str())
            .chain(record.user_id.as_deref())
            .filter(|user_id| !user_id.is_empty() && Some(*user_id) != job.user_id.as_deref())
            .collect();
        askers.sort_unstable();
        askers.dedup();
        for user_id in askers {
            self.ticks.announce(FlowEvent::Notify {
                user_id: user_id.to_owned(),
                notice: UserNotice::RequestImported(RequestImported {
                    event_id: new_event_id(),
                    release_group_mbid: group.clone(),
                    artist_name: release.artist_text(),
                    album_title: release.title.clone(),
                }),
            });
        }
    }

    /// Read the files off the async workers, never entering the library;
    /// with `beneath`, each beneath that real folder path.
    async fn probe(
        &self,
        library: &dyn LandingLibrary,
        paths: Vec<PathBuf>,
        beneath: Option<&Path>,
    ) -> Landing {
        let libraries = canonical_dirs(&library.library_dirs());
        let beneath = beneath.map(Path::to_path_buf);
        match tokio::task::spawn_blocking(move || {
            probe_beneath(&paths, &libraries, beneath.as_deref())
        })
        .await
        {
            Ok(landing) => landing,
            Err(error) => {
                tracing::error!(%error, "drop import probe did not run");
                Landing::default()
            }
        }
    }

    /// Settle an item that is processing; a lost race is logged.
    async fn save(&self, item: &ItemRecord) {
        match self.store.save_item(item, DropItemStatus::Processing).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(item_id = item.id, "drop item changed under the import"),
            Err(error) => tracing::warn!(item_id = item.id, %error, "drop item not saved"),
        }
    }

    /// Remove empty folders left in an upload's staging folder. The import
    /// folder's are its own pass's to remove.
    async fn tidy(&self, job: &JobRecord) {
        if job.source != DropJobSource::Upload {
            return;
        }
        let dir = PathBuf::from(&job.staging_dir);
        if let Err(error) = tokio::task::spawn_blocking(move || remove_empty_dirs(&dir)).await {
            tracing::warn!(%error, "drop staging tidy did not run");
        }
    }

    /// Tell the job's owner's open tabs. A job of no user has none to tell:
    /// the held list's triggers move the download revision instead.
    fn publish(&self, user_id: Option<&str>, job_id: &str) {
        let Some(user_id) = user_id else {
            return;
        };
        self.events.notify(
            user_id,
            UserNotice::DropImportUpdated(DropImportUpdated {
                event_id: new_event_id(),
                job_id: job_id.to_owned(),
            }),
        );
    }
}

/// How `files` pair with the album a match found, as a batch for the
/// intake, with the plan behind it. An automatic match imports the close
/// files and holds those whose tags name another track. A person's match
/// imports every paired file of a track the library lacks (their choice of
/// album is the authority), but a far file never stands in for an owned
/// track. With `extras`, files the release does not list go with the album
/// (bonus files stay beside it). `None` when nothing was found.
pub(crate) fn album_pairing(
    summary: &MatchSummary,
    files: usize,
    manual: bool,
    extras: bool,
) -> Option<(&FoundRelease, FilePlan, intake::Pairing)> {
    let found = summary.best()?;
    let release = &found.release;
    let target = Target {
        release_group_mbid: release.release_group_id.clone(),
        ..Target::default()
    };
    let file_plan = plan(summary, &target, files);
    let owned_track = |file: usize| {
        found
            .scored
            .pair_for(file)
            .is_some_and(|pair| summary.owns(&release.tracks[pair.track]))
    };
    let (import, held) = if manual {
        let import: Vec<(usize, usize)> = file_plan
            .import
            .iter()
            .chain(file_plan.held.iter())
            .filter(|(file, _)| !owned_track(*file))
            .copied()
            .collect();
        let held: Vec<(usize, usize)> = file_plan
            .held
            .iter()
            .filter(|(file, _)| owned_track(*file))
            .copied()
            .collect();
        (import, held)
    } else {
        (file_plan.import.clone(), file_plan.held.clone())
    };
    let pairing = intake::Pairing {
        import,
        owned: file_plan
            .owned
            .iter()
            .filter_map(|file| Some((*file, found.scored.pair_for(*file)?.track)))
            .collect(),
        held: held
            .into_iter()
            .map(|(file, track)| intake::Hold {
                file,
                track: Some(track),
                reason: Reason::TagMismatch,
                detail: String::new(),
            })
            .collect(),
        extras: if extras {
            file_plan.extra.clone()
        } else {
            Vec::new()
        },
    };
    Some((found, file_plan, pairing))
}

/// The `source` plugins hear an import of a job's files came from.
fn plugin_source(source: DropJobSource) -> &'static str {
    match source {
        DropJobSource::Upload => "drop_import",
        DropJobSource::ImportFolder => "import_folder",
    }
}

/// `path`'s place below `folder`, as stored.
fn rel_below(folder: &Path, path: &Path) -> String {
    path.strip_prefix(folder)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// An item settled with one reason: the sentence (plus specifics) as the
/// detail, the reason beside it.
fn settled(
    item: ItemRecord,
    status: DropItemStatus,
    reason: Reason,
    specifics: Option<String>,
) -> ItemRecord {
    let detail = match specifics {
        Some(more) => format!("{} {more}", reason.message()),
        None => reason.message().to_owned(),
    };
    ItemRecord {
        status,
        detail: Some(detail),
        reason_code: Some(reason),
        ..item
    }
}

/// Note ignored unreadable files next to a review reason.
fn with_unreadable(detail: Option<String>, unreadable: usize) -> Option<String> {
    if unreadable == 0 {
        return detail;
    }
    let noun = if unreadable == 1 {
        "file was"
    } else {
        "files were"
    };
    let note = format!("{unreadable} unreadable {noun} ignored.");
    Some(match detail {
        Some(detail) => format!("{detail} {note}"),
        None => note,
    })
}

/// The settled item's summary.
fn describe(done: &Counts, unreadable: usize, complete: bool) -> String {
    let plural = |count: usize, one: &str, many: &str| {
        if count == 1 {
            format!("{count} {one}")
        } else {
            format!("{count} {many}")
        }
    };
    let mut parts = Vec::new();
    if done.imported + done.bonus > 0 {
        parts.push(format!(
            "Imported {}",
            plural(done.imported, "file", "files")
        ));
    }
    if done.upgraded > 0 {
        parts.push(format!(
            "upgraded {}",
            plural(done.upgraded, "existing file", "existing files")
        ));
    }
    if done.bonus > 0 {
        parts.push(format!(
            "{} kept alongside the album",
            plural(done.bonus, "extra file", "extra files")
        ));
    }
    if done.owned > 0 {
        parts.push(format!(
            "{} already in your library",
            plural(done.owned, "track", "tracks")
        ));
    }
    if done.occupied > 0 {
        parts.push(format!(
            "{} skipped because a file is already in their place",
            plural(done.occupied, "file", "files")
        ));
    }
    if done.blocked > 0 {
        parts.push(format!(
            "{} kept because your copy couldn't be taken away",
            plural(done.blocked, "better file", "better files")
        ));
    }
    // Skipped and kept files wait in the held list too; each is counted
    // once, under why.
    let waiting = done
        .waiting
        .saturating_sub(done.occupied)
        .saturating_sub(done.blocked);
    if waiting > 0 {
        parts.push(format!(
            "{} left for review",
            plural(waiting, "file", "files")
        ));
    }
    if unreadable > 0 {
        parts.push(format!(
            "{} ignored",
            plural(unreadable, "unreadable file", "unreadable files")
        ));
    }
    if !complete && done.expected > 0 && done.covered < done.expected {
        parts.push(format!(
            "covers {} of {} album tracks",
            done.covered, done.expected
        ));
    }
    let mut text = parts.join(", ");
    if !text.is_empty() {
        text.push('.');
    }
    if done.tags_kept > 0 {
        text.push_str(" Tags kept as they came.");
    }
    text
}

/// Move uploads into a job folder as `NNN_<name>` (a copy when the
/// incoming file sits on another disk).
fn stage(dir: &Path, uploads: &[(String, PathBuf)]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (index, (name, from)) in uploads.iter().enumerate() {
        let to = dir.join(format!("{index:03}_{}", extract::safe_component(name)));
        if std::fs::rename(from, &to).is_err() {
            std::fs::copy(from, &to)?;
            std::fs::remove_file(from)?;
        }
    }
    Ok(())
}

/// Remove empty folders under `root` (deepest first), then `root` itself
/// when empty. Folders still holding review files stay.
fn remove_empty_dirs(root: &Path) {
    fn walk(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let path = entry.path();
                walk(&path);
                // Not empty means a review item still lives there.
                if std::fs::remove_dir(&path).is_err() {
                    tracing::debug!(dir = %path.display(), "drop folder kept: files remain");
                }
            }
        }
    }
    if !root.exists() {
        return;
    }
    walk(root);
    if std::fs::remove_dir(root).is_err() {
        tracing::debug!(dir = %root.display(), "drop staging folder kept: files remain");
    }
}
