//! Stage-13 fix D: shutdown stops in-flight scans promptly.
//!
//! Briefs (small scratch corpus, scripted tag timing):
//!
//! - `scan_tick_pre_signalled_shutdown_claims_nothing`: quiet
//!   shutdown sanity, the queued run stays queued.
//! - `scan_abandons_in_flight_index_on_shutdown`: a shutdown
//!   landing mid-index settles the run to cancelled within one
//!   checkpoint window, and a fresh run completes cleanly after.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use droppedneedle::library::scan::{
    BlockingPool, EffectivePolicy, LibraryRoot, LibraryScanCoordinator, MemoryScanStore,
    NullIdentifyQueue, RootRegistry, ScanKind, ScanRequest, ScanScope, ScanState, ScanStore,
    ScanTrigger, ScannedTags, StaticResolver, TagReadError, TagReader, WorkWakeups,
};
use tokio::sync::watch;

/// Scratch-dir sequence so parallel tests never share a root.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Tag reader that paces every read and signals shutdown on one
/// nominated read: a deterministic SIGTERM landing mid-index.
struct ShutdownOnRead {
    shutdown_tx: watch::Sender<bool>,
    signal_at: u64,
    delay: Duration,
    reads: AtomicU64,
}

impl TagReader for ShutdownOnRead {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        let nth = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if nth == self.signal_at {
            let _ = self.shutdown_tx.send(true);
        }
        std::thread::sleep(self.delay);
        Ok(ScannedTags::default())
    }
}

impl ShutdownOnRead {
    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

/// Fresh scratch root with `files` junk audio files. The walk only
/// stats and the scripted reader never parses, so junk bytes scan.
fn planted_root(tag: &str, files: usize) -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "droppedneedle-lib-scan-shutdown-{tag}-{}-{seq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    for n in 0..files {
        std::fs::write(root.join(format!("t{n:04}.flac")), b"junk-audio").expect("plant file");
    }
    root
}

fn coordinator_with(
    root: &Path,
    tags: Arc<ShutdownOnRead>,
) -> (
    LibraryScanCoordinator<MemoryScanStore, ShutdownOnRead, NullIdentifyQueue>,
    HashMap<String, PathBuf>,
) {
    let registry = RootRegistry::new(
        vec![LibraryRoot::new(
            "music",
            root.to_owned(),
            EffectivePolicy::Automatic,
        )],
        true,
        "rev-1",
    );
    let coordinator = LibraryScanCoordinator::new(
        Arc::new(MemoryScanStore::new()),
        BlockingPool::new(4),
        tags,
        Arc::new(NullIdentifyQueue::new()),
        Arc::new(StaticResolver::new(registry)),
    )
    .with_wakeups(WorkWakeups::new());
    let mut root_paths = HashMap::new();
    root_paths.insert("music".to_owned(), root.to_owned());
    (coordinator, root_paths)
}

fn scan_request(root: &Path) -> ScanRequest {
    ScanRequest {
        kind: ScanKind::Incremental,
        trigger: ScanTrigger::Manual,
        scopes: vec![ScanScope::root("music", &root.to_string_lossy(), "rev-1")],
        requested_by_user_id: None,
        policy_revision: "rev-1".to_owned(),
    }
}

#[tokio::test]
async fn scan_tick_pre_signalled_shutdown_claims_nothing() {
    let root = planted_root("quiet", 4);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let tags = Arc::new(ShutdownOnRead {
        shutdown_tx: shutdown_tx.clone(),
        signal_at: u64::MAX,
        delay: Duration::ZERO,
        reads: AtomicU64::new(0),
    });
    let (coordinator, root_paths) = coordinator_with(&root, Arc::clone(&tags));
    coordinator
        .request_run(&scan_request(&root))
        .expect("run requested");
    shutdown_tx.send(true).expect("shutdown sends");
    let outcome = coordinator
        .run_once_with_shutdown(&root_paths, &shutdown_rx)
        .await;
    assert!(
        outcome.is_none(),
        "pre-signalled shutdown claims no new work"
    );
    assert_eq!(tags.reads(), 0, "no tag read ran under shutdown");
    let current = coordinator.current();
    assert_eq!(current.len(), 1, "the queued run is still listed");
    assert_eq!(
        current[0].state,
        ScanState::Queued,
        "the queued run waits out the shutdown"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn scan_abandons_in_flight_index_on_shutdown() {
    const FILES: usize = 200;
    const SIGNAL_AT: u64 = 5;
    let root = planted_root("mid-scan", FILES);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let tags = Arc::new(ShutdownOnRead {
        shutdown_tx,
        signal_at: SIGNAL_AT,
        delay: Duration::from_millis(10),
        reads: AtomicU64::new(0),
    });
    let (coordinator, root_paths) = coordinator_with(&root, Arc::clone(&tags));
    coordinator
        .request_run(&scan_request(&root))
        .expect("run requested");
    let start = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        coordinator.run_once_with_shutdown(&root_paths, &shutdown_rx),
    )
    .await
    .expect("scan yields instead of waiting out the run")
    .expect("a run was driven");
    let elapsed = start.elapsed();
    println!(
        "shutdown abort settled after {elapsed:?}; tag reads={}",
        tags.reads()
    );
    assert_eq!(outcome.state, ScanState::Cancelled, "aborted run cancels");
    assert!(
        outcome.terminal_at.is_some(),
        "cancelled run carries a terminal stamp"
    );
    let reads = tags.reads();
    assert!(
        reads < FILES as u64,
        "aborted instead of draining ({reads}/{FILES} reads)"
    );
    // Index checkpoints every 16 files: at most one window plus the
    // in-flight read may finish past the signal.
    assert!(
        reads <= SIGNAL_AT + 16 + 1,
        "aborted within one checkpoint window, took {reads} reads"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "settled promptly, took {elapsed:?}"
    );
    // The next start re-runs cleanly: a fresh request completes, and
    // every catalog row is a whole committed row with a track id.
    let (_live, quiet) = watch::channel(false);
    coordinator
        .request_run(&scan_request(&root))
        .expect("re-run requested");
    let rerun = tokio::time::timeout(
        Duration::from_secs(30),
        coordinator.run_once_with_shutdown(&root_paths, &quiet),
    )
    .await
    .expect("re-run yields")
    .expect("re-run drove");
    assert_eq!(rerun.state, ScanState::Completed, "fresh run completes");
    let catalog = coordinator.store().catalog_entries("music");
    assert_eq!(catalog.len(), FILES, "every file indexed exactly once");
    assert!(
        catalog.iter().all(|(_, entry)| !entry.track_id.is_empty()),
        "no half-committed catalog rows"
    );
    let _ = std::fs::remove_dir_all(&root);
}
