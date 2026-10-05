//! Stage-8 library-scan briefs: scan-rate briefs over the stage-1
//! corpora plus the no-op purity pin.
//!
//! The stage-1 spikes (`stage1-spikes/v3_s1_*.md`) treat
//! `backend/tests/fixtures/library/` as the corpus source (generated
//! corpora byte-copy these files and retag). These briefs copy a subset of
//! those committed fixtures into sandbox roots under the temp dir and run
//! the real pipeline: walk, classify, tag seam, catalog commit, identify
//! offers, missing detection. Nothing here touches the network, the
//! production database, or the fixture directory itself (read-only copies
//! out; every root is a fresh sandbox).
//!
//! Scan and identify never write music files: the purity brief snapshots
//! every byte before and after full runs and pins zero writes.

use droppedneedle::library::scan;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scan::{
    BlockingPool, DirtyScopes, Disposition, EffectivePolicy, FsCoordinator, IdentifyQueue,
    LibraryRoot, LibraryScanCoordinator, MemoryScanStore, NullIdentifyQueue, NullTagReader,
    RevisionPublisher, RevisionSource, RootRegistry, RootSeamError, ScanInventoryItem, ScanKind,
    ScanRequest, ScanRun, ScanScope, ScanState, ScanStore, ScanTrigger, ScannedTags,
    ScheduleSettings, StaticResolver, StreamRootSeam, TagReadError, TagReader, Verdict,
    WatcherAction, WatcherSettings, WatcherState, WorkWakeups, counter_names, failure_codes,
    watcher_clear_pending, watcher_poll_once, watcher_request,
};
use sha2::{Digest, Sha256};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(0);

type TestCoordinator = LibraryScanCoordinator<MemoryScanStore, NullTagReader, NullIdentifyQueue>;

fn fixtures_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest.join("../backend/tests/fixtures/library");
    assert!(dir.is_dir(), "fixture corpus missing: {}", dir.display());
    dir
}

/// Fresh sandbox root. Unique per call so parallel briefs never share one.
fn sandbox_root(tag: &str) -> PathBuf {
    let id = SANDBOX_COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "dn-library-scan-{}-{}-{}",
        tag,
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("sandbox root");
    root
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

/// Copy one committed fixture into the sandbox. The fixture dir is only
/// ever read; the sandbox copy is the scan target.
fn plant_fixture(root: &Path, relative: &str, fixture: &str) -> PathBuf {
    let dest = root.join(relative);
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
    std::fs::copy(fixtures_dir().join(fixture), &dest).expect("copy fixture");
    dest
}

fn count(run: &ScanRun, name: &str) -> i64 {
    run.counters.get(name).copied().unwrap_or(0)
}

/// Byte-level snapshot of a root: relative path -> (size, mtime_ns, sha256).
/// mtime is pinned because a writer that "restores" content still moves it.
fn snapshot_tree(root: &Path) -> BTreeMap<String, (u64, i64, String)> {
    let mut snapshot = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("read sandbox dir");
        for entry in entries {
            let entry = entry.expect("sandbox entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("sandbox file type");
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if file_type.is_symlink() {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read sandbox file");
            let meta = std::fs::metadata(&path).expect("stat sandbox file");
            let mtime_ns = scan::mtime_ns_from_metadata(&meta);
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hex: String = hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let relative = path
                .strip_prefix(root)
                .expect("in-root")
                .to_string_lossy()
                .replace('\\', "/");
            snapshot.insert(relative, (bytes.len() as u64, mtime_ns, hex));
        }
    }
    snapshot
}

struct Rig {
    coordinator: TestCoordinator,
    tags: Arc<NullTagReader>,
    identify: Arc<NullIdentifyQueue>,
    root_paths: HashMap<String, PathBuf>,
    registry: RootRegistry,
}

fn rig(root: &Path) -> Rig {
    rig_with_revision(root, "rev-1")
}

fn rig_with_revision(root: &Path, revision: &str) -> Rig {
    let registry = RootRegistry::new(
        vec![LibraryRoot::new(
            "music",
            root.to_owned(),
            EffectivePolicy::Automatic,
        )],
        true,
        revision,
    );
    let tags = Arc::new(NullTagReader::new());
    let identify = Arc::new(NullIdentifyQueue::new());
    let coordinator = LibraryScanCoordinator::new(
        Arc::new(MemoryScanStore::new()),
        BlockingPool::new(4),
        Arc::clone(&tags),
        Arc::clone(&identify),
        Arc::new(StaticResolver::new(registry.clone())),
    )
    .with_wakeups(WorkWakeups::new());
    let mut root_paths = HashMap::new();
    root_paths.insert("music".to_owned(), root.to_owned());
    Rig {
        coordinator,
        tags,
        identify,
        root_paths,
        registry,
    }
}

fn whole_root_request(
    kind: ScanKind,
    trigger: ScanTrigger,
    root: &Path,
    revision: &str,
) -> ScanRequest {
    ScanRequest {
        kind,
        trigger,
        scopes: vec![ScanScope::root(
            "music",
            &root.display().to_string(),
            revision,
        )],
        requested_by_user_id: None,
        policy_revision: revision.to_owned(),
    }
}

/// Request one run and drive it to a terminal state. run_once is a full
/// worker pass (discover -> index -> reconcile), so one call finishes.
async fn drive_to_end(rig: &Rig, request: &ScanRequest) -> (ScanRun, Duration) {
    let result = rig
        .coordinator
        .request_run(request)
        .expect("request accepted");
    assert!(
        result.disposition == Disposition::Started || result.disposition == Disposition::Queued,
        "unexpected disposition: {:?}",
        result.disposition
    );
    let started = Instant::now();
    let run = rig
        .coordinator
        .run_once(&rig.root_paths)
        .await
        .expect("worker drove the run");
    assert!(
        run.state.is_terminal(),
        "run stopped mid-pipeline: {:?}",
        run.state
    );
    (run, started.elapsed())
}

fn plant_album_tree(root: &Path) {
    plant_fixture(root, "album_a/track01.flac", "flac_full_01.flac");
    plant_fixture(root, "album_a/track02.flac", "flac_full_02.flac");
    plant_fixture(root, "album_b/track01.mp3", "mp3_full_01.mp3");
    plant_fixture(root, "album_b/track01.m4a", "m4a_full_01.m4a");
    plant_fixture(root, "singles/odd.flac", "flac_no_tags.flac");
    plant_fixture(root, "singles/cjk.flac", "flac_cjk_01.flac");
}

/// Scan-rate line printed per brief (visible with --nocapture).
fn report_rate(brief: &str, files: i64, elapsed: Duration) {
    let secs = elapsed.as_secs_f64().max(1e-6);
    eprintln!(
        "scan-rate brief={brief} files={files} secs={secs:.3} files_per_sec={:.1}",
        files as f64 / secs
    );
}

#[tokio::test]
async fn initial_scan_brief() {
    let root = sandbox_root("initial");
    plant_album_tree(&root);
    let rig = rig(&root);
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (run, elapsed) = drive_to_end(&rig, &request).await;
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::DISCOVERED), 6);
    assert_eq!(count(&run, counter_names::TOTAL), 6);
    assert_eq!(count(&run, counter_names::NEW), 6);
    assert_eq!(count(&run, counter_names::CHANGED), 0);
    assert_eq!(count(&run, counter_names::UNCHANGED), 0);
    assert_eq!(count(&run, counter_names::INDEXED), 6);
    assert_eq!(count(&run, counter_names::INSPECTED), 6);
    assert_eq!(count(&run, counter_names::ERRORED), 0);
    assert_eq!(count(&run, counter_names::MISSING), 0);
    assert_eq!(count(&run, counter_names::IDENTIFICATION_ENQUEUED), 6);
    assert_eq!(rig.tags.reads(), 6, "every new file reads tags once");
    assert_eq!(rig.identify.enqueued_tracks(), 6);
    assert_eq!(
        rig.identify.albums().len(),
        3,
        "one offer per parent directory"
    );
    assert!(rig.coordinator.store().failures(&run.id).is_empty());
    report_rate("initial", 6, elapsed);
    assert!(elapsed < Duration::from_secs(60));
    cleanup(&root);
}

#[tokio::test]
async fn incremental_scan_brief() {
    let root = sandbox_root("incremental");
    plant_album_tree(&root);
    let rig = rig(&root);
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (first, _) = drive_to_end(&rig, &request).await;
    assert_eq!(first.state, ScanState::Completed);

    // One addition, one same-name rewrite, one deletion.
    plant_fixture(&root, "album_c/new.mp3", "management_full.mp3");
    std::fs::write(
        root.join("album_a/track01.flac"),
        b"rewritten-audio-bytes-longer",
    )
    .expect("rewrite keeps the name, changes size+mtime");
    std::fs::remove_file(root.join("singles/odd.flac")).expect("delete");

    let (run, elapsed) = drive_to_end(&rig, &request).await;
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::DISCOVERED), 6);
    assert_eq!(count(&run, counter_names::NEW), 1);
    assert_eq!(count(&run, counter_names::CHANGED), 1);
    assert_eq!(count(&run, counter_names::UNCHANGED), 4);
    assert_eq!(count(&run, counter_names::INDEXED), 2);
    assert_eq!(count(&run, counter_names::MISSING), 1);
    assert_eq!(count(&run, counter_names::ERRORED), 0);
    assert_eq!(rig.tags.reads(), 8, "only new+changed re-read tags");
    report_rate("incremental", 6, elapsed);
    cleanup(&root);
}

#[tokio::test]
async fn noop_scan_brief() {
    let root = sandbox_root("noop");
    plant_album_tree(&root);
    let rig = rig(&root);
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (first, _) = drive_to_end(&rig, &request).await;
    assert_eq!(first.state, ScanState::Completed);

    // Zero filesystem changes: the walk still discovers, nothing re-indexes.
    let (run, elapsed) = drive_to_end(&rig, &request).await;
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::DISCOVERED), 6);
    assert_eq!(count(&run, counter_names::NEW), 0);
    assert_eq!(count(&run, counter_names::CHANGED), 0);
    assert_eq!(count(&run, counter_names::UNCHANGED), 6);
    assert_eq!(count(&run, counter_names::INDEXED), 0);
    assert_eq!(count(&run, counter_names::MISSING), 0);
    assert_eq!(count(&run, counter_names::IDENTIFICATION_ENQUEUED), 0);
    assert_eq!(rig.tags.reads(), 6, "no-op run reads zero tags");
    report_rate("noop", 6, elapsed);
    cleanup(&root);
}

#[tokio::test]
async fn reindex_scan_brief() {
    let root = sandbox_root("reindex");
    plant_album_tree(&root);
    let rig = rig(&root);
    let initial = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (first, _) = drive_to_end(&rig, &initial).await;
    assert_eq!(first.state, ScanState::Completed);

    // rescan_files re-reads every in-policy file even though stat is clean.
    let reindex = whole_root_request(ScanKind::RescanFiles, ScanTrigger::Manual, &root, "rev-1");
    let (run, elapsed) = drive_to_end(&rig, &reindex).await;
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::NEW), 0);
    assert_eq!(count(&run, counter_names::CHANGED), 0);
    assert_eq!(
        count(&run, counter_names::UNCHANGED),
        6,
        "verdicts stay honest"
    );
    assert_eq!(
        count(&run, counter_names::INDEXED),
        6,
        "every file re-reads"
    );
    assert_eq!(rig.tags.reads(), 12, "initial 6 plus reindex 6");
    report_rate("reindex", 6, elapsed);
    cleanup(&root);
}

#[tokio::test]
async fn noop_purity_brief() {
    // Zero file writes across initial, incremental, no-op, and re-index
    // runs: same files, same sizes, same mtimes, same bytes.
    let root = sandbox_root("purity");
    plant_fixture(&root, "a.flac", "flac_full_01.flac");
    plant_fixture(&root, "sub/b.mp3", "mp3_full_01.mp3");
    plant_fixture(&root, "sub/c.m4a", "m4a_full_01.m4a");
    let before = snapshot_tree(&root);
    assert_eq!(before.len(), 3);

    let rig = rig(&root);
    let initial = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (first, _) = drive_to_end(&rig, &initial).await;
    assert_eq!(first.state, ScanState::Completed);
    assert_eq!(snapshot_tree(&root), before, "initial scan wrote files");

    let noop = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (second, _) = drive_to_end(&rig, &noop).await;
    assert_eq!(second.state, ScanState::Completed);
    assert_eq!(snapshot_tree(&root), before, "no-op scan wrote files");

    let reindex = whole_root_request(ScanKind::RescanFiles, ScanTrigger::Manual, &root, "rev-1");
    let (third, _) = drive_to_end(&rig, &reindex).await;
    assert_eq!(third.state, ScanState::Completed);
    assert_eq!(snapshot_tree(&root), before, "re-index scan wrote files");

    // Incremental with real changes: add one, rewrite one, delete one.
    // The run classifies the cycle honestly and writes nothing itself:
    // the post-change snapshot is stable across the run.
    plant_fixture(&root, "added/d.m4a", "m4a_full_01.m4a");
    std::fs::write(root.join("a.flac"), b"rewritten-audio-bytes").expect("rewrite");
    std::fs::remove_file(root.join("sub/c.m4a")).expect("delete");
    let changed = snapshot_tree(&root);
    assert_ne!(changed, before, "the cycle actually changed the tree");
    let with_changes =
        whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (fourth, _) = drive_to_end(&rig, &with_changes).await;
    assert_eq!(fourth.state, ScanState::Completed);
    assert_eq!(count(&fourth, counter_names::NEW), 1);
    assert_eq!(count(&fourth, counter_names::CHANGED), 1);
    assert_eq!(count(&fourth, counter_names::MISSING), 1);
    assert_eq!(count(&fourth, counter_names::ERRORED), 0);
    assert_eq!(snapshot_tree(&root), changed, "incremental run wrote files");

    // The watcher only stats: polling the live root changes nothing.
    let pool = BlockingPool::new(2);
    let settings = WatcherSettings::default();
    let mut state = WatcherState::new();
    let mut roots = HashMap::new();
    roots.insert("music".to_owned(), root.clone());
    let action = watcher_poll_once(&mut state, &settings, &rig.registry, &roots, &pool, 0.0).await;
    assert!(
        matches!(action, WatcherAction::Idle { .. }),
        "seed poll stays idle"
    );
    assert_eq!(snapshot_tree(&root), changed, "watcher poll wrote files");
    cleanup(&root);
}

#[tokio::test]
async fn deferred_tag_read_reoffers_next_run() {
    // F-12 end to end: exhaustion defers with a persisted marker, and the
    // file re-offers as changed on the next run despite clean stat (stat
    // comparison alone would skip it forever).
    use scan::{ArmableDeferTagReader, LibraryScanCoordinator, MemoryScanStore, NullIdentifyQueue};

    let root = sandbox_root("deferred");
    plant_fixture(&root, "a.flac", "flac_full_01.flac");
    plant_fixture(&root, "b.flac", "flac_full_02.flac");
    let registry = RootRegistry::new(
        vec![LibraryRoot::new(
            "music",
            root.clone(),
            EffectivePolicy::Automatic,
        )],
        true,
        "rev-1",
    );
    let tags = Arc::new(ArmableDeferTagReader::new());
    let coordinator: LibraryScanCoordinator<
        MemoryScanStore,
        ArmableDeferTagReader,
        NullIdentifyQueue,
    > = LibraryScanCoordinator::new(
        Arc::new(MemoryScanStore::new()),
        BlockingPool::new(2),
        Arc::clone(&tags),
        Arc::new(NullIdentifyQueue::new()),
        Arc::new(StaticResolver::new(registry)),
    );
    let mut root_paths = HashMap::new();
    root_paths.insert("music".to_owned(), root.clone());
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");

    // Run 1 indexes both files cleanly.
    coordinator.request_run(&request).expect("request 1");
    let first = coordinator.run_once(&root_paths).await.expect("run 1");
    assert_eq!(first.state, ScanState::Completed);
    assert_eq!(count(&first, counter_names::INDEXED), 2);

    // Run 2 is a re-index (forced re-read on clean stat) with a.flac
    // armed: the read defers and the marker persists.
    tags.arm("a.flac");
    let reindex = whole_root_request(ScanKind::RescanFiles, ScanTrigger::Manual, &root, "rev-1");
    coordinator.request_run(&reindex).expect("request 2");
    let second = coordinator.run_once(&root_paths).await.expect("run 2");
    assert_eq!(second.state, ScanState::Completed);
    assert_eq!(count(&second, counter_names::ERRORED), 1);
    let failures = coordinator.store().failures(&second.id);
    assert!(
        failures
            .iter()
            .any(|failure| failure.failure_code == failure_codes::TAG_READ_DEFERRED),
        "deferred read leaves a persisted marker row"
    );

    // Run 3 changes nothing on disk, but the marked file re-offers as
    // changed and a clean read clears the marker.
    coordinator.request_run(&request).expect("request 3");
    let third = coordinator.run_once(&root_paths).await.expect("run 3");
    assert_eq!(third.state, ScanState::Completed);
    assert_eq!(count(&third, counter_names::CHANGED), 1);
    assert_eq!(count(&third, counter_names::UNCHANGED), 1);
    assert_eq!(
        count(&third, counter_names::INDEXED),
        1,
        "only the re-offer re-indexes"
    );

    // Run 4 is a true no-op: the marker is gone.
    coordinator.request_run(&request).expect("request 4");
    let fourth = coordinator.run_once(&root_paths).await.expect("run 4");
    assert_eq!(count(&fourth, counter_names::UNCHANGED), 2);
    assert_eq!(count(&fourth, counter_names::INDEXED), 0);
    cleanup(&root);
}

#[tokio::test]
async fn failure_accounting_brief() {
    let root = sandbox_root("failures");
    plant_fixture(&root, "good.flac", "flac_full_01.flac");
    // Exact-name recycle bin: never library content, silently skipped.
    plant_fixture(&root, ".recycle/old.flac", "flac_full_02.flac");
    // Management sidecars: silently skipped.
    plant_fixture(
        &root,
        ".droppedneedle-management-9/sidecar.flac",
        "flac_full_02.flac",
    );
    // Non-audio: ignored.
    std::fs::write(root.join("notes.txt"), b"not music").expect("write notes");
    let rig = rig(&root);

    // Mixed scopes: one unconfigured root reports unavailable while the
    // valid scope keeps walking (GH-296 skip-and-report).
    let mut mixed = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    mixed.scopes.push(ScanScope {
        root_id: "ghost".to_owned(),
        scope_id: Some("ghost".to_owned()),
        relative_path: ".".to_owned(),
        root_path: None,
        effective_policy: EffectivePolicy::Automatic,
        policy_revision: "rev-1".to_owned(),
        estimated_count: None,
    });
    // Coordinator validation refuses unknown roots up front...
    assert!(rig.coordinator.request_run(&mixed).is_err());
    // ...so the skip-and-report path is exercised at the store seam, the
    // way a frozen policy-apply scope with a removed root arrives.
    let result = rig
        .coordinator
        .store()
        .request_run(&mixed, "run-mixed", 1.0);
    assert_eq!(result.disposition, Disposition::Started);
    let run = rig
        .coordinator
        .run_once(&rig.root_paths)
        .await
        .expect("mixed run");
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(
        count(&run, counter_names::INDEXED),
        1,
        "only good.flac indexes"
    );
    let failures = rig.coordinator.store().failures(&run.id);
    assert!(
        failures.iter().any(|failure| failure.root_id == "ghost"
            && failure.failure_code == failure_codes::ROOT_UNAVAILABLE),
        "ghost scope reports unavailable, run still completes"
    );

    // Every scope unreachable: the run fails honestly, never green.
    let mut ghost_only =
        whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    ghost_only.scopes = vec![ScanScope {
        root_id: "ghost".to_owned(),
        scope_id: Some("ghost".to_owned()),
        relative_path: ".".to_owned(),
        root_path: None,
        effective_policy: EffectivePolicy::Automatic,
        policy_revision: "rev-1".to_owned(),
        estimated_count: None,
    }];
    let result = rig
        .coordinator
        .store()
        .request_run(&ghost_only, "run-ghost", 2.0);
    assert_eq!(result.disposition, Disposition::Started);
    let run = rig
        .coordinator
        .run_once(&rig.root_paths)
        .await
        .expect("ghost run");
    assert_eq!(run.state, ScanState::Failed);
    assert_eq!(
        run.terminal_code.as_deref(),
        Some(failure_codes::ROOT_UNAVAILABLE)
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_and_twin_brief() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    let root = sandbox_root("links");
    plant_fixture(&root, "good.flac", "flac_full_01.flac");
    // Escape-out link: audited, never followed.
    symlink("/etc/hostname", root.join("escape.flac")).expect("symlink");
    // In-root alias: resolves onto its target, dedups to one inventory row.
    symlink(root.join("good.flac"), root.join("alias.flac")).expect("symlink");
    // NFC/NFD twins: distinct files on disk, one inventory key.
    let nfc = "caf\u{e9}.flac";
    let nfd: String = "cafe\u{301}.flac".to_owned();
    assert_ne!(nfc, nfd);
    std::fs::copy(fixtures_dir().join("flac_full_02.flac"), root.join(nfc)).expect("nfc twin");
    std::fs::copy(fixtures_dir().join("flac_full_02.flac"), root.join(&nfd)).expect("nfd twin");
    let twins_coexist = root.join(nfc).exists()
        && root.join(&nfd).exists()
        && std::fs::read_dir(&root)
            .expect("readdir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().as_bytes().ends_with(b".flac"))
            .count()
            >= 4;
    let rig = rig(&root);
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (run, _) = drive_to_end(&rig, &request).await;
    assert_eq!(run.state, ScanState::Completed);
    let failures = rig.coordinator.store().failures(&run.id);
    assert!(
        failures.iter().any(
            |failure| failure.failure_code == failure_codes::SYMLINK_ESCAPE_OUT
                && failure.relative_path == "escape.flac"
        ),
        "escape-out link is audited, got: {:?}",
        failures
            .iter()
            .map(|failure| &failure.failure_code)
            .collect::<Vec<_>>()
    );
    if twins_coexist {
        assert!(
            failures
                .iter()
                .any(|failure| failure.failure_code == failure_codes::NFC_TWIN_COLLISION),
            "NFC twins first-win with a loser row"
        );
        // good + alias-target-shared... alias resolves onto good (one row),
        // plus exactly one twin: 2 indexed rows.
        assert_eq!(count(&run, counter_names::INDEXED), 2);
    }
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn nonregular_file_brief() {
    // A FIFO wearing an audio suffix skips without touching tag reads.
    let root = sandbox_root("fifo");
    plant_fixture(&root, "good.flac", "flac_full_01.flac");
    let fifo = root.join("pipe.flac");
    let path = std::ffi::CString::new(fifo.to_string_lossy().into_owned()).expect("cstring");
    // SAFETY: mkfifo on a sandbox path, mode 0644, return checked.
    let created = unsafe { libc::mkfifo(path.as_ptr(), 0o644) } == 0;
    assert!(
        created,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );
    let rig = rig(&root);
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (run, _) = drive_to_end(&rig, &request).await;
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::INDEXED), 1);
    assert_eq!(rig.tags.reads(), 1, "the FIFO never reaches tag reads");
    let failures = rig.coordinator.store().failures(&run.id);
    assert!(
        failures.iter().any(
            |failure| failure.failure_code == failure_codes::NON_REGULAR_FILE
                && failure.relative_path == "pipe.flac"
        ),
        "FIFO is audited as non-regular"
    );
    cleanup(&root);
}

#[test]
fn root_seam_brief() {
    let root = sandbox_root("seam");
    plant_fixture(&root, "album/track.flac", "flac_full_01.flac");
    let registry = RootRegistry::new(
        vec![LibraryRoot::new(
            "music",
            root.clone(),
            EffectivePolicy::Automatic,
        )],
        true,
        "rev-1",
    );
    let seam = StreamRootSeam::new(registry);
    // Happy path resolves under the root.
    let resolved = seam
        .resolve_key("music", "album/track.flac")
        .expect("resolve");
    assert!(resolved.starts_with(&root));
    assert!(resolved.is_file());
    // Traversal, absolute, and empty keys refuse with the fixed error.
    assert_eq!(
        seam.resolve_key("music", "../etc/passwd"),
        Err(RootSeamError::Forbidden)
    );
    assert_eq!(
        seam.resolve_key("music", "/etc/passwd"),
        Err(RootSeamError::Forbidden)
    );
    assert_eq!(seam.resolve_key("music", ""), Err(RootSeamError::Forbidden));
    // Unknown roots name themselves (integrator maps to 404).
    assert!(matches!(
        seam.resolve_key("ghost", "a.flac"),
        Err(RootSeamError::UnknownRoot { .. })
    ));
    // Single-path callers (stage-6 shape) read the primary root.
    assert_eq!(seam.primary_music_root(), Some(root.clone()));
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn root_seam_symlink_escape_brief() {
    use std::os::unix::fs::symlink;

    let root = sandbox_root("seam-link");
    symlink("/etc/hostname", root.join("outside.flac")).expect("symlink");
    let registry = RootRegistry::new(
        vec![LibraryRoot::new(
            "music",
            root.clone(),
            EffectivePolicy::Automatic,
        )],
        true,
        "rev-1",
    );
    let seam = StreamRootSeam::new(registry);
    // The key screens clean, but canonicalization catches the escape.
    assert_eq!(
        seam.resolve_key("music", "outside.flac"),
        Err(RootSeamError::Forbidden)
    );
    cleanup(&root);
}

#[tokio::test]
async fn watcher_batching_brief() {
    let root = sandbox_root("watcher");
    plant_fixture(&root, "a.flac", "flac_full_01.flac");
    let rig = rig(&root);
    let pool = BlockingPool::new(2);
    let settings = WatcherSettings {
        enabled: true,
        poll_interval_seconds: 5.0,
        batch_window_seconds: 60.0,
    };
    let mut state = WatcherState::new();
    // Seed poll: idle.
    let action = watcher_poll_once(
        &mut state,
        &settings,
        &rig.registry,
        &rig.root_paths,
        &pool,
        0.0,
    )
    .await;
    assert!(matches!(action, WatcherAction::Idle { .. }));
    // Mutation starts batching; the window holds the request.
    plant_fixture(&root, "b.flac", "flac_full_02.flac");
    let action = watcher_poll_once(
        &mut state,
        &settings,
        &rig.registry,
        &rig.root_paths,
        &pool,
        10.0,
    )
    .await;
    assert!(
        matches!(action, WatcherAction::Batching { .. }),
        "rapid bursts collapse into one request"
    );
    // Past the window: due, and the request is incremental/automatic.
    let action = watcher_poll_once(
        &mut state,
        &settings,
        &rig.registry,
        &rig.root_paths,
        &pool,
        71.0,
    )
    .await;
    assert_eq!(action, WatcherAction::Due);
    let request = watcher_request(&rig.registry, &[]).expect("watcher request");
    assert_eq!(request.kind, ScanKind::Incremental);
    assert_eq!(request.trigger, ScanTrigger::Automatic);
    let result = rig
        .coordinator
        .request_run(&request)
        .expect("watcher scan requested");
    assert_eq!(result.disposition, Disposition::Started);
    watcher_clear_pending(&mut state);
    assert!(!state.is_pending());
    cleanup(&root);
}

#[test]
fn scheduler_tick_brief() {
    let root = sandbox_root("scheduler");
    let rig = rig(&root);
    // Manual frequency never requests.
    let manual = ScheduleSettings::manual();
    let called = std::sync::atomic::AtomicBool::new(false);
    let fired = scan::scheduler::tick(
        |_| {
            called.store(true, Ordering::SeqCst);
            Ok(Disposition::Started)
        },
        &rig.registry,
        &[],
        &manual,
        None,
        1_000.0,
    );
    assert!(!fired);
    assert!(!called.load(Ordering::SeqCst));
    // Due interval requests through the coordinator.
    let hourly = ScheduleSettings::new("1hr", "03:00", "UTC");
    let fired = scan::scheduler::tick(
        |request| {
            rig.coordinator
                .request_run(&request)
                .map(|result| result.disposition)
                .map_err(|error| error.to_string())
        },
        &rig.registry,
        &[],
        &hourly,
        None,
        1_000.0,
    );
    assert!(fired, "no terminal anchor means due now");
    assert_eq!(rig.coordinator.current().len(), 1);
    // A conflicted follow-up answers false (S-05): the queued run has
    // kind incremental, so a rescan request... is a different kind only
    // when requested; here a second identical request coalesces (true).
    let fired = scan::scheduler::tick(
        |request| {
            rig.coordinator
                .request_run(&request)
                .map(|result| result.disposition)
                .map_err(|error| error.to_string())
        },
        &rig.registry,
        &[],
        &hourly,
        None,
        1_000.0,
    );
    assert!(fired, "coalesced still means work will run");
    cleanup(&root);
}

#[tokio::test]
async fn supervisor_and_watcher_loops_brief() {
    use scan::supervisor::{SupervisorInputs, supervise_target_scans};
    use scan::watcher::{AsyncSleep, WatcherInputs, watch_library_filesystem};
    use std::sync::atomic::AtomicBool;

    // Supervisor loop with shutdown pre-set still runs startup recovery
    // (Hook A) before exiting.
    let root = sandbox_root("loops");
    plant_fixture(&root, "a.flac", "flac_full_01.flac");
    let rig = rig(&root);
    let inputs = SupervisorInputs {
        root_paths: Arc::new({
            let root = root.clone();
            move || {
                let mut roots = HashMap::new();
                roots.insert("music".to_owned(), root.clone());
                roots
            }
        }),
        schedule: Arc::new(|| ScheduleSettings::new("1hr", "03:00", "UTC")),
        inclusion_rules: Arc::new(Vec::new),
        dirty: DirtyScopes::new(),
        wakeups: WorkWakeups::new(),
        now_unix: Arc::new(|| 1_000.0),
    };
    let shutdown = AtomicBool::new(true);
    supervise_target_scans(&rig.coordinator, &inputs, &shutdown).await;
    let current = rig.coordinator.current();
    assert_eq!(current.len(), 1, "Hook A requests the resume scan");
    assert_eq!(current[0].trigger, ScanTrigger::StartupResume);

    // Watcher loop: the roots getter plants a file on its second call, so
    // iteration 2 sees a mutation and a zero window fires immediately.
    struct TripSecondSleep<'a> {
        calls: usize,
        shutdown: &'a AtomicBool,
    }
    impl AsyncSleep for TripSecondSleep<'_> {
        async fn sleep(&mut self, _duration: Duration) {
            self.calls += 1;
            if self.calls >= 2 {
                self.shutdown.store(true, Ordering::SeqCst);
            }
        }
    }
    let calls = Arc::new(AtomicU64::new(0));
    let watch_inputs = WatcherInputs {
        root_paths: Arc::new({
            let root = root.clone();
            let calls = Arc::clone(&calls);
            let fixtures = fixtures_dir();
            move || {
                if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                    std::fs::copy(fixtures.join("flac_full_02.flac"), root.join("b.flac"))
                        .expect("plant mid-loop");
                }
                let mut roots = HashMap::new();
                roots.insert("music".to_owned(), root.clone());
                roots
            }
        }),
        settings: Arc::new(|| WatcherSettings {
            enabled: true,
            poll_interval_seconds: 5.0,
            batch_window_seconds: 0.0,
        }),
        registry: Arc::new({
            let registry = rig.registry.clone();
            move || registry.clone()
        }),
        inclusion_rules: Arc::new(Vec::new),
        clock: Arc::new({
            let tick = Arc::new(AtomicU64::new(0));
            move || {
                // Each iteration reads the clock once; advance 10s per read.
                (tick.fetch_add(1, Ordering::SeqCst) * 10) as f64
            }
        }),
        wakeups: WorkWakeups::new(),
    };
    let requested = Arc::new(std::sync::Mutex::new(Vec::new()));
    let shutdown = AtomicBool::new(false);
    let sleep = TripSecondSleep {
        calls: 0,
        shutdown: &shutdown,
    };
    watch_library_filesystem(
        &watch_inputs,
        &BlockingPool::new(2),
        |request: ScanRequest| {
            requested
                .lock()
                .expect("lock")
                .push((request.kind, request.trigger));
            rig.coordinator
                .request_run(&request)
                .expect("watcher request accepted")
        },
        &shutdown,
        sleep,
    )
    .await;
    let requested = requested.lock().expect("lock");
    assert_eq!(requested.len(), 1, "one batched scan for the burst");
    assert_eq!(
        requested[0],
        (ScanKind::Incremental, ScanTrigger::Automatic)
    );
    cleanup(&root);
}

#[test]
fn dirty_scopes_brief() {
    let dirty = DirtyScopes::new();
    assert!(dirty.is_empty());
    dirty.mark("r1");
    dirty.mark_many(&["rule-9".to_owned()]);
    let mut listed = dirty.list();
    listed.sort();
    assert_eq!(listed, vec!["r1".to_owned(), "rule-9".to_owned()]);
    dirty.clear(&["r1".to_owned()]);
    assert_eq!(dirty.list(), vec!["rule-9".to_owned()]);
}

// --- Stage-8 fixup briefs: WMA skip, snapshot control, lease
// exclusion, poller shutdown, twin paths, fatal-tag wording. ---

#[tokio::test]
async fn wma_skipped_like_non_audio() {
    assert!(!scan::is_audio_file(Path::new("song.wma")));
    assert!(!scan::is_audio_file(Path::new("song.WMA")));
    assert!(scan::is_audio_file(Path::new("song.flac")));

    let root = sandbox_root("wma-skip");
    plant_fixture(&root, "song.wma", "management_full.wma");
    std::fs::write(root.join("notes.txt"), b"not audio").expect("txt");
    plant_fixture(&root, "good.flac", "flac_full_01.flac");
    let rig = rig(&root);
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let (run, _) = drive_to_end(&rig, &request).await;
    assert_eq!(run.state, ScanState::Completed);
    // The WMA file is never discovered, so it can never land a
    // fatal TAG_READ_FAILED row the way it did before the cut.
    assert_eq!(count(&run, counter_names::DISCOVERED), 1);
    assert_eq!(count(&run, counter_names::INDEXED), 1);
    assert_eq!(count(&run, counter_names::ERRORED), 0);
    assert_eq!(rig.tags.reads(), 1, "only the FLAC reads tags");
    assert!(
        rig.coordinator.store().failures(&run.id).is_empty(),
        "skipped files leave no failure rows"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn snapshot_tree_detects_byte_and_mtime_changes() {
    // Negative control for every purity pin above: the snapshot must
    // move when the tree does, on both the content and mtime signals.
    let root = sandbox_root("snapshot-control");
    plant_fixture(&root, "a.flac", "flac_full_01.flac");
    let before = snapshot_tree(&root);
    assert_eq!(before.len(), 1);

    let path = root.join("a.flac");
    let mut bytes = std::fs::read(&path).expect("read");
    bytes[100] ^= 0xFF;
    std::fs::write(&path, &bytes).expect("rewrite");
    let now_plus_hour = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as libc::time_t
        + 3600;
    let times = [
        libc::timespec {
            tv_sec: now_plus_hour,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: now_plus_hour,
            tv_nsec: 0,
        },
    ];
    let cpath = std::ffi::CString::new(path.to_string_lossy().into_owned()).expect("cstring");
    // SAFETY: utimensat on a sandbox path with a valid times pair.
    let touched = unsafe { libc::utimensat(libc::AT_FDCWD, cpath.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(
        touched,
        0,
        "utimensat failed: {}",
        std::io::Error::last_os_error()
    );

    let after = snapshot_tree(&root);
    assert_ne!(after, before, "snapshot missed the change");
    assert_eq!(after["a.flac"].0, before["a.flac"].0, "size unchanged");
    assert_ne!(after["a.flac"].1, before["a.flac"].1, "mtime signal pinned");
    assert_ne!(
        after["a.flac"].2, before["a.flac"].2,
        "content signal pinned"
    );
    cleanup(&root);
}

#[tokio::test]
async fn fs_write_lease_excludes_readers() {
    // The exclusion publish relies on: a held read lease refuses
    // writers, writers serialize, and every release bumps the root
    // revision so in-flight walks detect supersede.
    let fs = FsCoordinator::new();
    assert_eq!(fs.revision("music"), 0);
    let read = fs.read("music").await;
    assert!(fs.try_write("music").is_none(), "read lease blocks writers");
    drop(read);
    {
        let _write = fs.blocking_write("music");
        assert!(fs.try_write("music").is_none(), "one writer at a time");
    }
    assert_eq!(fs.revision("music"), 1, "release bumps the revision");
    let read = fs.read("music").await;
    assert!(fs.try_write("music").is_none());
    drop(read);
    let _write = fs.blocking_write("music");
}

#[tokio::test]
async fn revision_poller_loop_exits_on_shutdown() {
    use scan::poller::poll_library_revisions_periodically;
    use std::sync::atomic::AtomicBool;

    struct Empty;
    impl RevisionSource for Empty {
        fn stream_revisions(&self) -> Result<HashMap<String, u64>, String> {
            Ok(HashMap::new())
        }
    }
    struct Silent;
    impl RevisionPublisher for Silent {
        fn publish(
            &self,
            _channel: &str,
            _event: &str,
            _event_id: &str,
            _revisions: &HashMap<String, u64>,
        ) {
            panic!("a shutdown loop publishes nothing");
        }
    }
    let shutdown = AtomicBool::new(true);
    tokio::time::timeout(
        Duration::from_secs(5),
        poll_library_revisions_periodically(
            &|| Empty,
            &|| Silent,
            Duration::from_secs(60),
            &shutdown,
        ),
    )
    .await
    .expect("pre-set shutdown returns at once");
}

#[test]
fn inventory_batch_twin_rows_store_relative_paths() {
    // Direct batch writes dedup like the walk and must serve the same
    // relative path get_run hands to authed callers, never the
    // absolute server path.
    let store = MemoryScanStore::new();
    let request = ScanRequest {
        kind: ScanKind::Incremental,
        trigger: ScanTrigger::Manual,
        scopes: Vec::new(),
        requested_by_user_id: None,
        policy_revision: "rev-1".to_owned(),
    };
    store.request_run(&request, "run-1", 0.0);
    let item = |absolute: &str| ScanInventoryItem {
        root_id: "music".to_owned(),
        relative_path: "sub/twin.flac".to_owned(),
        absolute_path: absolute.to_owned(),
        file_size_bytes: 10,
        file_mtime_ns: 5,
        stat_revision: "10:5".to_owned(),
        effective_policy: EffectivePolicy::Automatic,
        comparison_result: Verdict::New,
        policy_revision: "rev-1".to_owned(),
        local_track_id: None,
        scope_relative_path: "sub/twin.flac".to_owned(),
    };
    store
        .add_inventory_batch(
            "run-1",
            vec![
                item("/srv/music/sub/twin.flac"),
                item("/srv/music/SUB/twin.flac"),
            ],
            1,
            1.0,
            0,
        )
        .unwrap();
    let failures = store.failures("run-1");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].failure_code, failure_codes::NFC_TWIN_COLLISION);
    assert_eq!(failures[0].relative_path, "sub/twin.flac");
}

/// Tag reader that fails every file fatally, for the indexer wording.
struct FatalTagReader;

impl TagReader for FatalTagReader {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        Err(TagReadError::Fatal)
    }
}

#[tokio::test]
async fn fatal_tag_read_is_human_and_counted() {
    let root = sandbox_root("fatal-tags");
    plant_fixture(&root, "a.flac", "flac_full_01.flac");
    let registry = RootRegistry::new(
        vec![LibraryRoot::new(
            "music",
            root.clone(),
            EffectivePolicy::Automatic,
        )],
        true,
        "rev-1",
    );
    let coordinator = LibraryScanCoordinator::new(
        Arc::new(MemoryScanStore::new()),
        BlockingPool::new(4),
        Arc::new(FatalTagReader),
        Arc::new(NullIdentifyQueue::new()),
        Arc::new(StaticResolver::new(registry)),
    )
    .with_wakeups(WorkWakeups::new());
    let request = whole_root_request(ScanKind::Incremental, ScanTrigger::Manual, &root, "rev-1");
    let result = coordinator.request_run(&request).expect("accepted");
    assert!(
        result.disposition == Disposition::Started || result.disposition == Disposition::Queued,
        "unexpected disposition: {:?}",
        result.disposition
    );
    let mut root_paths = HashMap::new();
    root_paths.insert("music".to_owned(), root.clone());
    let run = coordinator
        .run_once(&root_paths)
        .await
        .expect("worker drove the run");
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::ERRORED), 1);
    let failures = coordinator.store().failures(&run.id);
    let fatal = failures
        .iter()
        .find(|failure| failure.failure_code == failure_codes::TAG_READ_FAILED)
        .expect("fatal row");
    assert_eq!(
        fatal.failure_detail,
        "The tag read failed; the file was skipped for this run."
    );
    cleanup(&root);
}
