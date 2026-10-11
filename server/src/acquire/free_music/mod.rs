//! Free Music: DroppedNeedle's own lawful download client.
//!
//! Requests are served from the Internet Archive, only from items that
//! carry an explicit Creative Commons or public-domain licence. When no
//! other download source is set up and Free Music is on, album and track
//! requests come here (the request dispatch asks [`FreeMusic::takes_requests`]).
//!
//! One task: search the Archive for the artist and title, keep items
//! whose title really is the album (title containment of at least 0.60),
//! pair each with every format it offers, rank them (track count first,
//! then the preferred format, then lossless, then size), and download the
//! best. Downloaded files are checked against the download quality band;
//! a copy outside it is thrown away and the next source tried. The ranked
//! ladder and each source's outcome are stored, so a retry never repeats
//! a source that already failed. The files then go to drop import, which
//! identifies, tags and files them and resolves the request.
//!
//! Tasks live in SQLite ([`store`]); progress reaches the owner's tabs as
//! `free_music_updated`. A restart fails the tasks it cut off.

pub mod archive;
pub mod http;
pub mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;

use self::archive::{ArchiveError, ArchiveFile, ArchiveSource};
use self::store::{FreeMusicStore, NewTask, TaskRecord, status};
use super::drop_import::{DropImports, Viewer};
use super::landing::quality::{in_range, worst};
use crate::events::{EventSink, FreeMusicUpdated, UserNotice, new_event_id};
use crate::providers::archive::extension_for;

/// A candidate whose title bears no resemblance to the requested album is
/// a different record sharing an artist (v2 `_TITLE_MATCH_FLOOR`).
pub const TITLE_MATCH_FLOOR: f64 = 0.60;
/// Progress writes hit the store at most this often.
const PROGRESS_EVERY_SECS: f64 = 1.0;
/// Largest file accepted when the Archive declares no size.
const MAX_UNDECLARED_FILE_BYTES: i64 = 4 * (1 << 30);
/// Tasks listed (v2 `list_tasks` limit).
const LIST_LIMIT: i64 = 50;

/// One reason a task failed: code, sentence, action.
struct Failure {
    code: &'static str,
    message: &'static str,
    action: &'static str,
}

const DISABLED: Failure = Failure {
    code: "disabled",
    message: "Free Music is turned off.",
    action: "Turn Free Music on in Settings, or set up another download source.",
};
const ARCHIVE_UNREACHABLE: Failure = Failure {
    code: "archive_unreachable",
    message: "Couldn't reach the Internet Archive.",
    action: "Retry in a few minutes.",
};
const NO_SOURCE: Failure = Failure {
    code: "no_source",
    message: "No free source has this. Try buying it instead.",
    action: "Buy the album and add it with drop import.",
};
const NO_REMAINING_SOURCE: Failure = Failure {
    code: "no_remaining_source",
    message: "Every free source for this has already been tried.",
    action: "Buy the album and add it with drop import.",
};
const DOWNLOAD_FAILED: Failure = Failure {
    code: "download_failed",
    message: "The download failed.",
    action: "Retry. If it keeps failing, the Internet Archive may be having trouble.",
};
const QUALITY_MISMATCH: Failure = Failure {
    code: "quality_mismatch",
    message: "The downloaded copies didn't meet your download quality settings.",
    action: "Widen the quality range in download settings and retry, or buy the album.",
};
const NO_FILES: Failure = Failure {
    code: "no_files",
    message: "The download produced no files.",
    action: "Retry.",
};
const HANDOFF_FAILED: Failure = Failure {
    code: "import_failed",
    message: "The download finished but couldn't be handed to the importer.",
    action: "Check that a library folder is set up, then retry.",
};
const INTERRUPTED: Failure = Failure {
    code: "interrupted",
    message: "Interrupted by a restart.",
    action: "Retry.",
};

/// Settings one task reads when it runs.
#[derive(Debug, Clone)]
pub struct FreeMusicSettings {
    pub enabled: bool,
    /// Rank MP3 before FLAC.
    pub prefer_mp3: bool,
    /// Download quality band (tier names).
    pub quality_min: String,
    pub quality_max: String,
}

/// The quality policy a task was asked under, pinned when the request
/// was made and kept through retries (v2 acquisition quality snapshot):
/// a later settings change never moves a download already asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualitySnapshot {
    pub quality_min: String,
    pub quality_max: String,
    pub prefer_mp3: bool,
}

impl QualitySnapshot {
    /// The policy in the live settings now.
    pub fn of(settings: &FreeMusicSettings) -> Self {
        Self {
            quality_min: settings.quality_min.clone(),
            quality_max: settings.quality_max.clone(),
            prefer_mp3: settings.prefer_mp3,
        }
    }

    /// Stored form.
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Stable short hash of the stored form.
    pub fn hash(&self) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(self.encode().as_bytes());
        digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// One line a person reads: "MP3 320 to lossless, FLAC first".
    pub fn summary(&self) -> String {
        let label = |tier: &str| match tier {
            "low" => "any quality".to_owned(),
            "mp3_192" => "MP3 192".to_owned(),
            "mp3_256" => "MP3 256".to_owned(),
            "mp3_320" => "MP3 320".to_owned(),
            "lossless" => "lossless".to_owned(),
            other => other.to_owned(),
        };
        let first = if self.prefer_mp3 { "MP3" } else { "FLAC" };
        format!(
            "{} to {}, {first} first",
            label(&self.quality_min),
            label(&self.quality_max)
        )
    }

    /// Settings for one run: this policy, the live on/off switch.
    fn settings(&self, enabled: bool) -> FreeMusicSettings {
        FreeMusicSettings {
            enabled,
            prefer_mp3: self.prefer_mp3,
            quality_min: self.quality_min.clone(),
            quality_max: self.quality_max.clone(),
        }
    }
}

/// Album or track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskKind {
    Album,
    Track,
}

impl AskKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Track => "track",
        }
    }
}

/// One request Free Music should fetch.
#[derive(Debug, Clone)]
pub struct FreeMusicAsk {
    pub user_id: String,
    pub kind: AskKind,
    /// Release group (album) or recording (track).
    pub mbid: String,
    pub artist: String,
    pub title: String,
    /// Tracks on the album when known (0 otherwise).
    pub track_count: i64,
    pub origin: String,
    pub release_group_mbid: Option<String>,
    /// The edition asked for, when the request named one.
    pub release_mbid: Option<String>,
    /// The caller's key; a repeated ask answers the first task.
    pub idempotency_key: Option<String>,
}

/// Why a Free Music call was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreeMusicError {
    /// Wrong state or bad input; the message is safe to show.
    Invalid(String),
    /// No such task for this person.
    NotFound,
    /// Storage failed; the cause is logged, not shown.
    Unavailable(String),
}

impl From<store::StoreError> for FreeMusicError {
    fn from(error: store::StoreError) -> Self {
        Self::Unavailable(error.0)
    }
}

/// One Archive item paired with one of its formats.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    identifier: String,
    licence_url: String,
    /// Archive format string (`flac`, `vbr mp3`, ...).
    format: String,
    extension: &'static str,
    size_bytes: i64,
    filenames: Vec<String>,
    /// The size the Archive declares for each file, in `filenames` order.
    sizes: Vec<i64>,
}

/// One rung of the stored ladder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Rung {
    identifier: String,
    format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
}

const TRIED_COMPLETED: &str = "completed";
const TRIED_QUALITY: &str = "post_download_quality_mismatch";

/// The Free Music service.
pub struct FreeMusic {
    store: FreeMusicStore,
    archive: Arc<dyn ArchiveSource>,
    drops: Arc<DropImports>,
    settings: Arc<dyn Fn() -> FreeMusicSettings + Send + Sync>,
    /// Whether another download source is set up right now.
    other_sources: Arc<dyn Fn() -> bool + Send + Sync>,
    events: EventSink,
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl FreeMusic {
    pub fn new(
        store: FreeMusicStore,
        archive: Arc<dyn ArchiveSource>,
        drops: Arc<DropImports>,
        settings: Arc<dyn Fn() -> FreeMusicSettings + Send + Sync>,
        other_sources: Arc<dyn Fn() -> bool + Send + Sync>,
        events: EventSink,
    ) -> Self {
        Self {
            store,
            archive,
            drops,
            settings,
            other_sources,
            events,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Requests go to Free Music when it is on and no other download
    /// source (Soulseek, Usenet, a plugin) is set up (v2
    /// `AcquisitionDispatcher._use_free_music`).
    pub fn takes_requests(&self) -> bool {
        (self.settings)().enabled && !(self.other_sources)()
    }

    /// Start one task; answers its id.
    pub async fn start(self: &Arc<Self>, ask: FreeMusicAsk) -> Result<String, FreeMusicError> {
        let settings = (self.settings)();
        if !settings.enabled {
            return Err(FreeMusicError::Invalid(DISABLED.message.into()));
        }
        let snapshot = QualitySnapshot::of(&settings);
        let fresh = uuid::Uuid::new_v4().simple().to_string();
        let id = self
            .store
            .create(NewTask {
                id: fresh.clone(),
                user_id: ask.user_id.clone(),
                kind: ask.kind.as_str(),
                mbid: ask.mbid,
                artist: ask.artist,
                title: ask.title,
                track_count: ask.track_count,
                origin: ask.origin,
                release_group_mbid: ask.release_group_mbid,
                quality_snapshot_json: snapshot.encode(),
                quality_snapshot_hash: snapshot.hash(),
                quality_snapshot_summary: snapshot.summary(),
                release_mbid: ask.release_mbid,
                idempotency_key: ask.idempotency_key,
            })
            .await?;
        if id != fresh {
            return Ok(id);
        }
        self.spawn(&id);
        self.publish(&ask.user_id, &id, status::SEARCHING);
        Ok(id)
    }

    /// The status of one task, for the request views.
    pub async fn status_of(&self, id: &str) -> Option<String> {
        match self.store.get(id).await {
            Ok(task) => task.map(|task| task.status),
            Err(error) => {
                tracing::warn!(task_id = id, %error, "free music task unreadable");
                None
            }
        }
    }

    /// The person's tasks, newest first; admins may ask for everyone's.
    pub async fn list(
        &self,
        viewer: &Viewer,
        all: bool,
    ) -> Result<Vec<TaskRecord>, FreeMusicError> {
        let user = (!(all && viewer.admin)).then_some(viewer.user_id.as_str());
        Ok(self.store.list(user, LIST_LIMIT).await?)
    }

    /// One task the person may see.
    pub async fn get(&self, viewer: &Viewer, id: &str) -> Result<TaskRecord, FreeMusicError> {
        match self.store.get(id).await? {
            Some(task) if viewer.admin || task.user_id == viewer.user_id => Ok(task),
            _ => Err(FreeMusicError::NotFound),
        }
    }

    /// Stop a task that has not started importing.
    pub async fn cancel(&self, viewer: &Viewer, id: &str) -> Result<TaskRecord, FreeMusicError> {
        let task = self.get(viewer, id).await?;
        if status::is_terminal(&task.status) {
            return Err(FreeMusicError::Invalid(
                "That download has already finished.".into(),
            ));
        }
        if task.status == status::IMPORTING {
            return Err(FreeMusicError::Invalid(
                "This download is already being added to your library.".into(),
            ));
        }
        if !self.stop(&task).await? {
            return Err(FreeMusicError::Invalid(
                "That download has already finished.".into(),
            ));
        }
        self.get(viewer, id).await
    }

    /// Cancel a task that has not started importing and stop its run.
    /// Answers whether it was cancelled.
    async fn stop(&self, task: &TaskRecord) -> Result<bool, FreeMusicError> {
        if !self.store.cancel_active(&task.id).await? {
            return Ok(false);
        }
        if let Some(flag) = self.flag(&task.id) {
            flag.store(true, Ordering::SeqCst);
        }
        self.publish(&task.user_id, &task.id, status::CANCELLED);
        Ok(true)
    }

    /// Start a failed or cancelled task again.
    pub async fn retry(
        self: &Arc<Self>,
        viewer: &Viewer,
        id: &str,
    ) -> Result<TaskRecord, FreeMusicError> {
        let task = self.get(viewer, id).await?;
        if !matches!(task.status.as_str(), status::FAILED | status::CANCELLED) {
            return Err(FreeMusicError::Invalid(
                "Only a failed or cancelled download can be retried.".into(),
            ));
        }
        if self.flag(id).is_some() {
            return Err(FreeMusicError::Invalid(
                "Wait for this download to stop before retrying.".into(),
            ));
        }
        if !self.store.restart_terminal(id).await? {
            return Err(FreeMusicError::Invalid(
                "That download is no longer available to retry.".into(),
            ));
        }
        self.spawn(id);
        self.publish(&task.user_id, id, status::SEARCHING);
        self.get(viewer, id).await
    }

    /// Remove one finished task from the history.
    pub async fn remove(&self, viewer: &Viewer, id: &str) -> Result<(), FreeMusicError> {
        let task = self.get(viewer, id).await?;
        if !status::is_terminal(&task.status) {
            return Err(FreeMusicError::Invalid(
                "Cancel this download before removing it.".into(),
            ));
        }
        let Some(owner) = self.store.delete_terminal(id).await? else {
            return Err(FreeMusicError::Invalid(
                "That download is no longer available to remove.".into(),
            ));
        };
        self.publish(&owner, id, "removed");
        Ok(())
    }

    /// Remove every finished task of the person (admins: everyone's).
    pub async fn clear(&self, viewer: &Viewer, all: bool) -> Result<usize, FreeMusicError> {
        let user = (!(all && viewer.admin)).then(|| viewer.user_id.clone());
        let owners = self.store.delete_terminal_tasks(user).await?;
        let mut told: Vec<&String> = owners.iter().collect();
        told.sort_unstable();
        told.dedup();
        for owner in told {
            self.publish(owner, "", "removed");
        }
        Ok(owners.len())
    }

    /// Startup: tasks a restart cut off fail; their partial downloads go.
    pub async fn sweep_stale(&self) {
        match self
            .store
            .fail_stale(INTERRUPTED.message, INTERRUPTED.code, INTERRUPTED.action)
            .await
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(
                tasks = count,
                "free music tasks cut off by a restart failed"
            ),
            Err(error) => tracing::warn!(%error, "free music restart sweep failed"),
        }
    }

    fn flag(&self, id: &str) -> Option<Arc<AtomicBool>> {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
    }

    fn spawn(self: &Arc<Self>, id: &str) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_owned(), cancel.clone());
        let service = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            service.run(&id, &cancel).await;
            service
                .running
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
        });
    }

    async fn run(&self, id: &str, cancel: &AtomicBool) {
        let task = match self.store.get(id).await {
            Ok(Some(task)) => task,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(task_id = id, %error, "free music task unreadable");
                return;
            }
        };
        let dest = self.drops.incoming_dir().join(format!("free-{id}"));
        let outcome = self.download(&task, cancel, &dest).await;
        let files = match outcome {
            Ok(Some(files)) => files,
            Ok(None) => {
                remove_dir(&dest).await;
                return;
            }
            Err(failure) => {
                remove_dir(&dest).await;
                self.fail(&task, failure).await;
                return;
            }
        };
        if cancel.load(Ordering::SeqCst)
            || !self
                .advance(&task, &[status::DOWNLOADING], status::IMPORTING)
                .await
        {
            remove_dir(&dest).await;
            return;
        }
        let uploads: Vec<(String, PathBuf)> = files
            .into_iter()
            .map(|path| {
                // "CD2 - 01.flac": the disc folder survives the flat
                // staging names drop import gives uploads.
                let name = path
                    .strip_prefix(&dest)
                    .unwrap_or(&path)
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" - ");
                (name, path)
            })
            .collect();
        let handed = self
            .drops
            .create_job(&task.user_id, "Free Music", uploads)
            .await;
        remove_dir(&dest).await;
        if let Err(error) = handed {
            tracing::warn!(
                task_id = id,
                ?error,
                "free music could not hand files to drop import"
            );
            self.fail(&task, HANDOFF_FAILED).await;
            return;
        }
        self.advance(&task, &[status::IMPORTING], status::COMPLETED)
            .await;
    }

    /// Search, rank and download until one source gives files inside the
    /// quality band. `Ok(None)`: cancelled.
    async fn download(
        &self,
        task: &TaskRecord,
        cancel: &AtomicBool,
        dest: &Path,
    ) -> Result<Option<Vec<PathBuf>>, Failure> {
        let live = (self.settings)();
        if !live.enabled {
            return Err(DISABLED);
        }
        // The policy pinned when the request was made; a task stored
        // before snapshots existed uses today's settings.
        let settings = task
            .quality_snapshot_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<QualitySnapshot>(json).ok())
            .map_or(live.clone(), |snapshot| snapshot.settings(live.enabled));
        let track_count = self.track_count(task).await;
        let mut candidates = match self.candidates(task, track_count, &settings).await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "free music search failed");
                return Err(ARCHIVE_UNREACHABLE);
            }
        };
        if candidates.is_empty() {
            return Err(NO_SOURCE);
        }
        let mut ladder: Vec<Rung> =
            serde_json::from_str(&task.tried_candidates_json).unwrap_or_default();
        if ladder.is_empty() {
            ladder = candidates
                .iter()
                .map(|candidate| Rung {
                    identifier: candidate.identifier.clone(),
                    format: candidate.format.clone(),
                    outcome: None,
                })
                .collect();
            self.save_ladder(task, &ladder).await;
        } else {
            candidates = order_by_ladder(candidates, &ladder);
        }
        let tried = |candidate: &Candidate| {
            ladder.iter().any(|rung| {
                rung.identifier == candidate.identifier
                    && rung.format == candidate.format
                    && rung.outcome.is_some()
            })
        };
        let queue: Vec<Candidate> = candidates.into_iter().filter(|c| !tried(c)).collect();
        if queue.is_empty() {
            return Err(NO_REMAINING_SOURCE);
        }
        let mut last = NO_FILES;
        for candidate in queue {
            if cancel.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let started = self
                .store
                .start_candidate(
                    &task.id,
                    &candidate.identifier,
                    &candidate.licence_url,
                    candidate.extension,
                    i64::try_from(candidate.filenames.len()).unwrap_or(i64::MAX),
                    candidate.size_bytes,
                )
                .await;
            if !matches!(started, Ok(true)) {
                return Ok(None);
            }
            self.publish(&task.user_id, &task.id, status::DOWNLOADING);
            let files = match self.fetch(task, &candidate, cancel, dest).await {
                Ok(Some(files)) => files,
                Ok(None) => return Ok(None),
                Err(error) => {
                    tracing::warn!(task_id = %task.id, %error, "free music download failed");
                    return Err(DOWNLOAD_FAILED);
                }
            };
            if files.is_empty() {
                remove_dir(dest).await;
                continue;
            }
            let tier = probe_tier(files.clone()).await;
            let fits = tier
                .as_deref()
                .is_some_and(|tier| in_range(tier, &settings.quality_min, &settings.quality_max));
            if !fits {
                tracing::info!(task_id = %task.id, format = %candidate.format, ?tier, "free music copy outside the quality band");
                mark(&mut ladder, &candidate, TRIED_QUALITY);
                self.save_ladder(task, &ladder).await;
                last = QUALITY_MISMATCH;
                remove_dir(dest).await;
                continue;
            }
            mark(&mut ladder, &candidate, TRIED_COMPLETED);
            self.save_ladder(task, &ladder).await;
            return Ok(Some(files));
        }
        if cancel.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Err(last)
    }

    /// Candidates for one task, best first.
    async fn candidates(
        &self,
        task: &TaskRecord,
        track_count: i64,
        settings: &FreeMusicSettings,
    ) -> Result<Vec<Candidate>, ArchiveError> {
        let items = self.archive.search(&task.artist, &task.title).await?;
        let mut out = Vec::new();
        for item in items {
            if title_containment(&task.title, &item.title) < TITLE_MATCH_FLOOR {
                continue;
            }
            let (licence, files) = self.archive.files(&item.identifier).await?;
            if licence.is_empty() || files.is_empty() {
                continue;
            }
            let mut by_format: HashMap<String, Vec<ArchiveFile>> = HashMap::new();
            for file in files {
                by_format.entry(file.format.clone()).or_default().push(file);
            }
            for (format, entries) in by_format {
                let extension = extension_for(&format);
                if extension.is_empty() {
                    continue;
                }
                // A lossless format can never sit under a band that tops
                // out below lossless; skip it before spending bytes.
                if extension == "flac"
                    && !in_range("lossless", &settings.quality_min, &settings.quality_max)
                {
                    continue;
                }
                let chosen = select_files(task, entries);
                if chosen.is_empty() {
                    continue;
                }
                out.push(Candidate {
                    identifier: item.identifier.clone(),
                    licence_url: licence.clone(),
                    format,
                    extension,
                    size_bytes: chosen.iter().map(|file| file.size_bytes).sum(),
                    sizes: chosen.iter().map(|file| file.size_bytes).collect(),
                    filenames: chosen.into_iter().map(|file| file.name).collect(),
                });
            }
        }
        out.sort_by_key(|candidate| rank(candidate, track_count, settings));
        Ok(out)
    }

    /// Tracks the album should have, for ranking sources: the stored count,
    /// else the tracklist of the edition the library keeps (or, for an
    /// album it does not hold, the group's best-known release). Zero when
    /// MusicBrainz cannot say.
    async fn track_count(&self, task: &TaskRecord) -> i64 {
        if task.track_count > 0 || task.kind != "album" {
            return task.track_count;
        }
        let Some(library) = self.drops.library_port() else {
            return 0;
        };
        let group = task.release_group_mbid.as_deref().unwrap_or(&task.mbid);
        let asked = task.release_mbid.clone().filter(|id| !id.is_empty());
        let chosen = match asked {
            Some(asked) => Some(asked),
            None => library.chosen_edition(group).await,
        };
        let release = match chosen {
            Some(edition) => Some(edition),
            None => match library.group_releases(group).await {
                Ok(hits) => hits
                    .into_iter()
                    .max_by_key(|hit| hit.score)
                    .map(|hit| hit.id),
                Err(error) => {
                    tracing::warn!(task_id = %task.id, error = %error.0, "album tracklist unreadable; ranking without a track count");
                    None
                }
            },
        };
        let Some(release) = release else {
            return 0;
        };
        match library.release(&release).await {
            Ok(Some(release)) => i64::try_from(release.tracks.len()).unwrap_or(0),
            Ok(None) => 0,
            Err(error) => {
                tracing::warn!(task_id = %task.id, error = %error.0, "album tracklist unreadable; ranking without a track count");
                0
            }
        }
    }

    /// Download one candidate's files, one retry on a transient failure.
    async fn fetch(
        &self,
        task: &TaskRecord,
        candidate: &Candidate,
        cancel: &AtomicBool,
        dest: &Path,
    ) -> Result<Option<Vec<PathBuf>>, ArchiveError> {
        let mut last = None;
        for attempt in 1..=2 {
            if !matches!(self.store.attempt(&task.id, attempt).await, Ok(true)) {
                return Ok(None);
            }
            match self.fetch_once(task, candidate, cancel, dest).await {
                Ok(files) => return Ok(files),
                Err(error) => {
                    tracing::info!(task_id = %task.id, attempt, %error, "free music download attempt failed");
                    remove_dir(dest).await;
                    if cancel.load(Ordering::SeqCst) {
                        return Ok(None);
                    }
                    last = Some(error);
                }
            }
        }
        Err(last.unwrap_or_else(|| ArchiveError::Unusable("download failed".into())))
    }

    async fn fetch_once(
        &self,
        task: &TaskRecord,
        candidate: &Candidate,
        cancel: &AtomicBool,
        dest: &Path,
    ) -> Result<Option<Vec<PathBuf>>, ArchiveError> {
        let local =
            |error: std::io::Error| ArchiveError::Unusable(format!("local write failed: {error}"));
        tokio::fs::create_dir_all(dest).await.map_err(local)?;
        let mut written = Vec::with_capacity(candidate.filenames.len());
        let mut bytes: i64 = 0;
        let mut last_write = Instant::now();
        for (index, name) in candidate.filenames.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                return Ok(None);
            }
            // Keep the item's own folders (CD1/01.flac, CD2/01.flac), each
            // part made safe; a name that tries to climb out is flattened.
            let target = dest.join(relative_target(name, index, candidate.extension));
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(local)?;
            }
            let declared = candidate.sizes.get(index).copied().unwrap_or(0).max(0);
            let cap = if declared > 0 {
                declared + declared / 10 + (1 << 20)
            } else {
                MAX_UNDECLARED_FILE_BYTES
            };
            let mut file_bytes: i64 = 0;
            let mut file = tokio::fs::File::create(&target).await.map_err(local)?;
            let mut chunks = self.archive.open(&candidate.identifier, name).await?;
            while let Some(chunk) = chunks.next().await? {
                if cancel.load(Ordering::SeqCst) {
                    return Ok(None);
                }
                let size = i64::try_from(chunk.len()).unwrap_or(i64::MAX);
                file_bytes = file_bytes.saturating_add(size);
                if file_bytes > cap {
                    return Err(ArchiveError::Unusable(format!(
                        "{name} is larger than the Internet Archive says it is"
                    )));
                }
                file.write_all(&chunk).await.map_err(local)?;
                bytes = bytes.saturating_add(size);
                if last_write.elapsed().as_secs_f64() >= PROGRESS_EVERY_SECS {
                    last_write = Instant::now();
                    let files = i64::try_from(index).unwrap_or(0);
                    if !matches!(self.store.progress(&task.id, bytes, files).await, Ok(true)) {
                        return Ok(None);
                    }
                    self.publish(&task.user_id, &task.id, status::DOWNLOADING);
                }
            }
            file.flush().await.map_err(local)?;
            written.push(target);
        }
        let files = i64::try_from(written.len()).unwrap_or(0);
        if !matches!(self.store.progress(&task.id, bytes, files).await, Ok(true)) {
            return Ok(None);
        }
        Ok(Some(written))
    }

    async fn save_ladder(&self, task: &TaskRecord, ladder: &[Rung]) {
        let json = serde_json::to_string(ladder).unwrap_or_else(|_| "[]".to_owned());
        if let Err(error) = self.store.set_ladder(&task.id, json).await {
            tracing::warn!(task_id = %task.id, %error, "free music source ladder not saved");
        }
    }

    async fn advance(
        &self,
        task: &TaskRecord,
        from: &'static [&'static str],
        to: &'static str,
    ) -> bool {
        match self.store.advance(&task.id, from, to).await {
            Ok(true) => {
                self.publish(&task.user_id, &task.id, to);
                true
            }
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "free music status not saved");
                false
            }
        }
    }

    async fn fail(&self, task: &TaskRecord, failure: Failure) {
        match self
            .store
            .fail(&task.id, failure.message, failure.code, failure.action)
            .await
        {
            Ok(true) => self.publish(&task.user_id, &task.id, status::FAILED),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "free music failure not saved")
            }
        }
    }

    fn publish(&self, user_id: &str, task_id: &str, state: &str) {
        self.events.notify(
            user_id,
            UserNotice::FreeMusicUpdated(FreeMusicUpdated {
                event_id: new_event_id(),
                task_id: task_id.to_owned(),
                status: state.to_owned(),
            }),
        );
    }
}

/// The request dispatch's view of Free Music.
impl crate::acquire::dispatch::FreeMusicRoute for FreeMusic {
    fn takes_requests(&self) -> bool {
        FreeMusic::takes_requests(self)
    }

    fn start(self: Arc<Self>, ask: FreeMusicAsk) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async move {
            FreeMusic::start(&self, ask)
                .await
                .map_err(|error| format!("free music did not start: {error:?}"))
        })
    }

    fn status<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(self.status_of(task_id))
    }

    fn cancel<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let task = self.store.get(task_id).await.map_err(|error| error.0)?;
            if let Some(task) = task {
                self.stop(&task)
                    .await
                    .map_err(|error| format!("free music cancel failed: {error:?}"))?;
            }
            Ok(())
        })
    }
}

/// Where one Archive file lands under the download folder: its own
/// relative path with every part made safe, or a numbered name when the
/// path tries to leave the folder.
fn relative_target(name: &str, index: usize, extension: &str) -> PathBuf {
    use std::path::Component;
    let path = Path::new(name);
    if path
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        path.components()
            .map(|part| {
                super::drop_import::extract::safe_component(&part.as_os_str().to_string_lossy())
            })
            .collect()
    } else {
        PathBuf::from(format!("{index:03}.{extension}"))
    }
}

/// An album takes every file of its format in track order; a track takes
/// the one file whose title matches best, if any clears the floor.
fn select_files(task: &TaskRecord, mut entries: Vec<ArchiveFile>) -> Vec<ArchiveFile> {
    if task.kind == "album" {
        entries.sort_by(|a, b| {
            a.track
                .unwrap_or(0)
                .cmp(&b.track.unwrap_or(0))
                .then_with(|| a.name.cmp(&b.name))
        });
        return entries;
    }
    let scored = entries.into_iter().map(|entry| {
        let title = if entry.title.is_empty() {
            entry.name.clone()
        } else {
            entry.title.clone()
        };
        (title_containment(&task.title, &title), entry)
    });
    scored
        .filter(|(score, _)| *score >= TITLE_MATCH_FLOOR)
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, entry)| vec![entry])
        .unwrap_or_default()
}

/// Lower sorts first: track-count agreement, the preferred format,
/// lossless before lossy, then the bigger (better) copy.
fn rank(
    candidate: &Candidate,
    track_count: i64,
    settings: &FreeMusicSettings,
) -> (i64, u8, u8, i64) {
    let files = i64::try_from(candidate.filenames.len()).unwrap_or(i64::MAX);
    let count_gap = if track_count > 0 {
        (files - track_count).abs()
    } else {
        0
    };
    let preferred = if settings.prefer_mp3 { "mp3" } else { "flac" };
    let format_step = u8::from(candidate.extension != preferred);
    let lossy = u8::from(candidate.extension != "flac");
    (count_gap, format_step, lossy, -candidate.size_bytes)
}

/// Keep the stored ladder's order; a fresh search never reorders it.
fn order_by_ladder(candidates: Vec<Candidate>, ladder: &[Rung]) -> Vec<Candidate> {
    let mut pool = candidates;
    let mut ordered = Vec::new();
    for rung in ladder {
        if let Some(at) = pool
            .iter()
            .position(|c| c.identifier == rung.identifier && c.format == rung.format)
        {
            ordered.push(pool.remove(at));
        }
    }
    ordered
}

fn mark(ladder: &mut Vec<Rung>, candidate: &Candidate, outcome: &str) {
    match ladder
        .iter_mut()
        .find(|rung| rung.identifier == candidate.identifier && rung.format == candidate.format)
    {
        Some(rung) => rung.outcome = Some(outcome.to_owned()),
        None => ladder.push(Rung {
            identifier: candidate.identifier.clone(),
            format: candidate.format.clone(),
            outcome: Some(outcome.to_owned()),
        }),
    }
}

/// The worst quality tier among downloaded files, read from the files.
async fn probe_tier(files: Vec<PathBuf>) -> Option<String> {
    let landing = tokio::task::spawn_blocking(move || super::landing::probe::probe(&files, &[]))
        .await
        .ok()?;
    if landing.audio.is_empty() {
        return None;
    }
    worst(landing.audio.iter().map(|file| file.tier())).map(str::to_owned)
}

async fn remove_dir(dir: &Path) {
    if let Err(error) = tokio::fs::remove_dir_all(dir).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), %error, "free music download folder not removed");
    }
}

/// Token containment: the share of the requested title's words found in
/// the candidate's title (v2 `title_containment_score`).
pub fn title_containment(requested: &str, candidate: &str) -> f64 {
    let words = |text: &str| -> Vec<String> {
        text.to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let wanted = words(requested);
    if wanted.is_empty() {
        return 0.0;
    }
    let have = words(candidate);
    let hits = wanted.iter().filter(|word| have.contains(word)).count();
    hits as f64 / wanted.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(extension: &'static str, files: usize, size: i64) -> Candidate {
        Candidate {
            identifier: format!("{extension}-{files}-{size}"),
            licence_url: String::new(),
            format: extension.to_owned(),
            extension,
            size_bytes: size,
            filenames: vec![String::new(); files],
            sizes: vec![0; files],
        }
    }

    #[test]
    fn ranking_puts_track_count_then_format_first() {
        let settings = FreeMusicSettings {
            enabled: true,
            prefer_mp3: false,
            quality_min: "low".into(),
            quality_max: "lossless".into(),
        };
        let mut all = [
            candidate("mp3", 10, 900),
            candidate("flac", 9, 5000),
            candidate("flac", 10, 4000),
            candidate("flac", 10, 4500),
        ];
        all.sort_by_key(|candidate| rank(candidate, 10, &settings));
        let order: Vec<&str> = all.iter().map(|c| c.identifier.as_str()).collect();
        assert_eq!(
            order,
            ["flac-10-4500", "flac-10-4000", "mp3-10-900", "flac-9-5000"]
        );
        assert!((title_containment("Free Album", "free album (live)") - 1.0).abs() < 1e-9);
        assert!(title_containment("Free Album", "Something Else") < TITLE_MATCH_FLOOR);
    }
}
