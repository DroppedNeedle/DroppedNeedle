//! Zero-dependency recursive filesystem poller, work wakeups, dirty scopes.
//!
//! Port of v2's library filesystem watcher (supervisor hook C). The poller takes a recursive stat-only snapshot of every
//! library root once per poll interval and, when a snapshot moves, enqueues
//! a single incremental/automatic scan after a batching window so rapid
//! bursts collapse into one request.
//!
//! Recursion matters: on Linux only the immediate parent's mtime
//! moves when a nested file changes, so a shallow poll misses nested
//! mutations silently. The walk is stat-only (no tag reads, no hashing)
//! and runs on the blocking pool. Symlinks are never followed.
//!
//! Accepted limitation: changes preserving both mtime and size are
//! invisible to a stat-only snapshot and surface only through the rolling
//! schedule or a manual scan.
//!
//! This module also owns [`WorkWakeups`] (durable-work revision signals
//! the supervisor sleeps on) and [`DirtyScopes`] (Hook B marks left by
//! settings saves).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use super::fs::is_management_artifact;
use super::models::{ScanKind, ScanRequest, ScanRequestResult, ScanTrigger};
use super::pool::BlockingPool;
use super::roots::RootRegistry;
use super::scheduler::{InclusionRule, scheduled_scopes};

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

/// One snapshot entry: (mtime_ns, size_bytes, is_dir).
pub type SnapshotEntry = (i64, u64, bool);
/// Recursive stat-only snapshot keyed by root-relative path.
pub type Snapshot = HashMap<String, SnapshotEntry>;

/// Recursive stat-only snapshot of `root` (v2 `_snapshot_tree`).
/// Unreadable subdirectories are skipped; a missing top-level root is an
/// error, and the caller keeps its previous baseline instead of
/// scan-storming on a transient unmount.
pub fn snapshot_tree(root: &Path) -> std::io::Result<Snapshot> {
    let mut snapshot = Snapshot::new();
    let mut stack = vec![root.to_owned()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries.collect::<Vec<_>>(),
            Err(error) => {
                if current == root {
                    return Err(error);
                }
                tracing::debug!(path = %current.display(), "filesystem watcher skipping unreadable directory");
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            // file_type() never follows symlinks: a symlinked directory is
            // recorded as a single entry and never descended.
            let is_dir = file_type.is_dir();
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            // entry.metadata follows symlinks while file_type did not; for
            // symlinks re-stat without following so the snapshot records
            // the link itself.
            let meta = if file_type.is_symlink() {
                match std::fs::symlink_metadata(entry.path()) {
                    Ok(meta) => meta,
                    Err(_) => continue,
                }
            } else {
                meta
            };
            let relative = match entry.path().strip_prefix(root) {
                Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            // Own sidecar writes must not retrigger the watcher: the
            // server keeps its publish database under the management
            // prefix inside watched roots. The walk already skips the
            // same rule; skipping here drops both the entry and the
            // descent, since the push below never runs.
            if is_management_artifact(Path::new(&relative)) {
                continue;
            }
            let mtime_ns = super::revision::mtime_ns_from_metadata(&meta);
            snapshot.insert(relative, (mtime_ns, meta.len(), is_dir));
            if is_dir && !file_type.is_symlink() {
                stack.push(entry.path());
            }
        }
    }
    Ok(snapshot)
}

/// Durable-work wakeup revisions (v2 `DurableWorkWakeups`, reduced).
/// Supervisors sleep on a revision and wake early when notified.
#[derive(Debug, Clone, Default)]
pub struct WorkWakeups {
    inner: Arc<WorkWakeupsInner>,
}

#[derive(Debug, Default)]
struct WorkWakeupsInner {
    revisions: Mutex<HashMap<String, u64>>,
    notify: Notify,
}

impl WorkWakeups {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notify(&self, kind: &str) {
        *self
            .inner
            .revisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(kind.to_owned())
            .or_insert(0) += 1;
        self.inner.notify.notify_waiters();
    }

    pub fn revision(&self, kind: &str) -> u64 {
        self.inner
            .revisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(kind)
            .copied()
            .unwrap_or(0)
    }

    /// Wait until `kind` moves past `after_revision` or the timeout
    /// elapses. Returns true when work arrived.
    pub async fn wait(&self, kind: &str, after_revision: u64, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.revision(kind) != after_revision {
                return true;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let _ = tokio::time::timeout_at(deadline, self.inner.notify.notified()).await;
        }
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

    pub fn mark_many(&self, scope_ids: &[String]) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(scope_ids.iter().cloned());
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

/// Watcher loop state: per-root baselines plus the pending batch timer.
#[derive(Debug, Default)]
pub struct WatcherState {
    baselines: HashMap<String, (String, Snapshot)>,
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
    /// The window elapsed; the caller should request a scan now.
    Due,
}

/// Poll every root once and update the batch timer (v2
/// `watch_library_filesystem` body, one iteration). Pure decision logic:
/// the caller issues the scan request on [`WatcherAction::Due`].
pub async fn poll_once(
    state: &mut WatcherState,
    settings: &WatcherSettings,
    registry: &RootRegistry,
    root_paths: &HashMap<String, PathBuf>,
    pool: &BlockingPool,
    now: f64,
) -> WatcherAction {
    let poll_interval = settings.poll_interval_seconds.max(MIN_POLL_INTERVAL_SECS);
    let window = settings.batch_window_seconds.max(0.0);
    let enabled = settings.enabled && registry.enabled();
    if enabled {
        for (root_id, root_path) in root_paths {
            let path = root_path.clone();
            let pool = pool.clone();
            let snapshot = pool.run(move || snapshot_tree(&path)).await;
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    // Missing root: log and keep the previous baseline
                    // instead of scan-storming on a transient unmount.
                    tracing::debug!(root_id = %root_id, error = %error, "filesystem watcher root unreadable");
                    continue;
                }
            };
            match state.baselines.get(root_id) {
                None => {
                    // First sighting: seed silently so a (re)start never
                    // scans a steady tree.
                    state
                        .baselines
                        .insert(root_id.clone(), (root_path.display().to_string(), snapshot));
                }
                Some((previous_path, _)) if previous_path != &root_path.display().to_string() => {
                    // Re-pointed root id: reseed silently. Settings-driven
                    // root changes are already covered by Hook B marks.
                    state
                        .baselines
                        .insert(root_id.clone(), (root_path.display().to_string(), snapshot));
                }
                Some((_, previous)) => {
                    if snapshot != *previous {
                        state
                            .baselines
                            .insert(root_id.clone(), (root_path.display().to_string(), snapshot));
                        if state.pending_since.is_none() {
                            state.pending_since = Some(now);
                        }
                    }
                }
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

/// Build the watcher's scan request scopes (v2 watcher request body).
pub fn watcher_scopes(
    registry: &RootRegistry,
    rules: &[InclusionRule],
) -> Vec<super::models::ScanScope> {
    scheduled_scopes(registry, rules)
}

/// Build the watcher's scan request (v2 watcher request body).
pub fn watcher_request(registry: &RootRegistry, rules: &[InclusionRule]) -> Option<ScanRequest> {
    let scopes = watcher_scopes(registry, rules);
    if scopes.is_empty() {
        // No scheduled scopes: drop the pending scan (v2 debug path) and
        // let the caller clear the batch timer.
        return None;
    }
    Some(ScanRequest {
        kind: ScanKind::Incremental,
        trigger: ScanTrigger::Automatic,
        scopes,
        requested_by_user_id: None,
        policy_revision: registry.policy_revision().to_owned(),
    })
}

/// Clear the batch timer after the scan was requested (or dropped).
pub fn clear_pending(state: &mut WatcherState) {
    state.pending_since = None;
}

/// Run the watcher loop until `shutdown` is set. Every getter is re-read
/// each iteration, never captured: a settings save rebuilds singletons
/// and the next poll must see the new instances.
pub async fn watch_library_filesystem<F, G>(
    inputs: &WatcherInputs,
    pool: &BlockingPool,
    request: F,
    shutdown: &std::sync::atomic::AtomicBool,
    mut sleep: G,
) where
    F: Fn(ScanRequest) -> ScanRequestResult,
    G: AsyncSleep,
{
    let mut state = WatcherState::new();
    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let settings = (inputs.settings)();
        let registry = (inputs.registry)();
        let now = (inputs.clock)();
        let action = poll_once(
            &mut state,
            &settings,
            &registry,
            &(inputs.root_paths)(),
            pool,
            now,
        )
        .await;
        let sleep_secs = match action {
            WatcherAction::Idle { sleep_secs: wait } => wait,
            WatcherAction::Batching { sleep_secs: wait } => wait,
            WatcherAction::Due => {
                let rules = (inputs.inclusion_rules)();
                match watcher_request(&registry, &rules) {
                    None => {
                        tracing::debug!(
                            "filesystem watcher dropping pending scan: no scheduled scopes"
                        );
                        clear_pending(&mut state);
                    }
                    Some(scan) => {
                        let result = request(scan);
                        tracing::info!(
                            disposition = ?result.disposition,
                            "filesystem watcher requested incremental scan"
                        );
                        clear_pending(&mut state);
                        inputs.wakeups.notify("scan");
                    }
                }
                settings.poll_interval_seconds.max(MIN_POLL_INTERVAL_SECS)
            }
        };
        sleep
            .sleep(Duration::from_secs_f64(sleep_secs.max(0.0)))
            .await;
        // Loop iterations never fail: snapshot errors are contained per
        // root above, so no catch-all is needed to keep the lifetime
        // watcher alive.
    }
}

/// Watcher loop inputs. Getters are re-read every iteration.
pub struct WatcherInputs {
    pub root_paths: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>,
    pub settings: Arc<dyn Fn() -> WatcherSettings + Send + Sync>,
    pub registry: Arc<dyn Fn() -> RootRegistry + Send + Sync>,
    pub inclusion_rules: Arc<dyn Fn() -> Vec<InclusionRule> + Send + Sync>,
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub wakeups: WorkWakeups,
}

/// Async sleep seam so tests fast-forward the loop.
pub trait AsyncSleep {
    fn sleep(&mut self, duration: Duration) -> impl std::future::Future<Output = ()> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_returns_true_on_notify() {
        let wakeups = WorkWakeups::new();
        let moved = wakeups.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            moved.notify("scan");
        });
        assert!(wakeups.wait("scan", 0, Duration::from_secs(5)).await);
        assert!(!wakeups.wait("scan", 1, Duration::from_millis(10)).await);
    }

    #[test]
    fn snapshot_records_nested_files_recursively() {
        let scratch = crate::tooling::scratch::ScratchDir::new("scan-snap").expect("scratch");
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("sub").join("a.flac"), b"data").expect("write");
        let first = snapshot_tree(&root).expect("snapshot");
        assert!(first.contains_key("sub/a.flac"));
        assert!(first.contains_key("sub"));
        std::fs::write(root.join("sub").join("a.flac"), b"longer-data").expect("rewrite");
        let second = snapshot_tree(&root).expect("snapshot");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn poll_batches_mutation_into_due() {
        let scratch = crate::tooling::scratch::ScratchDir::new("scan-watch").expect("scratch");
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("a.flac"), b"data").expect("write");
        let registry = RootRegistry::new(
            vec![super::super::roots::LibraryRoot::new(
                "r1",
                root.clone(),
                super::super::models::EffectivePolicy::Automatic,
            )],
            true,
            "rev-1",
        );
        let mut roots = HashMap::new();
        roots.insert("r1".to_owned(), root.clone());
        let pool = BlockingPool::new(2);
        let settings = WatcherSettings {
            enabled: true,
            poll_interval_seconds: 5.0,
            batch_window_seconds: 60.0,
        };
        let mut state = WatcherState::new();
        // First sighting seeds silently: idle, not due.
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 0.0).await;
        assert!(matches!(action, WatcherAction::Idle { .. }));
        // A mutation starts batching.
        std::fs::write(root.join("b.flac"), b"data").expect("write");
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 10.0).await;
        assert!(matches!(action, WatcherAction::Batching { .. }));
        // Inside the window: still batching.
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 20.0).await;
        assert!(matches!(action, WatcherAction::Batching { .. }));
        // Past the window: due.
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 71.0).await;
        assert_eq!(action, WatcherAction::Due);
    }

    #[tokio::test]
    async fn staging_temps_do_not_trip_the_watcher() {
        let scratch =
            crate::tooling::scratch::ScratchDir::new("scan-watch-sidecar").expect("scratch");
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("a.flac"), b"data").expect("write");
        let registry = RootRegistry::new(
            vec![super::super::roots::LibraryRoot::new(
                "r1",
                root.clone(),
                super::super::models::EffectivePolicy::Automatic,
            )],
            true,
            "rev-1",
        );
        let mut roots = HashMap::new();
        roots.insert("r1".to_owned(), root.clone());
        let pool = BlockingPool::new(2);
        let settings = WatcherSettings {
            enabled: true,
            poll_interval_seconds: 5.0,
            batch_window_seconds: 60.0,
        };
        let mut state = WatcherState::new();
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 0.0).await;
        assert!(matches!(action, WatcherAction::Idle { .. }));
        // A publish staging temp lands beside the music: still idle, with
        // no batch pending.
        let temp = root.join(".droppedneedle-management-j1.a.flac.tmp");
        std::fs::write(&temp, b"staged").expect("write temp");
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 10.0).await;
        assert!(matches!(action, WatcherAction::Idle { .. }));
        assert!(!state.is_pending(), "staging temp leaves no batch pending");
        // Rewriting the temp stays quiet too, while a real music file
        // still trips the batch.
        std::fs::write(&temp, b"staged-v2").expect("rewrite temp");
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 20.0).await;
        assert!(matches!(action, WatcherAction::Idle { .. }));
        std::fs::write(root.join("b.flac"), b"data").expect("write music");
        let action = poll_once(&mut state, &settings, &registry, &roots, &pool, 30.0).await;
        assert!(matches!(action, WatcherAction::Batching { .. }));
    }
}
