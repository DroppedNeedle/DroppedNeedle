//! Library scan: what reaches the catalog, what survives interruption, and
//! what a scan must never do (write music files, lose rows to one bad
//! file, wipe a library whose share went away, follow a symlink out).
//!
//! Every root is a scratch copy of the committed fixtures, removed when
//! the test ends; nothing here touches the network or a real library.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::common::ScratchDir;
use droppedneedle::library::scan::{
    BlockingPool, EffectivePolicy, LibraryRoot, LibraryScanCoordinator, NullTagReader,
    RootRegistry, RootSeamError, RunStore, ScanKind, ScanRequest, ScanRun, ScanScope, ScanState,
    ScanTrigger, ScannedTags, SqliteScanStore, StaticResolver, StreamRootSeam, TagReadError,
    TagReader, WorkWakeups, counter_names, failure_codes,
};
use droppedneedle::library::wiring::LibrarySetup;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/library")
        .join(name)
}

fn plant(root: &Path, relative: &str, fixture_name: &str) -> PathBuf {
    let dest = root.join(relative);
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
    std::fs::copy(fixture(fixture_name), &dest).expect("copy fixture");
    dest
}

fn count(run: &ScanRun, name: &str) -> i64 {
    run.counters.get(name).copied().unwrap_or(0)
}

/// Relative path to (size, mtime, sha256) for every file under `root`.
fn snapshot(root: &Path) -> BTreeMap<String, (u64, i64, String)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).expect("read file");
            let meta = std::fs::metadata(&path).expect("stat");
            let digest = Sha256::digest(&bytes);
            out.insert(
                path.strip_prefix(root)
                    .expect("in root")
                    .to_string_lossy()
                    .into_owned(),
                (
                    bytes.len() as u64,
                    droppedneedle::library::scan::mtime_ns_from_metadata(&meta),
                    format!("{digest:x}"),
                ),
            );
        }
    }
    out
}

/// Production-wired bundle over a scratch database, with one root.
fn bundle(music: &Path) -> LibrarySetup {
    let users = droppedneedle::auth::wiring::AuthSetup::for_tests()
        .expect("test auth builds")
        .users;
    let library = LibrarySetup::for_tests(users, Arc::new(droppedneedle::ids::UuidGenerator))
        .expect("library bundle builds");
    library
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    library
}

/// Request a scan of every root and drive it to the end.
async fn scan(library: &LibrarySetup) -> ScanRun {
    let registry = library.live_registry();
    let result = library
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    for _ in 0..50 {
        library.supervisor_tick().await;
        if library.coordinator.current().is_empty() {
            break;
        }
    }
    library.coordinator.snapshot(&result.run_id).expect("run").0
}

fn query(library: &LibrarySetup, sql: &str) -> i64 {
    library
        .scan_store
        .query_i64_for_tests(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

/// The catalog holds what the tags say, with provenance and header
/// durations, the changed albums are offered to identification once, and
/// neither scan nor a no-op rescan writes a byte of music.
#[tokio::test]
async fn scan_catalogs_tags_without_touching_files() {
    let scratch = ScratchDir::new("scan-tags");
    let music = scratch.join("music");
    let tagged = plant(&music, "Album/01.flac", "management_full.flac");
    plant(&music, "Album/CD1/02.mp3", "mp3_full_01.mp3");
    plant(&music, "Loose/untagged.flac", "flac_no_tags.flac");
    let before = snapshot(&music);
    let library = bundle(&music);

    let run = scan(&library).await;
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::INDEXED), 3);
    let tag = droppedneedle::library::tags::read::read_tag_only(
        &tagged,
        droppedneedle::library::tags::AudioFormat::Flac,
    )
    .expect("tags read");
    assert_eq!(
        query(
            &library,
            &format!(
                "SELECT COUNT(*) FROM local_tracks WHERE relative_path = 'Album/01.flac' \
                 AND title = '{}' AND title_provenance = 'tag' AND duration_seconds > 0 \
                 AND sample_rate > 0",
                tag.title.replace('\'', "''")
            ),
        ),
        1,
        "tag title, provenance, and header properties land"
    );
    assert_eq!(
        query(
            &library,
            "SELECT COUNT(*) FROM local_tracks WHERE relative_path = 'Loose/untagged.flac' \
             AND title_provenance != 'tag' AND album_artist_provenance = 'placeholder'",
        ),
        1,
        "untagged files fall back with honest provenance"
    );
    let albums = query(
        &library,
        "SELECT COUNT(DISTINCT local_album_id) FROM local_tracks",
    );
    let jobs = query(&library, "SELECT COUNT(*) FROM library_identify_jobs");
    assert_eq!(jobs, albums, "one identify job per catalog album");
    assert_eq!(count(&run, counter_names::IDENTIFICATION_ENQUEUED), jobs);

    let again = scan(&library).await;
    assert_eq!(count(&again, counter_names::NEW), 0);
    assert_eq!(count(&again, counter_names::UNCHANGED), 3);
    assert_eq!(
        query(&library, "SELECT COUNT(*) FROM library_identify_jobs"),
        jobs,
        "an unchanged album is not offered again"
    );
    assert_eq!(snapshot(&music), before, "scans never write music files");
}

/// A vanished file is marked missing, never deleted. A scope that loses
/// most of its files at once (an unmounted share) is held back instead.
#[tokio::test]
async fn vanished_files_are_marked_missing_and_mass_loss_is_held_back() {
    let scratch = ScratchDir::new("scan-missing");
    let music = scratch.join("music");
    for n in 0..30 {
        plant(&music, &format!("Album/{n:02}.flac"), "flac_full_01.flac");
    }
    let library = bundle(&music);
    assert_eq!(count(&scan(&library).await, counter_names::INDEXED), 30);

    std::fs::remove_file(music.join("Album/00.flac")).expect("remove one");
    let run = scan(&library).await;
    assert_eq!(count(&run, counter_names::MISSING), 1);
    assert_eq!(
        query(
            &library,
            "SELECT COUNT(*) FROM local_tracks WHERE availability = 'missing' \
             AND relative_path = 'Album/00.flac'",
        ),
        1
    );

    for n in 1..30 {
        std::fs::remove_file(music.join(format!("Album/{n:02}.flac"))).expect("remove");
    }
    let run = scan(&library).await;
    assert_eq!(count(&run, counter_names::MISSING), 0);
    assert!(
        library
            .scan_store
            .failures(&run.id)
            .iter()
            .any(|failure| failure.failure_code == failure_codes::MASS_MISSING_GUARD)
    );
    assert_eq!(
        query(
            &library,
            "SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed'"
        ),
        29,
        "the catalog survives an empty walk"
    );
}

/// Tag reader that signals shutdown on one nominated read.
struct ShutdownOnRead {
    shutdown: watch::Sender<bool>,
    signal_at: u64,
    reads: AtomicU64,
}

impl TagReader for ShutdownOnRead {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) + 1 == self.signal_at {
            let _ = self.shutdown.send(true);
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(ScannedTags::default())
    }
}

fn coordinator<T: TagReader + 'static>(
    root: &Path,
    tags: Arc<T>,
) -> (
    LibraryScanCoordinator<SqliteScanStore, T>,
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
        Arc::new(SqliteScanStore::open_ephemeral().expect("scan store opens")),
        BlockingPool::new(4),
        tags,
        Arc::new(StaticResolver::new(registry)),
    )
    .with_wakeups(WorkWakeups::new());
    let paths = HashMap::from([("music".to_owned(), root.to_owned())]);
    (coordinator, paths)
}

fn request(root: &Path) -> ScanRequest {
    ScanRequest {
        kind: ScanKind::Incremental,
        trigger: ScanTrigger::Manual,
        scopes: vec![ScanScope::root("music", &root.to_string_lossy(), "rev-1")],
        requested_by_user_id: None,
        policy_revision: "rev-1".to_owned(),
    }
}

/// A shutdown mid-index leaves the run active; the next start resumes at
/// the first uncommitted file, so every file lands once and counts once.
#[tokio::test]
async fn shutdown_mid_index_resumes_without_double_counting() {
    let scratch = ScratchDir::new("scan-resume");
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    for n in 0..600 {
        std::fs::write(root.join(format!("t{n:04}.flac")), b"junk").expect("plant");
    }
    let (shutdown, signal) = watch::channel(false);
    let reader = Arc::new(ShutdownOnRead {
        shutdown,
        signal_at: 300,
        reads: AtomicU64::new(0),
    });
    let (coordinator, paths) = coordinator(&root, reader);
    let requested = coordinator.request_run(&request(&root)).expect("requested");
    assert!(
        coordinator
            .run_once_with_shutdown(&paths, &signal)
            .await
            .is_none(),
        "shutdown abandons the drive"
    );
    let (run, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
    assert_eq!(run.state, ScanState::Indexing, "the run stays resumable");

    let run = coordinator.run_once(&paths).await.expect("resumed");
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::TOTAL), 600);
    assert_eq!(count(&run, counter_names::INSPECTED), 600);
    assert_eq!(count(&run, counter_names::INDEXED), 600);
    assert_eq!(
        coordinator
            .store()
            .query_i64_for_tests("SELECT COUNT(*) FROM local_tracks")
            .expect("count"),
        600
    );
}

/// One inventory row the store refuses costs that file only: the rest of
/// its page lands, the failure names the store error, and the scope is
/// not trusted for missing detection.
#[tokio::test]
async fn a_refused_inventory_row_costs_that_file_only() {
    let scratch = ScratchDir::new("scan-refused");
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    for n in 0..10 {
        std::fs::write(root.join(format!("t{n:04}.flac")), b"junk").expect("plant");
    }
    let (coordinator, paths) = coordinator(&root, Arc::new(NullTagReader::new()));
    coordinator
        .store()
        .execute_batch_for_tests(
            "CREATE TRIGGER refuse_one BEFORE INSERT ON library_scan_inventory \
             WHEN NEW.relative_path = 't0003.flac' \
             BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
        )
        .expect("trigger");
    coordinator.request_run(&request(&root)).expect("requested");
    let run = coordinator.run_once(&paths).await.expect("driven");
    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(count(&run, counter_names::INDEXED), 9);
    let failures = coordinator.store().failures(&run.id);
    assert!(failures.iter().any(|failure| {
        failure.failure_code == failure_codes::WALK_ERROR
            && failure.relative_path == "t0003.flac"
            && failure.failure_detail.contains("refused for the test")
    }));
}

/// Symlinks never lead out of a root: the walk audits them and the stream
/// seam refuses them. A root reached through a symlink still treats its
/// own in-root links as inside.
#[tokio::test]
async fn symlinks_never_escape_a_root() {
    use std::os::unix::fs::symlink;

    let scratch = ScratchDir::new("scan-links");
    let real = scratch.join("real");
    std::fs::create_dir_all(&real).expect("real root");
    let root = scratch.join("music");
    symlink(&real, &root).expect("root symlink");
    plant(&root, "good.flac", "flac_full_01.flac");
    symlink(root.join("good.flac"), root.join("alias.flac")).expect("in-root symlink");
    symlink("/etc/hostname", root.join("escape.flac")).expect("symlink");
    let (coordinator, paths) = coordinator(&root, Arc::new(NullTagReader::new()));
    coordinator.request_run(&request(&root)).expect("requested");
    let run = coordinator.run_once(&paths).await.expect("driven");
    assert_eq!(count(&run, counter_names::INDEXED), 1);
    let escapes: Vec<String> = coordinator
        .store()
        .failures(&run.id)
        .into_iter()
        .filter(|failure| failure.failure_code == failure_codes::SYMLINK_ESCAPE_OUT)
        .map(|failure| failure.relative_path)
        .collect();
    assert_eq!(escapes, vec!["escape.flac".to_owned()]);
    let seam = StreamRootSeam::new(RootRegistry::new(
        vec![LibraryRoot::new(
            "music",
            root.clone(),
            EffectivePolicy::Automatic,
        )],
        true,
        "rev-1",
    ));
    assert_eq!(
        seam.resolve_key("music", "escape.flac"),
        Err(RootSeamError::Forbidden)
    );
}

/// A scan request the store cannot record is an error. It used to answer
/// a "conflict" naming a run id that was never written.
#[tokio::test]
async fn unrecorded_scan_request_is_an_error() {
    let scratch = ScratchDir::new("scan-unrecorded");
    let music = scratch.join("music");
    std::fs::create_dir_all(&music).expect("root");
    let library = bundle(&music);
    // No such user in this database: the auth_users reference fails.
    assert!(library.request_scan(None, "no-such-user").is_err());
}
