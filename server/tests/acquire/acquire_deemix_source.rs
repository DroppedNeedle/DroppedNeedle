//! The Deezer source over the Deemix mock, which answers from the
//! recordings in `tests/fixtures/deemix/`, Deezer's barcode lookup over
//! its recorded answers, and a real download journal. Each test drives
//! the source the way the worker does: enqueue, poll, landed, discard,
//! abort.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{Uri, header};
use axum::response::IntoResponse;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::deemix::client::DeemixClient;
use droppedneedle_acquire::deemix::session::DeemixSession;
use droppedneedle_acquire::deemix::source::{DeemixParts, DeemixSource};
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::state::AttemptState;
use droppedneedle_acquire::downloads::store::{NewTask, TrackAlbumColumns};
use droppedneedle_acquire::downloads::{DownloadSource, SourceError, SourceHandle, SourceTag};
use droppedneedle_acquire::downloads::{ExpectedTrack, TaskPlan, TrackAlbum, TrackPosition};
use droppedneedle_acquire::edition::EditionBasis;
use droppedneedle_acquire::reason::Reason;
use droppedneedle_acquire::search_jobs::candidates::Pin;
use droppedneedle_acquire::search_jobs::store::{JobPayload, JobStatus, JobStore, NewJob};
use droppedneedle_acquire::target::{SearchTarget, TrackWant};
use droppedneedle_kernel::http_client::HttpClientFactory;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::sections::{DownloadPolicy, recipe_for_range};
use droppedneedle_kernel::test_support::ManualClock;
use droppedneedle_providers::Providers;
use droppedneedle_testkit::deemix::DeemixMock;
use droppedneedle_testkit::usenet::{ServerGuard, serve_loopback};
use serde_json::{Value, json};

use crate::common::ScratchDir;
use crate::common::acquire::no_targets;
use crate::common::bundles::quiet_ticks;

const TIMEOUT: Duration = Duration::from_secs(10);
const IN_RAINBOWS: u64 = 14880659;
const IN_RAINBOWS_BARCODE: &str = "634904032463";
/// Past the sign-in limiter's longest wait.
const SIGN_IN_WAIT_MS: i64 = 21 * 60 * 1000;

fn fixture(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path);
    std::fs::read_to_string(path).expect("fixture reads")
}

fn recorded(path: &str) -> Value {
    serde_json::from_str(&fixture(path)).expect("fixture is JSON")
}

/// In Rainbows as MusicBrainz lists it: the recorded Deezer tracklist's
/// positions, titles, lengths and ISRCs.
fn in_rainbows() -> Vec<ExpectedTrack> {
    recorded("deemix/tracklist_album.json")["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .map(|track| ExpectedTrack {
            track_number: track["track_position"].as_i64().expect("position"),
            disc_number: track["disk_number"].as_i64().expect("disc"),
            duration_seconds: track["duration"].as_f64(),
            recording_mbid: None,
            title: track["title_short"].as_str().map(str::to_owned),
            release_track_mbid: None,
            isrcs: vec![track["isrc"].as_str().expect("isrc").to_owned()],
        })
        .collect()
}

fn plan(tracks: Vec<ExpectedTrack>, barcode: Option<&str>) -> TaskPlan {
    TaskPlan {
        release_mbid: Some("rel-in-rainbows".to_owned()),
        expected_tracks: tracks,
        barcode: barcode.map(str::to_owned),
        ..TaskPlan::default()
    }
}

/// Deezer's public API: the recorded barcode answers.
fn deezer_api() -> Router {
    Router::new().fallback(|uri: Uri| async move {
        let body = if uri.path() == format!("/deezer/album/upc:{IN_RAINBOWS_BARCODE}") {
            fixture("deezer/album_upc_12.json")
        } else {
            fixture("deezer/album_upc_unknown.json")
        };
        ([(header::CONTENT_TYPE, "application/json")], body).into_response()
    })
}

/// Deemix's "Overwrite files" on every `/connect` answer.
fn overwrite(mock: &DeemixMock, value: &str) {
    let mut state = mock.state();
    state.connect["settingsData"]["settings"]["overwriteFile"] = json!(value);
    state.connect_signed_in["settingsData"]["settings"]["overwriteFile"] = json!(value);
}

struct Rig {
    mock: DeemixMock,
    journal: Arc<Journal>,
    settings: ConfigStore,
    clock: ManualClock,
    mount: ScratchDir,
    overlaps: Arc<Mutex<Vec<PathBuf>>>,
    source: DeemixSource,
    _servers: (ServerGuard, ServerGuard),
    _settings_dir: ScratchDir,
}

/// The source over a Deemix that offers its saved sign-in and overwrites
/// files, reached at `path` below the mock's root.
async fn rig_at(path: &str) -> Rig {
    let mock = DeemixMock::new();
    mock.offer_saved_login();
    overwrite(&mock, "y");
    let (base, deemix) = serve_loopback(mock.router()).await.expect("Deemix mock");
    let (deezer_base, deezer) = serve_loopback(deezer_api()).await.expect("Deezer mock");
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let platform = crate::common::TestPlatform::on(db.db(), &dir).build().await;
    let providers = Providers::build(
        &platform,
        crate::common::outbound_at(&deezer_base).providers(),
        droppedneedle_kernel::db::cache::CacheDb::scratch(),
    );
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    let settings_dir = ScratchDir::new("deemix-settings");
    let settings =
        droppedneedle_acquire::test_support::scratch_settings(&settings_dir).expect("settings");
    let clock = ManualClock::at(1_800_000_000_000);
    let mount = ScratchDir::new("deemix-mount");
    let overlaps: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let neighbours = {
        let overlaps = Arc::clone(&overlaps);
        Arc::new(move || {
            Some(vec![(
                "library folder",
                overlaps.lock().expect("folders").clone(),
            )])
        })
    };
    let factory = HttpClientFactory::new().expect("http client factory");
    let client = DeemixClient::new(
        factory.no_redirect().clone(),
        &format!("{base}{path}"),
        Arc::new(DeemixSession::new()),
        TIMEOUT,
    )
    .expect("a usable base URL");
    let source = DeemixSource::new(DeemixParts {
        client,
        previews: providers.previews.clone(),
        journal: journal.clone(),
        targets: no_targets(&journal).await,
        policy: settings.live(),
        downloads_mount: mount.to_path_buf(),
        neighbours,
        clock: Arc::new(clock.clone()),
        memory: Arc::default(),
    });
    Rig {
        mock,
        journal,
        settings,
        clock,
        mount,
        overlaps,
        source,
        _servers: (deemix, deezer),
        _settings_dir: settings_dir,
    }
}

async fn rig() -> Rig {
    rig_at("/").await
}

impl Rig {
    /// Quality settings that take MP3 128, the free account's only tier.
    fn allow_low(&self) {
        let mut policy = (**self
            .settings
            .live::<DownloadPolicy>()
            .read()
            .expect("policy"))
        .clone();
        policy.quality_min = "low".to_owned();
        policy.quality_recipe = recipe_for_range("low", "lossless", &[]);
        self.settings.save_blocking(policy).expect("policy saves");
    }

    async fn album_task(&self, id: &str, plan: TaskPlan) {
        self.album_task_from(id, plan, "user").await;
    }

    async fn album_task_from(&self, id: &str, plan: TaskPlan, origin: &str) {
        let task = NewTask {
            id: id.to_owned(),
            user_id: "u1".to_owned(),
            artist_name: "Radiohead".to_owned(),
            album_title: "In Rainbows".to_owned(),
            release_group_mbid: "rg-in-rainbows".to_owned(),
            origin: origin.to_owned(),
            retry_count: 0,
        };
        self.journal
            .run("test.task", move |store| {
                store.insert_task(&task, 1.0)?;
                store.set_task_plan(&task.id, &plan, 1.0)
            })
            .await
            .expect("task seeds");
    }

    /// A song task for "Nude", the third track of In Rainbows.
    async fn song_task(&self, id: &str) {
        let task = NewTask {
            id: id.to_owned(),
            user_id: "u1".to_owned(),
            artist_name: "Radiohead".to_owned(),
            album_title: "Nude".to_owned(),
            release_group_mbid: String::new(),
            origin: "user".to_owned(),
            retry_count: 0,
        };
        let columns = TrackAlbumColumns {
            release_group_mbid: "rg-in-rainbows".to_owned(),
            release_mbid: "rel-in-rainbows".to_owned(),
            album_title: "In Rainbows".to_owned(),
            track_number: 3,
            disc_number: 1,
            track_count: 10,
            ..TrackAlbumColumns::default()
        };
        let plan = TaskPlan {
            track_album: Some(TrackAlbum {
                album_artist: "Radiohead".to_owned(),
                basis: EditionBasis::BestOfficial,
                wanted: vec![TrackPosition { disc: 1, track: 3 }],
            }),
            ..plan(in_rainbows(), None)
        };
        self.journal
            .run("test.song", move |store| {
                store.insert_track_task(&task, "rec-nude", 1.0)?;
                store.set_track_album(&task.id, &columns, 1.0)?;
                store.set_task_plan(&task.id, &plan, 1.0)
            })
            .await
            .expect("song task seeds");
    }

    /// Journal `handle` as the task's live attempt, as the worker does.
    async fn journal_attempt(&self, attempt_id: &str, task_id: &str, handle: &SourceHandle) {
        self.journal_attempt_in(attempt_id, task_id, handle, AttemptState::Acquiring)
            .await;
    }

    /// Journal `handle` as one of the task's attempts, in `state`.
    async fn journal_attempt_in(
        &self,
        attempt_id: &str,
        task_id: &str,
        handle: &SourceHandle,
        state: AttemptState,
    ) {
        let (attempt_id, task_id) = (attempt_id.to_owned(), task_id.to_owned());
        let job = handle.job_name.clone();
        let json = serde_json::to_string(handle).expect("handle encodes");
        self.journal
            .run("test.attempt", move |store| {
                store.insert_attempt(&attempt_id, &task_id, "deezer", 0, &job, &json, state, 1.0)
            })
            .await
            .expect("attempt journals");
    }

    async fn enqueue(&self, task_id: &str) -> Result<SourceHandle, SourceError> {
        self.source.enqueue(task_id, 0).await
    }

    /// The `getQueue` answer from now on; the shared answer is let go.
    fn queue(&self, answer: Value) {
        let mut state = self.mock.state();
        state.queue_walk.clear();
        state.queue = answer;
        drop(state);
        self.clock.advance(2_000);
    }

    fn calls(&self, path: &str) -> usize {
        self.mock.calls_to(path).len()
    }

    fn adds(&self) -> Vec<Value> {
        self.mock
            .calls_to("/api/addToQueue")
            .iter()
            .map(|call| serde_json::from_str(&call.body).expect("an add's body is JSON"))
            .collect()
    }
}

/// A queue item of the shape Deemix answers.
fn item(uuid: &str, status: &str, files: &[&str], errors: &[&str]) -> Value {
    let mut parts = uuid.split('_');
    let (kind, id, bitrate) = (
        parts.next().expect("kind"),
        parts.next().expect("id"),
        parts
            .next()
            .expect("bitrate")
            .parse::<u64>()
            .expect("bitrate"),
    );
    let files: Vec<Value> = files
        .iter()
        .map(|path| {
            json!({
                "path": path,
                "filename": path.rsplit('/').next(),
                "data": {"id": "1", "title": "t", "artist": "a"},
            })
        })
        .collect();
    let errors: Vec<Value> = errors
        .iter()
        .map(|errid| match *errid {
            "" => json!({"message": "x", "type": "track", "data": {}}),
            errid => json!({"message": "x", "errid": errid, "type": "track", "data": {}}),
        })
        .collect();
    json!({
        "uuid": uuid, "type": kind, "id": id, "bitrate": bitrate, "status": status,
        "size": 10, "downloaded": files.len(), "failed": errors.len(),
        "extrasPath": "/downloads//Radiohead - In Rainbows",
        "files": files, "errors": errors,
    })
}

fn queue_of(items: &[Value], current: Option<Value>) -> Value {
    let map: serde_json::Map<String, Value> = items
        .iter()
        .map(|item| {
            (
                item["uuid"].as_str().expect("uuid").to_owned(),
                item.clone(),
            )
        })
        .collect();
    let mut queue = json!({"queue": map, "queueOrder": []});
    if let Some(current) = current {
        queue["current"] = current;
    }
    queue
}

/// `current` naming `uuid`, with `files` finished.
fn current(uuid: &str, files: &[&str]) -> Value {
    let files: Vec<Value> = files
        .iter()
        .map(|path| json!({"path": path, "filename": "f", "data": {"id": "1"}}))
        .collect();
    json!({"uuid": uuid, "title": "In Rainbows", "downloaded": files.len(), "failed": 0,
        "progress": 10.0, "files": files})
}

fn unavailable(result: Result<SourceHandle, SourceError>) -> String {
    match result {
        Err(SourceError::Unavailable(text)) => text,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

fn rejected<T: std::fmt::Debug>(result: Result<T, SourceError>) -> Reason {
    match result {
        Err(SourceError::Rejected { reason, .. }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The enqueue gate signs the session in before it reads the account's
/// flags: the free account then gives only MP3 128, which the default
/// settings refuse, so nothing is added. Flags Deemix leaves out never
/// refuse: the add goes ahead at the best bitrate.
#[tokio::test]
async fn the_enqueue_gate_signs_in_before_it_reads_the_account() {
    let rig = rig().await;
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let text = unavailable(rig.enqueue("t1").await);
    assert!(text.contains("MP3 128"), "{text}");
    assert_eq!(rig.calls("/api/loginArl"), 1);
    assert_eq!(rig.calls("/api/addToQueue"), 0);

    {
        let mut state = rig.mock.state();
        state.connect_signed_in["currentUser"] = json!({});
        state.account_bitrates = vec![9];
    }
    rig.clock.advance(2_000);
    let handle = rig.enqueue("t1").await.expect("an add");
    assert_eq!(rig.adds()[0]["bitrate"], json!(9));
    assert!(
        handle.job_name.starts_with("album_14880659_9:t1:"),
        "{}",
        handle.job_name
    );
    assert_eq!(rig.calls("/api/loginArl"), 1);
}

/// A failed sign-in is never tried again until Deemix offers its sign-in
/// once more, and sign-ins keep their gap: a Deemix restart inside it
/// waits, and the next sign-in comes once the gap is up.
#[tokio::test]
async fn sign_ins_keep_to_their_limits() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    rig.album_task("t2", plan(in_rainbows(), None)).await;
    rig.enqueue("t1").await.expect("signed in and added");
    assert_eq!(rig.calls("/api/loginArl"), 1);

    rig.mock.restart();
    rig.clock.advance(2_000);
    let text = unavailable(rig.enqueue("t2").await);
    assert!(text.contains("Waiting a few minutes"), "{text}");
    assert_eq!(rig.calls("/api/loginArl"), 1);

    rig.clock.advance(SIGN_IN_WAIT_MS);
    rig.mock.state().login_fails = true;
    let text = unavailable(rig.enqueue("t2").await);
    assert!(text.contains("lost its Deezer sign-in"), "{text}");
    assert_eq!(rig.calls("/api/loginArl"), 2);

    rig.clock.advance(SIGN_IN_WAIT_MS);
    let text = unavailable(rig.enqueue("t2").await);
    assert!(text.contains("lost its Deezer sign-in"), "{text}");
    assert_eq!(rig.calls("/api/loginArl"), 2);
}

/// What only Deemix or this server can fix waits: an address that is not
/// Deemix, overwrite off, settings that allow nothing the account gives,
/// an account refused the bitrate, and a downloads folder overlapping a
/// library folder. None of them refuses the request or blames the album.
#[tokio::test]
async fn what_deemix_or_the_settings_must_fix_waits() {
    let html = rig_at("/deemix/").await;
    html.album_task("t1", plan(in_rainbows(), None)).await;
    let text = unavailable(html.enqueue("t1").await);
    assert!(text.contains("isn't Deemix"), "{text}");

    let rig = rig().await;
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    overwrite(&rig.mock, "n");
    let text = unavailable(rig.enqueue("t1").await);
    assert!(text.contains("Overwrite files"), "{text}");
    assert_eq!(rig.calls("/api/loginArl"), 0);

    overwrite(&rig.mock, "y");
    rig.clock.advance(2_000);
    let text = unavailable(rig.enqueue("t1").await);
    assert!(text.contains("quality settings don't allow"), "{text}");

    rig.allow_low();
    rig.mock.state().account_bitrates = Vec::new();
    rig.clock.advance(2_000);
    let text = unavailable(rig.enqueue("t1").await);
    assert!(text.contains("can't download at that quality"), "{text}");

    rig.overlaps
        .lock()
        .expect("folders")
        .push(rig.mount.to_path_buf());
    rig.clock.advance(SIGN_IN_WAIT_MS);
    let text = unavailable(rig.enqueue("t1").await);
    assert!(text.contains("overlaps a library folder"), "{text}");
    rig.overlaps.lock().expect("folders").clear();

    use std::os::unix::fs::PermissionsExt as _;
    let mode = |bits| std::fs::Permissions::from_mode(bits);
    std::fs::set_permissions(&*rig.mount, mode(0o555)).expect("mount read-only");
    let text = unavailable(rig.enqueue("t1").await);
    std::fs::set_permissions(&*rig.mount, mode(0o755)).expect("mount writable");
    assert!(text.contains("missing or not writable"), "{text}");
    std::fs::remove_dir(&*rig.mount).expect("mount removed");
    let text = unavailable(rig.enqueue("t1").await);
    assert!(text.contains("missing or not writable"), "{text}");
    assert_eq!(rig.calls("/api/addToQueue"), 1);
}

/// The barcode finds the album through Deezer's API, and an album whose
/// every track pairs by ISRC is taken without a name search. Without a
/// barcode the name search finds it. Either way the add sends the album's
/// link at the account's bitrate.
#[tokio::test]
async fn an_album_is_found_by_barcode_else_by_name() {
    let barcode = rig().await;
    barcode.allow_low();
    barcode
        .album_task("t1", plan(in_rainbows(), Some(IN_RAINBOWS_BARCODE)))
        .await;
    let handle = barcode.enqueue("t1").await.expect("barcode pick");
    assert_eq!(barcode.calls("/api/search"), 0);
    assert_eq!(barcode.calls("/api/getTracklist"), 1);
    assert_eq!(
        barcode.adds()[0],
        json!({"url": "https://www.deezer.com/album/14880659", "bitrate": 1})
    );
    assert_eq!(handle.job_name, "album_14880659_1:t1:0");
    assert_eq!(handle.release_identity, "deezer:album:14880659");
    assert!(!handle.adopted);

    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = rig.enqueue("t1").await.expect("name pick");
    let searched = rig.mock.calls_to("/api/search");
    assert_eq!(searched.len(), 1);
    assert_eq!(searched[0].query["term"], "Radiohead In Rainbows");
    assert!(rig.calls("/api/getTracklist") <= 5);
    assert_eq!(handle.release_identity, "deezer:album:14880659");
}

/// A pinned edition without the bonus track Deezer's album has is not
/// that album: nothing is added and the task looks elsewhere.
#[tokio::test]
async fn a_bonus_track_album_under_a_pin_sends_no_add() {
    let rig = rig().await;
    rig.allow_low();
    let mut tracks = in_rainbows();
    tracks.pop();
    rig.album_task("t1", plan(tracks, None)).await;
    assert_eq!(rejected(rig.enqueue("t1").await), Reason::NoSource);
    assert_eq!(rig.calls("/api/addToQueue"), 0);
}

/// A song is queued as its one track, by the track's link, taken from the
/// album that fits.
#[tokio::test]
async fn a_song_queues_one_track() {
    let rig = rig().await;
    rig.allow_low();
    rig.song_task("s1").await;
    let handle = rig.enqueue("s1").await.expect("track pick");
    assert_eq!(
        rig.adds(),
        [json!({"url": "https://www.deezer.com/track/138546807", "bitrate": 1})]
    );
    assert_eq!(handle.job_name, "track_138546807_1:s1:0");
    assert_eq!(handle.release_identity, "deezer:track:138546807");
}

/// While another task's attempt holds the album, at any bitrate, nothing
/// adds, removes or lands it.
#[tokio::test]
async fn another_tasks_attempt_holds_the_item() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    rig.album_task("other", plan(in_rainbows(), None)).await;
    let held = SourceHandle {
        job_name: "album_14880659_3:other:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    rig.journal_attempt("a-other", "other", &held).await;

    let text = unavailable(rig.enqueue("t1").await);
    assert!(text.contains("Another download"), "{text}");
    assert_eq!(rig.calls("/api/addToQueue"), 0);

    let mine = SourceHandle {
        job_name: "album_14880659_1:t1:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    assert!(!rig.source.discard(&mine).await.expect("discard answers"));
    assert!(!rig.source.abort(&mine).await.expect("abort answers"));
    assert_eq!(rig.calls("/api/removeFromQueue"), 0);
    assert_eq!(
        rejected(rig.source.landed(&mine).await),
        Reason::AlreadyDownloading
    );
}

/// An unfinished item already in Deemix that no attempt names is attached
/// with no add, as adopted, and giving up leaves it in Deemix. A finished
/// one is removed and added fresh.
#[tokio::test]
async fn items_already_in_deemix_are_taken_over() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    rig.queue(queue_of(
        &[item("album_14880659_1", "inQueue", &[], &[])],
        None,
    ));
    let handle = rig.enqueue("t1").await.expect("attached");
    assert!(handle.adopted);
    assert_eq!(handle.job_name, "album_14880659_1:t1:0");
    assert_eq!(rig.calls("/api/addToQueue"), 0);
    assert!(!rig.source.discard(&handle).await.expect("discard answers"));
    assert!(!rig.source.abort(&handle).await.expect("abort answers"));
    assert_eq!(rig.calls("/api/removeFromQueue"), 0);

    rig.album_task("t2", plan(in_rainbows(), None)).await;
    rig.queue(queue_of(
        &[item(
            "album_14880659_1",
            "completed",
            &["/downloads//a/01.mp3"],
            &[],
        )],
        None,
    ));
    let handle = rig.enqueue("t2").await.expect("added fresh");
    assert!(!handle.adopted);
    let removed = rig.mock.calls_to("/api/removeFromQueue");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].query["uuid"], "album_14880659_1");
    assert_eq!(rig.calls("/api/addToQueue"), 1);

    // An earlier attempt of this task names the item: it is ours.
    rig.album_task("t3", plan(in_rainbows(), None)).await;
    let earlier = SourceHandle {
        job_name: "album_14880659_1:t3:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    rig.journal_attempt_in("a-t3", "t3", &earlier, AttemptState::Complete)
        .await;
    rig.queue(queue_of(
        &[item("album_14880659_1", "downloading", &[], &[])],
        None,
    ));
    let handle = rig.enqueue("t3").await.expect("attached");
    assert!(!handle.adopted);
    assert_eq!(rig.calls("/api/addToQueue"), 1);
}

/// The attempt keeps the uuid Deemix answered with, not one worked out
/// here. An add that queues nothing waits twice and gives the album up on
/// the third; the next pick finds nothing else to take.
#[tokio::test]
async fn adds_keep_deemixs_uuid_and_give_up_after_three_empty_answers() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let mut answer = recorded("deemix/addToQueue_album.json");
    answer["data"]["obj"][0]["uuid"] = json!("album_14880659_3");
    rig.mock.script(
        "/api/addToQueue",
        200,
        "application/json; charset=utf-8",
        &answer.to_string(),
    );
    let handle = rig.enqueue("t1").await.expect("an add");
    assert_eq!(handle.job_name, "album_14880659_3:t1:0");

    rig.album_task("t2", plan(in_rainbows(), None)).await;
    rig.mock.state().scripted.clear();
    rig.mock.state().add_nothing = true;
    for _ in 0..2 {
        rig.clock.advance(2_000);
        let text = unavailable(rig.enqueue("t2").await);
        assert!(text.contains("queued nothing"), "{text}");
    }
    rig.clock.advance(2_000);
    assert_eq!(rejected(rig.enqueue("t2").await), Reason::ClientDropped);
    rig.clock.advance(2_000);
    assert_eq!(rejected(rig.enqueue("t2").await), Reason::NoSource);
    assert_eq!(rig.calls("/api/addToQueue"), 4);
}

/// A finished album's files map from Deemix's folder (its paths carry a
/// double slash) under the downloads folder and are kept on the attempt,
/// so a Retry import after Deemix let the item go lands the same files.
#[tokio::test]
async fn landed_files_are_mapped_and_kept_for_a_retry_import() {
    let rig = rig().await;
    rig.queue(recorded("deemix/getQueue_two_albums_7.json"));
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = SourceHandle {
        job_name: "album_14880659_1:t1:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    rig.journal_attempt("a1", "t1", &handle).await;
    let landed = rig.source.landed(&handle).await.expect("files listed");
    assert_eq!(landed.files.len(), 10);
    let faust = rig.mount.join("Radiohead - In Rainbows/06 - Faust Arp.mp3");
    assert!(
        landed.files.iter().any(|file| file.path == faust),
        "{landed:?}"
    );

    rig.queue(recorded("deemix/getQueue_empty.json"));
    let again = rig.source.landed(&handle).await.expect("stored files");
    assert_eq!(again.files, landed.files);
}

/// An album Deezer has no file for at the bitrate asked is added one step
/// lower inside the same attempt: the attempt now names the new item and
/// no fail-over is spent.
#[tokio::test]
async fn the_bitrate_step_rewrites_the_handle() {
    let rig = rig().await;
    rig.allow_low();
    {
        let mut state = rig.mock.state();
        state.connect_signed_in["currentUser"]["can_stream_hq"] = json!(true);
        state.account_bitrates = vec![1, 3];
    }
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = rig.enqueue("t1").await.expect("an add at 320");
    assert_eq!(handle.job_name, "album_14880659_3:t1:0");
    rig.journal_attempt("a1", "t1", &handle).await;

    rig.queue(queue_of(
        &[item(
            "album_14880659_3",
            "failed",
            &[],
            &["wrongBitrate", "wrongBitrateNoAlternative"],
        )],
        None,
    ));
    let progress = rig.source.poll(&handle).await.expect("stepped down");
    assert!(progress.queued_at_client);
    assert_eq!(rig.adds()[1]["bitrate"], json!(1));
    let attempts = rig.journal.read_attempts("t1").await.expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].job_name, "album_14880659_1:t1:0");
    let stored: SourceHandle = serde_json::from_str(
        &rig.journal
            .read_attempt_handle("a1")
            .await
            .expect("handle reads")
            .expect("a handle"),
    )
    .expect("handle decodes");
    assert_eq!(stored.job_name, "album_14880659_1:t1:0");
    let removed = rig.mock.calls_to("/api/removeFromQueue");
    assert_eq!(
        removed.last().expect("removed").query["uuid"],
        "album_14880659_3"
    );
}

/// An item whose failed tracks were all refused for the account, ending
/// `failed` or `withErrors`, gets a fresh session, a sign-in and one more
/// add; refused again it gives up with a reason that blames nothing.
#[tokio::test]
async fn account_errors_add_again_once_with_no_blame() {
    for (status, errid) in [
        ("failed", "notLoggedIn"),
        ("withErrors", "wrongLicenseNoAlternative"),
    ] {
        let rig = rig().await;
        rig.allow_low();
        rig.album_task("t1", plan(in_rainbows(), None)).await;
        let handle = rig.enqueue("t1").await.expect("an add");
        rig.journal_attempt("a1", "t1", &handle).await;
        let ended = queue_of(
            &[item(
                "album_14880659_1",
                status,
                &["/downloads//a/01.mp3"],
                &[errid],
            )],
            None,
        );

        rig.clock.advance(SIGN_IN_WAIT_MS);
        rig.queue(ended.clone());
        let progress = rig.source.poll(&handle).await.expect("added again");
        assert!(progress.queued_at_client, "{status}");
        assert_eq!(rig.calls("/api/loginArl"), 2, "{status}");
        assert_eq!(rig.calls("/api/removeFromQueue"), 1, "{status}");
        assert_eq!(rig.calls("/api/addToQueue"), 2, "{status}");

        rig.queue(ended);
        let reason = rejected(rig.source.poll(&handle).await);
        assert_eq!(reason, Reason::AccountRefused, "{status}");
        assert!(!reason.against_source());
    }
}

/// An item Deemix no longer holds was dropped there, which blames nothing.
#[tokio::test]
async fn a_vanished_item_blames_nothing() {
    let rig = rig().await;
    let handle = SourceHandle {
        job_name: "album_14880659_1:t1:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    let reason = rejected(rig.source.poll(&handle).await);
    assert_eq!(reason, Reason::ClientDropped);
    assert!(!reason.against_source());
}

/// Ours waits while nothing downloads (a Deemix restart leaves its queue
/// so): after two polls a minute apart the session is signed in, and only
/// a later poll that still sees it waiting removes and adds it again.
/// While no sign-in can be had, nothing is removed.
#[tokio::test]
async fn a_stranded_item_is_added_again_only_after_a_sign_in() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = rig.enqueue("t1").await.expect("an add");
    rig.journal_attempt("a1", "t1", &handle).await;
    rig.mock.restart();
    rig.mock.state().connect = recorded("deemix/connect_signed_out.json");
    overwrite(&rig.mock, "y");
    rig.clock.advance(SIGN_IN_WAIT_MS);
    let stranded = queue_of(&[item("album_14880659_1", "inQueue", &[], &[])], None);

    rig.queue(stranded.clone());
    assert!(
        rig.source
            .poll(&handle)
            .await
            .expect("waiting")
            .queued_at_client
    );
    rig.clock.advance(61_000);
    let text = match rig.source.poll(&handle).await {
        Err(SourceError::Unavailable(text)) => text,
        other => panic!("expected Unavailable, got {other:?}"),
    };
    assert!(text.contains("Not signed in"), "{text}");
    assert_eq!(rig.calls("/api/removeFromQueue"), 0);

    rig.mock.offer_saved_login();
    overwrite(&rig.mock, "y");
    rig.queue(stranded.clone());
    assert!(
        rig.source
            .poll(&handle)
            .await
            .expect("signed in")
            .queued_at_client
    );
    assert_eq!(rig.calls("/api/loginArl"), 2);
    assert_eq!(rig.calls("/api/removeFromQueue"), 0);

    rig.queue(stranded);
    assert!(
        rig.source
            .poll(&handle)
            .await
            .expect("added again")
            .queued_at_client
    );
    assert_eq!(rig.calls("/api/removeFromQueue"), 1);
    assert_eq!(rig.calls("/api/addToQueue"), 2);
}

/// Ours waiting behind another item reads as queued at the client, never
/// active; ours current reads as active, never queued, with the bytes of
/// its finished tracks on disk. A `current` naming an item that has left
/// the queue is nothing current.
#[tokio::test]
async fn polls_read_waiting_current_and_a_stale_current() {
    let rig = rig().await;
    let handle = SourceHandle {
        job_name: "album_14880659_1:t1:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    let ours = item("album_14880659_1", "inQueue", &[], &[]);
    let other = item("album_6158273_1", "downloading", &[], &[]);

    rig.queue(queue_of(
        &[ours.clone(), other],
        Some(current("album_6158273_1", &[])),
    ));
    let progress = rig.source.poll(&handle).await.expect("waiting");
    assert!(progress.queued_at_client && !progress.has_active_transfer);

    let album = rig.mount.join("Radiohead - In Rainbows");
    std::fs::create_dir_all(&album).expect("album folder");
    std::fs::write(album.join("01 - 15 Step.mp3"), [0u8; 1234]).expect("a track");
    let mut downloading = ours.clone();
    downloading["status"] = json!("downloading");
    rig.queue(queue_of(
        &[downloading],
        Some(current(
            "album_14880659_1",
            &["/downloads//Radiohead - In Rainbows/01 - 15 Step.mp3"],
        )),
    ));
    let progress = rig.source.poll(&handle).await.expect("current");
    assert!(progress.has_active_transfer && !progress.queued_at_client);
    assert_eq!(progress.downloaded_bytes, 1234);

    rig.queue(queue_of(&[ours], Some(current("album_6158273_1", &[]))));
    let progress = rig.source.poll(&handle).await.expect("stale current");
    assert!(progress.queued_at_client && !progress.has_active_transfer);
    rig.clock.advance(61_000);
    let progress = rig.source.poll(&handle).await.expect("signed in");
    assert!(progress.queued_at_client);
    assert_eq!(
        rig.calls("/api/loginArl"),
        1,
        "nothing current: the stranded rule signs in"
    );
}

/// A person's pick from a search is taken though the edition does not fit
/// it; a pick the search review made must still fit, and when it does not
/// the task looks further.
#[tokio::test]
async fn a_persons_pick_skips_the_fit_and_a_review_pick_must_fit() {
    let rig = rig().await;
    rig.allow_low();
    let mut tracks = in_rainbows();
    tracks.pop();
    let target = SearchTarget {
        artist: "Radiohead".to_owned(),
        album_title: "In Rainbows".to_owned(),
        year: None,
        tracklist: tracks.clone(),
        barcode: None,
        wanted: Vec::new(),
        track: None,
        release_preferred: false,
        tracklist_unread: false,
    };
    let candidates = rig.source.search(&target).await.expect("search lists");
    assert_eq!(
        candidates[0].pin,
        Pin::Release {
            identity: format!("deezer:album:{IN_RAINBOWS}")
        }
    );
    let jobs = JobStore::new(rig.journal.db().clone());
    jobs.insert(NewJob {
        id: "job-1".to_owned(),
        user_id: "u1".to_owned(),
        artist_name: "Radiohead".to_owned(),
        album_title: "In Rainbows".to_owned(),
        year: None,
        release_group_mbid: Some("rg-in-rainbows".to_owned()),
        release_mbid: Some("rel-in-rainbows".to_owned()),
    })
    .await
    .expect("job inserts");
    let payload = JobPayload {
        candidates,
        ..JobPayload::default()
    };
    jobs.finish("job-1", JobStatus::Completed, payload, None)
        .await
        .expect("job finishes");
    for (task, person) in [("person", true), ("review", false)] {
        rig.album_task(task, plan(tracks.clone(), None)).await;
        let task_id = task.to_owned();
        rig.journal
            .run("test.link", move |store| {
                store.link_candidate(&task_id, "deezer", None, "job-1", 0, person, 1.0)
            })
            .await
            .expect("pick links");
    }

    let handle = rig.enqueue("person").await.expect("the person's pick");
    assert!(handle.pinned);
    assert_eq!(
        handle.release_identity,
        format!("deezer:album:{IN_RAINBOWS}")
    );
    assert_eq!(rejected(rig.enqueue("review").await), Reason::NoSource);
    assert_eq!(rig.calls("/api/addToQueue"), 1);
}

/// An upgrade takes only a tier at or above the quality cutoff: the free
/// account's MP3 128 under the default lossless cutoff is no better copy.
#[tokio::test]
async fn an_upgrade_below_the_cutoff_has_no_better_copy() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task_from("up", plan(in_rainbows(), None), "upgrade")
        .await;
    assert_eq!(rejected(rig.enqueue("up").await), Reason::NoBetterCopy);
    assert_eq!(rig.calls("/api/addToQueue"), 0);
}

/// An item that finished with errors Deezer gave for its tracks lands what
/// arrived. Failures in Deemix itself, which name no cause, add the item
/// again once; failing that way again, the item fails over as a local
/// fault, blaming nothing, even one that finished with some tracks: a
/// partial landing would count the missing tracks against the album. A
/// mix of causes is neither: the item has failed.
#[tokio::test]
async fn deemixs_own_failures_add_the_item_again_once() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = rig.enqueue("t1").await.expect("an add");
    rig.journal_attempt("a1", "t1", &handle).await;

    rig.queue(queue_of(
        &[item(
            "album_14880659_1",
            "withErrors",
            &["/downloads//a/01.mp3"],
            &["notOnDeezer"],
        )],
        None,
    ));
    let progress = rig.source.poll(&handle).await.expect("lands");
    assert!(progress.all_terminal && progress.all_succeeded);

    let failed = queue_of(&[item("album_14880659_1", "failed", &[], &["", ""])], None);
    rig.queue(failed.clone());
    assert!(
        rig.source
            .poll(&handle)
            .await
            .expect("added again")
            .queued_at_client
    );
    assert_eq!(rig.calls("/api/removeFromQueue"), 1);
    assert_eq!(rig.calls("/api/addToQueue"), 2);
    rig.queue(failed);
    match rig.source.poll(&handle).await {
        Err(SourceError::Rejected { reason, detail }) => {
            assert_eq!(reason, Reason::LocalFault);
            assert!(!reason.against_source());
            assert!(detail.contains("disk and network"), "{detail}");
        }
        other => panic!("expected a local fault refusal, got {other:?}"),
    }
    assert_eq!(rig.calls("/api/removeFromQueue"), 1);

    let mixed = SourceHandle {
        job_name: "album_6158273_1:t2:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    rig.queue(queue_of(
        &[item(
            "album_6158273_1",
            "failed",
            &[],
            &["wrongBitrate", ""],
        )],
        None,
    ));
    let progress = rig.source.poll(&mixed).await.expect("failed");
    assert!(progress.all_terminal && !progress.all_succeeded);

    let song = SourceHandle {
        job_name: "track_138546807_1:t3:0".to_owned(),
        ..SourceHandle::new(SourceTag::Deezer)
    };
    let partial = queue_of(
        &[item(
            "track_138546807_1",
            "withErrors",
            &["/downloads//a/01.mp3"],
            &[""],
        )],
        None,
    );
    rig.queue(partial.clone());
    assert!(
        rig.source
            .poll(&song)
            .await
            .expect("added again")
            .queued_at_client
    );
    rig.queue(partial);
    assert_eq!(rejected(rig.source.poll(&song).await), Reason::LocalFault);
    assert_eq!(rig.calls("/api/addToQueue"), 3);
}

/// A re-add whose add queued nothing after the item was removed is sent
/// again on the next poll, rather than the download read as dropped.
#[tokio::test]
async fn a_readd_survives_an_add_that_queued_nothing() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = rig.enqueue("t1").await.expect("an add");
    rig.journal_attempt("a1", "t1", &handle).await;

    rig.mock.state().add_nothing = true;
    rig.queue(queue_of(
        &[item("album_14880659_1", "failed", &[], &[""])],
        None,
    ));
    match rig.source.poll(&handle).await {
        Err(SourceError::Unavailable(text)) => assert!(text.contains("queued nothing"), "{text}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(rig.calls("/api/removeFromQueue"), 1);

    rig.mock.state().add_nothing = false;
    rig.queue(recorded("deemix/getQueue_empty.json"));
    let progress = rig.source.poll(&handle).await.expect("added back");
    assert!(progress.queued_at_client);
    assert_eq!(rig.calls("/api/addToQueue"), 3);
}

/// A re-add Deemix answers under another uuid points the attempt at it,
/// and the item's one re-add goes with it: failing the same way again
/// under the new uuid, it is not added a third time.
#[tokio::test]
async fn a_readd_under_another_uuid_keeps_its_one_try() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = rig.enqueue("t1").await.expect("an add");
    rig.journal_attempt("a1", "t1", &handle).await;

    let mut answer = recorded("deemix/addToQueue_album.json");
    answer["data"]["obj"][0]["uuid"] = json!("album_14880659_3");
    rig.mock.script(
        "/api/addToQueue",
        200,
        "application/json; charset=utf-8",
        &answer.to_string(),
    );
    rig.queue(queue_of(
        &[item("album_14880659_1", "failed", &[], &[""])],
        None,
    ));
    rig.source.poll(&handle).await.expect("added again");
    let attempts = rig.journal.read_attempts("t1").await.expect("attempts");
    assert_eq!(attempts[0].job_name, "album_14880659_3:t1:0");
    let renamed = SourceHandle {
        job_name: attempts[0].job_name.clone(),
        ..handle
    };

    rig.queue(queue_of(
        &[item("album_14880659_3", "failed", &[], &[""])],
        None,
    ));
    assert_eq!(
        rejected(rig.source.poll(&renamed).await),
        Reason::LocalFault
    );
    assert_eq!(rig.calls("/api/removeFromQueue"), 1);
    assert_eq!(rig.calls("/api/addToQueue"), 2);
}

/// A pending re-add whose adds keep queueing nothing gives the download up
/// on the third, and is then forgotten: the next poll adds nothing.
#[tokio::test]
async fn a_pending_readd_gives_up_on_the_third_empty_add() {
    let rig = rig().await;
    rig.allow_low();
    rig.album_task("t1", plan(in_rainbows(), None)).await;
    let handle = rig.enqueue("t1").await.expect("an add");
    rig.journal_attempt("a1", "t1", &handle).await;

    rig.mock.state().add_nothing = true;
    rig.queue(queue_of(
        &[item("album_14880659_1", "failed", &[], &[""])],
        None,
    ));
    assert!(matches!(
        rig.source.poll(&handle).await,
        Err(SourceError::Unavailable(_))
    ));
    rig.queue(recorded("deemix/getQueue_empty.json"));
    assert!(matches!(
        rig.source.poll(&handle).await,
        Err(SourceError::Unavailable(_))
    ));
    rig.clock.advance(2_000);
    assert_eq!(
        rejected(rig.source.poll(&handle).await),
        Reason::ClientDropped
    );
    assert_eq!(rig.calls("/api/addToQueue"), 4);
    rig.clock.advance(2_000);
    assert_eq!(
        rejected(rig.source.poll(&handle).await),
        Reason::ClientDropped
    );
    assert_eq!(rig.calls("/api/addToQueue"), 4);
}

/// A manual search for a song lists an album that lacks the song, as a
/// possible copy with a note, rather than hiding it.
#[tokio::test]
async fn a_song_search_lists_an_album_without_the_song() {
    let rig = rig().await;
    let mut tracks = in_rainbows();
    tracks.push(ExpectedTrack {
        track_number: 11,
        title: Some("Down Is The New Up".to_owned()),
        isrcs: Vec::new(),
        ..tracks[0].clone()
    });
    let target = SearchTarget {
        artist: "Radiohead".to_owned(),
        album_title: "In Rainbows".to_owned(),
        year: None,
        tracklist: tracks,
        barcode: None,
        wanted: vec![TrackPosition { disc: 1, track: 11 }],
        track: Some(TrackWant {
            artist: "Radiohead".to_owned(),
            title: "Down Is The New Up".to_owned(),
            duration_seconds: None,
            no_album: None,
        }),
        release_preferred: false,
        tracklist_unread: false,
    };
    let candidates = rig.source.search(&target).await.expect("search lists");
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].view.tier,
        droppedneedle_acquire::search_jobs::candidates::CandidateTier::Possible
    );
    assert!(!candidates[0].view.note.text.is_empty());
}
