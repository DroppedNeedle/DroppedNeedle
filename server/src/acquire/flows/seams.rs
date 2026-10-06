//! Seams behind the acquisition flows: the boundary to code they do not own.
//!
//! Downloads dispatch and provider/indexer search live elsewhere (the
//! orchestrator and clients, and the search fan-out). The flows reach them
//! only through the traits here, so real implementations plug in without
//! touching flow logic. Every fallible method
//! returns a plain string cause, the playback-ports convention: provider
//! detail stays in the log, never on the wire.
//!
//! The scripted doubles (`ScriptedDownloads`, `ScriptedSearch`,
//! `ScriptedPoll`, and friends) are test stand-ins, not production
//! code. Time flows through [`Clock`] so cadence tests pin a [`ManualClock`]
//! instead of sleeping.

use std::sync::Mutex;
#[cfg(any(test, feature = "test-support"))]
use std::{collections::HashMap, sync::Arc};

use futures_util::future::BoxFuture;

/// Clock seam. Loops and ops read time through this so tests pin it.
pub trait Clock: Send + Sync {
    /// Current unix timestamp in seconds.
    fn now_unix(&self) -> i64;
}

/// System clock for production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Manual clock for tests: time moves only when a test advances it.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    now: Arc<Mutex<i64>>,
}

#[cfg(any(test, feature = "test-support"))]
impl ManualClock {
    /// Pin the clock at `at` (unix seconds).
    pub fn new(at: i64) -> Self {
        Self {
            now: Arc::new(Mutex::new(at)),
        }
    }

    /// Move the clock forward by `secs`.
    pub fn advance(&self, secs: i64) {
        if let Ok(mut now) = self.now.lock() {
            *now = now.saturating_add(secs);
        }
    }

    /// Jump the clock to `at`.
    pub fn set(&self, at: i64) {
        if let Ok(mut now) = self.now.lock() {
            *now = at;
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Clock for ManualClock {
    fn now_unix(&self) -> i64 {
        self.now.lock().map(|now| *now).unwrap_or(0)
    }
}

/// What kind of request a dispatch serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchKind {
    /// Whole album, keyed by release-group MBID.
    Album,
    /// Single track, keyed by recording MBID.
    Track,
}

/// One download dispatch: the orchestrator owns the queue behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRequest {
    /// Owning user id.
    pub user_id: String,
    /// Album or track.
    pub kind: DispatchKind,
    /// Release-group MBID (album) or recording MBID (track).
    pub mbid: String,
    /// Artist name for the task row.
    pub artist: String,
    /// Album or track title for the task row.
    pub title: String,
    /// Origin tag: `user`, `wanted`, `follow`, `upgrade`, `free-music`.
    pub origin: String,
    /// Caller-supplied idempotency key. A repeat dispatch with the same
    /// key answers the original task instead of minting a duplicate;
    /// `None` always mints fresh.
    pub idempotency_key: Option<String>,
}

/// One download task as the status sync sees it. `status` is one of
/// `downloading`, `processing`, `completed`, `partial`, `failed`,
/// `cancelled` (v2 `download_task.status`, `requests_page_service.py`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTaskView {
    /// Task id.
    pub task_id: String,
    /// Raw task status.
    pub status: String,
    /// Release-group MBID for album tasks, when known.
    pub album_mbid: Option<String>,
}

/// Outcome of an upgrade dispatch. `AlreadyInLibrary` answers the
/// `already_in_library` sentinel from v2 `request_upgrade_album`
/// (`core/tasks.py`): the sweep must not count it as enqueued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeDispatch {
    /// A new upgrade grab, with its task id.
    Enqueued(String),
    /// The album already meets cutoff; nothing was queued.
    AlreadyInLibrary,
}

/// Downloads dispatch seam, implemented by the orchestrator.
pub trait DownloadDispatch: Send + Sync {
    /// Queue a download; answers the new task id.
    fn dispatch<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<String, String>>;
    /// Current status of one task, or `None` when unknown.
    fn task_status<'a>(&'a self, task_id: &'a str)
    -> BoxFuture<'a, Result<Option<String>, String>>;
    /// Newest active task for an album across users, for the album-row
    /// fallback in the status sync.
    fn active_task_for_album<'a>(
        &'a self,
        rg_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<DownloadTaskView>, String>>;
    /// Queue an origin-`upgrade` grab with active-task dedup.
    fn dispatch_upgrade<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<UpgradeDispatch, String>>;
}

/// Scripted downloads double: statuses are pinned per task id, dispatches
/// are recorded for assertions.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ScriptedDownloads {
    inner: Mutex<ScriptedDownloadsInner>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct ScriptedDownloadsInner {
    dispatched: Vec<DispatchRequest>,
    upgrades: Vec<DispatchRequest>,
    statuses: HashMap<String, String>,
    album_tasks: HashMap<String, DownloadTaskView>,
    library: Vec<String>,
    next_id: u64,
    fail_dispatch: Option<String>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedDownloads {
    /// Fresh double with no tasks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin the status a task id reports.
    pub fn set_status(&self, task_id: &str, status: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.statuses.insert(task_id.to_owned(), status.to_owned());
        }
    }

    /// Pin the album-row fallback answer for one release group.
    pub fn set_album_task(&self, rg_mbid: &str, task: DownloadTaskView) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.album_tasks.insert(rg_mbid.to_owned(), task);
        }
    }

    /// Mark a release group as already meeting cutoff.
    pub fn mark_in_library(&self, rg_mbid: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.library.push(rg_mbid.to_owned());
        }
    }

    /// Fail every future dispatch with `cause`.
    pub fn fail_dispatches(&self, cause: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.fail_dispatch = Some(cause.to_owned());
        }
    }

    /// Dispatches recorded so far, in order.
    pub fn dispatched(&self) -> Vec<DispatchRequest> {
        self.inner
            .lock()
            .map(|inner| inner.dispatched.clone())
            .unwrap_or_default()
    }

    /// Upgrade dispatches recorded so far, in order.
    pub fn upgrades(&self) -> Vec<DispatchRequest> {
        self.inner
            .lock()
            .map(|inner| inner.upgrades.clone())
            .unwrap_or_default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedDownloads {
    fn dispatch_now(&self, request: &DispatchRequest) -> Result<String, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "downloads lock lost".to_owned())?;
        if let Some(cause) = inner.fail_dispatch.clone() {
            return Err(cause);
        }
        inner.next_id += 1;
        let task_id = format!("task-{}", inner.next_id);
        inner
            .statuses
            .insert(task_id.clone(), "downloading".to_owned());
        inner.dispatched.push(request.clone());
        Ok(task_id)
    }

    fn upgrade_now(&self, request: &DispatchRequest) -> Result<UpgradeDispatch, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "downloads lock lost".to_owned())?;
        if inner.library.iter().any(|mbid| mbid == &request.mbid) {
            return Ok(UpgradeDispatch::AlreadyInLibrary);
        }
        inner.next_id += 1;
        let task_id = format!("upgrade-{}", inner.next_id);
        inner
            .statuses
            .insert(task_id.clone(), "downloading".to_owned());
        inner.upgrades.push(request.clone());
        Ok(UpgradeDispatch::Enqueued(task_id))
    }
}

#[cfg(any(test, feature = "test-support"))]
impl DownloadDispatch for ScriptedDownloads {
    fn dispatch<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { self.dispatch_now(request) })
    }

    fn task_status<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            Ok(self
                .inner
                .lock()
                .ok()
                .and_then(|inner| inner.statuses.get(task_id).cloned()))
        })
    }

    fn active_task_for_album<'a>(
        &'a self,
        rg_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<DownloadTaskView>, String>> {
        Box::pin(async move {
            Ok(self
                .inner
                .lock()
                .ok()
                .and_then(|inner| inner.album_tasks.get(rg_mbid).cloned()))
        })
    }

    fn dispatch_upgrade<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<UpgradeDispatch, String>> {
        Box::pin(async move { self.upgrade_now(request) })
    }
}

/// One provider/indexer candidate for a wanted album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Release title as the provider reports it.
    pub title: String,
    /// Provider or indexer name.
    pub source: String,
}

/// Provider/indexer search seam, implemented by the search fan-out.
/// Async like the downloads `DownloadSource` seam: production fans out to
/// slskd and the Usenet indexers over HTTP.
pub trait CandidateSearch: Send + Sync {
    /// Search providers for an album's candidates.
    fn search_album<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>>;
}

/// Scripted search double: pinned candidates per `artist\x00title` key.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ScriptedSearch {
    inner: Mutex<ScriptedSearchInner>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct ScriptedSearchInner {
    candidates: HashMap<String, Result<Vec<Candidate>, String>>,
    queries: Vec<(String, String)>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedSearch {
    /// Fresh double answering no candidates everywhere.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin the answer for one album query.
    pub fn set(&self, artist: &str, title: &str, answer: Result<Vec<Candidate>, String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .candidates
                .insert(format!("{artist}\x00{title}"), answer);
        }
    }

    /// Queries seen so far, in order.
    pub fn queries(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .map(|inner| inner.queries.clone())
            .unwrap_or_default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl CandidateSearch for ScriptedSearch {
    fn search_album<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "search lock lost".to_owned())?;
            inner.queries.push((artist.to_owned(), title.to_owned()));
            inner
                .candidates
                .get(&format!("{artist}\x00{title}"))
                .cloned()
                .unwrap_or(Ok(Vec::new()))
        })
    }
}

/// One release row observed on a provider page for a followed artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedRelease {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Release title.
    pub title: String,
    /// First release date (`YYYY-MM-DD` when complete).
    pub first_release_date: Option<String>,
    /// MusicBrainz primary type (Album, Single, EP, ...).
    pub primary_type: Option<String>,
    /// MusicBrainz secondary types (Live, Compilation, ...).
    pub secondary_types: Vec<String>,
}

/// Follow-poll provider seam: one artist's release-group page.
/// Async: production reads the provider page over HTTP.
pub trait ReleasePoll: Send + Sync {
    /// Fetch every release group of the artist, in any order.
    fn poll_releases<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>>;
}

/// Scripted poll double: pinned pages per artist MBID.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ScriptedPoll {
    inner: Mutex<ScriptedPollInner>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct ScriptedPollInner {
    pages: HashMap<String, Result<Vec<ObservedRelease>, String>>,
    polls: Vec<String>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedPoll {
    /// Fresh double answering empty pages everywhere.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin one artist's page.
    pub fn set(&self, artist_mbid: &str, answer: Result<Vec<ObservedRelease>, String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pages.insert(artist_mbid.to_owned(), answer);
        }
    }

    /// Artists polled so far, in order.
    pub fn polls(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|inner| inner.polls.clone())
            .unwrap_or_default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ReleasePoll for ScriptedPoll {
    fn poll_releases<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().map_err(|_| "poll lock lost".to_owned())?;
            inner.polls.push(artist_mbid.to_owned());
            inner
                .pages
                .get(artist_mbid)
                .cloned()
                .unwrap_or(Ok(Vec::new()))
        })
    }
}

/// One durable tick. v2 fired plugin ticks as fire-and-forget asyncio tasks
/// (`_emit_plugin_event` in `drop_import_service.py`, `wanted_watcher_service.py`);
/// v3 emits each tick at its durable state transition, beside the registry
/// heartbeat and the store write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    /// Tick kind (`request_fulfilled`, `drop_import.resolved`, ...).
    pub kind: String,
    /// Human-readable detail for logs and tests.
    pub detail: String,
    /// Unix seconds when the tick fired.
    pub at: i64,
}

/// Durable tick sink. Production logs each tick, forwards the outcomes
/// plugins care about and sends user notices to the live event stream
/// (`acquire::plugin_events::FlowTicks`); tests read them back.
pub trait TickSink: Send + Sync {
    /// Record one tick.
    fn emit(&self, kind: &str, detail: &str, at: i64);

    /// Announce one flow outcome. The default drops it; the production
    /// sink forwards it to the plugin host or the event stream.
    fn announce(&self, event: FlowEvent) {
        let _ = event;
    }
}

/// A flow outcome announced to plugin subscribers or to a user's open
/// tabs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowEvent {
    /// Tell one user's open tabs (`wanted_*`, `auto_download_enqueued`,
    /// `request_imported`).
    Notify {
        /// Who to tell.
        user_id: String,
        /// What happened.
        notice: crate::events::UserNotice,
    },
    /// A request reached the library.
    RequestFulfilled {
        /// Request key (the release-group MBID for albums).
        request_id: String,
        /// Requesting user, when the request has one.
        user_id: String,
        /// Release-group MBID.
        release_group_mbid: String,
    },
    /// An import put files in the library.
    ImportFinished {
        /// Release-group MBID, when known.
        release_group_mbid: String,
        /// Files imported.
        track_count: i64,
        /// Where the files came from (`drop_import`).
        source: String,
    },
}

/// Memory tick sink recording every tick in order (tests).
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryTicks {
    ticks: Mutex<Vec<Tick>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryTicks {
    /// Fresh sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ticks recorded so far, in order.
    pub fn ticks(&self) -> Vec<Tick> {
        self.ticks
            .lock()
            .map(|ticks| ticks.clone())
            .unwrap_or_default()
    }

    /// Ticks of one kind, in order.
    pub fn of_kind(&self, kind: &str) -> Vec<Tick> {
        self.ticks
            .lock()
            .map(|ticks| {
                ticks
                    .iter()
                    .filter(|tick| tick.kind == kind)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl TickSink for MemoryTicks {
    fn emit(&self, kind: &str, detail: &str, at: i64) {
        if let Ok(mut ticks) = self.ticks.lock() {
            ticks.push(Tick {
                kind: kind.to_owned(),
                detail: detail.to_owned(),
                at,
            });
        }
    }
}

/// Handoff seam: landed free-music files enter the drop-import pipeline
/// (v2 `FreeMusicService` hands downloads to `DropImportService`, 01c).
/// Async: production adopts the files into a durable drop-import job.
pub trait LandedHandoff: Send + Sync {
    /// Adopt landed files as a drop-import job; answers the job id.
    fn land<'a>(
        &'a self,
        op_id: &'a str,
        user_id: &'a str,
        files: &'a [String],
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// Memory handoff recording every landing.
#[derive(Debug, Default)]
pub struct MemoryHandoff {
    landings: Mutex<Vec<(String, String, Vec<String>)>>,
}

impl MemoryHandoff {
    /// Fresh handoff.
    pub fn new() -> Self {
        Self::default()
    }

    /// Landings recorded so far: `(op_id, user_id, files)`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn landings(&self) -> Vec<(String, String, Vec<String>)> {
        self.landings
            .lock()
            .map(|landings| landings.clone())
            .unwrap_or_default()
    }
}

impl LandedHandoff for MemoryHandoff {
    fn land<'a>(
        &'a self,
        op_id: &'a str,
        user_id: &'a str,
        files: &'a [String],
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let mut landings = self
                .landings
                .lock()
                .map_err(|_| "handoff lock lost".to_owned())?;
            let job_id = format!("drop-{}", landings.len() + 1);
            landings.push((op_id.to_owned(), user_id.to_owned(), files.to_vec()));
            Ok(job_id)
        })
    }
}

/// Verdict on one dropped file. Only `BadSource` quarantines: the bytes came
/// from a bad source. `LocalFault` covers our own failures (destination
/// occupied, truncated copy, missing file) and must never quarantine the
/// source (v2 `file_processor.py` non-quarantine reasons).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyVerdict {
    /// The file is good; resolve it.
    Ok,
    /// The source is bad; quarantine with this reason.
    BadSource(String),
    /// Our side failed; fail the item without quarantining.
    LocalFault(String),
}

/// Drop-file verify seam. The library engine owns identification; the flow only
/// needs the verdict shape to route quarantine correctly.
pub trait DropVerify: Send + Sync {
    /// Verify one staged file.
    fn verify(&self, file_name: &str) -> VerifyVerdict;
}

/// Scripted verify double: pinned verdicts per file name, `Ok` by default.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ScriptedVerify {
    verdicts: Mutex<HashMap<String, VerifyVerdict>>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedVerify {
    /// Fresh double verifying everything as good.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin one file's verdict.
    pub fn set(&self, file_name: &str, verdict: VerifyVerdict) {
        if let Ok(mut verdicts) = self.verdicts.lock() {
            verdicts.insert(file_name.to_owned(), verdict);
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl DropVerify for ScriptedVerify {
    fn verify(&self, file_name: &str) -> VerifyVerdict {
        self.verdicts
            .lock()
            .ok()
            .and_then(|verdicts| verdicts.get(file_name).cloned())
            .unwrap_or(VerifyVerdict::Ok)
    }
}

/// Library organise seam. The library engine owns naming and placement; the flow only
/// needs the final path to finish the resolve.
pub trait LibraryOrganise: Send + Sync {
    /// Move one staged file into the library; answers the final path.
    fn organise(&self, job_id: &str, staged_path: &str) -> Result<String, String>;
}

/// Memory organise double recording placements.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryOrganise {
    placements: Mutex<Vec<(String, String)>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryOrganise {
    /// Fresh double.
    pub fn new() -> Self {
        Self::default()
    }

    /// Placements recorded so far: `(job_id, staged_path)`.
    pub fn placements(&self) -> Vec<(String, String)> {
        self.placements
            .lock()
            .map(|placements| placements.clone())
            .unwrap_or_default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl LibraryOrganise for MemoryOrganise {
    fn organise(&self, job_id: &str, staged_path: &str) -> Result<String, String> {
        let mut placements = self
            .placements
            .lock()
            .map_err(|_| "organise lock lost".to_owned())?;
        placements.push((job_id.to_owned(), staged_path.to_owned()));
        Ok(format!("/library/{job_id}/{}", file_name_of(staged_path)))
    }
}

/// Base name of a path, for the fake library layout.
#[cfg(any(test, feature = "test-support"))]
fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
