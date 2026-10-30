//! Folder watcher, work wakeups, dirty scopes.
//!
//! The watcher walks every library root once per poll interval with the
//! scan's own walk ([`folder_digests`]: stat only, links never followed,
//! the publisher's files skipped) and keeps one digest per folder. When
//! folders move it asks, after a batching window so a burst collapses into
//! one request, for an incremental scan of just those folders. The digests
//! it compares against are kept in `library_folder_digests` and written
//! once the scan it asked for has settled, so a restart neither rescans a
//! steady tree nor forgets a change the scan has not taken in yet.
//!
//! Accepted limitation: changes preserving both mtime and size are
//! invisible to a stat-only walk and surface only through the rolling
//! schedule or a manual scan.
//!
//! This module also owns [`WorkWakeups`] (the work revisions the loops
//! wake on) and [`DirtyScopes`] (Hook B marks left by
//! settings saves).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
use super::walk::{FolderDigest, folder_digests};

/// Default poll interval (v2 `DEFAULT_POLL_INTERVAL_SECONDS`).
pub const DEFAULT_POLL_INTERVAL_SECS: f64 = 300.0;
/// Default batching window (v2 `DEFAULT_BATCH_WINDOW_SECONDS`).
pub const DEFAULT_BATCH_WINDOW_SECS: f64 = 60.0;
/// Minimum poll interval; lower values clamp (v2 `_MIN_POLL_INTERVAL_SECONDS`).
pub const MIN_POLL_INTERVAL_SECS: f64 = 1.0;

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

/// Hook B dirty scope marks (v2 supervisor `dirty_scopes_getter` /
/// `dirty_scopes_clearer`). Hints only: marks clear on a non-conflict
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
}

/// Watcher loop state: what each root looked like, plus the batch timer.
#[derive(Debug, Default)]
pub struct WatcherState {
    roots: HashMap<String, RootWatch>,
    pending_since: Option<f64>,
}

impl WatcherState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_pending(&self) -> bool {
        self.pending_since.is_some()
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

/// Poll every root once and update the batch timer (v2
/// `watch_library_filesystem` body, one iteration). The caller asks for
/// the scan on [`WatcherAction::Due`], through [`ask_for_changes`].
#[allow(clippy::too_many_arguments)]
pub async fn poll_once(
    state: &mut WatcherState,
    settings: &WatcherSettings,
    registry: &RootRegistry,
    root_paths: &HashMap<String, PathBuf>,
    pool: &BlockingPool,
    store: &Arc<SqliteScanStore>,
    now: f64,
    stop: &CancellationToken,
) -> WatcherAction {
    let poll_interval = settings.poll_interval_seconds.max(MIN_POLL_INTERVAL_SECS);
    let window = settings.batch_window_seconds.max(0.0);
    let enabled = settings.enabled && registry.enabled();
    if enabled {
        state
            .roots
            .retain(|root_id, _| root_paths.contains_key(root_id));
        for (root_id, root_path) in root_paths {
            if stop.is_cancelled() {
                break;
            }
            let path = root_path.clone();
            let walk_stop = stop.clone();
            let walked = pool
                .run(move || folder_digests(&path, &|| walk_stop.is_cancelled()))
                .await
                .unwrap_or_else(|error| Err(std::io::Error::other(error)));
            let seen = match walked {
                Ok(seen) => seen,
                Err(error) => {
                    // A missing root keeps what is on record instead of
                    // scan-storming on a transient unmount.
                    tracing::debug!(root_id = %root_id, error = %error, "folder watcher root unreadable");
                    continue;
                }
            };
            if !matches!(state.roots.get(root_id), Some(watch) if watch.path == *root_path) {
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
                    },
                );
            }
            let Some(watch) = state.roots.get_mut(root_id) else {
                continue;
            };
            if let Some((run_id, asked)) = watch.asked.take() {
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
            watch.seen = seen;
            if watch.asked.is_none()
                && changes(&watch.recorded, &watch.seen) != Changes::None
                && state.pending_since.is_none()
            {
                state.pending_since = Some(now);
            }
        }
    }
    match state.pending_since {
        None => WatcherAction::Idle {
            sleep_secs: poll_interval,
        },
        Some(since) => {
            let elapsed = now - since;
            if elapsed >= window {
                WatcherAction::Due
            } else {
                WatcherAction::Batching {
                    sleep_secs: poll_interval.min(window - elapsed),
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
    use super::*;

    fn digest(value: i64, audio_files: i64) -> FolderDigest {
        FolderDigest {
            digest: value,
            audio_files,
        }
    }

    /// A walk ignores the publisher's staging temps, so a publish never
    /// sets the watcher off.
    #[test]
    fn staging_temps_leave_the_digests_alone() {
        let scratch = droppedneedle_kernel::db::scratch::ScratchDir::new("scan-watch-sidecar")
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
