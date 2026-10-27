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

use crate::common::library::TestSql as _;
use crate::common::library::library_at;
use crate::common::{ScratchDir, TestPlatform, config_store, migrated_db};
use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_kernel::db::{Budget, Db, LaneLimits};
use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_library::identify::models::RecallResult;
use droppedneedle_library::identify::providers::FakeProviders;
use droppedneedle_library::identify::service::{IdentifyDeps, IdentifyService};
use droppedneedle_library::identify::sqlite::SqliteIdentifyStore;
use droppedneedle_library::repairs::hygiene;
use droppedneedle_library::scan::ScanFailureRecord;
use droppedneedle_library::scan::{
    BlockingPool, Counter, EffectivePolicy, Event, FailureCode, LibraryRoot,
    LibraryScanCoordinator, NullTagReader, Phase, RootRegistry, RootSeamError, RunState, ScanKind,
    ScanPhase, ScanRequest, ScanRun, ScanScope, ScanState, ScanStoreError, ScanTrigger,
    ScannedTags, SqliteScanStore, StaticResolver, StreamRootSeam, TagReadError, TagReader,
    WorkWakeups,
};
use droppedneedle_library::wiring::LibrarySetup;
use sha2::{Digest, Sha256};

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

fn count(run: &ScanRun, counter: Counter) -> i64 {
    run.counters.get(counter)
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
                    droppedneedle_library::scan::mtime_ns_from_metadata(&meta),
                    format!("{digest:x}"),
                ),
            );
        }
    }
    out
}

/// Production-wired bundle over a database and settings in `scratch`, with
/// `scratch/music` as its one root.
async fn bundle(scratch: &Path) -> LibrarySetup {
    migrated_db(&scratch.join("app.db"));
    let db = droppedneedle_kernel::db::Db::open_for_tests(&scratch.join("app.db"))
        .expect("database opens");
    let platform = TestPlatform::on(&db, scratch)
        .settings(config_store(scratch))
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = scratch.join("music");
    library
        .scans
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
    let registry = library.core.live_registry();
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
        library.scans.supervisor_tick().await;
        if library.coordinator.current().is_empty() {
            break;
        }
    }
    library.coordinator.snapshot(&result.run_id).expect("run").0
}

fn query(library: &LibrarySetup, sql: &str) -> i64 {
    library
        .scan_store
        .db()
        .query_i64(sql)
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
    let library = bundle(&scratch).await;

    let run = scan(&library).await;
    assert_eq!(run.state.scan_state(), ScanState::Completed);
    assert_eq!(count(&run, Counter::Indexed), 3);
    let tag = droppedneedle_library::tags::read::read_tag_only(
        &tagged,
        droppedneedle_library::tags::AudioFormat::Flac,
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
    assert_eq!(count(&run, Counter::IdentificationEnqueued), jobs);

    let again = scan(&library).await;
    assert_eq!(count(&again, Counter::New), 0);
    assert_eq!(count(&again, Counter::Unchanged), 3);
    assert_eq!(
        query(&library, "SELECT COUNT(*) FROM library_identify_jobs"),
        jobs,
        "an unchanged album is not offered again"
    );
    assert_eq!(snapshot(&music), before, "scans never write music files");
}

/// Drafts and sealed decisions compare a file's tag revision to spot stale
/// state, so a new tag the scan reads must not change it: a file whose tags
/// did not change keeps the revision scans have always stored for it.
#[tokio::test]
async fn unchanged_tags_keep_their_revision() {
    const STORED: &str = "2b6fee3851ee5022e3d48c722ca55b1be2fc7ccede88c843fc387c8618cfe51f";
    let scratch = ScratchDir::new("scan-revision");
    let music = scratch.join("music");
    plant(&music, "Album/01.flac", "management_full.flac");
    let library = bundle(&scratch).await;

    let run = scan(&library).await;
    assert_eq!(run.state.scan_state(), ScanState::Completed);
    assert_eq!(
        query(
            &library,
            &format!(
                "SELECT COUNT(*) FROM local_tracks WHERE relative_path = 'Album/01.flac' \
                 AND tag_revision = '{STORED}'"
            ),
        ),
        1,
        "the tag revision of an unchanged file changed"
    );
}

/// A daily scan runs at the set time in the server's time zone (`TZ`), on
/// both sides of a daylight-saving change, and not a minute before.
#[tokio::test]
async fn daily_scans_run_at_local_time_across_a_clock_change() {
    use droppedneedle_kernel::clock::ManualClock;
    use droppedneedle_kernel::config::AppConfig;
    use droppedneedle_kernel::runtime_config::sections::{LibraryScanSchedule, ScanFrequency};
    // London moves from GMT to BST at 01:00 UTC on Sunday 30 March 2025.
    const SATURDAY_0300_GMT: i64 = 1_743_217_200_000;
    const SUNDAY_0300_BST: i64 = 1_743_300_000_000;
    let scratch = ScratchDir::new("scan-daily-zone");
    std::fs::create_dir_all(scratch.join("music")).expect("music folder");
    migrated_db(&scratch.join("app.db"));
    let db = Db::open_for_tests(&scratch.join("app.db")).expect("database opens");
    let clock = ManualClock::at(SATURDAY_0300_GMT - 15 * 3_600_000);
    let mut config = AppConfig::with_root(0, &scratch);
    config.timezone = Some("Europe/London".to_owned());
    let platform = TestPlatform::on(&db, &scratch)
        .settings(config_store(&scratch))
        .clock(Arc::new(clock.clone()))
        .config(config)
        .build()
        .await;
    platform
        .settings
        .save_blocking(LibraryScanSchedule {
            scan_frequency: ScanFrequency::Daily,
            daily_scan_time: "03:00".to_owned(),
        })
        .expect("schedule saves");
    let (library, _) = library_at(&platform);
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            scratch.join("music").to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let settle = || async {
        for _ in 0..50 {
            library.scans.supervisor_tick().await;
            if library.coordinator.current().is_empty() {
                break;
            }
        }
    };
    // The new folder's first scan ends on Friday at noon.
    settle().await;
    let automatic = || {
        query(
            &library,
            "SELECT COUNT(*) FROM library_scan_runs WHERE trigger = 'automatic'",
        )
    };
    for (due, day) in [(SATURDAY_0300_GMT, "Saturday"), (SUNDAY_0300_BST, "Sunday")] {
        let before = automatic();
        clock.set(due - 60_000);
        settle().await;
        assert_eq!(automatic(), before, "{day}: no scan before 03:00");
        clock.set(due);
        settle().await;
        assert_eq!(automatic(), before + 1, "{day}: one scan at 03:00");
    }
}

/// One file scanned, then its row put back the way an upgrade carries it:
/// no edition hints, its own title, its album unmatched and no
/// identification waiting.
async fn carried_file(name: &str, fixture_name: &str) -> (ScratchDir, LibrarySetup) {
    let scratch = ScratchDir::new(name);
    plant(&scratch.join("music"), "Album/01.flac", fixture_name);
    let library = bundle(&scratch).await;
    scan(&library).await;
    library
        .scan_store
        .db()
        .run_sql(
            "DELETE FROM local_track_edition_tags; DELETE FROM library_identify_jobs; \
             UPDATE local_tracks SET title = 'Carried', membership_source = 'legacy_import'; \
             INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
             updated_at) SELECT local_album_id, 'unmatched', 'NO_CANDIDATES', 0 \
             FROM local_tracks",
        )
        .expect("carried row");
    (scratch, library)
}

/// Tags that carry a release date and a disc total of one, and no other
/// edition hint.
struct DateOnlyReader;

impl TagReader for DateOnlyReader {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        let mut read = ScannedTags::default();
        read.tag.title = "Song".to_owned();
        read.tag.artist = "Artist".to_owned();
        read.tag.album_artist = Some("Artist".to_owned());
        read.tag.album = "Album".to_owned();
        read.tag.track_number = 1;
        read.tag.edition.release_date = Some("1997-03-01".to_owned());
        read.tag.edition.total_discs = Some(1);
        Ok(read)
    }
}

/// A release date or a single-disc total is stored, but edition matching
/// reads neither, so the album is not identified again for it.
#[tokio::test]
async fn hints_matching_cannot_use_do_not_look_an_album_up_again() {
    let scratch = ScratchDir::new("scan-edition-date");
    let music = scratch.join("music");
    plant(&music, "Album/01.flac", "flac_full_01.flac");
    let (coordinator, paths) = coordinator(&music, Arc::new(DateOnlyReader));
    let scan = || async {
        coordinator
            .request_run(&request(&music))
            .expect("requested");
        coordinator.run_once(&paths).await.expect("scan runs")
    };
    scan().await;
    let db = coordinator.store().db();
    db.run_sql(
        "DELETE FROM local_track_edition_tags; DELETE FROM library_identify_jobs; \
         INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         updated_at) SELECT local_album_id, 'unmatched', 'NO_CANDIDATES', 0 FROM local_tracks",
    )
    .expect("carried row");

    scan().await;
    let count = |sql: &str| db.query_i64(sql).expect("counts");
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM local_track_edition_tags \
             WHERE release_date = '1997-03-01' AND total_discs = 1"
        ),
        1,
        "the row is filled"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM library_identify_jobs"),
        0,
        "a date alone is no reason to look the album up again"
    );
}

/// A file carried over from v2 stays unchanged, so the scan never read
/// the edition hints in its tags (barcode, catalog number, media). A scan
/// stores them from the file's tags and changes nothing else: the carried
/// row keeps what it holds, the file counts as unchanged and is never
/// written, and an album without a confirmed match is offered to
/// identification again so the hints can steer its edition.
#[tokio::test]
async fn a_carried_file_gets_its_edition_hints_from_its_tags() {
    let (scratch, library) = carried_file("scan-edition-fill", "management_full.flac").await;
    let music = scratch.join("music");
    let before = snapshot(&music);
    let db = library.scan_store.db();

    let run = scan(&library).await;
    assert_eq!(run.state.scan_state(), ScanState::Completed);
    assert_eq!(count(&run, Counter::Unchanged), 1);
    assert_eq!(count(&run, Counter::Changed), 0);
    assert_eq!(
        query(
            &library,
            "SELECT COUNT(*) FROM local_track_edition_tags WHERE barcode = '1234567890123' \
             AND catalog_number = 'CAT-42' AND media = 'Digital Media' \
             AND release_country = 'GB'",
        ),
        1,
        "the edition hints come from the file's tags"
    );
    assert_eq!(
        query(
            &library,
            "SELECT COUNT(*) FROM local_tracks WHERE title = 'Carried'"
        ),
        1,
        "nothing else in the carried row changes"
    );
    assert_eq!(
        query(&library, "SELECT COUNT(*) FROM library_identify_jobs"),
        1,
        "the unmatched album is looked up again"
    );
    assert_eq!(snapshot(&music), before, "scans never write music files");

    db.run_sql("UPDATE local_track_edition_tags SET barcode = 'kept'")
        .expect("marked");
    scan(&library).await;
    assert_eq!(
        query(
            &library,
            "SELECT COUNT(*) FROM local_track_edition_tags WHERE barcode = 'kept'"
        ),
        1,
        "a filled file is not read again"
    );
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
    let library = bundle(&scratch).await;
    assert_eq!(count(&scan(&library).await, Counter::Indexed), 30);

    std::fs::remove_file(music.join("Album/00.flac")).expect("remove one");
    let run = scan(&library).await;
    assert_eq!(count(&run, Counter::Missing), 1);
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
    assert_eq!(count(&run, Counter::Missing), 0);
    assert!(
        library
            .scan_store
            .failures(&run.id)
            .unwrap()
            .iter()
            .any(|failure| failure.failure_code == FailureCode::MassMissingGuard)
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

/// Scan only `folder` of the music root and drive it to the end.
async fn scan_folder(library: &LibrarySetup, folder: &str) -> ScanRun {
    let registry = library.core.live_registry();
    let mut scopes = registry.scheduled_root_scopes();
    for scope in &mut scopes {
        scope.relative_path = folder.to_owned();
    }
    let result = library
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes,
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    for _ in 0..50 {
        library.scans.supervisor_tick().await;
        if library.coordinator.current().is_empty() {
            break;
        }
    }
    library.coordinator.snapshot(&result.run_id).expect("run").0
}

/// A folder the guard holds back stays held, from the start of the first
/// scan that held it, through more held scans and through a clean scan of
/// a narrower folder. The first scan that finds it as expected lets it go
/// and looks again at the albums parked on the hold, and only those. A
/// clean scan with nothing held wakes nothing.
#[tokio::test]
async fn a_held_back_folder_is_held_until_a_scan_finds_it_again() {
    let scratch = ScratchDir::new("scan-held");
    let music = scratch.join("music");
    for n in 0..30 {
        plant(&music, &format!("Album/{n:02}.flac"), "flac_full_01.flac");
    }
    plant(&music, "Other/01.flac", "flac_full_01.flac");
    let library = bundle(&scratch).await;
    assert_eq!(count(&scan(&library).await, Counter::Indexed), 31);
    let held = |library: &LibrarySetup| {
        (
            query(library, "SELECT COUNT(*) FROM library_scan_held_scopes"),
            query(
                library,
                "SELECT COUNT(*) FROM library_scan_held_scopes \
                 WHERE root_id = 'music' AND relative_path = '.'",
            ),
        )
    };

    let aside = scratch.join("aside");
    std::fs::create_dir_all(&aside).expect("aside");
    for n in 1..30 {
        let name = format!("{n:02}.flac");
        std::fs::rename(music.join("Album").join(&name), aside.join(&name)).expect("move");
    }
    let first = scan(&library).await;
    assert_eq!(held(&library), (1, 1));
    let held_at = query(&library, "SELECT held_at FROM library_scan_held_scopes");
    assert_eq!(
        held_at,
        query(
            &library,
            &format!(
                "SELECT CAST(started_at * 1000 AS INTEGER) FROM library_scan_runs \
                 WHERE id = '{}'",
                first.id
            ),
        )
    );
    scan(&library).await;
    assert_eq!(held(&library), (1, 1));
    assert_eq!(
        query(&library, "SELECT held_at FROM library_scan_held_scopes"),
        held_at,
        "a hold keeps its start"
    );
    let narrow = scan_folder(&library, "Other").await;
    assert_eq!(narrow.state.scan_state(), ScanState::Completed);
    assert_eq!(
        held(&library),
        (1, 1),
        "a narrower clean scan lets nothing go"
    );

    let album_of = |path: &str| {
        library
            .scan_store
            .album_for_track(
                &library
                    .scan_store
                    .track_at("music", path)
                    .unwrap()
                    .expect("track"),
            )
            .unwrap()
            .expect("album")
    };
    let (parked, taken) = (album_of("Album/00.flac"), album_of("Other/01.flac"));
    library
        .scan_store
        .db()
        .run_sql(&format!(
            "INSERT INTO library_management_automatic_queue (local_album_id, queued_at, \
             not_before, attempts, last_reason_code) VALUES \
             ('{parked}', 1, 1e15, 3, 'SCAN_HELD_BACK'), ('{taken}', 1, 1e15, 0, 'TAKEN_OVER');"
        ))
        .expect("parked");
    let due = |library: &LibrarySetup, album: &str| {
        query(
            library,
            &format!(
                "SELECT COUNT(*) FROM library_management_automatic_queue \
                 WHERE local_album_id = '{album}' AND not_before < 1e15 AND attempts = 0"
            ),
        )
    };

    for n in 1..30 {
        let name = format!("{n:02}.flac");
        std::fs::rename(aside.join(&name), music.join("Album").join(&name)).expect("move back");
    }
    scan(&library).await;
    assert_eq!(held(&library), (0, 0));
    assert_eq!(due(&library, &parked), 1, "looked at again");
    assert_eq!(due(&library, &taken), 0, "another reason stays");

    library
        .scan_store
        .db()
        .run_sql(&format!(
            "UPDATE library_management_automatic_queue SET not_before = 1e15 \
             WHERE local_album_id = '{parked}'"
        ))
        .expect("parked again");
    scan(&library).await;
    assert_eq!(due(&library, &parked), 0, "nothing held, nothing woken");
}

/// Tag reader that signals shutdown on one nominated read, and holds that
/// read until the test has seen the drive abandoned.
struct ShutdownOnRead {
    shutdown: CancellationToken,
    signal_at: u64,
    reads: AtomicU64,
    abandoned: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl TagReader for ShutdownOnRead {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) + 1 == self.signal_at {
            self.shutdown.cancel();
            if let Ok(abandoned) = self.abandoned.lock() {
                let _ = abandoned.recv();
            }
        }
        Ok(ScannedTags::default())
    }
}

fn coordinator<T: TagReader + 'static>(
    root: &Path,
    tags: Arc<T>,
) -> (LibraryScanCoordinator<T>, HashMap<String, PathBuf>) {
    // The database sits beside the music, in the test's scratch folder.
    let db = root.with_file_name("scan.db");
    migrated_db(&db);
    let db = Db::open_for_tests(&db).expect("database opens");
    coordinator_on(db, root, tags)
}

/// A coordinator over `db` with `root` as its one root.
fn coordinator_on<T: TagReader + 'static>(
    db: Db,
    root: &Path,
    tags: Arc<T>,
) -> (LibraryScanCoordinator<T>, HashMap<String, PathBuf>) {
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
        Arc::new(SqliteScanStore::new(db, Arc::new(SystemClock))),
        BlockingPool::new(4),
        tags,
        Arc::new(StaticResolver::new(registry)),
        Arc::new(SystemClock),
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
    let signal = CancellationToken::new();
    let (resume, abandoned) = std::sync::mpsc::channel();
    let reader = Arc::new(ShutdownOnRead {
        shutdown: signal.clone(),
        signal_at: 300,
        reads: AtomicU64::new(0),
        abandoned: std::sync::Mutex::new(abandoned),
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
    drop(resume);
    let (run, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
    assert_eq!(
        run.state.scan_state(),
        ScanState::Indexing,
        "the run stays resumable"
    );

    let run = coordinator.run_once(&paths).await.expect("resumed");
    assert_eq!(run.state.scan_state(), ScanState::Completed);
    assert_eq!(count(&run, Counter::Total), 600);
    assert_eq!(count(&run, Counter::Inspected), 600);
    assert_eq!(count(&run, Counter::Indexed), 600);
    assert_eq!(
        coordinator
            .store()
            .db()
            .query_i64("SELECT COUNT(*) FROM local_tracks")
            .expect("count"),
        600
    );
}

/// Tag reader that holds one nominated read until the test lets it go,
/// then signals shutdown before the read returns, so nothing after it
/// commits.
struct HoldOnRead {
    shutdown: CancellationToken,
    hold_at: u64,
    reads: AtomicU64,
    reached: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl TagReader for HoldOnRead {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) + 1 == self.hold_at {
            if let Ok(reached) = self.reached.lock() {
                let _ = reached.send(());
            }
            if let Ok(release) = self.release.lock() {
                let _ = release.recv();
            }
            self.shutdown.cancel();
        }
        Ok(ScannedTags::default())
    }
}

/// Tag reader that holds every read while its gate is shut.
#[derive(Default)]
struct GatedReader {
    /// Whether the gate is open, and how many reads wait at it.
    state: std::sync::Mutex<(bool, usize)>,
    turned: std::sync::Condvar,
}

impl GatedReader {
    fn set(&self, open: bool) {
        self.state.lock().expect("gate").0 = open;
        self.turned.notify_all();
    }

    /// Wait until a read is held at the shut gate.
    async fn held(self: &Arc<Self>) {
        let gate = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let state = gate.state.lock().expect("gate");
            let (state, timeout) = gate
                .turned
                .wait_timeout_while(state, Duration::from_secs(30), |(_, waiting)| *waiting == 0)
                .expect("gate");
            assert!(
                !timeout.timed_out() && state.1 > 0,
                "no read reached the gate"
            );
        })
        .await
        .expect("waits");
    }
}

impl TagReader for GatedReader {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        let mut state = self.state.lock().expect("gate");
        state.1 += 1;
        self.turned.notify_all();
        let mut state = self
            .turned
            .wait_while(state, |(open, _)| !*open)
            .expect("gate");
        state.1 -= 1;
        Ok(ScannedTags::default())
    }
}

/// A pause, a resume and a stop asked of a running scan each answer what
/// the run does now and settle where the worker can stop: the paused run
/// picks up in the phase it left, the stopped one keeps the time it spent
/// in each phase it reached, and an ended run refuses another pause.
#[tokio::test]
async fn pause_resume_and_stop_settle_where_the_worker_stops() {
    let scratch = ScratchDir::new("scan-controls");
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    for n in 0..20 {
        std::fs::write(root.join(format!("t{n:02}.flac")), b"junk").expect("plant");
    }
    let gate = Arc::new(GatedReader::default());
    let (coordinator, paths) = coordinator(&root, Arc::clone(&gate));
    let id = coordinator
        .request_run(&request(&root))
        .expect("requested")
        .run_id;
    let indexing = RunState::Running(Phase::Indexing);

    // Paused while the worker is held inside a tag read.
    let (paused, ()) = tokio::join!(coordinator.run_once(&paths), async {
        gate.held().await;
        let (run, _, _) = coordinator.snapshot(&id).expect("run");
        assert_eq!(run.state, indexing);
        let (run, _) = coordinator
            .control(&id, Event::Pause, run.row_revision)
            .expect("pause is taken");
        assert_eq!(run.state, RunState::Pausing(Phase::Indexing));
        gate.set(true);
    });
    let paused = paused.expect("the drive settles");
    assert_eq!(paused.state, RunState::Paused(Phase::Indexing));
    assert_eq!(paused.state.resume_phase(), Some(ScanPhase::Indexing));

    // Resumed, then stopped while held again.
    gate.set(false);
    let (run, _) = coordinator
        .control(&id, Event::Resume, paused.row_revision)
        .expect("resume is taken");
    assert_eq!(run.state, indexing);
    let (stopped, ()) = tokio::join!(coordinator.run_once(&paths), async {
        gate.held().await;
        let (run, _, _) = coordinator.snapshot(&id).expect("run");
        assert_eq!(run.state, indexing);
        let (run, _) = coordinator
            .control(&id, Event::Stop, run.row_revision)
            .expect("stop is taken");
        assert_eq!(run.state, RunState::Stopping(Phase::Indexing));
        gate.set(true);
    });
    let stopped = stopped.expect("the drive settles");
    assert_eq!(stopped.state.scan_state(), ScanState::Cancelled);
    for phase in [Phase::Discovering, Phase::Indexing] {
        assert!(
            stopped.phase_timings.contains_key(&phase),
            "time kept for {phase:?}: {:?}",
            stopped.phase_timings
        );
    }
    let refused = coordinator.control(&id, Event::Pause, stopped.row_revision);
    assert!(
        matches!(refused, Err(ScanStoreError::InvalidControl { .. })),
        "{refused:?}"
    );
}

/// New music shows as a scan goes: each window that writes catalog rows
/// moves the catalog revision in its own transaction, open tabs hear of it
/// while the scan is still running, and the move outlives a shutdown that
/// leaves the run unfinished.
#[tokio::test]
async fn a_scan_moves_the_catalog_revision_as_it_goes() {
    use droppedneedle_kernel::db::{DbConfig, open_runtime};
    use droppedneedle_kernel::events::{EventHub, revisions};

    let scratch = ScratchDir::new("scan-revision");
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    // One window of 256 files, and the first read of the next is held.
    for n in 0..300 {
        std::fs::write(root.join(format!("t{n:04}.flac")), b"junk").expect("plant");
    }
    let path = scratch.join("scan.db");
    migrated_db(&path);
    let runtime = open_runtime(&DbConfig::new(&path))
        .await
        .expect("database opens");
    let db = runtime.db().clone();
    let revision = |db: &Db| {
        db.query_i64("SELECT value FROM library_catalog_revision WHERE singleton = 1")
            .expect("revision reads")
    };
    let before = revision(&db);

    let hub = EventHub::new();
    let mut listener = hub.subscribe("listener");
    let supervisor = droppedneedle_kernel::jobs::Supervisor::start(
        db.clone(),
        Arc::new(SystemClock),
        CancellationToken::new(),
    )
    .await
    .expect("supervisor starts");
    revisions::start(&supervisor, hub.clone(), db.clone()).expect("event loop starts");

    let signal = CancellationToken::new();
    let (reached_tx, reached) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let reader = Arc::new(HoldOnRead {
        shutdown: signal.clone(),
        hold_at: 257,
        reads: AtomicU64::new(0),
        reached: std::sync::Mutex::new(reached_tx),
        release: std::sync::Mutex::new(release_rx),
    });
    let (coordinator, paths) = coordinator_on(db.clone(), &root, reader);
    let requested = coordinator.request_run(&request(&root)).expect("requested");

    let held = async {
        tokio::task::spawn_blocking(move || reached.recv_timeout(Duration::from_secs(30)))
            .await
            .expect("waits")
            .expect("the second window's first read is reached");
        let (run, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
        assert_eq!(
            count(&run, Counter::Indexed),
            256,
            "held after the first window"
        );
        let moved = revision(&db);
        assert!(
            moved > before,
            "the first window moved the catalog revision: {before} -> {moved}"
        );
        let heard = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let frame = listener.next().await.expect("the hub stays open");
                if frame.name() != "activity.changed" {
                    continue;
                }
                let data: serde_json::Value =
                    serde_json::from_str(frame.data()).expect("data is JSON");
                if data["revisions"]["catalog"].as_i64().unwrap_or(0) > before {
                    return;
                }
            }
        })
        .await;
        assert!(heard.is_ok(), "a tab hears the moved catalog mid-scan");
        release.send(()).expect("released");
        moved
    };
    let (drive, moved) = tokio::join!(coordinator.run_once_with_shutdown(&paths, &signal), held);
    assert!(drive.is_none(), "shutdown abandons the drive");
    let (run, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
    assert_eq!(
        run.state.scan_state(),
        ScanState::Indexing,
        "the run never ended"
    );
    drop(coordinator);

    // A closed lane ends the event loop, with the supervisor never stopped.
    runtime.shutdown().await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.is_running(revisions::JOB_NAME) {
        assert!(
            std::time::Instant::now() < deadline,
            "the event loop ends with the lane"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    supervisor.shutdown(Duration::from_secs(5)).await;

    let runtime = open_runtime(&DbConfig::new(&path))
        .await
        .expect("database opens again");
    assert_eq!(
        revision(runtime.db()),
        moved,
        "the bump outlived a shutdown before the run's end"
    );
    runtime.shutdown().await;
}

/// A run that has ended but still owes its end passes counts as a scan
/// in progress for the work that must never run beside one, until a drive
/// settles it.
#[tokio::test]
async fn a_run_owing_its_end_passes_still_counts_as_scanning() {
    let scratch = ScratchDir::new("scan-end-owed");
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    let (coordinator, paths) = coordinator(&root, Arc::new(NullTagReader::new()));
    let requested = coordinator.request_run(&request(&root)).expect("requested");
    let (run, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
    coordinator
        .store()
        .request_control(&run.id, Event::Stop, run.row_revision, 1.0)
        .expect("ended without its passes");
    let current = coordinator.current();
    assert_eq!(current.len(), 1, "the run owing passes counts");
    assert_eq!(current[0].id, run.id);
    assert!(coordinator.run_once(&paths).await.is_none());
    assert!(
        coordinator.current().is_empty(),
        "settled, it no longer counts"
    );
}

/// A run whose end passes fail still ends. Every file vanishes, so the
/// mass-missing guard holds the scope back, and recording the held folder
/// is refused at the run's end; the run completes all the same and leaves
/// nothing for the next drive.
#[tokio::test]
async fn a_run_whose_end_passes_fail_still_ends() {
    let scratch = ScratchDir::new("scan-end-fails");
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    for n in 0..25 {
        std::fs::write(root.join(format!("t{n:04}.flac")), b"junk").expect("plant");
    }
    let (coordinator, paths) = coordinator(&root, Arc::new(NullTagReader::new()));
    coordinator.request_run(&request(&root)).expect("requested");
    let first = coordinator.run_once(&paths).await.expect("driven");
    assert_eq!(first.state.scan_state(), ScanState::Completed);
    for n in 0..25 {
        std::fs::remove_file(root.join(format!("t{n:04}.flac"))).expect("remove");
    }
    coordinator
        .store()
        .db()
        .run_sql(
            "CREATE TRIGGER refuse_hold BEFORE INSERT ON library_scan_held_scopes \
             BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
        )
        .expect("trigger");
    let requested = coordinator.request_run(&request(&root)).expect("requested");
    let ended = coordinator.run_once(&paths).await.expect("driven");
    assert_eq!(ended.state.scan_state(), ScanState::Completed);
    let (stored, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
    assert_eq!(stored.state.scan_state(), ScanState::Completed);
    let failures = coordinator
        .store()
        .failures(&requested.run_id)
        .expect("failures");
    assert!(
        failures
            .iter()
            .any(|failure| failure.failure_code == FailureCode::MassMissingGuard),
        "the guard held the scope back"
    );
    assert!(
        coordinator.run_once(&paths).await.is_none(),
        "nothing is left to drive"
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
        .db()
        .run_sql(
            "CREATE TRIGGER refuse_one BEFORE INSERT ON library_scan_inventory \
             WHEN NEW.relative_path = 't0003.flac' \
             BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
        )
        .expect("trigger");
    coordinator.request_run(&request(&root)).expect("requested");
    let run = coordinator.run_once(&paths).await.expect("driven");
    assert_eq!(run.state.scan_state(), ScanState::Completed);
    assert_eq!(count(&run, Counter::Indexed), 9);
    let failures = coordinator.store().failures(&run.id).unwrap();
    assert!(failures.iter().any(|failure| {
        failure.failure_code == FailureCode::WalkError
            && failure.relative_path == "t0003.flac"
            && failure.failure_detail.contains("refused for the test")
    }));
}

/// Scan `files` junk files in one index window. With `bad`, the catalog
/// refuses `t0002.flac` and the catalog write of `t0005.flac` runs past
/// the write budget, which is short here. Returns the run, its failure
/// list and the catalog rows.
async fn window_with_bad_files(
    tag: &str,
    files: usize,
    bad: bool,
) -> (ScanRun, Vec<ScanFailureRecord>, i64) {
    let scratch = ScratchDir::new(tag);
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    for n in 0..files {
        std::fs::write(root.join(format!("t{n:04}.flac")), b"junk").expect("plant");
    }
    let path = scratch.join("scan.db");
    migrated_db(&path);
    let limits = LaneLimits {
        background: Budget {
            warn: Duration::from_millis(250),
            abort: Duration::from_secs(1),
        },
        ..LaneLimits::default()
    };
    let db = Db::open_for_tests_with(&path, limits).expect("database opens");
    let (coordinator, paths) = coordinator_on(db, &root, Arc::new(NullTagReader::new()));
    if bad {
        // A count over a three-way cross join of 1,000 rows outlasts the
        // budget many times over; the lane aborts it.
        coordinator
            .store()
            .db()
            .run_sql(
                "CREATE TABLE spin (v INTEGER); \
                 INSERT INTO spin WITH RECURSIVE n(i) AS \
                 (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000) SELECT i FROM n; \
                 CREATE TRIGGER refuse_one BEFORE INSERT ON local_tracks \
                 WHEN NEW.relative_path = 't0002.flac' \
                 BEGIN SELECT RAISE(ABORT, 'refused for the test'); END; \
                 CREATE TRIGGER slow_one BEFORE INSERT ON local_tracks \
                 WHEN NEW.relative_path = 't0005.flac' \
                 BEGIN SELECT (SELECT COUNT(*) FROM spin a, spin b, spin c); END;",
            )
            .expect("triggers");
    }
    coordinator.request_run(&request(&root)).expect("requested");
    let run = coordinator.run_once(&paths).await.expect("driven");
    let failures = coordinator.store().failures(&run.id).expect("failures");
    let tracks = coordinator
        .store()
        .db()
        .query_i64("SELECT COUNT(*) FROM local_tracks")
        .expect("count");
    (run, failures, tracks)
}

/// One index window holds a file the catalog refuses and a file whose
/// catalog write runs past the write budget. Each costs that file only:
/// the window is committed again row by row, and split by path down to
/// the slow file, which is given up. Every other file lands, the run
/// completes, both files are on the failure list, and the counts equal a
/// clean scan's but for those two files, moved from indexed to errored.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_and_a_slow_file_cost_those_files_only() {
    let (clean, _, clean_tracks) = window_with_bad_files("scan-window-clean", 8, false).await;
    let (run, failures, tracks) = window_with_bad_files("scan-window-bad", 8, true).await;
    assert_eq!(run.state.scan_state(), ScanState::Completed);
    assert_eq!((clean_tracks, tracks), (8, 6));
    let mut refused: Vec<&str> = failures
        .iter()
        .filter(|failure| failure.failure_code == FailureCode::CatalogCommitFailed)
        .map(|failure| failure.relative_path.as_str())
        .collect();
    refused.sort_unstable();
    assert_eq!(refused, ["t0002.flac", "t0005.flac"]);
    // Untagged, each file is an album of its own, so the two files that
    // did not land take their identification offers with them.
    let moved = |counter| match counter {
        Counter::Indexed | Counter::New | Counter::IdentificationEnqueued => -2,
        Counter::Errored => 2,
        _ => 0,
    };
    for counter in Counter::ALL {
        assert_eq!(
            count(&run, counter),
            count(&clean, counter) + moved(counter),
            "counter {counter:?}"
        );
    }
}

/// A foreground write made while a scan indexes waits at most for one
/// bulk turn: it is answered, never refused busy, well inside a second.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_write_during_indexing_is_answered_quickly() {
    let scratch = ScratchDir::new("scan-foreground");
    let root = scratch.join("music");
    std::fs::create_dir_all(&root).expect("root");
    for n in 0..4096 {
        std::fs::write(root.join(format!("t{n:04}.flac")), b"junk").expect("plant");
    }
    let (coordinator, paths) = coordinator(&root, Arc::new(NullTagReader::new()));
    let coordinator = Arc::new(coordinator);
    let requested = coordinator.request_run(&request(&root)).expect("requested");
    let driver = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { coordinator.run_once(&paths).await })
    };
    loop {
        let (run, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
        assert!(
            run.terminal_at.is_none(),
            "the scan ended before it was probed"
        );
        if count(&run, Counter::Indexed) > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let db = coordinator.store().db().clone();
    let probe = tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let answer = db.run_sql(
            "UPDATE library_event_stream_revisions SET value = value WHERE stream_kind = 'scan'",
        );
        (answer, started.elapsed())
    });
    let (answer, waited) = probe.await.expect("probe runs");
    let (run, _, _) = coordinator.snapshot(&requested.run_id).expect("run");
    assert!(
        run.terminal_at.is_none(),
        "the probe ran while the scan indexed"
    );
    answer.expect("a foreground write is answered");
    assert!(waited < Duration::from_secs(1), "it waited {waited:?}");
    let ended = driver.await.expect("drive").expect("driven");
    assert_eq!(ended.state.scan_state(), ScanState::Completed);
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
    assert_eq!(count(&run, Counter::Indexed), 1);
    let escapes: Vec<String> = coordinator
        .store()
        .failures(&run.id)
        .unwrap()
        .into_iter()
        .filter(|failure| failure.failure_code == FailureCode::SymlinkEscapeOut)
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

/// The lyrics sweep marks the tracks whose files carry lyrics, a tag or a
/// `.lrc` named like the track (its extension in any case), and only reads:
/// no audio or lyrics file changes, a `.lrc` that is a link is not
/// followed, and a `.lrc` deleted before the next scan takes its mark.
#[tokio::test]
async fn lyrics_sweep_marks_what_files_carry_and_only_reads() {
    use std::os::unix::fs::symlink;

    let scratch = ScratchDir::new("scan-lyrics");
    let music = scratch.join("music");
    plant(&music, "Tagged/01 Words.flac", "flac_lyrics_tag.flac");
    plant(&music, "Album/01 Airbag.flac", "flac_full_01.flac");
    let sidecar = plant(&music, "Album/01 Airbag.LRC", "lyrics_airbag.lrc");
    plant(
        &music,
        "Album/02 Paranoid Android.flac",
        "flac_full_02.flac",
    );
    plant(&music, "Linked/01 Song A.flac", "flac_compilation_01.flac");
    let elsewhere = plant(&scratch.join("elsewhere"), "song.lrc", "lyrics_airbag.lrc");
    symlink(&elsewhere, music.join("Linked/01 Song A.lrc")).expect("lyrics link");
    let before = snapshot(&music);
    let library = bundle(&scratch).await;
    let mark = |library: &LibrarySetup, path: &str, source: &str| {
        query(
            library,
            &format!(
                "SELECT COUNT(*) FROM local_track_lyrics l \
                 JOIN local_tracks t ON t.id = l.local_track_id \
                 WHERE t.relative_path = '{path}' AND l.source = '{source}'"
            ),
        )
    };
    let marks = |library: &LibrarySetup| query(library, "SELECT COUNT(*) FROM local_track_lyrics");

    scan(&library).await;
    assert!(
        library
            .scan_store
            .refresh_track_lyrics(&|| false)
            .unwrap()
            .complete
    );
    assert_eq!(mark(&library, "Tagged/01 Words.flac", "tag"), 1);
    assert_eq!(mark(&library, "Album/01 Airbag.flac", "sidecar"), 1);
    assert_eq!(marks(&library), 2, "no plain file and no linked .lrc");
    assert_eq!(snapshot(&music), before, "the sweep never writes a file");

    std::fs::remove_file(&sidecar).expect("lyrics file removes");
    // However coarse the filesystem's clock, the folder reads as changed.
    std::fs::File::open(music.join("Album"))
        .and_then(|dir| {
            dir.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
        })
        .expect("folder time sets");
    scan(&library).await;
    assert!(
        library
            .scan_store
            .refresh_track_lyrics(&|| false)
            .unwrap()
            .complete
    );
    assert_eq!(mark(&library, "Tagged/01 Words.flac", "tag"), 1);
    assert_eq!(marks(&library), 1, "the deleted .lrc took its mark");
    let mut kept = before;
    kept.remove("Album/01 Airbag.LRC");
    assert_eq!(snapshot(&music), kept);
}

/// A scan request the store cannot record is an error. It used to answer
/// a "conflict" naming a run id that was never written.
#[tokio::test]
async fn unrecorded_scan_request_is_an_error() {
    let scratch = ScratchDir::new("scan-unrecorded");
    let music = scratch.join("music");
    std::fs::create_dir_all(&music).expect("root");
    let library = bundle(&scratch).await;
    // No such user in this database: the auth_users reference fails.
    assert!(library.scans.request_scan(None, "no-such-user").is_err());
}

/// What happens between two scan windows: a listener, a curator or a
/// job acting while the scan is half way through an album.
type Meanwhile = Box<dyn FnOnce() + Send>;

/// Tag reader for the regroup briefs: every file reads as a track of the
/// album `albums` names for its path, and one nominated read first runs
/// `meanwhile`.
struct RetagReader {
    albums: std::sync::Mutex<HashMap<String, String>>,
    /// The MusicBrainz ids and numbers a file carries, when it has any.
    ids: std::sync::Mutex<HashMap<String, FileIds>>,
    /// File name, and what happens just before it is read.
    meanwhile: std::sync::Mutex<Option<(String, Meanwhile)>>,
    /// The length a file reads as, when it has one.
    lengths: std::sync::Mutex<HashMap<String, f64>>,
    /// The title tag a file carries instead of its file name; empty for
    /// none.
    titles: std::sync::Mutex<HashMap<String, String>>,
    /// The sample rate, bit depth and channel count a file reads as, when
    /// it has them.
    streams: std::sync::Mutex<HashMap<String, Stream>>,
    root: PathBuf,
}

/// A file's sample rate, bit depth and channel count.
type Stream = (u32, u8, u32);

impl RetagReader {
    fn retag(&self, relative: &str, album: &str) {
        self.albums
            .lock()
            .expect("albums")
            .insert(relative.to_owned(), album.to_owned());
    }
}

impl TagReader for RetagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let name = path.file_name().expect("name").to_string_lossy();
        let relative = path
            .strip_prefix(&self.root)
            .expect("under the root")
            .to_string_lossy()
            .into_owned();
        let due = {
            let mut meanwhile = self.meanwhile.lock().expect("meanwhile");
            match meanwhile.as_ref() {
                Some((at, _)) if *at == name => meanwhile.take(),
                _ => None,
            }
        };
        if let Some((_, act)) = due {
            act();
        }
        let mut read = ScannedTags::default();
        read.tag.title = self
            .titles
            .lock()
            .expect("titles")
            .get(&relative)
            .cloned()
            .unwrap_or_else(|| name.into_owned());
        read.tag.artist = "Artist".to_owned();
        read.tag.album_artist = Some("Artist".to_owned());
        read.tag.album = self.albums.lock().expect("albums")[&relative].clone();
        read.header.duration_seconds = self
            .lengths
            .lock()
            .expect("lengths")
            .get(&relative)
            .copied();
        if let Some((rate, depth, channels)) = self
            .streams
            .lock()
            .expect("streams")
            .get(&relative)
            .copied()
        {
            read.header.sample_rate = Some(rate);
            read.header.bit_depth = Some(depth);
            read.header.channels = Some(channels);
        }
        if let Some(ids) = self.ids.lock().expect("ids").get(&relative) {
            let some = |value: &str| (!value.is_empty()).then(|| value.to_owned());
            read.tag.musicbrainz_release_id = some(&ids.release);
            read.tag.musicbrainz_release_track_id = some(&ids.release_track);
            read.tag.musicbrainz_recording_id = some(&ids.recording);
            read.tag.disc_number = ids.disc;
            read.tag.track_number = ids.track;
        }
        Ok(read)
    }
}

/// The last file of a 300-file album, read in the second window's last
/// batch: what runs before it runs after the first window committed.
const LAST_FILE: &str = "t299.flac";

struct RegroupRig {
    scratch: ScratchDir,
    root: PathBuf,
    reader: Arc<RetagReader>,
    coordinator: Arc<LibraryScanCoordinator<RetagReader>>,
    rewrites: usize,
    /// Album titles no other brief uses, so each brief's album ids (and
    /// the log lines naming them) are its own.
    before: String,
    after: String,
}

impl RegroupRig {
    fn new(name: &str) -> Self {
        let scratch = ScratchDir::new(name);
        let root = scratch.join("music");
        std::fs::create_dir_all(&root).expect("root");
        let db = scratch.join("app.db");
        migrated_db(&db);
        let store = Arc::new(SqliteScanStore::new(
            droppedneedle_kernel::db::Db::open_for_tests(&db).expect("database opens"),
            Arc::new(SystemClock),
        ));
        let reader = Arc::new(RetagReader {
            albums: std::sync::Mutex::default(),
            ids: std::sync::Mutex::default(),
            meanwhile: std::sync::Mutex::default(),
            lengths: std::sync::Mutex::default(),
            titles: std::sync::Mutex::default(),
            streams: std::sync::Mutex::default(),
            root: root.clone(),
        });
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
            store,
            BlockingPool::new(4),
            Arc::clone(&reader),
            Arc::new(StaticResolver::new(registry)),
            Arc::new(SystemClock),
        )
        .with_wakeups(WorkWakeups::new());
        Self {
            scratch,
            root,
            reader,
            coordinator: Arc::new(coordinator),
            rewrites: 0,
            before: format!("{name} before"),
            after: format!("{name} after"),
        }
    }

    fn db(&self) -> PathBuf {
        self.scratch.join("app.db")
    }

    /// Write (or rewrite, so the scan sees a changed file) every file and
    /// tag it with the album.
    fn tag(&mut self, files: &[String], album: &str) {
        self.rewrites += 1;
        for relative in files {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, vec![b'x'; self.rewrites]).expect("plant");
            self.reader.retag(relative, album);
        }
    }

    /// Run `act` just before the file named `at` is read.
    fn meanwhile(&self, at: &str, act: impl FnOnce() + Send + 'static) {
        *self.reader.meanwhile.lock().expect("meanwhile") = Some((at.to_owned(), Box::new(act)));
    }

    /// Run the statements between the two windows.
    fn meanwhile_sql(&self, sql: String) {
        let store = Arc::clone(self.coordinator.store());
        self.meanwhile(LAST_FILE, move || {
            store.db().run_sql(&sql).expect("mid-scan statement runs");
        });
    }

    async fn scan(&self) {
        assert_eq!(
            self.scan_to_its_end().await.state.scan_state(),
            ScanState::Completed
        );
    }

    async fn scan_to_its_end(&self) -> ScanRun {
        self.coordinator
            .request_run(&request(&self.root))
            .expect("requested");
        let paths = HashMap::from([("music".to_owned(), self.root.clone())]);
        self.coordinator.run_once(&paths).await.expect("scan runs")
    }

    fn album_of(&self, relative: &str) -> String {
        let store = self.coordinator.store();
        let track = store
            .track_at("music", relative)
            .unwrap()
            .expect("track indexed");
        store.album_for_track(&track).unwrap().expect("album")
    }

    fn accept_match(&self, album: &str) {
        self.run(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', 'rg-1', 'rel-1', 'manual', 0);"
        ));
    }

    fn run(&self, sql: &str) {
        self.coordinator
            .store()
            .db()
            .run_sql(sql)
            .expect("statement runs");
    }

    fn count(&self, sql: &str) -> i64 {
        self.coordinator
            .store()
            .db()
            .query_i64(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// Plays, favorites and playlist entries that name the album (a play
    /// only when its track is on that album too), and how many exist.
    fn listener_rows(&self, album: &str) -> (i64, i64) {
        let naming = self.count(&format!(
            "SELECT (SELECT COUNT(*) FROM library_play_history h JOIN local_tracks t \
             ON t.id = h.local_track_id AND t.local_album_id = h.local_album_id \
             WHERE h.local_album_id = '{album}') \
             + (SELECT COUNT(*) FROM library_user_favorites WHERE item_id = '{album}') \
             + (SELECT COUNT(*) FROM playlist_tracks WHERE album_id = '{album}')"
        ));
        let all = self.count(
            "SELECT (SELECT COUNT(*) FROM library_play_history) \
             + (SELECT COUNT(*) FROM library_user_favorites) \
             + (SELECT COUNT(*) FROM playlist_tracks)",
        );
        (naming, all)
    }
}

/// The album the scan's first window moved the retagged tracks to, as a
/// SQL expression: no row until the first track has left `album`.
fn new_row(album: &str) -> String {
    format!(
        "(SELECT local_album_id FROM local_tracks WHERE relative_path = 'a/t000.flac' \
         AND local_album_id != '{album}')"
    )
}

/// What a listener does with a track of the retagged album once the scan
/// has moved it: plays it (filed, as playback files it, under the album
/// the track is on right then), favorites that album and adds the track
/// to a playlist. Nothing is written unless the track has left `album`.
fn listener_acts(album: &str) -> String {
    let moved = format!(
        "FROM local_tracks WHERE relative_path = 'a/t000.flac' AND local_album_id != '{album}'"
    );
    format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) SELECT 'play-1', 'listener', id, local_album_id, \
         'Song', 'Artist', '2026-01-01T00:00:00Z' {moved}; \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         SELECT 'listener', 'album', local_album_id, 0 {moved}; \
         INSERT INTO playlists (id, name, created_at, updated_at) VALUES ('list', 'List', '', ''); \
         INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
         album_name, album_id, track_source_id, source_type, created_at) \
         SELECT 'entry', 'list', 0, 'Song', 'Artist', 'After', local_album_id, id, 'local', '' \
         {moved};"
    )
}

/// A scanned album of 300 files, big enough that a retag of all of it
/// commits in two windows. Returns the rig, the files and the album.
async fn big_album(name: &str) -> (RegroupRig, Vec<String>, String) {
    let mut rig = RegroupRig::new(name);
    let files: Vec<String> = (0..300).map(|n| format!("a/t{n:03}.flac")).collect();
    let before = rig.before.clone();
    rig.tag(&files, &before);
    rig.scan().await;
    let album = rig.album_of("a/t000.flac");
    (rig, files, album)
}

/// An album with an accepted match, retagged whole across two windows.
/// `meanwhile` runs between the two.
async fn retag_across_two_windows(
    name: &str,
    meanwhile: impl Fn(&str) -> String,
) -> (RegroupRig, String) {
    let (mut rig, files, album) = big_album(name).await;
    rig.accept_match(&album);

    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.meanwhile_sql(meanwhile(&album));
    rig.scan().await;
    (rig, album)
}

/// Warnings about retagged albums, from every thread: the scan commits on
/// a blocking thread, which a per-thread subscriber would not see. Call
/// before the scan whose warning the brief asserts.
fn retag_warnings() -> &'static std::sync::Mutex<Vec<String>> {
    static LINES: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    struct Capture;
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let line = String::from_utf8_lossy(buf);
            if line.contains("retagged album")
                || line.contains("tracks not continued")
                || line.contains("not placed on the album's edition")
                || line.contains("could not be merged")
            {
                retag_warnings()
                    .lock()
                    .expect("lines")
                    .push(line.into_owned());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(|| Capture)
            .with_ansi(false)
            .with_max_level(tracing::level_filters::LevelFilter::WARN)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("no other global subscriber");
    });
    LINES.get_or_init(Default::default)
}

/// How many warnings that both rows were kept name these two albums.
fn warnings_naming(album: &str, new_row: &str) -> usize {
    retag_warnings()
        .lock()
        .expect("lines")
        .iter()
        .filter(|line| line.contains("both kept") && line.contains(album) && line.contains(new_row))
        .count()
}

fn warned_both_kept(album: &str, new_row: &str) -> bool {
    warnings_naming(album, new_row) > 0
}

/// A retagged album keeps its row and its accepted match when someone
/// plays, favorites and playlists it while the scan is half way through
/// it, and what they made lands on the kept row.
#[tokio::test]
async fn retagged_album_keeps_its_match_when_played_mid_scan() {
    let (rig, album) = retag_across_two_windows("scan-regroup-played", listener_acts).await;

    assert_eq!(rig.album_of("a/t000.flac"), album);
    assert_eq!(rig.album_of("a/t299.flac"), album);
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_albums a JOIN local_album_external_identities i \
             ON i.local_album_id = a.id WHERE a.id = '{album}' AND a.title = '{}'",
            rig.after
        )),
        1,
        "the kept row has the new name and the match"
    );
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM local_albums"),
        1,
        "the new row is gone"
    );
    assert_eq!(
        rig.listener_rows(&album),
        (3, 3),
        "the play, the favorite and the playlist entry name the kept row"
    );
}

/// A management preview someone made of the new row while the scan was
/// half way through the album names that row and its revisions. The scan
/// does not rewrite it: both rows stay, every play and favorite stays
/// where it was, and the pair is recorded and logged, once a reason.
#[tokio::test]
async fn retag_with_a_plan_on_the_new_row_is_left_apart_and_logged() {
    retag_warnings();
    let (rig, album) = retag_across_two_windows("scan-regroup-planned", |album| {
        let new_row = new_row(album);
        format!(
            "{} INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
             VALUES ('plan', 'library_management', 'ready', 0, 0); \
             INSERT INTO library_management_job_snapshots (job_id, mode, origin, phase, \
             selection_json, profile_revision, settings_revision, naming_revision, \
             policy_revision, catalog_revision, profile_snapshot_json, created_at, updated_at) \
             VALUES ('plan', 'preview', 'manual', 'ready', '{{}}', 'p', 's', 'n', 'rev-1', 0, \
             '{{}}', 0, 0); \
             INSERT INTO library_management_plan_items (job_id, ordinal, bundle_ordinal, \
             local_album_id, expected_album_revision, expected_catalog_revision, \
             expected_policy_revision, expected_profile_revision, expected_root_id, \
             expected_relative_path, expected_stat_revision, expected_tag_revision, \
             expected_file_fingerprint, source_path_identity, desired_document_json, \
             desired_document_hash, eligibility, reason_code, created_at) \
             SELECT 'plan', 0, 0, {new_row}, 1, 0, 'rev-1', 'p', 'music', 'a/t000.flac', 's', \
             't', 'f', 'i', '{{}}', '{}', 'blocked', 'IDENTITY_NOT_ACCEPTED', 0 \
             WHERE {new_row} IS NOT NULL;",
            listener_acts(album),
            "0".repeat(64),
        )
    })
    .await;

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album, "the retag was not followed");
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_management_plan_items \
             WHERE local_album_id = '{new_row}' AND expected_album_revision = 1"
        )),
        1,
        "the plan item is as it was made"
    );
    assert_eq!(
        rig.listener_rows(&new_row),
        (3, 3),
        "the play, the favorite and the playlist entry still name the new row"
    );
    assert!(warned_both_kept(&album, &new_row), "the log names both");
    // The end of the scan tries the join that keeps the new row instead,
    // and the preview still open there refuses it as live work.
    assert_eq!(retag_pairs(&rig), open_pair(&album, &new_row, "live_work"));

    let warned = warnings_naming(&album, &new_row);
    rig.scan().await;
    assert_eq!(retag_pairs(&rig).len(), 1, "a later scan records it once");
    assert_eq!(
        warnings_naming(&album, &new_row),
        warned,
        "and says it once"
    );
}

/// Catalog cleanup retires an empty album row into the one live album of
/// the same name. Half way through a retagged album the scan's new row
/// looks like that live album; retiring a row into it ties it down, and
/// the retag can then not be followed. Cleanup leaves it until the scan
/// has ended.
#[tokio::test]
async fn cleanup_leaves_a_half_retagged_album_alone_mid_scan() {
    let (mut rig, files, album) = big_album("scan-regroup-cleanup").await;
    rig.accept_match(&album);
    let after = rig.after.clone();
    rig.tag(&files, &after);
    let db = rig.db();
    let new_row = new_row(&album);
    let retired = Arc::new(std::sync::Mutex::new(None));
    let cell = Arc::clone(&retired);
    rig.meanwhile(LAST_FILE, move || {
        let mut conn =
            droppedneedle_kernel::db::open_connection(&db).expect("side connection opens");
        conn.execute_batch(&format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
             created_at, updated_at) SELECT 'shell', root_id, 'shell', title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, 'automatic', 0, 0 \
             FROM local_albums WHERE id = {new_row}; \
             INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
             VALUES ('cleanup', 'repair', 'running', 0, 0);"
        ))
        .expect("an empty row of the same name");
        let tx = conn.transaction().expect("transaction");
        let changed = hygiene::clean_album(&tx, "shell", "cleanup", 0.0).expect("cleanup runs");
        tx.commit().expect("commit");
        *cell.lock().expect("cell") = Some(changed.is_some());
    });
    rig.scan().await;

    assert_eq!(
        *retired.lock().expect("cell"),
        Some(false),
        "the empty row waits for the scan to end"
    );
    assert_eq!(rig.album_of("a/t000.flac"), album);
    assert_eq!(rig.album_of("a/t299.flac"), album);
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_album_external_identities WHERE local_album_id = '{album}'"
        )),
        1,
        "the retagged album keeps its row and its match"
    );
}

/// Catalog cleanup folds the stray half of an album a v2 import split in
/// two into the album its files' tags name, dropping the v2 choice. The
/// fold keeps that choice, so undoing it brings the album, its files,
/// their v2 matches, its "leave unmanaged" and its takeover mark back. An
/// undo never replaces a match a person made since on a moved file, the
/// cleanup never folds the album again, and an undo runs once. The fold's
/// history entry offers Undo only while the undo would apply, and a
/// curator's undo answers why it cannot.
#[tokio::test]
async fn a_folded_v2_split_comes_back_on_undo() {
    use droppedneedle_library::http::history::album_history_view;
    use droppedneedle_library::identity::FOLD_REASON;
    use droppedneedle_library::operations::models::OperationError;

    const SPLIT_GROUP: &str = "1d1b2a6e-8e1a-4c4f-9b7e-0f4f8c3b2a10";
    const TAGGED_GROUP: &str = "2e2c3b7f-9f2b-4d5a-8c8f-1a5a9d4c3b21";
    let dir = ScratchDir::new("fold-undo");
    std::fs::create_dir_all(dir.join("music")).expect("music folder");
    let library = bundle(&dir).await;
    let ops = crate::common::library::operations(&library);
    let path = dir.join("app.db");
    let mut conn = droppedneedle_kernel::db::open_connection(&path).expect("connection opens");
    let mut seed = String::from(
        "INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
         VALUES ('cleanup', 'repair', 'running', 0, 0); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('artist', 'artist', 'artist', 'person', 0, 0); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
         created_at, updated_at) VALUES ('split', 'root-1', 'split', 'album', 'album', \
         'artist', 'artist', 'artist', 'legacy_import', 0, 0), ('kept', 'root-1', 'kept', \
         'album', 'album', 'artist', 'artist', 'artist', 'automatic', 0, 0);",
    );
    for (track, album, number) in [("s1", "split", 3), ("s2", "split", 4), ("k1", "kept", 1)] {
        seed.push_str(&format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, tag_revision, title, \
             title_folded, artist_name, album_title, album_title_folded, tag_album_title, \
             tag_album_artist_name, embedded_release_group_mbid, disc_number, track_number, \
             file_format, ingest_source, imported_at, membership_source) \
             VALUES ('{track}', '{album}', 'root-1', '/music/a/{track}.flac', \
             'a/{track}.flac', '{track}', 1, 1, 'stat', 'tag', '{track}', '{track}', \
             'artist', 'album', 'album', 'album', 'artist', '{TAGGED_GROUP}', 1, {number}, \
             'flac', 'scan', 0, 'automatic');"
        ));
    }
    seed.push_str(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
         VALUES ('split', 'musicbrainz', '{SPLIT_GROUP}', 'release-split', 'legacy_import', \
         5, 1), ('kept', 'musicbrainz', '{TAGGED_GROUP}', 'release-kept', 'automatic', 5, 1); \
         INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at, \
         row_revision) VALUES \
         ('s1', 'musicbrainz', 'rec-1', 'release-split', 'rt-1', 'legacy_import', 5, 1), \
         ('s2', 'musicbrainz', 'rec-2', 'release-split', NULL, 'legacy_import', 5, 1); \
         INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) VALUES ('split', 'administrator_choice', \
         'curator', 3); \
         INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('split', 'files_changed', 4);"
    ));
    conn.execute_batch(&seed).expect("the split album seeds");
    let rows = |conn: &rusqlite::Connection| -> Vec<String> {
        let mut statement = conn
            .prepare(
                "SELECT 'album ' || a.id || ' ' || COALESCE(a.retired_into_album_id, '-') \
                 || ' ' || COALESCE(i.release_group_mbid, '-') || ' ' \
                 || COALESCE(i.release_mbid, '-') || ' ' || COALESCE(i.decision_source, '-') \
                 FROM local_albums a LEFT JOIN local_album_external_identities i \
                 ON i.local_album_id = a.id \
                 UNION ALL SELECT 'track ' || t.id || ' ' || t.local_album_id || ' ' \
                 || COALESCE(i.release_mbid, '-') || ' ' || COALESCE(i.release_track_mbid, '-') \
                 || ' ' || COALESCE(i.decision_source, '-') FROM local_tracks t \
                 LEFT JOIN local_track_external_identities i ON i.local_track_id = t.id \
                 UNION ALL SELECT 'alias ' || alias || ' ' || local_album_id \
                 FROM local_album_aliases \
                 UNION ALL SELECT 'exclusion ' || reason || ' ' || excluded_by_user_id \
                 FROM library_management_exclusions WHERE local_album_id = 'split' \
                 UNION ALL SELECT 'takeover ' || cause FROM library_album_takeovers \
                 WHERE local_album_id = 'split' ORDER BY 1",
            )
            .expect("query prepares");
        statement
            .query_map([], |row| row.get(0))
            .expect("query runs")
            .collect::<rusqlite::Result<_>>()
            .expect("rows read")
    };
    let before = rows(&conn);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs_f64();
    let undo_ids = || -> Vec<Option<String>> {
        let history = album_history_view(&ops, "kept", "curator").expect("history reads");
        history
            .items
            .into_iter()
            .map(|entry| entry.undo_id)
            .collect()
    };
    let tx = conn.transaction().expect("transaction");
    let changed = hygiene::clean_album(&tx, "split", "cleanup", now).expect("cleanup runs");
    tx.commit().expect("commit");
    assert!(changed.is_some(), "the split half is folded");
    let folded = rows(&conn);
    assert!(
        folded.contains(&"album split kept - - -".to_owned()),
        "{folded:?}"
    );
    let action: String = conn
        .query_row(
            "SELECT id FROM library_catalog_actions WHERE reason_code = ?1",
            [FOLD_REASON],
            |row| row.get(0),
        )
        .expect("the fold is recorded");
    assert_eq!(undo_ids(), [Some(action.clone())], "the entry offers Undo");
    let undo = |fold: &str| match ops.undo_fold(fold, "curator") {
        Ok(()) => "restored".to_owned(),
        Err(OperationError::NotFound(reason) | OperationError::Conflict(reason)) => {
            reason.code.to_owned()
        }
        Err(other) => panic!("undo failed: {other}"),
    };
    assert_eq!(undo("no-such-fold"), "FOLD_NOT_FOUND");

    let persons = "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at) \
         VALUES ('s1', 'musicbrainz', 'rec-1', 'release-kept', 'rt-k3', 'manual', 15)";
    conn.execute(persons, [])
        .expect("a person places s1 on the kept album");
    let chosen = rows(&conn);
    assert_eq!(undo_ids(), [None], "no Undo while it would not apply");
    assert_eq!(
        undo(&action),
        "STALE_REVISION",
        "a person's later match stays"
    );
    assert_eq!(rows(&conn), chosen, "a refused undo changes nothing");
    conn.execute(
        "DELETE FROM local_track_external_identities WHERE local_track_id = 's1'",
        [],
    )
    .expect("the person's match goes");

    assert_eq!(undo(&action), "restored");
    assert_eq!(
        rows(&conn),
        before,
        "the album, its v2 matches, its exclusion and its takeover mark are back"
    );
    let tx = conn.transaction().expect("transaction");
    let again = hygiene::clean_album(&tx, "split", "cleanup", now).expect("cleanup runs");
    tx.commit().expect("commit");
    assert_eq!(again, None, "an undone fold stays undone");
    assert_eq!(undo_ids(), [None], "the entry stays, without Undo");
    assert_eq!(undo(&action), "STALE_REVISION", "an undo runs once");
}

/// The first file of the second window of a 300-file album.
const SECOND_WINDOW: &str = "t256.flac";

/// Counts the identification jobs waiting for an album. `album` is a SQL
/// expression.
fn queued_jobs_sql(album: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM library_identify_jobs WHERE state = 'queued' \
         AND local_album_id = {album}"
    )
}

impl RegroupRig {
    /// Identification jobs waiting for the album.
    fn queued_jobs(&self, album: &str) -> i64 {
        self.count(&queued_jobs_sql(&format!("'{album}'")))
    }
}

/// Half a retagged album is not offered for identification: a job on it
/// would judge half an album and could match it, parting it from the row
/// that holds its match. Here a job, had one been queued, would have
/// matched the new row between the two windows. None was, so the album
/// keeps its row and its match and is offered once, whole.
#[tokio::test]
async fn half_retagged_album_is_not_identified_before_the_retag_lands() {
    let (rig, album) = retag_across_two_windows("scan-regroup-held-offer", |album| {
        let new_row = new_row(album);
        format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             SELECT {new_row}, 'musicbrainz', 'rg-half', 'rel-half', 'automatic', 0 \
             WHERE ({}) > 0;",
            queued_jobs_sql(&new_row)
        )
    })
    .await;

    assert_eq!(rig.album_of("a/t000.flac"), album);
    assert_eq!(rig.album_of("a/t299.flac"), album);
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1);
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_album_external_identities WHERE local_album_id = '{album}' \
             AND release_mbid = 'rel-1' AND decision_source = 'manual'"
        )),
        1,
        "the match a person chose is the album's match"
    );
    assert_eq!(rig.queued_jobs(&album), 1, "offered once, whole");
}

/// Jobs waiting for the new row at the moment `at` is read, counted into
/// the returned cell.
fn count_new_row_jobs_at(
    rig: &RegroupRig,
    album: &str,
    at: &str,
    then: impl FnOnce() + Send + 'static,
) -> Arc<std::sync::Mutex<Option<i64>>> {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let store = Arc::clone(rig.coordinator.store());
    let sql = queued_jobs_sql(&new_row(album));
    let cell = Arc::clone(&seen);
    rig.meanwhile(at, move || {
        *cell.lock().expect("cell") = Some(store.db().query_i64(&sql).expect("count"));
        then();
    });
    seen
}

/// A scan stopped half way through a retagged album leaves the album in
/// two rows for good. The new one is held back while the scan runs and
/// offered for identification when the scan ends.
#[tokio::test]
async fn stopped_scan_offers_the_half_retagged_album() {
    let (mut rig, files, album) = big_album("scan-regroup-stopped").await;
    let after = rig.after.clone();
    rig.tag(&files, &after);
    let coordinator = Arc::clone(&rig.coordinator);
    let waiting = count_new_row_jobs_at(&rig, &album, SECOND_WINDOW, move || {
        let run = coordinator
            .store()
            .list_current()
            .unwrap()
            .pop()
            .expect("the scan is running");
        coordinator
            .control(&run.id, Event::Stop, run.row_revision)
            .expect("stop is taken");
    });
    let run = rig.scan_to_its_end().await;
    assert_eq!(run.state.scan_state(), ScanState::Cancelled);

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album);
    assert_eq!(rig.album_of("a/t299.flac"), album, "the retag is half done");
    assert_eq!(
        *waiting.lock().expect("cell"),
        Some(0),
        "not offered while the scan was on it"
    );
    assert_eq!(rig.queued_jobs(&new_row), 1, "offered when the scan ended");
}

/// When the rest of the old album goes somewhere else, the old album is
/// empty but nothing follows: the tracks went two ways. The album held
/// back since the first window is offered in the window that settled it.
#[tokio::test]
async fn album_split_two_ways_offers_the_held_half() {
    let (mut rig, files, album) = big_album("scan-regroup-two-ways").await;
    let after = rig.after.clone();
    rig.tag(&files[..256], &after);
    rig.tag(&files[256..], "scan-regroup-two-ways third");
    let waiting = count_new_row_jobs_at(&rig, &album, LAST_FILE, || {});
    rig.scan().await;

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album);
    assert_ne!(rig.album_of("a/t299.flac"), new_row);
    assert_eq!(
        *waiting.lock().expect("cell"),
        Some(0),
        "not offered while the old album still held tracks"
    );
    assert_eq!(rig.queued_jobs(&new_row), 1, "offered once it emptied");
}

/// A match written on an album, as SQL. `album` is a SQL expression.
fn match_sql(album: &str, source: &str, release: &str) -> String {
    format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
         SELECT {album}, 'musicbrainz', 'rg-' || '{release}', '{release}', '{source}', 0, 4 \
         WHERE {album} IS NOT NULL;"
    )
}

/// The album's match: its release and who decided it.
fn match_of(rig: &RegroupRig, album: &str) -> Option<(String, String)> {
    use rusqlite::OptionalExtension as _;
    droppedneedle_kernel::db::open_connection(&rig.db())
        .expect("side connection opens")
        .query_row(
            "SELECT release_mbid, decision_source FROM local_album_external_identities \
             WHERE local_album_id = ?1",
            [album],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .expect("match reads")
}

fn matched(release: &str, source: &str) -> Option<(String, String)> {
    Some((release.to_owned(), source.to_owned()))
}

/// Run the identification job waiting for the album, as the worker
/// would, and return its verdict.
async fn identify_now(rig: &RegroupRig, album: &str) -> String {
    let store = Arc::new(SqliteIdentifyStore::new(
        droppedneedle_kernel::db::Db::open_for_tests(&rig.db()).expect("identify store opens"),
        Arc::new(SystemClock),
    ));
    let service = IdentifyService::new(IdentifyDeps {
        store: store.clone(),
        providers: Arc::new(FakeProviders::with_recall(RecallResult::default())),
    });
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
        + 60_000;
    let job = store
        .claim(now_ms, 60_000)
        .unwrap()
        .expect("a job is waiting");
    assert_eq!(job.local_album_id, album, "the waiting job is the album's");
    service
        .run_claimed_job(&job.id, now_ms)
        .await
        .unwrap()
        .expect("the job reports")
        .reason_code
}

/// Retag a 300-file album across two windows. `old` is the match it
/// holds before, as `(source, release)`; `on_new_row` is written between
/// the two windows and takes the new row as a SQL expression.
async fn retag_with_matches(
    name: &str,
    old: Option<(&str, &str)>,
    on_new_row: impl Fn(&str) -> String,
) -> (RegroupRig, String) {
    let (mut rig, files, album) = big_album(name).await;
    if let Some((source, release)) = old {
        rig.run(&match_sql(&format!("'{album}'"), source, release));
    }
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.meanwhile_sql(on_new_row(&new_row(&album)));
    rig.scan().await;
    (rig, album)
}

/// What an identification job leaves beside an automatic match: the
/// "unsure" flag, an undo for the edition, a place in the automatic
/// management queue and a review waiting for a person.
fn automatic_leftovers(album: &str, tag: &str) -> String {
    format!(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, updated_at) \
         SELECT {album}, 'unconfirmed', '{tag}', 0 WHERE {album} IS NOT NULL; \
         INSERT INTO library_automatic_edition_undo (id, local_album_id, \
         expected_post_album_revision, expected_post_identity_revision, reason_code, created_at) \
         SELECT 'undo-{tag}', {album}, 1, 4, 'AUTOMATIC_EXACT_EDITION', 0 \
         WHERE {album} IS NOT NULL; \
         INSERT INTO library_management_automatic_queue (local_album_id, queued_at, not_before) \
         SELECT {album}, 0, 0 WHERE {album} IS NOT NULL; \
         INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_ms, updated_ms) \
         SELECT 'pending-{tag}', {album}, 'R', '[]', 'pending', 0, 0 WHERE {album} IS NOT NULL;"
    )
}

/// Rows left on `album` in the tables that hang on a match, in the order:
/// flag, automatic undo, choice undo, remap queue, management queue,
/// reviews.
fn hanging_rows(rig: &RegroupRig, album: &str) -> [i64; 6] {
    [
        "library_album_match_state",
        "library_automatic_edition_undo",
        "library_edition_choice_undo",
        "library_edition_remap_queue",
        "library_management_automatic_queue",
        "library_identify_reviews",
    ]
    .map(|table| {
        rig.count(&format!(
            "SELECT COUNT(*) FROM {table} WHERE local_album_id = '{album}'"
        ))
    })
}

/// An automatic match that landed on the new row mid-scan (a job already
/// running when the retag began) gives way to the match a person chose
/// for the album. Everything the job left with it goes too, and a later
/// identification pass leaves the person's match as it is.
#[tokio::test]
async fn automatic_match_made_mid_scan_yields_to_a_chosen_one() {
    let (rig, album) = retag_with_matches(
        "scan-regroup-auto-vs-chosen",
        Some(("manual", "rel-1")),
        |new_row| {
            format!(
                "{} {}",
                match_sql(new_row, "automatic", "rel-half"),
                automatic_leftovers(new_row, "half")
            )
        },
    )
    .await;

    assert_eq!(rig.album_of("a/t000.flac"), album);
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
    assert_eq!(
        hanging_rows(&rig, &album),
        [0, 0, 0, 1, 1, 0],
        "only a fresh look by automatic management, and the files waiting to be \
         placed on the chosen edition, are left"
    );
    assert_eq!(placements_queued(&rig, &album), vec!["rel-1".to_owned()]);
    assert_eq!(identify_now(&rig, &album).await, "CHOSEN_EDITION_KEPT");
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// An automatic match that landed on the new row mid-scan is the later
/// verdict on the files as they are now tagged. It replaces an automatic
/// match on the old row (or fills an empty one) with its flag, stays
/// revisable, and the whole album is offered again in the same window.
/// Undo rows and proofs made for either earlier match are gone, a review
/// still waiting on half the album is gone, and a review a person closed
/// is kept.
#[tokio::test]
async fn automatic_match_made_mid_scan_replaces_a_revisable_one() {
    for (name, old) in [
        ("scan-regroup-auto-vs-auto", Some(("automatic", "rel-1"))),
        ("scan-regroup-auto-vs-none", None),
    ] {
        let (mut rig, files, album) = big_album(name).await;
        if let Some((source, release)) = old {
            let quoted = format!("'{album}'");
            rig.run(&format!(
                "{} {} DELETE FROM library_identify_reviews; \
                 DELETE FROM library_management_automatic_queue;",
                match_sql(&quoted, source, release),
                automatic_leftovers(&quoted, "old")
            ));
        }
        let after = rig.after.clone();
        rig.tag(&files, &after);
        let new_row = new_row(&album);
        rig.meanwhile_sql(format!(
            "{} {} INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
             candidates_json, state, created_ms, updated_ms) \
             SELECT 'closed', {new_row}, 'R', '[]', 'dismissed', 0, 0;",
            match_sql(&new_row, "automatic", "rel-half"),
            automatic_leftovers(&new_row, "half"),
        ));
        rig.scan().await;

        assert_eq!(rig.album_of("a/t000.flac"), album, "{name}");
        assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1, "{name}");
        assert_eq!(
            match_of(&rig, &album),
            matched("rel-half", "automatic"),
            "{name}"
        );
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM local_album_external_identities i \
                 JOIN library_album_match_state s ON s.local_album_id = i.local_album_id \
                 WHERE i.local_album_id = '{album}' AND i.row_revision > 4 \
                 AND s.reason_code = 'half'"
            )),
            1,
            "{name}: the flag came with the match, at a revision neither row has had"
        );
        assert_eq!(hanging_rows(&rig, &album), [1, 0, 0, 0, 1, 1], "{name}");
        assert_eq!(
            rig.count("SELECT COUNT(*) FROM library_identify_reviews WHERE id = 'closed'"),
            1,
            "{name}: the closed review is the one kept"
        );
        assert_eq!(rig.queued_jobs(&album), 1, "{name}: offered whole");
    }
}

/// A match a person chose (or one imported from v2) for the new row
/// mid-scan becomes the album's match when the old row held none or an
/// automatic one. The edition still waiting to be placed on the files is
/// queued for the kept row, the undo made for the new row is dropped, and
/// a later identification pass leaves the match as it is.
#[tokio::test]
async fn chosen_match_made_mid_scan_moves_to_the_kept_row() {
    for (name, old, source) in [
        (
            "scan-regroup-chosen-vs-auto",
            Some(("embedded", "rel-1")),
            "manual",
        ),
        ("scan-regroup-chosen-vs-none", None, "legacy_import"),
    ] {
        let (rig, album) = retag_with_matches(name, old, |new_row| {
            format!(
                "{} INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, \
                 queued_at, origin) SELECT {new_row}, 'rel-new', 0, 'group_pin'; \
                 INSERT INTO library_edition_choice_undo (id, local_album_id, action_id, \
                 expected_identity_revision, created_at) SELECT 'undo', {new_row}, 'a', 4, 0;",
                match_sql(new_row, source, "rel-new")
            )
        })
        .await;

        assert_eq!(rig.album_of("a/t000.flac"), album, "{name}");
        assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1, "{name}");
        assert_eq!(match_of(&rig, &album), matched("rel-new", source), "{name}");
        assert_eq!(hanging_rows(&rig, &album), [0, 0, 0, 1, 1, 0], "{name}");
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM library_edition_remap_queue WHERE local_album_id = '{album}' \
                 AND release_mbid = 'rel-new' AND origin = 'group_pin' AND attempts = 0"
            )),
            1,
            "{name}: the chosen edition is queued for the kept row"
        );
        assert_eq!(
            identify_now(&rig, &album).await,
            "CHOSEN_EDITION_KEPT",
            "{name}"
        );
        assert_eq!(match_of(&rig, &album), matched("rel-new", source), "{name}");
    }
}

/// Every track of the album (a SQL expression) placed on `release`, as an
/// automatic identification leaves them.
fn tracks_on(album: &str, release: &str) -> String {
    format!(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at) \
         SELECT id, 'musicbrainz', 'rec-' || id, '{release}', '{release}-' || id, \
         'automatic', 0 FROM local_tracks WHERE local_album_id = {album};"
    )
}

/// The releases the album's files wait to be placed on with nothing else
/// changed (`place_only` rows, queued by nobody).
fn placements_queued(rig: &RegroupRig, album: &str) -> Vec<String> {
    let conn = droppedneedle_kernel::db::open_connection(&rig.db()).expect("side connection opens");
    let mut statement = conn
        .prepare(
            "SELECT release_mbid FROM library_edition_remap_queue WHERE local_album_id = ?1 \
             AND origin = 'place_only' AND chosen_by_user_id IS NULL",
        )
        .expect("queue reads");
    statement
        .query_map([album], |row| row.get(0))
        .expect("queue rows")
        .collect::<Result<_, _>>()
        .expect("queue rows")
}

/// A person's release that a follow leaves on the kept row while the
/// files sit on another release is queued to have the files placed on it,
/// and nothing else: one `place_only` row naming the release. No row when
/// the files already sit on it, and none for an automatic match.
#[tokio::test]
async fn followed_album_queues_its_files_for_a_chosen_release() {
    for (name, tracks, arriving, queued) in [
        (
            "scan-regroup-place-chosen",
            "rel-a",
            "manual",
            vec!["rel-b"],
        ),
        ("scan-regroup-place-sits", "rel-b", "manual", vec![]),
        ("scan-regroup-place-automatic", "rel-a", "automatic", vec![]),
    ] {
        let (mut rig, files, album) = big_album(name).await;
        let quoted = format!("'{album}'");
        rig.run(&format!(
            "{} {}",
            match_sql(&quoted, "automatic", tracks),
            tracks_on(&quoted, tracks)
        ));
        let after = rig.after.clone();
        rig.tag(&files, &after);
        rig.meanwhile_sql(match_sql(&new_row(&album), arriving, "rel-b"));
        rig.scan().await;

        assert_eq!(rig.album_of("a/t000.flac"), album, "{name}: followed");
        assert_eq!(match_of(&rig, &album), matched("rel-b", arriving), "{name}");
        assert_eq!(placements_queued(&rig, &album), queued, "{name}");
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM library_edition_remap_queue \
                 WHERE local_album_id = '{album}'"
            )),
            queued.len() as i64,
            "{name}: no other row"
        );
    }
}

/// The same release chosen by a person on both rows is one choice: the
/// kept row's match stays exactly as it was and the new row's goes.
#[tokio::test]
async fn same_release_chosen_on_both_rows_is_kept_once() {
    let (rig, album) = retag_with_matches(
        "scan-regroup-chosen-same",
        Some(("manual", "rel-1")),
        |new_row| match_sql(new_row, "legacy_import", "rel-1"),
    )
    .await;

    assert_eq!(rig.album_of("a/t000.flac"), album);
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
    assert_eq!(
        rig.count(&format!(
            "SELECT row_revision FROM local_album_external_identities \
             WHERE local_album_id = '{album}'"
        )),
        4,
        "the kept match was not rewritten"
    );
    assert_eq!(identify_now(&rig, &album).await, "CHOSEN_EDITION_KEPT");
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// Different releases chosen by people on the two rows are not the scan's
/// to pick between: both rows stay with their own match, nothing a
/// listener made moves, and the pair is recorded and logged.
#[tokio::test]
async fn different_releases_chosen_on_the_two_rows_stay_apart() {
    retag_warnings();
    let (mut rig, files, album) = big_album("scan-regroup-chosen-differ").await;
    rig.run(&match_sql(&format!("'{album}'"), "manual", "rel-1"));
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.meanwhile_sql(format!(
        "{} {}",
        listener_acts(&album),
        match_sql(&new_row(&album), "manual", "rel-2")
    ));
    rig.scan().await;

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album, "the retag was not followed");
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
    assert_eq!(match_of(&rig, &new_row), matched("rel-2", "manual"));
    assert_eq!(
        rig.listener_rows(&new_row),
        (3, 3),
        "the play, the favorite and the playlist entry still name the new row"
    );
    assert!(warned_both_kept(&album, &new_row), "the log names both");
    assert_eq!(
        retag_pairs(&rig),
        open_pair(&album, &new_row, "chosen_matches_differ")
    );
}

/// A person who empties the old album by hand while the scan is half way
/// through its retag leaves nothing for the scan to settle: no window
/// sees the old album lose its last track. The half the scan did move was
/// held back all the same, and is offered when the run ends.
#[tokio::test]
async fn held_album_is_offered_though_the_old_album_was_emptied_by_hand() {
    let (mut rig, files, album) = big_album("scan-regroup-emptied-by-hand").await;
    let after = rig.after.clone();
    let before = rig.before.clone();
    rig.tag(&files[..256], &after);
    rig.tag(&files[256..], &before);
    let new_row_sql = new_row(&album);
    rig.meanwhile_sql(format!(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
         created_at, updated_at) SELECT 'third', root_id, 'third', 'Third', 'third', \
         album_artist_name, album_artist_name_folded, album_artist_id, 'automatic', 0, 0 \
         FROM local_albums WHERE id = '{album}' AND {new_row_sql} IS NOT NULL; \
         UPDATE local_tracks SET local_album_id = 'third', membership_source = 'manual', \
         membership_locked = 1 WHERE local_album_id = '{album}' AND {new_row_sql} IS NOT NULL;"
    ));
    rig.scan().await;

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album);
    assert_eq!(
        rig.album_of("a/t299.flac"),
        "third",
        "the rest was moved by hand"
    );
    assert_eq!(rig.queued_jobs(&new_row), 1, "offered when the scan ended");
}

/// Once a retagged album has been followed, its row is an ordinary album
/// again, scan still running or not: cleanup retires an empty row of the
/// same name into it. It used to pass the row by, and an empty row is
/// only looked at once.
#[tokio::test]
async fn cleanup_does_not_skip_the_survivor_of_a_finished_follow() {
    const OTHER: &str = "scan-regroup-cleanup-survivor other";
    let mut rig = RegroupRig::new("scan-regroup-cleanup-survivor");
    let small: Vec<String> = (0..10).map(|n| format!("0/s{n}.flac")).collect();
    let big: Vec<String> = (0..300).map(|n| format!("a/t{n:03}.flac")).collect();
    let (before, after) = (rig.before.clone(), rig.after.clone());
    rig.tag(&small, &before);
    rig.tag(&big, OTHER);
    rig.scan().await;
    let album = rig.album_of("0/s0.flac");

    // The small album sorts first, so its retag is read and followed in
    // the first window; the other album's files only keep the scan going.
    rig.tag(&small, &after);
    rig.tag(&big, OTHER);
    let db = rig.db();
    let survivor = album.clone();
    let retired = Arc::new(std::sync::Mutex::new(None));
    let cell = Arc::clone(&retired);
    rig.meanwhile(LAST_FILE, move || {
        let mut conn =
            droppedneedle_kernel::db::open_connection(&db).expect("side connection opens");
        conn.execute_batch(&format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
             created_at, updated_at) SELECT 'shell', root_id, 'shell', title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, 'automatic', 0, 0 \
             FROM local_albums WHERE id = '{survivor}' AND title = '{after}'; \
             INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
             VALUES ('cleanup', 'repair', 'running', 0, 0);"
        ))
        .expect("an empty row of the followed album's name");
        let tx = conn.transaction().expect("transaction");
        let changed = hygiene::clean_album(&tx, "shell", "cleanup", 0.0).expect("cleanup runs");
        tx.commit().expect("commit");
        *cell.lock().expect("cell") = Some(changed.is_some());
    });
    rig.scan().await;

    assert_eq!(rig.album_of("0/s0.flac"), album, "the retag was followed");
    assert_eq!(
        *retired.lock().expect("cell"),
        Some(true),
        "the empty row was retired into the followed album mid-scan"
    );
}

/// A scan cancelled while it was still waiting is over. Starting the app
/// again does not end it a second time.
#[tokio::test]
async fn run_cancelled_while_queued_is_not_ended_again_at_startup() {
    let rig = RegroupRig::new("scan-cancelled-queued");
    rig.coordinator
        .request_run(&request(&rig.root))
        .expect("requested");
    let run = rig
        .coordinator
        .store()
        .list_current()
        .unwrap()
        .pop()
        .expect("the run is waiting");
    rig.coordinator
        .control(&run.id, Event::Stop, run.row_revision)
        .expect("stop is taken");
    let stamp = "SELECT CAST(terminal_at * 1000 AS INTEGER) + row_revision \
                 FROM library_scan_runs WHERE state = 'cancelled'";
    let ended = rig.count(stamp);

    let later = rig.count("SELECT CAST(terminal_at AS INTEGER) FROM library_scan_runs") + 3600;
    rig.coordinator
        .store()
        .recover(&droppedneedle_library::scan::RunStop::never(), later as f64)
        .unwrap();
    assert_eq!(
        rig.count(stamp),
        ended,
        "the run row is as the cancel left it"
    );
}

/// A scanned album of `count` files in one folder, small enough to be
/// read in one window, with a match a person chose. Returns the rig, the
/// files and the album.
async fn chosen_album(name: &str, count: usize) -> (RegroupRig, Vec<String>, String) {
    let mut rig = RegroupRig::new(name);
    let files: Vec<String> = (0..count).map(|n| format!("a/t{n:03}.flac")).collect();
    let before = rig.before.clone();
    rig.tag(&files, &before);
    rig.scan().await;
    let album = rig.album_of("a/t000.flac");
    rig.accept_match(&album);
    (rig, files, album)
}

impl RegroupRig {
    /// Delete the files and scan, so their tracks have been missing since
    /// before the next scan begins.
    async fn lose(&self, files: &[String]) {
        for relative in files {
            std::fs::remove_file(self.root.join(relative)).expect("file removed");
        }
        self.scan().await;
    }

    /// Track rows on the album, whatever their state.
    fn rows_on(&self, album: &str) -> i64 {
        self.count(&format!(
            "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = '{album}'"
        ))
    }
}

/// Files deleted long ago leave their track rows on the album for good.
/// They do not keep the album from being followed when the files that are
/// left are retagged together: the album keeps its row and its match, the
/// missing tracks stay on it, and it is offered for identification under
/// its new name.
#[tokio::test]
async fn album_missing_a_few_tracks_is_followed() {
    let (mut rig, files, album) = chosen_album("scan-regroup-few-missing", 12).await;
    rig.lose(&files[9..]).await;
    rig.run("DELETE FROM library_identify_jobs");

    let after = rig.after.clone();
    rig.tag(&files[..9], &after);
    rig.scan().await;

    assert_eq!(rig.album_of("a/t000.flac"), album, "followed");
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1);
    assert_eq!(
        rig.rows_on(&album),
        12,
        "the three missing tracks are on the kept album"
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_albums a JOIN local_album_external_identities i \
             ON i.local_album_id = a.id WHERE a.id = '{album}' AND a.title = '{after}' \
             AND i.release_mbid = 'rel-1' AND i.decision_source = 'manual' \
             AND i.row_revision = 1"
        )),
        1,
        "the kept row has the new name and the very match it had"
    );
    assert_eq!(rig.queued_jobs(&album), 1, "offered again");
}

/// A track a person excluded does not keep its album from being followed
/// either.
#[tokio::test]
async fn excluded_track_does_not_hold_its_album() {
    let (mut rig, files, album) = chosen_album("scan-regroup-excluded", 10).await;
    rig.run(
        "UPDATE local_tracks SET availability = 'excluded', manual_excluded = 1, \
         excluded_at = 0 WHERE relative_path = 'a/t009.flac'",
    );

    let after = rig.after.clone();
    rig.tag(&files[..9], &after);
    rig.scan().await;

    assert_eq!(rig.album_of("a/t000.flac"), album, "followed");
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1);
    assert_eq!(rig.rows_on(&album), 10);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// Files a scan does not see may only be out of reach for a moment (a
/// share that dropped out, files taken away to be re-encoded). In the scan
/// that finds them gone they still hold their album: the files retagged
/// in that same scan do not walk off with the album's row and its match,
/// even when they are most of the album, and nothing is recorded.
#[tokio::test]
async fn files_gone_in_this_very_scan_still_hold_their_album() {
    let (mut rig, files, album) = chosen_album("scan-regroup-unreachable", 15).await;
    for relative in &files[9..] {
        std::fs::remove_file(rig.root.join(relative)).expect("file removed");
    }
    let after = rig.after.clone();
    rig.tag(&files[..9], &after);
    rig.scan().await;

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album, "not followed");
    assert_eq!(rig.rows_on(&album), 6);
    assert_eq!(rig.rows_on(&new_row), 9);
    assert_eq!(retag_pairs(&rig), vec![]);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
    assert_eq!(match_of(&rig, &new_row), None);
    assert_eq!(rig.queued_jobs(&new_row), 1, "offered when the scan ended");
}

/// A file that comes back with the tags it had keeps its track row, and
/// is filed by what those tags say: not on the album that was retagged
/// while it was away.
#[tokio::test]
async fn returned_file_keeps_its_track_and_is_filed_by_its_tags() {
    let (mut rig, files, album) = chosen_album("scan-regroup-returned", 12).await;
    let track = rig
        .coordinator
        .store()
        .track_at("music", "a/t011.flac")
        .unwrap()
        .expect("track indexed");
    rig.lose(&files[9..]).await;
    let (before, after) = (rig.before.clone(), rig.after.clone());
    rig.tag(&files[..9], &after);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t000.flac"), album, "followed");

    rig.tag(&files[11..], &before);
    rig.scan().await;

    assert_eq!(
        rig.coordinator
            .store()
            .track_at("music", "a/t011.flac")
            .unwrap(),
        Some(track),
        "the same track"
    );
    assert_ne!(rig.album_of("a/t011.flac"), album);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// A match carried over from v2 that names only the release group, and a
/// release of that group a person chose for the new row mid-scan, are one
/// album. It is followed, and the match that names the release is kept.
#[tokio::test]
async fn imported_group_match_and_a_chosen_release_of_it_are_one_album() {
    let (mut rig, files, album) = big_album("scan-regroup-imported-group").await;
    rig.run(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at, row_revision) \
         VALUES ('{album}', 'musicbrainz', 'rg-rel-1', 'legacy_import', 0, 4);"
    ));
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.meanwhile_sql(match_sql(&new_row(&album), "manual", "rel-1"));
    rig.scan().await;

    assert_eq!(rig.album_of("a/t000.flac"), album, "followed");
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// The pairs of album rows recorded as looking like one retagged album,
/// as (old row, new row, reason, state).
fn retag_pairs(rig: &RegroupRig) -> Vec<(String, String, String, String)> {
    let conn = droppedneedle_kernel::db::open_connection(&rig.db()).expect("side connection opens");
    let mut stmt = conn
        .prepare("SELECT old_album_id, new_album_id, reason, state FROM library_retag_pairs")
        .expect("pairs read");
    stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })
    .expect("pairs read")
    .collect::<rusqlite::Result<_>>()
    .expect("pairs read")
}

fn open_pair(album: &str, new_row: &str, reason: &str) -> Vec<(String, String, String, String)> {
    vec![(
        album.to_owned(),
        new_row.to_owned(),
        reason.to_owned(),
        "open".to_owned(),
    )]
}

impl RegroupRig {
    /// Move the time the missing tracks went missing `seconds` back.
    fn age_missing(&self, seconds: i64) {
        self.run(&format!(
            "UPDATE local_tracks SET missing_since = missing_since - {seconds} \
             WHERE availability = 'missing'"
        ));
    }

    /// Whether the album waits for automatic management with the stamp of
    /// a scan that followed it across a retag.
    fn stamped(&self, album: &str) -> bool {
        self.count(&format!(
            "SELECT COUNT(*) FROM library_management_automatic_queue \
             WHERE local_album_id = '{album}' AND retag_followed_at IS NOT NULL"
        )) == 1
    }
}

/// A day and a second.
const OVER_A_DAY: i64 = 86_401;

/// What is left of an album that lost most of its tracks is followed when
/// it is retagged, once the tracks left behind have been gone a day: the
/// album keeps its row and its match under the new name, the missing
/// tracks stay on it, and nothing is recorded. A tie is followed too.
#[tokio::test]
async fn mostly_missing_album_is_followed_once_the_rest_is_long_gone() {
    for (name, count, kept) in [
        ("scan-regroup-mostly-missing", 12, 5),
        ("scan-regroup-missing-tie", 2, 1),
    ] {
        let (mut rig, files, album) = chosen_album(name, count).await;
        rig.lose(&files[kept..]).await;
        rig.age_missing(OVER_A_DAY);
        let after = rig.after.clone();
        rig.tag(&files[..kept], &after);
        rig.scan().await;

        assert_eq!(rig.album_of("a/t000.flac"), album, "{name}: followed");
        assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1, "{name}");
        assert_eq!(rig.rows_on(&album), count as i64, "{name}");
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM local_albums a JOIN local_album_external_identities i \
                 ON i.local_album_id = a.id WHERE a.id = '{album}' AND a.title = '{after}' \
                 AND i.release_mbid = 'rel-1' AND i.decision_source = 'manual' \
                 AND i.row_revision = 1"
            )),
            1,
            "{name}: the kept row has the new name and the very match it had"
        );
        assert_eq!(retag_pairs(&rig), vec![], "{name}");
    }
}

/// Tracks gone for less than a day may only be out of reach. When they
/// are half the album or more, the retagged files do not take the album's
/// row and match: nothing is followed and nothing is recorded. Files
/// added with the retag never left the album, so they do not count
/// towards it.
#[tokio::test]
async fn mostly_missing_album_waits_a_day() {
    for (name, added) in [
        ("scan-regroup-missing-recent", 0),
        ("scan-regroup-missing-recent-added", 3),
    ] {
        let (mut rig, files, album) = chosen_album(name, 12).await;
        rig.lose(&files[5..]).await;
        rig.age_missing(3_600);
        let mut retagged = files[..5].to_vec();
        retagged.extend((0..added).map(|n| format!("a/n{n}.flac")));
        let after = rig.after.clone();
        rig.tag(&retagged, &after);
        rig.scan().await;

        let new_row = rig.album_of("a/t000.flac");
        assert_ne!(new_row, album, "{name}: not followed");
        assert_eq!(rig.rows_on(&album), 7, "{name}");
        assert_eq!(rig.rows_on(&new_row), 5 + added, "{name}");
        assert_eq!(retag_pairs(&rig), vec![], "{name}");
        assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"), "{name}");
        assert_eq!(match_of(&rig, &new_row), None, "{name}");
    }
}

/// An album retagged to a new name with a file added in the same go is
/// followed: the kept row takes the added file too, and nothing is
/// recorded.
#[tokio::test]
async fn retag_with_a_file_added_is_followed() {
    let (mut rig, mut files, album) = chosen_album("scan-regroup-file-added", 10).await;
    files.push("a/t010.flac".to_owned());
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.scan().await;

    assert_eq!(rig.album_of("a/t000.flac"), album, "followed");
    assert_eq!(rig.album_of("a/t010.flac"), album);
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_albums"), 1);
    assert_eq!(rig.rows_on(&album), 11);
    assert_eq!(retag_pairs(&rig), vec![]);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// A four-file album, `prepare` run on it (SQL, given the album id), then
/// retagged whole and followed. Returns the rig and the album.
async fn followed_album(name: &str, prepare: impl FnOnce(&str) -> String) -> (RegroupRig, String) {
    let mut rig = RegroupRig::new(name);
    let files: Vec<String> = (0..4).map(|n| format!("a/t{n:03}.flac")).collect();
    let before = rig.before.clone();
    rig.tag(&files, &before);
    rig.scan().await;
    let album = rig.album_of("a/t000.flac");
    rig.run(&prepare(&album));
    rig.run("DELETE FROM library_identify_jobs");

    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t000.flac"), album, "{name}: followed");
    (rig, album)
}

/// A followed album whose edition a person chose, and none of whose files
/// the app has a record of, is stamped in the scan's own write, so that
/// automatic management leaves the hand retag alone. Queueing the album
/// again keeps the stamp.
#[tokio::test]
async fn followed_album_with_a_chosen_edition_is_stamped() {
    let (rig, album) = followed_album("scan-regroup-stamp", |album| {
        match_sql(&format!("'{album}'"), "manual", "rel-1")
    })
    .await;
    assert!(rig.stamped(&album));

    let conn = droppedneedle_kernel::db::open_connection(&rig.db()).expect("side connection opens");
    droppedneedle_library::management::automatic::queue(&conn, &album, 2.0e9)
        .expect("queued again");
    assert!(rig.stamped(&album), "queueing again keeps the stamp");
}

/// No stamp on a followed album whose edition is the app's own guess, one
/// with no match at all (it is identified from its new tags), or one with
/// a file the app has a record of, written by v3 or carried over from v2.
#[tokio::test]
async fn followed_album_is_stamped_only_for_a_chosen_edition_the_app_never_wrote() {
    fn chosen_with_state(album: &str, columns: &str, values: &str) -> String {
        format!(
            "{} INSERT INTO library_track_management_state (local_track_id, {columns}) \
             SELECT id, {values} FROM local_tracks WHERE local_album_id = '{album}' \
             AND relative_path = 'a/t001.flac';",
            match_sql(&format!("'{album}'"), "manual", "rel-1")
        )
    }
    type Prepare = fn(&str) -> String;
    let cases: [(&str, Prepare); 4] = [
        ("scan-regroup-stamp-automatic", |album| {
            match_sql(&format!("'{album}'"), "automatic", "rel-1")
        }),
        ("scan-regroup-stamp-no-match", |_| String::new()),
        ("scan-regroup-stamp-written", |album| {
            chosen_with_state(album, "written_sha256", "'abc'")
        }),
        ("scan-regroup-stamp-v2", |album| {
            chosen_with_state(
                album,
                "managed_path_revision, last_outcome",
                "'v2', 'succeeded'",
            )
        }),
    ];
    for (name, prepare) in cases {
        let (rig, album) = followed_album(name, prepare).await;
        assert!(!rig.stamped(&album), "{name}");
        if name == "scan-regroup-stamp-no-match" {
            assert_eq!(rig.queued_jobs(&album), 1, "offered for identification");
        }
    }
}

/// Files retagged into an album that was already there are a person
/// joining two albums. Nothing is followed and nothing is recorded.
#[tokio::test]
async fn retag_into_an_album_that_was_there_is_not_a_pair() {
    let mut rig = RegroupRig::new("scan-regroup-joined");
    let files: Vec<String> = (0..10).map(|n| format!("a/t{n:03}.flac")).collect();
    let there: Vec<String> = (0..3).map(|n| format!("a/u{n}.flac")).collect();
    let (before, after) = (rig.before.clone(), rig.after.clone());
    rig.tag(&files, &before);
    rig.tag(&there, &after);
    rig.scan().await;
    let album = rig.album_of("a/t000.flac");
    let other = rig.album_of("a/u0.flac");
    assert_ne!(album, other);
    rig.accept_match(&album);

    rig.tag(&files, &after);
    rig.scan().await;

    assert_eq!(rig.album_of("a/t000.flac"), other);
    assert_eq!(retag_pairs(&rig), vec![]);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
    assert_eq!(match_of(&rig, &other), None);
}

/// Two albums retagged into one new album in the same scan are a person
/// joining them too: the new row is this scan's, but the tracks on it
/// that came from the other album are not. Neither album is followed and
/// neither is recorded as a pair.
#[tokio::test]
async fn two_albums_retagged_into_a_new_one_are_not_pairs() {
    let mut rig = RegroupRig::new("scan-regroup-two-into-one");
    let first: Vec<String> = (0..10).map(|n| format!("a/t{n:03}.flac")).collect();
    let second: Vec<String> = (0..10).map(|n| format!("a/u{n}.flac")).collect();
    let (before, after) = (rig.before.clone(), rig.after.clone());
    rig.tag(&first, &before);
    rig.tag(&second, "scan-regroup-two-into-one other");
    rig.scan().await;
    let (one, other) = (rig.album_of("a/t000.flac"), rig.album_of("a/u0.flac"));
    assert_ne!(one, other);
    rig.accept_match(&one);
    rig.accept_match(&other);

    rig.tag(&first, &after);
    rig.tag(&second, &after);
    rig.scan().await;

    let joined = rig.album_of("a/t000.flac");
    assert_eq!(rig.album_of("a/u0.flac"), joined);
    assert!(joined != one && joined != other, "neither was followed");
    assert_eq!(retag_pairs(&rig), vec![]);
    assert_eq!(match_of(&rig, &one), matched("rel-1", "manual"));
    assert_eq!(match_of(&rig, &other), matched("rel-1", "manual"));
}

/// "Leave unmanaged" and a custom edition are stored as a match a person
/// made that names no release. Against a release a person chose for the
/// other row that is a different choice, of the same release group or
/// not: both rows stay as they are and the pair is recorded.
#[tokio::test]
async fn no_release_choice_and_a_chosen_release_stay_apart() {
    let (mut rig, files, album) = big_album("scan-regroup-no-release").await;
    rig.run(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at, row_revision) \
         VALUES ('{album}', 'musicbrainz', 'rg-rel-1', 'manual', 0, 4);"
    ));
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.meanwhile_sql(match_sql(&new_row(&album), "legacy_import", "rel-1"));
    rig.scan().await;

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album, "not followed");
    assert_eq!(
        retag_pairs(&rig),
        open_pair(&album, &new_row, "chosen_matches_differ")
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_album_external_identities \
             WHERE local_album_id = '{album}' AND release_mbid IS NULL \
             AND decision_source = 'manual' AND row_revision = 4"
        )),
        1,
        "the choice on the old row is untouched"
    );
    assert_eq!(match_of(&rig, &new_row), matched("rel-1", "legacy_import"));
}

/// A user for rows that must name a person.
const PERSON: &str = "INSERT OR IGNORE INTO auth_users (id, display_name, role, created_at) \
                      VALUES ('u1', 'Person', 'admin', 'now');";

impl RegroupRig {
    /// One text value read by SQL, `None` for no row or NULL.
    fn text(&self, sql: &str) -> Option<String> {
        use rusqlite::OptionalExtension as _;
        droppedneedle_kernel::db::open_connection(&self.db())
            .expect("side connection opens")
            .query_row(sql, [], |row| row.get::<_, Option<String>>(0))
            .optional()
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
            .flatten()
    }

    /// The album an album was merged into.
    fn retired_into(&self, album: &str) -> Option<String> {
        self.text(&format!(
            "SELECT retired_into_album_id FROM local_albums WHERE id = '{album}'"
        ))
    }

    fn departures(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM library_album_departures")
    }

    /// Stop the next scan just before the file named `at` is read.
    fn stop_before(&self, at: &str) {
        let coordinator = Arc::clone(&self.coordinator);
        self.meanwhile(at, move || {
            let run = coordinator
                .store()
                .list_current()
                .unwrap()
                .pop()
                .expect("the scan is running");
            coordinator
                .control(&run.id, Event::Stop, run.row_revision)
                .expect("stop is taken");
        });
    }

    /// What is left on an album of the match it held: the match, its flag,
    /// both kinds of edition undo, the remap, a custom edition, its place
    /// in the automatic management queue and reviews waiting for a person.
    fn match_leftovers(&self, album: &str) -> i64 {
        [
            "local_album_external_identities",
            "library_album_match_state",
            "library_edition_choice_undo",
            "library_automatic_edition_undo",
            "library_edition_remap_queue",
            "library_custom_edition_active",
            "library_management_automatic_queue",
        ]
        .iter()
        .map(|table| {
            self.count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE local_album_id = '{album}'"
            ))
        })
        .sum::<i64>()
            + self.count(&format!(
                "SELECT COUNT(*) FROM library_identify_reviews \
                 WHERE local_album_id = '{album}' AND state = 'pending'"
            ))
    }
}

/// A scanned album of `count` files in one folder, read in one window.
async fn plain_album(name: &str, count: usize) -> (RegroupRig, Vec<String>, String) {
    let mut rig = RegroupRig::new(name);
    let files: Vec<String> = (0..count).map(|n| format!("a/t{n:03}.flac")).collect();
    let before = rig.before.clone();
    rig.tag(&files, &before);
    rig.scan().await;
    let album = rig.album_of("a/t000.flac");
    rig.run(PERSON);
    (rig, files, album)
}

/// A listener's favorite of the album. An old row that nothing names is
/// dropped when it empties, and there is nothing left to join.
fn favorite(album: &str) -> String {
    format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'album', '{album}', 0);"
    )
}

/// A six-file album with a listener's favorite, retagged in two sittings:
/// three files seen by one scan, the other three by the next. `prepare`
/// runs on the old row first and `between` (given the old row and the new
/// row) between the two. Returns the rig, the old row and the new row.
async fn two_sittings(
    name: &str,
    prepare: impl FnOnce(&str) -> String,
    between: impl FnOnce(&str, &str) -> String,
) -> (RegroupRig, String, String) {
    let (mut rig, files, album) = plain_album(name, 6).await;
    rig.run(&favorite(&album));
    rig.run(&prepare(&album));
    rig.run("DELETE FROM library_identify_jobs");
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;
    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(
        new_row, album,
        "{name}: the first half sits on a row of its own"
    );
    assert_eq!(rig.retired_into(&album), None, "{name}: nothing joined yet");
    rig.run(&between(&album, &new_row));
    rig.tag(&files[3..], &after);
    rig.scan().await;
    (rig, album, new_row)
}

/// A plan item of a job in `state` on the album (a SQL expression).
fn plan_on(album: &str, job: &str, state: &str) -> String {
    format!(
        "INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
         SELECT '{job}', 'library_management', '{state}', 0, 0 WHERE {album} IS NOT NULL; \
         INSERT INTO library_management_job_snapshots (job_id, mode, origin, phase, \
         selection_json, profile_revision, settings_revision, naming_revision, \
         policy_revision, catalog_revision, profile_snapshot_json, created_at, updated_at) \
         SELECT '{job}', 'preview', 'manual', 'ready', '{{}}', 'p', 's', 'n', 'rev-1', 0, \
         '{{}}', 0, 0 WHERE {album} IS NOT NULL; \
         INSERT INTO library_management_plan_items (job_id, ordinal, bundle_ordinal, \
         local_album_id, expected_album_revision, expected_catalog_revision, \
         expected_policy_revision, expected_profile_revision, expected_root_id, \
         expected_relative_path, expected_stat_revision, expected_tag_revision, \
         expected_file_fingerprint, source_path_identity, desired_document_json, \
         desired_document_hash, eligibility, reason_code, created_at) \
         SELECT '{job}', 0, 0, {album}, 1, 0, 'rev-1', 'p', 'music', 'a/t000.flac', 's', \
         't', 'f', 'i', '{{}}', '{}', 'blocked', 'IDENTITY_NOT_ACCEPTED', 0 \
         WHERE {album} IS NOT NULL;",
        "0".repeat(64),
    )
}

/// A scan stopped between its two windows leaves a retag half done. The
/// next scan finishes it, and its end joins the two rows: one album, the
/// new row, which the old id opens; the plays, favorites and playlist
/// entries of the old row are with it. Without a play or a match, the old
/// row emptied mid-scan is kept for the favorite and the playlist entry
/// alone, which name it without a foreign key, so they are carried too.
/// The tracks that left are on record when the first scan is cancelled,
/// and off it once the album is joined.
#[tokio::test]
async fn retag_across_a_cancelled_scan_is_joined_at_the_next() {
    for (name, played) in [
        ("scan-across-cancelled", true),
        ("scan-across-cancelled-no-play", false),
    ] {
        let (mut rig, files, album) = big_album(name).await;
        let play = if played {
            "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
             track_name, artist_name, played_at) SELECT 'play-1', 'listener', id, \
             local_album_id, 'Song', 'Artist', '2026-01-01T00:00:00Z' FROM local_tracks \
             WHERE relative_path = 'a/t299.flac';"
        } else {
            ""
        };
        rig.run(&format!(
            "{play} INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('listener', 'album', '{album}', 0); \
             INSERT INTO playlists (id, name, created_at, updated_at) \
             VALUES ('list', 'List', '', ''); \
             INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
             album_name, album_id, source_type, created_at) \
             VALUES ('entry', 'list', 0, 'Song', 'Artist', 'Before', '{album}', 'local', '');"
        ));
        let after = rig.after.clone();
        rig.tag(&files, &after);
        rig.stop_before(SECOND_WINDOW);
        assert_eq!(
            rig.scan_to_its_end().await.state.scan_state(),
            ScanState::Cancelled
        );
        let new_row = rig.album_of("a/t000.flac");
        assert_ne!(new_row, album, "{name}");
        assert_eq!(
            rig.departures(),
            256,
            "{name}: the tracks that left are on record"
        );

        rig.scan().await;

        assert_eq!(rig.album_of("a/t299.flac"), new_row, "{name}");
        assert_eq!(rig.retired_into(&album), Some(new_row.clone()), "{name}");
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM local_album_aliases WHERE alias = '{album}' \
                 AND local_album_id = '{new_row}'"
            )),
            1,
            "{name}: the old id opens the new row"
        );
        let (naming, all) = rig.listener_rows(&new_row);
        assert_eq!(
            naming, all,
            "{name}: everything a listener made names the new row"
        );
        assert_eq!(all, if played { 3 } else { 2 }, "{name}");
        assert_eq!(rig.departures(), 0, "{name}: off the record once joined");
    }
}

/// The old row's match, with what was made for it on the old row: a
/// flag, an undo, a credit proof, a place in the automatic management
/// queue, a review waiting for a person, the person who chose it and a
/// remap waiting to place it.
fn old_with(source: &'static str, release: &'static str) -> impl Fn(&str) -> String {
    move |album: &str| {
        let quoted = format!("'{album}'");
        format!(
            "{} {} UPDATE local_album_external_identities SET selected_by_user_id = 'u1' \
             WHERE local_album_id = '{album}'; \
             INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, \
             queued_at, origin) VALUES ('{album}', '{release}', 0, 'group_pin');",
            match_sql(&quoted, source, release),
            automatic_leftovers(&quoted, "old"),
        )
    }
}

/// The match on the album that stays after a retag across two scans, one
/// case for each way the two rows' matches can stand. A person's match is
/// never replaced or dropped except by an equal one; two different
/// people's choices are left apart and recorded. Wherever the old row's
/// match goes or moves, nothing made for it is left on the old row.
#[tokio::test]
async fn match_across_two_scans_lands_on_the_row_that_stays() {
    // A person's match, the new row with none: it moves, with its chooser,
    // its flag and its waiting remap, at a revision neither row has had. A
    // review a person closed on the old row moves with it.
    let (rig, old, new) = two_sittings(
        "scan-across-chosen-none",
        old_with("manual", "rel-1"),
        |old, _| {
            format!(
                "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
                 candidates_json, state, created_ms, updated_ms) \
                 VALUES ('closed-old', '{old}', 'R', '[]', 'approved', 0, 0);"
            )
        },
    )
    .await;
    assert_eq!(
        rig.text("SELECT local_album_id FROM library_identify_reviews WHERE id = 'closed-old'"),
        Some(new.clone())
    );
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(match_of(&rig, &new), matched("rel-1", "manual"));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_album_external_identities i \
             JOIN library_album_match_state s ON s.local_album_id = i.local_album_id \
             JOIN library_edition_remap_queue q ON q.local_album_id = i.local_album_id \
             WHERE i.local_album_id = '{new}' AND i.selected_by_user_id = 'u1' \
             AND i.row_revision > 4 AND s.reason_code = 'old' AND q.origin = 'group_pin'"
        )),
        1
    );
    assert_eq!(rig.match_leftovers(&old), 0);

    // A person's match against an automatic one: the person's moves, the
    // automatic one and what was made for it go, and the review waiting on
    // the new row is closed as a choice by hand closes it, by nobody.
    let (rig, old, new) = two_sittings(
        "scan-across-chosen-auto",
        old_with("manual", "rel-1"),
        |_, new| {
            let quoted = format!("'{new}'");
            format!(
                "DELETE FROM library_management_automatic_queue \
                 WHERE local_album_id = {quoted}; {} {}",
                match_sql(&quoted, "automatic", "rel-2"),
                automatic_leftovers(&quoted, "new")
            )
        },
    )
    .await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(match_of(&rig, &new), matched("rel-1", "manual"));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_identify_reviews WHERE local_album_id = '{new}' \
             AND id = 'pending-new' AND state = 'dismissed' AND resolved_by_user_id IS NULL"
        )),
        1
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_automatic_edition_undo WHERE local_album_id = '{new}'"
        )),
        0
    );
    assert_eq!(rig.match_leftovers(&old), 0);

    // Two equal choices: the new row's stays as it was.
    let (rig, old, new) = two_sittings(
        "scan-across-chosen-equal",
        old_with("manual", "rel-1"),
        |_, new| match_sql(&format!("'{new}'"), "legacy_import", "rel-1"),
    )
    .await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(match_of(&rig, &new), matched("rel-1", "legacy_import"));
    assert_eq!(
        rig.count(&format!(
            "SELECT row_revision FROM local_album_external_identities \
             WHERE local_album_id = '{new}'"
        )),
        4
    );
    assert_eq!(rig.match_leftovers(&old), 0);

    // Two different choices: no merge, the pair is recorded.
    let (rig, old, new) = two_sittings(
        "scan-across-chosen-differ",
        old_with("manual", "rel-1"),
        |_, new| match_sql(&format!("'{new}'"), "manual", "rel-2"),
    )
    .await;
    assert_eq!(rig.retired_into(&old), None);
    assert_eq!(match_of(&rig, &old), matched("rel-1", "manual"));
    assert_eq!(match_of(&rig, &new), matched("rel-2", "manual"));
    assert_eq!(
        retag_pairs(&rig),
        open_pair(&old, &new, "chosen_matches_differ")
    );

    // A custom edition on the old row: no merge, the pair is recorded.
    let (rig, old, new) = two_sittings(
        "scan-across-custom-edition",
        |album| {
            format!(
                "INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
                 release_group_mbid, album_title, album_artist_name, source_album_revision, \
                 input_revision, content_hash, sealed_by_user_id, sealed_at) \
                 VALUES ('custom', '{album}', 1, 'rg', 'T', 'A', 1, 'i', 'h', 'u1', 0); \
                 INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
                 activated_at) VALUES ('{album}', 'custom', 0);"
            )
        },
        |_, _| String::new(),
    )
    .await;
    assert_eq!(rig.retired_into(&old), None);
    assert_eq!(retag_pairs(&rig), open_pair(&old, &new, "custom_edition"));

    // An automatic match, or none, against any match: the new row's stays.
    for (name, old_match) in [
        ("scan-across-auto-chosen", Some("automatic")),
        ("scan-across-none-auto", None),
    ] {
        let (rig, old, new) = two_sittings(
            name,
            |album| old_match.map_or_else(String::new, |source| old_with(source, "rel-1")(album)),
            |_, new| match_sql(&format!("'{new}'"), "manual", "rel-2"),
        )
        .await;
        assert_eq!(rig.retired_into(&old), Some(new.clone()), "{name}");
        assert_eq!(match_of(&rig, &new), matched("rel-2", "manual"), "{name}");
        assert_eq!(rig.match_leftovers(&old), 0, "{name}");
    }

    // An automatic match against none: it moves with its flag at a new
    // revision, as within one scan, and a review waiting on the new row
    // (it judged half the album) goes.
    let (rig, old, new) = two_sittings(
        "scan-across-auto-none",
        old_with("automatic", "rel-1"),
        |_, new| {
            format!(
                "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
                 candidates_json, state, created_ms, updated_ms) \
                 VALUES ('half', '{new}', 'R', '[]', 'pending', 0, 0);"
            )
        },
    )
    .await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(match_of(&rig, &new), matched("rel-1", "automatic"));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_album_external_identities i \
             JOIN library_album_match_state s ON s.local_album_id = i.local_album_id \
             WHERE i.local_album_id = '{new}' AND i.row_revision > 4 AND s.reason_code = 'old'"
        )),
        1
    );
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM library_identify_reviews WHERE id = 'half'"),
        0
    );
    assert_eq!(rig.match_leftovers(&old), 0);

    // No match on either row: joined all the same.
    let (rig, old, new) = two_sittings(
        "scan-across-none-none",
        |_| String::new(),
        |_, _| String::new(),
    )
    .await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(match_of(&rig, &new), None);
}

/// A person's release that moves from the old row onto the new row of an
/// album joined across two scans, whose files sit on another release, is
/// queued to have the files placed on it, and nothing else.
#[tokio::test]
async fn album_joined_across_scans_queues_its_files_for_a_chosen_release() {
    let (rig, old, new) = two_sittings(
        "scan-across-place-chosen",
        |album| {
            let quoted = format!("'{album}'");
            format!(
                "{} {}",
                match_sql(&quoted, "manual", "rel-1"),
                tracks_on(&quoted, "rel-2")
            )
        },
        |_, _| String::new(),
    )
    .await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(match_of(&rig, &new), matched("rel-1", "manual"));
    assert_eq!(placements_queued(&rig, &new), vec!["rel-1".to_owned()]);
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM library_edition_remap_queue"),
        1
    );
}

/// Seven of ten files retagged and the other three deleted in one
/// sitting: the scan that sees it waits (files gone in that very scan
/// still hold their album), and the next one joins it.
#[tokio::test]
async fn retag_what_you_keep_and_delete_the_rest_is_joined_a_scan_later() {
    let (mut rig, files, album) = chosen_album("scan-across-keep-seven", 10).await;
    for relative in &files[7..] {
        std::fs::remove_file(rig.root.join(relative)).expect("file removed");
    }
    let after = rig.after.clone();
    rig.tag(&files[..7], &after);
    rig.scan().await;
    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album);
    assert_eq!(rig.retired_into(&album), None, "nothing at the first end");

    rig.scan().await;
    assert_eq!(rig.retired_into(&album), Some(new_row.clone()), "joined");
    assert_eq!(rig.rows_on(&new_row), 10);
    assert_eq!(match_of(&rig, &new_row), matched("rel-1", "manual"));
    assert_eq!(retag_pairs(&rig), vec![]);
}

/// Three of fifteen files retagged while the other twelve are away. Until
/// the twelve have been gone a day nothing is joined or recorded and the
/// match stays where it is; after that the next scan joins the album.
/// When the twelve come back first, nothing is joined.
#[tokio::test]
async fn few_retagged_while_the_rest_is_away_wait_a_day() {
    for (name, back) in [
        ("scan-across-rest-away", false),
        ("scan-across-rest-back", true),
    ] {
        let (mut rig, files, album) = chosen_album(name, 15).await;
        for relative in &files[3..] {
            std::fs::remove_file(rig.root.join(relative)).expect("file removed");
        }
        let (before, after) = (rig.before.clone(), rig.after.clone());
        rig.tag(&files[..3], &after);
        rig.scan().await;
        let new_row = rig.album_of("a/t000.flac");
        rig.scan().await;
        assert_eq!(rig.retired_into(&album), None, "{name}: still waiting");
        assert_eq!(retag_pairs(&rig), vec![], "{name}");
        assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"), "{name}");

        if back {
            rig.tag(&files[3..], &before);
        } else {
            rig.age_missing(OVER_A_DAY);
        }
        rig.scan().await;
        if back {
            assert_eq!(rig.retired_into(&album), None, "{name}: they hold it again");
            assert_eq!(rig.album_of("a/t014.flac"), album, "{name}");
            assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"), "{name}");
        } else {
            assert_eq!(rig.retired_into(&album), Some(new_row.clone()), "{name}");
            assert_eq!(
                match_of(&rig, &new_row),
                matched("rel-1", "manual"),
                "{name}"
            );
        }
    }
}

/// Fixing one stray track never stops a later rename being followed: a
/// track retagged into an album with tracks of its own says nothing about
/// the album it left, so the rename in one later scan is followed within
/// that scan, as ever.
#[tokio::test]
async fn stray_track_moved_earlier_does_not_stop_a_rename() {
    let (mut rig, files, album) = chosen_album("scan-across-stray", 10).await;
    let other: Vec<String> = (0..3).map(|n| format!("a/u{n}.flac")).collect();
    let elsewhere = "scan-across-stray elsewhere";
    rig.tag(&other, elsewhere);
    rig.scan().await;
    rig.tag(&files[9..], elsewhere);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t009.flac"), rig.album_of("a/u0.flac"));

    let after = rig.after.clone();
    rig.tag(&files[..9], &after);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t000.flac"), album, "followed");
    assert_eq!(
        rig.text(&format!(
            "SELECT title FROM local_albums WHERE id = '{album}'"
        )),
        Some(after)
    );
    assert_eq!(retag_pairs(&rig), vec![]);
}

/// A two-disc album split into two albums over two scans is not joined to
/// either half: the pair of the old album and the half split off last is
/// recorded, and the match stays where it is. Split in one scan, nothing
/// is recorded.
#[tokio::test]
async fn album_split_over_two_scans_is_a_pair() {
    let (mut rig, files, album) = chosen_album("scan-across-two-discs", 10).await;
    rig.tag(&files[..5], "scan-across-two-discs one");
    rig.scan().await;
    rig.tag(&files[5..], "scan-across-two-discs two");
    rig.scan().await;
    let second = rig.album_of("a/t005.flac");
    assert_eq!(
        retag_pairs(&rig),
        open_pair(&album, &second, "tracks_elsewhere")
    );
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
    assert_eq!(rig.retired_into(&album), None);

    let (mut rig, files, album) = chosen_album("scan-across-split-once", 10).await;
    rig.tag(&files[..5], "scan-across-split-once one");
    rig.tag(&files[5..], "scan-across-split-once two");
    rig.scan().await;
    assert_eq!(retag_pairs(&rig), vec![]);
    assert_eq!(rig.retired_into(&album), None);
}

/// Half of an album on a new row since an earlier scan, with a favorite
/// and a taken-over mark set on that row; then the other half and that
/// row's files are all retagged to a third name in one scan. It is one
/// album, holding the match, the favorite and the mark, and neither
/// earlier row is left behind. The two halves are read in separate
/// windows, in both orders: the row of the first half first, or the rest
/// of the old album first.
#[tokio::test]
async fn half_then_all_to_a_third_name_is_one_album() {
    for (name, first_half_read_first) in [
        ("scan-across-third-name", true),
        ("scan-across-third-name-reversed", false),
    ] {
        let (mut rig, files, album) = big_album(name).await;
        rig.accept_match(&album);
        let first = if first_half_read_first {
            &files[..256]
        } else {
            &files[256..]
        };
        let after = rig.after.clone();
        rig.tag(first, &after);
        rig.scan().await;
        let half = rig.album_of(&first[0]);
        assert_ne!(half, album, "{name}");
        rig.run(&format!(
            "{} INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
             VALUES ('{half}', 'files_changed', 1234);",
            favorite(&half)
        ));
        let third = format!("{name} third");
        rig.tag(&files, &third);
        rig.scan().await;

        let kept = rig.album_of("a/t000.flac");
        for relative in ["a/t000.flac", "a/t255.flac", "a/t256.flac", "a/t299.flac"] {
            assert_eq!(rig.album_of(relative), kept, "{name}");
        }
        assert_eq!(rig.retired_into(&album), Some(kept.clone()), "{name}");
        if half != kept {
            assert_eq!(rig.retired_into(&half), Some(kept.clone()), "{name}");
        }
        assert_eq!(match_of(&rig, &kept), matched("rel-1", "manual"), "{name}");
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM library_user_favorites WHERE item_id = '{kept}'"
            )),
            1,
            "{name}: the favorite is with the album"
        );
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = '{kept}' \
                 AND noticed_at = 1234"
            )),
            1,
            "{name}: and so is the mark"
        );
        assert_eq!(
            rig.count(
                "SELECT COUNT(*) FROM local_albums a WHERE a.retired_into_album_id IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id)"
            ),
            0,
            "{name}: no empty row is left"
        );
    }
}

/// Two albums retagged into one new album in one scan are a person joining
/// them. When that album is then renamed over two sittings, the album the
/// rename leaves is joined to the new name, but the two albums its tracks
/// came from are not: they stay as they were, and the album that stays is
/// not stamped from the release a person chose for one of them.
#[tokio::test]
async fn albums_joined_by_hand_earlier_are_not_joined_to_a_later_rename() {
    let mut rig = RegroupRig::new("scan-across-combined");
    let b: Vec<String> = (0..3).map(|n| format!("a/u{n}.flac")).collect();
    let c: Vec<String> = (0..3).map(|n| format!("a/v{n}.flac")).collect();
    rig.tag(&b, "scan-across-combined b");
    rig.tag(&c, "scan-across-combined c");
    rig.scan().await;
    let (album_b, album_c) = (rig.album_of("a/u0.flac"), rig.album_of("a/v0.flac"));
    rig.accept_match(&album_b);
    rig.run(&favorite(&album_c));

    let before = rig.before.clone();
    rig.tag(&b, &before);
    rig.tag(&c, &before);
    rig.scan().await;
    let combined = rig.album_of("a/u0.flac");
    assert_eq!(rig.album_of("a/v0.flac"), combined);
    assert_eq!(rig.retired_into(&album_b), None);
    rig.run(&favorite(&combined));

    let after = rig.after.clone();
    rig.tag(&b, &after);
    rig.scan().await;
    rig.tag(&c, &after);
    rig.scan().await;

    let renamed = rig.album_of("a/u0.flac");
    assert_eq!(rig.album_of("a/v0.flac"), renamed);
    assert_eq!(rig.retired_into(&combined), Some(renamed.clone()));
    assert_eq!(rig.retired_into(&album_b), None);
    assert_eq!(rig.retired_into(&album_c), None);
    assert_eq!(match_of(&rig, &album_b), matched("rel-1", "manual"));
    assert!(!rig.stamped(&renamed));
}

/// A pair a person dismissed is their answer about that album: renaming the
/// other album over two sittings later does not join the first one to the
/// new name.
#[tokio::test]
async fn dismissed_album_is_not_joined_by_a_later_rename() {
    let (mut rig, old, new) = two_sittings(
        "scan-across-dismissed-then-renamed",
        |_| String::new(),
        |old, new| {
            format!(
                "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
                 recorded_at, updated_at) VALUES ('{old}', '{new}', 'tracks_elsewhere', \
                 'dismissed', 0, 0);"
            )
        },
    )
    .await;
    assert_eq!(rig.retired_into(&old), None);
    rig.run(&favorite(&new));
    let files: Vec<String> = (0..6).map(|n| format!("a/t{n:03}.flac")).collect();
    let last = "scan-across-dismissed-then-renamed last";
    rig.tag(&files[..3], last);
    rig.scan().await;
    rig.tag(&files[3..], last);
    rig.scan().await;

    let renamed = rig.album_of("a/t000.flac");
    assert_eq!(rig.retired_into(&new), Some(renamed));
    assert_eq!(rig.retired_into(&old), None, "the dismissed album stays");
}

/// A track judged by when it left the album would make an album that was
/// there before look like part of it, once the track moved on again. An
/// album with a chosen release has three files retagged to a new name and
/// the other seven deleted; an unrelated album is added; the three are
/// retagged into it; a day later the old album is still not merged into
/// the unrelated one.
#[tokio::test]
async fn track_that_moved_on_again_does_not_make_an_older_album_a_piece() {
    let (mut rig, files, album) = chosen_album("scan-across-moved-on", 10).await;
    for relative in &files[3..] {
        std::fs::remove_file(rig.root.join(relative)).expect("file removed");
    }
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;
    let elsewhere = "scan-across-moved-on elsewhere";
    let other: Vec<String> = (0..3).map(|n| format!("a/u{n}.flac")).collect();
    rig.tag(&other, elsewhere);
    rig.scan().await;
    let unrelated = rig.album_of("a/u0.flac");
    rig.tag(&files[..3], elsewhere);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t000.flac"), unrelated);
    rig.age_missing(OVER_A_DAY);
    rig.scan().await;

    assert_eq!(rig.retired_into(&album), None);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
    assert_eq!(match_of(&rig, &unrelated), None);
}

/// Two albums retagged into one new album over two scans are a person
/// joining them: neither is joined to it nor recorded, whichever album's
/// files the scan reads first.
#[tokio::test]
async fn two_albums_retagged_into_one_over_two_scans_are_not_joined() {
    for (name, prefixes) in [
        ("scan-across-two-into-one", ["t", "u"]),
        ("scan-across-two-into-one-reversed", ["u", "t"]),
    ] {
        let mut rig = RegroupRig::new(name);
        let first: Vec<String> = (0..4)
            .map(|n| format!("a/{}{n}.flac", prefixes[0]))
            .collect();
        let second: Vec<String> = (0..4)
            .map(|n| format!("a/{}{n}.flac", prefixes[1]))
            .collect();
        let (before, after) = (rig.before.clone(), rig.after.clone());
        rig.tag(&first, &before);
        rig.tag(&second, &format!("{name} other"));
        rig.scan().await;
        let (one, other) = (rig.album_of(&first[0]), rig.album_of(&second[0]));
        rig.accept_match(&one);

        rig.tag(&first[..2], &after);
        rig.tag(&second[..2], &after);
        rig.scan().await;
        rig.tag(&first[2..], &after);
        rig.tag(&second[2..], &after);
        rig.scan().await;

        let joined = rig.album_of(&first[3]);
        assert_eq!(rig.album_of(&second[3]), joined, "{name}");
        assert_eq!(rig.retired_into(&one), None, "{name}");
        assert_eq!(rig.retired_into(&other), None, "{name}");
        assert_eq!(retag_pairs(&rig), vec![], "{name}");
        assert_eq!(match_of(&rig, &one), matched("rel-1", "manual"), "{name}");
    }
}

/// Retagged over two scans with a file added: joined, the added file on
/// the row that stays, nothing recorded. Files added do not count towards
/// the wait: five of twelve retagged over two scans with three added,
/// while the other seven have been gone an hour, is not joined.
#[tokio::test]
async fn retag_over_two_scans_with_a_file_added() {
    let (mut rig, files, album) = chosen_album("scan-across-added", 6).await;
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;
    let mut rest = files[3..].to_vec();
    rest.push("a/n0.flac".to_owned());
    rig.tag(&rest, &after);
    rig.scan().await;
    let new_row = rig.album_of("a/t000.flac");
    assert_eq!(rig.retired_into(&album), Some(new_row.clone()));
    assert_eq!(rig.album_of("a/n0.flac"), new_row);
    assert_eq!(rig.rows_on(&new_row), 7);
    assert_eq!(retag_pairs(&rig), vec![]);

    let (mut rig, files, album) = chosen_album("scan-across-added-waits", 12).await;
    rig.lose(&files[5..]).await;
    rig.age_missing(3_600);
    let after = rig.after.clone();
    rig.tag(&files[..2], &after);
    rig.scan().await;
    let mut rest = files[2..5].to_vec();
    rest.extend((0..3).map(|n| format!("a/n{n}.flac")));
    rig.tag(&rest, &after);
    rig.scan().await;
    assert_eq!(rig.retired_into(&album), None, "waits for the seven");
    assert_eq!(retag_pairs(&rig), vec![]);
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// A plan item of a finished job on the new row keeps the follow within
/// the scan from deleting that row. The end of the same scan joins the old
/// row into it instead: no plan row changes and no pair is left.
#[tokio::test]
async fn finished_work_on_the_new_row_is_joined_at_the_end_of_the_scan() {
    let (rig, album) = retag_across_two_windows("scan-across-finished-plan", |album| {
        plan_on(&new_row(album), "plan", "succeeded")
    })
    .await;
    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album);
    assert_eq!(rig.retired_into(&album), Some(new_row.clone()));
    assert_eq!(match_of(&rig, &new_row), matched("rel-1", "manual"));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_management_plan_items \
             WHERE local_album_id = '{new_row}' AND expected_album_revision = 1"
        )),
        1,
        "the plan item is as it was made"
    );
    assert_eq!(retag_pairs(&rig), vec![]);
}

/// A preview left ready on the old row stops the join and is recorded as
/// live work. Once the preview is discarded, the next scan's end joins
/// the two and the pair goes.
#[tokio::test]
async fn ready_preview_on_the_old_row_waits_until_it_is_discarded() {
    let (rig, old, new) = two_sittings(
        "scan-across-live-work",
        |album| {
            let quoted = format!("'{album}'");
            format!(
                "{} {}",
                match_sql(&quoted, "manual", "rel-1"),
                plan_on(&quoted, "preview", "ready")
            )
        },
        |_, _| String::new(),
    )
    .await;
    assert_eq!(rig.retired_into(&old), None);
    assert_eq!(retag_pairs(&rig), open_pair(&old, &new, "live_work"));

    rig.run("UPDATE library_operation_jobs SET state = 'cancelled' WHERE id = 'preview'");
    rig.scan().await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(match_of(&rig, &new), matched("rel-1", "manual"));
    assert_eq!(retag_pairs(&rig), vec![]);
}

/// A store error while joining keeps both rows and is recorded once, with
/// one warning. The end of the same scan tries again, as a join, and
/// fails the same way: still one row, no second warning. Once the store
/// works, the next scan's end joins them and the pair goes.
#[tokio::test]
async fn store_error_is_recorded_once_and_tried_again() {
    retag_warnings();
    let (mut rig, files, album) = chosen_album("scan-across-store-error", 4).await;
    rig.run(
        "CREATE TRIGGER refuse_stamp BEFORE UPDATE OF retag_followed_at \
         ON library_management_automatic_queue WHEN NEW.retag_followed_at IS NOT NULL \
         BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
    );
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.scan().await;
    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album);
    assert_eq!(
        retag_pairs(&rig),
        open_pair(&album, &new_row, "store_error")
    );
    assert_eq!(warnings_naming(&album, &new_row), 1);

    rig.run("DROP TRIGGER refuse_stamp");
    rig.scan().await;
    assert_eq!(rig.retired_into(&album), Some(new_row.clone()));
    assert_eq!(retag_pairs(&rig), vec![]);
    assert!(rig.stamped(&new_row));
    assert_eq!(warnings_naming(&album, &new_row), 1);
}

/// The first half moved to another folder by the app between the two
/// sittings: the second half lands on a third row of its own, nothing is
/// joined, and the old album and that third row are recorded as a pair.
#[tokio::test]
async fn first_half_moved_away_leaves_a_pair() {
    let (mut rig, files, album) = chosen_album("scan-across-moved-away", 6).await;
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;
    let first = rig.album_of("a/t000.flac");
    std::fs::create_dir_all(rig.root.join("b")).expect("mkdir");
    for relative in &files[..3] {
        let moved = relative.replacen("a/", "b/", 1);
        std::fs::rename(rig.root.join(relative), rig.root.join(&moved)).expect("file moved");
        rig.reader.retag(&moved, &after);
        rig.run(&format!(
            "UPDATE local_tracks SET relative_path = '{moved}', path_hash = '{:x}', \
             file_path = '{}' WHERE relative_path = '{relative}'",
            Sha256::digest(moved.as_bytes()),
            rig.root.join(&moved).display()
        ));
    }
    rig.tag(&files[3..], &after);
    rig.scan().await;

    let third = rig.album_of("a/t003.flac");
    assert_ne!(third, first);
    assert_eq!(rig.album_of("b/t000.flac"), first);
    assert_eq!(rig.retired_into(&album), None);
    assert_eq!(
        retag_pairs(&rig),
        open_pair(&album, &third, "tracks_elsewhere")
    );
    assert_eq!(match_of(&rig, &album), matched("rel-1", "manual"));
}

/// Joined across two scans, the album that stays is stamped for automatic
/// management to leave it unmanaged only when the old row's match was a
/// release a person chose and the app has no record of any of its files:
/// not for an automatic match, not for a release a person chose on the
/// new row after the retag (the same one or another), not when one file
/// has a record of the app's (v3 or v2), and not for the app's own retag.
/// The old row's match is set between the two sittings here, so the first
/// one stamps nothing.
#[tokio::test]
async fn album_joined_across_scans_is_stamped_only_for_a_hand_retag_of_a_chosen_release() {
    let recorded = |columns: &str, values: &str, which: &str| {
        format!(
            "INSERT INTO library_track_management_state (local_track_id, {columns}) \
             SELECT id, {values} FROM local_tracks WHERE {which};"
        )
    };
    let cases: [(&str, String, bool); 7] = [
        ("scan-across-stamp", String::new(), true),
        ("scan-across-stamp-automatic", "automatic".to_owned(), false),
        ("scan-across-stamp-new-chosen", "new".to_owned(), false),
        ("scan-across-stamp-new-equal", "equal".to_owned(), false),
        (
            "scan-across-stamp-written",
            recorded("written_sha256", "'abc'", "relative_path = 'a/t004.flac'"),
            false,
        ),
        (
            "scan-across-stamp-v2",
            recorded(
                "managed_path_revision, last_outcome",
                "'v2', 'succeeded'",
                "relative_path = 'a/t001.flac'",
            ),
            false,
        ),
        (
            "scan-across-stamp-own-retag",
            recorded("written_sha256", "'abc'", "1"),
            false,
        ),
    ];
    for (name, extra, stamped) in cases {
        let (rig, old, new) = two_sittings(
            name,
            |_| String::new(),
            |old, new| {
                let quoted = format!("'{old}'");
                match extra.as_str() {
                    "automatic" => match_sql(&quoted, "automatic", "rel-1"),
                    "new" => format!(
                        "{} {}",
                        match_sql(&quoted, "automatic", "rel-1"),
                        match_sql(&format!("'{new}'"), "manual", "rel-2")
                    ),
                    "equal" => format!(
                        "{} {}",
                        match_sql(&quoted, "manual", "rel-1"),
                        match_sql(&format!("'{new}'"), "manual", "rel-1")
                    ),
                    other => format!("{} {other}", match_sql(&quoted, "manual", "rel-1")),
                }
            },
        )
        .await;
        assert_eq!(rig.retired_into(&old), Some(new.clone()), "{name}");
        assert_eq!(rig.stamped(&new), stamped, "{name}");
    }
}

/// The half of an album with a chosen release that a first sitting
/// retagged to a new name, files the app never wrote, is stamped when that
/// scan ends, so automatic management does not write half an album while
/// it is being retagged; the same with a file added in that sitting.
/// Handed back between the sittings (the app then wrote the files), the
/// second sitting joins the two and stamps nothing.
#[tokio::test]
async fn first_half_of_a_hand_retag_is_stamped_when_the_scan_ends() {
    for (name, added) in [
        ("scan-across-first-half", false),
        ("scan-across-first-half-added", true),
    ] {
        let (mut rig, files, album) = chosen_album(name, 6).await;
        let mut first = files[..3].to_vec();
        if added {
            first.push("a/n0.flac".to_owned());
        }
        let after = rig.after.clone();
        rig.tag(&first, &after);
        rig.scan().await;
        let new_row = rig.album_of("a/t000.flac");
        assert!(rig.stamped(&new_row), "{name}");
        assert_eq!(rig.retired_into(&album), None, "{name}");
    }

    let (mut rig, files, album) = chosen_album("scan-across-first-half-handed-back", 6).await;
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;
    let new_row = rig.album_of("a/t000.flac");
    rig.run(&format!(
        "UPDATE library_management_automatic_queue SET retag_followed_at = NULL \
         WHERE local_album_id = '{new_row}'; \
         INSERT INTO library_track_management_state (local_track_id, written_sha256) \
         SELECT id, 'written' FROM local_tracks WHERE local_album_id = '{new_row}';"
    ));
    rig.tag(&files[3..], &after);
    rig.scan().await;
    assert_eq!(rig.retired_into(&album), Some(new_row.clone()));
    assert!(!rig.stamped(&new_row));
}

/// A match a person chose for the half retagged in a first sitting, while
/// the scan was on it, is their word for those files: that half is neither
/// stamped nor given the old album's "leave unmanaged".
#[tokio::test]
async fn first_half_with_a_match_a_person_chose_is_left_to_them() {
    let (mut rig, files, album) = big_album("scan-across-first-half-chosen").await;
    rig.accept_match(&album);
    rig.run(&format!(
        "{PERSON} INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) VALUES ('{album}', 'by hand', 'u1', 5);"
    ));
    let (before, after) = (rig.before.clone(), rig.after.clone());
    rig.tag(&files[..256], &after);
    // Rewritten unchanged, so the scan reads the second window too.
    rig.tag(&files[256..], &before);
    rig.meanwhile_sql(match_sql(&new_row(&album), "manual", "rel-2"));
    rig.scan().await;

    let half = rig.album_of("a/t000.flac");
    assert_ne!(half, album);
    assert_eq!(match_of(&rig, &half), matched("rel-2", "manual"));
    assert!(!rig.stamped(&half));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_management_exclusions WHERE local_album_id = '{half}'"
        )),
        0
    );
}

/// A store error while leaving the first half the way the old album was is
/// recorded as a pair, and the end of a later scan tries it again while the
/// old album is still held: it lands, and the pair goes.
#[tokio::test]
async fn first_half_left_unstamped_by_a_store_error_is_tried_again() {
    let (mut rig, files, album) = chosen_album("scan-across-first-half-error", 6).await;
    rig.run(
        "CREATE TRIGGER refuse_stamp BEFORE UPDATE OF retag_followed_at \
         ON library_management_automatic_queue WHEN NEW.retag_followed_at IS NOT NULL \
         BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
    );
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;
    let half = rig.album_of("a/t000.flac");
    assert!(!rig.stamped(&half));
    assert_eq!(retag_pairs(&rig), open_pair(&album, &half, "store_error"));

    rig.run("DROP TRIGGER refuse_stamp");
    rig.scan().await;
    assert!(rig.stamped(&half));
    assert_eq!(retag_pairs(&rig), vec![]);
    assert_eq!(rig.retired_into(&album), None);
}

/// "Leave unmanaged", a taken-over mark and a chosen release on an album
/// do not reach an album with tracks of its own that one of its tracks
/// was retagged into.
#[tokio::test]
async fn track_retagged_into_another_album_takes_nothing_with_it() {
    let (mut rig, files, album) = chosen_album("scan-across-into-other", 6).await;
    rig.run(PERSON);
    rig.run(&format!(
        "INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) VALUES ('{album}', 'by hand', 'u1', 5); \
         INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('{album}', 'files_changed', 1234);"
    ));
    let other: Vec<String> = (0..3).map(|n| format!("a/u{n}.flac")).collect();
    let elsewhere = "scan-across-into-other elsewhere";
    rig.tag(&other, elsewhere);
    rig.scan().await;
    let target = rig.album_of("a/u0.flac");
    rig.tag(&files[5..], elsewhere);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t005.flac"), target);
    assert!(!rig.stamped(&target));
    assert_eq!(
        rig.count(&format!(
            "SELECT (SELECT COUNT(*) FROM library_management_exclusions \
             WHERE local_album_id = '{target}') + (SELECT COUNT(*) FROM library_album_takeovers \
             WHERE local_album_id = '{target}')"
        )),
        0
    );
}

/// The half of an album marked "leave unmanaged" that a first sitting
/// retagged is left unmanaged too when that scan ends, with an audit
/// action saying so. Once per pair: lifted there, a later scan that moves
/// more of the album does not put it back, and a mark set on the old
/// album since is not copied.
#[tokio::test]
async fn leave_unmanaged_reaches_the_first_half_once() {
    let (mut rig, files, album) = plain_album("scan-across-leave-unmanaged", 6).await;
    rig.run(&format!(
        "INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) VALUES ('{album}', 'by hand', 'u1', 5);"
    ));
    let after = rig.after.clone();
    rig.tag(&files[..2], &after);
    rig.scan().await;
    let new_row = rig.album_of("a/t000.flac");
    let excluded = format!(
        "SELECT COUNT(*) FROM library_management_exclusions WHERE local_album_id = '{new_row}'"
    );
    assert_eq!(rig.count(&excluded), 1);
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE local_album_id = '{new_row}' \
             AND action_kind = 'retag_first_half'"
        )),
        1
    );

    rig.run(&format!(
        "DELETE FROM library_management_exclusions WHERE local_album_id = '{new_row}'; \
         INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('{album}', 'files_changed', 1234);"
    ));
    rig.tag(&files[2..4], &after);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t003.flac"), new_row);
    assert_eq!(rig.count(&excluded), 0, "not put back");
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = '{new_row}'"
        )),
        0,
        "a later mark is not copied"
    );
}

/// The app's taken-over mark: the half a first sitting retagged has it
/// when that scan ends, and handed back there, a later scan does not put
/// it back. Set on the old row between the sittings, the row that stays
/// after the join has it, with the time it was noticed.
#[tokio::test]
async fn taken_over_mark_follows_the_retag() {
    let (mut rig, files, album) = plain_album("scan-across-mark", 6).await;
    rig.run(&format!(
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES ('{album}', 'files_changed', 1234);"
    ));
    let after = rig.after.clone();
    rig.tag(&files[..2], &after);
    rig.scan().await;
    let new_row = rig.album_of("a/t000.flac");
    let mark = format!(
        "SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = '{new_row}' \
         AND noticed_at = 1234"
    );
    assert_eq!(rig.count(&mark), 1);
    rig.run(&format!(
        "DELETE FROM library_album_takeovers WHERE local_album_id = '{new_row}'"
    ));
    rig.tag(&files[2..4], &after);
    rig.scan().await;
    assert_eq!(rig.count(&mark), 0, "handed back, not put back");

    let (rig, old, new) = two_sittings(
        "scan-across-mark-joined",
        |_| String::new(),
        |old, _| {
            format!(
                "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
                 VALUES ('{old}', 'files_changed', 1234);"
            )
        },
    )
    .await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = '{new}' \
             AND noticed_at = 1234"
        )),
        1
    );
}

/// An album field overridden on both rows: the new row's override stands
/// after the join, and the old one stays on the old row.
#[tokio::test]
async fn new_rows_override_stands_after_the_join() {
    let override_sql = |id: &str, album: &str, value: &str| {
        format!(
            "INSERT INTO library_management_overrides (id, subject_kind, local_album_id, \
             field_name, value_json, mode, subject_revision, created_at, updated_at) \
             VALUES ('{id}', 'album', '{album}', 'title', '\"{value}\"', 'replace', 1, 0, 0);"
        )
    };
    let (rig, old, new) = two_sittings(
        "scan-across-overrides",
        |album| override_sql("old", album, "Old"),
        |_, new| override_sql("new", new, "New"),
    )
    .await;
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(
        rig.text(&format!(
            "SELECT value_json FROM library_management_overrides \
             WHERE local_album_id = '{new}' AND field_name = 'title'"
        )),
        Some("\"New\"".to_owned())
    );
    assert_eq!(
        rig.text("SELECT local_album_id FROM library_management_overrides WHERE id = 'old'"),
        Some(old)
    );
}

/// A pair a person dismissed is never joined.
#[tokio::test]
async fn dismissed_pair_is_never_joined() {
    let (rig, old, new) = two_sittings(
        "scan-across-dismissed",
        |_| String::new(),
        |old, new| {
            format!(
                "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
                 recorded_at, updated_at) VALUES ('{old}', '{new}', 'tracks_elsewhere', \
                 'dismissed', 0, 0);"
            )
        },
    )
    .await;
    assert_eq!(rig.retired_into(&old), None);
    assert_eq!(
        retag_pairs(&rig),
        vec![(
            old,
            new,
            "tracks_elsewhere".to_owned(),
            "dismissed".to_owned()
        )]
    );
}

/// An album joined twice across scans: the first id still opens the last
/// album.
#[tokio::test]
async fn album_joined_twice_keeps_its_first_id() {
    let (mut rig, files, first) = plain_album("scan-across-twice", 6).await;
    rig.run(&favorite(&first));
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;
    rig.tag(&files[3..], &after);
    rig.scan().await;
    let second = rig.album_of("a/t000.flac");
    assert_eq!(rig.retired_into(&first), Some(second.clone()));

    let last_name = "scan-across-twice last";
    rig.tag(&files[..3], last_name);
    rig.scan().await;
    rig.tag(&files[3..], last_name);
    rig.scan().await;
    let last = rig.album_of("a/t000.flac");
    assert_ne!(last, second);
    assert_eq!(rig.retired_into(&second), Some(last.clone()));
    assert_eq!(rig.retired_into(&first), Some(last));
}

/// A track that comes back to the album it left is off the record.
#[tokio::test]
async fn returned_track_is_off_the_record() {
    let (mut rig, files, album) = plain_album("scan-across-returned", 6).await;
    let (before, after) = (rig.before.clone(), rig.after.clone());
    rig.tag(&files[..3], &after);
    rig.scan().await;
    assert_eq!(rig.departures(), 3);
    rig.tag(&files[..3], &before);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t000.flac"), album);
    assert_eq!(rig.departures(), 0);
}

/// Merging an album by hand removes the open retag pair it is the old row
/// of: there is nothing left to join.
#[tokio::test]
async fn hand_merge_removes_an_open_pair() {
    use droppedneedle_library::corrections::models::{
        ApplyMeta, MembershipKind, MembershipRequest,
    };

    let scratch = ScratchDir::new("scan-pair-hand-merge");
    let music = scratch.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    plant(&music, "b/01.flac", "flac_full_02.flac");
    let library = bundle(&scratch).await;
    scan(&library).await;
    let store = &library.scan_store;
    let track = store.track_at("music", "a/01.flac").unwrap().expect("a/01");
    let source = store.album_for_track(&track).unwrap().expect("album a");
    let target = store
        .album_for_track(&store.track_at("music", "b/01.flac").unwrap().expect("b/01"))
        .unwrap()
        .expect("album b");
    store
        .db()
        .run_sql(&format!(
            "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
             recorded_at, updated_at) VALUES ('{source}', '{target}', 'tracks_elsewhere', \
             'open', 0, 0);"
        ))
        .expect("pair recorded");

    let corrections = crate::common::library::services(&library).corrections;
    let request = MembershipRequest {
        kind: MembershipKind::Merge,
        album_id: None,
        track_ids: vec![track],
        expected_album_revisions: BTreeMap::new(),
        target_album_id: Some(target),
        title: None,
        album_artist_name: None,
        identity_choice: Default::default(),
    };
    let preview = corrections
        .preview_membership(&request, "admin")
        .expect("previewed");
    corrections
        .apply_membership(
            &request,
            &ApplyMeta {
                preview_token: preview.token,
                idempotency_key: None,
                actor: "admin".to_owned(),
            },
        )
        .expect("merged");
    assert_eq!(
        query(&library, "SELECT COUNT(*) FROM library_retag_pairs"),
        0
    );
}

/// A reset grouping that makes a new album credits it to the row a scan
/// of the file keys, the name row, not an older row of the same name that
/// is linked to another artist. Nobody's favourite moves.
#[tokio::test]
async fn a_reset_album_is_credited_as_a_scan_keys_it() {
    let scratch = ScratchDir::new("scan-reset-artist");
    let music = scratch.join("music");
    // Credited "Radiohead" with no album artist id and no listed artists.
    plant(&music, "a/01.flac", "flac_full_01.flac");
    let library = bundle(&scratch).await;
    scan(&library).await;
    let store = &library.scan_store;
    let track = store.track_at("music", "a/01.flac").unwrap().expect("a/01");
    let album = store.album_for_track(&track).unwrap().expect("album");
    let db = droppedneedle_kernel::db::open_connection(&scratch.join("app.db")).expect("db opens");
    let name_row: String = db
        .query_row(
            "SELECT local_artist_id FROM local_album_artists WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("album credit");
    // An older live row of the same name, linked to another artist; the
    // track is held on its album by hand, and the album's key moved on, so
    // a reset makes a new album.
    store
        .db()
        .run_sql(&format!(
            "INSERT INTO local_artists (id, display_name, sort_name, folded_name, \
             normalized_name, kind, created_at, updated_at) \
             SELECT 'twin', display_name, sort_name, folded_name, normalized_name, kind, \
             created_at - 1, updated_at FROM local_artists WHERE id = '{name_row}'; \
             INSERT INTO local_artist_external_identities (local_artist_id, provider, \
             provider_artist_id, decision_source, selected_at) \
             VALUES ('twin', 'musicbrainz', 'cccccccc-0000-4000-8000-00000000000c', 'manual', 1); \
             INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('listener', 'artist', 'twin', 0), ('listener', 'artist', '{name_row}', 0); \
             UPDATE local_tracks SET membership_locked = 1, membership_source = 'manual' \
             WHERE id = '{track}'; \
             UPDATE local_albums SET grouping_key = 'moved-on' WHERE id = '{album}';"
        ))
        .expect("seeds");

    reset_grouping(&library, &album, &track);
    let reset = store.album_for_track(&track).unwrap().expect("reset album");
    assert_ne!(reset, album, "the reset made a new album");
    let credited: (String, String) = db
        .query_row(
            "SELECT a.album_artist_id, c.local_artist_id FROM local_albums a \
             JOIN local_album_artists c ON c.local_album_id = a.id WHERE a.id = ?1",
            [&reset],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("reset album credit");
    assert_eq!(credited, (name_row.clone(), name_row.clone()));
    for artist in ["twin", name_row.as_str()] {
        assert_eq!(
            query(
                &library,
                &format!(
                    "SELECT COUNT(*) FROM library_user_favorites \
                     WHERE item_kind = 'artist' AND item_id = '{artist}'"
                )
            ),
            1,
            "{artist} keeps its favourite"
        );
    }
}

/// A reset album's credit carries no artist id: the stored album artist id
/// is only the file's first one, so for "A & B" it would name A alone. The
/// album stays on the "A & B" name row, not on A's row, and the next scan
/// writes the ids the file holds.
#[tokio::test]
async fn a_reset_album_credit_carries_no_artist_id() {
    const ID_A: &str = "aaaaaaaa-1111-4111-8111-00000000000a";
    let scratch = ScratchDir::new("scan-reset-joined");
    let music = scratch.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    let library = bundle(&scratch).await;
    scan(&library).await;
    let store = &library.scan_store;
    let track = store.track_at("music", "a/01.flac").unwrap().expect("a/01");
    let album = store.album_for_track(&track).unwrap().expect("album");
    // The file names "A & B" with ids for A and B; the column keeps A's.
    // A has a row of its own, linked by a person.
    store
        .db()
        .run_sql(&format!(
            "UPDATE local_tracks SET album_artist_name = 'A & B', \
             embedded_album_artist_mbid = '{ID_A}', membership_locked = 1, \
             membership_source = 'manual' WHERE id = '{track}'; \
             INSERT INTO local_artists (id, display_name, folded_name, normalized_name, kind, \
             created_at, updated_at) VALUES ('ar-a', 'A', 'a', 'a', 'person', 0, 0); \
             INSERT INTO local_artist_external_identities (local_artist_id, provider, \
             provider_artist_id, decision_source, selected_at) \
             VALUES ('ar-a', 'musicbrainz', '{ID_A}', 'manual', 1); \
             UPDATE local_albums SET grouping_key = 'moved-on' WHERE id = '{album}';"
        ))
        .expect("seeds");
    reset_grouping(&library, &album, &track);
    let reset = store.album_for_track(&track).unwrap().expect("reset album");
    assert_ne!(reset, album, "the reset made a new album");
    let db = droppedneedle_kernel::db::open_connection(&scratch.join("app.db")).expect("db opens");
    let credited: (String, String, Option<String>) = db
        .query_row(
            "SELECT c.local_artist_id, r.display_name, c.artist_mbid \
             FROM local_album_artists c JOIN local_artists r ON r.id = c.local_artist_id \
             WHERE c.local_album_id = ?1",
            [&reset],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("reset album credit");
    assert_ne!(credited.0, "ar-a", "not filed under A alone");
    assert_eq!(credited.1, "A & B");
    assert_eq!(credited.2, None, "no id on the album credit");
}

/// Reset `track` on `album` back to automatic grouping, as an admin.
fn reset_grouping(library: &LibrarySetup, album: &str, track: &str) {
    use droppedneedle_library::corrections::models::{
        ApplyMeta, MembershipKind, MembershipRequest,
    };
    let corrections = crate::common::library::services(library).corrections;
    let request = MembershipRequest {
        kind: MembershipKind::Reset,
        album_id: Some(album.to_owned()),
        track_ids: vec![track.to_owned()],
        expected_album_revisions: BTreeMap::new(),
        target_album_id: None,
        title: None,
        album_artist_name: None,
        identity_choice: Default::default(),
    };
    let preview = corrections
        .preview_membership(&request, "admin")
        .expect("previewed");
    corrections
        .apply_membership(
            &request,
            &ApplyMeta {
                preview_token: preview.token,
                idempotency_key: None,
                actor: "admin".to_owned(),
            },
        )
        .expect("reset");
}

// ---------------------------------------------------------------------------
// A missing track continuing into the track that took its place.
// ---------------------------------------------------------------------------

/// The release the continuation briefs' albums are matched to, and one of
/// its tracks.
const REL_R: &str = "rel-r";
const TRACK_X: &str = "rt-x";

/// One album of `count` files, matched by a person to [`REL_R`]. Returns
/// the rig, the album and the track rows of `a/t000.flac` onwards.
async fn continuation_album(name: &str, count: usize) -> (RegroupRig, String, Vec<String>) {
    let mut rig = RegroupRig::new(name);
    let files: Vec<String> = (0..count).map(|n| format!("a/t{n:03}.flac")).collect();
    let before = rig.before.clone();
    rig.tag(&files, &before);
    rig.scan().await;
    let album = rig.album_of("a/t000.flac");
    rig.run(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at) \
         VALUES ('{album}', 'musicbrainz', 'rg-r', '{REL_R}', 'manual', 0);"
    ));
    let store = rig.coordinator.store();
    let tracks = files
        .iter()
        .map(|file| {
            store
                .track_at("music", file)
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    (rig, album, tracks)
}

/// A track identity naming `release` (or none) and `release_track` (or
/// none), with the recording `rec-x`.
fn map_track(track: &str, release: Option<&str>, release_track: Option<&str>) -> String {
    let quote = |value: Option<&str>| value.map_or("NULL".to_owned(), |v| format!("'{v}'"));
    format!(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at) \
         VALUES ('{track}', 'musicbrainz', 'rec-x', {}, {}, 'manual', 0) \
         ON CONFLICT (local_track_id, provider) DO UPDATE SET \
         release_mbid = excluded.release_mbid, \
         release_track_mbid = excluded.release_track_mbid;",
        quote(release),
        quote(release_track)
    )
}

/// A listener's star on the track.
fn star(track: &str) -> String {
    format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{track}', 1);"
    )
}

impl RegroupRig {
    /// Stars on the track, and how many aliases exist.
    fn stars_and_aliases(&self, track: &str) -> (i64, i64) {
        (
            self.count(&format!(
                "SELECT COUNT(*) FROM library_user_favorites WHERE item_id = '{track}'"
            )),
            self.count("SELECT COUNT(*) FROM local_track_aliases"),
        )
    }

    /// The alias rows, as (old, new), in order.
    fn aliases(&self) -> Vec<(String, String)> {
        let rows = self.count("SELECT COUNT(*) FROM local_track_aliases");
        (0..rows)
            .map(|n| {
                let pick = |column: &str| {
                    self.text(&format!(
                        "SELECT {column} FROM local_track_aliases ORDER BY alias LIMIT 1 OFFSET {n}"
                    ))
                    .expect("alias row")
                };
                (pick("alias"), pick("local_track_id"))
            })
            .collect()
    }

    /// Continuation audit rows written so far.
    fn continuations(&self) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = 'continue_tracks'",
        )
    }
}

/// When the proof is not there, nothing moves and no alias is made. Each
/// case is an album matched to release R whose first file is starred and
/// then deleted; the rows and mappings are as the case says.
#[tokio::test]
async fn a_missing_track_does_not_continue_without_proof() {
    struct Case {
        name: &'static str,
        files: usize,
        /// Statements over the album and its rows, run before the delete.
        setup: fn(&str, &[String]) -> String,
        /// The first file is deleted (otherwise it stays: two copies).
        delete: bool,
    }
    let cases = [
        Case {
            name: "one missing and two present",
            files: 3,
            setup: |_, t| {
                [&t[0], &t[1], &t[2]]
                    .iter()
                    .map(|track| map_track(track, Some(REL_R), Some(TRACK_X)))
                    .collect()
            },
            delete: true,
        },
        Case {
            name: "the same recording on another release track",
            files: 2,
            setup: |_, t| {
                map_track(&t[0], Some(REL_R), Some(TRACK_X))
                    + &map_track(&t[1], Some(REL_R), Some("rt-other"))
            },
            delete: true,
        },
        Case {
            name: "a best guess",
            files: 2,
            setup: |album, t| {
                map_track(&t[0], Some(REL_R), Some(TRACK_X))
                    + &map_track(&t[1], Some(REL_R), Some(TRACK_X))
                    + &format!(
                        "INSERT INTO library_album_match_state (local_album_id, state, \
                         reason_code, release_mbid, updated_at) \
                         VALUES ('{album}', 'unconfirmed', 'EDITION_UNCERTAIN', '{REL_R}', 0);"
                    )
            },
            delete: true,
        },
        Case {
            name: "a match naming no release",
            files: 2,
            setup: |album, t| {
                map_track(&t[0], None, Some(TRACK_X))
                    + &map_track(&t[1], None, Some(TRACK_X))
                    + &format!(
                        "UPDATE local_album_external_identities SET release_mbid = NULL \
                         WHERE local_album_id = '{album}';"
                    )
            },
            delete: true,
        },
        Case {
            name: "a custom edition",
            files: 2,
            setup: |album, t| {
                map_track(&t[0], Some(REL_R), Some(TRACK_X))
                    + &map_track(&t[1], Some(REL_R), Some(TRACK_X))
                    + &format!(
                        "INSERT INTO auth_users (id, display_name, role, created_at) \
                         VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
                         INSERT INTO library_custom_edition_manifests (id, local_album_id, \
                         version, release_group_mbid, album_title, album_artist_name, \
                         source_album_revision, input_revision, content_hash, \
                         sealed_by_user_id, sealed_at) VALUES ('m1', '{album}', 1, 'rg-r', \
                         'T', 'A', 1, 'in', 'hash', 'curator', 0); \
                         INSERT INTO library_custom_edition_active (local_album_id, \
                         manifest_id, activated_at) VALUES ('{album}', 'm1', 0);"
                    )
            },
            delete: true,
        },
        Case {
            name: "the old row's mapping naming another release",
            files: 2,
            setup: |_, t| {
                map_track(&t[0], Some("rel-other"), Some(TRACK_X))
                    + &map_track(&t[1], Some(REL_R), Some(TRACK_X))
            },
            delete: true,
        },
        Case {
            name: "the new row's mapping naming another release",
            files: 2,
            setup: |_, t| {
                map_track(&t[0], Some(REL_R), Some(TRACK_X))
                    + &map_track(&t[1], Some("rel-other"), Some(TRACK_X))
            },
            delete: true,
        },
        Case {
            name: "the old row present (two copies)",
            files: 2,
            setup: |_, t| {
                map_track(&t[0], Some(REL_R), Some(TRACK_X))
                    + &map_track(&t[1], Some(REL_R), Some(TRACK_X))
            },
            delete: false,
        },
        Case {
            name: "same names and recording, no release track on the old row",
            files: 2,
            setup: |_, t| {
                map_track(&t[0], Some(REL_R), None) + &map_track(&t[1], Some(REL_R), Some(TRACK_X))
            },
            delete: true,
        },
    ];
    for (index, case) in cases.iter().enumerate() {
        let (rig, album, tracks) =
            continuation_album(&format!("scan-continue-refused-{index}"), case.files).await;
        rig.run(&(case.setup)(&album, &tracks));
        rig.run(&star(&tracks[0]));
        if case.delete {
            rig.lose(&["a/t000.flac".to_owned()]).await;
        } else {
            rig.scan().await;
        }
        assert_eq!(rig.stars_and_aliases(&tracks[0]), (1, 0), "{}", case.name);
        assert_eq!(rig.continuations(), 0, "{}", case.name);
    }

    // The old row on another album row of the same release.
    let mut rig = RegroupRig::new("scan-continue-refused-other-album");
    let (first, second) = ("first".to_owned(), "second".to_owned());
    rig.tag(&["a/t000.flac".to_owned()], &first);
    rig.tag(&["b/t001.flac".to_owned()], &second);
    rig.scan().await;
    let store = rig.coordinator.store();
    let old = store
        .track_at("music", "a/t000.flac")
        .unwrap()
        .expect("indexed");
    let new = store
        .track_at("music", "b/t001.flac")
        .unwrap()
        .expect("indexed");
    for album in [rig.album_of("a/t000.flac"), rig.album_of("b/t001.flac")] {
        rig.run(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', 'rg-r', '{REL_R}', 'manual', 0);"
        ));
    }
    rig.run(
        &(map_track(&old, Some(REL_R), Some(TRACK_X))
            + &map_track(&new, Some(REL_R), Some(TRACK_X))
            + &star(&old)),
    );
    rig.lose(&["a/t000.flac".to_owned()]).await;
    assert_eq!(rig.stars_and_aliases(&old), (1, 0), "another album row");
}

/// Where the proof is there, the missing rows on the release track all
/// continue into the one present row at the end of the scan: two missing
/// rows into one, an excluded row on the same track left as it is, and
/// rows that name no release (an approval or an import wrote them).
#[tokio::test]
async fn missing_tracks_continue_into_the_one_present_row() {
    let (rig, _, t) = continuation_album("scan-continue-two-missing", 3).await;
    for track in &t {
        rig.run(&map_track(track, Some(REL_R), Some(TRACK_X)));
    }
    rig.run(&(star(&t[0]) + &star(&t[1])));
    rig.lose(&["a/t000.flac".to_owned(), "a/t001.flac".to_owned()])
        .await;
    assert_eq!(rig.stars_and_aliases(&t[2]), (1, 2));
    assert_eq!(rig.aliases(), {
        let mut expected = vec![(t[0].clone(), t[2].clone()), (t[1].clone(), t[2].clone())];
        expected.sort();
        expected
    });
    assert_eq!(rig.continuations(), 1);

    let (rig, _, t) = continuation_album("scan-continue-excluded", 3).await;
    for track in &t {
        rig.run(&map_track(track, Some(REL_R), Some(TRACK_X)));
    }
    rig.run(&format!(
        "UPDATE local_tracks SET availability = 'excluded', manual_excluded = 1, \
         excluded_at = 1 WHERE id = '{}';",
        t[2]
    ));
    rig.run(&(star(&t[0]) + &star(&t[2])));
    rig.lose(&["a/t000.flac".to_owned()]).await;
    assert_eq!(rig.aliases(), vec![(t[0].clone(), t[1].clone())]);
    assert_eq!(rig.stars_and_aliases(&t[1]).0, 1);
    assert_eq!(
        rig.stars_and_aliases(&t[2]).0,
        1,
        "the excluded row keeps its own"
    );

    let (rig, _, t) = continuation_album("scan-continue-no-release", 2).await;
    rig.run(
        &(map_track(&t[0], None, Some(TRACK_X))
            + &map_track(&t[1], None, Some(TRACK_X))
            + &star(&t[0])),
    );
    rig.lose(&["a/t000.flac".to_owned()]).await;
    assert_eq!(rig.aliases(), vec![(t[0].clone(), t[1].clone())]);
    assert_eq!(rig.stars_and_aliases(&t[1]).0, 1);
}

/// After a move the old file can come back, go again, or the new one can
/// go. The old row answers for itself again while its file is there and
/// keeps nothing it gave; what it gathers meanwhile moves when it goes
/// again. A chain ends on the last row and no alias names an alias. When
/// the new row goes instead, both ids end on the old row.
#[tokio::test]
async fn continued_tracks_follow_their_files_back_and_forth() {
    let (mut rig, _, t) = continuation_album("scan-continue-back-and-forth", 2).await;
    let (old, new) = (t[0].clone(), t[1].clone());
    rig.run(
        &(map_track(&old, Some(REL_R), Some(TRACK_X))
            + &map_track(&new, Some(REL_R), Some(TRACK_X))
            + &star(&old)),
    );
    let first = ["a/t000.flac".to_owned()];
    rig.lose(&first).await;
    assert_eq!(rig.aliases(), vec![(old.clone(), new.clone())]);

    let before = rig.before.clone();
    rig.tag(&first, &before);
    rig.scan().await;
    assert_eq!(
        rig.coordinator
            .store()
            .track_at("music", "a/t000.flac")
            .unwrap(),
        Some(old.clone()),
        "the old row has its file again"
    );
    assert_eq!(rig.aliases(), vec![], "it answers for itself");
    assert_eq!(rig.stars_and_aliases(&new).0, 1, "what moved stays");
    rig.run(&format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
         artist_name, played_at) VALUES ('back', 'listener', '{old}', 'T', 'A', '2026');"
    ));
    rig.lose(&first).await;
    assert_eq!(rig.aliases(), vec![(old.clone(), new.clone())]);
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_play_history WHERE id = 'back' AND local_track_id = '{new}'"
        )),
        1,
        "the play made while it was back moves"
    );

    // A flip: the old file back again, then the new one deleted. Both ids
    // end on the old row.
    rig.tag(&first, &before);
    rig.scan().await;
    rig.lose(&["a/t001.flac".to_owned()]).await;
    assert_eq!(rig.aliases(), vec![(new.clone(), old.clone())]);
    assert_eq!(rig.stars_and_aliases(&old).0, 1);

    // A chain: O1 into O2, then O2 into N. Both old ids name N.
    let (rig, _, t) = continuation_album("scan-continue-chain", 3).await;
    rig.run(
        &(map_track(&t[0], Some(REL_R), Some(TRACK_X))
            + &map_track(&t[1], Some(REL_R), Some(TRACK_X))
            + &star(&t[0])),
    );
    rig.lose(&["a/t000.flac".to_owned()]).await;
    assert_eq!(rig.aliases(), vec![(t[0].clone(), t[1].clone())]);
    rig.run(&map_track(&t[2], Some(REL_R), Some(TRACK_X)));
    rig.lose(&["a/t001.flac".to_owned()]).await;
    assert_eq!(
        rig.aliases(),
        vec![(t[0].clone(), t[2].clone()), (t[1].clone(), t[2].clone())]
    );
    assert_eq!(rig.stars_and_aliases(&t[2]).0, 1);
    assert_eq!(rig.continuations(), 2);
}

/// A store error while tracks continue leaves the identification seal
/// that started it in place, moves nothing and logs one warning. The next
/// scan's end continues them.
#[tokio::test]
async fn a_store_error_holds_the_continuation_not_the_seal() {
    use droppedneedle_library::identify::models::{AutomaticSeal, DecisionSource, TrackIdentity};

    retag_warnings();
    let (rig, album, t) = continuation_album("scan-continue-store-error", 2).await;
    let (old, new) = (t[0].clone(), t[1].clone());
    rig.run(&format!(
        "UPDATE local_album_external_identities SET decision_source = 'automatic' \
         WHERE local_album_id = '{album}';"
    ));
    rig.run(&(map_track(&old, Some(REL_R), Some(TRACK_X)) + &star(&old)));
    rig.lose(&["a/t000.flac".to_owned()]).await;
    rig.run(
        "CREATE TRIGGER refuse_alias BEFORE INSERT ON local_track_aliases \
         BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
    );
    let identify = SqliteIdentifyStore::new(
        droppedneedle_kernel::db::Db::open_for_tests(&rig.db()).expect("identify store opens"),
        Arc::new(SystemClock),
    );
    assert!(
        identify
            .seal_automatic(&AutomaticSeal {
                local_album_id: album.clone(),
                release_group_mbid: "rg-r".to_owned(),
                release_mbid: Some(REL_R.to_owned()),
                tracks: vec![TrackIdentity {
                    local_track_id: new.clone(),
                    provider: "musicbrainz".to_owned(),
                    recording_mbid: Some("rec-x".to_owned()),
                    release_track_mbid: Some(TRACK_X.to_owned()),
                    decision_source: DecisionSource::Automatic,
                    row_revision: 1,
                }],
                flag: None,
            })
            .unwrap()
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_track_external_identities \
             WHERE local_track_id = '{new}' AND release_track_mbid = '{TRACK_X}'"
        )),
        1,
        "the seal lands"
    );
    assert_eq!(rig.stars_and_aliases(&old), (1, 0), "nothing moves");
    let warned = || {
        retag_warnings()
            .lock()
            .expect("lines")
            .iter()
            .filter(|line| {
                line.contains("tracks not continued")
                    && line.contains(&album)
                    && line.contains(&old)
                    && line.contains(&new)
            })
            .count()
    };
    assert_eq!(warned(), 1);

    rig.run("DROP TRIGGER refuse_alias");
    rig.scan().await;
    assert_eq!(rig.aliases(), vec![(old.clone(), new.clone())]);
    assert_eq!(rig.stars_and_aliases(&new).0, 1);
    assert_eq!(warned(), 1);
}

/// An edition choice that leaves a file which was on the release track as
/// an extra, and places no file on it, moves nothing onto that file: the
/// continuation runs after the extras are cleared.
#[tokio::test]
async fn an_edition_choice_moves_nothing_onto_a_file_it_leaves_out() {
    use droppedneedle_library::operations::models::ReidentificationCandidate;

    let (rig, album, t) = continuation_album("scan-continue-choice-extra", 3).await;
    let (old, extra, placed) = (t[0].clone(), t[1].clone(), t[2].clone());
    rig.run(&(map_track(&old, Some(REL_R), Some(TRACK_X)) + &star(&old)));
    rig.lose(&["a/t000.flac".to_owned()]).await;
    // A stale mapping on the file the edition is about to leave out.
    rig.run(&map_track(&extra, Some(REL_R), Some(TRACK_X)));

    let candidate: ReidentificationCandidate = serde_json::from_value(serde_json::json!({
        "candidate_key": "c1",
        "automatic_safe": true,
        "evidence": {
            "candidate_key": "c1",
            "release_group_mbid": "rg-r",
            "release_mbid": REL_R,
            "album_title": "T",
            "album_artist_name": "Artist",
            "track_evidence": [{
                "local_track_id": placed,
                "classification": "supported",
                "evidence_kinds": [],
                "recording_mbid": "rec-y",
                "release_track_mbid": "rt-y",
            }],
            "score": 1.0,
            "reason_code": "SUPPORTED",
        },
        "artist_mbid": null,
        "release_type": null,
        "release_date": null,
        "local_album_title": "T",
        "local_album_artist_name": "Artist",
        "album_title_classification": "supported",
        "album_artist_classification": "supported",
        "tracks": [{"local_track_id": placed, "title": "Y", "disc_number": 1, "position": 2}],
        "unmatched_expected_tracks": [],
    }))
    .expect("candidate");
    let mut db = droppedneedle_kernel::db::open_connection(&rig.db()).expect("db opens");
    let tx = db.transaction().expect("transaction");
    let choice =
        droppedneedle_library::operations::choice::choose(&tx, &album, &candidate, None, 1.0)
            .expect("edition chosen");
    tx.commit().expect("committed");
    assert_eq!(choice.extra_track_ids, vec![extra.clone()]);
    assert_eq!(rig.stars_and_aliases(&old), (1, 0));
    assert_eq!(rig.stars_and_aliases(&extra).0, 0);
}

// ---------------------------------------------------------------------------
// An album joined to the album whose files replaced it on its edition.
// ---------------------------------------------------------------------------

/// An album of `count` files matched by a person to [`REL_R`], file `n`
/// mapped to the release track `rt-n` and the first starred. Returns the
/// rig, the album and its rows.
async fn mapped_album(name: &str, count: usize) -> (RegroupRig, String, Vec<String>) {
    let (rig, album, rows) = continuation_album(name, count).await;
    for (n, row) in rows.iter().enumerate() {
        rig.run(&map_track(row, Some(REL_R), Some(&format!("rt-{n}"))));
    }
    rig.run(&star(&rows[0]));
    (rig, album, rows)
}

/// The album matched by the app to `release`, as identification seals it.
fn on_release(album: &str, release: &str) -> String {
    format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at) \
         VALUES ('{album}', 'musicbrainz', 'rg-r', '{release}', 'automatic', 0);"
    )
}

/// A best guess of [`REL_R`] on the album.
fn best_guess(album: &str) -> String {
    format!(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         release_mbid, updated_at) VALUES ('{album}', 'unconfirmed', 'EDITION_UNCERTAIN', \
         '{REL_R}', 0);"
    )
}

impl RegroupRig {
    /// Retag the files `a/tNNN.flac` numbered `numbers` to the album
    /// `album` and rename them into `folder` in one go, and scan. Returns
    /// the new rows.
    async fn replace(&mut self, numbers: &[usize], folder: &str, album: &str) -> Vec<String> {
        let files: Vec<String> = numbers
            .iter()
            .map(|n| format!("{folder}/t{n:03}.flac"))
            .collect();
        self.tag(&files, album);
        for n in numbers {
            std::fs::remove_file(self.root.join(format!("a/t{n:03}.flac"))).expect("removed");
        }
        self.scan().await;
        self.rows_at(&files)
    }

    fn rows_at(&self, files: &[String]) -> Vec<String> {
        files
            .iter()
            .map(|file| {
                self.coordinator
                    .store()
                    .track_at("music", file)
                    .unwrap()
                    .expect("indexed")
            })
            .collect()
    }

    /// Match the album to `release` with the rows given as `(row, n)`
    /// mapped to `rt-n`, as identification would.
    fn match_rows(&self, album: &str, release: &str, rows: &[(&str, usize)]) {
        self.run(&on_release(album, release));
        for (row, n) in rows {
            self.run(&map_track(row, Some(release), Some(&format!("rt-{n}"))));
        }
    }

    /// The scan runs on record, oldest first.
    fn runs(&self) -> Vec<String> {
        let total = self.count("SELECT COUNT(*) FROM library_scan_runs");
        (0..total)
            .map(|n| {
                self.text(&format!(
                    "SELECT id FROM library_scan_runs ORDER BY started_at LIMIT 1 OFFSET {n}"
                ))
                .expect("run")
            })
            .collect()
    }

    /// Scan only `folder`, as a scan of one folder does.
    async fn scan_folder(&self, folder: &str) {
        let mut scope = ScanScope::root("music", &self.root.to_string_lossy(), "rev-1");
        scope.relative_path = folder.to_owned();
        let mut request = request(&self.root);
        request.scopes = vec![scope];
        self.coordinator.request_run(&request).expect("requested");
        let paths = HashMap::from([("music".to_owned(), self.root.clone())]);
        let run = self.coordinator.run_once(&paths).await.expect("scan runs");
        assert_eq!(run.state.scan_state(), ScanState::Completed);
    }

    fn open_pairs(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM library_retag_pairs WHERE state = 'open'")
    }

    /// The old album is merged into the new one and the starred old row's
    /// history is on the new row on its release track.
    fn assert_joined(&self, old: &str, new: &str, starred: (&str, &str), case: &str) {
        assert_eq!(self.retired_into(old).as_deref(), Some(new), "{case}");
        assert_eq!(self.stars_and_aliases(starred.0).0, 0, "{case}");
        assert_eq!(self.stars_and_aliases(starred.1).0, 1, "{case}");
        assert_eq!(self.open_pairs(), 0, "{case}");
        assert_eq!(
            self.count(&format!(
                "SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = 'merge_album' \
                 AND reason_code = 'RETAG_JOINED_SAME_RELEASE' AND local_album_id = '{new}'"
            )),
            1,
            "{case}"
        );
    }

    /// Nothing joined, nothing recorded, the star where it was.
    fn assert_apart(&self, old: &str, starred: &str, case: &str) {
        assert_eq!(self.retired_into(old), None, "{case}");
        assert_eq!(self.open_pairs(), 0, "{case}");
        assert_eq!(self.stars_and_aliases(starred).0, 1, "{case}");
    }
}

/// How one brief departs from the plain story: an album of two files
/// matched to [`REL_R`], mapped and its first file starred; both files
/// retagged to a new name and renamed into `b/` in one scan; the new album
/// then matched to [`REL_R`] with its rows mapped like the old ones; one
/// more scan.
#[derive(Clone, Copy, Default)]
struct Story {
    /// The second file stays where it was.
    keep_one: bool,
    /// The new album's release, when it is not [`REL_R`].
    new_release: Option<&'static str>,
    guess_on_old: bool,
    guess_on_new: bool,
    old_names_no_release: bool,
    custom_on_old: bool,
    /// The old rows' mappings name another release.
    old_rows_elsewhere: bool,
    /// A pair naming the old album that a person dismissed.
    dismissed: bool,
    /// A person removed the old rows in the app.
    removed_in_app: bool,
    /// The old rows went missing between two scans.
    missing_between_scans: bool,
    no_missing_since: bool,
    /// The scan that read the new files stopped before it marked the old
    /// rows missing, so the next one marks them.
    interrupted: bool,
}

/// The story told: the rig, the old album, the new one, the starred old
/// row and the new row on its release track.
async fn tell(name: &str, story: Story) -> (RegroupRig, String, String, String, String) {
    let (mut rig, old, old_rows) = mapped_album(name, 2).await;
    if story.old_rows_elsewhere {
        for (n, row) in old_rows.iter().enumerate() {
            rig.run(&map_track(row, Some("rel-other"), Some(&format!("rt-{n}"))));
        }
    }
    if story.guess_on_old {
        rig.run(&best_guess(&old));
    }
    if story.old_names_no_release {
        rig.run(&format!(
            "UPDATE local_album_external_identities SET release_mbid = NULL \
             WHERE local_album_id = '{old}';"
        ));
    }
    if story.custom_on_old {
        rig.run(&format!(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
             INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
             release_group_mbid, album_title, album_artist_name, source_album_revision, \
             input_revision, content_hash, sealed_by_user_id, sealed_at) VALUES ('m1', \
             '{old}', 1, 'rg-r', 'T', 'A', 1, 'in', 'hash', 'curator', 0); \
             INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
             activated_at) VALUES ('{old}', 'm1', 0);"
        ));
    }
    let numbers: &[usize] = if story.keep_one { &[0] } else { &[0, 1] };
    let after = rig.after.clone();
    let new_rows = rig.replace(numbers, "b", &after).await;
    let new = rig.album_of("b/t000.flac");
    assert_ne!(new, old, "{name}: a new album row");
    let runs = rig.runs();
    if story.interrupted {
        rig.run(&format!(
            "UPDATE local_tracks SET availability = 'indexed', missing_since = NULL \
             WHERE local_album_id = '{old}'; \
             UPDATE library_scan_runs SET state = 'cancelled' WHERE id = '{}';",
            runs[1]
        ));
    }
    let rows: Vec<(&str, usize)> = new_rows.iter().map(String::as_str).zip(0..).collect();
    rig.match_rows(&new, story.new_release.unwrap_or(REL_R), &rows);
    if story.guess_on_new {
        rig.run(&best_guess(&new));
    }
    if story.dismissed {
        rig.run(&format!(
            "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
             recorded_at, updated_at) VALUES ('{old}', '{new}', 'tracks_elsewhere', \
             'dismissed', 0, 0);"
        ));
    }
    for row in old_rows.iter().filter(|_| story.removed_in_app) {
        rig.run(&format!(
            "INSERT INTO library_catalog_actions (id, action_kind, local_album_id, \
             local_track_id, before_json, after_json, reason_code, created_at) \
             VALUES ('removed-{row}', 'remove_track', '{old}', '{row}', '{{}}', '{{}}', \
             'FILE_RECYCLED', 0);"
        ));
    }
    if story.missing_between_scans {
        rig.run(&format!(
            "UPDATE local_tracks SET missing_since = \
             ((SELECT terminal_at FROM library_scan_runs WHERE id = '{}') \
             + (SELECT started_at FROM library_scan_runs WHERE id = '{}')) / 2 \
             WHERE local_album_id = '{old}' AND availability = 'missing';",
            runs[0], runs[1]
        ));
    }
    if story.no_missing_since {
        rig.run(&format!(
            "UPDATE local_tracks SET missing_since = NULL WHERE local_album_id = '{old}';"
        ));
    }
    rig.scan().await;
    if story.interrupted {
        // The new files were there before the scan that marked the old
        // ones missing: the join waits one more scan.
        assert_eq!(rig.retired_into(&old), None, "{name}: waits a scan");
        rig.scan().await;
    }
    (rig, old, new, old_rows[0].clone(), new_rows[0].clone())
}

/// An album retagged to a new name and renamed in one scan, whose new
/// album is matched to its edition afterwards, is joined at the end of the
/// next scan with no file changed: the old album retired into the new one
/// and the star on the new track. So is one whose scan stopped after it
/// read the new files and before it marked the old ones missing: the next
/// scan marks them and joins.
#[tokio::test]
async fn an_album_replaced_on_its_edition_is_joined_at_the_run_end() {
    let (rig, old, new, o, n) = tell("join-run-end", Story::default()).await;
    rig.assert_joined(&old, &new, (&o, &n), "plain");
    assert_eq!(rig.continuations(), 1);

    let interrupted = Story {
        interrupted: true,
        ..Story::default()
    };
    let (rig, old, new, o, n) = tell("join-run-end-interrupted", interrupted).await;
    rig.assert_joined(&old, &new, (&o, &n), "interrupted");
}

/// Without proof that the new album's files replaced the old album's,
/// nothing is joined or recorded and the star stays on the old track. The
/// plain story, each condition lifted, joins.
#[tokio::test]
async fn an_album_is_not_joined_without_proof_it_was_replaced() {
    let (rig, old, new, o, n) = tell("join-refused-twin", Story::default()).await;
    rig.assert_joined(&old, &new, (&o, &n), "the plain story");

    let refusals = [
        (
            "the old album holds a present file",
            Story {
                keep_one: true,
                ..Story::default()
            },
        ),
        (
            "the new album on another release",
            Story {
                new_release: Some("rel-other"),
                ..Story::default()
            },
        ),
        (
            "a best guess on the old album",
            Story {
                guess_on_old: true,
                ..Story::default()
            },
        ),
        (
            "a best guess on the new album",
            Story {
                guess_on_new: true,
                ..Story::default()
            },
        ),
        (
            "the old album's match naming no release",
            Story {
                old_names_no_release: true,
                ..Story::default()
            },
        ),
        (
            "a custom edition on the old album",
            Story {
                custom_on_old: true,
                ..Story::default()
            },
        ),
        (
            "the old rows' mappings naming another release",
            Story {
                old_rows_elsewhere: true,
                ..Story::default()
            },
        ),
        (
            "a dismissed pair naming the old album",
            Story {
                dismissed: true,
                ..Story::default()
            },
        ),
        (
            "removed by a person in the app",
            Story {
                removed_in_app: true,
                missing_between_scans: true,
                ..Story::default()
            },
        ),
        (
            "removed by a person while a scan ran",
            Story {
                removed_in_app: true,
                ..Story::default()
            },
        ),
        (
            "missing with no time",
            Story {
                no_missing_since: true,
                ..Story::default()
            },
        ),
    ];
    for (index, (case, story)) in refusals.into_iter().enumerate() {
        let (rig, old, new, o, _) = tell(&format!("join-refused-{index}"), story).await;
        if story.custom_on_old {
            // A custom edition is hands-off: its replacement is held and
            // offered to a person instead.
            assert_eq!(rig.hands_off_pairs(), vec![pair(&old, &new)], "{case}");
            rig.run("DELETE FROM library_retag_pairs WHERE reason = 'replaced_hands_off';");
        }
        rig.assert_apart(&old, &o, case);
    }

    // Two copies a finished scan saw side by side, the old one deleted
    // since. Joined when that scan did not finish, or did not walk the
    // folder to the end; not when the history before the new copy is gone.
    for (index, (case, lift, joined)) in [
        ("two copies seen side by side", "", false),
        (
            "the scan that saw both stopped",
            "UPDATE library_scan_runs SET state = 'cancelled' WHERE id = '{run}';",
            true,
        ),
        (
            "the scan that saw both did not walk the folder",
            "UPDATE library_scan_run_scopes SET discovery_state = 'pending' \
             WHERE run_id = '{run}';",
            true,
        ),
        (
            "history pruned back past the new copy",
            "DELETE FROM library_scan_runs WHERE id IN ('{run}', '{first}');",
            false,
        ),
        (
            "history pruned to the scan that saw both",
            "DELETE FROM library_scan_runs WHERE id = '{run}';",
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut rig, old, old_rows) = mapped_album(&format!("join-copies-{index}"), 2).await;
        let copies = vec!["b/t000.flac".to_owned(), "b/t001.flac".to_owned()];
        let after = rig.after.clone();
        rig.tag(&copies, &after);
        rig.scan().await;
        let runs = rig.runs();
        rig.lose(&["a/t000.flac".to_owned(), "a/t001.flac".to_owned()])
            .await;
        rig.run(&lift.replace("{run}", &runs[1]).replace("{first}", &runs[0]));
        let new = rig.album_of("b/t000.flac");
        let new_rows = rig.rows_at(&copies);
        rig.match_rows(&new, REL_R, &[(&new_rows[0], 0), (&new_rows[1], 1)]);
        rig.scan().await;
        if joined {
            rig.assert_joined(&old, &new, (&old_rows[0], &new_rows[0]), case);
        } else {
            rig.assert_apart(&old, &old_rows[0], case);
        }
    }

    // Two copies seen side by side, a track added to the one kept and the
    // other deleted in one go: the track seen in both copies stops the
    // join, though the others were replaced.
    for (index, (case, lift, joined)) in [
        ("a copy completed and the other deleted", false, false),
        ("the scan that saw both stopped", true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut rig, old, old_rows) = mapped_album(&format!("join-veto-{index}"), 3).await;
        let after = rig.after.clone();
        rig.tag(&["b/t000.flac".to_owned()], &after);
        rig.scan().await;
        let runs = rig.runs();
        std::fs::remove_file(rig.root.join("a/t000.flac")).expect("removed");
        let new_rows = rig.replace(&[1, 2], "b", &after).await;
        if lift {
            rig.run(&format!(
                "UPDATE library_scan_runs SET state = 'cancelled' WHERE id = '{}';",
                runs[1]
            ));
        }
        let new = rig.album_of("b/t000.flac");
        let first = rig.rows_at(&["b/t000.flac".to_owned()]);
        rig.match_rows(
            &new,
            REL_R,
            &[(&first[0], 0), (&new_rows[0], 1), (&new_rows[1], 2)],
        );
        rig.scan().await;
        if joined {
            rig.assert_joined(&old, &new, (&old_rows[0], &first[0]), case);
        } else {
            rig.assert_apart(&old, &old_rows[0], case);
        }
    }

    // The old album deleted and seen gone by a finished scan, then the
    // same edition added: a deletion and a new arrival, not a retag.
    for (index, (case, lift, joined)) in [
        ("deleted, then the edition added", false, false),
        ("the scan that saw it gone stopped", true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut rig, old, old_rows) = mapped_album(&format!("join-late-{index}"), 2).await;
        // Another album stays, so the deletion is not taken for a share
        // that went away.
        rig.tag(&["z/t000.flac".to_owned()], "join-late bystander");
        rig.scan().await;
        rig.lose(&["a/t000.flac".to_owned(), "a/t001.flac".to_owned()])
            .await;
        let lost = rig.runs().pop().expect("the scan that saw it gone");
        let files = vec!["b/t000.flac".to_owned(), "b/t001.flac".to_owned()];
        let after = rig.after.clone();
        rig.tag(&files, &after);
        rig.scan().await;
        if lift {
            rig.run(&format!(
                "UPDATE library_scan_runs SET state = 'cancelled' WHERE id = '{lost}';"
            ));
        }
        let new = rig.album_of("b/t000.flac");
        let new_rows = rig.rows_at(&files);
        rig.match_rows(&new, REL_R, &[(&new_rows[0], 0), (&new_rows[1], 1)]);
        rig.scan().await;
        if joined {
            rig.assert_joined(&old, &new, (&old_rows[0], &new_rows[0]), case);
        } else {
            rig.assert_apart(&old, &old_rows[0], case);
        }
    }

    // A copy of the edition added and scanned on its own, then the old
    // album's share drops out: the join waits a scan, and the share back
    // in time keeps the two albums apart.
    let case = "a share that drops out after a copy arrived";
    let (mut rig, old, old_rows) = mapped_album("join-share", 2).await;
    let files = vec!["b/t000.flac".to_owned(), "b/t001.flac".to_owned()];
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.scan_folder("b").await;
    let new = rig.album_of("b/t000.flac");
    let new_rows = rig.rows_at(&files);
    rig.match_rows(&new, REL_R, &[(&new_rows[0], 0), (&new_rows[1], 1)]);
    let away = rig.scratch.join("away");
    std::fs::rename(rig.root.join("a"), &away).expect("share gone");
    rig.scan().await;
    rig.assert_apart(&old, &old_rows[0], case);
    std::fs::rename(&away, rig.root.join("a")).expect("share back");
    rig.scan().await;
    rig.assert_apart(&old, &old_rows[0], case);
    assert_eq!(rig.album_of("a/t000.flac"), old, "{case}");
}

impl RegroupRig {
    /// Record `a/` as a drive or share mounted inside the library folder,
    /// as a scan that found it on its own filesystem would.
    fn mount_a(&self) {
        self.run("INSERT INTO library_scan_mounts (root_id, relative_path) VALUES ('music', 'a');");
    }

    /// Empty `a/` of the old album's files, leaving the folder.
    fn empty_a(&self, count: usize) {
        for n in 0..count {
            std::fs::remove_file(self.root.join(format!("a/t{n:03}.flac"))).expect("removed");
        }
    }

    /// The latest scan held `a/` back, and the old album is untouched: its
    /// rows indexed at their paths with their ids, nothing joined or
    /// recorded, the star on its first row.
    fn assert_share_kept(&self, old: &str, rows: &[String], case: &str) {
        self.assert_apart(old, &rows[0], case);
        for (n, row) in rows.iter().enumerate() {
            assert_eq!(
                self.coordinator
                    .store()
                    .track_at("music", &format!("a/t{n:03}.flac"))
                    .unwrap()
                    .as_ref(),
                Some(row),
                "{case}: the old row stays indexed at its path"
            );
        }
        let latest = self.runs().pop().expect("a scan ran");
        assert_eq!(
            self.count(&format!(
                "SELECT COUNT(*) FROM library_scan_failures WHERE run_id = '{latest}' \
                 AND root_id = 'music' AND relative_path = 'a' \
                 AND failure_code = 'MASS_MISSING_GUARD'"
            )),
            1,
            "{case}: the share is held back"
        );
    }
}

/// A drive or share mounted inside a library folder that a scan finds
/// empty is held back: nothing under it is marked missing or moved, and its
/// album is never joined into a copy added elsewhere, whether the copy came
/// in the same scan, came first, or carries the old album's own tags. Back
/// again, the share lets the hold go and the two albums stay apart. With a
/// file left on the share it was not dropped, and the album is joined.
#[tokio::test]
async fn a_share_that_goes_empty_keeps_its_album() {
    let case = "a copy in the scan the share drops";
    let (mut rig, old, old_rows) = mapped_album("share-same-scan", 2).await;
    rig.mount_a();
    let after = rig.after.clone();
    let new_rows = rig.replace(&[0, 1], "b", &after).await;
    rig.assert_share_kept(&old, &old_rows, case);
    let new = rig.album_of("b/t000.flac");
    rig.match_rows(&new, REL_R, &[(&new_rows[0], 0), (&new_rows[1], 1)]);
    rig.scan().await;
    rig.assert_share_kept(&old, &old_rows, case);
    let first = &rig.runs()[1];
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_scan_held_scopes WHERE root_id = 'music' \
             AND relative_path = 'a' AND held_at = (SELECT CAST(started_at * 1000 AS INTEGER) \
             FROM library_scan_runs WHERE id = '{first}')"
        )),
        1,
        "{case}: held since the first scan"
    );
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM library_scan_held_scopes"),
        1
    );
    let before = rig.before.clone();
    rig.tag(
        &["a/t000.flac".to_owned(), "a/t001.flac".to_owned()],
        &before,
    );
    rig.scan().await;
    let latest = rig.runs().pop().expect("a scan ran");
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_scan_failures WHERE run_id = '{latest}' \
             AND failure_code = 'MASS_MISSING_GUARD'"
        )),
        0,
        "{case}: back again"
    );
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM library_scan_held_scopes"),
        0
    );
    rig.assert_apart(&old, &old_rows[0], case);
    assert_eq!(rig.album_of("a/t000.flac"), old, "{case}");
    assert_eq!(rig.album_of("b/t000.flac"), new, "{case}");

    let case = "a copy that came first";
    let (mut rig, old, old_rows) = mapped_album("share-copy-first", 2).await;
    rig.mount_a();
    let files = vec!["b/t000.flac".to_owned(), "b/t001.flac".to_owned()];
    let after = rig.after.clone();
    rig.tag(&files, &after);
    rig.scan_folder("b").await;
    let new = rig.album_of("b/t000.flac");
    let new_rows = rig.rows_at(&files);
    rig.match_rows(&new, REL_R, &[(&new_rows[0], 0), (&new_rows[1], 1)]);
    rig.empty_a(2);
    rig.scan().await;
    rig.assert_share_kept(&old, &old_rows, case);
    rig.scan().await;
    rig.assert_share_kept(&old, &old_rows, case);

    let case = "a copy with the old album's own tags";
    let (mut rig, old, old_rows) = mapped_album("share-own-tags", 2).await;
    rig.mount_a();
    let before = rig.before.clone();
    rig.tag(&files, &before);
    rig.empty_a(2);
    rig.scan().await;
    for copy in rig.rows_at(&files) {
        assert!(!old_rows.contains(&copy), "{case}: the copy is a new track");
    }
    rig.assert_share_kept(&old, &old_rows, case);

    let case = "a file left on the share";
    let (mut rig, old, old_rows) = mapped_album("share-up", 2).await;
    rig.mount_a();
    rig.tag(&["a/z000.flac".to_owned()], "share-up bystander");
    let after = rig.after.clone();
    let new_rows = rig.replace(&[0, 1], "b", &after).await;
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = '{old}' \
             AND availability = 'missing'"
        )),
        2,
        "{case}: marked missing"
    );
    let new = rig.album_of("b/t000.flac");
    rig.match_rows(&new, REL_R, &[(&new_rows[0], 0), (&new_rows[1], 1)]);
    rig.scan().await;
    rig.assert_joined(&old, &new, (&old_rows[0], &new_rows[0]), case);
}

/// A join waits as a follow waits. A track of the old album deleted in the
/// same go holds it for a scan; when no more tracks were replaced than
/// stayed, the rest must have been gone a day, the replaced rows aside.
#[tokio::test]
async fn a_join_waits_for_the_rest_of_the_old_album() {
    use droppedneedle_library::identify::models::{AutomaticSeal, DecisionSource, TrackIdentity};

    let (mut rig, old, old_rows) = mapped_album("join-wait-held", 3).await;
    rig.run(&format!(
        "DELETE FROM local_track_external_identities WHERE local_track_id = '{}';",
        old_rows[2]
    ));
    std::fs::remove_file(rig.root.join("a/t002.flac")).expect("removed");
    let after = rig.after.clone();
    let new_rows = rig.replace(&[0, 1], "b", &after).await;
    let new = rig.album_of("b/t000.flac");
    let identify = SqliteIdentifyStore::new(
        droppedneedle_kernel::db::Db::open_for_tests(&rig.db()).expect("identify store opens"),
        Arc::new(SystemClock),
    );
    assert!(
        identify
            .seal_automatic(&AutomaticSeal {
                local_album_id: new.clone(),
                release_group_mbid: "rg-r".to_owned(),
                release_mbid: Some(REL_R.to_owned()),
                tracks: new_rows
                    .iter()
                    .enumerate()
                    .map(|(n, row)| TrackIdentity {
                        local_track_id: row.clone(),
                        provider: "musicbrainz".to_owned(),
                        recording_mbid: Some("rec-x".to_owned()),
                        release_track_mbid: Some(format!("rt-{n}")),
                        decision_source: DecisionSource::Automatic,
                        row_revision: 1,
                    })
                    .collect(),
                flag: None,
            })
            .unwrap()
    );
    rig.assert_apart(&old, &old_rows[0], "a track gone in the latest scan");
    rig.scan().await;
    rig.assert_joined(&old, &new, (&old_rows[0], &new_rows[0]), "a scan later");

    let (mut rig, old, old_rows) = mapped_album("join-wait-day", 4).await;
    for n in 1..4 {
        std::fs::remove_file(rig.root.join(format!("a/t{n:03}.flac"))).expect("removed");
    }
    let after = rig.after.clone();
    let new_rows = rig.replace(&[0], "b", &after).await;
    let new = rig.album_of("b/t000.flac");
    rig.match_rows(&new, REL_R, &[(&new_rows[0], 0)]);
    rig.scan().await;
    rig.assert_apart(&old, &old_rows[0], "three gone under a day");
    rig.run(&format!(
        "UPDATE local_tracks SET missing_since = missing_since - 86400 \
         WHERE local_album_id = '{old}' AND id <> '{}';",
        old_rows[0]
    ));
    rig.scan().await;
    rig.assert_joined(
        &old,
        &new,
        (&old_rows[0], &new_rows[0]),
        "the rest a day gone",
    );
}

/// An album whose tracks were replaced on two new albums is never merged
/// by the app: each pair is recorded once for a person, who merges one.
#[tokio::test]
async fn an_album_replaced_on_two_albums_is_left_to_a_person() {
    use droppedneedle_library::corrections::models::PairJoinRequest;

    let (mut rig, old, old_rows) = mapped_album("join-split", 2).await;
    rig.tag(&["b/t000.flac".to_owned()], "join-split one");
    rig.tag(&["c/t001.flac".to_owned()], "join-split two");
    for n in 0..2 {
        std::fs::remove_file(rig.root.join(format!("a/t{n:03}.flac"))).expect("removed");
    }
    rig.scan().await;
    let (one, two) = (rig.album_of("b/t000.flac"), rig.album_of("c/t001.flac"));
    let rows = rig.rows_at(&["b/t000.flac".to_owned(), "c/t001.flac".to_owned()]);
    rig.match_rows(&one, REL_R, &[(&rows[0], 0)]);
    rig.match_rows(&two, REL_R, &[(&rows[1], 1)]);
    rig.scan().await;
    assert_eq!(rig.retired_into(&old), None);
    let pair = |new: &str| {
        (
            old.clone(),
            new.to_owned(),
            "tracks_elsewhere".to_owned(),
            "open".to_owned(),
        )
    };
    let mut expected = vec![pair(&one), pair(&two)];
    expected.sort();
    let pairs = || {
        let mut pairs = retag_pairs(&rig);
        pairs.sort();
        pairs
    };
    assert_eq!(pairs(), expected);
    let recorded_at = rig.count("SELECT SUM(updated_at) FROM library_retag_pairs");
    rig.scan().await;
    assert_eq!(pairs(), expected, "recorded once");
    assert_eq!(
        rig.count("SELECT SUM(updated_at) FROM library_retag_pairs"),
        recorded_at
    );
    assert_eq!(rig.stars_and_aliases(&old_rows[0]).0, 1);

    rig.run(PERSON);
    let db = droppedneedle_kernel::db::Db::open_for_tests(&rig.db()).expect("database opens");
    let platform = TestPlatform::on(&db, &rig.scratch)
        .settings(config_store(&rig.scratch))
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let corrections = crate::common::library::services(&library).corrections;
    let listed = corrections.retag_pairs().expect("pairs listed");
    assert_eq!(listed.total, 2);
    let revision = |album: &str| {
        listed
            .pairs
            .iter()
            .flat_map(|pair| [&pair.old_album, &pair.new_album])
            .find(|side| side.id == album)
            .expect("listed")
            .identity_revision
    };
    corrections
        .join_retag_pair(&PairJoinRequest {
            old_album_id: old.clone(),
            new_album_id: one.clone(),
            keep_edition_of: None,
            expected_identity_revisions: BTreeMap::from([
                (old.clone(), revision(&old)),
                (one.clone(), revision(&one)),
            ]),
            actor: "u1".to_owned(),
        })
        .expect("joined");
    assert_eq!(rig.retired_into(&old), Some(one));
    assert_eq!(corrections.retag_pairs().expect("pairs listed").total, 0);
}

// ---------------------------------------------------------------------------
// A scanned file placed on its album's edition by its own ids.
// ---------------------------------------------------------------------------

/// The MusicBrainz ids and numbers one file carries.
#[derive(Clone, Default)]
struct FileIds {
    release: String,
    release_track: String,
    recording: String,
    disc: u32,
    track: u32,
}

fn ids(release: &str, release_track: &str, recording: &str, disc: u32, track: u32) -> FileIds {
    FileIds {
        release: release.to_owned(),
        release_track: release_track.to_owned(),
        recording: recording.to_owned(),
        disc,
        track,
    }
}

/// The edition the placement briefs' albums are matched to, its merged
/// old id and another release.
const REL_P: &str = "rel-p";
const REL_P_OLD: &str = "rel-p-old";
const REL_Q: &str = "rel-q";

/// A file on track `n` of [`REL_P`]'s first disc: `p-rt-n`, `rec-n`.
fn on_p(n: u32) -> FileIds {
    ids(REL_P, &format!("p-rt-{n}"), &format!("rec-{n}"), 1, n)
}

/// [`REL_P`]'s document: `p-rt-1` and `p-rt-2` on disc 1, `p-rt-3` first
/// on disc 2; recordings `rec-1` to `rec-3`. MusicBrainz merged
/// [`REL_P_OLD`] into it.
fn edition_p() -> droppedneedle_library::matching::Release {
    use droppedneedle_library::matching::{Release, ReleaseTrack};
    let track = |n: u32, disc: u32, position: u32| ReleaseTrack {
        id: format!("p-rt-{n}"),
        recording_id: format!("rec-{n}"),
        title: format!("Track {n}"),
        disc,
        position,
        absolute_position: n,
        ..ReleaseTrack::default()
    };
    Release {
        id: REL_P.to_owned(),
        release_group_id: "rg-p".to_owned(),
        title: "P".to_owned(),
        tracks: vec![track(1, 1, 1), track(2, 1, 2), track(3, 2, 1)],
        old_ids: vec![REL_P_OLD.to_owned()],
        ..Release::default()
    }
}

/// A person's identity on `release_track` of `release` with `recording`,
/// at the disc and position given.
fn map_on(
    track: &str,
    release: &str,
    release_track: &str,
    recording: &str,
    place: Option<(u32, u32)>,
) -> String {
    let (disc, position) = place.map_or(("NULL".to_owned(), "NULL".to_owned()), |(d, p)| {
        (d.to_string(), p.to_string())
    });
    format!(
        "INSERT OR REPLACE INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, medium_position, \
         release_track_position, decision_source, selected_at) VALUES ('{track}', \
         'musicbrainz', '{recording}', '{release}', '{release_track}', {disc}, {position}, \
         'manual', 0);"
    )
}

impl RegroupRig {
    /// Write `files` with these ids, tagged with the album `album`.
    fn plant_ids(&mut self, files: &[(String, FileIds)], album: &str) {
        for (file, ids) in files {
            self.reader
                .ids
                .lock()
                .expect("ids")
                .insert(file.clone(), ids.clone());
        }
        let names: Vec<String> = files.iter().map(|(file, _)| file.clone()).collect();
        self.tag(&names, album);
    }

    fn store_release(&self, release: &droppedneedle_library::matching::Release) {
        SqliteIdentifyStore::new(
            droppedneedle_kernel::db::Db::open_for_tests(&self.db()).expect("identify store opens"),
            Arc::new(SystemClock),
        )
        .save_release(release, droppedneedle_kernel::db::Lane::Background)
        .unwrap();
    }

    /// The album matched to `release` with `source`, chosen at `at`.
    fn choose(&self, album: &str, release: &str, source: &str, at: f64) {
        self.run(&format!(
            "INSERT OR REPLACE INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', 'rg-p', '{release}', '{source}', {at});"
        ));
    }

    /// The track's identity: release track, release, recording, source,
    /// disc, position and revision.
    fn identity(&self, track: &str) -> Option<String> {
        self.text(&format!(
            "SELECT release_track_mbid || '|' || COALESCE(release_mbid, '') || '|' || \
             recording_mbid || '|' || decision_source || '|' || \
             COALESCE(medium_position, '') || '|' || COALESCE(release_track_position, '') \
             || '|' || row_revision FROM local_track_external_identities \
             WHERE local_track_id = '{track}'"
        ))
    }

    fn release_track_of(&self, track: &str) -> Option<String> {
        self.text(&format!(
            "SELECT release_track_mbid FROM local_track_external_identities \
             WHERE local_track_id = '{track}'"
        ))
    }

    fn mark_of(&self, album: &str) -> Option<String> {
        self.text(&format!(
            "SELECT cause FROM library_album_takeovers WHERE local_album_id = '{album}'"
        ))
    }

    /// Marks, stamps and aliases anywhere.
    fn marks_stamps_aliases(&self) -> (i64, i64, i64) {
        (
            self.count("SELECT COUNT(*) FROM library_album_takeovers"),
            self.count(
                "SELECT COUNT(*) FROM library_management_automatic_queue \
                 WHERE retag_followed_at IS NOT NULL",
            ),
            self.count("SELECT COUNT(*) FROM local_track_aliases"),
        )
    }
}

/// A match state on the album.
fn match_state_row(album: &str, state: &str, code: &str) -> String {
    format!(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         release_mbid, updated_at) VALUES ('{album}', '{state}', '{code}', '{REL_P}', 0);"
    )
}

/// What a scan adds to an album with an edition is placed on the track its
/// own ids name, and only then. Each case is an album a person matched to
/// [`REL_P`], whose document is on file unless the case says otherwise,
/// with files carrying the ids given; the last file is the one looked at,
/// after one more scan (or the scan that finds the files `lose` names
/// gone). A refusal leaves its identity as it was, marks nothing and
/// makes no alias.
#[tokio::test]
async fn a_scanned_file_is_placed_by_its_own_ids() {
    struct Case {
        name: &'static str,
        files: Vec<FileIds>,
        /// Statements over the album and its rows, before the last scan.
        setup: fn(&str, &[String]) -> String,
        document: bool,
        lose: &'static [usize],
        /// The release track the last file ends on; `None` for a refusal.
        placed: Option<&'static str>,
    }
    fn none(_: &str, _: &[String]) -> String {
        String::new()
    }
    let case = |name, files, placed| Case {
        name,
        files,
        setup: none,
        document: true,
        lose: &[],
        placed,
    };
    let cases = vec![
        case(
            "release track, no release id",
            vec![ids("", "p-rt-1", "rec-1", 1, 1)],
            Some("p-rt-1"),
        ),
        case(
            "release track, a recording found elsewhere on the release",
            vec![ids(REL_P, "p-rt-1", "rec-2", 1, 2)],
            None,
        ),
        case(
            "release track, a merged recording at the track's place",
            vec![ids(REL_P, "p-rt-1", "rec-merged", 1, 1)],
            Some("p-rt-1"),
        ),
        case(
            "release and recording at the place, another edition's track id",
            vec![ids(REL_P, "q-rt-2", "rec-2", 1, 2)],
            Some("p-rt-2"),
        ),
        case(
            "recording at the place, no release id",
            vec![ids("", "q-rt-2", "rec-2", 1, 2)],
            None,
        ),
        case(
            "release and recording on another disc",
            vec![ids(REL_P, "", "rec-2", 2, 2)],
            None,
        ),
        case(
            "release and recording at another position",
            vec![ids(REL_P, "", "rec-2", 1, 3)],
            None,
        ),
        case("names and position only", vec![ids("", "", "", 1, 2)], None),
        case(
            "release id of another release",
            vec![ids(REL_Q, "p-rt-1", "rec-1", 1, 1)],
            None,
        ),
        case(
            "release id merged into the edition",
            vec![ids(REL_P_OLD, "p-rt-1", "rec-1", 1, 1)],
            Some("p-rt-1"),
        ),
        case("two files on one free track", vec![on_p(1), on_p(1)], None),
        Case {
            setup: |_, rows| map_on(&rows[0], REL_P, "p-rt-1", "rec-1", Some((1, 1))),
            ..case(
                "the track held by an indexed row",
                vec![on_p(1), on_p(1)],
                None,
            )
        },
        Case {
            setup: |_, rows| map_on(&rows[0], REL_P, "p-rt-1", "rec-1", Some((1, 1))),
            lose: &[0],
            ..case(
                "the track held by a missing row only",
                vec![on_p(1), on_p(1)],
                Some("p-rt-1"),
            )
        },
        Case {
            setup: |album, _| match_state_row(album, "unconfirmed", "EDITION_UNCERTAIN"),
            ..case("a best guess", vec![on_p(1)], None)
        },
        Case {
            setup: |album, _| match_state_row(album, "unmatched", "CHOSEN_EDITION_NOT_LISTED"),
            ..case("a chosen edition not listed", vec![on_p(1)], None)
        },
        Case {
            setup: |album, _| {
                format!(
                    "INSERT INTO auth_users (id, display_name, role, created_at) \
                     VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
                     INSERT INTO library_custom_edition_manifests (id, local_album_id, \
                     version, release_group_mbid, album_title, album_artist_name, \
                     source_album_revision, input_revision, content_hash, \
                     sealed_by_user_id, sealed_at) VALUES ('m1', '{album}', 1, 'rg-p', \
                     'T', 'A', 1, 'in', 'hash', 'curator', 0); \
                     INSERT INTO library_custom_edition_active (local_album_id, \
                     manifest_id, activated_at) VALUES ('{album}', 'm1', 0);"
                )
            },
            ..case("a custom edition", vec![on_p(1)], None)
        },
        Case {
            setup: |album, _| {
                format!(
                    "UPDATE local_album_external_identities SET release_mbid = NULL \
                     WHERE local_album_id = '{album}';"
                )
            },
            ..case("leave unmanaged", vec![on_p(1)], None)
        },
        Case {
            setup: |album, _| match_state_row(album, "unmatched", "CHOSEN_EDITION_FITS_NO_FILE"),
            ..case(
                "a chosen edition that fitted no file",
                vec![on_p(1)],
                Some("p-rt-1"),
            )
        },
        Case {
            setup: |_, rows| {
                format!(
                    "UPDATE local_tracks SET availability = 'excluded', manual_excluded = 1, \
                     excluded_at = 1 WHERE id = '{}';",
                    rows[0]
                )
            },
            ..case("an excluded row", vec![on_p(1)], None)
        },
        Case {
            setup: |_, rows| map_on(&rows[0], REL_Q, "q-rt-1", "rec-1", None),
            ..case(
                "a row mapped to another release's track",
                vec![on_p(1)],
                None,
            )
        },
        Case {
            setup: |_, rows| map_on(&rows[0], REL_P, "p-rt-1", "rec-1", None),
            document: false,
            lose: &[0],
            ..case(
                "no document: the track an old row named",
                vec![on_p(1), on_p(1)],
                Some("p-rt-1"),
            )
        },
        Case {
            setup: |_, rows| map_on(&rows[0], REL_P, "p-rt-2", "rec-2", Some((1, 2))),
            document: false,
            lose: &[0],
            ..case(
                "no document: recording at the place an old row recorded",
                vec![
                    ids(REL_P, "", "rec-2", 1, 2),
                    ids(REL_P, "q-rt-2", "rec-2", 1, 2),
                ],
                Some("p-rt-2"),
            )
        },
        Case {
            setup: |_, rows| map_on(&rows[0], REL_P, "p-rt-2", "rec-2", None),
            document: false,
            lose: &[0],
            ..case(
                "no document: recording at a place no old row recorded",
                vec![
                    ids(REL_P, "", "rec-2", 1, 2),
                    ids(REL_P, "q-rt-2", "rec-2", 1, 2),
                ],
                None,
            )
        },
    ];
    for (index, case) in cases.iter().enumerate() {
        let mut rig = RegroupRig::new(&format!("scan-place-{index}"));
        let files: Vec<(String, FileIds)> = case
            .files
            .iter()
            .enumerate()
            .map(|(n, ids)| (format!("a/t{n:03}.flac"), ids.clone()))
            .collect();
        let before = rig.before.clone();
        rig.plant_ids(&files, &before);
        rig.scan().await;
        let names: Vec<String> = files.iter().map(|(file, _)| file.clone()).collect();
        let rows = rig.rows_at(&names);
        let album = rig.album_of("a/t000.flac");
        for file in &names {
            assert_eq!(rig.album_of(file), album, "{}", case.name);
        }
        if case.document {
            rig.store_release(&edition_p());
        }
        rig.choose(&album, REL_P, "manual", 0.0);
        rig.run(&(case.setup)(&album, &rows));
        let last = rows.last().expect("a file");
        let identity = rig.identity(last);
        let lost: Vec<String> = case.lose.iter().map(|&n| names[n].clone()).collect();
        if lost.is_empty() {
            rig.scan().await;
        } else {
            rig.lose(&lost).await;
        }
        match case.placed {
            Some(release_track) => {
                assert_eq!(
                    rig.release_track_of(last).as_deref(),
                    Some(release_track),
                    "{}",
                    case.name
                );
                let written = rig.identity(last).expect("placed");
                assert!(
                    written.contains(&format!("|{REL_P}|")) && written.contains("|manual|"),
                    "{}: {written}",
                    case.name
                );
                assert_eq!(
                    rig.count(&format!(
                        "SELECT COUNT(*) FROM library_album_match_state \
                         WHERE local_album_id = '{album}'"
                    )),
                    0,
                    "{}",
                    case.name
                );
                assert_eq!(rig.mark_of(&album), None, "{}", case.name);
            }
            None => {
                assert_eq!(rig.identity(last), identity, "{}", case.name);
                assert_eq!(rig.marks_stamps_aliases(), (0, 0, 0), "{}", case.name);
            }
        }
    }
}

/// One file of [`REL_P`]'s track 1, `a/t000.flac`, on an album matched to
/// it by `source` at `chosen_at`, mapped, with the document on file.
/// Returns the rig, the album and the row.
async fn placed_album(name: &str, source: &str, chosen_at: f64) -> (RegroupRig, String, String) {
    let mut rig = RegroupRig::new(name);
    let before = rig.before.clone();
    rig.plant_ids(&[("a/t000.flac".to_owned(), on_p(1))], &before);
    rig.scan().await;
    let album = rig.album_of("a/t000.flac");
    let old = rig.rows_at(&["a/t000.flac".to_owned()]).remove(0);
    rig.store_release(&edition_p());
    rig.choose(&album, REL_P, source, chosen_at);
    rig.run(&map_on(&old, REL_P, "p-rt-1", "rec-1", Some((1, 1))));
    (rig, album, old)
}

impl RegroupRig {
    /// The file at `file`, which must be on track 1 of [`REL_P`].
    fn placed_at(&self, file: &str, case: &str) -> String {
        let row = self.rows_at(&[file.to_owned()]).remove(0);
        assert_eq!(
            self.release_track_of(&row).as_deref(),
            Some("p-rt-1"),
            "{case}"
        );
        row
    }

    /// Retag the album's file to the new album name and rename it into
    /// `b/` in one go, and scan.
    async fn retag_and_rename(&mut self) {
        let after = self.after.clone();
        self.plant_ids(&[("b/t000.flac".to_owned(), on_p(1))], &after);
        self.lose(&["a/t000.flac".to_owned()]).await;
    }
}

/// An album whose edition a person chose and that the app never wrote is
/// marked taken over, and parked, when a placed file is the old one
/// retagged by hand, and only then. Every case places the new file on the
/// old file's track.
#[tokio::test]
async fn a_placement_marks_only_a_hand_retag() {
    let (mut rig, album, _) = placed_album("scan-place-mark-retag", "manual", 0.0).await;
    rig.retag_and_rename().await;
    rig.placed_at("b/t000.flac", "retag");
    assert_eq!(rig.mark_of(&album).as_deref(), Some("retag_followed"));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_management_automatic_queue \
             WHERE local_album_id = '{album}' AND last_reason_code = 'TAKEN_OVER'"
        )),
        1,
        "parked"
    );

    let (mut rig, album, _) = placed_album("scan-place-mark-later", "manual", 1e12).await;
    rig.retag_and_rename().await;
    rig.placed_at("b/t000.flac", "chosen after the file came");
    assert_eq!(rig.mark_of(&album), None, "chosen after the file came");

    // A copy a finished scan saw beside the original, then the original
    // deleted: with the same tags it is not marked; retitled while the
    // original was still there, it is.
    for (retitled, mark) in [(false, None), (true, Some("retag_followed"))] {
        let (mut rig, album, _) =
            placed_album(&format!("scan-place-mark-copy-{retitled}"), "manual", 0.0).await;
        let title = if retitled {
            rig.after.clone()
        } else {
            rig.before.clone()
        };
        rig.plant_ids(&[("a/t000 copy.flac".to_owned(), on_p(1))], &title);
        rig.scan().await;
        assert_eq!(rig.album_of("a/t000 copy.flac"), album);
        rig.lose(&["a/t000.flac".to_owned()]).await;
        rig.placed_at("a/t000 copy.flac", "copy");
        assert_eq!(rig.mark_of(&album).as_deref(), mark, "retitled: {retitled}");
    }

    // The album keeps a second track, so it is still there when the file
    // comes back.
    let (mut rig, album, _) = placed_album("scan-place-mark-readded", "manual", 0.0).await;
    let before = rig.before.clone();
    rig.plant_ids(&[("a/t001.flac".to_owned(), on_p(2))], &before);
    rig.scan().await;
    rig.lose(&["a/t000.flac".to_owned()]).await;
    rig.plant_ids(&[("a/again.flac".to_owned(), on_p(1))], &before);
    rig.scan().await;
    assert_eq!(rig.album_of("a/again.flac"), album);
    rig.placed_at("a/again.flac", "added after a scan saw it gone");
    assert_eq!(rig.mark_of(&album), None, "added after a scan saw it gone");

    let (mut rig, album, old) = placed_album("scan-place-mark-record", "manual", 0.0).await;
    rig.run(&format!(
        "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
         VALUES ('{old}', 'sha');"
    ));
    rig.retag_and_rename().await;
    rig.placed_at("b/t000.flac", "a missing row with a record");
    assert_eq!(rig.mark_of(&album), None, "a missing row with a record");

    let (mut rig, album, _) = placed_album("scan-place-mark-automatic", "automatic", 0.0).await;
    rig.retag_and_rename().await;
    let row = rig.placed_at("b/t000.flac", "an edition the app chose");
    assert!(
        rig.identity(&row).expect("placed").contains("|automatic|"),
        "revisable"
    );
    assert_eq!(rig.mark_of(&album), None, "an edition the app chose");
}

/// A store error in one album rolls back its placement and its mark
/// together, with one warning naming it; the other album is placed and
/// the run ends. The next run end places and marks it.
#[tokio::test]
async fn a_failed_placement_takes_its_mark_with_it() {
    use droppedneedle_library::matching::{Release, ReleaseTrack};

    retag_warnings();
    let (mut rig, album, _) = placed_album("scan-place-store-error", "manual", 0.0).await;
    rig.store_release(&Release {
        id: REL_Q.to_owned(),
        tracks: vec![ReleaseTrack {
            id: "q-rt-1".to_owned(),
            recording_id: "rec-q".to_owned(),
            disc: 1,
            position: 1,
            ..ReleaseTrack::default()
        }],
        ..Release::default()
    });
    rig.plant_ids(
        &[(
            "c/t000.flac".to_owned(),
            ids(REL_Q, "q-rt-1", "rec-q", 1, 1),
        )],
        "Other",
    );
    rig.scan().await;
    let other = rig.album_of("c/t000.flac");
    assert_ne!(other, album);
    rig.choose(&other, REL_Q, "manual", 0.0);
    rig.run(
        "CREATE TRIGGER refuse_placement BEFORE INSERT ON local_track_external_identities \
         WHEN NEW.release_track_mbid = 'p-rt-1' \
         BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;",
    );
    rig.retag_and_rename().await;
    let new = rig.rows_at(&["b/t000.flac".to_owned()]).remove(0);
    assert_eq!(rig.release_track_of(&new), None);
    assert_eq!(
        rig.mark_of(&album),
        None,
        "the mark goes with the placement"
    );
    let other_row = rig.rows_at(&["c/t000.flac".to_owned()]).remove(0);
    assert_eq!(rig.release_track_of(&other_row).as_deref(), Some("q-rt-1"));
    let warned = || {
        retag_warnings()
            .lock()
            .expect("lines")
            .iter()
            .filter(|line| {
                line.contains("not placed on the album's edition") && line.contains(&album)
            })
            .count()
    };
    assert_eq!(warned(), 1);

    rig.run("DROP TRIGGER refuse_placement");
    rig.scan().await;
    assert_eq!(rig.release_track_of(&new).as_deref(), Some("p-rt-1"));
    assert_eq!(rig.mark_of(&album).as_deref(), Some("retag_followed"));
    assert_eq!(warned(), 1);
}

// ---------------------------------------------------------------------------
// Which artist row a file's credits land on, and the merge of rows
// MusicBrainz proves to be one artist.
// ---------------------------------------------------------------------------

const MBID_X: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const MBID_Y: &str = "aaaaaaaa-0000-4000-8000-000000000002";
const MBID_Z: &str = "aaaaaaaa-0000-4000-8000-000000000003";
const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";
const CONVERGED: &str = "AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE";

/// One side of a file's artist tags. A listed side writes its names to
/// the list key (ARTISTS, ALBUMARTISTS) as well as joined to the single
/// one; an unlisted side writes only the single key.
#[derive(Clone, Copy)]
struct Side<'a> {
    names: &'a [&'a str],
    ids: &'a [&'a str],
    sort: Option<&'a str>,
    listed: bool,
}

fn listed<'a>(names: &'a [&'a str], ids: &'a [&'a str]) -> Side<'a> {
    Side {
        names,
        ids,
        sort: None,
        listed: true,
    }
}

fn plain<'a>(names: &'a [&'a str]) -> Side<'a> {
    Side {
        names,
        ids: &[],
        sort: None,
        listed: false,
    }
}

/// A scan of real tagged files through the production tag reader.
struct ArtistRig {
    scratch: ScratchDir,
    root: PathBuf,
    coordinator: Arc<LibraryScanCoordinator<droppedneedle_library::adapters::LoftyTagReader>>,
    writes: AtomicU64,
}

impl ArtistRig {
    fn new(name: &str) -> Self {
        let scratch = ScratchDir::new(name);
        let root = scratch.join("music");
        std::fs::create_dir_all(&root).expect("root");
        let db = scratch.join("app.db");
        migrated_db(&db);
        let store = Arc::new(SqliteScanStore::new(
            droppedneedle_kernel::db::Db::open_for_tests(&db).expect("database opens"),
            Arc::new(SystemClock),
        ));
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
            store,
            BlockingPool::new(2),
            Arc::new(droppedneedle_library::adapters::LoftyTagReader),
            Arc::new(StaticResolver::new(registry)),
            Arc::new(SystemClock),
        )
        .with_wakeups(WorkWakeups::new());
        Self {
            scratch,
            root,
            coordinator: Arc::new(coordinator),
            writes: AtomicU64::new(0),
        }
    }

    fn db(&self) -> rusqlite::Connection {
        droppedneedle_kernel::db::open_connection(&self.scratch.join("app.db")).expect("db opens")
    }

    /// Plant (or retag) `relative` as a track of `album` with these
    /// artist tags, and date it later than before so the next scan reads
    /// it again.
    fn put(&self, relative: &str, album: &str, album_side: Side<'_>, track_side: Side<'_>) {
        self.put_on(relative, album, None, album_side, track_side);
    }

    /// [`Self::put`], with the release id the album is keyed by.
    fn put_on(
        &self,
        relative: &str,
        album: &str,
        release: Option<&str>,
        album_side: Side<'_>,
        track_side: Side<'_>,
    ) {
        use droppedneedle_library::tags::TagField;
        use droppedneedle_library::tags::save::{TagEdit, save_tags};
        let path = self.root.join(relative);
        if !path.exists() {
            plant(&self.root, relative, "management_full.flac");
        }
        let strings = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
        let list = |side: Side<'_>| {
            if side.listed {
                strings(side.names)
            } else {
                Vec::new()
            }
        };
        let sort = |side: Side<'_>| {
            side.sort
                .map(|sort| vec![sort.to_owned()])
                .unwrap_or_default()
        };
        let edits = vec![
            TagEdit::verbatim(TagField::Title, vec![relative.to_owned()]),
            TagEdit::verbatim(TagField::Album, vec![album.to_owned()]),
            TagEdit::verbatim(TagField::Artist, vec![track_side.names.join(" & ")]),
            TagEdit::verbatim(TagField::Artists, list(track_side)),
            TagEdit::verbatim(TagField::ArtistSort, sort(track_side)),
            TagEdit::verbatim(TagField::AlbumArtist, vec![album_side.names.join(" & ")]),
            TagEdit::verbatim(TagField::AlbumArtists, list(album_side)),
            TagEdit::verbatim(TagField::AlbumArtistSort, sort(album_side)),
            TagEdit::verbatim(TagField::MusicBrainzArtistId, strings(track_side.ids)),
            TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, strings(album_side.ids)),
            TagEdit::verbatim(TagField::MusicBrainzReleaseId, strings(release.as_slice())),
            TagEdit::verbatim(TagField::MusicBrainzReleaseGroupId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
        ];
        save_tags(&path, &edits).expect("file tagged");
        let write = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("file opens")
            .set_modified(
                std::time::SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(1_700_000_000 + write),
            )
            .expect("mtime set");
    }

    async fn scan(&self) {
        self.coordinator
            .request_run(&request(&self.root))
            .expect("requested");
        let paths = HashMap::from([("music".to_owned(), self.root.clone())]);
        let run = self.coordinator.run_once(&paths).await.expect("scan runs");
        assert_eq!(run.state.scan_state(), ScanState::Completed);
    }

    fn run(&self, sql: &str) {
        self.coordinator
            .store()
            .db()
            .run_sql(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    fn count(&self, sql: &str) -> i64 {
        self.coordinator
            .store()
            .db()
            .query_i64(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    fn text(&self, sql: &str) -> Option<String> {
        self.db()
            .query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// The artist row of the track credit at `position` of the file.
    fn track_artist(&self, relative: &str, position: i64) -> String {
        self.text(&format!(
            "SELECT c.local_artist_id FROM local_track_artists c JOIN local_tracks t \
             ON t.id = c.local_track_id WHERE t.relative_path = '{relative}' \
             AND c.position = {position}"
        ))
        .expect("track credit")
    }

    /// The artist rows of the file's album: its first album credit and
    /// its `album_artist_id`.
    fn album_artist(&self, relative: &str) -> (String, String) {
        let credit = self
            .text(&format!(
                "SELECT c.local_artist_id FROM local_album_artists c JOIN local_tracks t \
                 ON t.local_album_id = c.local_album_id \
                 WHERE t.relative_path = '{relative}' AND c.position = 0"
            ))
            .expect("album credit");
        let artist = self
            .text(&format!(
                "SELECT a.album_artist_id FROM local_albums a JOIN local_tracks t \
                 ON t.local_album_id = a.id WHERE t.relative_path = '{relative}'"
            ))
            .expect("album artist");
        (credit, artist)
    }

    fn name(&self, artist: &str) -> String {
        self.text(&format!(
            "SELECT display_name FROM local_artists WHERE id = '{artist}'"
        ))
        .expect("artist name")
    }

    fn retired_into(&self, artist: &str) -> Option<String> {
        self.text(&format!(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = '{artist}'"
        ))
    }

    /// A person's link of the artist to a MusicBrainz artist, as a
    /// contribution writes it.
    fn link(&self, artist: &str, mbid: &str) {
        self.link_by(artist, mbid, "manual");
    }

    /// A link of the artist to a MusicBrainz artist with this decision
    /// source.
    fn link_by(&self, artist: &str, mbid: &str, source: &str) {
        self.run(&format!(
            "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
             provider_artist_id, decision_source, selected_at) \
             VALUES ('{artist}', 'musicbrainz', '{mbid}', '{source}', 1)"
        ));
    }

    /// One favorite and one play of the artist for `user`.
    fn listen(&self, user: &str, artist: &str) {
        self.run(&format!(
            "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('{user}', 'artist', '{artist}', 1); \
             INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
             artist_name, played_at) VALUES ('{user}-{artist}', '{user}', '{artist}', 'Song', \
             'Artist', '2026-01-01T00:00:00Z');"
        ));
    }

    fn merges(&self) -> i64 {
        self.count(&format!(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = '{CONVERGED}'"
        ))
    }

    /// Match the album of `relative` to a release crediting `name` as
    /// `mbid` on the album and on every track, with each indexed track
    /// mapped, and work out what the match proves, as identification does.
    fn match_album(&self, relative: &str, mbid: &str, name: &str) {
        use droppedneedle_library::identify::sqlite::SqliteIdentifyStore;
        use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack};

        const RELEASE: &str = "bbbbbbbb-0000-4000-8000-0000000000e1";
        const GROUP: &str = "bbbbbbbb-0000-4000-8000-0000000000e2";
        let album = self
            .text(&format!(
                "SELECT local_album_id FROM local_tracks WHERE relative_path = '{relative}'"
            ))
            .expect("album");
        self.run(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', '{GROUP}', '{RELEASE}', 'automatic', 1); \
             INSERT INTO local_track_external_identities (local_track_id, provider, \
             recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at) \
             SELECT id, 'musicbrainz', 'rec-' || number, '{RELEASE}', 'rt-' || number, \
             'automatic', 1 FROM (SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS number \
             FROM local_tracks WHERE local_album_id = '{album}' AND availability = 'indexed');"
        ));
        let credit = vec![CreditedArtist {
            id: mbid.to_owned(),
            name: name.to_owned(),
            sort_name: None,
            join: String::new(),
        }];
        let numbers = self.count(&format!(
            "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = '{album}' \
             AND availability = 'indexed'"
        ));
        let store = SqliteIdentifyStore::new(
            droppedneedle_kernel::db::Db::open_for_tests(&self.scratch.join("app.db"))
                .expect("store opens"),
            Arc::new(SystemClock),
        );
        store
            .save_release(
                &Release {
                    id: RELEASE.to_owned(),
                    release_group_id: GROUP.to_owned(),
                    title: "Album".to_owned(),
                    artists: credit.clone(),
                    tracks: (1..=numbers as u32)
                        .map(|number| ReleaseTrack {
                            id: format!("rt-{number}"),
                            recording_id: format!("rec-{number}"),
                            title: format!("Song {number}"),
                            artists: credit.clone(),
                            disc: 1,
                            position: number,
                            absolute_position: number,
                            ..ReleaseTrack::default()
                        })
                        .collect(),
                    ..Release::default()
                },
                droppedneedle_kernel::db::Lane::Background,
            )
            .unwrap();
        store.link_album_artists(&album).unwrap();
    }

    /// One sweep of identification's artist links, as its loop runs it.
    fn sweep_artist_links(&self) {
        use droppedneedle_library::identify::sqlite::SqliteIdentifyStore;
        SqliteIdentifyStore::new(
            droppedneedle_kernel::db::Db::open_for_tests(&self.scratch.join("app.db"))
                .expect("store opens"),
            Arc::new(SystemClock),
        )
        .sweep_artist_links()
        .expect("sweep runs");
    }

    /// The MusicBrainz link of the artist and who made it.
    fn link_source(&self, artist: &str) -> Option<String> {
        self.text(&format!(
            "SELECT provider_artist_id || ' ' || decision_source \
             FROM local_artist_external_identities WHERE local_artist_id = '{artist}'"
        ))
    }
}

/// An artist row with no link, proven by its files to be MusicBrainz
/// artist x, merges at the next run end into the row a person then links
/// to x, with no file changed: its credits, album artist, favorites and
/// plays move, its id stays as an alias, and the duplicate artists list
/// shows the merge as resolved. A second run end merges nothing. A later
/// file named like the retired row but with another id gets a row of its
/// own, not the survivor.
#[tokio::test]
async fn rows_proven_to_be_one_artist_merge_at_the_run_end() {
    use droppedneedle_library::reconcile::groups::build;
    use droppedneedle_library::reconcile::models::GroupState;
    use droppedneedle_library::reconcile::store::inputs;

    let rig = ArtistRig::new("artist-converge");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.put(
        "B/01.flac",
        "Album B",
        listed(&["En"], &[MBID_X]),
        listed(&["En"], &[MBID_X]),
    );
    rig.scan().await;
    let survivor = rig.track_artist("A/01.flac", 0);
    let retired = rig.track_artist("B/01.flac", 0);
    assert_ne!(survivor, retired);
    rig.listen("listener", &survivor);
    rig.listen("listener", &retired);
    rig.link(&survivor, MBID_X);

    rig.scan().await;
    assert_eq!(rig.retired_into(&retired), Some(survivor.clone()));
    assert_eq!(
        rig.text(&format!(
            "SELECT local_artist_id || ' ' || kind FROM local_artist_aliases \
             WHERE alias = '{retired}'"
        )),
        Some(format!("{survivor} merged_artist"))
    );
    assert_eq!(rig.track_artist("B/01.flac", 0), survivor);
    assert_eq!(
        rig.album_artist("B/01.flac"),
        (survivor.clone(), survivor.clone())
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_user_favorites WHERE item_kind = 'artist' \
             AND item_id = '{survivor}'"
        )),
        1
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_user_favorites WHERE item_id = '{retired}'"
        )),
        0
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_play_history WHERE local_artist_id = '{survivor}'"
        )),
        2
    );
    assert_eq!(rig.merges(), 1);
    let resolved: Vec<_> = build(&inputs(&rig.db()).expect("groups read"))
        .into_iter()
        .filter(|group| group.state == GroupState::ResolvedAutomatically)
        .collect();
    assert_eq!(resolved.len(), 1);
    let mut members: Vec<&str> = resolved[0].members.iter().map(|m| m.id.as_str()).collect();
    members.sort_unstable();
    let mut expected = vec![survivor.as_str(), retired.as_str()];
    expected.sort_unstable();
    assert_eq!(members, expected);
    assert_eq!(resolved[0].provider_mbids, vec![MBID_X.to_owned()]);

    rig.scan().await;
    assert_eq!(rig.merges(), 1, "nothing new to merge");

    rig.put(
        "C/01.flac",
        "Album C",
        listed(&["En"], &[MBID_Y]),
        listed(&["En"], &[MBID_Y]),
    );
    rig.scan().await;
    let other = rig.track_artist("C/01.flac", 0);
    assert_ne!(other, survivor, "another artist named like the merged row");
    assert_ne!(other, retired);
    assert_eq!(rig.name(&other), "En");
    assert_eq!(rig.retired_into(&other), None);
}

/// A row with a file that carries no id is not merged, even though its
/// other files prove it is the linked artist: the id-less file may be
/// another artist's.
#[tokio::test]
async fn a_row_with_a_file_without_the_id_is_not_merged() {
    let rig = ArtistRig::new("artist-converge-partial");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.put(
        "B/01.flac",
        "Album B",
        listed(&["En"], &[MBID_X]),
        listed(&["En"], &[MBID_X]),
    );
    rig.put("B/02.flac", "Album B", plain(&["En"]), plain(&["En"]));
    rig.scan().await;
    let linked = rig.track_artist("A/01.flac", 0);
    let proven = rig.track_artist("B/01.flac", 0);
    assert_eq!(rig.track_artist("B/02.flac", 0), proven);
    rig.link(&linked, MBID_X);
    rig.scan().await;
    assert_eq!(rig.retired_into(&proven), None);
    assert_eq!(rig.merges(), 0);
}

/// A group of three rows proven to be one artist, two of which an
/// administrator marked as distinct, is not merged at all. Once the mark
/// is gone, both merge into the linked row in one go.
#[tokio::test]
async fn a_group_marked_distinct_is_not_merged() {
    let rig = ArtistRig::new("artist-converge-distinct");
    rig.put("S/01.flac", "Album S", plain(&["Es"]), plain(&["Es"]));
    rig.put("A/01.flac", "Album A", plain(&["Ay"]), plain(&["Ay"]));
    rig.put("B/01.flac", "Album B", plain(&["Bee"]), plain(&["Bee"]));
    rig.scan().await;
    let survivor = rig.track_artist("S/01.flac", 0);
    let mut pair = [
        rig.track_artist("A/01.flac", 0),
        rig.track_artist("B/01.flac", 0),
    ];
    pair.sort();
    let [left, right] = &pair;
    // Two rows each proven by its files, as rows split before the scan
    // kept ids are once their files are read again.
    rig.run(&format!(
        "UPDATE local_track_artists SET artist_mbid = '{MBID_X}' \
         WHERE local_artist_id IN ('{left}', '{right}'); \
         UPDATE local_album_artists SET artist_mbid = '{MBID_X}' \
         WHERE local_artist_id IN ('{left}', '{right}'); \
         INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_artist_reconciliation_dismissals (left_artist_id, \
         right_artist_id, left_artist_revision, right_artist_revision, dismissed_by_user_id, \
         created_at, updated_at) VALUES ('{left}', '{right}', 1, 1, 'admin', 1, 1);"
    ));
    rig.link(&survivor, MBID_X);
    rig.scan().await;
    assert_eq!(rig.retired_into(left), None);
    assert_eq!(rig.retired_into(right), None);
    assert_eq!(rig.merges(), 0);

    rig.run("DELETE FROM library_artist_reconciliation_dismissals");
    rig.scan().await;
    assert_eq!(rig.retired_into(left), Some(survivor.clone()));
    assert_eq!(rig.retired_into(right), Some(survivor));
    assert_eq!(rig.merges(), 1);
}

/// MusicBrainz's `[unknown]` artist, which stands for no one artist.
const UNKNOWN_MBID: &str = "125ec42a-7229-4250-afc5-e057484327fe";

/// Two differently named artists whose files both carry MusicBrainz's
/// `[unknown]` id keep a row each after the scan and its run end, and
/// neither credit stores the id: a special-purpose id is never proof.
#[tokio::test]
async fn a_placeholder_artist_id_keeps_two_artists_apart() {
    let rig = ArtistRig::new("artist-placeholder");
    rig.put(
        "A/01.flac",
        "Album A",
        listed(&["Oh"], &[UNKNOWN_MBID]),
        listed(&["Oh"], &[UNKNOWN_MBID]),
    );
    rig.put(
        "B/01.flac",
        "Album B",
        listed(&["En"], &[UNKNOWN_MBID]),
        listed(&["En"], &[UNKNOWN_MBID]),
    );
    rig.scan().await;
    let oh = rig.track_artist("A/01.flac", 0);
    let en = rig.track_artist("B/01.flac", 0);
    assert_ne!(oh, en);
    assert_eq!(rig.name(&oh), "Oh");
    assert_eq!(rig.name(&en), "En");
    assert_eq!(rig.retired_into(&oh), None);
    assert_eq!(rig.retired_into(&en), None);
    assert_eq!(
        rig.count(&format!(
            "SELECT (SELECT COUNT(*) FROM local_track_artists WHERE artist_mbid = '{UNKNOWN_MBID}') \
             + (SELECT COUNT(*) FROM local_album_artists WHERE artist_mbid = '{UNKNOWN_MBID}')"
        )),
        0
    );
}

/// Rows whose credits already store the `[unknown]` id, as files scanned
/// before it was dropped left them, stay apart at the run end.
#[tokio::test]
async fn rows_holding_a_placeholder_id_are_not_merged() {
    let rig = ArtistRig::new("artist-placeholder-stored");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.put("B/01.flac", "Album B", plain(&["En"]), plain(&["En"]));
    rig.scan().await;
    let oh = rig.track_artist("A/01.flac", 0);
    let en = rig.track_artist("B/01.flac", 0);
    assert_ne!(oh, en);
    rig.run(&format!(
        "UPDATE local_track_artists SET artist_mbid = '{UNKNOWN_MBID}' \
         WHERE local_artist_id IN ('{oh}', '{en}'); \
         UPDATE local_album_artists SET artist_mbid = '{UNKNOWN_MBID}' \
         WHERE local_artist_id IN ('{oh}', '{en}');"
    ));
    rig.scan().await;
    assert_eq!(rig.retired_into(&oh), None);
    assert_eq!(rig.retired_into(&en), None);
    assert_eq!(rig.merges(), 0);
}

/// A merge the store refuses is rolled back with one warning and the run
/// still ends; the next run end merges.
#[tokio::test]
async fn a_refused_merge_waits_for_the_next_run() {
    let rig = ArtistRig::new("artist-converge-refused");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.put(
        "B/01.flac",
        "Album B",
        listed(&["En"], &[MBID_X]),
        listed(&["En"], &[MBID_X]),
    );
    rig.scan().await;
    let survivor = rig.track_artist("A/01.flac", 0);
    let retired = rig.track_artist("B/01.flac", 0);
    rig.link(&survivor, MBID_X);
    rig.run(
        "CREATE TRIGGER refuse_alias BEFORE INSERT ON local_artist_aliases \
         BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    );
    let warned = || {
        retag_warnings()
            .lock()
            .expect("lines")
            .iter()
            .filter(|line| line.contains("could not be merged") && line.contains(&retired))
            .count()
    };
    retag_warnings();
    rig.scan().await;
    assert_eq!(rig.retired_into(&retired), None);
    assert_eq!(rig.track_artist("B/01.flac", 0), retired);
    assert_eq!(rig.merges(), 0);
    assert_eq!(warned(), 1);

    rig.run("DROP TRIGGER refuse_alias");
    rig.scan().await;
    assert_eq!(rig.retired_into(&retired), Some(survivor));
    assert_eq!(rig.merges(), 1);
}

/// ARTIST "Ay feat. Bee" with one id, no ARTISTS list, while the row "Ay"
/// is proven to be that id: the credit stays on its own row and "Ay" is
/// not renamed.
#[tokio::test]
async fn a_joined_credit_with_one_id_keeps_its_own_row() {
    let rig = ArtistRig::new("artist-joined");
    rig.put(
        "A/01.flac",
        "Album A",
        listed(&["Ay"], &[MBID_X]),
        listed(&["Ay"], &[MBID_X]),
    );
    let joined = Side {
        names: &["Ay feat. Bee"],
        ids: &[MBID_X],
        sort: None,
        listed: false,
    };
    rig.put("F/01.flac", "Album F", plain(&["Ay feat. Bee"]), joined);
    rig.scan().await;
    let own = rig.track_artist("A/01.flac", 0);
    let feat = rig.track_artist("F/01.flac", 0);
    assert_ne!(own, feat);
    assert_eq!(rig.name(&own), "Ay");
    assert_eq!(rig.name(&feat), "Ay feat. Bee");
}

/// Ids that do not line up with the names (fewer ids than names, or an
/// id that is not a UUID) prove nothing: the credits go by name.
#[tokio::test]
async fn ids_that_do_not_line_up_go_by_name() {
    let rig = ArtistRig::new("artist-unaligned");
    rig.put(
        "P/01.flac",
        "Album P",
        plain(&["Pe"]),
        listed(&["Pe", "Cue"], &[MBID_X]),
    );
    rig.put(
        "R/01.flac",
        "Album R",
        plain(&["Ar"]),
        listed(&["Ar"], &["a/b"]),
    );
    rig.scan().await;
    let pe = rig.track_artist("P/01.flac", 0);
    assert_eq!(rig.name(&pe), "Pe");
    assert_eq!(rig.album_artist("P/01.flac").0, pe);
    assert_eq!(rig.name(&rig.track_artist("P/01.flac", 1)), "Cue");
    assert_eq!(rig.name(&rig.track_artist("R/01.flac", 0)), "Ar");
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM local_track_artists WHERE artist_mbid IS NOT NULL"),
        0
    );
}

/// An album whose match is accepted, tagged with no artist ids, is
/// retagged with another sort name: the rescan puts its credits on a new
/// row. The next sweep works the album through again, and the artist is
/// one row holding the MusicBrainz id, the old row's id kept as an alias.
/// A rescan that changes nothing leaves the album's state as it was.
#[tokio::test]
async fn a_changed_sort_name_keeps_one_artist_row() {
    let rig = ArtistRig::new("artist-sort-change");
    let side = |sort| Side {
        names: &["Oh"],
        ids: &[],
        sort: Some(sort),
        listed: false,
    };
    let tag = |sort| {
        for file in ["A/01.flac", "A/02.flac"] {
            rig.put(file, "Album A", side(sort), side(sort));
        }
    };
    tag("Oh, The");
    rig.scan().await;
    rig.match_album("A/01.flac", MBID_X, "Oh");
    let old = rig.track_artist("A/01.flac", 0);
    assert_eq!(rig.link_source(&old), Some(format!("{MBID_X} automatic")));
    let album = |rig: &ArtistRig| {
        rig.text("SELECT local_album_id FROM local_tracks WHERE relative_path = 'A/01.flac'")
            .expect("album")
    };
    let matched = album(&rig);

    tag("Oh Band");
    rig.scan().await;
    let new = rig.track_artist("A/01.flac", 0);
    assert_ne!(new, old, "the new sort name keys another row");
    assert_eq!(album(&rig), matched);
    rig.sweep_artist_links();

    assert_eq!(rig.retired_into(&old).as_deref(), Some(new.as_str()));
    assert_eq!(rig.link_source(&new), Some(format!("{MBID_X} automatic")));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_artist_aliases WHERE alias = '{old}' \
             AND local_artist_id = '{new}' AND kind = 'merged_artist'"
        )),
        1
    );
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM local_artists WHERE display_name = 'Oh' \
             AND retired_into_artist_id IS NULL"
        ),
        1
    );
    assert_eq!(rig.album_artist("A/01.flac"), (new.clone(), new.clone()));

    let state = |rig: &ArtistRig| {
        rig.text(&format!(
            "SELECT input_revision || ' ' || updated_at FROM library_artist_reconciliation_state \
             WHERE local_album_id = '{matched}'"
        ))
    };
    let settled = state(&rig);
    assert!(settled.is_some());
    tag("Oh Band");
    rig.scan().await;
    assert_eq!(
        state(&rig),
        settled,
        "unchanged credits leave the state alone"
    );
}

/// A Various Artists id proves nothing: the album credit takes the name
/// row, keeps no id, and the Various Artists record is left alone.
#[tokio::test]
async fn a_various_artists_id_goes_by_name() {
    let rig = ArtistRig::new("artist-various");
    let sentinel = "SELECT row_revision FROM local_artists \
                    WHERE id = '00000000-0000-4000-8000-000000000001'";
    let before = rig.count(sentinel);
    rig.put(
        "V/01.flac",
        "Album V",
        listed(&["Various Artists"], &[VARIOUS_ARTISTS_MBID]),
        listed(&["Vee"], &[MBID_X]),
    );
    rig.scan().await;
    let (credit, album_artist) = rig.album_artist("V/01.flac");
    assert_eq!(credit, album_artist);
    assert_ne!(credit, "00000000-0000-4000-8000-000000000001");
    assert_eq!(rig.name(&credit), "Various Artists");
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM local_album_artists WHERE artist_mbid IS NOT NULL"),
        0
    );
    assert_eq!(rig.count(sentinel), before);
}

/// A name change in a file without artist ids makes a new row, as it
/// always has: names prove nothing. The album, kept by its release id,
/// takes the new row as its artist along with its credit.
#[tokio::test]
async fn a_name_change_without_ids_makes_a_new_row() {
    const RELEASE: &str = "bbbbbbbb-0000-4000-8000-000000000001";
    let rig = ArtistRig::new("artist-renamed-plain");
    rig.put_on(
        "G/01.flac",
        "Album G",
        Some(RELEASE),
        plain(&["Ex"]),
        plain(&["Ex"]),
    );
    rig.scan().await;
    let old = rig.track_artist("G/01.flac", 0);
    let album = "SELECT local_album_id FROM local_tracks WHERE relative_path = 'G/01.flac'";
    let before = rig.text(album);
    rig.put_on(
        "G/01.flac",
        "Album G",
        Some(RELEASE),
        plain(&["Wy"]),
        plain(&["Wy"]),
    );
    rig.scan().await;
    let new = rig.track_artist("G/01.flac", 0);
    assert_ne!(new, old);
    assert_eq!(rig.name(&new), "Wy");
    assert_eq!(rig.name(&old), "Ex");
    assert_eq!(rig.text(album), before, "the same album");
    assert_eq!(rig.album_artist("G/01.flac"), (new.clone(), new));
}

/// One side of a file naming `name` with this sort name and no id.
fn sorted(name: &'static [&'static str], sort: &'static str) -> Side<'static> {
    Side {
        names: name,
        ids: &[],
        sort: Some(sort),
        listed: false,
    }
}

/// The duplicate artists list's open groups, each with its state, its
/// sorted member ids and the record it recommends keeping.
fn open_groups(rig: &ArtistRig) -> Vec<(&'static str, Vec<String>, Option<String>)> {
    use droppedneedle_library::reconcile::groups::build;
    use droppedneedle_library::reconcile::models::GroupState;
    use droppedneedle_library::reconcile::store::inputs;
    build(&inputs(&rig.db()).expect("groups read"))
        .into_iter()
        .filter(|group| group.state != GroupState::ResolvedAutomatically)
        .map(|group| {
            let mut ids: Vec<String> = group.members.into_iter().map(|m| m.id).collect();
            ids.sort_unstable();
            (group.state.as_str(), ids, group.recommended_survivor_id)
        })
        .collect()
}

fn sorted_ids(ids: &[&str]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    ids.sort_unstable();
    ids
}

/// A retag that changes only an artist's sort name, with no ids and no
/// match, moves its credits to a new row; at the run end the old row
/// merges into it with its favorite and play, its id kept as an alias,
/// and the duplicate artists list shows the merge as resolved. A later
/// file under the old name and sort with another artist's id does not
/// land on the survivor once that is linked to someone else.
#[tokio::test]
async fn a_sort_name_change_carries_the_old_row_over() {
    use droppedneedle_library::reconcile::groups::build;
    use droppedneedle_library::reconcile::models::GroupState;
    use droppedneedle_library::reconcile::store::inputs;

    let rig = ArtistRig::new("artist-follow-sort");
    let tag = |sort: &'static str| {
        for file in ["A/01.flac", "A/02.flac"] {
            rig.put(
                file,
                "Album A",
                sorted(&["Oh"], sort),
                sorted(&["Oh"], sort),
            );
        }
    };
    tag("Oh, The");
    rig.scan().await;
    let old = rig.track_artist("A/01.flac", 0);
    rig.listen("listener", &old);

    tag("Oh Band");
    rig.scan().await;
    let new = rig.track_artist("A/01.flac", 0);
    assert_ne!(new, old, "the new sort name keys another row");
    assert_eq!(rig.retired_into(&old), Some(new.clone()));
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_user_favorites WHERE item_kind = 'artist' \
             AND item_id = '{new}'"
        )),
        1
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_play_history WHERE local_artist_id = '{new}'"
        )),
        1
    );
    assert_eq!(
        rig.text(&format!(
            "SELECT local_artist_id || ' ' || kind FROM local_artist_aliases \
             WHERE alias = '{old}'"
        )),
        Some(format!("{new} merged_artist"))
    );
    let resolved: Vec<_> = build(&inputs(&rig.db()).expect("groups read"))
        .into_iter()
        .filter(|group| group.state == GroupState::ResolvedAutomatically)
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].reason_code, "AUTOMATIC_SAME_NAME_CREDITS_MOVED");
    assert!(resolved[0].provider_mbids.is_empty());

    rig.link(&new, MBID_X);
    let other = Side {
        names: &["Oh"],
        ids: &[MBID_Y],
        sort: Some("Oh, The"),
        listed: true,
    };
    rig.put("C/01.flac", "Album C", other, other);
    rig.scan().await;
    assert_ne!(
        rig.track_artist("C/01.flac", 0),
        new,
        "an id the survivor's link contradicts keeps off it"
    );
}

/// A retag that renames an artist, with no ids, leaves the old row and
/// its favorite where they are and offers the two rows in the duplicate
/// artists list, the new one as the record to keep.
#[tokio::test]
async fn a_renamed_artist_is_offered_not_merged() {
    let rig = ArtistRig::new("artist-follow-rename");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.scan().await;
    let old = rig.track_artist("A/01.flac", 0);
    rig.listen("listener", &old);

    rig.put("A/01.flac", "Album A", plain(&["Ox"]), plain(&["Ox"]));
    rig.scan().await;
    let new = rig.track_artist("A/01.flac", 0);
    assert_ne!(new, old);
    assert_eq!(rig.retired_into(&old), None);
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM library_user_favorites WHERE item_id = '{old}'"
        )),
        1
    );
    assert_eq!(
        open_groups(&rig),
        vec![(
            "credits_moved",
            sorted_ids(&[&old, &new]),
            Some(new.clone()),
        )]
    );
}

/// Moves of one pair recorded both by name and with an id are still one
/// destination: the list offers the two rows once, the new one to keep.
#[tokio::test]
async fn a_pair_moved_by_name_and_by_id_is_one_destination() {
    let rig = ArtistRig::new("artist-follow-both-kinds");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.scan().await;
    let old = rig.track_artist("A/01.flac", 0);
    rig.listen("listener", &old);
    rig.put("A/01.flac", "Album A", plain(&["Ox"]), plain(&["Ox"]));
    rig.scan().await;
    let new = rig.track_artist("A/01.flac", 0);
    rig.run(&format!(
        "INSERT INTO local_artist_merge_candidates (id, left_artist_id, right_artist_id, \
         reason_code, created_at, updated_at) \
         VALUES ('moved-with-id:{old}:{new}', '{old}', '{new}', 'CREDITS_MOVED_WITH_ID', 1, 1)"
    ));

    assert_eq!(
        open_groups(&rig),
        vec![(
            "credits_moved",
            sorted_ids(&[&old, &new]),
            Some(new.clone()),
        )]
    );
}

/// A retag that splits an artist's albums between two sort names leaves
/// the old row unmerged, and the list offers it with both new rows as one
/// group.
#[tokio::test]
async fn credits_split_across_two_rows_are_offered_as_one_group() {
    let rig = ArtistRig::new("artist-follow-split");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.put("B/01.flac", "Album B", plain(&["Oh"]), plain(&["Oh"]));
    rig.scan().await;
    let old = rig.track_artist("A/01.flac", 0);
    rig.listen("listener", &old);

    let one = sorted(&["Oh"], "Oh, The");
    let two = sorted(&["Oh"], "Oh Band");
    rig.put("A/01.flac", "Album A", one, one);
    rig.put("B/01.flac", "Album B", two, two);
    rig.scan().await;
    let first = rig.track_artist("A/01.flac", 0);
    let second = rig.track_artist("B/01.flac", 0);
    assert_ne!(first, second);
    assert_eq!(rig.retired_into(&old), None);
    let groups = open_groups(&rig);
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].1, sorted_ids(&[&old, &first, &second]));
}

/// A sort-name change of a row a person linked is not merged by name;
/// the list offers the old row with the new one.
#[tokio::test]
async fn a_linked_row_left_empty_by_a_sort_change_is_offered() {
    let rig = ArtistRig::new("artist-follow-linked");
    let tag = |sort: &'static str| {
        rig.put(
            "A/01.flac",
            "Album A",
            sorted(&["Oh"], sort),
            sorted(&["Oh"], sort),
        );
    };
    tag("Oh, The");
    rig.scan().await;
    let old = rig.track_artist("A/01.flac", 0);
    rig.listen("listener", &old);
    rig.link(&old, MBID_X);

    tag("Oh Band");
    rig.scan().await;
    let new = rig.track_artist("A/01.flac", 0);
    assert_ne!(new, old);
    assert_eq!(rig.retired_into(&old), None);
    let groups = open_groups(&rig);
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].1, sorted_ids(&[&old, &new]));
}

/// Two artists who share a name but not their MusicBrainz id get a row
/// each.
#[tokio::test]
async fn two_artists_with_one_name_get_two_rows() {
    let rig = ArtistRig::new("artist-same-name");
    rig.put(
        "J1/01.flac",
        "Album J1",
        listed(&["Jay"], &[MBID_X]),
        listed(&["Jay"], &[MBID_X]),
    );
    rig.put(
        "J2/01.flac",
        "Album J2",
        listed(&["Jay"], &[MBID_Y]),
        listed(&["Jay"], &[MBID_Y]),
    );
    rig.scan().await;
    let one = rig.track_artist("J1/01.flac", 0);
    let two = rig.track_artist("J2/01.flac", 0);
    assert_ne!(one, two);
    assert_eq!(rig.name(&two), "Jay");
    assert_eq!(rig.album_artist("J2/01.flac"), (two.clone(), two));
}

/// After a person merges artist A (files with id a) into artist B
/// (linked to b), files of both read again credit B, and no row is made
/// for A's name or id.
#[tokio::test]
async fn a_persons_merge_holds_when_the_files_are_read_again() {
    let rig = ArtistRig::new("artist-person-merge");
    let em = listed(&["Em"], &[MBID_X]);
    let be = listed(&["Be"], &[MBID_Y]);
    rig.put("M/01.flac", "Album M", em, em);
    rig.put("N/01.flac", "Album N", be, be);
    rig.scan().await;
    let merged = rig.track_artist("M/01.flac", 0);
    let kept = rig.track_artist("N/01.flac", 0);
    rig.link(&kept, MBID_Y);
    // What the manual merge leaves behind.
    rig.run(&format!(
        "UPDATE local_track_artists SET local_artist_id = '{kept}' \
         WHERE local_artist_id = '{merged}'; \
         UPDATE local_album_artists SET local_artist_id = '{kept}' \
         WHERE local_artist_id = '{merged}'; \
         UPDATE local_albums SET album_artist_id = '{kept}' WHERE album_artist_id = '{merged}'; \
         UPDATE local_artists SET retired_into_artist_id = '{kept}' WHERE id = '{merged}'; \
         INSERT INTO local_artist_aliases (alias, local_artist_id, kind, created_at) \
         VALUES ('{merged}', '{kept}', 'merged_artist', 1);"
    ));
    let rows = rig.count("SELECT COUNT(*) FROM local_artists");
    rig.put("M/01.flac", "Album M", em, em);
    rig.put("N/01.flac", "Album N", be, be);
    rig.scan().await;
    assert_eq!(rig.track_artist("M/01.flac", 0), kept);
    assert_eq!(rig.album_artist("M/01.flac"), (kept.clone(), kept.clone()));
    assert_eq!(rig.track_artist("N/01.flac", 0), kept);
    assert_eq!(rig.count("SELECT COUNT(*) FROM local_artists"), rows);
}

/// ARTISTS with id x and only ALBUMARTIST, while the row the name keys to
/// is proven to be another artist y: the album credit and the album's
/// artist go with the track credit to x's row, not to y's by name.
#[tokio::test]
async fn an_album_artist_without_an_id_follows_the_track_credit() {
    let rig = ArtistRig::new("artist-album-side");
    rig.put(
        "Y/01.flac",
        "Album Y",
        listed(&["Foo"], &[MBID_Y]),
        listed(&["Foo"], &[MBID_Y]),
    );
    rig.put(
        "X/01.flac",
        "Album X",
        plain(&["Foo"]),
        listed(&["Foo"], &[MBID_X]),
    );
    rig.scan().await;
    let y = rig.track_artist("Y/01.flac", 0);
    let x = rig.track_artist("X/01.flac", 0);
    assert_ne!(x, y);
    assert_eq!(rig.album_artist("X/01.flac"), (x.clone(), x));
}

/// One file carrying another artist's id (a copy-paste slip) lands on
/// that artist's row but does not rename it while its own files use its
/// name.
#[tokio::test]
async fn one_stray_id_does_not_rename_an_artist() {
    let rig = ArtistRig::new("artist-stray-id");
    let bar = listed(&["Bar"], &[MBID_X]);
    rig.put("B/01.flac", "Album B", bar, bar);
    rig.put("B/02.flac", "Album B", bar, bar);
    let foo = listed(&["Foo"], &[MBID_X]);
    rig.put("F/01.flac", "Album F", foo, foo);
    rig.scan().await;
    let bar_row = rig.track_artist("B/01.flac", 0);
    assert_eq!(rig.track_artist("F/01.flac", 0), bar_row);
    assert_eq!(rig.name(&bar_row), "Bar");
}

/// An artist proven only by its own files (no link) keeps its row when
/// every file is renamed with its id: the row takes the new name and
/// sort name, and its favorite and play stay with it.
#[tokio::test]
async fn a_renamed_artist_proven_by_its_files_keeps_its_row() {
    let rig = ArtistRig::new("artist-renamed-by-id");
    let old = listed(&["Old"], &[MBID_X]);
    rig.put("R/01.flac", "Album R", old, old);
    rig.put("R/02.flac", "Album R", old, old);
    rig.scan().await;
    let artist = rig.track_artist("R/01.flac", 0);
    rig.listen("listener", &artist);
    let new = Side {
        names: &["New"],
        ids: &[MBID_X],
        sort: Some("New, The"),
        listed: true,
    };
    rig.put("R/01.flac", "Album R", new, new);
    rig.put("R/02.flac", "Album R", new, new);
    rig.scan().await;
    assert_eq!(rig.track_artist("R/01.flac", 0), artist);
    assert_eq!(rig.track_artist("R/02.flac", 0), artist);
    assert_eq!(
        rig.album_artist("R/02.flac"),
        (artist.clone(), artist.clone())
    );
    assert_eq!(
        rig.text(&format!(
            "SELECT display_name || ' / ' || sort_name FROM local_artists WHERE id = '{artist}'"
        )),
        Some("New / New, The".to_owned())
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT (SELECT COUNT(*) FROM library_user_favorites WHERE item_id = '{artist}') \
             + (SELECT COUNT(*) FROM library_play_history WHERE local_artist_id = '{artist}')"
        )),
        2
    );
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM local_artists WHERE display_name = 'Old' \
             AND retired_into_artist_id IS NULL"
        ),
        0
    );
}

/// One file whose album artist "Jay" has id z and whose track artist
/// "Jay" has id x credits two rows: one file never puts two ids on one
/// row.
#[tokio::test]
async fn one_file_never_puts_two_ids_on_one_row() {
    let rig = ArtistRig::new("artist-two-ids-one-file");
    rig.put(
        "K/01.flac",
        "Album K",
        listed(&["Jay"], &[MBID_Z]),
        listed(&["Jay"], &[MBID_X]),
    );
    rig.scan().await;
    let (album, _) = rig.album_artist("K/01.flac");
    let track = rig.track_artist("K/01.flac", 0);
    assert_ne!(album, track);
}

/// An artist renamed by an outside tagger that also moves its files (new
/// folder, new album artist) takes the new name in the scan that reads
/// them: the old paths, still indexed until that scan ends, do not hold
/// the old name, and neither does a file of the artist already missing.
#[tokio::test]
async fn an_artist_renamed_and_moved_by_a_tagger_takes_the_new_name() {
    let rig = ArtistRig::new("artist-renamed-moved");
    let foo = listed(&["Foo"], &[MBID_X]);
    rig.put("Foo/A/01.flac", "Album A", foo, foo);
    rig.put("Foo/A/02.flac", "Album A", foo, foo);
    rig.put("Foo/B/01.flac", "Album B", foo, foo);
    rig.scan().await;
    let artist = rig.track_artist("Foo/A/01.flac", 0);
    std::fs::remove_file(rig.root.join("Foo/B/01.flac")).expect("file removed");
    rig.scan().await;
    assert_eq!(
        rig.text("SELECT availability FROM local_tracks WHERE relative_path = 'Foo/B/01.flac'"),
        Some("missing".to_owned())
    );

    let bar = listed(&["Bar"], &[MBID_X]);
    std::fs::create_dir_all(rig.root.join("Bar/A")).expect("folder made");
    for file in ["01.flac", "02.flac"] {
        std::fs::rename(
            rig.root.join("Foo/A").join(file),
            rig.root.join("Bar/A").join(file),
        )
        .expect("file moved");
        rig.put(&format!("Bar/A/{file}"), "Album A", bar, bar);
    }
    rig.scan().await;
    assert_eq!(rig.track_artist("Bar/A/01.flac", 0), artist);
    assert_eq!(rig.track_artist("Bar/A/02.flac", 0), artist);
    assert_eq!(rig.name(&artist), "Bar");
}

/// A row the app linked from its matched album takes in, at the run end,
/// a row whose every credit holds the same id, but only once every credit
/// of its own is proven: a file of it on an unmatched album holds the
/// merge back until that file carries the id too.
#[tokio::test]
async fn an_automatic_link_merges_only_when_fully_proven() {
    let rig = ArtistRig::new("artist-automatic-link");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    let en = listed(&["En"], &[MBID_X]);
    rig.put("B/01.flac", "Album B", en, en);
    rig.scan().await;
    let linked = rig.track_artist("A/01.flac", 0);
    let proven = rig.track_artist("B/01.flac", 0);
    rig.match_album("A/01.flac", MBID_X, "Oh");
    assert_eq!(
        rig.link_source(&linked),
        Some(format!("{MBID_X} automatic"))
    );

    rig.put("C/01.flac", "Album C", plain(&["Oh"]), plain(&["Oh"]));
    rig.scan().await;
    assert_eq!(rig.track_artist("C/01.flac", 0), linked);
    assert_eq!(rig.retired_into(&proven), None);
    assert_eq!(rig.merges(), 0);

    let oh = listed(&["Oh"], &[MBID_X]);
    rig.put("C/01.flac", "Album C", oh, oh);
    rig.scan().await;
    assert_eq!(rig.track_artist("C/01.flac", 0), linked);
    assert_eq!(rig.retired_into(&proven), Some(linked.clone()));
    assert_eq!(rig.track_artist("B/01.flac", 0), linked);
    assert_eq!(rig.merges(), 1);
    assert_eq!(
        rig.link_source(&linked),
        Some(format!("{MBID_X} automatic"))
    );
}

/// A row proven only by its matched album and a row proven only by its
/// files, neither linked, are merged at one run end, the younger into the
/// older.
#[tokio::test]
async fn rows_proven_by_an_album_and_by_files_merge_at_the_run_end() {
    let rig = ArtistRig::new("artist-proof-merge");
    rig.put("A/01.flac", "Album A", plain(&["Pe"]), plain(&["Pe"]));
    rig.scan().await;
    let older = rig.track_artist("A/01.flac", 0);
    rig.match_album("A/01.flac", MBID_X, "Pe");
    // As when another row held the id while the album was matched.
    rig.run(&format!(
        "DELETE FROM local_artist_external_identities WHERE local_artist_id = '{older}'"
    ));
    let en = listed(&["En"], &[MBID_X]);
    rig.put("B/01.flac", "Album B", en, en);
    rig.scan().await;
    let younger = rig
        .text("SELECT id FROM local_artists WHERE display_name = 'En'")
        .expect("the files' row");
    assert_ne!(younger, older);
    assert_eq!(rig.retired_into(&younger), Some(older.clone()));
    assert_eq!(rig.retired_into(&older), None);
    assert_eq!(rig.track_artist("B/01.flac", 0), older);
    assert_eq!(rig.merges(), 1);
}

/// The same, with a file of the older row on an unmatched album: that row
/// is not proven on every credit, so it takes in no other row.
#[tokio::test]
async fn a_row_its_album_proves_only_in_part_takes_in_no_other_row() {
    let rig = ArtistRig::new("artist-proof-partial");
    rig.put("A/01.flac", "Album A", plain(&["Pe"]), plain(&["Pe"]));
    rig.put("U/01.flac", "Album U", plain(&["Pe"]), plain(&["Pe"]));
    rig.scan().await;
    let older = rig.track_artist("A/01.flac", 0);
    assert_eq!(rig.track_artist("U/01.flac", 0), older);
    rig.match_album("A/01.flac", MBID_X, "Pe");
    rig.run(&format!(
        "DELETE FROM local_artist_external_identities WHERE local_artist_id = '{older}'"
    ));
    let en = listed(&["En"], &[MBID_X]);
    rig.put("B/01.flac", "Album B", en, en);
    rig.scan().await;
    let younger = rig
        .text("SELECT id FROM local_artists WHERE display_name = 'En'")
        .expect("the files' row");
    assert_eq!(rig.retired_into(&younger), None);
    assert_eq!(rig.retired_into(&older), None);
    assert_eq!(rig.merges(), 0);
}

/// A file carrying the id the app linked a row to lands on that row
/// under the file's name, and the row takes the name.
#[tokio::test]
async fn a_file_with_an_automatically_linked_id_lands_on_its_row() {
    let rig = ArtistRig::new("artist-automatic-lands");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.scan().await;
    let linked = rig.track_artist("A/01.flac", 0);
    rig.link_by(&linked, MBID_X, "automatic");
    let pe = listed(&["Pe"], &[MBID_X]);
    rig.put("A/01.flac", "Album A", pe, pe);
    rig.scan().await;
    assert_eq!(rig.track_artist("A/01.flac", 0), linked);
    assert_eq!(
        rig.album_artist("A/01.flac"),
        (linked.clone(), linked.clone())
    );
    assert_eq!(rig.name(&linked), "Pe");
}

/// A file named like a row the app linked, carrying another id, does not
/// land on that row.
#[tokio::test]
async fn a_file_with_another_id_stays_off_an_automatically_linked_row() {
    let rig = ArtistRig::new("artist-automatic-other");
    rig.put("A/01.flac", "Album A", plain(&["Oh"]), plain(&["Oh"]));
    rig.scan().await;
    let linked = rig.track_artist("A/01.flac", 0);
    rig.link_by(&linked, MBID_X, "automatic");
    let oh = listed(&["Oh"], &[MBID_Y]);
    rig.put("B/01.flac", "Album B", oh, oh);
    rig.scan().await;
    assert_ne!(rig.track_artist("B/01.flac", 0), linked);
    assert_ne!(rig.album_artist("B/01.flac").0, linked);
    assert_eq!(rig.track_artist("A/01.flac", 0), linked);
}

/// How a hands-off brief's old album is kept from the app.
#[derive(Clone, Copy, PartialEq)]
enum Hands {
    Unmanaged,
    Custom,
    TakenOver,
    /// Matched by the app, nothing kept.
    Automatic,
}

/// SQL that keeps `album` from the app as `hands` says.
fn keep(album: &str, hands: Hands) -> String {
    match hands {
        Hands::Unmanaged => format!(
            "{PERSON} INSERT INTO library_management_exclusions (local_album_id, reason, \
             excluded_by_user_id, excluded_at) VALUES ('{album}', 'by hand', 'u1', 5);"
        ),
        Hands::Custom => format!(
            "{PERSON} INSERT INTO library_custom_edition_manifests (id, local_album_id, \
             version, release_group_mbid, album_title, album_artist_name, \
             source_album_revision, input_revision, content_hash, sealed_by_user_id, \
             sealed_at) VALUES ('m-{album}', '{album}', 1, 'rg-r', 'T', 'A', 1, 'in', 'hash', \
             'u1', 0); \
             INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
             activated_at) VALUES ('{album}', 'm-{album}', 0);"
        ),
        Hands::TakenOver => format!(
            "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
             VALUES ('{album}', 'files_changed', 0);"
        ),
        Hands::Automatic => on_release(album, REL_R),
    }
}

/// How a hands-off brief moves the old album's files in one scan.
#[derive(Clone, Copy)]
struct Move {
    hands: Hands,
    old: &'static str,
    new: &'static str,
    /// Put before each file name where it lands.
    prefix: &'static str,
    count: usize,
    /// How many of the old files are moved (the first ones).
    moved: usize,
    /// The moved files carry a new album name.
    retag: bool,
    /// An album of three files elsewhere in the root, so the mass-missing
    /// guard holds no scope back.
    filler: bool,
    /// Lengths of the old files and of the moved ones, from these seconds.
    old_lengths: Option<f64>,
    new_lengths: Option<f64>,
}

impl Default for Move {
    fn default() -> Self {
        Self {
            hands: Hands::Unmanaged,
            old: "x/old",
            new: "x/new",
            prefix: "",
            count: 3,
            moved: 3,
            retag: true,
            filler: true,
            old_lengths: None,
            new_lengths: None,
        }
    }
}

impl RegroupRig {
    /// Files `folder/tNNN.flac` tagged with `album`, lengths from `from`
    /// seconds when given.
    fn plant(&mut self, folder: &str, count: usize, album: &str, from: Option<f64>) -> Vec<String> {
        let files: Vec<String> = (0..count)
            .map(|n| format!("{folder}/t{n:03}.flac"))
            .collect();
        self.tag(&files, album);
        self.lengths(&files, from);
        files
    }

    fn lengths(&self, files: &[String], from: Option<f64>) {
        let mut lengths = self.reader.lengths.lock().expect("lengths");
        for (n, file) in files.iter().enumerate() {
            match from {
                Some(from) => lengths.insert(file.clone(), from + n as f64),
                None => lengths.remove(file),
            };
        }
    }

    /// Move `files` into `folder` (each name after `prefix`), tagged with
    /// `album`; no scan. Returns where they landed.
    fn move_into(
        &mut self,
        files: &[String],
        folder: &str,
        prefix: &str,
        album: &str,
        from: Option<f64>,
    ) -> Vec<String> {
        let landed: Vec<String> = files
            .iter()
            .map(|file| {
                let name = file
                    .rsplit_once('/')
                    .map_or(file.as_str(), |(_, name)| name);
                format!("{folder}/{prefix}{name}")
            })
            .collect();
        for file in files {
            std::fs::remove_file(self.root.join(file)).expect("removed");
        }
        self.tag(&landed, album);
        self.lengths(&landed, from);
        landed
    }

    /// Albums held as a hands-off album's replacement.
    fn held(&self) -> Vec<String> {
        self.texts(
            "SELECT local_album_id FROM library_album_takeovers \
             WHERE cause = 'replaced_hands_off' ORDER BY local_album_id",
        )
    }

    /// Open pairs recorded for a hands-off album, as "old>new".
    fn hands_off_pairs(&self) -> Vec<String> {
        self.texts(
            "SELECT old_album_id || '>' || new_album_id FROM library_retag_pairs \
             WHERE reason = 'replaced_hands_off' AND state = 'open' \
             ORDER BY old_album_id, new_album_id",
        )
    }

    fn texts(&self, sql: &str) -> Vec<String> {
        let conn =
            droppedneedle_kernel::db::open_connection(&self.db()).expect("side connection opens");
        let mut statement = conn.prepare(sql).expect(sql);
        statement
            .query_map([], |row| row.get(0))
            .expect(sql)
            .collect::<rusqlite::Result<_>>()
            .expect(sql)
    }

    /// `management_stepped_back` actions for a hands-off replacement.
    fn hands_off_actions(&self) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM library_catalog_actions \
             WHERE action_kind = 'management_stepped_back' AND reason_code = 'replaced_hands_off'",
        )
    }
}

fn pair(old: &str, new: &str) -> String {
    format!("{old}>{new}")
}

/// The story `story` tells: the rig, the old album and the album the first
/// moved file is on after the scan.
async fn moved_story(name: &str, story: Move) -> (RegroupRig, String, String) {
    let mut rig = RegroupRig::new(name);
    let before = rig.before.clone();
    let files = rig.plant(story.old, story.count, &before, story.old_lengths);
    if story.filler {
        rig.plant("filler", 3, "Filler", None);
    }
    rig.scan().await;
    let old = rig.album_of(&files[0]);
    rig.run(&keep(&old, story.hands));
    let album = if story.retag {
        rig.after.clone()
    } else {
        before
    };
    let lengths = story.new_lengths.or(story.old_lengths);
    let landed = rig.move_into(
        &files[..story.moved],
        story.new,
        story.prefix,
        &album,
        lengths,
    );
    rig.scan().await;
    let new = rig.album_of(&landed[0]);
    (rig, old, new)
}

/// New files that appear beside an album a person keeps from the app, in
/// the scan that sees that album's files go, are held from automatic
/// writes as that album's replacement; when the old album is empty and
/// the only one they fit, the two are offered as a pair.
#[tokio::test]
async fn new_files_beside_a_hands_off_album_are_held() {
    let rows = [
        (
            "renamed in its folder",
            Move {
                new: "x/old",
                prefix: "r-",
                ..Move::default()
            },
            true,
        ),
        ("moved beside it", Move::default(), true),
        (
            "at the top level",
            Move {
                old: "old",
                new: "new",
                ..Move::default()
            },
            true,
        ),
        (
            "a custom edition",
            Move {
                hands: Hands::Custom,
                ..Move::default()
            },
            true,
        ),
        (
            "taken over",
            Move {
                hands: Hands::TakenOver,
                ..Move::default()
            },
            true,
        ),
        (
            "two of five, three left",
            Move {
                count: 5,
                moved: 2,
                ..Move::default()
            },
            false,
        ),
        (
            "lengths that fit",
            Move {
                old_lengths: Some(100.0),
                ..Move::default()
            },
            true,
        ),
    ];
    for (index, (case, story, paired)) in rows.into_iter().enumerate() {
        let (rig, old, new) = moved_story(&format!("hands-off-held-{index}"), story).await;
        assert_ne!(old, new, "{case}: a new album");
        assert_eq!(rig.held(), vec![new.clone()], "{case}");
        let pairs = if paired {
            vec![pair(&old, &new)]
        } else {
            vec![]
        };
        assert_eq!(rig.hands_off_pairs(), pairs, "{case}");
        assert_eq!(rig.retired_into(&old), None, "{case}");
    }

    // Half of twenty files retagged and renamed beside, the rest deleted:
    // the mass-missing guard holds the scope back and the old rows stay
    // indexed. Held, with no pair while the old album still has rows.
    let (mut rig, _, files) = kept_album("hands-off-held-guard", 20).await;
    let after = rig.after.clone();
    let landed = rig.move_into(&files[..10], "x/new", "", &after, None);
    for file in &files[10..] {
        std::fs::remove_file(rig.root.join(file)).expect("removed");
    }
    rig.scan().await;
    assert_eq!(
        rig.count(
            "SELECT COUNT(*) FROM library_scan_failures WHERE failure_code = 'MASS_MISSING_GUARD'"
        ),
        1,
        "the guard held the scope back"
    );
    assert_eq!(rig.held(), vec![rig.album_of(&landed[0])], "guard");
    assert!(rig.hands_off_pairs().is_empty(), "guard");

    // Held once: a second run end writes nothing more, and a hand-back
    // (the mark lifted) is not undone by a later scan.
    let (rig, old, new) = moved_story("hands-off-held-once", Move::default()).await;
    assert_eq!(rig.hands_off_actions(), 1);
    rig.scan().await;
    assert_eq!(rig.hands_off_actions(), 1);
    assert_eq!(rig.hands_off_pairs(), vec![pair(&old, &new)]);
    rig.run(&format!(
        "DELETE FROM library_album_takeovers WHERE local_album_id = '{new}'"
    ));
    rig.scan().await;
    assert!(rig.held().is_empty());
    assert_eq!(rig.hands_off_actions(), 1);
}

/// Each row lifts one condition of the plain story ("moved beside it"),
/// which holds: nothing is held or recorded.
#[tokio::test]
async fn new_files_are_not_held_without_the_signal() {
    let rows = [
        (
            "an automatic match, nothing kept",
            Move {
                hands: Hands::Automatic,
                ..Move::default()
            },
        ),
        (
            "a folder not beside the old one",
            Move {
                new: "y/new",
                ..Move::default()
            },
        ),
        (
            "moved and renamed, tags unchanged",
            Move {
                retag: false,
                ..Move::default()
            },
        ),
        (
            "retagged in place",
            Move {
                new: "x/old",
                ..Move::default()
            },
        ),
        (
            "lengths that fit no lost file",
            Move {
                old_lengths: Some(100.0),
                new_lengths: Some(500.0),
                ..Move::default()
            },
        ),
    ];
    for (index, (case, story)) in rows.into_iter().enumerate() {
        let (rig, _, _) = moved_story(&format!("hands-off-not-{index}"), story).await;
        assert!(rig.held().is_empty(), "{case}");
        assert!(rig.hands_off_pairs().is_empty(), "{case}");
        assert_eq!(rig.hands_off_actions(), 0, "{case}");
    }
}

/// A hands-off album of `count` files in `x/old`, in a rig with a filler
/// album: the rig, the album and its files.
async fn kept_album(name: &str, count: usize) -> (RegroupRig, String, Vec<String>) {
    let mut rig = RegroupRig::new(name);
    let before = rig.before.clone();
    let files = rig.plant("x/old", count, &before, None);
    rig.plant("filler", 3, "Filler", None);
    rig.scan().await;
    let old = rig.album_of(&files[0]);
    rig.run(&keep(&old, Hands::Unmanaged));
    (rig, old, files)
}

/// A retag spread over scans: held at the first scan's end, the pair once
/// the old album is empty.
#[tokio::test]
async fn a_retag_two_scans_see_is_held_then_paired() {
    let (mut rig, old, files) = kept_album("hands-off-two-runs", 4).await;
    let after = rig.after.clone();
    let landed = rig.move_into(&files[..2], "x/new", "", &after, None);
    rig.scan().await;
    let new = rig.album_of(&landed[0]);
    assert_eq!(rig.held(), vec![new.clone()], "first sitting");
    assert!(rig.hands_off_pairs().is_empty(), "first sitting");
    rig.move_into(&files[2..], "x/new", "", &after, None);
    rig.scan().await;
    assert_eq!(rig.held(), vec![new.clone()], "second sitting");
    assert_eq!(
        rig.hands_off_pairs(),
        vec![pair(&old, &new)],
        "second sitting"
    );

    // Two retagged and renamed, then the other three deleted.
    // With real lengths: the files deleted later fit none of the new ones.
    let (mut rig, old, files) = kept_album("hands-off-delete-rest", 5).await;
    let before = rig.before.clone();
    rig.tag(&files, &before);
    rig.lengths(&files, Some(100.0));
    rig.scan().await;
    let after = rig.after.clone();
    let landed = rig.move_into(&files[..2], "x/new", "", &after, Some(100.0));
    rig.scan().await;
    let new = rig.album_of(&landed[0]);
    assert_eq!(rig.held(), vec![new.clone()], "retagged");
    assert!(rig.hands_off_pairs().is_empty(), "retagged");
    rig.lose(&files[2..]).await;
    assert_eq!(
        rig.hands_off_pairs(),
        vec![pair(&old, &new)],
        "the rest deleted"
    );
}

/// Copying first and deleting later (or the reverse) is two scans: the
/// new files are not held. Both in one scan, they are.
#[tokio::test]
async fn new_files_and_the_loss_in_different_scans_are_not_held() {
    for (case, copy_first) in [("copy first", true), ("delete first", false)] {
        let (mut rig, _, files) = kept_album(&format!("hands-off-apart-{copy_first}"), 3).await;
        let after = rig.after.clone();
        let copies: Vec<String> = files
            .iter()
            .map(|file| file.replace("x/old", "x/new"))
            .collect();
        if copy_first {
            rig.tag(&copies, &after);
            rig.scan().await;
            rig.lose(&files).await;
        } else {
            rig.lose(&files).await;
            rig.tag(&copies, &after);
            rig.scan().await;
        }
        assert!(rig.held().is_empty(), "{case}");
        assert!(rig.hands_off_pairs().is_empty(), "{case}");
    }
    let (mut rig, old, files) = kept_album("hands-off-apart-twin", 3).await;
    let after = rig.after.clone();
    let landed = rig.move_into(&files, "x/new", "", &after, None);
    rig.scan().await;
    let new = rig.album_of(&landed[0]);
    assert_eq!(rig.held(), vec![new.clone()], "one scan");
    assert_eq!(rig.hands_off_pairs(), vec![pair(&old, &new)], "one scan");
}

/// A new album that also took a track from another album is not a
/// replacement; without that track it is.
#[tokio::test]
async fn a_new_album_holding_a_moved_track_is_not_held() {
    for moved_onto in [true, false] {
        let mut rig = RegroupRig::new(&format!("hands-off-moved-onto-{moved_onto}"));
        let before = rig.before.clone();
        let files = rig.plant("x/old", 3, &before, None);
        rig.plant("filler", 3, "Filler", None);
        let other = vec!["x/new/c.flac".to_owned()];
        rig.tag(&other, "Other");
        rig.scan().await;
        let old = rig.album_of(&files[0]);
        rig.run(&keep(&old, Hands::Unmanaged));
        let after = rig.after.clone();
        let landed = rig.move_into(&files, "x/new", "", &after, None);
        if moved_onto {
            rig.tag(&other, &after);
        }
        rig.scan().await;
        let new = rig.album_of(&landed[0]);
        if moved_onto {
            assert!(rig.held().is_empty(), "a track moved onto it");
        } else {
            assert_eq!(rig.held(), vec![new], "only new files");
        }
    }
}

/// One album split into two new ones beside it: both held, two pairs.
/// Two hands-off albums losing files beside one new album: held, with no
/// pair while both fit it, and a pair with the one it fits by length. At
/// the top level, an album deleted and two unrelated ones copied in: held
/// only where the lengths fit.
#[tokio::test]
async fn splits_and_several_albums_beside() {
    let (mut rig, old, files) = kept_album("hands-off-split", 4).await;
    let first = rig.move_into(&files[..2], "x/one", "", "One", None);
    let second = rig.move_into(&files[2..], "x/two", "", "Two", None);
    rig.scan().await;
    let (one, two) = (rig.album_of(&first[0]), rig.album_of(&second[0]));
    let mut held = vec![one.clone(), two.clone()];
    held.sort();
    assert_eq!(rig.held(), held, "split");
    let mut pairs = vec![pair(&old, &one), pair(&old, &two)];
    pairs.sort();
    assert_eq!(rig.hands_off_pairs(), pairs, "split");

    for fits_one in [false, true] {
        let mut rig = RegroupRig::new(&format!("hands-off-two-old-{fits_one}"));
        let lengths = |from: f64| fits_one.then_some(from);
        let first = rig.plant("x/one", 2, "First", lengths(100.0));
        let second = rig.plant("x/two", 2, "Second", lengths(200.0));
        rig.plant("filler", 3, "Filler", None);
        rig.scan().await;
        let (a1, a2) = (rig.album_of(&first[0]), rig.album_of(&second[0]));
        rig.run(&keep(&a1, Hands::Unmanaged));
        rig.run(&keep(&a2, Hands::Unmanaged));
        let after = rig.after.clone();
        let landed = if fits_one {
            for file in &second {
                std::fs::remove_file(rig.root.join(file)).expect("removed");
            }
            rig.move_into(&first, "x/new", "", &after, lengths(100.0))
        } else {
            let mut all = first.clone();
            all.extend(second.clone());
            rig.move_into(&all, "x/new", "", &after, None)
        };
        rig.scan().await;
        let new = rig.album_of(&landed[0]);
        assert_eq!(rig.held(), vec![new.clone()], "fits one: {fits_one}");
        let pairs = if fits_one {
            vec![pair(&a1, &new)]
        } else {
            vec![]
        };
        assert_eq!(rig.hands_off_pairs(), pairs, "fits one: {fits_one}");
    }

    for fits in [false, true] {
        let mut rig = RegroupRig::new(&format!("hands-off-flat-{fits}"));
        let files = rig.plant("X - Live", 2, "Live", Some(100.0));
        rig.plant("filler", 3, "Filler", None);
        rig.scan().await;
        let old = rig.album_of(&files[0]);
        rig.run(&keep(&old, Hands::Unmanaged));
        for file in &files {
            std::fs::remove_file(rig.root.join(file)).expect("removed");
        }
        let length = if fits { 100.0 } else { 300.0 };
        let y = rig.plant("Y - New", 2, "New", Some(length));
        rig.plant("Z - Other", 2, "Other", Some(400.0));
        rig.scan().await;
        if fits {
            let new = rig.album_of(&y[0]);
            assert_eq!(rig.held(), vec![new.clone()], "flat, fits");
            assert_eq!(rig.hands_off_pairs(), vec![pair(&old, &new)], "flat, fits");
        } else {
            assert!(rig.held().is_empty(), "flat");
            assert!(rig.hands_off_pairs().is_empty(), "flat");
        }
    }
}

/// A 300-file hands-off album retagged into `b/` over two windows, with
/// `meanwhile` run between them (or the scan stopped there): the rig, the
/// old album and the new one.
async fn big_retag(
    name: &str,
    meanwhile: Option<&str>,
    stop: bool,
) -> (RegroupRig, String, String) {
    let (mut rig, files, old) = big_album(name).await;
    rig.run(&keep(&old, Hands::Unmanaged));
    let after = rig.after.clone();
    let landed = rig.move_into(&files, "b", "", &after, None);
    if let Some(sql) = meanwhile {
        rig.meanwhile_sql(sql.to_owned());
    }
    if stop {
        rig.stop_before(LAST_FILE);
        assert_eq!(
            rig.scan_to_its_end().await.state.scan_state(),
            ScanState::Cancelled
        );
    } else {
        rig.scan().await;
    }
    let new = rig.album_of(&landed[0]);
    (rig, old, new)
}

/// A large retag: held when the scan stops after its first window (the
/// pair at the next scan's end); not held when a person chose the new
/// album's match while it ran, or its first rows came from an import.
#[tokio::test]
async fn a_large_retag_is_held_by_what_the_scan_saw() {
    let (rig, old, new) = big_retag("hands-off-big-stopped", None, true).await;
    assert_eq!(rig.held(), vec![new.clone()], "stopped");
    assert!(rig.hands_off_pairs().is_empty(), "stopped");
    rig.scan().await;
    assert_eq!(
        rig.hands_off_pairs(),
        vec![pair(&old, &new)],
        "the next scan"
    );

    let (rig, _, new) = big_retag("hands-off-big-twin", None, false).await;
    assert_eq!(rig.held(), vec![new], "the twin");

    let chosen = "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at) \
         SELECT DISTINCT local_album_id, 'musicbrainz', 'rg-1', 'rel-1', 'manual', 0 \
         FROM local_tracks WHERE relative_path LIKE 'b/%';";
    let imported = "UPDATE local_tracks SET ingest_source = 'download' \
         WHERE relative_path LIKE 'b/%';";
    for (index, (case, sql)) in [("a person's match", chosen), ("imported rows", imported)]
        .into_iter()
        .enumerate()
    {
        let (rig, _, _) = big_retag(&format!("hands-off-big-{index}"), Some(sql), false).await;
        assert!(rig.held().is_empty(), "{case}");
        assert!(rig.hands_off_pairs().is_empty(), "{case}");
    }
}

/// An album only taken over (no "leave unmanaged"), matched to an edition:
/// its replacement is held, and paired once the old files are marked
/// missing. Once the new album is matched to the same edition, the join on
/// proof still goes ahead and the new album stays held. Matched to another
/// edition, the two stay a pair.
#[tokio::test]
async fn a_taken_over_album_is_still_joined_on_proof() {
    for same in [true, false] {
        let (mut rig, old, old_rows) = mapped_album(&format!("hands-off-proof-{same}"), 2).await;
        rig.run(&keep(&old, Hands::TakenOver));
        let after = rig.after.clone();
        let new_rows = rig.replace(&[0, 1], "b", &after).await;
        let new = rig.album_of("b/t000.flac");
        assert_eq!(rig.held(), vec![new.clone()], "held at once");
        rig.scan().await;
        assert_eq!(rig.hands_off_pairs(), vec![pair(&old, &new)], "paired");
        let rows: Vec<(&str, usize)> = new_rows.iter().map(String::as_str).zip(0..).collect();
        rig.match_rows(&new, if same { REL_R } else { "rel-other" }, &rows);
        rig.scan().await;
        assert_eq!(rig.mark_of(&new).as_deref(), Some("replaced_hands_off"));
        if same {
            assert_eq!(rig.retired_into(&old).as_deref(), Some(new.as_str()));
            assert_eq!(rig.stars_and_aliases(&old_rows[0]).0, 0);
            assert_eq!(rig.stars_and_aliases(&new_rows[0]).0, 1);
        } else {
            assert_eq!(rig.retired_into(&old), None);
            assert_eq!(rig.hands_off_pairs(), vec![pair(&old, &new)]);
        }
    }
}

// ---------------------------------------------------------------------------
// Other music put at a track's path.
// ---------------------------------------------------------------------------

/// What one file says about itself: its title tag and recording id (empty
/// for none) and its length.
#[derive(Clone, Copy)]
struct Says {
    title: &'static str,
    recording: &'static str,
    length: Option<f64>,
}

const fn says(title: &'static str, recording: &'static str, length: Option<f64>) -> Says {
    Says {
        title,
        recording,
        length,
    }
}

impl RegroupRig {
    /// Write each file anew as track `n` of `album`, saying what `says`
    /// gives.
    fn say(&mut self, files: &[(&str, u32, Says)], album: &str) {
        for (file, n, says) in files {
            self.reader
                .titles
                .lock()
                .expect("titles")
                .insert((*file).to_owned(), says.title.to_owned());
            self.reader
                .ids
                .lock()
                .expect("ids")
                .insert((*file).to_owned(), ids("", "", says.recording, 1, *n));
            let mut lengths = self.reader.lengths.lock().expect("lengths");
            match says.length {
                Some(length) => lengths.insert((*file).to_owned(), length),
                None => lengths.remove(*file),
            };
        }
        let names: Vec<String> = files
            .iter()
            .map(|(file, _, _)| (*file).to_owned())
            .collect();
        self.tag(&names, album);
    }

    /// The row at a path, whatever its state.
    fn row_at(&self, relative: &str) -> String {
        self.text(&format!(
            "SELECT id FROM local_tracks WHERE relative_path = '{relative}'"
        ))
        .unwrap_or_else(|| panic!("a row at {relative}"))
    }

    /// Whether `track` is missing at the hidden name it gave its path
    /// `relative` up for.
    fn retired_at(&self, track: &str, relative: &str) -> bool {
        self.count(&format!(
            "SELECT COUNT(*) FROM local_tracks WHERE id = '{track}' \
             AND availability = 'missing' \
             AND relative_path = '{HIDDEN_PREFIX}gone/{track}/{relative}'"
        )) == 1
    }

    /// Every pair the `other_music_at_path` actions name, as (album, old
    /// track, new track), in the order written.
    fn other_music(&self) -> Vec<(String, String, String)> {
        let conn = droppedneedle_kernel::db::open_connection(&self.db()).expect("opens");
        let mut stmt = conn
            .prepare(
                "SELECT a.local_album_id, json_extract(p.value, '$.old_track_id'), \
                 json_extract(p.value, '$.new_track_id') FROM library_catalog_actions a, \
                 json_each(a.after_json, '$.pairs') p \
                 WHERE a.action_kind = 'other_music_at_path' \
                 AND a.reason_code = 'OTHER_MUSIC_AT_PATH' AND a.actor_user_id IS NULL \
                 ORDER BY a.created_at, a.rowid, p.key",
            )
            .expect("prepares");
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("reads")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    fn other_music_actions(&self, album: &str) -> i64 {
        self.count(&format!(
            "SELECT COUNT(*) FROM library_catalog_actions \
             WHERE action_kind = 'other_music_at_path' AND local_album_id = '{album}'"
        ))
    }
}

use droppedneedle_library::publish::HIDDEN_PREFIX;

/// When the latest scan run started.
const LATEST_START: &str = "(SELECT MAX(started_at) FROM library_scan_runs)";

/// Another album copied over an old one with the same file names: the
/// three files whose length moved more than five seconds come in as new
/// tracks on a new album, and the old tracks stay on the old album,
/// missing, with everything people made of them. The fourth, within five
/// seconds, keeps its track and moves. The old album is not handed to the
/// new music, not even a scan later when the old tracks have been gone a
/// day, and its history says what happened.
#[tokio::test]
async fn another_album_copied_over_an_old_one_comes_in_as_new_tracks() {
    let mut rig = RegroupRig::new("scan-other-music-album");
    let files = ["a/01.flac", "a/02.flac", "a/03.flac", "a/04.flac"];
    let before = rig.before.clone();
    rig.say(
        &[
            (files[0], 1, says("Old One", "rec-a1", Some(200.0))),
            (files[1], 2, says("Old Two", "rec-a2", Some(210.0))),
            (files[2], 3, says("Old Three", "rec-a3", Some(220.0))),
            (files[3], 4, says("Old Four", "rec-a4", Some(230.0))),
        ],
        &before,
    );
    rig.scan().await;
    let album = rig.album_of(files[0]);
    let tracks: Vec<String> = files.iter().map(|file| rig.row_at(file)).collect();
    let mut setup = format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at) \
         VALUES ('{album}', 'musicbrainz', 'rg-r', '{REL_R}', 'manual', 0); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'album', '{album}', 1); \
         INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES ('play-1', 'listener', '{}', '{album}', \
         'Old Two', 'Artist', '2026-01-01T00:00:00Z'); \
         INSERT INTO playlists (id, name, created_at, updated_at) VALUES ('list', 'List', '', ''); \
         INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
         album_name, album_id, track_source_id, source_type, created_at) VALUES ('entry', \
         'list', 0, 'Old One', 'Artist', 'Before', '{album}', '{}', 'local', ''); \
         INSERT INTO library_track_management_state (local_track_id, written_sha256) \
         VALUES ('{}', 'sha');",
        tracks[1], tracks[0], tracks[2]
    );
    setup += &star(&tracks[0]);
    for (n, track) in tracks.iter().enumerate() {
        setup += &map_track(track, Some(REL_R), Some(&format!("rt-{n}")));
    }
    rig.run(&setup);

    let new_album = "Another Album";
    rig.say(
        &[
            (files[0], 1, says("New One", "rec-c1", Some(230.0))),
            (files[1], 2, says("New Two", "rec-c2", Some(240.0))),
            (files[2], 3, says("New Three", "rec-c3", Some(250.0))),
            (files[3], 4, says("New Four", "rec-c4", Some(233.0))),
        ],
        new_album,
    );
    rig.scan().await;

    let new_rows: Vec<String> = files[..3].iter().map(|file| rig.row_at(file)).collect();
    let b = rig.album_of(files[0]);
    assert_ne!(b, album, "the old album row is not handed to the new music");
    for (n, file) in files[..3].iter().enumerate() {
        let (old, new) = (&tracks[n], &new_rows[n]);
        assert_ne!(old, new, "{file} is a new track");
        assert!(rig.retired_at(old, file), "{file}'s old track is retired");
        assert_eq!(rig.album_of(file), b, "{file} is on the new album");
        assert_eq!(
            rig.count(&format!(
                "SELECT COUNT(*) FROM local_tracks WHERE id = '{old}' \
                 AND local_album_id = '{album}' AND missing_since >= {LATEST_START}"
            )),
            1,
            "{file}'s old track went missing in this run, on the old album"
        );
        assert_eq!(
            rig.count(&format!(
                "SELECT (SELECT COUNT(*) FROM local_track_external_identities \
                   WHERE local_track_id = '{old}' AND release_track_mbid = 'rt-{n}') \
                 + 10 * (SELECT COUNT(*) FROM local_track_external_identities \
                   WHERE local_track_id = '{new}') \
                 + 100 * (SELECT COUNT(*) FROM library_track_management_state \
                   WHERE local_track_id = '{new}')"
            )),
            1,
            "{file}: the mapping stays on the old track; the new one has none and no record"
        );
    }
    assert_eq!(rig.stars_and_aliases(&tracks[0]), (1, 0));
    assert_eq!(
        rig.count(&format!(
            "SELECT (SELECT COUNT(*) FROM library_play_history \
               WHERE local_track_id = '{}' AND local_album_id = '{album}') \
             + (SELECT COUNT(*) FROM playlist_tracks WHERE track_source_id = '{}') \
             + (SELECT COUNT(*) FROM library_track_management_state \
               WHERE local_track_id = '{}' AND written_sha256 = 'sha')",
            tracks[1], tracks[0], tracks[2]
        )),
        3,
        "the play, the playlist entry and the record stay"
    );
    assert_eq!(rig.row_at(files[3]), tracks[3], "within five seconds");
    assert_eq!(rig.album_of(files[3]), b);
    assert_eq!(
        rig.text(&format!("SELECT title FROM local_albums WHERE id = '{b}'")),
        Some(new_album.to_owned())
    );

    let expected: Vec<(String, String, String)> = (0..3)
        .map(|n| (album.clone(), tracks[n].clone(), new_rows[n].clone()))
        .collect();
    assert_eq!(rig.other_music(), expected);
    assert_eq!(rig.other_music_actions(&album), 1);
    let conn = droppedneedle_kernel::db::open_connection(&rig.db()).expect("opens");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs_f64();
    use droppedneedle_library::operations::history::{HistoryKind, album_history};
    let told: Vec<(HistoryKind, usize, Vec<String>)> =
        album_history(&conn, &album, "listener", now)
            .expect("history reads")
            .expect("album live")
            .into_iter()
            .map(|entry| (entry.kind, entry.track_count, entry.track_titles))
            .collect();
    let titles = ["Old One", "Old Two", "Old Three"]
        .map(str::to_owned)
        .to_vec();
    assert_eq!(told, vec![(HistoryKind::TracksOtherMusic, 3, titles)]);

    let still_apart = |rig: &RegroupRig, when: &str| {
        assert_eq!(rig.retired_into(&album), None, "{when}");
        assert_eq!(
            rig.text(&format!(
                "SELECT title FROM local_albums WHERE id = '{album}'"
            )),
            Some(before.clone()),
            "{when}"
        );
        assert_eq!(
            rig.count(&format!(
                "SELECT (SELECT COUNT(*) FROM library_user_favorites \
                   WHERE item_kind = 'album' AND item_id = '{album}') \
                 + (SELECT COUNT(*) FROM local_album_external_identities \
                   WHERE local_album_id = '{album}' AND release_mbid = '{REL_R}')"
            )),
            2,
            "{when}: the star and the match stay"
        );
    };
    still_apart(&rig, "after the scan");

    rig.age_missing(2 * 86_400);
    rig.scan().await;
    still_apart(&rig, "a scan later");
    assert_eq!(
        rig.count("SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = 'merge_album'"),
        0
    );
    assert!(retag_pairs(&rig).is_empty());
    assert_eq!(rig.other_music_actions(&album), 1);
}

/// Where the line is between the same track and other music at its path:
/// a length change of more than five seconds and a recording id that
/// differs, or, when either file lacks one, titles from both files' tags
/// that differ. One file per folder; each row is a row of the task's
/// table.
#[tokio::test]
async fn other_music_at_a_path_needs_a_length_gap_and_a_contradiction() {
    struct Line {
        old: Says,
        new: Says,
        exclude: bool,
        lose_first: bool,
        new_track: bool,
    }
    const SONG: Says = says("Song", "x", Some(200.0));
    const UNNAMED: Says = says("Song", "", Some(200.0));
    let line = |old: Says, new: Says, new_track: bool| Line {
        old,
        new,
        exclude: false,
        lose_first: false,
        new_track,
    };
    let lines = [
        line(SONG, says("Song", "y", Some(230.0)), true),
        line(UNNAMED, says("Other", "y", Some(230.0)), true),
        line(UNNAMED, says("Other", "", Some(230.0)), true),
        line(SONG, says("Other", "y", Some(205.5)), true),
        line(SONG, says("Other", "y", Some(204.5)), false),
        line(SONG, says("Other", "y", Some(200.0)), false),
        line(SONG, says("Other", "x", Some(230.0)), false),
        line(UNNAMED, says("Song", "", Some(230.0)), false),
        line(
            says("Song", "x", None),
            says("Other", "y", Some(230.0)),
            false,
        ),
        line(SONG, says("Other", "y", None), false),
        Line {
            exclude: true,
            ..line(SONG, says("Other", "y", Some(230.0)), false)
        },
        Line {
            lose_first: true,
            ..line(SONG, says("Other", "y", Some(230.0)), true)
        },
        line(UNNAMED, says("", "", Some(230.0)), false),
        line(
            says("", "", Some(200.0)),
            says("Other", "y", Some(230.0)),
            false,
        ),
    ];
    let mut rig = RegroupRig::new("scan-other-music-line");
    let file = |n: usize| format!("line{:02}/t.flac", n + 1);
    for (n, line) in lines.iter().enumerate() {
        rig.say(&[(&file(n), 1, line.old)], &format!("Line {}", n + 1));
    }
    rig.scan().await;
    let old: Vec<String> = (0..lines.len()).map(|n| rig.row_at(&file(n))).collect();
    let since = |rig: &RegroupRig, track: &str| {
        rig.text(&format!(
            "SELECT CAST(missing_since AS TEXT) FROM local_tracks WHERE id = '{track}'"
        ))
    };
    let mut missing_since = BTreeMap::new();
    for (n, line) in lines.iter().enumerate() {
        if line.exclude {
            rig.run(&format!(
                "UPDATE local_tracks SET manual_excluded = 1, availability = 'excluded', \
                 excluded_at = 1 WHERE id = '{}';",
                old[n]
            ));
        }
        if line.lose_first {
            rig.lose(&[file(n)]).await;
            missing_since.insert(n, since(&rig, &old[n]).expect("missing since"));
        }
    }
    for (n, line) in lines.iter().enumerate() {
        rig.say(&[(&file(n), 1, line.new)], &format!("Line {}", n + 1));
    }
    rig.scan().await;

    let pairs = rig.other_music();
    for (n, line) in lines.iter().enumerate() {
        let row = n + 1;
        let path = file(n);
        let now = rig.row_at(&path);
        let named: Vec<&(String, String, String)> =
            pairs.iter().filter(|(_, was, _)| *was == old[n]).collect();
        if line.new_track {
            assert_ne!(now, old[n], "row {row}: a new track");
            assert!(rig.retired_at(&old[n], &path), "row {row}: retired");
            assert_eq!(named.len(), 1, "row {row}: one pair");
            assert_eq!(named[0].2, now, "row {row}: the pair names the new track");
        } else {
            assert_eq!(now, old[n], "row {row}: the same track");
            assert!(named.is_empty(), "row {row}: no action");
        }
        if line.exclude {
            assert_eq!(
                rig.text(&format!(
                    "SELECT availability FROM local_tracks WHERE id = '{now}'"
                ))
                .as_deref(),
                Some("excluded"),
                "row {row}: still excluded"
            );
        }
        if let Some(kept) = missing_since.get(&n) {
            assert_eq!(
                since(&rig, &old[n]).as_ref(),
                Some(kept),
                "row {row}: keeps its own missing_since"
            );
        }
    }
    assert_eq!(pairs.len(), 5);
}

/// The old file put back at its path after other music took it: the old
/// track comes back with its id and star, the other music's track is
/// retired in turn, and the old track no longer counts as replaced.
#[tokio::test]
async fn the_old_file_put_back_brings_the_old_track_back() {
    let mut rig = RegroupRig::new("scan-other-music-back");
    let before = rig.before.clone();
    let original = says("Song", "rec-song", Some(200.0));
    rig.say(
        &[
            ("a/01.flac", 1, original),
            ("a/02.flac", 2, says("Next", "rec-next", Some(210.0))),
        ],
        &before,
    );
    rig.scan().await;
    let album = rig.album_of("a/01.flac");
    let old = rig.row_at("a/01.flac");
    rig.run(&star(&old));

    rig.say(
        &[("a/01.flac", 1, says("Other", "rec-other", Some(230.0)))],
        &before,
    );
    rig.scan().await;
    let other = rig.row_at("a/01.flac");
    assert_ne!(other, old);
    assert!(rig.retired_at(&old, "a/01.flac"));

    rig.say(&[("a/01.flac", 1, original)], &before);
    rig.scan().await;
    assert_eq!(rig.row_at("a/01.flac"), old, "the old track is back");
    assert_eq!(
        rig.text(&format!(
            "SELECT availability FROM local_tracks WHERE id = '{old}'"
        ))
        .as_deref(),
        Some("indexed")
    );
    assert_eq!(rig.stars_and_aliases(&old), (1, 0));
    assert!(rig.retired_at(&other, "a/01.flac"));
    assert_eq!(
        rig.other_music(),
        vec![
            (album.clone(), old.clone(), other.clone()),
            (album.clone(), other.clone(), old.clone()),
        ]
    );
    assert_eq!(
        rig.count(&format!(
            "SELECT COUNT(*) FROM local_tracks t WHERE t.id = '{old}' \
             AND EXISTS (SELECT 1 FROM library_catalog_actions a, \
               json_each(a.after_json, '$.pairs') p \
               WHERE a.local_album_id = t.local_album_id \
               AND a.action_kind = 'other_music_at_path' \
               AND json_extract(p.value, '$.old_track_id') = t.id) \
             AND substr(t.relative_path, 1, length('{HIDDEN_PREFIX}gone/' || t.id || '/')) \
               = '{HIDDEN_PREFIX}gone/' || t.id || '/'"
        )),
        0,
        "the old track no longer counts as replaced by other music"
    );
}

// ---------------------------------------------------------------------------
// A new copy of the same recording at a track's path.
// ---------------------------------------------------------------------------

/// What one file says about itself, for the new-copy briefs.
#[derive(Clone, Copy)]
struct Rip {
    title: &'static str,
    recording: &'static str,
    release: &'static str,
    /// The album title; `None` for the row's own.
    album: Option<&'static str>,
    track: u32,
    length: f64,
    stream: Stream,
}

/// Recording X, title `T` from tags, track 1, 200 s at 44.1 kHz, 16-bit,
/// stereo, no release id.
const RIP: Rip = Rip {
    title: "T",
    recording: "X",
    release: "",
    album: None,
    track: 1,
    length: 200.0,
    stream: (44_100, 16, 2),
};

/// [`RIP`] at 96 kHz.
const HI_RES: Rip = Rip {
    stream: (96_000, 16, 2),
    ..RIP
};

impl RegroupRig {
    /// Write the file anew on `album` (or the album the rip names),
    /// saying what `rip` gives.
    fn rip(&mut self, file: &str, album: &str, rip: Rip) {
        let file = file.to_owned();
        self.reader
            .titles
            .lock()
            .expect("titles")
            .insert(file.clone(), rip.title.to_owned());
        self.reader.ids.lock().expect("ids").insert(
            file.clone(),
            ids(rip.release, "", rip.recording, 1, rip.track),
        );
        self.reader
            .lengths
            .lock()
            .expect("lengths")
            .insert(file.clone(), rip.length);
        self.reader
            .streams
            .lock()
            .expect("streams")
            .insert(file.clone(), rip.stream);
        self.tag(&[file], rip.album.unwrap_or(album));
    }
}

/// Where the line is between a new copy of a track's recording and a hand
/// edit: a FLAC on the same album, disc and track, the same recording by
/// id or, when either file has none, by tag title within five seconds,
/// whose sample rate, bit depth or channels changed or whose length moved
/// by more than a second, on an album the app has not stepped back from.
/// A new copy loses the app's record of the old file; anything else keeps
/// it. One file per folder; each line is a row of the task's table.
#[tokio::test]
async fn a_new_copy_at_a_path_needs_the_same_recording_and_new_audio() {
    use droppedneedle_library::operations::history::{HistoryKind, album_history};
    use droppedneedle_library::publish::record::record;

    const V3: &str = "INSERT INTO library_track_management_state (local_track_id, \
                      written_sha256) VALUES ('{track}', 'x');";
    struct Line {
        file: &'static str,
        old: Rip,
        new: Rip,
        /// Run after the first scan; `{track}` and `{album}` name the row.
        setup: &'static str,
        lose_first: bool,
        cleared: bool,
    }
    let line = |new: Rip, cleared: bool| Line {
        file: "t.flac",
        old: RIP,
        new,
        setup: V3,
        lose_first: false,
        cleared,
    };
    let no_ids = Rip {
        title: "",
        recording: "",
        ..RIP
    };
    let lines = [
        line(HI_RES, true),
        line(
            Rip {
                stream: (44_100, 24, 2),
                ..RIP
            },
            true,
        ),
        line(
            Rip {
                stream: (44_100, 16, 1),
                ..RIP
            },
            true,
        ),
        line(
            Rip {
                length: 201.5,
                ..RIP
            },
            true,
        ),
        line(
            Rip {
                length: 200.5,
                ..RIP
            },
            false,
        ),
        line(
            Rip {
                recording: "Y",
                ..HI_RES
            },
            false,
        ),
        line(
            Rip {
                recording: "",
                ..HI_RES
            },
            true,
        ),
        line(
            Rip {
                recording: "",
                title: "U",
                ..HI_RES
            },
            false,
        ),
        Line {
            old: no_ids,
            ..line(
                Rip {
                    recording: "",
                    title: "t",
                    ..HI_RES
                },
                false,
            )
        },
        line(
            Rip {
                recording: "",
                length: 208.0,
                ..HI_RES
            },
            false,
        ),
        Line {
            file: "t.mp3",
            ..line(HI_RES, false)
        },
        line(Rip { track: 2, ..HI_RES }, false),
        line(
            Rip {
                album: Some("Copy elsewhere"),
                ..HI_RES
            },
            false,
        ),
        Line {
            setup: "",
            ..line(HI_RES, false)
        },
        Line {
            setup: "INSERT INTO library_track_management_state (local_track_id, \
                    managed_path_revision, last_outcome) VALUES ('{track}', 'v2', 'succeeded');",
            ..line(HI_RES, true)
        },
        Line {
            setup: "INSERT INTO library_track_management_state (local_track_id, \
                    written_sha256, managed_path_revision, last_outcome) \
                    VALUES ('{track}', 'x', 'v2', 'succeeded');",
            ..line(HI_RES, true)
        },
        Line {
            setup: "INSERT INTO library_track_management_state (local_track_id, \
                    written_sha256) VALUES ('{track}', 'x'); \
                    INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
                    VALUES ('{album}', 'files_changed', 0);",
            ..line(HI_RES, false)
        },
        Line {
            setup: "INSERT INTO library_track_management_state (local_track_id, \
                    written_sha256) VALUES ('{track}', 'x'); \
                    UPDATE local_tracks SET manual_excluded = 1, availability = 'excluded', \
                    excluded_at = 1 WHERE id = '{track}';",
            ..line(HI_RES, false)
        },
        Line {
            setup: "INSERT INTO library_track_management_state (local_track_id, \
                    written_sha256) VALUES ('{track}', 'x'); \
                    UPDATE local_tracks SET sample_rate = NULL WHERE id = '{track}';",
            ..line(HI_RES, false)
        },
        Line {
            lose_first: true,
            ..line(HI_RES, true)
        },
        Line {
            old: Rip {
                release: "rel-copy",
                ..RIP
            },
            ..line(
                Rip {
                    release: "rel-copy",
                    album: Some("Copy renamed"),
                    ..HI_RES
                },
                true,
            )
        },
    ];
    let names = [
        "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "15b",
        "16", "17", "18", "19", "20",
    ];
    assert_eq!(names.len(), lines.len());
    let mut rig = RegroupRig::new("scan-new-copy-line");
    let file = |n: usize| format!("copy{:02}/{}", n + 1, lines[n].file);
    let album_name = |n: usize| format!("Copy {}", names[n]);
    for (n, line) in lines.iter().enumerate() {
        rig.rip(&file(n), &album_name(n), line.old);
    }
    rig.scan().await;
    let tracks: Vec<String> = (0..lines.len()).map(|n| rig.row_at(&file(n))).collect();
    let albums: Vec<String> = tracks
        .iter()
        .map(|track| {
            rig.text(&format!(
                "SELECT local_album_id FROM local_tracks WHERE id = '{track}'"
            ))
            .expect("an album")
        })
        .collect();
    for (n, line) in lines.iter().enumerate() {
        rig.run(
            &line
                .setup
                .replace("{track}", &tracks[n])
                .replace("{album}", &albums[n]),
        );
    }
    for (n, line) in lines.iter().enumerate() {
        if line.lose_first {
            rig.lose(&[file(n)]).await;
        }
    }
    let state = |rig: &RegroupRig, track: &str| {
        rig.text(&format!(
            "SELECT COALESCE(written_sha256, '-') || ' ' || COALESCE(managed_path_revision, '-') \
             || ' ' || row_revision FROM library_track_management_state \
             WHERE local_track_id = '{track}'"
        ))
    };
    let kept: Vec<Option<String>> = tracks.iter().map(|track| state(&rig, track)).collect();
    rig.run("DELETE FROM library_management_automatic_queue;");
    for (n, line) in lines.iter().enumerate() {
        rig.rip(&file(n), &album_name(n), line.new);
    }
    rig.scan().await;

    let conn = droppedneedle_kernel::db::open_connection(&rig.db()).expect("opens");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs_f64();
    for (n, line) in lines.iter().enumerate() {
        let row = names[n];
        let (track, album) = (&tracks[n], &albums[n]);
        assert_eq!(&rig.row_at(&file(n)), track, "row {row}: the same track");
        let actions = rig.count(&format!(
            "SELECT COUNT(*) FROM library_catalog_actions a, json_each(a.after_json, '$.tracks') t \
             WHERE a.action_kind = 'new_copy_at_path' AND a.reason_code = 'NEW_COPY_AT_PATH' \
             AND a.local_album_id = '{album}' AND json_extract(t.value, '$.track_id') = '{track}'"
        ));
        let on_album = rig.count(&format!(
            "SELECT COUNT(*) FROM library_catalog_actions \
             WHERE action_kind = 'new_copy_at_path' AND local_album_id = '{album}'"
        ));
        if line.cleared {
            assert!(
                !record(&conn, track).expect("record reads").exists(),
                "row {row}: the record is gone"
            );
            assert_eq!(
                rig.count(&format!(
                    "SELECT COUNT(*) FROM library_track_management_state \
                     WHERE local_track_id = '{track}' AND managed_path_revision IS NOT NULL"
                )),
                0,
                "row {row}: no v2 record left"
            );
            assert_eq!((actions, on_album), (1, 1), "row {row}: one action");
            assert_eq!(
                rig.count(&format!(
                    "SELECT COUNT(*) FROM library_management_automatic_queue \
                     WHERE local_album_id = '{album}' AND not_before < 1e15"
                )),
                1,
                "row {row}: the album is queued"
            );
            let told: Vec<(HistoryKind, usize)> = album_history(&conn, album, "listener", now)
                .expect("history reads")
                .expect("album live")
                .into_iter()
                .map(|entry| (entry.kind, entry.track_count))
                .collect();
            assert_eq!(
                told,
                vec![(HistoryKind::TracksNewCopy, 1)],
                "row {row}: one history entry"
            );
        } else {
            assert_eq!(state(&rig, track), kept[n], "row {row}: the record is kept");
            assert_eq!(on_album, 0, "row {row}: no action");
        }
        if line.lose_first {
            assert_eq!(
                rig.text(&format!(
                    "SELECT availability FROM local_tracks WHERE id = '{track}'"
                ))
                .as_deref(),
                Some("indexed"),
                "row {row}: indexed again"
            );
        }
    }
}
