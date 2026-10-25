//! Download tasks: the admin reimport guard behind the request history
//! card, the dispatch seam's journal reads and writes, and the worker's
//! retry pass. Scratch journal only; the reimport landing itself is covered
//! by the download import journey.

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
use droppedneedle_acquire::downloads::{
    http::downloads_router,
    state::{AttemptState, TaskStatus},
    store::{NewTask, TaskOutcome},
};
use droppedneedle_acquire::landing::LandingService;
use droppedneedle_acquire::reason::Reason;
use droppedneedle_acquire::worker::{
    DownloadWorker, FixedSource, WorkerConfig, run_startup_recovery,
};
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::ids::UuidGenerator;
use serde_json::Value;
use tower::ServiceExt as _;

use crate::common::auth::{test_principal, with_test_principal};
use crate::common::bundles::quiet_ticks;

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

/// A worker with the landing attached and no download client configured.
fn worker_over(journal: &Arc<Journal>) -> Arc<DownloadWorker> {
    let staging = staging_for(journal);
    let landing = Arc::new(LandingService::new(
        journal.clone(),
        None,
        settings_beside(journal).live(),
        staging.join("held"),
    ));
    Arc::new(
        DownloadWorker::fixed(
            journal.clone(),
            Vec::new(),
            WorkerConfig {
                staging_root: staging,
                ..WorkerConfig::default()
            },
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(landing),
    )
}

async fn send(journal: &Arc<Journal>, (req, who): (Request<Body>, Who)) -> (StatusCode, Value) {
    let principal = test_principal(who.0, who.1, "sess-test", SessionKind::Standard);
    let router = downloads_router(worker_over(journal)).expect("router builds");
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

// Non-admins are refused; missing, never-downloaded and live tasks answer
// 404; a reimportable task whose download client is gone answers 409 and
// stays failed.
#[tokio::test]
async fn reimport_guards_role_link_and_client() {
    let journal = journal().await;
    let (status, _) = send(
        &journal,
        post("/api/v3/downloads/tasks/t-linked/reimport", USER),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = send(
        &journal,
        post("/api/v3/downloads/tasks/t-bare/reimport", ADMIN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &journal,
        post("/api/v3/downloads/tasks/t-live/reimport", ADMIN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &journal,
        post("/api/v3/downloads/tasks/nope/reimport", ADMIN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

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

// The dispatch seam reads progress and the reimport guard from the
// journal, so the request views render live task data.
#[tokio::test]
async fn dispatch_reads_progress_and_guard_from_journal() {
    let dispatch = dispatch_over(journal().await);

    let snapshot = dispatch.task("t-linked").await.unwrap().unwrap();
    assert_eq!(snapshot.status, TaskStatus::Failed);
    assert_eq!(snapshot.error_message.as_deref(), Some("mount gone"));
    assert_eq!(snapshot.progress_percent, 0);
    assert!(dispatch.task("nope").await.unwrap().is_none());

    assert!(dispatch.reimportable("t-linked").await.unwrap());
    assert!(!dispatch.reimportable("t-bare").await.unwrap());
    assert!(!dispatch.reimportable("t-live").await.unwrap());
    assert!(!dispatch.reimportable("nope").await.unwrap());
}

/// Dispatch over `journal`, staging under its scratch database directory.
fn dispatch_over(journal: Arc<Journal>) -> DownloadDispatch {
    let staging = staging_for(&journal);
    let policy = settings_beside(&journal).live();
    DownloadDispatch::new(journal, Arc::new(UuidGenerator), staging, policy)
}

/// Default settings beside the journal's scratch database.
fn settings_beside(journal: &Journal) -> droppedneedle_kernel::runtime_config::ConfigStore {
    let dir = journal
        .db()
        .path()
        .parent()
        .expect("scratch dir")
        .join("settings");
    droppedneedle_acquire::settings::scratch_settings(&dir).expect("settings open")
}

fn staging_for(journal: &Journal) -> std::path::PathBuf {
    journal
        .db()
        .path()
        .parent()
        .expect("scratch database directory")
        .join("staging")
}

async fn task_count(db: &AcquireDb) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM download_tasks")
        .fetch_one(db.pool())
        .await
        .unwrap()
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
    assert_eq!(attempt.disposition, "discard");
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
    let staging = staging_for(&journal);
    let worker = Arc::new(DownloadWorker::fixed(
        journal,
        Vec::new(),
        WorkerConfig {
            staging_root: staging,
            ..WorkerConfig::default()
        },
        droppedneedle_kernel::events::EventHub::new(),
        Default::default(),
    ));

    worker.run_once(1).await;
    assert_eq!(
        task_count(&db).await,
        1,
        "the injected failure blocks the insert"
    );

    db.write("test.untrigger", |tx| {
        tx.execute_batch("DROP TRIGGER fail_retry;")?;
        Ok(())
    })
    .await
    .unwrap();
    worker.run_once(2).await;
    assert_eq!(
        task_count(&db).await,
        2,
        "the next pass spawns the successor"
    );
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
    use droppedneedle_acquire::downloads::landing_rows::ImportDecisionRow;

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
                outcome: "rejected".to_owned(),
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
    let (status, _) = send(&journal, get("/api/v3/downloads/tasks/q-bob", USER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

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

    let staging = staging_for(&journal);
    let recovery = tokio::time::timeout(
        Duration::from_secs(20),
        run_startup_recovery(&journal, &staging),
    )
    .await;
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
    let staging = staging_for(&journal);
    let source = Arc::new(FixedSource::new(staging.join("downloads"), Vec::new()));
    let worker = Arc::new(DownloadWorker::fixed(
        journal.clone(),
        vec![crate::as_source(source)],
        WorkerConfig {
            staging_root: staging,
            ..WorkerConfig::default()
        },
        droppedneedle_kernel::events::EventHub::new(),
        Default::default(),
    ));

    worker.run_once(1).await;
    let row = journal.read_task("t-short").await.unwrap().unwrap();
    assert_eq!(
        row.status,
        TaskStatus::Queued,
        "an unread history settles nothing"
    );
}
