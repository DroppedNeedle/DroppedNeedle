//! Download tasks: the admin reimport guard behind the request history
//! card, the dispatch seam's journal reads and writes, and the worker's
//! retry pass. Scratch journal only; the reimport landing itself is covered
//! by the download import journey.

use crate::common::Probe;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::dispatch::{
    DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget, DownloadDispatch,
};
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::worker::{DownloadWorker, run_startup_recovery};
use droppedneedle_acquire::downloads::{
    state::{AttemptDisposition, AttemptState, TaskStatus},
    store::{NewTask, TaskOutcome},
};
use droppedneedle_acquire::reason::Reason;
use droppedneedle_acquire::test_support::downloads_router;
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::ids::UuidGenerator;
use serde_json::Value;
use tower::ServiceExt as _;

use crate::common::acquire::{FixedAlbums, review_off_in, save_slskd, worker_over};
use crate::common::auth::{test_principal, with_test_principal};
use crate::common::bundles::quiet_ticks;
use droppedneedle_testkit::slskd::MockSlskd;
use serde_json::json;

/// A signed-in caller: user id and role.
type Who = (&'static str, Role);

const ADMIN: Who = ("u-root", Role::Admin);
const USER: Who = ("u-ada", Role::User);

fn task(id: &str) -> NewTask {
    NewTask {
        id: id.to_owned(),
        user_id: "u-ada".to_owned(),
        artist_name: "artist".to_owned(),
        album_title: "album".to_owned(),
        release_group_mbid: "rg-1".to_owned(),
        origin: "user".to_owned(),
        retry_count: 0,
    }
}

fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Scratch journal with one failed task whose download left its files
/// (`t-linked`, a preserved soulseek attempt), one failed task that never
/// downloaded (`t-bare`), and one live task (`t-live`).
async fn journal() -> Arc<Journal> {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    journal
        .run("test.seed", |store| {
            let now = now_f64();
            for id in ["t-linked", "t-bare", "t-live"] {
                store.insert_task(&task(id), now)?;
            }
            store.insert_attempt(
                "t-linked-a0",
                "t-linked",
                "soulseek",
                0,
                "",
                r#"{"source":"soulseek","username":"peer","filenames":["a.flac"],"job_name":""}"#,
                AttemptState::Preserved,
                now,
            )?;
            store.transition_task(
                "t-linked",
                TaskStatus::Failed,
                now,
                Some(TaskOutcome::new(Reason::DownloadFailed, "mount gone")),
            )?;
            store.transition_task(
                "t-bare",
                TaskStatus::Failed,
                now,
                Some(TaskOutcome::new(Reason::DownloadFailed, "mount gone")),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    journal
}

fn post(uri: &str, who: Who) -> (Request<Body>, Who) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();
    (request, who)
}

/// The production worker over `journal`, its settings beside the scratch
/// database, with no download client set up.
async fn idle_worker(journal: &Arc<Journal>) -> Arc<DownloadWorker> {
    let (worker, _) = worker_over(
        journal.db(),
        journal.clone(),
        &settings_beside(journal),
        staging_for(journal).join("slskd"),
        Arc::new(FixedAlbums::default()),
        None,
        droppedneedle_kernel::events::EventHub::new(),
    )
    .await;
    worker
}

/// The production worker over `journal` with Soulseek on, at `mock`, and
/// the search review off.
async fn soulseek_worker(journal: &Arc<Journal>, mock: &MockSlskd) -> Arc<DownloadWorker> {
    let settings = settings_beside(journal);
    save_slskd(&settings, &mock.base_url());
    review_off_in(&settings);
    let (worker, _) = worker_over(
        journal.db(),
        journal.clone(),
        &settings,
        staging_for(journal).join("slskd"),
        Arc::new(FixedAlbums::default()),
        None,
        droppedneedle_kernel::events::EventHub::new(),
    )
    .await;
    worker
}

/// The one file every Soulseek peer of these tests offers.
const PEER_FILE: &str = "@@music\\artist\\album\\01 Track.flac";

/// A Soulseek peer offering the album as one file, [`PEER_FILE`].
fn soulseek_peer(index: i64) -> Value {
    json!({"username": format!("peer-{index}"), "hasFreeUploadSlot": true,
        "uploadSpeed": 1_000_000, "queueLength": 0, "fileCount": 1, "lockedFileCount": 0,
        "lockedFiles": [], "token": 1,
        "files": [{"filename": PEER_FILE, "size": 30_000_000, "extension": "flac",
                   "bitDepth": 16, "sampleRate": 44_100, "length": 200}]})
}

async fn send(journal: &Arc<Journal>, (req, who): (Request<Body>, Who)) -> (StatusCode, Value) {
    let principal = test_principal(who.0, who.1, "sess-test", SessionKind::Standard);
    let router = downloads_router(idle_worker(journal).await).expect("router builds");
    let response = with_test_principal(router, principal)
        .oneshot(req)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// The dispatch seam reads a task's progress and the reimport guard from
/// the journal. The route refuses non-admins, answers 404 for missing,
/// never-downloaded and live tasks, and 409 for a reimportable task whose
/// download client is gone, which stays failed.
#[tokio::test]
async fn reimport_guard_and_task_snapshot() {
    let journal = journal().await;
    let dispatch = dispatch_over(journal.clone());

    let snapshot = dispatch.task("t-linked").await.unwrap().unwrap();
    assert_eq!(snapshot.status, TaskStatus::Failed);
    assert_eq!(snapshot.error_message.as_deref(), Some("mount gone"));
    assert_eq!(snapshot.progress_percent, 0);
    assert!(dispatch.task("nope").await.unwrap().is_none());

    assert!(dispatch.reimportable("t-linked").await.unwrap());
    assert!(!dispatch.reimportable("t-bare").await.unwrap());
    assert!(!dispatch.reimportable("t-live").await.unwrap());
    assert!(!dispatch.reimportable("nope").await.unwrap());

    let (status, _) = send(
        &journal,
        post("/api/v3/downloads/tasks/t-linked/reimport", USER),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    for id in ["t-bare", "t-live", "nope"] {
        let (status, _) = send(
            &journal,
            post(&format!("/api/v3/downloads/tasks/{id}/reimport"), ADMIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
    }

    let (status, _) = send(
        &journal,
        post("/api/v3/downloads/tasks/t-linked/reimport", ADMIN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let row = journal
        .run("test.read", |store| store.get_task("t-linked"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TaskStatus::Failed);
}

/// Dispatch over `journal`, with settings beside its scratch database.
fn dispatch_over(journal: Arc<Journal>) -> DownloadDispatch {
    let policy = settings_beside(&journal).live();
    DownloadDispatch::new(journal, Arc::new(UuidGenerator), policy)
}

/// Default settings beside the journal's scratch database.
fn settings_beside(journal: &Journal) -> droppedneedle_kernel::runtime_config::ConfigStore {
    let dir = journal
        .db()
        .path()
        .parent()
        .expect("scratch dir")
        .join("settings");
    droppedneedle_acquire::test_support::scratch_settings(&dir).expect("settings open")
}

fn staging_for(journal: &Journal) -> std::path::PathBuf {
    journal
        .db()
        .path()
        .parent()
        .expect("scratch database directory")
        .join("staging")
}

fn task_count(db: &AcquireDb) -> i64 {
    db.db().one("SELECT COUNT(*) FROM download_tasks", [])
}

// A dispatch whose task insert fails keeps no idempotency key, so the
// next dispatch with that key starts a real task instead of answering a
// task id that was never written.
#[tokio::test]
async fn failed_dispatch_keeps_no_key() {
    let db = AcquireDb::scratch().unwrap();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let dispatch = dispatch_over(journal.clone());
    let request = DispatchRequest {
        user_id: "u-new".to_owned(),
        target: DispatchTarget::Album {
            release_group_mbid: "rg-1".to_owned(),
        },
        artist_name: "artist".to_owned(),
        title: "album".to_owned(),
        origin: DispatchOrigin::User,
        release_mbid: None,
        wanted: Vec::new(),
        planned_album: None,
        idempotency_key: Some("ask-1".to_owned()),
    };
    // No such user yet: the task insert fails on its foreign key.
    assert!(dispatch.dispatch(&request).await.is_err());

    db.add_user("u-new", "New", "user").await.unwrap();
    let DispatchOutcome::Dispatched { task_id } = dispatch.dispatch(&request).await.unwrap() else {
        panic!("dispatch starts a task");
    };
    let row = journal
        .run("test.read", move |store| store.get_task(&task_id))
        .await
        .unwrap();
    assert!(row.is_some(), "the answered task exists");
}

// Cancel marks the live transfer for discard in the same write that
// cancels the task, so the cleanup pass stops the client transfer.
#[tokio::test]
async fn cancel_hands_the_transfer_to_cleanup() {
    let journal = journal().await;
    journal
        .run("test.attempt", |store| {
            store.insert_attempt(
                "t-live-a0",
                "t-live",
                "usenet",
                0,
                "job-1",
                "{}",
                AttemptState::Acquiring,
                now_f64(),
            )
        })
        .await
        .unwrap();
    dispatch_over(journal.clone())
        .cancel("t-live")
        .await
        .unwrap();

    let (task, attempt) = journal
        .run("test.read", |store| {
            Ok((store.get_task("t-live")?, store.get_attempt("t-live-a0")?))
        })
        .await
        .unwrap();
    assert_eq!(task.unwrap().status, TaskStatus::Cancelled);
    let attempt = attempt.unwrap();
    assert_eq!(attempt.state, AttemptState::CleanupPending);
    assert_eq!(attempt.disposition, AttemptDisposition::Discard);
}

// An auto-retry whose successor insert fails is tried again on the next
// pass instead of being suppressed for good.
#[tokio::test]
async fn failed_retry_insert_retries_next_pass() {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    journal
        .run("test.seed", |store| {
            store.insert_task(&task("t-failed"), 1_000.0)?;
            store.transition_task(
                "t-failed",
                TaskStatus::Failed,
                1_000.0,
                Some(TaskOutcome::new(Reason::NoSource, "no peers")),
            )
        })
        .await
        .unwrap();
    db.write("test.trigger", |tx| {
        tx.execute_batch(
            "CREATE TRIGGER fail_retry BEFORE INSERT ON download_tasks \
             WHEN NEW.retry_count > 0 BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let worker = idle_worker(&journal).await;

    worker.run_once(1).await;
    assert_eq!(task_count(&db), 1, "the injected failure blocks the insert");

    db.write("test.untrigger", |tx| {
        tx.execute_batch("DROP TRIGGER fail_retry;")?;
        Ok(())
    })
    .await
    .unwrap();
    worker.run_once(2).await;
    assert_eq!(task_count(&db), 2, "the next pass spawns the successor");
}

// One live download per album, retries included: a failed task whose
// album another user's download already fetches, or the library already
// holds, spawns no retry. It stops, and its row says why. A failed task
// with neither still retries.
#[tokio::test]
async fn a_retry_stops_once_the_album_is_on_its_way_or_in() {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    db.add_user("u-bob", "Bob", "user").await.unwrap();
    db.write("test.seed_library", |tx| {
        tx.execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
               VALUES ('ar1', 'A', 'a', 'person', 0, 0);
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
               album_artist_id, album_artist_name, grouping_source, created_at, updated_at)
               VALUES ('al1', 'r1', 'g', 'T', 't', 'ar1', 'A', 'automatic', 0, 0);
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
               path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
               album_title, album_title_folded, file_format, bit_rate, ingest_source,
               imported_at, membership_source, availability)
               VALUES ('tr1', 'al1', 'r1', 'a.mp3', 'a.mp3', 'h', 1, 0, 's', 'T', 't', 'T', 't',
                 'mp3', 320, 'scan', 0, 'automatic', 'indexed');
             INSERT INTO local_album_external_identities
               (local_album_id, release_group_mbid, decision_source, selected_at)
               VALUES ('al1', 'rg-held', 'automatic', 0);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    journal
        .run("test.seed", |store| {
            for (id, group) in [
                ("t-busy", "rg-busy"),
                ("t-held", "rg-held"),
                ("t-plain", "rg-plain"),
            ] {
                let row = NewTask {
                    release_group_mbid: group.to_owned(),
                    ..task(id)
                };
                store.insert_task(&row, 1_000.0)?;
                store.transition_task(
                    id,
                    TaskStatus::Failed,
                    1_000.0,
                    Some(TaskOutcome::new(Reason::NoSource, "no peers")),
                )?;
            }
            // Bob asked for the busy album since, in another case.
            let live = NewTask {
                user_id: "u-bob".to_owned(),
                release_group_mbid: "RG-BUSY".to_owned(),
                ..task("t-bob")
            };
            store.insert_task(&live, 2_000.0)
        })
        .await
        .unwrap();
    let worker = idle_worker(&journal).await;

    worker.run_once(1).await;

    assert_eq!(task_count(&db), 5, "only the plain failure retried");
    let (_, body) = send(&journal, get("/api/v3/downloads/tasks", USER)).await;
    let items = body["items"].as_array().cloned().unwrap_or_default();
    let row = |id: &str| {
        items
            .iter()
            .find(|item| item["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} listed: {body}"))
    };
    for (id, code, text) in [
        ("t-busy", "already_downloading", "Already downloading."),
        ("t-held", "already_in_library", "Already in your library."),
    ] {
        let row = row(id);
        assert_eq!(row["status"], "cancelled", "{id}: {row}");
        assert_eq!(row["reason"]["code"], code, "{id}: {row}");
        assert_eq!(row["reason"]["text"], text, "{id}: {row}");
        assert!(row["next_retry_at"].is_null(), "{id}: no retry is due");
    }
    assert_eq!(row("t-plain")["status"], "failed");
}

fn get(uri: &str, who: Who) -> (Request<Body>, Who) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    (request, who)
}

// The queue journey: a user sees only their own tasks, each failure
// carries one plain reason from the import decision, a retry clears the
// album's blocklist and starts a successor, cancel stops it, clear removes
// finished rows, and the blocklist stays admin only.
#[tokio::test]
async fn queue_journey_scopes_explains_retries_and_clears() {
    use droppedneedle_acquire::downloads::QUARANTINE_TTL_SECONDS;
    use droppedneedle_acquire::downloads::landing_rows::{ImportDecisionRow, ImportOutcome};

    let db = AcquireDb::scratch().unwrap();
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    db.add_user("u-bob", "Bob", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    journal
        .run("test.seed", |store| {
            let now = now_f64();
            store.insert_task(&task("q-failed"), now)?;
            store.insert_task(&task("q-done"), now - 10.0)?;
            store.insert_task(
                &NewTask {
                    user_id: "u-bob".to_owned(),
                    // Another album: a live download of Ada's would turn
                    // her retry into that download.
                    release_group_mbid: "rg-bob".to_owned(),
                    ..task("q-bob")
                },
                now,
            )?;
            store.transition_task(
                "q-failed",
                TaskStatus::Failed,
                now,
                Some(TaskOutcome::new(Reason::DownloadFailed, "x")),
            )?;
            store.transition_task("q-done", TaskStatus::Completed, now, None)?;
            store.record_import_decision(&ImportDecisionRow {
                task_id: "q-failed".to_owned(),
                outcome: ImportOutcome::Rejected,
                reason_code: Some(Reason::WrongAlbum),
                checks_json: "[]".to_owned(),
                missing_positions: "[]".to_owned(),
                decided_at: now,
                ..Default::default()
            })?;
            store.record_quarantine(
                "soulseek",
                "peer/a.flac",
                "download_failed",
                Some("rg-1"),
                now,
                QUARANTINE_TTL_SECONDS,
            )
        })
        .await
        .unwrap();

    let (status, body) = send(&journal, get("/api/v3/downloads/tasks", USER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "own tasks only: {body}");
    let failed = items.iter().find(|item| item["id"] == "q-failed").unwrap();
    assert_eq!(failed["reason"]["code"], "wrong_album");
    assert!(failed["reason"]["action"].as_str().unwrap().len() > 10);
    assert_eq!(failed["decision"]["outcome"], "rejected");
    let (status, _) = send(&journal, get("/api/v3/downloads/quarantine", USER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, body) = send(&journal, get("/api/v3/downloads/quarantine", ADMIN)).await;
    assert_eq!(body["items"][0]["username"], "peer");

    let (status, body) = send(
        &journal,
        post("/api/v3/downloads/tasks/q-failed/retry", USER),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let successor = body["task_id"].as_str().unwrap().to_owned();
    let (_, body) = send(&journal, get("/api/v3/downloads/quarantine", ADMIN)).await;
    assert_eq!(body["items"].as_array().map(Vec::len), Some(0));

    let uri = format!("/api/v3/downloads/tasks/{successor}/cancel");
    let (status, _) = send(&journal, post(&uri, USER)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&journal, post("/api/v3/downloads/clear", USER)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cleared"], 2, "the done task and the cancelled retry");

    let (_, body) = send(&journal, get("/api/v3/downloads/activity-summary", USER)).await;
    assert_eq!(body["failed_count"], 1);
    assert_eq!(body["active_count"], 0);
}

// A long write on the lane does not hold up the journal's reads: startup
// recovery lists the live tasks on the read pool while a write is open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn journal_reads_do_not_wait_for_a_held_write() {
    let journal = journal().await;
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let db = journal.db().db().clone();
    let held = tokio::spawn(async move {
        db.write(Lane::Background, "test.hold", move |_tx| {
            let _ = started_tx.send(());
            // Held until recovery is done, or for longer than recovery may
            // take: a read behind this write cannot finish in time.
            Ok::<_, DbError>(release_rx.recv_timeout(Duration::from_secs(30)).is_ok())
        })
        .await
    });
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("the held write started");

    let recovery =
        tokio::time::timeout(Duration::from_secs(20), run_startup_recovery(&journal)).await;
    let _ = release_tx.send(());
    let released = held.await.unwrap().unwrap();
    let report = recovery
        .expect("the live-task read waited for the held write")
        .unwrap();
    assert!(released, "the write stayed open while recovery read");
    assert_eq!(report.redispatched, 1);
}

// A task no source can serve any more is never settled as a plain failure
// while its landing history cannot be read: the tracks it already imported
// would be lost from its outcome. It waits for the next pass instead.
#[tokio::test]
async fn unreadable_history_never_fails_a_partly_imported_task() {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    journal
        .run("test.seed", |store| {
            store.insert_task(&task("t-short"), 1_000.0)?;
            for index in 0..3 {
                store.insert_attempt(
                    &format!("t-short-a{index}"),
                    "t-short",
                    "soulseek",
                    index,
                    "",
                    r#"{"source":"soulseek","username":"peer","filenames":["a.flac"],"job_name":""}"#,
                    AttemptState::Preserved,
                    1_000.0,
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    db.write("test.history", |tx| {
        tx.execute_batch(
            "INSERT INTO download_import_decisions \
             (task_id, outcome, files_total, files_imported, decided_at) \
             VALUES ('t-short', 'partial', 2, 1, 1000.0); \
             ALTER TABLE download_import_decisions RENAME TO download_import_decisions_away;",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    // A source that is on, so the pass reaches the task: with its three
    // attempts used it would settle, but the history does not read.
    let mock = MockSlskd::start().await.expect("mock starts");
    let worker = soulseek_worker(&journal, &mock).await;

    worker.run_once(1).await;
    let row = journal.read_task("t-short").await.unwrap().unwrap();
    assert_eq!(
        row.status,
        TaskStatus::Queued,
        "an unread history settles nothing"
    );
}

/// How the album's first copy was chosen.
#[derive(Clone, Copy)]
enum Chosen {
    /// Searched for by the worker.
    Searched,
    /// Taken by the automatic search review.
    Reviewed,
    /// Picked by a person from a search.
    Picked,
}

/// One queued Soulseek album polled past the 15-minute preferred-quality
/// wait (still under the queued timeout). Answers each attempt's index and
/// state, the task's last outcome and the blocklist size.
async fn queued_past_the_wait(
    copies: i64,
    chosen: Chosen,
    discards_fail: bool,
) -> (Vec<(i64, String)>, Option<Reason>, usize) {
    use droppedneedle_acquire::downloads::store::TaskDetails;
    use droppedneedle_acquire::search_jobs::store::{
        JobPayload, JobStore, NewJob, PickedTask, ReviewOutcome,
    };
    use droppedneedle_kernel::test_support::ManualClock;

    let scratch = AcquireDb::scratch().unwrap();
    let clock = Arc::new(ManualClock::at(1_800_000_000_000));
    let db = AcquireDb::new(scratch.db().clone(), clock.clone());
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    journal
        .run("test.seed", |store| {
            store.insert_task(&task("t-slow"), 1_800_000_000.0)
        })
        .await
        .unwrap();
    let job = || NewJob {
        id: "job-1".to_owned(),
        user_id: "u-ada".to_owned(),
        artist_name: "artist".to_owned(),
        album_title: "album".to_owned(),
        year: None,
        release_group_mbid: Some("rg-1".to_owned()),
        release_mbid: None,
    };
    let payload = || JobPayload {
        release_mbid: None,
        tracks_total: None,
        candidates: Vec::new(),
    };
    let jobs = JobStore::new(db);
    let peer = Some("peer-0".to_owned());
    match chosen {
        Chosen::Searched => {}
        Chosen::Reviewed => {
            let take = ReviewOutcome::Take(0, "soulseek".to_owned(), peer);
            assert!(
                jobs.record_review(job(), payload(), "t-slow", take)
                    .await
                    .unwrap()
            );
        }
        Chosen::Picked => {
            let park = ReviewOutcome::Park;
            assert!(
                jobs.record_review(job(), payload(), "t-slow", park)
                    .await
                    .unwrap()
            );
            let picked = PickedTask {
                task: task("t-unused"),
                details: TaskDetails::default(),
                source: "soulseek".to_owned(),
                username: peer,
                candidate_index: 0,
                quality_waived: false,
            };
            let linked = jobs.pick("job-1", picked).await.unwrap().unwrap();
            assert_eq!(linked, "t-slow", "the parked download takes the pick");
        }
    }
    // Each of `copies` peers has the album; an enqueued copy waits in its
    // peer's queue.
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses((0..copies).map(soulseek_peer).collect());
    mock.queue_enqueued(true);
    mock.fail_removals(discards_fail);
    let worker = soulseek_worker(&journal, &mock).await;
    worker.run_once(1).await;
    clock.advance(16 * 60 * 1000);
    worker.run_once(2).await;
    clock.advance(60 * 1000);
    worker.run_once(3).await;
    let (attempts, quarantined) = journal
        .read("test.read", |store| {
            Ok((
                store.list_attempts("t-slow")?,
                store.list_quarantine()?.len(),
            ))
        })
        .await
        .unwrap();
    let task = journal.read_task("t-slow").await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Downloading);
    let attempts = attempts
        .into_iter()
        .map(|attempt| (attempt.candidate_index, attempt.state.as_str().to_owned()))
        .collect();
    drop(scratch);
    (attempts, task.outcome_code, quarantined)
}

// A Soulseek copy queued at its peer past the preferred-quality wait gives
// way only once the next copy is queued: with a second peer it moves, the
// slow copy is cleaned up (or kept for cleanup while slskd refuses to drop
// it) and nothing is blocklisted; with one peer it keeps its place. A first
// copy the automatic review took gives way; one a person picked does not.
#[tokio::test]
async fn a_queued_copy_gives_way_only_to_a_copy_that_is_queued() {
    let waiting = |index: i64| (index, "acquiring".to_owned());
    let moved = vec![(0, "complete".to_owned()), waiting(1)];

    let (attempts, outcome, quarantined) = queued_past_the_wait(2, Chosen::Searched, false).await;
    assert_eq!(
        attempts, moved,
        "the next copy is queued, the slow one discarded"
    );
    assert_eq!(outcome, Some(Reason::QueueGaveWay));
    assert_eq!(quarantined, 0, "a slow peer is not blocklisted");

    let (attempts, outcome, quarantined) = queued_past_the_wait(2, Chosen::Searched, true).await;
    assert_eq!(
        attempts,
        vec![(0, "cleanup_pending".to_owned()), waiting(1)],
        "a slow copy slskd will not drop stays for cleanup"
    );
    assert_eq!(outcome, Some(Reason::QueueGaveWay));
    assert_eq!(quarantined, 0, "a slow peer is not blocklisted");

    let (attempts, outcome, _) = queued_past_the_wait(1, Chosen::Searched, false).await;
    assert_eq!(attempts, vec![waiting(0)], "the only peer keeps its place");
    assert_eq!(outcome, None);

    let (attempts, outcome, _) = queued_past_the_wait(2, Chosen::Reviewed, false).await;
    assert_eq!(attempts, moved, "an automatic pick gives way");
    assert_eq!(outcome, Some(Reason::QueueGaveWay));

    let (attempts, outcome, _) = queued_past_the_wait(2, Chosen::Picked, false).await;
    assert_eq!(attempts, vec![waiting(0)], "a person's pick stays");
    assert_eq!(outcome, None);
}

// A restart measures a copy from its own enqueue, not from the task's
// creation. A copy queued ten minutes ago and moving bytes stays downloading
// on a fresh worker's first poll, though its task is seven hours old; the
// deadline would blocklist it.
#[tokio::test]
async fn a_restart_measures_a_copy_from_its_own_enqueue() {
    use droppedneedle_kernel::test_support::ManualClock;

    let now = 1_800_000_000.0;
    let scratch = AcquireDb::scratch().unwrap();
    let clock = Arc::new(ManualClock::at(1_800_000_000_000));
    let db = AcquireDb::new(scratch.db().clone(), clock.clone());
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    journal
        .run("test.seed", move |store| {
            store.insert_task(&task("t-old"), now - 7.0 * 3600.0)?;
            store.insert_attempt(
                "t-old-a0",
                "t-old",
                "soulseek",
                0,
                "",
                &json!({"source": "soulseek", "username": "peer-0", "filenames": [PEER_FILE],
                    "job_name": "t-old-0"})
                .to_string(),
                AttemptState::Acquiring,
                now - 600.0,
            )?;
            store.transition_task("t-old", TaskStatus::Downloading, now, None)
        })
        .await
        .unwrap();
    // The copy is moving bytes at its peer.
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses(vec![soulseek_peer(0)]);
    mock.inject_transfer_progress(
        "peer-0",
        PEER_FILE,
        30_000_000,
        12_000_000,
        "InProgress",
        None,
        None,
    );
    // A new worker has an empty poll cache, as after a restart.
    let worker = soulseek_worker(&journal, &mock).await;
    worker.run_once(1).await;
    let task = journal.read_task("t-old").await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Downloading);
    let quarantined = journal
        .read("test.read", |store| Ok(store.list_quarantine()?.len()))
        .await
        .unwrap();
    assert_eq!(quarantined, 0, "a copy that is moving is not blocklisted");
}

/// An album ask without a release fetches the album's chosen edition. A
/// busy read of that edition queues nothing and answers busy, so the ask
/// waits for a later pass, which fetches the chosen edition: the album is
/// never queued without it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_edition_read_defers_the_dispatch_until_the_edition_reads() {
    use droppedneedle_acquire::dispatch::DispatchError;
    use droppedneedle_kernel::db::{DbConfig, LaneLimits, open_runtime};

    let dir = crate::common::ScratchDir::new("dispatch-busy-edition");
    let path = dir.join("journal.db");
    crate::common::migrated_db(&path);
    let mut config = DbConfig::new(
        &path,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    )
    .with_read_wait(Duration::from_millis(200))
    .with_lane_limits(LaneLimits {
        watch_tick: Duration::from_millis(10),
        ..LaneLimits::default()
    });
    config.readers = 1;
    let runtime = open_runtime(&config).await.expect("runtime opens");
    let db = AcquireDb::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    db.write("test.seed_edition", |tx| {
        tx.execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
               VALUES ('ar1', 'A', 'a', 'person', 0, 0);
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
               album_artist_id, grouping_source, created_at, updated_at)
               VALUES ('al1', 'r1', 'g', 'T', 't', 'ar1', 'automatic', 0, 0);
             INSERT INTO local_album_external_identities (local_album_id, provider,
               release_group_mbid, release_mbid, decision_source, selected_at, row_revision)
               VALUES ('al1', 'musicbrainz', 'rg-1', 'rel-chosen', 'manual', 5, 1);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let settings = droppedneedle_acquire::test_support::scratch_settings(&dir.join("settings"))
        .expect("settings open");
    let dispatch = DownloadDispatch::new(journal.clone(), Arc::new(UuidGenerator), settings.live());
    let request = DispatchRequest {
        user_id: "u-ada".to_owned(),
        target: DispatchTarget::Album {
            release_group_mbid: "rg-1".to_owned(),
        },
        artist_name: "artist".to_owned(),
        title: "album".to_owned(),
        origin: DispatchOrigin::User,
        release_mbid: None,
        wanted: Vec::new(),
        planned_album: None,
        idempotency_key: None,
    };

    // The one reader is taken, so the edition read hears busy.
    let (entered, inside) = std::sync::mpsc::channel();
    let (let_go, release) = std::sync::mpsc::channel::<()>();
    let reader = {
        let db = runtime.db().clone();
        std::thread::spawn(move || {
            db.read_blocking("test.hold", move |_| {
                let _ = entered.send(());
                let _ = release.recv();
                Ok::<_, DbError>(())
            })
        })
    };
    inside
        .recv_timeout(Duration::from_secs(10))
        .expect("the reader is held");
    let busy = dispatch.dispatch(&request).await;
    drop(let_go);
    reader
        .join()
        .expect("reader joins")
        .expect("held read ends");
    assert!(
        matches!(busy, Err(DispatchError::Busy { .. })),
        "a busy edition read is put back, got {busy:?}"
    );
    assert_eq!(task_count(&db), 0, "nothing is queued");

    let DispatchOutcome::Dispatched { task_id } = dispatch.dispatch(&request).await.unwrap() else {
        panic!("the next pass dispatches");
    };
    let details = journal
        .read("test.details", move |store| store.task_details(&task_id))
        .await
        .unwrap();
    assert_eq!(details.release_mbid.as_deref(), Some("rel-chosen"));
    runtime.shutdown().await;
}

/// MusicBrainz answers `asked` with the release it was merged into, and
/// counts how often it is asked.
struct Redirecting {
    asked: &'static str,
    found: droppedneedle_acquire::target::lookup::AlbumRelease,
    calls: std::sync::atomic::AtomicUsize,
}

impl droppedneedle_acquire::target::lookup::AlbumLookup for Redirecting {
    fn recording_releases<'a>(
        &'a self,
        _recording_mbid: &'a str,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<
            Option<Vec<droppedneedle_acquire::target::lookup::ReleaseCandidate>>,
            droppedneedle_acquire::target::lookup::LookupError,
        >,
    > {
        Box::pin(async { Ok(None) })
    }

    fn release<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_acquire::target::lookup::AlbumRelease>,
            droppedneedle_acquire::target::lookup::LookupError,
        >,
    > {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let found = (release_mbid == self.asked).then(|| self.found.clone());
        Box::pin(async move { Ok(found) })
    }

    fn first_release_date<'a>(
        &'a self,
        _release_group_mbid: &'a str,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<Option<String>, droppedneedle_acquire::target::lookup::LookupError>,
    > {
        Box::pin(async { Ok(None) })
    }
}

/// "Get missing tracks" asks for some positions of one edition. When
/// MusicBrainz redirects that edition to the release it was merged into,
/// the plan keeps the tracklist under the found release, and every later
/// target (another source, a retry of the search) reuses it with the same
/// positions, without asking MusicBrainz again.
#[tokio::test]
async fn a_redirected_edition_keeps_its_plan_for_later_targets() {
    use droppedneedle_acquire::downloads::TrackPosition;
    use droppedneedle_acquire::target::Targets;
    use droppedneedle_acquire::target::lookup::{AlbumRelease, AlbumTrack};

    let db = AcquireDb::scratch().unwrap();
    db.add_user("u-ada", "Ada", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    let dispatch = dispatch_over(journal.clone());
    let DispatchOutcome::Dispatched { task_id } = dispatch
        .dispatch(&DispatchRequest {
            user_id: "u-ada".to_owned(),
            target: DispatchTarget::Album {
                release_group_mbid: "rg-1".to_owned(),
            },
            artist_name: "artist".to_owned(),
            title: "album".to_owned(),
            origin: DispatchOrigin::User,
            release_mbid: Some("rel-asked".to_owned()),
            wanted: vec![(1, 2)],
            planned_album: None,
            idempotency_key: None,
        })
        .await
        .unwrap()
    else {
        panic!("the album dispatches");
    };
    let lookup = Arc::new(Redirecting {
        asked: "rel-asked",
        found: AlbumRelease {
            id: "rel-found".to_owned(),
            title: "album".to_owned(),
            release_group_mbid: "rg-1".to_owned(),
            artist: "artist".to_owned(),
            various_artists: false,
            year: Some(1994),
            tracks: (1..=3)
                .map(|position| AlbumTrack {
                    disc: 1,
                    position,
                    title: format!("track {position}"),
                    duration_seconds: Some(200.0),
                    recording_mbid: format!("rec-{position}"),
                    release_track_mbid: format!("rt-{position}"),
                })
                .collect(),
        },
        calls: 0.into(),
    });
    let targets = Targets::new(journal.clone(), lookup.clone());
    let task = journal.read_task(&task_id).await.unwrap().unwrap();
    let wanted = [TrackPosition { disc: 1, track: 2 }];

    let first = targets.target(&task).await.expect("first target");
    assert_eq!(
        (first.tracklist.len(), first.wanted.as_slice()),
        (3, &wanted[..])
    );
    let later = targets.target(&task).await.expect("later target");
    assert_eq!(later, first, "a later target is the planned one");
    assert_eq!(
        lookup.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "MusicBrainz is asked once"
    );
}
