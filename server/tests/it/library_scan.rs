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

use crate::common::{ScratchDir, migrated_db};
use droppedneedle::library::identify::models::RecallResult;
use droppedneedle::library::identify::providers::FakeProviders;
use droppedneedle::library::identify::service::{IdentifyDeps, IdentifyService};
use droppedneedle::library::identify::sqlite::SqliteIdentifyStore;
use droppedneedle::library::repairs::hygiene;
use droppedneedle::library::scan::{
    BlockingPool, EffectivePolicy, LibraryRoot, LibraryScanCoordinator, NullTagReader,
    RootRegistry, RootSeamError, ScanControl, ScanKind, ScanRequest, ScanRun, ScanScope, ScanState,
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

/// Tag reader that signals shutdown on one nominated read, and holds that
/// read until the test has seen the drive abandoned.
struct ShutdownOnRead {
    shutdown: watch::Sender<bool>,
    signal_at: u64,
    reads: AtomicU64,
    abandoned: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl TagReader for ShutdownOnRead {
    fn read_tags(&self, _path: &Path) -> Result<ScannedTags, TagReadError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) + 1 == self.signal_at {
            let _ = self.shutdown.send(true);
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
        Arc::new(SqliteScanStore::scratch().expect("scan store opens")),
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
    let (resume, abandoned) = std::sync::mpsc::channel();
    let reader = Arc::new(ShutdownOnRead {
        shutdown,
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

/// What happens between two scan windows: a listener, a curator or a
/// job acting while the scan is half way through an album.
type Meanwhile = Box<dyn FnOnce() + Send>;

/// Tag reader for the regroup briefs: every file reads as a track of the
/// album `albums` names for its path, and one nominated read first runs
/// `meanwhile`.
struct RetagReader {
    albums: std::sync::Mutex<HashMap<String, String>>,
    /// File name, and what happens just before it is read.
    meanwhile: std::sync::Mutex<Option<(String, Meanwhile)>>,
}

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
        let folder = path.parent().and_then(Path::file_name).expect("folder");
        let relative = format!("{}/{name}", folder.to_string_lossy());
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
        read.tag.title = name.into_owned();
        read.tag.artist = "Artist".to_owned();
        read.tag.album_artist = Some("Artist".to_owned());
        read.tag.album = self.albums.lock().expect("albums")[&relative].clone();
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
        let store = Arc::new(SqliteScanStore::open(&db).expect("scan store opens"));
        let reader = Arc::new(RetagReader {
            albums: std::sync::Mutex::default(),
            meanwhile: std::sync::Mutex::default(),
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
            store
                .execute_batch_for_tests(&sql)
                .expect("mid-scan statement runs");
        });
    }

    async fn scan(&self) {
        assert_eq!(self.scan_to_its_end().await.state, ScanState::Completed);
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
        let track = store.track_at("music", relative).expect("track indexed");
        store.album_for_track(&track).expect("album")
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
            .execute_batch_for_tests(sql)
            .expect("statement runs");
    }

    fn count(&self, sql: &str) -> i64 {
        self.coordinator
            .store()
            .query_i64_for_tests(sql)
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
            if line.contains("retagged album") {
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
        let mut conn = droppedneedle::db::open_connection(&db).expect("side connection opens");
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
        *cell.lock().expect("cell") = Some(store.query_i64_for_tests(&sql).expect("count"));
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
            .pop()
            .expect("the scan is running");
        coordinator
            .control(&run.id, ScanControl::Stop, false, run.row_revision)
            .expect("stop is taken");
    });
    let run = rig.scan_to_its_end().await;
    assert_eq!(run.state, ScanState::Cancelled);

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
    droppedneedle::db::open_connection(&rig.db())
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
    let store = Arc::new(SqliteIdentifyStore::open(&rig.db()).expect("identify store opens"));
    let service = IdentifyService::new(IdentifyDeps {
        identities: store.clone(),
        facts: store.clone(),
        proofs: store.clone(),
        aliases: store.clone(),
        queue: store.clone(),
        reviews: store.clone(),
        releases: store.clone(),
        providers: Arc::new(FakeProviders::with_recall(RecallResult::default())),
    });
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
        + 60_000;
    let job = store.claim(now_ms, 60_000).expect("a job is waiting");
    assert_eq!(job.local_album_id, album, "the waiting job is the album's");
    service
        .run_claimed_job(&job.id, now_ms)
        .await
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
/// "unsure" flag, an undo for the edition, a credit proof, a place in the
/// automatic management queue and a review waiting for a person.
fn automatic_leftovers(album: &str, tag: &str) -> String {
    format!(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, updated_at) \
         SELECT {album}, 'unconfirmed', '{tag}', 0 WHERE {album} IS NOT NULL; \
         INSERT INTO library_automatic_edition_undo (id, local_album_id, \
         expected_post_album_revision, expected_post_identity_revision, reason_code, created_at) \
         SELECT 'undo-{tag}', {album}, 1, 4, 'AUTOMATIC_EXACT_EDITION', 0 \
         WHERE {album} IS NOT NULL; \
         INSERT INTO library_identify_credit_proofs (local_album_id, local_track_id, \
         source_local_artist_id, artist_mbid, release_mbid, album_identity_revision, \
         track_identity_revision) SELECT {album}, id, 'artist-{tag}', 'mbid', 'rel', 4, 1 \
         FROM local_tracks WHERE relative_path = 'a/t000.flac' AND {album} IS NOT NULL; \
         INSERT INTO library_management_automatic_queue (local_album_id, queued_at, not_before) \
         SELECT {album}, 0, 0 WHERE {album} IS NOT NULL; \
         INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_ms, updated_ms) \
         SELECT 'pending-{tag}', {album}, 'R', '[]', 'pending', 0, 0 WHERE {album} IS NOT NULL;"
    )
}

/// Rows left on `album` in the tables that hang on a match, in the order:
/// flag, automatic undo, choice undo, credit proofs, remap queue,
/// management queue, reviews.
fn hanging_rows(rig: &RegroupRig, album: &str) -> [i64; 7] {
    [
        "library_album_match_state",
        "library_automatic_edition_undo",
        "library_edition_choice_undo",
        "library_identify_credit_proofs",
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
        [0, 0, 0, 0, 0, 1, 0],
        "only a fresh look by automatic management is left"
    );
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
        assert_eq!(hanging_rows(&rig, &album), [1, 0, 0, 0, 0, 1, 1], "{name}");
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
        assert_eq!(hanging_rows(&rig, &album), [0, 0, 0, 0, 1, 1, 0], "{name}");
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
        let mut conn = droppedneedle::db::open_connection(&db).expect("side connection opens");
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
        .pop()
        .expect("the run is waiting");
    rig.coordinator
        .control(&run.id, ScanControl::Stop, false, run.row_revision)
        .expect("stop is taken");
    let stamp = "SELECT CAST(terminal_at * 1000 AS INTEGER) + row_revision \
                 FROM library_scan_runs WHERE state = 'cancelled'";
    let ended = rig.count(stamp);

    let later = rig.count("SELECT CAST(terminal_at AS INTEGER) FROM library_scan_runs") + 3600;
    rig.coordinator.store().recover(later as f64);
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
        .expect("track indexed");
    rig.lose(&files[9..]).await;
    let (before, after) = (rig.before.clone(), rig.after.clone());
    rig.tag(&files[..9], &after);
    rig.scan().await;
    assert_eq!(rig.album_of("a/t000.flac"), album, "followed");

    rig.tag(&files[11..], &before);
    rig.scan().await;

    assert_eq!(
        rig.coordinator.store().track_at("music", "a/t011.flac"),
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
    let conn = droppedneedle::db::open_connection(&rig.db()).expect("side connection opens");
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

    let conn = droppedneedle::db::open_connection(&rig.db()).expect("side connection opens");
    droppedneedle::library::management::automatic::queue(&conn, &album, 2.0e9)
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
        droppedneedle::db::open_connection(&self.db())
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
                .pop()
                .expect("the scan is running");
            coordinator
                .control(&run.id, ScanControl::Stop, false, run.row_revision)
                .expect("stop is taken");
        });
    }

    /// What is left on an album of the match it held: the match, its flag,
    /// both kinds of edition undo, the remap, a custom edition, credit
    /// proofs, its place in the automatic management queue and reviews
    /// waiting for a person.
    fn match_leftovers(&self, album: &str) -> i64 {
        [
            "local_album_external_identities",
            "library_album_match_state",
            "library_edition_choice_undo",
            "library_automatic_edition_undo",
            "library_edition_remap_queue",
            "library_custom_edition_active",
            "library_identify_credit_proofs",
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
        assert_eq!(rig.scan_to_its_end().await.state, ScanState::Cancelled);
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
    use droppedneedle::library::corrections::Corrections;
    use droppedneedle::library::corrections::models::{
        ApplyMeta, MembershipKind, MembershipRequest,
    };

    let scratch = ScratchDir::new("scan-pair-hand-merge");
    let music = scratch.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    plant(&music, "b/01.flac", "flac_full_02.flac");
    let library = bundle(&music);
    scan(&library).await;
    let store = &library.scan_store;
    let track = store.track_at("music", "a/01.flac").expect("a/01");
    let source = store.album_for_track(&track).expect("album a");
    let target = store
        .album_for_track(&store.track_at("music", "b/01.flac").expect("b/01"))
        .expect("album b");
    store
        .execute_batch_for_tests(&format!(
            "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
             recorded_at, updated_at) VALUES ('{source}', '{target}', 'tracks_elsewhere', \
             'open', 0, 0);"
        ))
        .expect("pair recorded");

    let corrections = Corrections::new(&library);
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
