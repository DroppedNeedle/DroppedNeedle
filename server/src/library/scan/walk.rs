//! One-walk, bounded-queue discovery.
//!
//! Port of v2's library inventory scanner. A
//! blocking producer walks each scope and streams file stats through a
//! bounded channel; the async consumer dedups, classifies, and persists
//! inventory in batches. Skip-and-report runs through everything: one bad
//! path records a failure row and the walk continues.
//!
//! Purity: the walk only lists directories and stats files. Nothing here
//! opens a file for writing, creates files, or mutates the tree.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use unicode_normalization::UnicodeNormalization;

use super::fs::FsCoordinator;
use super::fs::is_management_artifact;
use super::models::{
    EffectivePolicy, ScanFailureRecord, ScanInventoryItem, ScanPhase, ScanRun, ScanScope,
    ScanState, ScopeDiscoveryState, Verdict, failure_codes,
};
use super::pool::BlockingPool;
use super::revision::{exact_stat_revision, mtime_ns_from_metadata};
use super::roots::PolicyResolver;
use super::seams::Checkpoint;
use super::store::{ClassifyInput, ScanStore, ScanStoreError};

/// Bounded discovery queue (v2 `INVENTORY_QUEUE_SIZE`).
pub const INVENTORY_QUEUE_SIZE: usize = 256;
/// Inventory rows persisted per batch (v2 `INVENTORY_BATCH_SIZE`).
pub const INVENTORY_BATCH_SIZE: usize = 256;
/// Reap horizon for wedged detached walkers as a multiple of the walk
/// deadline (v2 `DETACHED_WALKER_REAP_MULTIPLIER`).
pub const DETACHED_WALKER_REAP_MULTIPLIER: f64 = 3.0;
/// Default walk deadline (v2 `walk_deadline_seconds`).
pub const DEFAULT_WALK_DEADLINE_SECS: f64 = 30.0;
/// Default cap on in-flight detached walkers (v2 `max_detached_walkers`).
pub const DEFAULT_MAX_DETACHED_WALKERS: usize = 4;

/// Audio suffixes, lowercase with dot (v2 `AUDIO_EXTENSIONS` in
/// `local_files_service.py`). Matching is case-insensitive ASCII, which
/// equals v2's `casefold` for these suffixes.
pub const AUDIO_EXTENSIONS: &[&str] = &[".flac", ".mp3", ".ogg", ".m4a", ".aac", ".wav", ".opus"];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let mut dotted = String::with_capacity(ext.len() + 1);
            dotted.push('.');
            dotted.push_str(&ext.to_lowercase());
            AUDIO_EXTENSIONS.contains(&dotted.as_str())
        })
        .unwrap_or(false)
}

/// POSIX text for a path that is always bindable as stored TEXT (v2
/// `_text_safe_posix`): names that are not valid UTF-8 are
/// losslessly percent-encoded from their raw bytes instead.
pub fn text_safe_posix(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let parts: Vec<String> = path
            .components()
            .map(|component| {
                let bytes = component.as_os_str().as_bytes();
                match std::str::from_utf8(bytes) {
                    Ok(text) => text.to_owned(),
                    Err(_) => percent_encode_bytes(bytes),
                }
            })
            .collect();
        if parts.is_empty() {
            return String::new();
        }
        let mut joined = parts.join("/");
        if path.is_absolute() {
            joined.insert(0, '/');
        }
        joined
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().replace('\\', "/")
    }
}

fn percent_encode_bytes(bytes: &[u8]) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";
    let mut out = String::new();
    for byte in bytes {
        if UNRESERVED.contains(byte) {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Relativize for failure and heartbeat paths (v2 `_relativize`).
pub fn relativize(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) => text_safe_posix(relative),
        Err(_) => text_safe_posix(path),
    }
}

/// NFC-normalized inventory key for one in-root path (v2 `_inventory_key`,
/// step 4.13). macOS writes NFD bytes, so an NFC<->NFD rename of an
/// unchanged file must classify as a move (same key) instead of a
/// delete+new pair. Only the key is normalized; display strings keep their
/// on-disk form, and case-only variants are unaffected.
pub fn inventory_key(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) => {
            let posix = text_safe_posix(relative);
            posix.nfc().collect()
        }
        Err(_) => text_safe_posix(path).nfc().collect(),
    }
}

/// Class-name-plus-errno detail, never the message or filesystem paths
/// (v2 `_walk_failure_detail`).
fn walk_failure_detail(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(errno) => format!("IoError (errno={}) while walking.", errno_name(errno)),
        None => "IoError while walking.".to_owned(),
    }
}

fn errno_name(errno: i32) -> &'static str {
    match errno {
        1 => "EPERM",
        2 => "ENOENT",
        5 => "EIO",
        13 => "EACCES",
        20 => "ENOTDIR",
        21 => "EISDIR",
        22 => "EINVAL",
        23 => "ENFILE",
        24 => "EMFILE",
        28 => "ENOSPC",
        30 => "EROFS",
        36 => "ENAMETOOLONG",
        40 => "ELOOP",
        _ => "EUNKNOWN",
    }
}

fn walk_error_code(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(errno) => format!("WALK_{}", errno_name(errno)),
        None => failure_codes::WALK_ERROR.to_owned(),
    }
}

/// Thread-safe liveness signal written by the walk producer (v2
/// `_WalkHeartbeat`): `WALK_TIMEOUT` fires only when no progress
/// signal arrived for the whole deadline.
#[derive(Debug)]
struct WalkHeartbeat {
    inner: Mutex<HeartbeatState>,
}

#[derive(Debug)]
struct HeartbeatState {
    touched_at: Instant,
    last_directory: String,
}

impl WalkHeartbeat {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HeartbeatState {
                touched_at: Instant::now(),
                last_directory: String::new(),
            }),
        }
    }

    fn touch(&self, directory: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.touched_at = Instant::now();
        if !directory.is_empty() {
            state.last_directory = directory.to_owned();
        }
    }

    fn age(&self) -> Duration {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .touched_at
            .elapsed()
    }

    fn last_directory(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_directory
            .clone()
    }
}

#[derive(Debug, Clone)]
struct DiscoveredFile {
    resolved: PathBuf,
    size_bytes: u64,
    mtime_ns: i64,
    mtime_secs: f64,
    revision: String,
}

#[derive(Debug, Clone)]
struct WalkErrorInfo {
    path: PathBuf,
    code: String,
    detail: String,
}

#[derive(Debug, Clone)]
enum WalkMessage {
    File(DiscoveredFile),
    Skips(Vec<(String, String)>),
    WalkError(WalkErrorInfo),
    Done,
}

fn send_with_stop(
    sender: &mpsc::Sender<WalkMessage>,
    stopped: &AtomicBool,
    message: WalkMessage,
) -> bool {
    let mut message = message;
    loop {
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                message = returned;
                if stopped.load(Ordering::Relaxed) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn system_clock() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One-walk bounded-queue inventory scanner (v2 `LibraryInventoryScanner`).
pub struct InventoryScanner<S: ScanStore> {
    store: Arc<S>,
    pool: BlockingPool,
    fs: Option<FsCoordinator>,
    walk_deadline: Duration,
    max_detached_walkers: usize,
    detached: Arc<Mutex<HashMap<u64, Instant>>>,
    next_detached_id: AtomicU64,
    detached_reap_multiplier: f64,
    pending_probes: Arc<AtomicUsize>,
    probe_max_workers: usize,
    clock: Clock,
}

impl<S: ScanStore> InventoryScanner<S> {
    pub fn new(store: Arc<S>, pool: BlockingPool) -> Self {
        Self {
            store,
            pool,
            fs: None,
            walk_deadline: Duration::from_secs_f64(DEFAULT_WALK_DEADLINE_SECS),
            max_detached_walkers: DEFAULT_MAX_DETACHED_WALKERS,
            detached: Arc::new(Mutex::new(HashMap::new())),
            next_detached_id: AtomicU64::new(1),
            detached_reap_multiplier: DETACHED_WALKER_REAP_MULTIPLIER,
            pending_probes: Arc::new(AtomicUsize::new(0)),
            probe_max_workers: 1,
            clock: Arc::new(system_clock),
        }
    }

    pub fn with_filesystem(mut self, fs: FsCoordinator) -> Self {
        self.fs = Some(fs);
        self
    }

    fn reap_stale_detached_walkers(&self) -> usize {
        // Forget detached walkers older than the reap horizon so
        // scans keep moving; a late finish stays a safe noop via the
        // reaper task's remove.
        let horizon = self.walk_deadline.mul_f64(self.detached_reap_multiplier);
        let now = Instant::now();
        let mut detached = self
            .detached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let stale: Vec<u64> = detached
            .iter()
            .filter(|(_, started)| now.duration_since(**started) > horizon)
            .map(|(id, _)| *id)
            .collect();
        for id in &stale {
            detached.remove(id);
        }
        stale.len()
    }

    fn detach_walker(&self, handle: tokio::task::JoinHandle<()>) -> bool {
        // The cap is enforced. Beyond max in-flight wedged walkers
        // the task is refused (and counted as leaked) instead of being
        // tracked silently; the caller fails the run with
        // WALKER_UNAVAILABLE.
        if handle.is_finished() {
            return true;
        }
        let mut detached = self
            .detached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if detached.len() >= self.max_detached_walkers {
            drop(detached);
            self.reap_stale_detached_walkers();
            detached = self
                .detached
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if detached.len() >= self.max_detached_walkers {
                tracing::warn!(
                    max = self.max_detached_walkers,
                    "library_scan event=detached_walker_cap_exceeded"
                );
                return false;
            }
        }
        let id = self.next_detached_id.fetch_add(1, Ordering::Relaxed);
        detached.insert(id, Instant::now());
        drop(detached);
        // A late finish of an already-reaped walker is a safe noop: the
        // remove tolerates the missing entry and the join result is
        // consumed so no error escapes.
        let detached_map = Arc::clone(&self.detached);
        tokio::spawn(async move {
            let _ = handle.await;
            detached_map
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
        });
        true
    }

    fn record_failure(
        &self,
        run_id: &str,
        scope: &ScanScope,
        relative_path: String,
        failure_code: &str,
        failure_detail: String,
    ) {
        self.store.record_failures(
            run_id,
            vec![ScanFailureRecord {
                root_id: scope.root_id.clone(),
                relative_path,
                failure_code: failure_code.to_owned(),
                recorded_at: (self.clock)(),
                failure_detail,
                phase: ScanPhase::Discovering,
            }],
        );
    }

    fn fail_run(
        &self,
        run_id: &str,
        expected_state: ScanState,
        expected_revision: u64,
        code: &str,
    ) -> ScanRun {
        self.store
            .transition(
                run_id,
                expected_state,
                expected_revision,
                ScanState::Failed,
                (self.clock)(),
                Some(code),
            )
            .unwrap_or_else(|_| {
                self.store
                    .get_run(run_id)
                    .map(|(run, _, _)| run)
                    .unwrap_or_else(|_| ScanRun {
                        id: run_id.to_owned(),
                        kind: super::models::ScanKind::Incremental,
                        trigger: super::models::ScanTrigger::Manual,
                        state: ScanState::Failed,
                        phase: ScanPhase::Discovering,
                        requested_by_user_id: None,
                        aggregate_scope: "all".to_owned(),
                        queued_at: 0.0,
                        started_at: None,
                        updated_at: (self.clock)(),
                        terminal_at: Some((self.clock)()),
                        resume_phase: None,
                        requested_control: super::models::RequestedControl::None,
                        terminal_code: Some(code.to_owned()),
                        coalesced_request_count: 0,
                        row_revision: expected_revision,
                        event_revision: 0,
                        counters: HashMap::new(),
                        phase_timings: HashMap::new(),
                    })
            })
    }

    /// Probe one scope path on the pool with the walk deadline. Returns
    /// the probe outcome: directory or not.
    async fn probe_selected(&self, selected: &Path) -> Result<bool, ProbeOutcome> {
        // One probe slot by default. When occupied, give
        // the wedged stat one bounded deadline to finish before failing
        // the run.
        if !self.claim_probe_slot() {
            tokio::time::sleep(self.walk_deadline).await;
            if !self.claim_probe_slot() {
                return Err(ProbeOutcome::Unavailable);
            }
        }
        let pending = Arc::clone(&self.pending_probes);
        let released = Arc::new(AtomicBool::new(false));
        let job_released = Arc::clone(&released);
        let selected = selected.to_owned();
        let pool = self.pool.clone();
        let job = tokio::spawn(async move {
            let result = pool.run(move || selected.is_dir()).await;
            if !job_released.swap(true, Ordering::SeqCst) {
                pending.fetch_sub(1, Ordering::SeqCst);
            }
            result
        });
        match tokio::time::timeout(self.walk_deadline, job).await {
            Ok(Ok(exists)) => Ok(exists),
            Ok(Err(_)) => Err(ProbeOutcome::Wedged),
            Err(_) => {
                // Tombstone the slot so it is recovered instead of
                // staying occupied for the process lifetime.
                if !released.swap(true, Ordering::SeqCst) {
                    self.pending_probes.fetch_sub(1, Ordering::SeqCst);
                }
                Err(ProbeOutcome::Wedged)
            }
        }
    }

    fn claim_probe_slot(&self) -> bool {
        let mut current = self.pending_probes.load(Ordering::SeqCst);
        loop {
            if current >= self.probe_max_workers {
                return false;
            }
            match self.pending_probes.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Discover every scope of a run (v2 `discover`).
    pub async fn discover<C: Checkpoint>(
        &self,
        run: &ScanRun,
        scopes: &[ScanScope],
        root_paths: &HashMap<String, PathBuf>,
        resolver: &PolicyResolver,
        checkpoint: &C,
    ) -> ScanRun {
        // GH-296 skip-and-report: a scope whose root cannot be resolved or
        // probed is recorded while remaining scopes keep walking.
        // The run fails wholesale only when nothing was discoverable.
        let mut current = run.clone();
        let mut unavailable_scopes = 0usize;
        for scope in scopes {
            if self
                .store
                .scope_discovery_state(&run.id, &scope.root_id, &scope.relative_path)
                == ScopeDiscoveryState::Completed
            {
                continue;
            }
            if !checkpoint.check(&run.id, &scope.policy_revision) {
                return self
                    .store
                    .get_run(&run.id)
                    .map(|(run, _, _)| run)
                    .unwrap_or(current);
            }
            let root = root_paths
                .get(&scope.root_id)
                .cloned()
                .or_else(|| scope.root_path.as_ref().map(PathBuf::from));
            let Some(root) = root else {
                self.record_failure(
                    &run.id,
                    scope,
                    scope.relative_path.clone(),
                    failure_codes::ROOT_UNAVAILABLE,
                    "The library root has no configured path.".to_owned(),
                );
                self.store.complete_scope_discovery(
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                    ScopeDiscoveryState::Unavailable,
                    Some(failure_codes::ROOT_UNAVAILABLE),
                );
                unavailable_scopes += 1;
                continue;
            };
            let selected = if scope.relative_path == "." {
                root.clone()
            } else {
                root.join(&scope.relative_path)
            };
            match self.probe_selected(&selected).await {
                Err(ProbeOutcome::Unavailable) => {
                    self.record_failure(
                        &run.id,
                        scope,
                        scope.relative_path.clone(),
                        failure_codes::PROBE_UNAVAILABLE,
                        "A previous root probe never completed; scanning is paused until the filesystem responds or the service restarts.".to_owned(),
                    );
                    self.store.complete_scope_discovery(
                        &run.id,
                        &scope.root_id,
                        &scope.relative_path,
                        ScopeDiscoveryState::Unavailable,
                        Some(failure_codes::PROBE_UNAVAILABLE),
                    );
                    return self.fail_run(
                        &run.id,
                        current.state,
                        current.row_revision,
                        failure_codes::PROBE_UNAVAILABLE,
                    );
                }
                Err(ProbeOutcome::Wedged) => {
                    tracing::warn!(run_id = %run.id, root_id = %scope.root_id, "library_scan event=walk_timeout");
                    self.record_failure(
                        &run.id,
                        scope,
                        scope.relative_path.clone(),
                        failure_codes::WALK_TIMEOUT,
                        format!(
                            "The library root probe exceeded {:.1}s.",
                            self.walk_deadline.as_secs_f64()
                        ),
                    );
                    self.store.complete_scope_discovery(
                        &run.id,
                        &scope.root_id,
                        &scope.relative_path,
                        ScopeDiscoveryState::Unavailable,
                        Some(failure_codes::WALK_TIMEOUT),
                    );
                    return self.fail_run(
                        &run.id,
                        current.state,
                        current.row_revision,
                        failure_codes::WALK_TIMEOUT,
                    );
                }
                Ok(false) => {
                    self.record_failure(
                        &run.id,
                        scope,
                        scope.relative_path.clone(),
                        failure_codes::ROOT_UNAVAILABLE,
                        format!("The library root path is missing: {}", scope.relative_path),
                    );
                    self.store.complete_scope_discovery(
                        &run.id,
                        &scope.root_id,
                        &scope.relative_path,
                        ScopeDiscoveryState::Unavailable,
                        Some(failure_codes::ROOT_UNAVAILABLE),
                    );
                    unavailable_scopes += 1;
                    continue;
                }
                Ok(true) => {}
            }
            let mut restarts = 0u32;
            let mut superseded_scope = false;
            let (completed, walk_failure_code) = loop {
                let generation = self.store.scope_discovery_generation(
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                );
                let filesystem_revision = self.fs.as_ref().map(|fs| fs.revision(&scope.root_id));
                let (next, completed, code) = self
                    .walk_scope(
                        &current, scope, &root, &selected, resolver, checkpoint, generation,
                    )
                    .await;
                current = next;
                // GH-444: a transient mid-walk stall must not fail the whole
                // run. Re-walk boundedly (worst case 3 walks + 2 sleeps);
                // only the exact WALK_TIMEOUT code retries.
                if !completed && code.as_deref() == Some(failure_codes::WALK_TIMEOUT) {
                    current = self
                        .store
                        .get_run(&run.id)
                        .map(|(run, _, _)| run)
                        .unwrap_or(current);
                    if restarts < 2
                        && current.state == ScanState::Discovering
                        && checkpoint.check(&run.id, &scope.policy_revision)
                    {
                        restarts += 1;
                        tokio::time::sleep(self.walk_deadline).await;
                        current = self
                            .store
                            .get_run(&run.id)
                            .map(|(run, _, _)| run)
                            .unwrap_or(current);
                        if current.state != ScanState::Discovering
                            || !checkpoint.check(&run.id, &scope.policy_revision)
                        {
                            break (false, code);
                        }
                        tracing::warn!(run_id = %run.id, attempt = restarts, "library_scan event=walk_timeout_retry");
                        self.store.restart_scope_discovery(
                            &run.id,
                            &scope.root_id,
                            &scope.relative_path,
                        );
                        current = self
                            .store
                            .get_run(&run.id)
                            .map(|(run, _, _)| run)
                            .unwrap_or(current);
                        self.store.cleanup_stale_inventory(&run.id);
                        continue;
                    }
                }
                if !completed || self.fs.is_none() {
                    break (completed, code);
                }
                let Some(fs) = self.fs.as_ref() else {
                    break (completed, code);
                };
                let _lease = fs.read(&scope.root_id).await;
                if fs.revision(&scope.root_id) == filesystem_revision.unwrap_or(0) {
                    // Only a clean, un-degraded walk records
                    // the fence; a partially-read scope keeps the
                    // reconciler conservative.
                    if code.is_none() && !superseded_scope {
                        fs.record_scan_revision(&run.id, &scope.root_id);
                    }
                    break (true, code);
                }
                restarts += 1;
                if restarts >= 3 {
                    // Sustained concurrent publication would
                    // otherwise re-walk this scope forever.
                    tracing::warn!(run_id = %run.id, restarts, "library_scan event=walk_superseded");
                    superseded_scope = true;
                    break (true, code);
                }
                drop(_lease);
                self.store
                    .restart_scope_discovery(&run.id, &scope.root_id, &scope.relative_path);
                current = self
                    .store
                    .get_run(&run.id)
                    .map(|(run, _, _)| run)
                    .unwrap_or(current);
                self.store.cleanup_stale_inventory(&run.id);
            };
            if !completed {
                current = self
                    .store
                    .get_run(&run.id)
                    .map(|(run, _, _)| run)
                    .unwrap_or(current);
                if current.state == ScanState::Discovering {
                    let code = walk_failure_code
                        .clone()
                        .unwrap_or_else(|| failure_codes::ROOT_PERMISSION_DENIED.to_owned());
                    current = self.fail_run(&run.id, current.state, current.row_revision, &code);
                }
                return current;
            }
            if superseded_scope {
                self.store.complete_scope_discovery(
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                    ScopeDiscoveryState::PartiallyRead,
                    Some(failure_codes::WALK_SUPERSEDED),
                );
            } else if let Some(code) = &walk_failure_code {
                self.store.complete_scope_discovery(
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                    ScopeDiscoveryState::PartiallyRead,
                    Some(code),
                );
            } else {
                self.store.complete_scope_discovery(
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                    ScopeDiscoveryState::Completed,
                    None,
                );
            }
        }
        if unavailable_scopes > 0 && unavailable_scopes == scopes.len() {
            // GH-296: every scope proved unreachable, so the run fails
            // instead of completing silently green.
            tracing::warn!(run_id = %run.id, count = unavailable_scopes, "library_scan event=all_scopes_unavailable");
            return self.fail_run(
                &run.id,
                current.state,
                current.row_revision,
                failure_codes::ROOT_UNAVAILABLE,
            );
        }
        current
    }

    /// Walk one scope and persist its inventory (v2 `_walk_scope`).
    #[allow(clippy::too_many_arguments)]
    async fn walk_scope<C: Checkpoint>(
        &self,
        run: &ScanRun,
        scope: &ScanScope,
        root: &Path,
        selected: &Path,
        resolver: &PolicyResolver,
        checkpoint: &C,
        generation: u64,
    ) -> (ScanRun, bool, Option<String>) {
        let (sender, mut receiver) = mpsc::channel::<WalkMessage>(INVENTORY_QUEUE_SIZE);
        let stopped = Arc::new(AtomicBool::new(false));
        let _stop_guard = StopGuard(Arc::clone(&stopped));
        let heartbeat = Arc::new(WalkHeartbeat::new());
        let producer_heartbeat = Arc::clone(&heartbeat);
        let producer_stopped = Arc::clone(&stopped);
        let producer_selected = selected.to_owned();
        let producer_root = root.to_owned();
        let pool = self.pool.clone();
        let mut producer = Some(tokio::spawn(async move {
            pool.run(move || {
                produce_inventory(
                    &producer_selected,
                    &producer_root,
                    &sender,
                    &producer_stopped,
                    &producer_heartbeat,
                )
            })
            .await;
        }));

        let mut batch: Vec<(PathBuf, DiscoveredFile, String)> = Vec::new();
        let mut current = run.clone();
        let mut row_revision = run.row_revision;
        let mut completed = true;
        let mut discard_remaining = false;
        let mut detached = false;
        // A checkpoint-false exit is pause/stop/supersede,
        // not a filesystem error.
        let mut control_exit = false;
        let mut walk_failure_code: Option<String> = None;
        // First degraded code becomes the scope diagnostic.
        let mut degraded_code: Option<String> = None;
        let mut stale_cleanup_pending = true;
        let mut last_checkpoint = Instant::now();
        let mut last_log = Instant::now();
        let mut last_item_at = Instant::now();
        // Distinct persisted keys within one discovery generation.
        let mut seen: HashSet<String> = HashSet::new();
        let mut discovered = 0usize;

        loop {
            let item = match tokio::time::timeout(Duration::from_millis(250), receiver.recv()).await
            {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(_) => {
                    if heartbeat.age() > self.walk_deadline
                        && last_item_at.elapsed() > self.walk_deadline
                    {
                        completed = false;
                        stopped.store(true, Ordering::Relaxed);
                        walk_failure_code = Some(failure_codes::WALK_TIMEOUT.to_owned());
                        let last = heartbeat.last_directory();
                        tracing::warn!(run_id = %run.id, "library_scan event=walk_timeout");
                        self.record_failure(
                            &run.id,
                            scope,
                            if last.is_empty() {
                                scope.relative_path.clone()
                            } else {
                                relativize(Path::new(&last), root)
                            },
                            failure_codes::WALK_TIMEOUT,
                            format!(
                                "The directory walk made no delivered progress for {:.1}s.",
                                self.walk_deadline.as_secs_f64()
                            ),
                        );
                        // The producer is wedged in a syscall; awaiting it
                        // would wedge the scan worker, so it is detached
                        // instead (never awaited either way). This
                        // branch breaks right after, so the take below only
                        // misses if the branch ever re-runs; skipping then
                        // keeps the timeout in force instead of panicking.
                        if let Some(handle) = producer.take()
                            && !self.detach_walker(handle)
                        {
                            walk_failure_code = Some(failure_codes::WALKER_UNAVAILABLE.to_owned());
                            self.record_failure(
                                &run.id,
                                scope,
                                scope.relative_path.clone(),
                                failure_codes::WALKER_UNAVAILABLE,
                                "Too many wedged directory walks are still in flight; the walk was not started.".to_owned(),
                            );
                        }
                        detached = true;
                        break;
                    }
                    if !checkpoint.check(&run.id, &scope.policy_revision) {
                        completed = false;
                        stopped.store(true, Ordering::Relaxed);
                        discard_remaining = true;
                        control_exit = true;
                    }
                    last_checkpoint = Instant::now();
                    continue;
                }
            };
            last_item_at = Instant::now();
            match item {
                WalkMessage::Done => break,
                _ if discard_remaining => continue,
                WalkMessage::Skips(skips) => {
                    for (relative_path, failure_code) in skips {
                        let failure_detail = match failure_code.as_str() {
                            failure_codes::SYMLINK_ESCAPE_OUT => {
                                "A symbolic link resolves outside its library root; it was not followed."
                            }
                            failure_codes::NON_REGULAR_FILE => {
                                "The path is not a regular file (FIFO, socket, or device node); it was skipped without reading tags."
                            }
                            _ => "A filename is not valid UTF-8; the file was skipped.",
                        }
                        .to_owned();
                        self.record_failure(
                            &run.id,
                            scope,
                            relative_path,
                            &failure_code,
                            failure_detail,
                        );
                    }
                }
                WalkMessage::WalkError(error) => {
                    // GH-296: record the row but keep consuming.
                    if degraded_code.is_none() {
                        degraded_code = Some(error.code.clone());
                    }
                    tracing::warn!(run_id = %run.id, path = %relativize(&error.path, root), "library_scan event=walk_error");
                    self.record_failure(
                        &run.id,
                        scope,
                        relativize(&error.path, root),
                        &error.code,
                        error.detail,
                    );
                }
                WalkMessage::File(file) => {
                    // An in-root alias resolves onto its target's
                    // own path; dedupe against everything already
                    // persisted in this generation.
                    let key = inventory_key(&file.resolved, root);
                    if !seen.insert(key.clone()) {
                        // Collision rule: first wins, the
                        // loser gets an inventory-phase failure row naming
                        // its own on-disk form, and the run stays green.
                        self.record_failure(
                            &run.id,
                            scope,
                            text_safe_posix(
                                file.resolved.strip_prefix(root).unwrap_or(&file.resolved),
                            ),
                            failure_codes::NFC_TWIN_COLLISION,
                            "Two on-disk names normalize to the same inventory key; the first file won and this twin was skipped.".to_owned(),
                        );
                        continue;
                    }
                    batch.push((file.resolved.clone(), file, key));
                    if batch.len() >= INVENTORY_BATCH_SIZE {
                        let (revision, persisted) = self.persist_batch(
                            &run.id,
                            row_revision,
                            scope,
                            &batch,
                            resolver,
                            generation,
                        );
                        row_revision = revision;
                        // A skipped batch degrades the scope: its files are
                        // absent from inventory, so the scope must not read
                        // Completed and let reconcile delete their catalog
                        // rows as missing.
                        if persisted {
                            discovered += batch.len();
                        } else if degraded_code.is_none() {
                            degraded_code = Some(failure_codes::WALK_ERROR.to_owned());
                        }
                        batch.clear();
                        if stale_cleanup_pending {
                            stale_cleanup_pending = self.store.cleanup_stale_inventory(&run.id) > 0;
                        }
                        if !checkpoint.check(&run.id, &scope.policy_revision) {
                            completed = false;
                            stopped.store(true, Ordering::Relaxed);
                            discard_remaining = true;
                            control_exit = true;
                        }
                        last_checkpoint = Instant::now();
                    } else if last_checkpoint.elapsed() >= Duration::from_millis(250) {
                        if !checkpoint.check(&run.id, &scope.policy_revision) {
                            completed = false;
                            stopped.store(true, Ordering::Relaxed);
                            discard_remaining = true;
                            control_exit = true;
                        }
                        last_checkpoint = Instant::now();
                    }
                    if last_log.elapsed() >= Duration::from_secs(30) {
                        tracing::info!(
                            discovered = discovered + batch.len(),
                            "library_scan event=discovery_progress"
                        );
                        last_log = Instant::now();
                    }
                }
            }
        }
        if !batch.is_empty() && completed {
            let (revision, persisted) =
                self.persist_batch(&run.id, row_revision, scope, &batch, resolver, generation);
            row_revision = revision;
            if !persisted && degraded_code.is_none() {
                degraded_code = Some(failure_codes::WALK_ERROR.to_owned());
            }
            if stale_cleanup_pending {
                self.store.cleanup_stale_inventory(&run.id);
            }
        }
        if !detached && let Some(handle) = producer.take() {
            let _ = handle.await;
        }
        current.row_revision = row_revision;
        if !completed {
            let scope_error = if control_exit && walk_failure_code.is_none() {
                None
            } else {
                Some(
                    walk_failure_code
                        .clone()
                        .unwrap_or_else(|| failure_codes::ROOT_PERMISSION_DENIED.to_owned()),
                )
            };
            self.store.complete_scope_discovery(
                &run.id,
                &scope.root_id,
                &scope.relative_path,
                ScopeDiscoveryState::PartiallyRead,
                scope_error.as_deref(),
            );
        } else if let Some(code) = &degraded_code {
            // The walk finished, but some paths were unreadable.
            self.store.complete_scope_discovery(
                &run.id,
                &scope.root_id,
                &scope.relative_path,
                ScopeDiscoveryState::PartiallyRead,
                Some(code),
            );
        }
        if degraded_code.is_none() {
            (current, completed, walk_failure_code)
        } else {
            (current, completed, degraded_code)
        }
    }

    /// Classify and persist one inventory page (v2 `_persist_batch`).
    /// Returns the revision plus whether the batch landed: a skipped
    /// batch degrades the scope (its files are invisible to reconcile),
    /// so the outcome must reach the caller, not just the log.
    #[allow(clippy::too_many_arguments)]
    fn persist_batch(
        &self,
        run_id: &str,
        row_revision: u64,
        scope: &ScanScope,
        batch: &[(PathBuf, DiscoveredFile, String)],
        resolver: &PolicyResolver,
        generation: u64,
    ) -> (u64, bool) {
        // Items build on demand: the first attempt moves its batch, and
        // only the rare stale retry rebuilds (reclassifying is cheap
        // against the store's snapshot). Cloning every batch doubled
        // discovery's string churn for a path that never fires.
        let build_items = || {
            let inputs: Vec<ClassifyInput> = batch
                .iter()
                .map(|(_, file, key)| {
                    (
                        key.clone(),
                        file.size_bytes,
                        file.mtime_ns,
                        file.mtime_secs,
                        file.revision.clone(),
                    )
                })
                .collect();
            let verdicts = self.store.classify(&scope.root_id, &inputs, Some(run_id));
            let mut items = Vec::with_capacity(batch.len());
            for ((path, file, key), _) in batch.iter().zip(inputs.iter()) {
                let policy = resolver.resolve(path).unwrap_or(scope.effective_policy);
                let (mut verdict, track_id) =
                    verdicts.get(key).cloned().unwrap_or((Verdict::New, None));
                if policy == EffectivePolicy::Excluded {
                    verdict = Verdict::Excluded;
                }
                items.push(ScanInventoryItem {
                    root_id: scope.root_id.clone(),
                    relative_path: key.clone(),
                    absolute_path: path.display().to_string(),
                    file_size_bytes: file.size_bytes,
                    file_mtime_ns: file.mtime_ns,
                    stat_revision: file.revision.clone(),
                    effective_policy: policy,
                    comparison_result: verdict,
                    policy_revision: scope.policy_revision.clone(),
                    local_track_id: track_id,
                    scope_relative_path: scope.relative_path.clone(),
                });
            }
            items
        };
        // Single-worker stores never go stale here; retry once with a
        // fresh revision so a racing writer costs one retry instead of a
        // failed run. A second stale is recorded and the batch
        // is skipped: losing one page with a failure row beats failing
        // the whole run. Any other store error (the SQLite store already
        // retried its lock races) is recorded the same way: a
        // skipped page must never vanish silently.
        match self.store.add_inventory_batch(
            run_id,
            build_items(),
            row_revision,
            (self.clock)(),
            generation,
        ) {
            Ok(revision) => (revision, true),
            Err(ScanStoreError::StaleRevision { .. }) => {
                let fresh = self
                    .store
                    .get_run(run_id)
                    .map(|(run, _, _)| run.row_revision)
                    .unwrap_or(row_revision);
                match self.store.add_inventory_batch(
                    run_id,
                    build_items(),
                    fresh,
                    (self.clock)(),
                    generation,
                ) {
                    Ok(revision) => (revision, true),
                    Err(error) => {
                        tracing::error!(%error, "scan inventory batch lost its retry");
                        self.record_failure(
                            run_id,
                            scope,
                            scope.relative_path.clone(),
                            failure_codes::WALK_ERROR,
                            "IoError while walking.".to_owned(),
                        );
                        (fresh, false)
                    }
                }
            }
            Err(error) => {
                tracing::error!(%error, "scan inventory batch failed");
                self.record_failure(
                    run_id,
                    scope,
                    scope.relative_path.clone(),
                    failure_codes::WALK_ERROR,
                    "The inventory page failed to persist; the page was skipped.".to_owned(),
                );
                (row_revision, false)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    Unavailable,
    Wedged,
}

struct StopGuard(Arc<AtomicBool>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Blocking walk producing inventory messages (v2 `producer`). Runs on the
/// pool: stat-only, symlink-cautious, skip-and-report.
fn produce_inventory(
    selected: &Path,
    root: &Path,
    sender: &mpsc::Sender<WalkMessage>,
    stopped: &AtomicBool,
    heartbeat: &WalkHeartbeat,
) {
    let mut stack = vec![selected.to_owned()];
    // Unreadable directories are collected and reported instead of
    // aborting the walk.
    let mut walk_errors: Vec<WalkErrorInfo> = Vec::new();
    match std::fs::read_dir(selected) {
        Ok(_) => {}
        Err(error) => {
            let info = WalkErrorInfo {
                path: selected.to_owned(),
                code: walk_error_code(&error),
                detail: walk_failure_detail(&error),
            };
            send_with_stop(sender, stopped, WalkMessage::WalkError(info));
            send_with_stop(sender, stopped, WalkMessage::Done);
            return;
        }
    }
    while let Some(directory) = stack.pop() {
        if stopped.load(Ordering::Relaxed) {
            break;
        }
        heartbeat.touch(&directory.display().to_string());
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries.collect::<Vec<_>>(),
            Err(error) => {
                walk_errors.push(WalkErrorInfo {
                    path: directory.clone(),
                    code: walk_error_code(&error),
                    detail: walk_failure_detail(&error),
                });
                continue;
            }
        };
        let mut skips: Vec<(String, String)> = Vec::new();
        let mut inspected: Vec<Result<DiscoveredFile, WalkErrorInfo>> = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    walk_errors.push(WalkErrorInfo {
                        path: directory.clone(),
                        code: walk_error_code(&error),
                        detail: walk_failure_detail(&error),
                    });
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    walk_errors.push(WalkErrorInfo {
                        path: entry.path(),
                        code: walk_error_code(&error),
                        detail: walk_failure_detail(&error),
                    });
                    continue;
                }
            };
            let path = entry.path();
            if file_type.is_dir() {
                if !is_management_artifact(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !is_audio_file(&path) || is_management_artifact(&path) {
                continue;
            }
            if path.to_str().is_none() {
                // Non-UTF-8 names would poison downstream TEXT
                // binds; skip and report with a percent-encoded key.
                let relative = path
                    .strip_prefix(root)
                    .map(text_safe_posix)
                    .unwrap_or_else(|_| text_safe_posix(&path));
                skips.push((relative, failure_codes::WALK_NAME_ENCODING.to_owned()));
                continue;
            }
            // In-root file symlinks resolve onto their target's own path;
            // escape-out links are audited below (symlinks are never
            // followed into the library).
            let resolved = if file_type.is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(resolved) => resolved,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        inspected.push(Err(WalkErrorInfo {
                            path: path.clone(),
                            code: walk_error_code(&error),
                            detail: walk_failure_detail(&error),
                        }));
                        continue;
                    }
                }
            } else {
                path.clone()
            };
            if file_type.is_symlink() && !resolved.starts_with(root) {
                let relative = path
                    .strip_prefix(root)
                    .map(text_safe_posix)
                    .unwrap_or_else(|_| text_safe_posix(&path));
                skips.push((relative, failure_codes::SYMLINK_ESCAPE_OUT.to_owned()));
                continue;
            }
            let meta = match std::fs::metadata(&resolved) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    inspected.push(Err(WalkErrorInfo {
                        path: path.clone(),
                        code: walk_error_code(&error),
                        detail: walk_failure_detail(&error),
                    }));
                    continue;
                }
            };
            if file_type.is_symlink() && meta.is_dir() {
                // A symlinked directory is recorded as nothing and
                // never descended.
                continue;
            }
            if !meta.is_file() {
                // FIFOs/sockets/devices with an audio suffix would
                // wedge a tag reader in open(); skip them in the
                // inventory phase without touching the tag-reader budget.
                let relative = path
                    .strip_prefix(root)
                    .map(text_safe_posix)
                    .unwrap_or_else(|_| text_safe_posix(&path));
                skips.push((relative, failure_codes::NON_REGULAR_FILE.to_owned()));
                continue;
            }
            let mtime_ns = mtime_ns_from_metadata(&meta);
            inspected.push(Ok(DiscoveredFile {
                resolved,
                size_bytes: meta.len(),
                mtime_ns,
                mtime_secs: mtime_ns as f64 / 1_000_000_000.0,
                revision: exact_stat_revision(meta.len(), mtime_ns),
            }));
        }
        if !skips.is_empty() && !send_with_stop(sender, stopped, WalkMessage::Skips(skips)) {
            break;
        }
        let mut halted = false;
        for item in inspected {
            let message = match item {
                Ok(file) => WalkMessage::File(file),
                Err(error) => WalkMessage::WalkError(error),
            };
            if !send_with_stop(sender, stopped, message) {
                halted = true;
                break;
            }
            // Delivery is a progress signal, not just reads.
            heartbeat.touch(&directory.display().to_string());
        }
        if halted {
            break;
        }
    }
    for error in walk_errors {
        if !send_with_stop(sender, stopped, WalkMessage::WalkError(error)) {
            break;
        }
    }
    send_with_stop(sender, stopped, WalkMessage::Done);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_suffix_match_is_case_insensitive() {
        assert!(is_audio_file(Path::new("a.FLAC")));
        assert!(is_audio_file(Path::new("a.Opus")));
        assert!(!is_audio_file(Path::new("a.txt")));
        assert!(!is_audio_file(Path::new("aflac")));
    }

    #[test]
    fn inventory_key_normalizes_nfd_to_nfc() {
        let root = Path::new("/music");
        let nfd: String = "cafe\u{301}.flac".to_owned();
        let key = inventory_key(&Path::new("/music").join(&nfd), root);
        assert_eq!(key, "caf\u{e9}.flac");
    }
}
