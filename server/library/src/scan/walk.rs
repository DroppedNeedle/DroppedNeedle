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

use super::fs::is_management_artifact;
use super::models::{
    EffectivePolicy, Event, FailureCode, Phase, RunState, ScanFailureRecord, ScanInventoryItem,
    ScanPhase, ScanRun, ScanScope, ScopeDiscoveryState, Verdict,
};
use super::pool::BlockingPool;
use super::revision::{exact_stat_revision, mtime_ns_from_metadata};
use super::roots::PolicyResolver;
use super::seams::Checkpoint;
use super::sqlite_store::SqliteScanStore;
use super::stop::RunStop;
use super::store::{ClassifyInput, ScanStoreError};
use crate::clock::LibraryClock as _;

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

fn walk_error_code(error: &std::io::Error) -> FailureCode {
    match error.raw_os_error() {
        Some(1) => FailureCode::WalkEperm,
        Some(2) => FailureCode::WalkEnoent,
        Some(5) => FailureCode::WalkEio,
        Some(13) => FailureCode::WalkEacces,
        Some(20) => FailureCode::WalkEnotdir,
        Some(21) => FailureCode::WalkEisdir,
        Some(22) => FailureCode::WalkEinval,
        Some(23) => FailureCode::WalkEnfile,
        Some(24) => FailureCode::WalkEmfile,
        Some(28) => FailureCode::WalkEnospc,
        Some(30) => FailureCode::WalkErofs,
        Some(36) => FailureCode::WalkEnametoolong,
        Some(40) => FailureCode::WalkEloop,
        Some(_) => FailureCode::WalkEunknown,
        None => FailureCode::WalkError,
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
    code: FailureCode,
    detail: String,
}

#[derive(Debug, Clone)]
enum WalkMessage {
    File(DiscoveredFile),
    Skips(Vec<(String, FailureCode)>),
    WalkError(WalkErrorInfo),
    /// A folder on another filesystem than the folder above it, as its
    /// inventory key.
    Mount(String),
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

/// One-walk bounded-queue inventory scanner (v2 `LibraryInventoryScanner`).
pub struct InventoryScanner {
    store: Arc<SqliteScanStore>,
    pool: BlockingPool,
    walk_deadline: Duration,
    max_detached_walkers: usize,
    detached: Arc<Mutex<HashMap<u64, Instant>>>,
    next_detached_id: AtomicU64,
    detached_reap_multiplier: f64,
    pending_probes: Arc<AtomicUsize>,
    probe_max_workers: usize,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl InventoryScanner {
    pub fn new(
        store: Arc<SqliteScanStore>,
        pool: BlockingPool,
        clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self {
            store,
            pool,
            walk_deadline: Duration::from_secs_f64(DEFAULT_WALK_DEADLINE_SECS),
            max_detached_walkers: DEFAULT_MAX_DETACHED_WALKERS,
            detached: Arc::new(Mutex::new(HashMap::new())),
            next_detached_id: AtomicU64::new(1),
            detached_reap_multiplier: DETACHED_WALKER_REAP_MULTIPLIER,
            pending_probes: Arc::new(AtomicUsize::new(0)),
            probe_max_workers: 1,
            clock,
        }
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
        stop: &RunStop,
        run_id: &str,
        scope: &ScanScope,
        relative_path: String,
        failure_code: FailureCode,
        failure_detail: String,
    ) -> Result<(), ScanStoreError> {
        self.store.record_failures(
            stop,
            run_id,
            &[ScanFailureRecord {
                root_id: scope.root_id.clone(),
                relative_path,
                failure_code,
                recorded_at: self.clock.now_secs(),
                failure_detail,
                phase: ScanPhase::Discovering,
            }],
        )
    }

    /// End the run as failed. A run that moved meanwhile (a control or a
    /// supersede landed first) is answered as it is now.
    fn fail_run(
        &self,
        run_id: &str,
        expected_revision: u64,
        code: FailureCode,
    ) -> Result<ScanRun, ScanStoreError> {
        match self.store.transition(
            &RunStop::never(),
            run_id,
            expected_revision,
            Event::Fail,
            self.clock.now_secs(),
            Some(code),
        ) {
            Err(ScanStoreError::StaleRevision { .. }) => Ok(self.store.get_run(run_id)?.0),
            other => other,
        }
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
            Ok(Ok(Ok(exists))) => Ok(exists),
            Ok(Ok(Err(_)) | Err(_)) => Err(ProbeOutcome::Wedged),
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
        stop: &RunStop,
    ) -> Result<ScanRun, ScanStoreError> {
        // GH-296 skip-and-report: a scope whose root cannot be resolved or
        // probed is recorded while remaining scopes keep walking.
        // The run fails wholesale only when nothing was discoverable.
        let mut current = run.clone();
        let mut unavailable_scopes = 0usize;
        for scope in scopes {
            if self.store.scope_discovery_state(
                stop,
                &run.id,
                &scope.root_id,
                &scope.relative_path,
            )? == ScopeDiscoveryState::Completed
            {
                continue;
            }
            if !checkpoint.check(&run.id, &scope.policy_revision) {
                return Ok(self.store.reread_run(stop, &run.id)?.0);
            }
            let root = root_paths
                .get(&scope.root_id)
                .cloned()
                .or_else(|| scope.root_path.as_ref().map(PathBuf::from));
            let Some(root) = root else {
                self.record_failure(
                    stop,
                    &run.id,
                    scope,
                    scope.relative_path.clone(),
                    FailureCode::RootUnavailable,
                    "The library root has no configured path.".to_owned(),
                )?;
                self.store.complete_scope_discovery(
                    stop,
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                    ScopeDiscoveryState::Unavailable,
                    Some(FailureCode::RootUnavailable),
                )?;
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
                        stop,
                        &run.id,
                        scope,
                        scope.relative_path.clone(),
                        FailureCode::ProbeUnavailable,
                        "A previous root probe never completed; scanning is paused until the filesystem responds or the service restarts.".to_owned(),
                    )?;
                    self.store.complete_scope_discovery(
                        stop,
                        &run.id,
                        &scope.root_id,
                        &scope.relative_path,
                        ScopeDiscoveryState::Unavailable,
                        Some(FailureCode::ProbeUnavailable),
                    )?;
                    return self.fail_run(
                        &run.id,
                        current.row_revision,
                        FailureCode::ProbeUnavailable,
                    );
                }
                Err(ProbeOutcome::Wedged) => {
                    tracing::warn!(run_id = %run.id, root_id = %scope.root_id, "library_scan event=walk_timeout");
                    self.record_failure(
                        stop,
                        &run.id,
                        scope,
                        scope.relative_path.clone(),
                        FailureCode::WalkTimeout,
                        format!(
                            "The library root probe exceeded {:.1}s.",
                            self.walk_deadline.as_secs_f64()
                        ),
                    )?;
                    self.store.complete_scope_discovery(
                        stop,
                        &run.id,
                        &scope.root_id,
                        &scope.relative_path,
                        ScopeDiscoveryState::Unavailable,
                        Some(FailureCode::WalkTimeout),
                    )?;
                    return self.fail_run(&run.id, current.row_revision, FailureCode::WalkTimeout);
                }
                Ok(false) => {
                    self.record_failure(
                        stop,
                        &run.id,
                        scope,
                        scope.relative_path.clone(),
                        FailureCode::RootUnavailable,
                        format!("The library root path is missing: {}", scope.relative_path),
                    )?;
                    self.store.complete_scope_discovery(
                        stop,
                        &run.id,
                        &scope.root_id,
                        &scope.relative_path,
                        ScopeDiscoveryState::Unavailable,
                        Some(FailureCode::RootUnavailable),
                    )?;
                    unavailable_scopes += 1;
                    continue;
                }
                Ok(true) => {}
            }
            let mut restarts = 0u32;
            let (completed, walk_failure_code) = loop {
                let generation = self.store.scope_discovery_generation(
                    stop,
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                )?;
                self.store
                    .begin_scope_walk(stop, &run.id, &scope.root_id, &scope.relative_path)?;
                let (next, completed, code) = self
                    .walk_scope(
                        &current, scope, &root, &selected, resolver, checkpoint, generation, stop,
                    )
                    .await?;
                current = next;
                // GH-444: a transient mid-walk stall must not fail the whole
                // run. Re-walk boundedly (worst case 3 walks + 2 sleeps);
                // only the exact WALK_TIMEOUT code retries.
                if !completed && code == Some(FailureCode::WalkTimeout) {
                    current = self.store.reread_run(stop, &run.id)?.0;
                    if restarts < 2
                        && current.state == RunState::Running(Phase::Discovering)
                        && checkpoint.check(&run.id, &scope.policy_revision)
                    {
                        restarts += 1;
                        tokio::time::sleep(self.walk_deadline).await;
                        current = self.store.reread_run(stop, &run.id)?.0;
                        if current.state != RunState::Running(Phase::Discovering)
                            || !checkpoint.check(&run.id, &scope.policy_revision)
                        {
                            break (false, code);
                        }
                        tracing::warn!(run_id = %run.id, attempt = restarts, "library_scan event=walk_timeout_retry");
                        self.store.restart_scope_discovery(
                            stop,
                            &run.id,
                            &scope.root_id,
                            &scope.relative_path,
                        )?;
                        current = self.store.reread_run(stop, &run.id)?.0;
                        self.store.cleanup_stale_inventory(stop, &run.id)?;
                        continue;
                    }
                }
                break (completed, code);
            };
            if !completed {
                current = self.store.reread_run(stop, &run.id)?.0;
                if current.state == RunState::Running(Phase::Discovering) {
                    let code = walk_failure_code.unwrap_or(FailureCode::RootPermissionDenied);
                    current = self.fail_run(&run.id, current.row_revision, code)?;
                }
                return Ok(current);
            }
            if let Some(code) = walk_failure_code {
                self.store.complete_scope_discovery(
                    stop,
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                    ScopeDiscoveryState::PartiallyRead,
                    Some(code),
                )?;
            } else {
                self.store.complete_scope_discovery(
                    stop,
                    &run.id,
                    &scope.root_id,
                    &scope.relative_path,
                    ScopeDiscoveryState::Completed,
                    None,
                )?;
            }
        }
        if unavailable_scopes > 0 && unavailable_scopes == scopes.len() {
            // GH-296: every scope proved unreachable, so the run fails
            // instead of completing silently green.
            tracing::warn!(run_id = %run.id, count = unavailable_scopes, "library_scan event=all_scopes_unavailable");
            return self.fail_run(&run.id, current.row_revision, FailureCode::RootUnavailable);
        }
        Ok(current)
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
        stop: &RunStop,
    ) -> Result<(ScanRun, bool, Option<FailureCode>), ScanStoreError> {
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
            // A panic drops the sender before `Done`; the consumer reads
            // that as an incomplete walk.
            let _ = pool
                .run(move || {
                    produce_inventory(
                        &producer_selected,
                        &producer_root,
                        &sender,
                        &producer_stopped,
                        &producer_heartbeat,
                        &folder_device,
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
        let mut walk_failure_code: Option<FailureCode> = None;
        // First degraded code becomes the scope diagnostic.
        let mut degraded_code: Option<FailureCode> = None;
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
                Ok(None) => {
                    // The producer ended without `Done`: it panicked. What
                    // it delivered is not the whole scope, so reconcile
                    // must not read the gap as missing files.
                    completed = false;
                    walk_failure_code = Some(FailureCode::WalkError);
                    self.record_failure(
                        stop,
                        &run.id,
                        scope,
                        scope.relative_path.clone(),
                        FailureCode::WalkError,
                        "The directory walk stopped unexpectedly.".to_owned(),
                    )?;
                    break;
                }
                Err(_) => {
                    if heartbeat.age() > self.walk_deadline
                        && last_item_at.elapsed() > self.walk_deadline
                    {
                        completed = false;
                        stopped.store(true, Ordering::Relaxed);
                        walk_failure_code = Some(FailureCode::WalkTimeout);
                        let last = heartbeat.last_directory();
                        tracing::warn!(run_id = %run.id, "library_scan event=walk_timeout");
                        self.record_failure(
                            stop,
                            &run.id,
                            scope,
                            if last.is_empty() {
                                scope.relative_path.clone()
                            } else {
                                relativize(Path::new(&last), root)
                            },
                            FailureCode::WalkTimeout,
                            format!(
                                "The directory walk made no delivered progress for {:.1}s.",
                                self.walk_deadline.as_secs_f64()
                            ),
                        )?;
                        // The producer is wedged in a syscall; awaiting it
                        // would wedge the scan worker, so it is detached
                        // instead (never awaited either way). This
                        // branch breaks right after, so the take below only
                        // misses if the branch ever re-runs; skipping then
                        // keeps the timeout in force instead of panicking.
                        if let Some(handle) = producer.take()
                            && !self.detach_walker(handle)
                        {
                            walk_failure_code = Some(FailureCode::WalkerUnavailable);
                            self.record_failure(
                                stop,
                                &run.id,
                                scope,
                                scope.relative_path.clone(),
                                FailureCode::WalkerUnavailable,
                                "Too many wedged directory walks are still in flight; the walk was not started.".to_owned(),
                            )?;
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
                // Recorded however the run ends.
                WalkMessage::Mount(relative_path) => {
                    self.store
                        .record_mount(stop, &scope.root_id, &relative_path)?;
                }
                _ if discard_remaining => continue,
                WalkMessage::Skips(skips) => {
                    for (relative_path, failure_code) in skips {
                        let failure_detail = match failure_code {
                            FailureCode::SymlinkEscapeOut => {
                                "A symbolic link resolves outside its library root; it was not followed."
                            }
                            FailureCode::NonRegularFile => {
                                "The path is not a regular file (FIFO, socket, or device node); it was skipped without reading tags."
                            }
                            _ => "A filename is not valid UTF-8; the file was skipped.",
                        }
                        .to_owned();
                        self.record_failure(
                            stop,
                            &run.id,
                            scope,
                            relative_path,
                            failure_code,
                            failure_detail,
                        )?;
                    }
                }
                WalkMessage::WalkError(error) => {
                    // GH-296: record the row but keep consuming.
                    if degraded_code.is_none() {
                        degraded_code = Some(error.code);
                    }
                    tracing::warn!(run_id = %run.id, path = %relativize(&error.path, root), "library_scan event=walk_error");
                    self.record_failure(
                        stop,
                        &run.id,
                        scope,
                        relativize(&error.path, root),
                        error.code,
                        error.detail,
                    )?;
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
                            stop,
                            &run.id,
                            scope,
                            text_safe_posix(
                                file.resolved.strip_prefix(root).unwrap_or(&file.resolved),
                            ),
                            FailureCode::NfcTwinCollision,
                            "Two on-disk names normalize to the same inventory key; the first file won and this twin was skipped.".to_owned(),
                        )?;
                        continue;
                    }
                    batch.push((file.resolved.clone(), file, key));
                    if batch.len() >= INVENTORY_BATCH_SIZE {
                        let (revision, persisted) = self.persist_batch(
                            stop,
                            &run.id,
                            row_revision,
                            scope,
                            &batch,
                            resolver,
                            generation,
                        )?;
                        row_revision = revision;
                        // A skipped batch degrades the scope: its files are
                        // absent from inventory, so the scope must not read
                        // Completed and let reconcile delete their catalog
                        // rows as missing.
                        if persisted {
                            discovered += batch.len();
                        } else if degraded_code.is_none() {
                            degraded_code = Some(FailureCode::WalkError);
                        }
                        batch.clear();
                        if stale_cleanup_pending {
                            self.store.cleanup_stale_inventory(stop, &run.id)?;
                            stale_cleanup_pending = false;
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
            let (revision, persisted) = self.persist_batch(
                stop,
                &run.id,
                row_revision,
                scope,
                &batch,
                resolver,
                generation,
            )?;
            row_revision = revision;
            if !persisted && degraded_code.is_none() {
                degraded_code = Some(FailureCode::WalkError);
            }
            if stale_cleanup_pending {
                self.store.cleanup_stale_inventory(stop, &run.id)?;
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
                Some(walk_failure_code.unwrap_or(FailureCode::RootPermissionDenied))
            };
            self.store.complete_scope_discovery(
                stop,
                &run.id,
                &scope.root_id,
                &scope.relative_path,
                ScopeDiscoveryState::PartiallyRead,
                scope_error,
            )?;
        } else if let Some(code) = degraded_code {
            // The walk finished, but some paths were unreadable.
            self.store.complete_scope_discovery(
                stop,
                &run.id,
                &scope.root_id,
                &scope.relative_path,
                ScopeDiscoveryState::PartiallyRead,
                Some(code),
            )?;
        }
        if degraded_code.is_none() {
            Ok((current, completed, walk_failure_code))
        } else {
            Ok((current, completed, degraded_code))
        }
    }

    /// Classify and persist one inventory page (v2 `_persist_batch`).
    /// Returns the revision plus whether the batch landed: a skipped
    /// batch degrades the scope (its files are invisible to reconcile),
    /// so the outcome must reach the caller, not just the log.
    #[allow(clippy::too_many_arguments)]
    fn persist_batch(
        &self,
        stop: &RunStop,
        run_id: &str,
        row_revision: u64,
        scope: &ScanScope,
        batch: &[(PathBuf, DiscoveredFile, String)],
        resolver: &PolicyResolver,
        generation: u64,
    ) -> Result<(u64, bool), ScanStoreError> {
        // Items build on demand: the first attempt moves its batch, and
        // only the rare stale retry rebuilds (reclassifying is cheap
        // against the store's snapshot). Cloning every batch doubled
        // discovery's string churn for a path that never fires.
        let build_items = || -> Result<Vec<ScanInventoryItem>, ScanStoreError> {
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
            let verdicts = self
                .store
                .classify(stop, &scope.root_id, &inputs, Some(run_id))?;
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
            Ok(items)
        };
        // Single-worker stores never go stale here; retry once with a
        // fresh revision so a racing writer costs one retry instead of a
        // failed run. A second stale is recorded and the batch is
        // skipped: losing one page with a failure row beats failing the
        // whole run. A row the store refuses costs that row only (the
        // store stores the page row by row); any other store error ends
        // the drive by the scan's failure rule.
        match self.store.add_inventory_batch(
            stop,
            run_id,
            build_items()?,
            row_revision,
            self.clock.now_secs(),
            generation,
        ) {
            Ok((revision, refused)) => Ok((revision, refused == 0)),
            Err(ScanStoreError::StaleRevision { .. }) => {
                let fresh = self.store.reread_run(stop, run_id)?.0.row_revision;
                match self.store.add_inventory_batch(
                    stop,
                    run_id,
                    build_items()?,
                    fresh,
                    self.clock.now_secs(),
                    generation,
                ) {
                    Ok((revision, refused)) => Ok((revision, refused == 0)),
                    Err(error @ ScanStoreError::StaleRevision { .. }) => {
                        tracing::error!(%error, "scan inventory batch lost its retry");
                        self.record_failure(
                            stop,
                            run_id,
                            scope,
                            scope.relative_path.clone(),
                            FailureCode::WalkError,
                            format!("The inventory page failed to persist: {error}"),
                        )?;
                        Ok((fresh, false))
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
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

/// The filesystem a folder is on, or `None` when it cannot be read. Follows
/// a symlink, so a root reached through one reads as its target.
#[cfg(unix)]
fn folder_device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|meta| meta.dev())
}

/// Mounts are found on unix only.
#[cfg(not(unix))]
fn folder_device(_path: &Path) -> Option<u64> {
    None
}

/// A folder is a mount when it is on another filesystem than the folder
/// above it. A folder whose filesystem cannot be read counts as on its
/// parent's.
fn is_mount(parent: Option<u64>, folder: Option<u64>) -> bool {
    matches!((parent, folder), (Some(parent), Some(folder)) if parent != folder)
}

/// The filesystem of `selected`, reporting each folder from below the root
/// down to it that is a mount, so a scan of one folder still finds the
/// mount above it. The root is the base: it is never compared with its own
/// parent.
fn scope_device(
    selected: &Path,
    root: &Path,
    device: &dyn Fn(&Path) -> Option<u64>,
    report: &mut dyn FnMut(&Path),
) -> Option<u64> {
    let mut current = device(root);
    let Ok(below) = selected.strip_prefix(root) else {
        return current;
    };
    let mut folder = root.to_owned();
    for part in below.components() {
        folder.push(part);
        let own = device(&folder);
        if is_mount(current, own) {
            report(&folder);
        }
        current = own.or(current);
    }
    current
}

/// Blocking walk producing inventory messages (v2 `producer`). Runs on the
/// pool: stat-only, symlink-cautious, skip-and-report. `device` reads the
/// filesystem of each folder the walk descends into, once, to report mounts.
fn produce_inventory(
    selected: &Path,
    root: &Path,
    sender: &mpsc::Sender<WalkMessage>,
    stopped: &AtomicBool,
    heartbeat: &WalkHeartbeat,
    device: &dyn Fn(&Path) -> Option<u64>,
) {
    let report_mount = |folder: &Path| {
        let key = inventory_key(folder, root);
        if !key.is_empty() && key != "." {
            send_with_stop(sender, stopped, WalkMessage::Mount(key));
        }
    };
    // Symlink targets resolve to canonical paths; compare them against the
    // canonical root so a root reached through a symlink (`/music` ->
    // `/mnt/nas/music`) still recognizes its own files.
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
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
    let base = scope_device(selected, root, device, &mut |folder| report_mount(folder));
    // Each folder carries its filesystem, for its subfolders to compare.
    let mut stack = vec![(selected.to_owned(), base)];
    while let Some((directory, directory_device)) = stack.pop() {
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
        let mut skips: Vec<(String, FailureCode)> = Vec::new();
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
                    let own = device(&path);
                    if is_mount(directory_device, own) {
                        report_mount(&path);
                    }
                    stack.push((path, own.or(directory_device)));
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
                skips.push((relative, FailureCode::WalkNameEncoding));
                continue;
            }
            // In-root file symlinks resolve onto their target's own path;
            // escape-out links are audited below (symlinks are never
            // followed into the library).
            let resolved = if file_type.is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(target) => match target.strip_prefix(&canonical_root) {
                        // Re-expressed under the configured root, so its
                        // inventory key matches a plain file's.
                        Ok(inside) => root.join(inside),
                        Err(_) => {
                            let relative = path
                                .strip_prefix(root)
                                .map(text_safe_posix)
                                .unwrap_or_else(|_| text_safe_posix(&path));
                            skips.push((relative, FailureCode::SymlinkEscapeOut));
                            continue;
                        }
                    },
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
                skips.push((relative, FailureCode::NonRegularFile));
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

    /// The mounts a walk of `selected` under `root` reports.
    fn mounts_walked(
        selected: &Path,
        root: &Path,
        device: &dyn Fn(&Path) -> Option<u64>,
    ) -> Vec<String> {
        let (sender, mut receiver) = mpsc::channel(INVENTORY_QUEUE_SIZE);
        let stopped = AtomicBool::new(false);
        produce_inventory(
            selected,
            root,
            &sender,
            &stopped,
            &WalkHeartbeat::new(),
            device,
        );
        let mut mounts = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            if let WalkMessage::Mount(folder) = message {
                mounts.push(folder);
            }
        }
        mounts
    }

    /// A folder on another filesystem than the folder above it is reported
    /// once, by its path in the root, also by a walk of a folder below it.
    /// The root is never compared with its own parent, so a root reached
    /// through a symlink reports nothing.
    #[test]
    fn a_walk_reports_the_folders_on_another_filesystem() {
        let scratch =
            droppedneedle_kernel::db::scratch::ScratchDir::new("walk-mounts").expect("scratch");
        let root = scratch.join("music");
        for file in ["local/1.flac", "nas/x/2.flac", "nas/y/z/3.flac"] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, b"x").expect("write");
        }
        let nas = root.join("nas");
        let device = |path: &Path| Some(if path.starts_with(&nas) { 2 } else { 1 });
        assert_eq!(mounts_walked(&root, &root, &device), ["nas"]);
        assert_eq!(mounts_walked(&root.join("nas/x"), &root, &device), ["nas"]);

        #[cfg(unix)]
        {
            let link = scratch.join("link");
            std::os::unix::fs::symlink(&root, &link).expect("symlink");
            assert!(mounts_walked(&link, &link, &folder_device).is_empty());
        }
    }
}
