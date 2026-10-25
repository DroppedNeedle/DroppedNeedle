//! Drop import: music a person bought elsewhere, uploaded straight into
//! the library.
//!
//! An upload (loose files, folders, zips) becomes one job. The job is
//! unpacked and split into album-shaped items ([`extract`]), each item is
//! identified the way the scanner would ([`identify`]), and the files that
//! match go into the library through the same staged publisher a finished
//! download uses: an upload better than the library's copy replaces it
//! (the old file goes to the recycle bin), and files the release does
//! not list go alongside the album. Items nothing could identify wait as `needs_review`
//! until a person picks the album (match) or throws them away (discard).
//!
//! Every item that stops short carries one reason code, one plain
//! sentence and one action ([`reasons`]). A fully imported album resolves
//! its open request and wanted watch and tells everyone who asked for it.
//! Progress reaches the person's open tabs as `drop_import_updated`.
//!
//! Jobs and items live in SQLite ([`store`]); a restart fails the jobs it
//! cut off (their staged files stay for review or cleanup) instead of
//! importing anything twice.

pub mod extract;
pub mod http;
pub mod identify;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::identify::Outcome;
use self::store::{DropStore, ItemRecord, JobRecord, item_status, job_status};
use crate::flows::seams::{FlowEvent, TickSink};
use crate::landing::canonical_dirs;
use crate::landing::matching::{MatchSummary, plan};
use crate::landing::ports::{
    ExtraFile, ImportFailure, ImportFile, ImportOrigin, ImportRequest, LandingLibrary,
};
use crate::landing::probe::{Landing, probe};
use crate::landing::specs::Target;
use crate::landing::{quality, specs};
use crate::reason::Reason;
use crate::requests::models::RequestKind;
use crate::requests::settle::announce_fulfilled;
use crate::requests::sqlite::RequestStore;
use droppedneedle_kernel::events::{
    DropImportUpdated, EventHub, RequestImported, UserNotice, new_event_id,
};

/// Most files one upload may carry (v2 `_MAX_FILES_PER_DROP`).
pub const MAX_FILES_PER_DROP: usize = 25;
/// More audio than this in one folder is not one album.
const MAX_FILES_PER_UNIT: usize = 60;
/// Review items older than this are pruned with their files at startup.
const REVIEW_RETENTION_SECS: f64 = 90.0 * 86_400.0;
/// Jobs listed per page (v2 `list_jobs` limit).
const LIST_LIMIT: i64 = 50;
/// Where uploads stream before a job exists.
const INCOMING_DIR: &str = "_incoming";

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
}

impl From<store::StoreError> for DropError {
    fn from(error: store::StoreError) -> Self {
        Self::Unavailable(error.0)
    }
}

/// What organising one item did.
#[derive(Debug, Default)]
struct Organised {
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
    /// Files that wait for review, grouped by why.
    leftovers: Vec<(Reason, Vec<PathBuf>)>,
    /// Release tracks now in the library, out of the edition's tracks.
    covered: usize,
    expected: usize,
}

/// The drop-import service.
pub struct DropImports {
    store: DropStore,
    library: Option<Arc<dyn LandingLibrary>>,
    root: PathBuf,
    events: EventHub,
    ticks: Arc<dyn TickSink>,
    ledger: RequestStore,
}

impl DropImports {
    /// Service over its store, the library port, a staging folder of its
    /// own, the event sink, and the request stores it resolves.
    pub fn new(
        store: DropStore,
        library: Option<Arc<dyn LandingLibrary>>,
        root: PathBuf,
        events: EventHub,
        ticks: Arc<dyn TickSink>,
        ledger: RequestStore,
    ) -> Self {
        Self {
            store,
            library,
            root,
            events,
            ticks,
            ledger,
        }
    }

    /// Where uploads stream before a job exists: the same disk as the job
    /// folders, so adopting them is a rename.
    pub fn incoming_dir(&self) -> PathBuf {
        self.root.join(INCOMING_DIR)
    }

    fn library(&self) -> Result<Arc<dyn LandingLibrary>, DropError> {
        self.library.clone().ok_or_else(|| {
            DropError::Invalid("The library isn't ready yet. Try again shortly.".into())
        })
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
        if self.library()?.library_dirs().is_empty() {
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
        self.publish(user_id, &job_id);
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

    /// One job, when the person may see it.
    pub async fn get_job(&self, viewer: &Viewer, job_id: &str) -> Result<JobRecord, DropError> {
        match self.store.get_job(job_id).await? {
            Some(job) if viewer.admin || job.user_id == viewer.user_id => Ok(job),
            _ => Err(DropError::NotFound),
        }
    }

    /// An item and its job, when the person may act on it.
    async fn owned_item(
        &self,
        viewer: &Viewer,
        item_id: i64,
    ) -> Result<(ItemRecord, JobRecord), DropError> {
        let item = self
            .store
            .get_item(item_id)
            .await?
            .ok_or(DropError::NotFound)?;
        let job = self.get_job(viewer, &item.job_id).await?;
        Ok((item, job))
    }

    /// Import a review item as the album the person picked. Their choice
    /// is the authority: the files are paired with that album's tracks
    /// without the closeness check.
    pub async fn match_item(
        self: &Arc<Self>,
        viewer: &Viewer,
        item_id: i64,
        release_group_mbid: &str,
    ) -> Result<ItemRecord, DropError> {
        let (item, job) = self.owned_item(viewer, item_id).await?;
        if item.status != item_status::NEEDS_REVIEW {
            return Err(DropError::Conflict(
                "Only items awaiting review can be matched.".into(),
            ));
        }
        let group = release_group_mbid.trim();
        if group.is_empty() {
            return Err(DropError::Invalid("Pick an album to match.".into()));
        }
        let library = self.library()?;
        let paths: Vec<PathBuf> = item
            .staging_paths
            .iter()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .collect();
        if paths.is_empty() {
            return Err(DropError::Invalid(
                "The uploaded files are no longer on disk. Drop them in again.".into(),
            ));
        }
        if !self.store.claim_review(item.id).await? {
            return Err(DropError::Conflict(
                "This item is already being handled.".into(),
            ));
        }
        let landing = self.probe(library.as_ref(), paths).await;
        let outcome = identify::assigned(library.as_ref(), &landing, group).await;
        let summary = match outcome {
            Outcome::Matched(summary) => summary,
            other => {
                let reason = match other {
                    Outcome::Unavailable(detail) => {
                        tracing::warn!(item_id, %detail, "drop match: MusicBrainz unavailable");
                        Reason::MusicbrainzUnavailable
                    }
                    Outcome::Review { reason, .. } => reason,
                    Outcome::Matched(_) => Reason::ImportFailed,
                };
                let message = format!("{} {}", reason.message(), reason.action());
                let mut back = item.clone();
                back.status = item_status::NEEDS_REVIEW.to_owned();
                self.store.save_item(&back, item_status::PROCESSING).await?;
                return Err(DropError::Invalid(message));
            }
        };
        let mut working = item.clone();
        working.status = item_status::PROCESSING.to_owned();
        self.organise(&job, working, library.as_ref(), &landing, &summary, true)
            .await?;
        self.tidy(&job).await;
        self.publish(&job.user_id, &job.id);
        self.store
            .get_item(item.id)
            .await?
            .ok_or(DropError::NotFound)
    }

    /// Throw a review item's files away.
    pub async fn discard_item(
        &self,
        viewer: &Viewer,
        item_id: i64,
    ) -> Result<ItemRecord, DropError> {
        let (item, job) = self.owned_item(viewer, item_id).await?;
        if item.status != item_status::NEEDS_REVIEW {
            return Err(DropError::Conflict(
                "Only items awaiting review can be discarded.".into(),
            ));
        }
        let mut discarded = item.clone();
        discarded.status = item_status::DISCARDED.to_owned();
        discarded.staging_paths = Vec::new();
        discarded.detail = Some("Discarded.".to_owned());
        discarded.reason_code = None;
        if !self
            .store
            .save_item(&discarded, item_status::NEEDS_REVIEW)
            .await?
        {
            return Err(DropError::Conflict(
                "This item is already being handled.".into(),
            ));
        }
        remove_files(item.staging_paths.iter().map(PathBuf::from).collect()).await;
        self.tidy(&job).await;
        self.publish(&job.user_id, &job.id);
        self.store
            .get_item(item.id)
            .await?
            .ok_or(DropError::NotFound)
    }

    /// Startup housekeeping: jobs a restart cut off fail (a hand match
    /// cut off goes back to review), review items past the retention age
    /// go with their files, and staging folders with nothing left to
    /// review are removed, orphans and partial uploads included. When a
    /// cut-off upload cannot be read or kept, the sweep stops there: its
    /// job stays processing with its files, for a later sweep to group.
    pub async fn sweep_stale(&self) {
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
                    for unit in units {
                        let paths: Vec<String> = unit
                            .files
                            .iter()
                            .map(|path| path.to_string_lossy().into_owned())
                            .collect();
                        if let Err(error) = self
                            .store
                            .add_item(&job_id, &unit.folder_name, &paths)
                            .await
                        {
                            tracing::warn!(%job_id, %error, "cut-off drop upload not kept; staging left as is");
                            return;
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cut-off drop jobs unreadable; staging left as is");
                return;
            }
        }
        match self.store.fail_stale_processing(Reason::Interrupted).await {
            Ok(0) => {}
            Ok(failed) => tracing::info!(jobs = failed, "drop imports cut off by a restart failed"),
            Err(error) => tracing::warn!(%error, "drop import restart sweep failed"),
        }
        match self
            .store
            .prune_stale_review_items(self.store.db().now_unix_f64() - REVIEW_RETENTION_SECS)
            .await
        {
            Ok(paths) if !paths.is_empty() => {
                tracing::info!(files = paths.len(), "old drop-import review items pruned");
                remove_files(paths.into_iter().map(PathBuf::from).collect()).await;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "drop import review pruning failed"),
        }
        // Every folder under the drop staging root goes, except those of
        // jobs that still hold files for review. That covers finished
        // jobs, orphaned folders no job names, and half-written uploads.
        let keep: HashSet<std::ffi::OsString> = match self.store.staging_dirs().await {
            Ok(dirs) => dirs
                .into_iter()
                .filter(|(_, keep)| *keep)
                .filter_map(|(dir, _)| PathBuf::from(dir).file_name().map(ToOwned::to_owned))
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "drop import jobs unreadable; staging left as is");
                return;
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
            Ok((status, items)) => {
                tracing::info!(job_id, status, items, "drop import job finished");
                return;
            }
            Err(cause) => cause,
        };
        tracing::error!(job_id, %cause, "drop import job failed");
        if let Err(error) = self
            .store
            .set_job_status(
                job_id,
                job_status::FAILED,
                Some("The import didn't finish. Check the server log.".to_owned()),
            )
            .await
        {
            tracing::warn!(job_id, %error, "drop import failure not recorded");
        }
        self.publish(&job.user_id, job_id);
    }

    /// Answers the job's final status and how many items it made.
    async fn process_job(&self, job: &JobRecord) -> Result<(&'static str, usize), String> {
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
                .set_job_status(&job.id, job_status::FAILED, Some(error))
                .await
                .map_err(|error| error.0)?;
            self.publish(&job.user_id, &job.id);
            return Ok((job_status::FAILED, 0));
        }
        let mut items = Vec::with_capacity(units.len());
        for unit in units {
            let paths: Vec<String> = unit
                .files
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            let id = self
                .store
                .add_item(&job.id, &unit.folder_name, &paths)
                .await
                .map_err(|error| error.0)?;
            items.push((id, unit));
        }
        self.publish(&job.user_id, &job.id);
        let count = items.len();
        for (id, unit) in items {
            if let Err(cause) = self
                .process_item(job, id, &unit.folder_name, unit.files)
                .await
            {
                tracing::error!(item_id = id, %cause, "drop import item failed");
                if let Ok(Some(item)) = self.store.get_item(id).await {
                    let failed = settled(item, item_status::FAILED, Reason::ImportFailed, None);
                    if let Err(error) = self.store.save_item(&failed, item_status::PROCESSING).await
                    {
                        tracing::warn!(item_id = id, %error, "drop item failure not recorded");
                    }
                }
            }
            self.publish(&job.user_id, &job.id);
        }
        // A damaged archive next to good ones is still reported: the job
        // completes and the notes say what was skipped.
        let note = (!notes.is_empty()).then(|| notes.join(" "));
        self.store
            .set_job_status(&job.id, job_status::COMPLETED, note)
            .await
            .map_err(|error| error.0)?;
        self.tidy(job).await;
        self.publish(&job.user_id, &job.id);
        Ok((job_status::COMPLETED, count))
    }

    async fn process_item(
        &self,
        job: &JobRecord,
        item_id: i64,
        folder: &str,
        paths: Vec<PathBuf>,
    ) -> Result<(), String> {
        let item = self
            .store
            .get_item(item_id)
            .await
            .map_err(|error| error.0)?
            .ok_or("item vanished")?;
        let library = self
            .library()
            .map_err(|_| "library port not attached".to_owned())?;
        let landing = self.probe(library.as_ref(), paths).await;
        let unreadable = landing.unreadable.len();
        if landing.audio.is_empty() {
            let mut failed = settled(item, item_status::FAILED, Reason::Corrupt, None);
            failed.staging_paths = Vec::new();
            self.save(&failed).await;
            return Ok(());
        }
        if landing.audio.len() > MAX_FILES_PER_UNIT {
            let specifics = format!("It has {} audio files.", landing.audio.len());
            let review = settled(
                item,
                item_status::NEEDS_REVIEW,
                Reason::TooManyFiles,
                Some(specifics),
            );
            self.save(&review).await;
            return Ok(());
        }
        match identify::identify(library.as_ref(), &landing, folder).await {
            Outcome::Matched(summary) => {
                self.organise(job, item, library.as_ref(), &landing, &summary, false)
                    .await
                    .map_err(|error| format!("{error:?}"))?;
            }
            Outcome::Review { reason, detail } => {
                let detail = with_unreadable(detail, unreadable);
                self.save(&settled(item, item_status::NEEDS_REVIEW, reason, detail))
                    .await;
            }
            Outcome::Unavailable(cause) => {
                tracing::warn!(item_id, %cause, "drop import: MusicBrainz unavailable");
                self.save(&settled(
                    item,
                    item_status::NEEDS_REVIEW,
                    Reason::MusicbrainzUnavailable,
                    None,
                ))
                .await;
            }
        }
        Ok(())
    }

    /// Put an identified item's files in the library, settle the item,
    /// and send whatever did not import to review as a new item.
    async fn organise(
        &self,
        job: &JobRecord,
        mut item: ItemRecord,
        library: &dyn LandingLibrary,
        landing: &Landing,
        summary: &MatchSummary,
        manual: bool,
    ) -> Result<(), DropError> {
        let Some(found) = summary.best() else {
            self.save(&settled(
                item,
                item_status::NEEDS_REVIEW,
                Reason::NoAlbumMatch,
                None,
            ))
            .await;
            return Ok(());
        };
        let release = &found.release;
        let target = Target {
            release_group_mbid: release.release_group_id.clone(),
            ..Target::default()
        };
        let file_plan = plan(summary, &target, landing.audio.len());
        // An automatic match imports the close files and holds those whose
        // tags name another track. A person's match imports every paired
        // file of a track the library lacks (their choice of album is the
        // authority), but a far file never stands in for an owned track.
        let owned_track = |file: usize| {
            found
                .scored
                .pair_for(file)
                .is_some_and(|pair| summary.owns(&release.tracks[pair.track]))
        };
        let (mut import, held): (Vec<(usize, usize)>, Vec<usize>) = if manual {
            let import = file_plan
                .import
                .iter()
                .chain(file_plan.held.iter())
                .filter(|(file, _)| !owned_track(*file))
                .copied()
                .collect();
            let held = file_plan
                .held
                .iter()
                .map(|(file, _)| *file)
                .filter(|file| owned_track(*file))
                .collect();
            (import, held)
        } else {
            (
                file_plan.import.clone(),
                file_plan.held.iter().map(|(file, _)| *file).collect(),
            )
        };

        // A file strictly better than every library copy of its track may
        // replace them (v2: the old file goes to the recycle bin). Only
        // files close to the track are candidates (`plan` holds far ones),
        // equal or worse is skipped.
        let mut owned_files: Vec<usize> = Vec::new();
        let mut replaces: HashMap<usize, (usize, Vec<String>)> = HashMap::new();
        for file in &file_plan.owned {
            let Some(track) = found.scored.pair_for(*file).map(|pair| pair.track) else {
                owned_files.push(*file);
                continue;
            };
            let release_track = &release.tracks[track];
            let copies = library
                .owned_copies(
                    &release.release_group_id,
                    None,
                    &release.id,
                    &release_track.id,
                    &release_track.recording_id,
                )
                .await;
            let tier = landing.audio[*file].tier();
            if !copies.is_empty() && copies.iter().all(|copy| quality::beats(tier, copy.tier)) {
                replaces.insert(
                    *file,
                    (
                        track,
                        copies.into_iter().map(|copy| copy.track_id).collect(),
                    ),
                );
            } else {
                owned_files.push(*file);
            }
        }

        // AcoustID, when a key is set, checks what is about to go in, as
        // the download landing does: a file whose audio is another
        // recording goes to review. An upgrade from a person's match must
        // be confirmed by the fingerprint, since no closeness gate vetted
        // the album; unconfirmed, the library copy stays.
        let mut checked: Vec<(usize, usize)> = replaces
            .iter()
            .map(|(file, (track, _))| (*file, *track))
            .collect();
        if !manual {
            checked.extend(import.iter().copied());
        }
        let heard = if checked.is_empty() {
            HashMap::new()
        } else {
            library
                .fingerprints(
                    checked
                        .iter()
                        .map(|(file, _)| (file.to_string(), landing.audio[*file].path.clone()))
                        .collect(),
                )
                .await
        };
        let mut misheard: Vec<usize> = Vec::new();
        for (file, track) in checked {
            let release_track = &release.tracks[track];
            let recordings = heard
                .get(&file.to_string())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let disagrees = specs::fingerprint_disagrees(
                recordings,
                &release_track.recording_id,
                landing.audio[file].header.duration_seconds,
                release_track.length_ms.map(|ms| ms as f64 / 1000.0),
            );
            let confirmed = recordings
                .iter()
                .any(|heard| heard.eq_ignore_ascii_case(&release_track.recording_id));
            if disagrees {
                replaces.remove(&file);
                import.retain(|(kept, _)| *kept != file);
                misheard.push(file);
            } else if manual && !confirmed && replaces.remove(&file).is_some() {
                tracing::info!(
                    item_id = item.id,
                    file,
                    "upgrade from a hand match not confirmed by AcoustID; library copy kept"
                );
                owned_files.push(file);
            }
        }

        // Each upgrade's old copies go in the publish that places it: the
        // library takes them to the recycle bin only once the upload is
        // in, and an upload it does not take keeps them where they are.
        let mut upgrades: Vec<(usize, usize, Vec<String>)> = replaces
            .into_iter()
            .map(|(file, (track, old))| (file, track, old))
            .collect();
        upgrades.sort_by_key(|(file, _, _)| *file);
        let mut replaced: HashMap<usize, Vec<String>> = HashMap::new();
        for (file, track, old) in upgrades {
            replaced.insert(file, old);
            import.push((file, track));
        }
        let import = import;
        // Files the release does not account for go with the album (v2
        // kept bonus files alongside it).
        let extras: Vec<usize> = file_plan.extra.clone();

        let mut done = Organised {
            owned: owned_files.len(),
            expected: release.tracks.len(),
            ..Organised::default()
        };
        let mut imported_tracks: HashSet<usize> = HashSet::new();
        let mut consumed: Vec<PathBuf> = owned_files
            .iter()
            .map(|index| landing.audio[*index].path.clone())
            .collect();
        // Uploads whose place in the library is taken, and upgrades whose
        // old copy could not be taken away, are kept for review, never
        // deleted.
        let mut occupied: Vec<PathBuf> = Vec::new();
        let mut blocked: Vec<PathBuf> = Vec::new();
        if !import.is_empty() || !extras.is_empty() {
            let request = ImportRequest {
                task_id: format!("drop-{}", item.id),
                origin: ImportOrigin::Drop,
                staging: None,
                release: release.clone(),
                join: None,
                files: import
                    .iter()
                    .map(|(file, track)| ImportFile {
                        path: landing.audio[*file].path.clone(),
                        track: *track,
                        replaces: replaced.get(file).cloned().unwrap_or_default(),
                    })
                    .collect(),
                actor: job.user_id.clone(),
                extras: extras
                    .iter()
                    .map(|index| extra_file(&landing.audio[*index]))
                    .collect(),
            };
            match library.import(request).await {
                Ok(receipt) => {
                    done.tags_kept = receipt.tags_kept;
                    let skipped: HashSet<usize> =
                        receipt.skipped.iter().map(|(index, _)| *index).collect();
                    let refused: HashMap<usize, &str> = receipt
                        .blocked
                        .iter()
                        .map(|(index, detail)| (*index, detail.as_str()))
                        .collect();
                    for (index, (file, track)) in import.iter().enumerate() {
                        let path = landing.audio[*file].path.clone();
                        if let Some(detail) = refused.get(&index) {
                            tracing::warn!(
                                item_id = item.id,
                                detail,
                                "library copy not taken away; the upgrade waits for review"
                            );
                            done.blocked += 1;
                            blocked.push(path);
                            continue;
                        }
                        if !skipped.contains(&index) {
                            done.imported += 1;
                            done.upgraded += usize::from(replaced.contains_key(file));
                            imported_tracks.insert(*track);
                            consumed.push(path);
                            continue;
                        }
                        done.occupied += 1;
                        occupied.push(path);
                    }
                    for (offset, file) in extras.iter().enumerate() {
                        let path = landing.audio[*file].path.clone();
                        if skipped.contains(&(import.len() + offset)) {
                            done.occupied += 1;
                            occupied.push(path);
                        } else {
                            done.bonus += 1;
                            consumed.push(path);
                        }
                    }
                }
                Err(failure) => match &failure {
                    ImportFailure::Occupied(detail) => {
                        tracing::info!(item_id = item.id, %detail, "drop import destination taken");
                        done.occupied = import.len() + extras.len();
                        occupied.extend(
                            import
                                .iter()
                                .map(|(file, _)| *file)
                                .chain(extras.iter().copied())
                                .map(|file| landing.audio[file].path.clone()),
                        );
                    }
                    ImportFailure::LocalFault(detail) | ImportFailure::NamingScript(detail) => {
                        tracing::warn!(item_id = item.id, %detail, "library could not take a drop import");
                        let reason = if matches!(failure, ImportFailure::NamingScript(_)) {
                            Reason::NamingScriptFailed
                        } else {
                            Reason::LibraryUnavailable
                        };
                        self.save(&settled(item, item_status::NEEDS_REVIEW, reason, None))
                            .await;
                        return Ok(());
                    }
                },
            }
        }
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
        let paths = |files: &[usize]| -> Vec<PathBuf> {
            files
                .iter()
                .map(|index| landing.audio[*index].path.clone())
                .collect()
        };
        for (reason, files) in [
            (Reason::TagMismatch, paths(&held)),
            (Reason::FingerprintMismatch, paths(&misheard)),
            (Reason::Samples, landing.samples.clone()),
            (Reason::UpgradeBlocked, blocked),
            (Reason::TargetOccupied, occupied),
        ] {
            let files: Vec<PathBuf> = files
                .into_iter()
                .filter(|path| !consumed.contains(path))
                .collect();
            if !files.is_empty() {
                done.leftovers.push((reason, files));
            }
        }

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
        item.files_imported = i64::try_from(placed).unwrap_or(i64::MAX);
        item.staging_paths = Vec::new();
        let summary_text = describe(&done, unreadable, fulfills);
        let settled_item = if placed > 0 {
            ItemRecord {
                status: item_status::IMPORTED.to_owned(),
                detail: Some(summary_text),
                reason_code: None,
                ..item
            }
        } else if done.occupied > 0 || done.blocked > 0 || done.owned > 0 {
            let reason = if done.occupied > 0 {
                Reason::TargetOccupied
            } else if done.blocked > 0 {
                Reason::UpgradeBlocked
            } else {
                Reason::AlreadyOwned
            };
            ItemRecord {
                status: item_status::SKIPPED.to_owned(),
                detail: Some(format!("{} {summary_text}", reason.message())),
                reason_code: Some(reason),
                ..item
            }
        } else {
            let mut review = settled(item, item_status::NEEDS_REVIEW, Reason::NoAlbumMatch, None);
            review.staging_paths = landing
                .audio
                .iter()
                .map(|file| file.path.to_string_lossy().into_owned())
                .collect();
            self.save(&review).await;
            return Ok(());
        };
        self.save(&settled_item).await;
        for (reason, files) in &done.leftovers {
            self.review_leftovers(job, &settled_item, files, *reason)
                .await;
        }
        remove_files(consumed).await;
        if placed > 0 {
            self.after_import(job, release, placed, fulfills).await;
        }
        Ok(())
    }

    /// Files of an imported folder that did not import wait for a person
    /// as their own review item, one item per reason.
    async fn review_leftovers(
        &self,
        job: &JobRecord,
        parent: &ItemRecord,
        files: &[PathBuf],
        reason: Reason,
    ) {
        let paths: Vec<String> = files
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let name = format!("{} (left over)", parent.folder_name);
        let id = match self.store.add_item(&job.id, &name, &paths).await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(%error, "left-over drop files not queued for review");
                return;
            }
        };
        let item = ItemRecord {
            id,
            job_id: job.id.clone(),
            folder_name: name,
            status: item_status::PROCESSING.to_owned(),
            updated_at: self.store.db().now_unix_f64(),
            release_group_mbid: None,
            album_title: None,
            artist_name: None,
            files_total: i64::try_from(paths.len()).unwrap_or(i64::MAX),
            files_imported: 0,
            detail: None,
            reason_code: None,
            staging_paths: paths,
        };
        let album = parent.album_title.clone().unwrap_or_default();
        let detail = (!album.is_empty()).then(|| format!("They came with {album}."));
        self.save(&settled(item, item_status::NEEDS_REVIEW, reason, detail))
            .await;
    }

    /// After files reached the library: tell plugins, and when the album
    /// is now complete, resolve its request and wanted watch and tell
    /// everyone else who asked for it (v2 `_after_import`).
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
            source: "drop_import".to_owned(),
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
        match self.ledger.settle_in_library(&group, "imported", at).await {
            Ok(fulfilled) => {
                // A request with no owner left is credited to the uploader.
                let fulfilled = fulfilled
                    .into_iter()
                    .map(|mut row| {
                        row.user_id.get_or_insert_with(|| job.user_id.clone());
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
            .filter(|user_id| !user_id.is_empty() && *user_id != job.user_id)
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

    /// Read the files off the async workers, never entering the library.
    async fn probe(&self, library: &dyn LandingLibrary, paths: Vec<PathBuf>) -> Landing {
        let libraries = canonical_dirs(&library.library_dirs());
        match tokio::task::spawn_blocking(move || probe(&paths, &libraries)).await {
            Ok(landing) => landing,
            Err(error) => {
                tracing::error!(%error, "drop import probe did not run");
                Landing::default()
            }
        }
    }

    /// Settle an item that is processing; a lost race is logged.
    async fn save(&self, item: &ItemRecord) {
        match self.store.save_item(item, item_status::PROCESSING).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(item_id = item.id, "drop item changed under the import"),
            Err(error) => tracing::warn!(item_id = item.id, %error, "drop item not saved"),
        }
    }

    /// Remove empty folders left in a job's staging folder.
    async fn tidy(&self, job: &JobRecord) {
        let dir = PathBuf::from(&job.staging_dir);
        if let Err(error) = tokio::task::spawn_blocking(move || remove_empty_dirs(&dir)).await {
            tracing::warn!(%error, "drop staging tidy did not run");
        }
    }

    fn publish(&self, user_id: &str, job_id: &str) {
        self.events.notify(
            user_id,
            UserNotice::DropImportUpdated(DropImportUpdated {
                event_id: new_event_id(),
                job_id: job_id.to_owned(),
            }),
        );
    }
}

/// An item settled with one reason: the sentence (plus specifics) as the
/// detail, the reason beside it.
fn settled(
    item: ItemRecord,
    status: &str,
    reason: Reason,
    specifics: Option<String>,
) -> ItemRecord {
    let detail = match specifics {
        Some(more) => format!("{} {more}", reason.message()),
        None => reason.message().to_owned(),
    };
    ItemRecord {
        status: status.to_owned(),
        detail: Some(detail),
        reason_code: Some(reason),
        ..item
    }
}

/// An extra file as the album will carry it: its own title (else the
/// file name) and its own disc and track numbers.
fn extra_file(file: &crate::landing::probe::LandedFile) -> ExtraFile {
    let title = file.tag.title.trim();
    ExtraFile {
        path: file.path.clone(),
        title: if title.is_empty() {
            file.path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            title.to_owned()
        },
        disc: file.tag.disc_number.max(1),
        position: file.tag.track_number,
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

/// The settled item's summary (v2 `_finish_item` wording).
fn describe(done: &Organised, unreadable: usize, complete: bool) -> String {
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
            "{} kept because your copy couldn't go to the recycle bin",
            plural(done.blocked, "better file", "better files")
        ));
    }
    let waiting: usize = done.leftovers.iter().map(|(_, files)| files.len()).sum();
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

/// Remove files off the async workers, logging any that stay.
async fn remove_files(paths: Vec<PathBuf>) {
    let done = tokio::task::spawn_blocking(move || {
        for path in paths {
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), %error, "staged drop file not removed");
            }
        }
    })
    .await;
    if let Err(error) = done {
        tracing::warn!(%error, "staged drop file removal did not run");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reads_like_v2() {
        let done = Organised {
            imported: 9,
            owned: 1,
            leftovers: vec![(Reason::TagMismatch, vec![PathBuf::from("x.flac")])],
            covered: 10,
            expected: 12,
            ..Organised::default()
        };
        assert_eq!(
            describe(&done, 0, false),
            "Imported 9 files, 1 track already in your library, 1 file left for review, \
             covers 10 of 12 album tracks."
        );
    }
}
