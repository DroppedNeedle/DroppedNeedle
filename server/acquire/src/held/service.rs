//! What a person can do with an entry of the held list, and the loop that
//! keeps the list tidy.
//!
//! Import places files as a person's choice through the one intake, with
//! the plan of the download they came from: the check that held a file is
//! skipped, the comparison with your copy still runs, and a file no better
//! than it stays held until "Use this file" says to replace yours. Match
//! places them on an album a curator picked. Re-check asks AcoustID again.
//! Discard deletes the held copies. Each acts on the whole entry or the
//! files named, one batch per download they came from, and claims the
//! entry first (`held` to `placing`), so a second action, or the prune,
//! never runs on it at once. The loop also presses Retry by itself on
//! files our side could not take, at growing gaps.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickCtx};
use droppedneedle_providers::RequestPriority;

use super::HeldFolders;
use super::store::{Entry, EntryOrigin, FileState, HeldFile, HeldRows, Pairing};
use crate::db::AcquireDb;
use crate::downloads::journal::Journal;
use crate::downloads::plan::PlannedAlbum;
use crate::downloads::queue_rows::Viewer;
use crate::downloads::state::TaskStatus;
use crate::downloads::store::{DownloadStore, TaskRow};
use crate::downloads::worker::DownloadWorker;
use crate::drop_import::{DropError, DropImports, identify};
use crate::intake::{self, Gate, IntakeBatch, IntakeFile, Origin, Outcome};
use crate::landing::canonical_dirs;
use crate::landing::ports::LandingLibrary;
use crate::landing::probe::{self, LandedFile, Landing};
use crate::landing::specs;
use crate::reason::{Reason, ReasonKind};
use droppedneedle_library::matching::Release;

/// Job name of the loop that removes expired entries' copies, finishes
/// actions a restart cut off and retries files our side could not take.
pub const PRUNE_JOB: &str = "acquire-held-prune";

/// Fingerprint checks one bulk re-check runs at most. Skipped files do
/// not count.
pub const BULK_RECHECK_LIMIT: usize = 25;

/// Entries one prune pass expires at most; the rest wait for the next.
const PRUNE_BATCH: i64 = 200;

/// Entries one pass retries at most; the rest wait for the next.
const RETRY_BATCH: i64 = 20;

/// Why an action on the held list did not happen. The sentences are shown
/// as they are.
#[derive(Debug, thiserror::Error)]
pub enum HeldError {
    /// No such entry or file the caller may see.
    #[error("{0}")]
    NotFound(&'static str),
    /// The action cannot run now; the sentence says what to do.
    #[error("{0}")]
    Refused(String),
    /// Another action runs on the entry now.
    #[error("This entry is already being handled. Wait a moment and refresh.")]
    InUse,
    /// No library folder takes imports yet.
    #[error("Imports are not set up yet. Add a library folder in settings, then try again.")]
    NotSetUp,
    /// The request asked for something that cannot be: the sentence says
    /// what.
    #[error("{0}")]
    Invalid(String),
    /// The database was busy; nothing was changed and the action can run
    /// again.
    #[error("held list busy: {0}")]
    Busy(String),
    /// The store or the library could not be read or written.
    #[error("held list unavailable: {0}")]
    Unavailable(String),
}

impl From<DbError> for HeldError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { operation } => Self::Busy(operation),
            DbError::LaneClosed => Self::Busy("lane closed".to_owned()),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for HeldError {
    fn from(error: rusqlite::Error) -> Self {
        Self::from(DbError::from(error))
    }
}

impl From<crate::downloads::store::StoreError> for HeldError {
    fn from(error: crate::downloads::store::StoreError) -> Self {
        match error {
            crate::downloads::store::StoreError::Db(error) => Self::from(error),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

impl From<DropError> for HeldError {
    fn from(error: DropError) -> Self {
        match error {
            DropError::Invalid(message) => Self::Invalid(message),
            DropError::NotFound => Self::NotFound(GONE),
            DropError::Conflict(message) => Self::Refused(message),
            DropError::Unavailable(cause) => Self::Unavailable(cause),
        }
    }
}

const GONE: &str = "That held entry was not found. It may have been handled already.";

/// What an action did with the files it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Acted {
    /// Files now in the library.
    pub imported: usize,
    /// Files the library already held, so nothing was added.
    pub owned: usize,
    /// Files deleted.
    pub discarded: usize,
    /// Files still held, with why.
    pub held: Vec<Reason>,
}

/// One file of a bulk re-check.
#[derive(Debug, Clone)]
pub struct Rechecked {
    pub entry_id: i64,
    pub file_id: i64,
    pub outcome: Result<bool, String>,
}

/// The held list's actions and its prune.
pub struct HeldService {
    db: AcquireDb,
    journal: Arc<Journal>,
    worker: Arc<DownloadWorker>,
    drops: Arc<DropImports>,
    folders: HeldFolders,
    /// Entries a restart found claimed: the action on them stopped part
    /// way, and the prune finishes them once the library's writes settle.
    cut_off: Mutex<Vec<i64>>,
}

impl HeldService {
    pub fn new(
        journal: Arc<Journal>,
        worker: Arc<DownloadWorker>,
        drops: Arc<DropImports>,
        folders: HeldFolders,
    ) -> Self {
        Self {
            db: journal.db().clone(),
            journal,
            worker,
            drops,
            folders,
            cut_off: Mutex::new(Vec::new()),
        }
    }

    /// The two folders held copies live in.
    pub fn folders(&self) -> &HeldFolders {
        &self.folders
    }

    fn now(&self) -> i64 {
        self.db.clock().now_ms()
    }

    async fn read<R, F>(&self, name: &'static str, op: F) -> Result<R, HeldError>
    where
        R: Send + 'static,
        F: FnOnce(&HeldRows<'_>) -> rusqlite::Result<R> + Send + 'static,
    {
        self.db
            .db()
            .read(name, move |conn| {
                Ok::<_, HeldError>(op(&HeldRows::new(conn))?)
            })
            .await
    }

    async fn write<R, F>(&self, name: &'static str, op: F) -> Result<R, HeldError>
    where
        R: Send + 'static,
        F: FnOnce(&HeldRows<'_>, &DownloadStore<'_>) -> Result<R, HeldError> + Send + 'static,
    {
        self.db
            .db()
            .write(Lane::Foreground, name, move |tx| {
                op(&HeldRows::new(tx), &DownloadStore::new(tx))
            })
            .await
    }

    fn library(&self) -> Result<Arc<dyn LandingLibrary>, HeldError> {
        self.worker
            .landing()
            .and_then(|landing| landing.library().cloned())
            .ok_or(HeldError::NotSetUp)
    }

    /// Open entries with their held files, newest first: the viewer's own,
    /// or everyone's for an admin who asks for `all`.
    pub async fn list(
        &self,
        viewer: &Viewer,
        release_group_mbid: Option<String>,
        task_id: Option<String>,
        all: bool,
    ) -> Result<Vec<Entry>, HeldError> {
        let owner = (!(all && viewer.admin)).then(|| viewer.user_id.clone());
        self.read("held.list", move |rows| {
            rows.list(
                owner.as_deref(),
                release_group_mbid.as_deref(),
                task_id.as_deref(),
            )
        })
        .await
    }

    /// One open entry the viewer may act on. Someone else's answers not
    /// found.
    pub async fn get(&self, viewer: &Viewer, entry_id: i64) -> Result<Entry, HeldError> {
        self.read("held.entry", move |rows| rows.entry(entry_id))
            .await?
            .filter(|entry| entry.state.is_open() && viewer.may_touch(&entry.user_id))
            .ok_or(HeldError::NotFound(GONE))
    }

    /// Claim an entry for one action and answer it as it is now.
    async fn claim(&self, viewer: &Viewer, entry_id: i64) -> Result<Entry, HeldError> {
        self.get(viewer, entry_id).await?;
        let claimed = self
            .write("held.claim", move |rows, _| {
                if !rows.claim(entry_id)? {
                    return Ok(None);
                }
                Ok(rows.entry(entry_id)?)
            })
            .await?;
        claimed.ok_or(HeldError::InUse)
    }

    /// Give an entry back after an action: waiting again, or resolved
    /// when none of its files is held any more.
    async fn release(&self, entry_id: i64) {
        // An entry whose placed files could not be recorded stays claimed
        // for the prune, which settles it by what the library holds.
        if self.is_cut_off(entry_id) {
            return;
        }
        let now = self.now();
        if let Err(error) = self
            .write("held.release", move |rows, _| {
                Ok(rows.release(entry_id, now)?)
            })
            .await
        {
            tracing::warn!(entry_id, %error, "held entry not given back; the prune finishes it");
            self.remember_cut_off(entry_id);
        }
    }

    fn is_cut_off(&self, entry_id: i64) -> bool {
        self.cut_off
            .lock()
            .is_ok_and(|cut_off| cut_off.contains(&entry_id))
    }

    /// The library took files but what became of them could not be
    /// written: the entry stays claimed and the prune settles it from the
    /// library's own record, so no file is placed twice.
    fn record_failed(&self, entry_id: i64, error: HeldError) -> HeldError {
        tracing::warn!(entry_id, %error, "placed held files not recorded; the prune settles them");
        self.remember_cut_off(entry_id);
        error
    }

    fn remember_cut_off(&self, entry_id: i64) {
        if let Ok(mut cut_off) = self.cut_off.lock()
            && !cut_off.contains(&entry_id)
        {
            cut_off.push(entry_id);
        }
    }

    /// The entry's held files an action takes: the ones named, or all. An
    /// entry with no held file left takes none, and the action's release
    /// resolves it.
    fn chosen(entry: &Entry, file_ids: Option<&[i64]>) -> Result<Vec<HeldFile>, HeldError> {
        if entry.files.is_empty() {
            return Ok(Vec::new());
        }
        let files: Vec<HeldFile> = match file_ids {
            None => entry.files.clone(),
            Some(ids) => entry
                .files
                .iter()
                .filter(|file| ids.contains(&file.id))
                .cloned()
                .collect(),
        };
        if files.is_empty() {
            return Err(HeldError::NotFound(
                "None of those files is held here any more. Refresh the list.",
            ));
        }
        Ok(files)
    }

    /// Import held files as a person's choice. A file held because our
    /// side could not take it is simply tried again. When none went in,
    /// the answer says why the first stays held.
    pub async fn import(
        &self,
        viewer: &Viewer,
        entry_id: i64,
        file_ids: Option<Vec<i64>>,
        actor: &str,
    ) -> Result<Acted, HeldError> {
        self.import_for(viewer, entry_id, file_ids, actor, Asker::Person)
            .await
    }

    /// [`Self::import`] for a person or for the held loop's retry.
    async fn import_for(
        &self,
        viewer: &Viewer,
        entry_id: i64,
        file_ids: Option<Vec<i64>>,
        actor: &str,
        asker: Asker,
    ) -> Result<Acted, HeldError> {
        let entry = self.claim(viewer, entry_id).await?;
        let result = match Self::chosen(&entry, file_ids.as_deref()) {
            Ok(files) => self.place(&entry, files, actor, asker).await,
            Err(error) => Err(error),
        };
        self.release(entry_id).await;
        let acted = result?;
        match acted.held.first() {
            Some(reason) if acted.imported == 0 => Err(refused(*reason)),
            _ => Ok(acted),
        }
    }

    /// Place an entry on the album a curator picked: its files are paired
    /// by the app, and a file that would replace a library copy needs
    /// AcoustID to confirm it.
    pub async fn match_album(
        &self,
        viewer: &Viewer,
        entry_id: i64,
        release_group_mbid: &str,
        file_ids: Option<Vec<i64>>,
        actor: &str,
    ) -> Result<Acted, HeldError> {
        let group = release_group_mbid.trim().to_ascii_lowercase();
        if group.is_empty() {
            return Err(HeldError::Invalid("Pick an album to match.".to_owned()));
        }
        let entry = self.claim(viewer, entry_id).await?;
        let result = match Self::chosen(&entry, file_ids.as_deref()) {
            Ok(files) if files.is_empty() => Ok(Acted::default()),
            Ok(files) => match entry.origin {
                EntryOrigin::Upload => self
                    .drops
                    .match_entry(&entry, &files, &group)
                    .await
                    .map(|matched| Acted {
                        imported: matched.imported,
                        owned: matched.owned,
                        discarded: 0,
                        held: Vec::new(),
                    })
                    .map_err(HeldError::from),
                EntryOrigin::Download => self.match_download(&entry, files, &group, actor).await,
            },
            Err(error) => Err(error),
        };
        self.release(entry_id).await;
        result
    }

    /// Ask AcoustID again about files it heard as another recording or
    /// could not confirm; a file it now confirms is imported.
    pub async fn recheck(
        &self,
        viewer: &Viewer,
        entry_id: i64,
        file_ids: Option<Vec<i64>>,
        actor: &str,
    ) -> Result<Acted, HeldError> {
        let entry = self.claim(viewer, entry_id).await?;
        let result = match Self::chosen(&entry, file_ids.as_deref()) {
            Ok(files) if files.is_empty() => Ok(Acted::default()),
            Ok(files) => {
                let files: Vec<HeldFile> = files.into_iter().filter(rechecks).collect();
                if files.is_empty() {
                    Err(HeldError::Refused(
                        "Only files AcoustID heard as another recording, or could not confirm, \
                         can be re-checked."
                            .to_owned(),
                    ))
                } else {
                    self.recheck_files(&entry, files, actor).await
                }
            }
            Err(error) => Err(error),
        };
        self.release(entry_id).await;
        result
    }

    /// Re-check the viewer's AcoustID holds in one go: the entries listed,
    /// in order, or every entry newest first. At most
    /// [`BULK_RECHECK_LIMIT`] checks run; one file's failure never stops
    /// the rest. Entries the viewer may not act on are left out.
    pub async fn recheck_bulk(
        &self,
        viewer: &Viewer,
        entry_ids: Option<Vec<i64>>,
        actor: &str,
    ) -> Result<Vec<Rechecked>, HeldError> {
        let mut entries = self.list(viewer, None, None, true).await?;
        if let Some(ids) = entry_ids {
            let mut seen = HashSet::new();
            entries = ids
                .into_iter()
                .filter(|id| seen.insert(*id))
                .filter_map(|id| entries.iter().find(|entry| entry.id == id).cloned())
                .collect();
        }
        let mut results = Vec::new();
        let mut checks = 0;
        for entry in entries {
            let files: Vec<HeldFile> = entry
                .files
                .iter()
                .filter(|f| rechecks(f))
                .cloned()
                .collect();
            if files.is_empty() {
                continue;
            }
            if checks >= BULK_RECHECK_LIMIT {
                break;
            }
            let files: Vec<HeldFile> = files
                .into_iter()
                .take(BULK_RECHECK_LIMIT - checks)
                .collect();
            checks += files.len();
            let ids: Vec<i64> = files.iter().map(|file| file.id).collect();
            let outcome = match self.claim(viewer, entry.id).await {
                Ok(claimed) => {
                    let mine: Vec<HeldFile> = claimed
                        .files
                        .into_iter()
                        .filter(|file| ids.contains(&file.id))
                        .collect();
                    let done = self.recheck_each(&entry, mine).await;
                    let done = self.import_confirmed(&entry, done, actor).await;
                    self.release(entry.id).await;
                    done
                }
                Err(error) => ids.iter().map(|id| (*id, Err(error.to_string()))).collect(),
            };
            results.extend(outcome.into_iter().map(|(file_id, outcome)| Rechecked {
                entry_id: entry.id,
                file_id,
                outcome,
            }));
        }
        Ok(results)
    }

    async fn recheck_files(
        &self,
        entry: &Entry,
        files: Vec<HeldFile>,
        actor: &str,
    ) -> Result<Acted, HeldError> {
        let checked = self.recheck_each(entry, files).await;
        let results = self.import_confirmed(entry, checked, actor).await;
        let mut acted = Acted::default();
        for (_, outcome) in results {
            match outcome {
                Ok(true) => acted.imported += 1,
                Ok(false) => acted.held.push(Reason::FingerprintMismatch),
                Err(message) => return Err(HeldError::Refused(message)),
            }
        }
        Ok(acted)
    }

    /// Each file's AcoustID answer: confirmed (with the file) or not.
    async fn recheck_each(
        &self,
        entry: &Entry,
        files: Vec<HeldFile>,
    ) -> Vec<(HeldFile, Result<bool, String>)> {
        let library = match self.library() {
            Ok(library) => library,
            Err(error) => {
                return files
                    .into_iter()
                    .map(|file| (file, Err(error.to_string())))
                    .collect();
            }
        };
        let mut out = Vec::with_capacity(files.len());
        for file in files {
            let path = match self.on_disk(entry, &file).await {
                Ok(Some(path)) => path,
                Ok(None) => {
                    out.push((file, Err("The held file is no longer on disk.".to_owned())));
                    continue;
                }
                Err(error) => {
                    out.push((file, Err(error.to_string())));
                    continue;
                }
            };
            let heard = library.fingerprints(vec![(String::new(), path)]).await;
            let heard = heard.get("").map(Vec::as_slice).unwrap_or(&[]);
            // No AcoustID answer is no confirmation: the file stays held.
            let confirmed = !heard.is_empty()
                && !specs::fingerprint_disagrees(
                    heard,
                    file.pairing.recording_mbid.as_deref().unwrap_or(""),
                    file.duration_seconds,
                    file.pairing.expected_duration_seconds,
                );
            out.push((file, Ok(confirmed)));
        }
        out
    }

    /// Import the files AcoustID confirmed; answers each file's outcome.
    async fn import_confirmed(
        &self,
        entry: &Entry,
        checked: Vec<(HeldFile, Result<bool, String>)>,
        actor: &str,
    ) -> Vec<(i64, Result<bool, String>)> {
        let mut confirmed = Vec::new();
        let mut results = Vec::new();
        for (file, outcome) in checked {
            match outcome {
                Ok(true) => confirmed.push(file),
                other => results.push((file.id, other)),
            }
        }
        if confirmed.is_empty() {
            return results;
        }
        let ids: Vec<i64> = confirmed.iter().map(|file| file.id).collect();
        match self.place(entry, confirmed, actor, Asker::Person).await {
            Ok(_) => {
                let still: HashSet<i64> = self
                    .read("held.recheck_left", {
                        let entry_id = entry.id;
                        move |rows| rows.entry(entry_id)
                    })
                    .await
                    .ok()
                    .flatten()
                    .map(|entry| entry.files.iter().map(|file| file.id).collect())
                    .unwrap_or_default();
                results.extend(ids.into_iter().map(|id| (id, Ok(!still.contains(&id)))));
            }
            Err(error) => {
                if let HeldError::Unavailable(cause) = &error {
                    tracing::warn!(entry_id = entry.id, %cause, "held re-check import failed");
                }
                let message = match error {
                    HeldError::Unavailable(_) => "The re-check failed.".to_owned(),
                    other => other.to_string(),
                };
                results.extend(ids.into_iter().map(|id| (id, Err(message.clone()))));
            }
        }
        results
    }

    /// Delete held files. For a download, a task none of whose files is
    /// held any more loses its wrong-product verdict, and the album's
    /// automatic retry wakes.
    pub async fn discard(
        &self,
        viewer: &Viewer,
        entry_id: i64,
        file_ids: Option<Vec<i64>>,
    ) -> Result<Acted, HeldError> {
        let entry = self.claim(viewer, entry_id).await?;
        let files = match Self::chosen(&entry, file_ids.as_deref()) {
            Ok(files) => files,
            Err(error) => {
                self.release(entry_id).await;
                return Err(error);
            }
        };
        let ids: Vec<i64> = files.iter().map(|file| file.id).collect();
        let tasks: Vec<String> = files
            .iter()
            .filter_map(|file| file.source_task_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let now = self.now();
        let discarded = self
            .write("held.discard", move |rows, store| {
                let moved = rows.resolve(entry_id, &ids, FileState::Discarded, now)?;
                for task in &tasks {
                    if !rows.task_holds(task)? {
                        store.clear_wrong_product_verdict(task)?;
                    }
                }
                rows.release(entry_id, now)?;
                Ok(moved)
            })
            .await;
        let discarded = match discarded {
            Ok(discarded) => discarded,
            Err(error) => {
                self.release(entry_id).await;
                return Err(error);
            }
        };
        for file in &files {
            self.remove(file).await;
        }
        if let Some(item) = entry.drop_item_id {
            self.drops.tidy_item(item).await;
        }
        self.worker.wake();
        Ok(Acted {
            discarded,
            ..Acted::default()
        })
    }

    /// Where a held file can be read for listening: a regular file below
    /// its own folder, reached through no link.
    pub async fn audio_path(&self, viewer: &Viewer, file_id: i64) -> Result<PathBuf, HeldError> {
        let file = self
            .read("held.file", move |rows| {
                let Some(file) = rows.file(file_id)? else {
                    return Ok(None);
                };
                Ok(rows.entry(file.entry_id)?.map(|entry| (file, entry)))
            })
            .await?
            .filter(|(_, entry)| entry.state.is_open() && viewer.may_touch(&entry.user_id))
            .map(|(file, _)| file)
            .ok_or(HeldError::NotFound("That held file was not found."))?;
        let path = self
            .folders
            .locate(file.root, &file.path)
            .ok_or(HeldError::NotFound("That held file was not found."))?;
        let root = self.folders.dir(file.root).to_path_buf();
        let checked = tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path).ok()?;
            let real = std::fs::canonicalize(&path).ok()?;
            let base = std::fs::canonicalize(&root).ok()?;
            (meta.file_type().is_file() && real.starts_with(&base)).then_some(real)
        })
        .await
        .ok()
        .flatten();
        checked.ok_or(HeldError::NotFound("That held file is no longer on disk."))
    }

    /// The held copy of a file, when it is still there. A copy that
    /// vanished is resolved as discarded, so it stops showing. A folder
    /// that cannot be read refuses the action and changes nothing: the
    /// copy may well still be there.
    async fn on_disk(&self, entry: &Entry, file: &HeldFile) -> Result<Option<PathBuf>, HeldError> {
        let Some(path) = self.folders.locate(file.root, &file.path) else {
            return Ok(None);
        };
        match tokio::fs::try_exists(&path).await {
            Ok(true) => return Ok(Some(path)),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(file_id = file.id, %error, "held file could not be checked");
                return Err(HeldError::Refused(
                    "The held file could not be read. Try again in a minute.".to_owned(),
                ));
            }
        }
        let (entry_id, file_id, now) = (entry.id, file.id, self.now());
        if let Err(error) = self
            .write("held.vanished", move |rows, _| {
                Ok(rows.resolve(entry_id, &[file_id], FileState::Discarded, now)?)
            })
            .await
        {
            tracing::warn!(file_id, %error, "a vanished held file stays listed");
        }
        Ok(None)
    }

    async fn remove(&self, file: &HeldFile) {
        super::remove_copy(self.folders.dir(file.root), &file.path).await;
    }

    /// Place held files as a person's choice: one batch per download they
    /// came from, each with that download's plan, so a file fetched for a
    /// copy joins it and a version replaces yours.
    async fn place(
        &self,
        entry: &Entry,
        files: Vec<HeldFile>,
        actor: &str,
        asker: Asker,
    ) -> Result<Acted, HeldError> {
        let mut groups: Vec<Vec<HeldFile>> = Vec::new();
        for file in files {
            match groups
                .iter_mut()
                .find(|group| group[0].source_task_id == file.source_task_id)
            {
                Some(group) => group.push(file),
                None => groups.push(vec![file]),
            }
        }
        let mut acted = Acted::default();
        for group in groups {
            self.place_group(entry, group, actor, asker, &mut acted)
                .await?;
        }
        Ok(acted)
    }

    async fn place_group(
        &self,
        entry: &Entry,
        files: Vec<HeldFile>,
        actor: &str,
        asker: Asker,
        acted: &mut Acted,
    ) -> Result<(), HeldError> {
        let library = self.library()?;
        if entry.origin == EntryOrigin::Upload && files.iter().any(|file| !file.is_paired()) {
            return Err(HeldError::Refused(
                "These files are not paired with any track yet. Pick the album with Match."
                    .to_owned(),
            ));
        }
        let mut paths = Vec::with_capacity(files.len());
        for file in &files {
            let Some(path) = self.on_disk(entry, file).await? else {
                return Err(HeldError::Refused(
                    "The held file is no longer on disk. Download the album again.".to_owned(),
                ));
            };
            paths.push(path);
        }
        let first = &files[0];
        let (task, target) = self.task_target(first).await?;
        let (release, join) = self
            .release_for(&library, entry, first, target.as_ref(), asker.priority())
            .await?;
        let mut batch_files = Vec::with_capacity(files.len());
        let mut pairing = intake::Pairing::default();
        for (index, (file, path)) in files.iter().zip(&paths).enumerate() {
            // A file whose quality cannot be read is never judged against
            // your copy: that could replace it with something unplayable.
            let Some(probed) = probe_held(path).await else {
                return Err(HeldError::Refused(
                    "The held file could not be read to compare its quality with your copy, \
                     so nothing was changed. Play it to check it, or discard it."
                        .to_owned(),
                ));
            };
            let track = track_on(&release, &file.pairing).ok_or_else(|| {
                HeldError::Refused(
                    "This track is not on the edition chosen for the album. Choose the edition \
                     it belongs to on the album page, or discard the file."
                        .to_owned(),
                )
            })?;
            batch_files.push(IntakeFile {
                file: probed,
                held_for: Some(file.reason),
            });
            // Every file is compared with the library's copies of its
            // track; with none there, it is placed as it is.
            pairing.owned.push((index, track));
        }
        let origin = self.origin_of(entry, first, task.as_ref());
        let batch = IntakeBatch {
            files: batch_files,
            target: intake::Target {
                release: release.clone(),
                join,
                asked: target.map(|target| target.album_wanted).unwrap_or_default(),
            },
            origin,
            gate: asker.gate(),
            owner: actor.to_owned(),
        };
        let placement = match intake::place(library.as_ref(), &batch, pairing, None).await {
            Ok(placement) => placement,
            Err(deferred) => {
                tracing::warn!(
                    entry_id = entry.id,
                    detail = %deferred.detail,
                    code = deferred.reason.code(),
                    "held import refused"
                );
                if deferred.reason != Reason::LibraryChanged {
                    return Err(refused(deferred.reason));
                }
                // The library changed under the import twice: the files
                // stay held with the publisher's words, to try again.
                let ids: Vec<i64> = files.iter().map(|file| file.id).collect();
                let detail = deferred.detail.clone();
                self.write("held.rehold", move |rows, _| {
                    for id in &ids {
                        rows.rehold(*id, Reason::LibraryChanged, Some(&detail))?;
                    }
                    Ok(())
                })
                .await?;
                acted
                    .held
                    .extend(std::iter::repeat_n(Reason::LibraryChanged, files.len()));
                return Ok(());
            }
        };
        let mut imported: Vec<i64> = Vec::new();
        let mut reheld: Vec<(i64, Reason, String)> = Vec::new();
        for (file, outcome) in files.iter().zip(&placement.outcomes) {
            match outcome {
                Outcome::Imported { .. } => imported.push(file.id),
                Outcome::Held { reason, detail, .. } => {
                    // A file the person chose to replace yours stays chosen
                    // when the recycle bin refused your copy: once the bin
                    // works, the same button tries again.
                    let (reason, detail) = match (file.reason, reason) {
                        (
                            chosen @ (Reason::NoBetterThanYours | Reason::VersionLowerQuality),
                            Reason::UpgradeBlocked,
                        ) => (
                            chosen,
                            format!("Your copy could not go to the recycle bin: {detail}"),
                        ),
                        _ => (*reason, detail.clone()),
                    };
                    tracing::info!(
                        entry_id = entry.id,
                        file_id = file.id,
                        code = reason.code(),
                        detail,
                        "held file stays held"
                    );
                    reheld.push((file.id, reason, detail));
                    acted.held.push(reason);
                }
                // A person's import is never dropped as owned; the file
                // stays as it was.
                Outcome::AlreadyOwned { .. } | Outcome::Untouched => acted.held.push(file.reason),
            }
        }
        let (entry_id, item, now) = (entry.id, entry.drop_item_id, self.now());
        let count = imported.len();
        let done = imported.clone();
        self.write("held.import", move |rows, _| {
            if let Some(item) = item {
                rows.add_item_imports(item, count)?;
            }
            rows.resolve(entry_id, &done, FileState::Imported, now)?;
            for (id, reason, detail) in &reheld {
                rows.rehold(*id, *reason, Some(detail))?;
            }
            Ok(())
        })
        .await
        .map_err(|error| self.record_failed(entry_id, error))?;
        acted.imported += count;
        for file in files.iter().filter(|file| imported.contains(&file.id)) {
            self.remove(file).await;
        }
        if count > 0 {
            tracing::info!(entry_id = entry.id, files = count, "held files imported");
            match (item, task) {
                (Some(item), _) => {
                    self.drops.announce_import(&release.release_group_id, count);
                    self.drops.tidy_item(item).await;
                }
                (None, Some(task)) => self.settle_task(&library, &task, &release).await,
                (None, None) => {}
            }
        }
        Ok(())
    }

    /// The batch origin of an entry's files: the download they came from
    /// (its import staged under the entry's own name), or the upload.
    fn origin_of(&self, entry: &Entry, file: &HeldFile, task: Option<&TaskRow>) -> Origin {
        match (entry.origin, entry.drop_item_id) {
            (EntryOrigin::Upload, Some(drop_item_id)) => Origin::Upload { drop_item_id },
            _ => Origin::Download {
                task_id: file
                    .source_task_id
                    .clone()
                    .unwrap_or_else(|| staging_name(entry)),
                upgrade: task.is_some_and(|task| task.origin == "upgrade"),
                staging: Some(staging_name(entry)),
                downloads_folders: None,
            },
        }
    }

    /// What the download a held file came from was for: its row, and its
    /// details and plan. `None` when the file names no download, or the
    /// download is gone. A plan that does not read is an error, never an
    /// empty plan: that would lose the copy the file joins.
    async fn task_target(
        &self,
        file: &HeldFile,
    ) -> Result<(Option<TaskRow>, Option<specs::Target>), HeldError> {
        let Some(task_id) = file.source_task_id.clone() else {
            return Ok((None, None));
        };
        let Some(task) = self.journal.read_task(&task_id).await? else {
            return Ok((None, None));
        };
        let (details, plan) = self
            .journal
            .read("downloads.held_plan", move |store| {
                Ok((store.task_details(&task_id)?, store.task_plan(&task_id)?))
            })
            .await?;
        let target = crate::landing::target_for(&task, &details, &plan);
        Ok((Some(task), Some(target)))
    }

    /// The release the files go into and the copy they join: the planned
    /// copy's release when the download was fetched for one, else the
    /// album's chosen edition, else the one the file was paired to. A file
    /// paired to another album than that (a curator's Match) goes on the
    /// album it was paired to, never back onto the one it was taken from.
    async fn release_for(
        &self,
        library: &Arc<dyn LandingLibrary>,
        entry: &Entry,
        file: &HeldFile,
        target: Option<&specs::Target>,
        priority: RequestPriority,
    ) -> Result<(Release, Option<PlannedAlbum>), HeldError> {
        let unmatched = || {
            HeldError::Refused(
                "This file was never matched to an album, so it cannot be imported as it is. \
                 Match it to an album, or discard it."
                    .to_owned(),
            )
        };
        let paired = file
            .pairing
            .release_mbid
            .clone()
            .filter(|mbid| !mbid.is_empty());
        let planned = target.and_then(|target| target.planned_album.clone());
        let release_mbid = match &planned {
            Some(planned) => planned.release_mbid.clone(),
            None => match entry
                .release_group_mbid
                .as_deref()
                .filter(|mbid| !mbid.is_empty())
            {
                Some(group) => library
                    .chosen_edition(group)
                    .await
                    .or_else(|| paired.clone())
                    .ok_or_else(unmatched)?,
                None => paired.clone().ok_or_else(unmatched)?,
            },
        };
        let release = read_release(library, entry, &release_mbid, priority).await?;
        // A paired edition MusicBrainz no longer has leaves the chosen one.
        // One it could not be asked about refuses for now: the pairing may
        // name another album, so the chosen one is never assumed.
        if let Some(paired) = paired.filter(|mbid| !release.answers_to(mbid)) {
            match library.release(&paired, priority).await {
                Ok(Some(other))
                    if !other
                        .release_group_id
                        .eq_ignore_ascii_case(&release.release_group_id) =>
                {
                    return Ok((other, None));
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(entry_id = entry.id, ?error, "paired release lookup failed");
                    return Err(HeldError::Refused(
                        "MusicBrainz could not be reached to read the matched album's \
                         tracklist. Try again in a minute."
                            .to_owned(),
                    ));
                }
            }
        }
        Ok((release, planned))
    }

    /// Place a download's held files on the album a curator picked: one
    /// batch per download they came from, each paired by the app.
    async fn match_download(
        &self,
        entry: &Entry,
        files: Vec<HeldFile>,
        group: &str,
        actor: &str,
    ) -> Result<Acted, HeldError> {
        let library = self.library()?;
        let mut groups: Vec<Vec<HeldFile>> = Vec::new();
        for file in files {
            match groups
                .iter_mut()
                .find(|batch| batch[0].source_task_id == file.source_task_id)
            {
                Some(batch) => batch.push(file),
                None => groups.push(vec![file]),
            }
        }
        let mut acted = Acted::default();
        for files in groups {
            let mut paths = Vec::with_capacity(files.len());
            let mut kept = Vec::with_capacity(files.len());
            for file in files {
                if let Some(path) = self.on_disk(entry, &file).await? {
                    paths.push(path);
                    kept.push(file);
                }
            }
            if kept.is_empty() {
                continue;
            }
            let landing = probe_files(library.as_ref(), paths).await;
            // Files that no longer read are left as they are.
            let files: Vec<HeldFile> = kept
                .into_iter()
                .filter(|file| {
                    self.folders
                        .locate(file.root, &file.path)
                        .is_some_and(|path| landing.audio.iter().any(|audio| audio.path == path))
                })
                .collect();
            if files.is_empty() {
                return Err(HeldError::Refused(
                    "The held files could not be read as audio, so nothing was changed. Play \
                     them to check them, or discard them."
                        .to_owned(),
                ));
            }
            let summary = match identify::assigned(library.as_ref(), &landing, group).await {
                identify::Outcome::Matched(summary) => summary,
                identify::Outcome::Review { reason, .. } => return Err(refused(reason)),
                identify::Outcome::Unavailable(detail) => {
                    tracing::warn!(entry_id = entry.id, %detail, "held match: MusicBrainz unavailable");
                    return Err(refused(Reason::MusicbrainzUnavailable));
                }
            };
            let Some((found, _, pairing)) =
                crate::drop_import::album_pairing(&summary, landing.audio.len(), true, false)
            else {
                return Err(refused(Reason::NoAlbumMatch));
            };
            let release = found.release.clone();
            let first = &files[0];
            let (task, _) = self.task_target(first).await?;
            let batch = IntakeBatch {
                files: landing
                    .audio
                    .iter()
                    .map(|file| IntakeFile {
                        file: file.clone(),
                        held_for: None,
                    })
                    .collect(),
                target: intake::Target {
                    release: release.clone(),
                    join: None,
                    asked: Vec::new(),
                },
                origin: self.origin_of(entry, first, task.as_ref()),
                gate: Gate::PersonChoseAlbum,
                owner: actor.to_owned(),
            };
            let placement = intake::place(library.as_ref(), &batch, pairing, None)
                .await
                .map_err(|deferred| {
                    tracing::warn!(entry_id = entry.id, detail = %deferred.detail, "held match refused");
                    refused(deferred.reason)
                })?;
            let by_path = |path: &Path| {
                files.iter().find(|file| {
                    self.folders
                        .locate(file.root, &file.path)
                        .is_some_and(|located| located == path)
                })
            };
            let mut decided: Vec<Decided> = Vec::new();
            let mut consumed = Vec::new();
            for (audio, outcome) in landing.audio.iter().zip(&placement.outcomes) {
                let Some(file) = by_path(&audio.path) else {
                    continue;
                };
                match outcome {
                    Outcome::Imported { .. } => {
                        acted.imported += 1;
                        consumed.push(file.clone());
                        decided.push((file.id, Some(FileState::Imported), None));
                    }
                    Outcome::AlreadyOwned { .. } => {
                        acted.owned += 1;
                        consumed.push(file.clone());
                        decided.push((file.id, Some(FileState::AlreadyOwned), None));
                    }
                    Outcome::Held {
                        reason,
                        detail,
                        track,
                    } => {
                        acted.held.push(*reason);
                        let pairing = track
                            .and_then(|track| release.tracks.get(track))
                            .map(|on| Pairing {
                                release_mbid: Some(release.id.clone()),
                                release_track_mbid: Some(on.id.clone()),
                                recording_mbid: Some(on.recording_id.clone()),
                                disc_number: Some(i64::from(on.disc.max(1))),
                                track_number: Some(i64::from(on.position)),
                                track_title: Some(on.title.clone()),
                                expected_duration_seconds: on
                                    .length_ms
                                    .map(|ms| ms as f64 / 1000.0),
                            })
                            .unwrap_or_else(|| file.pairing.clone());
                        decided.push((file.id, None, Some((*reason, detail.clone(), pairing))));
                    }
                    Outcome::Untouched => acted.held.push(file.reason),
                }
            }
            let (entry_id, now) = (entry.id, self.now());
            self.write("held.match", move |rows, _| {
                for (id, state, held) in &decided {
                    if let Some(state) = state {
                        rows.resolve(entry_id, &[*id], *state, now)?;
                    }
                    if let Some((reason, detail, pairing)) = held {
                        rows.rehold(*id, *reason, Some(detail))?;
                        rows.pair(*id, pairing)?;
                    }
                }
                Ok(())
            })
            .await
            .map_err(|error| self.record_failed(entry_id, error))?;
            for file in &consumed {
                self.remove(file).await;
            }
            if let Some(task) = task
                && acted.imported > 0
            {
                self.settle_task(&library, &task, &release).await;
            }
        }
        Ok(acted)
    }

    /// The import may have made the album whole: then a failed or partial
    /// source task completes, so it stops waiting on a retry it no longer
    /// needs. Best effort: the import already stuck.
    async fn settle_task(
        &self,
        library: &Arc<dyn LandingLibrary>,
        task: &TaskRow,
        release: &Release,
    ) {
        if !matches!(task.status, TaskStatus::Failed | TaskStatus::Partial) {
            return;
        }
        let task_id = task.id.clone();
        let probe = task_id.clone();
        match self
            .read("held.task_holds", move |rows| rows.task_holds(&probe))
            .await
        {
            Ok(false) => {}
            Ok(true) => return,
            Err(error) => {
                tracing::warn!(%task_id, %error, "held files unreadable after a held import");
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
        let now = self.db.now_unix_f64();
        let id = task_id.clone();
        match self
            .journal
            .run("downloads.held_settle", move |store| {
                store.complete_after_manual_import(&id, now)
            })
            .await
        {
            Ok(true) => self.worker.announce_completed(task).await,
            Ok(false) => {}
            Err(error) => tracing::warn!(%task_id, %error, "task not settled after a held import"),
        }
    }

    /// Startup, before any action can run: remember the entries an action
    /// had claimed when the server stopped, and remove copies in the held
    /// folder that no open entry names (a copy made just before a stop,
    /// before its row). The drop staging folder is left to its own job
    /// clean-up.
    pub async fn recover(&self) {
        match self.read("held.placing", |rows| rows.placing()).await {
            Ok(entries) => {
                for entry in entries {
                    self.remember_cut_off(entry.id);
                }
            }
            Err(error) => tracing::warn!(%error, "held entries unreadable at start"),
        }
        let named = match self
            .read("held.folder", |rows| rows.held_folder_paths())
            .await
        {
            Ok(named) => named,
            Err(error) => {
                tracing::warn!(%error, "held files unreadable; the held folder is left as is");
                return;
            }
        };
        let dir = self.folders.held.clone();
        let swept = tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                return 0;
            };
            let mut removed = 0;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if named.contains(&name) {
                    continue;
                }
                let path = entry.path();
                let gone = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match gone {
                    Ok(()) => removed += 1,
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "stray held copy not removed");
                    }
                }
            }
            removed
        })
        .await;
        match swept {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "stray held copies removed"),
            Err(error) => tracing::warn!(%error, "held folder sweep did not run"),
        }
    }

    /// One pass of the prune loop: finish actions a restart cut off,
    /// remove the held copies of entries past their expiry, then retry the
    /// files our side could not take where their next try is due. Answers
    /// how many entries expired.
    pub async fn tend(&self) -> usize {
        self.finish_cut_off().await;
        let expired = self.expire_due().await;
        self.retry_due().await;
        expired
    }

    /// Retry each due entry's files held for a library reason, exactly as
    /// its owner pressing Retry would. An entry whose files still cannot
    /// go in waits longer before its next try; a busy database, imports
    /// not set up yet, or a person acting on the entry spend no try.
    async fn retry_due(&self) {
        let now = self.now();
        let due = match self
            .read("held.retry_due", move |rows| {
                rows.due_retries(now, RETRY_BATCH)
            })
            .await
        {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(%error, "held entries to retry unreadable; the retry waits");
                return;
            }
        };
        for (entry_id, owner) in due {
            let files = match self
                .read("held.retry_files", move |rows| rows.entry(entry_id))
                .await
            {
                Ok(Some(entry)) => entry
                    .files
                    .iter()
                    .filter(|file| retries_by_itself(file))
                    .map(|file| file.id)
                    .collect::<Vec<_>>(),
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(entry_id, %error, "held entry to retry unreadable");
                    continue;
                }
            };
            if files.is_empty() {
                continue;
            }
            let viewer = Viewer {
                user_id: owner.clone(),
                admin: false,
            };
            match self
                .import_for(&viewer, entry_id, Some(files), &owner, Asker::Loop)
                .await
            {
                Ok(acted) => {
                    tracing::info!(entry_id, imported = acted.imported, "held files retried");
                }
                Err(HeldError::Busy(operation)) => {
                    tracing::info!(entry_id, operation, "held retry waits for a busy database");
                    continue;
                }
                Err(HeldError::NotSetUp) => return,
                Err(HeldError::InUse | HeldError::NotFound(_)) => continue,
                Err(HeldError::Refused(message) | HeldError::Invalid(message)) => {
                    tracing::info!(entry_id, message, "held files still held after a retry");
                }
                Err(HeldError::Unavailable(cause)) => {
                    tracing::warn!(entry_id, %cause, "held retry failed");
                }
            }
            let now = self.now();
            if let Err(error) = self
                .write("held.retry_schedule", move |rows, _| {
                    Ok(rows.schedule_retry(entry_id, now)?)
                })
                .await
            {
                tracing::warn!(entry_id, %error, "held retry not rescheduled; the next pass tries again");
            }
        }
    }

    /// An action a restart cut off: once every library write from its
    /// staging folder is over, each file that went in is marked imported
    /// there and then (so nobody places it twice), and the entry waits
    /// again with the rest.
    async fn finish_cut_off(&self) {
        let pending: Vec<i64> = self
            .cut_off
            .lock()
            .map(|cut_off| cut_off.clone())
            .unwrap_or_default();
        if pending.is_empty() {
            return;
        }
        let Ok(library) = self.library() else {
            return;
        };
        for entry_id in pending {
            let entry = match self
                .read("held.cut_off", move |rows| rows.entry(entry_id))
                .await
            {
                Ok(Some(entry)) if entry.state == super::store::EntryState::Placing => entry,
                Ok(_) => {
                    self.forget_cut_off(entry_id);
                    continue;
                }
                Err(error) => {
                    tracing::warn!(entry_id, %error, "cut-off held entry unreadable");
                    continue;
                }
            };
            let placed = match library.placed_sources(&staging_name(&entry)).await {
                Ok(Some(placed)) => placed,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(entry_id, %error, "cut-off held entry waits for the library");
                    continue;
                }
            };
            let mut committed = Vec::new();
            for file in &entry.files {
                let Some(path) = self.folders.locate(file.root, &file.path) else {
                    continue;
                };
                let hashed = tokio::task::spawn_blocking(move || {
                    droppedneedle_kernel::db::backup::file_sha256(&path)
                })
                .await;
                if let Ok(Ok((sha, _))) = hashed
                    && placed.contains(&sha)
                {
                    committed.push(file.clone());
                }
            }
            let ids: Vec<i64> = committed.iter().map(|file| file.id).collect();
            let (item, now) = (entry.drop_item_id, self.now());
            let written = self
                .write("held.finish_cut_off", move |rows, _| {
                    if let Some(item) = item {
                        rows.add_item_imports(item, ids.len())?;
                    }
                    rows.resolve(entry_id, &ids, FileState::Imported, now)?;
                    rows.release(entry_id, now)?;
                    Ok(())
                })
                .await;
            match written {
                Ok(()) => {
                    tracing::info!(
                        entry_id,
                        imported = committed.len(),
                        "held action cut off by a restart finished"
                    );
                    for file in &committed {
                        self.remove(file).await;
                    }
                    self.forget_cut_off(entry_id);
                }
                Err(error) => tracing::warn!(entry_id, %error, "cut-off held entry not finished"),
            }
        }
    }

    fn forget_cut_off(&self, entry_id: i64) {
        if let Ok(mut cut_off) = self.cut_off.lock() {
            cut_off.retain(|id| *id != entry_id);
        }
    }

    /// Expire every entry past its expiry: each is claimed first (`held`
    /// to `expired`, so one being acted on is skipped), and only once the
    /// claim is written are its held copies removed. Nothing else is
    /// deleted.
    async fn expire_due(&self) -> usize {
        let now = self.now();
        let due = match self
            .read("held.due", move |rows| rows.due(now, PRUNE_BATCH))
            .await
        {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(%error, "expired held entries unreadable; the prune waits");
                return 0;
            }
        };
        let mut expired = 0;
        for entry_id in due {
            let claimed = self
                .write("held.expire", move |rows, _| {
                    let item = rows.entry(entry_id)?.and_then(|entry| entry.drop_item_id);
                    Ok(rows.expire(entry_id, now)?.map(|copies| (copies, item)))
                })
                .await;
            let (copies, item) = match claimed {
                Ok(Some(claimed)) => claimed,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(entry_id, %error, "expired held entry not claimed");
                    continue;
                }
            };
            for (root, path) in &copies {
                super::remove_copy(self.folders.dir(*root), path).await;
            }
            if let Some(item) = item {
                self.drops.tidy_item(item).await;
            }
            tracing::info!(
                entry_id,
                files = copies.len(),
                "held entry expired after 90 days"
            );
            expired += 1;
        }
        expired
    }
}

/// What a match decided for one held file: resolved, or held again with
/// its reason, detail and new pairing.
type Decided = (i64, Option<FileState>, Option<(Reason, String, Pairing)>);

/// The staging folder name an entry's imports are written under, which is
/// how a restart finds what an action placed.
fn staging_name(entry: &Entry) -> String {
    match (entry.origin, entry.drop_item_id) {
        (EntryOrigin::Upload, Some(item)) => format!("drop-{item}"),
        _ => format!("held-{}", entry.id),
    }
}

/// Who asked for an import: a person pressing a button, or the held loop
/// retrying by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asker {
    Person,
    Loop,
}

impl Asker {
    /// A person waits on the answer; the loop is background work.
    fn priority(self) -> RequestPriority {
        match self {
            Self::Person => RequestPriority::UserInitiated,
            Self::Loop => RequestPriority::BackgroundSync,
        }
    }

    /// The intake gate: the loop never places a second copy of a track
    /// the library already holds.
    fn gate(self) -> Gate {
        match self {
            Self::Person => Gate::PersonChoseFile,
            Self::Loop => Gate::HeldRetry,
        }
    }
}

/// One release's tracklist, or the refusal that says why it cannot be read.
async fn read_release(
    library: &Arc<dyn LandingLibrary>,
    entry: &Entry,
    release_mbid: &str,
    priority: RequestPriority,
) -> Result<Release, HeldError> {
    match library.release(release_mbid, priority).await {
        Ok(Some(release)) => Ok(release),
        Ok(None) => Err(HeldError::Refused(
            "The album's edition is no longer on MusicBrainz. Choose another edition \
             on the album page, then import again."
                .to_owned(),
        )),
        Err(error) => {
            tracing::warn!(entry_id = entry.id, ?error, "release lookup failed");
            Err(HeldError::Refused(
                "MusicBrainz could not be reached to read the album's tracklist. \
                 Try again in a minute."
                    .to_owned(),
            ))
        }
    }
}

/// Whether a file's hold is one AcoustID can settle on a second ask.
fn rechecks(file: &HeldFile) -> bool {
    matches!(
        file.reason,
        Reason::FingerprintMismatch | Reason::FingerprintUnconfirmed
    )
}

/// The answer for a file that stays held.
fn refused(reason: Reason) -> HeldError {
    HeldError::Refused(format!("{} {}", reason.message(), reason.action()))
}

/// Read files off the async workers, never entering the library.
async fn probe_files(library: &dyn LandingLibrary, paths: Vec<PathBuf>) -> Landing {
    let libraries = canonical_dirs(&library.library_dirs());
    match tokio::task::spawn_blocking(move || probe::probe(&paths, &libraries)).await {
        Ok(landing) => landing,
        Err(error) => {
            tracing::error!(%error, "held file probe did not run");
            Landing::default()
        }
    }
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
fn track_on(release: &Release, pairing: &Pairing) -> Option<usize> {
    let same = |left: &str, right: Option<&str>| {
        right.is_some_and(|right| !right.is_empty() && left.eq_ignore_ascii_case(right))
    };
    release
        .tracks
        .iter()
        .position(|track| same(&track.id, pairing.release_track_mbid.as_deref()))
        .or_else(|| {
            release
                .tracks
                .iter()
                .position(|track| same(&track.recording_id, pairing.recording_mbid.as_deref()))
        })
}

/// Whether a reason's hold is retried as it is ("Retry") rather than
/// imported past the check that held it.
pub fn is_retry(reason: Reason) -> bool {
    reason.kind() == ReasonKind::Library
}

/// Whether a held file offers Retry, and so is retried by itself: held
/// for a library reason and paired with a track.
pub fn retries_by_itself(file: &HeldFile) -> bool {
    is_retry(file.reason) && file.is_paired()
}

/// The held list's loop runs every five minutes, the first pass a minute
/// after start: often enough for the first automatic retry, five minutes
/// after a hold.
const HELD_PRUNE_INTERVAL: Duration = Duration::from_secs(300);
const HELD_PRUNE_DELAY: Duration = Duration::from_secs(60);

/// The held list's loop: a minute after start, then every five
/// minutes, it finishes actions a restart cut off, removes the held
/// copies of entries past their expiry and retries the files our side
/// could not take whose next try is due.
pub(crate) fn spawn_held_prune(
    supervisor: &Supervisor,
    held: Arc<HeldService>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        PRUNE_JOB,
        Schedule::new(HELD_PRUNE_INTERVAL).with_initial_delay(HELD_PRUNE_DELAY),
        FnTick(move |ctx: TickCtx| {
            let held = held.clone();
            async move {
                tokio::select! {
                    () = ctx.token().cancelled() => {}
                    _ = held.tend() => {}
                }
                Ok(Next::Continue)
            }
        }),
    )
}
