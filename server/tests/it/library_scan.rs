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

/// Whether a warning that both rows were kept names these two albums.
fn warned_both_kept(album: &str, new_row: &str) -> bool {
    retag_warnings()
        .lock()
        .expect("lines")
        .iter()
        .any(|line| line.contains("both kept") && line.contains(album) && line.contains(new_row))
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
/// where it was, and the log says which two albums were left apart.
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
/// listener made moves, and the log says which two albums were left
/// apart.
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
/// that finds them gone they still hold their album: three bonus tracks
/// retagged in that same scan do not walk off with the album's row and
/// its match.
#[tokio::test]
async fn files_gone_in_this_very_scan_still_hold_their_album() {
    let (mut rig, files, album) = chosen_album("scan-regroup-unreachable", 15).await;
    for relative in &files[3..] {
        std::fs::remove_file(rig.root.join(relative)).expect("file removed");
    }
    let after = rig.after.clone();
    rig.tag(&files[..3], &after);
    rig.scan().await;

    let new_row = rig.album_of("a/t000.flac");
    assert_ne!(new_row, album, "not followed");
    assert_eq!(rig.rows_on(&album), 12);
    assert_eq!(rig.rows_on(&new_row), 3);
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
