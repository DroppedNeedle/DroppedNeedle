//! Folder watcher, work wakeups, dirty scopes.
//!
//! The watcher walks every library root once per poll interval the way a
//! scan does ([`FolderRecord`]: stat only, links never followed, the
//! publisher's files skipped) and keeps one digest per folder. A full read
//! lists every folder and stats every file; it runs at a root's first pass
//! and every 6 hours after. The passes between are quick: they `lstat`
//! each folder on record and list again only those whose own times or
//! inode moved, or whose times were too close to the read that recorded
//! them to tell a later change apart. A root being scanned, or an
//! excluded root with no included folder, is not walked; an excluded root
//! is walked only below its included folders. The wait between passes is
//! at least 20 times the last quick pass.
//!
//! When folders move it asks, after a batching window so a burst collapses
//! into one request, for an incremental scan of just those folders. A
//! folder gone or holding fewer music files asks for the whole root. The
//! watcher only asks: it never judges a file gone. The digests it compares
//! against are kept in `library_folder_digests` and written once the scan
//! it asked for has settled, so a restart neither rescans a steady tree
//! nor forgets a change the scan has not taken in yet.
//!
//! Accepted limitation: a quick pass misses a file rewritten in place
//! (a tagger saving into the file, a copy over it), which moves no
//! folder's times, and any change on a filesystem that does not move a
//! folder's times (some network, FUSE and pooled shares). The next full
//! read, at most 6 hours later, or the scheduled scan picks it up. A
//! change that keeps both a file's mtime and size is seen by neither and
//! surfaces only through the scheduled scan or a manual one.
//!
//! This module also owns [`WorkWakeups`] (the work revisions the loops
//! wake on) and [`DirtyScopes`] (Hook B marks left by
//! settings saves).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use droppedneedle_kernel::jobs::CancellationToken;
use tokio::sync::watch;

use super::models::scope_covers_path;
use super::models::{Outcome, RunState, ScanKind, ScanRequest, ScanScope, ScanTrigger};
use super::pool::BlockingPool;
use super::roots::RootRegistry;
use super::scheduler::{InclusionRule, scheduled_scopes};
use super::sqlite_store::SqliteScanStore;
use super::stop::RunStop;
use super::store::ScanStoreError;
use super::walk::{FolderDigest, FolderRecord};

/// Default poll interval.
pub const DEFAULT_POLL_INTERVAL_SECS: f64 = 300.0;
/// Default batching window.
pub const DEFAULT_BATCH_WINDOW_SECS: f64 = 60.0;
/// Minimum poll interval; lower values clamp.
pub const MIN_POLL_INTERVAL_SECS: f64 = 1.0;
/// How often each root gets a full read, which stats every file.
pub const FULL_READ_EVERY_SECS: f64 = 6.0 * 3600.0;
/// The wait between passes is at least this many quick passes long.
pub const QUICK_PASS_GAP_FACTOR: f64 = 20.0;

/// The wait before the next pass: the poll interval, or 20 times the last
/// quick pass when that is longer, so a slow share is never kept busy.
pub fn gap(poll_interval_seconds: f64, last_quick_secs: f64) -> f64 {
    poll_interval_seconds
        .max(MIN_POLL_INTERVAL_SECS)
        .max(QUICK_PASS_GAP_FACTOR * last_quick_secs)
}

/// True when a root's next pass is a full read: its record started
/// elsewhere (its included folders changed), or 6 hours have passed since
/// its last full read, or the clock reads earlier than that read.
fn full_read_due(record_starts: &[String], full_at: f64, starts: &[String], now: f64) -> bool {
    record_starts != starts || now - full_at >= FULL_READ_EVERY_SECS || now < full_at
}

/// Watcher settings: enabled flag, poll interval, batch window.
#[derive(Debug, Clone)]
pub struct WatcherSettings {
    pub enabled: bool,
    pub poll_interval_seconds: f64,
    pub batch_window_seconds: f64,
}

impl Default for WatcherSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECS,
            batch_window_seconds: DEFAULT_BATCH_WINDOW_SECS,
        }
    }
}

/// Work wakeups: one revision per kind of work ("scan", "identification")
/// that the loops wake on. `notify` moves it, from any thread.
#[derive(Debug, Clone, Default)]
pub struct WorkWakeups {
    kinds: Arc<Mutex<HashMap<String, watch::Sender<u64>>>>,
}

impl WorkWakeups {
    pub fn new() -> Self {
        Self::default()
    }

    fn sender(&self, kind: &str) -> watch::Sender<u64> {
        self.kinds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(kind.to_owned())
            .or_insert_with(|| watch::channel(0).0)
            .clone()
    }

    /// Say that work of `kind` arrived.
    pub fn notify(&self, kind: &str) {
        self.sender(kind)
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// The current revision of `kind`.
    pub fn revision(&self, kind: &str) -> u64 {
        *self.sender(kind).borrow()
    }

    /// A receiver that changes whenever work of `kind` arrives, for a loop
    /// to wake on.
    pub fn subscribe(&self, kind: &str) -> watch::Receiver<u64> {
        self.sender(kind).subscribe()
    }
}

/// Hook B dirty scope marks. Hints only: marks clear on a non-conflict
/// request, and a crash loses them safely since the next rolling scan
/// converges.
#[derive(Debug, Clone, Default)]
pub struct DirtyScopes {
    inner: Arc<Mutex<HashSet<String>>>,
}

impl DirtyScopes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark(&self, scope_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(scope_id.to_owned());
    }

    pub fn list(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear(&self, scope_ids: &[String]) {
        let mut marks = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for id in scope_ids {
            marks.remove(id);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }
}

/// Folder digests of one root, keyed by the folder's path in the root.
pub type Digests = HashMap<String, FolderDigest>;

/// Most folders one root names in a request; past it the watcher asks for
/// the whole root, which is one walk instead of many.
pub const MAX_FOLDER_SCOPES: usize = 100;

/// What the watcher knows of one root.
#[derive(Debug)]
struct RootWatch {
    path: PathBuf,
    /// The digests on record, as in the table.
    recorded: Digests,
    /// What the last walk found.
    seen: Digests,
    /// The run asked to scan this root's changes, and the digests it was
    /// asked for, recorded once it settles.
    asked: Option<(String, Digests)>,
    /// The last walk, folder by folder, which a quick pass starts from.
    /// In memory only.
    folders: FolderRecord,
    /// When the last full read ran, in seconds on the kernel clock.
    full_at: f64,
}

/// Watcher loop state: what each root looked like, plus the batch timer.
#[derive(Debug, Default)]
pub struct WatcherState {
    roots: HashMap<String, RootWatch>,
    pending_since: Option<f64>,
    /// How long the last poll's quick passes took, in seconds.
    last_quick_secs: f64,
}

impl WatcherState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_pending(&self) -> bool {
        self.pending_since.is_some()
    }

    /// The wait before the next pass under `settings`, from [`gap`].
    pub fn gap(&self, settings: &WatcherSettings) -> f64 {
        gap(settings.poll_interval_seconds, self.last_quick_secs)
    }
}

/// What one watcher iteration decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WatcherAction {
    /// No mutation pending; sleep the poll interval.
    Idle { sleep_secs: f64 },
    /// A mutation is batching; sleep until the window ends or the next poll.
    Batching { sleep_secs: f64 },
    /// The window elapsed; the caller should ask for the scan now.
    Due,
}

/// What changed in one root since its recorded digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changes {
    None,
    /// These folders, each new or holding at least as many music files as
    /// before, shallowest first.
    Folders(Vec<String>),
    /// A folder went away or lost music files. The loss is judged by a scan
    /// of the whole root, as the scan's guard against a share that dropped
    /// out weighs it against everything the scope holds.
    WholeRoot,
}

/// Compare a walk with the recorded digests.
pub fn changes(recorded: &Digests, seen: &Digests) -> Changes {
    if recorded.keys().any(|folder| !seen.contains_key(folder)) {
        return Changes::WholeRoot;
    }
    let mut folders = Vec::new();
    for (folder, now) in seen {
        match recorded.get(folder) {
            Some(before) if before == now => {}
            Some(before) if now.audio_files < before.audio_files => return Changes::WholeRoot,
            _ => folders.push(folder.clone()),
        }
    }
    if folders.is_empty() {
        return Changes::None;
    }
    folders.sort_by(|left, right| depth(left).cmp(&depth(right)).then(left.cmp(right)));
    Changes::Folders(folders)
}

/// Path depth in components; `.` is 0.
fn depth(folder: &str) -> usize {
    if folder == "." {
        0
    } else {
        folder.split('/').count()
    }
}

/// The row writes that turn `recorded` into `target`.
fn row_changes(recorded: &Digests, target: &Digests) -> Vec<(String, Option<FolderDigest>)> {
    let mut rows: Vec<(String, Option<FolderDigest>)> = target
        .iter()
        .filter(|(folder, digest)| recorded.get(*folder) != Some(digest))
        .map(|(folder, digest)| (folder.clone(), Some(*digest)))
        .collect();
    rows.extend(
        recorded
            .keys()
            .filter(|folder| !target.contains_key(*folder))
            .map(|folder| (folder.clone(), None)),
    );
    rows
}

/// The scopes that scan root `root_id`'s `changes`, inside what the
/// automatic schedule walks (`scheduled`). A folder inside another named
/// folder adds nothing.
pub fn change_scopes(
    registry: &RootRegistry,
    scheduled: &[ScanScope],
    root_id: &str,
    changes: &Changes,
) -> Vec<ScanScope> {
    let own = scheduled.iter().filter(|scope| scope.root_id == root_id);
    let folders = match changes {
        Changes::None => return Vec::new(),
        Changes::Folders(folders) if folders.len() <= MAX_FOLDER_SCOPES => folders,
        Changes::Folders(_) | Changes::WholeRoot => return own.cloned().collect(),
    };
    let root = registry.resolve(root_id);
    let mut scopes: Vec<ScanScope> = Vec::new();
    for folder in folders {
        if scopes
            .iter()
            .any(|scope| scope_covers_path(&scope.relative_path, folder))
        {
            continue;
        }
        // A folder the schedule does not walk (in an excluded root, outside
        // its included paths) is not scanned.
        let Some(home) = own
            .clone()
            .find(|scope| scope_covers_path(&scope.relative_path, folder))
        else {
            continue;
        };
        let mut scope = home.clone();
        if scope.relative_path != *folder {
            scope.relative_path = folder.clone();
            scope.estimated_count = None;
            if let Some(root) = root {
                scope.effective_policy = root.policy_for(&root.path.join(folder));
            }
        }
        scopes.push(scope);
    }
    scopes
}

/// Run a store call on the blocking pool.
async fn on_store<T: Send + 'static>(
    pool: &BlockingPool,
    store: &Arc<SqliteScanStore>,
    call: impl FnOnce(&SqliteScanStore) -> Result<T, ScanStoreError> + Send + 'static,
) -> Result<T, ScanStoreError> {
    let store = Arc::clone(store);
    pool.run(move || call(&store))
        .await
        .unwrap_or_else(|error| {
            Err(ScanStoreError::Internal {
                message: error.to_string(),
            })
        })
}

/// Record `target` as root `root_id`'s digests. True when it is on record.
async fn record(
    pool: &BlockingPool,
    store: &Arc<SqliteScanStore>,
    root_id: &str,
    recorded: &Digests,
    target: &Digests,
) -> bool {
    let rows = row_changes(recorded, target);
    if rows.is_empty() {
        return true;
    }
    let id = root_id.to_owned();
    match on_store(pool, store, move |store| {
        store.record_folder_digests(&id, &rows)
    })
    .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(root_id, %error, "folder watcher could not record folder digests; tries again next poll");
            false
        }
    }
}

/// The roots a queued or running scan names. A read that fails names
/// none, so every root is walked as usual.
async fn scanning_roots(pool: &BlockingPool, store: &Arc<SqliteScanStore>) -> HashSet<String> {
    let read = on_store(pool, store, |store| {
        let mut roots = HashSet::new();
        for run in store.list_current()? {
            match store.get_run(&run.id) {
                Ok((_, scopes, _)) => roots.extend(scopes.into_iter().map(|scope| scope.root_id)),
                // Ended and pruned since the list was read.
                Err(ScanStoreError::NotFound { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(roots)
    })
    .await;
    read.unwrap_or_else(|error| {
        tracing::warn!(%error, "folder watcher could not read the current scans; walks every library folder");
        HashSet::new()
    })
}

/// Where the watcher walks root `root_id`: the folders the schedule scans
/// there (`.` for the whole root). Empty for an excluded root with no
/// included folder.
fn walk_starts(scheduled: &[ScanScope], root_id: &str) -> Vec<String> {
    let mut starts: Vec<String> = scheduled
        .iter()
        .filter(|scope| scope.root_id == root_id)
        .map(|scope| scope.relative_path.clone())
        .collect();
    starts.sort();
    starts.dedup();
    starts
}

/// Put on record what the scan `watch` asked for once it has settled.
async fn settle(
    pool: &BlockingPool,
    store: &Arc<SqliteScanStore>,
    root_id: &str,
    watch: &mut RootWatch,
) {
    let Some((run_id, asked)) = watch.asked.take() else {
        return;
    };
    let id = run_id.clone();
    let settled = match on_store(pool, store, move |store| {
        store.reread_run(&RunStop::never(), &id)
    })
    .await
    {
        Ok((run, _, _)) => match run.state {
            // Done, or stopped by a person: on record either way.
            RunState::Ended {
                outcome: Outcome::Completed | Outcome::Cancelled,
                ..
            } => Some(true),
            // Failed or superseded: asked for again.
            RunState::Ended { .. } => Some(false),
            _ => None,
        },
        // Pruned from the history: it ended long ago.
        Err(ScanStoreError::NotFound { .. }) => Some(true),
        Err(error) => {
            tracing::warn!(root_id = %root_id, %error, "folder watcher could not read the scan it asked for");
            None
        }
    };
    match settled {
        Some(true) if record(pool, store, root_id, &watch.recorded, &asked).await => {
            watch.recorded = asked;
        }
        Some(false) => {}
        Some(true) | None => watch.asked = Some((run_id, asked)),
    }
}

/// True when `watch` saw changes and is not waiting on a scan of them.
fn wants_scan(watch: &RootWatch) -> bool {
    watch.asked.is_none() && changes(&watch.recorded, &watch.seen) != Changes::None
}

/// Poll every root once and update the batch timer, one iteration. The
/// caller asks for the scan on [`WatcherAction::Due`], through
/// [`ask_for_changes`]. `rules` are the excluded roots' included folders.
#[allow(clippy::too_many_arguments)]
pub async fn poll_once(
    state: &mut WatcherState,
    settings: &WatcherSettings,
    registry: &RootRegistry,
    rules: &[InclusionRule],
    root_paths: &HashMap<String, PathBuf>,
    pool: &BlockingPool,
    store: &Arc<SqliteScanStore>,
    now: f64,
    stop: &CancellationToken,
) -> WatcherAction {
    let window = settings.batch_window_seconds.max(0.0);
    let enabled = settings.enabled && registry.enabled();
    if enabled {
        state
            .roots
            .retain(|root_id, _| root_paths.contains_key(root_id));
        let scheduled = scheduled_scopes(registry, rules);
        let scanning = scanning_roots(pool, store).await;
        let mut quick_secs: Option<f64> = None;
        for (root_id, root_path) in root_paths {
            if stop.is_cancelled() {
                break;
            }
            let starts = walk_starts(&scheduled, root_id);
            let known = matches!(state.roots.get(root_id), Some(watch) if watch.path == *root_path);
            if starts.is_empty() || scanning.contains(root_id) {
                // Not walked this pass: what is on record stays, and the
                // scan the watcher asked for still settles.
                if let Some(watch) = state.roots.get_mut(root_id).filter(|_| known) {
                    settle(pool, store, root_id, watch).await;
                    if wants_scan(watch) && state.pending_since.is_none() {
                        state.pending_since = Some(now);
                    }
                }
                continue;
            }
            let previous = match state.roots.get_mut(root_id) {
                Some(watch)
                    if known
                        && !full_read_due(watch.folders.starts(), watch.full_at, &starts, now) =>
                {
                    Some(std::mem::take(&mut watch.folders))
                }
                _ => None,
            };
            let quick = previous.is_some();
            let path = root_path.clone();
            let walk_stop = stop.clone();
            let walk_from = starts.clone();
            let started = Instant::now();
            let walked = pool
                .run(move || {
                    let previous = previous.unwrap_or_default();
                    let next = previous.pass(&path, &walk_from, now, &|| walk_stop.is_cancelled());
                    (previous, next)
                })
                .await;
            let (previous, walked) = match walked {
                Ok(pair) => pair,
                Err(error) => (FolderRecord::default(), Err(std::io::Error::other(error))),
            };
            let next = match walked {
                Ok(next) => next,
                Err(error) => {
                    // A missing root keeps what is on record instead of
                    // scan-storming on a transient unmount, and a stopped
                    // pass keeps the walk it started from.
                    if let Some(watch) = state.roots.get_mut(root_id).filter(|_| quick) {
                        watch.folders = previous;
                    }
                    tracing::debug!(root_id = %root_id, error = %error, "folder watcher root unreadable");
                    continue;
                }
            };
            if quick {
                *quick_secs.get_or_insert(0.0) += started.elapsed().as_secs_f64();
            }
            let mut seen = next.digests();
            if !known {
                let moved = state.roots.remove(root_id).is_some();
                let id = root_id.clone();
                let recorded = match on_store(pool, store, move |store| store.folder_digests(&id))
                    .await
                {
                    Ok(recorded) => recorded,
                    Err(error) => {
                        tracing::warn!(root_id = %root_id, %error, "folder watcher could not read folder digests; tries again next poll");
                        continue;
                    }
                };
                let recorded = if moved || recorded.is_empty() {
                    // A root seen for the first time, or pointed at another
                    // folder, is taken as found without a scan: saving it in
                    // the settings already asked for one.
                    if !record(pool, store, root_id, &recorded, &seen).await {
                        continue;
                    }
                    seen.clone()
                } else {
                    recorded
                };
                state.roots.insert(
                    root_id.clone(),
                    RootWatch {
                        path: root_path.clone(),
                        recorded,
                        seen: Digests::new(),
                        asked: None,
                        folders: FolderRecord::default(),
                        full_at: now,
                    },
                );
            }
            let Some(watch) = state.roots.get_mut(root_id) else {
                continue;
            };
            // Folders on record outside where the root is walked (an
            // excluded root's other folders) stay as they are.
            for (folder, digest) in &watch.recorded {
                if !starts.iter().any(|start| scope_covers_path(start, folder)) {
                    seen.entry(folder.clone()).or_insert(*digest);
                }
            }
            watch.folders = next;
            if !quick {
                watch.full_at = now;
            }
            settle(pool, store, root_id, watch).await;
            watch.seen = seen;
            if wants_scan(watch) && state.pending_since.is_none() {
                state.pending_since = Some(now);
            }
        }
        if let Some(secs) = quick_secs {
            state.last_quick_secs = secs;
        }
    }
    let gap = state.gap(settings);
    match state.pending_since {
        None => WatcherAction::Idle { sleep_secs: gap },
        Some(since) => {
            let elapsed = now - since;
            if elapsed >= window {
                WatcherAction::Due
            } else {
                WatcherAction::Batching {
                    sleep_secs: gap.min(window - elapsed),
                }
            }
        }
    }
}

/// How asking for the watcher's scan went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// This run will scan the changes.
    Run(String),
    /// Not now (a busy database): the batch stays due.
    Later,
    /// Refused: the batch closes, and a later poll that still sees the
    /// changes asks again.
    Refused,
}

/// Ask, through `ask`, for an incremental scan of the folders that changed
/// in every root not already waiting on a scan, and close the batch. A
/// root whose changes lie where the schedule does not walk has them put
/// on record at once.
pub async fn ask_for_changes(
    state: &mut WatcherState,
    registry: &RootRegistry,
    rules: &[InclusionRule],
    pool: &BlockingPool,
    store: &Arc<SqliteScanStore>,
    ask: impl FnOnce(&ScanRequest) -> Asked,
) {
    let scheduled = scheduled_scopes(registry, rules);
    let mut root_ids: Vec<String> = state.roots.keys().cloned().collect();
    root_ids.sort();
    let mut scopes: Vec<ScanScope> = Vec::new();
    let mut asking: Vec<String> = Vec::new();
    for root_id in root_ids {
        let Some(watch) = state.roots.get_mut(&root_id) else {
            continue;
        };
        if watch.asked.is_some() {
            continue;
        }
        let found = changes(&watch.recorded, &watch.seen);
        if found == Changes::None {
            continue;
        }
        let own = change_scopes(registry, &scheduled, &root_id, &found);
        if own.is_empty() {
            if record(pool, store, &root_id, &watch.recorded, &watch.seen).await {
                watch.recorded = watch.seen.clone();
            }
            continue;
        }
        scopes.extend(own);
        asking.push(root_id);
    }
    if scopes.is_empty() {
        tracing::debug!("folder watcher closing its batch: nothing to scan");
        state.pending_since = None;
        return;
    }
    let request = ScanRequest {
        kind: ScanKind::Incremental,
        trigger: ScanTrigger::Automatic,
        scopes,
        requested_by_user_id: None,
        policy_revision: registry.fingerprint().to_owned(),
    };
    match ask(&request) {
        Asked::Run(run_id) => {
            for root_id in asking {
                if let Some(watch) = state.roots.get_mut(&root_id) {
                    watch.asked = Some((run_id.clone(), watch.seen.clone()));
                }
            }
            state.pending_since = None;
        }
        Asked::Refused => state.pending_since = None,
        Asked::Later => {}
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::walk::{FolderStat, folder_digests};
    use super::*;

    fn digest(value: i64, audio_files: i64) -> FolderDigest {
        FolderDigest {
            digest: value,
            audio_files,
        }
    }

    /// Seconds since the epoch at which `path` was last modified.
    fn modified_secs(path: &Path) -> f64 {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs_f64()
    }

    /// Date `folder` a second later, as a change made after the last read
    /// does where the filesystem's clock has moved on: these tests run
    /// faster than its timestamps tick.
    fn later(folder: &Path) {
        let modified = std::fs::metadata(folder)
            .and_then(|meta| meta.modified())
            .expect("mtime");
        std::fs::File::open(folder)
            .and_then(|dir| dir.set_modified(modified + std::time::Duration::from_secs(1)))
            .expect("date");
    }

    /// Write into `path` without moving its folder's times, as a tagger
    /// saving in place does, and date it later.
    fn rewrite_in_place(path: &Path) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open");
        file.write_all(b"Y").expect("write");
        let modified = file
            .metadata()
            .and_then(|meta| meta.modified())
            .expect("mtime");
        file.set_modified(modified + std::time::Duration::from_secs(5))
            .expect("date");
    }

    /// A folder whose times are within 2 seconds of its read, or later
    /// than it, is read again; once they are 2 seconds older it is not.
    #[test]
    fn racy_folders_are_read_again_until_their_times_settle() {
        const SEC: i64 = 1_000_000_000;
        let stat = FolderStat {
            mtime_ns: 1_000 * SEC,
            ctime_ns: 1_000 * SEC,
            inode: 7,
        };
        assert!(stat.racy(1_000.0), "read in the same second");
        assert!(stat.racy(1_001.9), "read 1.9 s after");
        assert!(stat.racy(990.0), "times later than the read");
        assert!(!stat.racy(1_002.0), "read 2 s after");
        let changed_late = FolderStat {
            ctime_ns: 1_001 * SEC,
            ..stat
        };
        assert!(changed_late.racy(1_002.5), "the ctime counts too");
        assert!(!changed_late.racy(1_003.0));
    }

    /// The wait is the poll interval, or 20 quick passes when longer,
    /// never under the floor.
    #[test]
    fn the_gap_is_the_poll_interval_or_twenty_quick_passes() {
        assert_eq!(gap(300.0, 0.0), 300.0);
        assert_eq!(gap(300.0, 15.0), 300.0);
        assert_eq!(gap(300.0, 20.0), 400.0);
        assert_eq!(gap(0.1, 0.0), MIN_POLL_INTERVAL_SECS);
    }

    /// Quick passes add up to the digests a full walk finds: on a steady
    /// tree, after a change inside a folder whose name is not UTF-8 (which
    /// folds into the folder above), and after a new nested folder. A file
    /// rewritten in place is missed until a full read.
    #[test]
    fn quick_passes_add_up_to_what_a_full_walk_finds() {
        use std::os::unix::ffi::OsStrExt as _;

        let scratch = droppedneedle_kernel::test_support::ScratchDir::new("scan-watch-quick")
            .expect("scratch");
        let root = scratch.to_path_buf();
        let odd = root
            .join("P")
            .join(std::ffi::OsStr::from_bytes(b"Disc \xff"));
        std::fs::create_dir_all(odd.join("Sub")).expect("mkdir");
        std::fs::create_dir_all(root.join("X")).expect("mkdir");
        std::fs::write(odd.join("1.flac"), b"one").expect("write");
        std::fs::write(odd.join("Sub/2.flac"), b"two").expect("write");
        std::fs::write(root.join("X/3.flac"), b"three").expect("write");
        let made = modified_secs(&root.join("X/3.flac"));
        let whole = [".".to_owned()];
        let pass = |record: &FolderRecord, at: f64| {
            record
                .pass(&root, &whole, made + at, &|| false)
                .expect("pass")
        };

        let full = pass(&FolderRecord::default(), 10.0);
        let before = folder_digests(&root, &|| false).expect("walk");
        assert_eq!(full.digests(), before);
        let steady = pass(&full, 20.0);
        assert_eq!(steady.digests(), before, "steady tree");

        std::fs::write(odd.join("4.flac"), b"four").expect("write");
        later(&odd);
        let quick = pass(&steady, 30.0);
        let walked = folder_digests(&root, &|| false).expect("walk");
        assert_eq!(quick.digests(), walked, "a change below a non-UTF-8 name");
        assert_ne!(quick.digests()["P"], before["P"], "folded into P");
        assert_eq!(quick.digests()["P"].audio_files, 3);

        std::fs::create_dir_all(root.join("P/New/CD1")).expect("mkdir");
        std::fs::write(root.join("P/New/CD1/5.flac"), b"five").expect("write");
        later(&root.join("P"));
        let quick = pass(&quick, 40.0);
        assert_eq!(
            quick.digests(),
            folder_digests(&root, &|| false).expect("walk"),
            "a new nested folder"
        );

        let settled = quick.digests();
        rewrite_in_place(&root.join("X/3.flac"));
        let quick = pass(&quick, 50.0);
        assert_eq!(quick.digests(), settled, "an edit in place waits");
        let full = pass(&FolderRecord::default(), 60.0);
        assert_ne!(full.digests()["X"], settled["X"]);
        assert_eq!(
            full.digests(),
            folder_digests(&root, &|| false).expect("walk")
        );
    }

    /// A folder gone, or swapped for a link, drops out of a quick pass with
    /// everything below it, and nothing behind the link is read. A start
    /// reached through a link is not walked at all.
    #[test]
    fn a_quick_pass_reads_nothing_below_a_folder_gone_or_turned_into_a_link() {
        let scratch = droppedneedle_kernel::test_support::ScratchDir::new("scan-watch-link")
            .expect("scratch");
        let base = scratch.to_path_buf();
        let root = base.join("music");
        for folder in ["A/B", "Gone/In", "Keep"] {
            std::fs::create_dir_all(root.join(folder)).expect("mkdir");
            std::fs::write(root.join(folder).join("1.flac"), b"one").expect("write");
        }
        std::fs::create_dir_all(base.join("elsewhere/B/C")).expect("mkdir");
        std::fs::write(base.join("elsewhere/B/C/2.flac"), b"two").expect("write");
        let made = modified_secs(&root.join("Keep/1.flac"));
        let whole = [".".to_owned()];
        let full = FolderRecord::default()
            .pass(&root, &whole, made + 10.0, &|| false)
            .expect("pass");

        scratch.remove_folder("music/Gone").expect("remove");
        scratch.remove_folder("music/A").expect("remove");
        scratch.link_folder("elsewhere", "music/A").expect("link");
        later(&root);
        let quick = full
            .pass(&root, &whole, made + 20.0, &|| false)
            .expect("pass");
        let mut folders: Vec<String> = quick.digests().into_keys().collect();
        folders.sort();
        assert_eq!(folders, [".", "Keep"]);
        assert_eq!(
            quick.digests(),
            folder_digests(&root, &|| false).expect("walk")
        );

        let through = FolderRecord::default()
            .pass(&root, &["A/B".to_owned()], made + 30.0, &|| false)
            .expect("pass");
        assert!(through.digests().is_empty(), "a start behind a link");
    }

    /// A walk ignores the publisher's staging temps, so a publish never
    /// sets the watcher off.
    #[test]
    fn staging_temps_leave_the_digests_alone() {
        let scratch = droppedneedle_kernel::test_support::ScratchDir::new("scan-watch-sidecar")
            .expect("scratch");
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(root.join("a")).expect("mkdir");
        std::fs::write(root.join("a/1.flac"), b"data").expect("write");
        let before = folder_digests(&root, &|| false).expect("walk");
        std::fs::write(
            root.join("a/.droppedneedle-management-j1.1.flac.tmp"),
            b"staged",
        )
        .expect("write temp");
        std::fs::create_dir_all(root.join(".droppedneedle-management-j2")).expect("mkdir");
        assert_eq!(folder_digests(&root, &|| false).expect("walk"), before);
        std::fs::write(root.join("a/2.flac"), b"data").expect("write music");
        let after = folder_digests(&root, &|| false).expect("walk");
        assert_eq!(
            changes(&before, &after),
            Changes::Folders(vec!["a".to_owned()])
        );
    }

    /// A folder that went away or lost music files asks for the whole root;
    /// new and grown folders are named, shallowest first.
    #[test]
    fn losses_ask_for_the_whole_root() {
        let recorded: Digests = [
            (".".to_owned(), digest(1, 0)),
            ("a".to_owned(), digest(2, 3)),
        ]
        .into();
        let mut seen = recorded.clone();
        seen.insert("a".to_owned(), digest(9, 3));
        seen.insert("b/c".to_owned(), digest(4, 1));
        seen.insert("b".to_owned(), digest(5, 0));
        assert_eq!(
            changes(&recorded, &seen),
            Changes::Folders(vec!["a".to_owned(), "b".to_owned(), "b/c".to_owned()])
        );
        seen.insert("a".to_owned(), digest(9, 2));
        assert_eq!(changes(&recorded, &seen), Changes::WholeRoot);
        seen.remove("a");
        assert_eq!(changes(&recorded, &seen), Changes::WholeRoot);
    }
}
