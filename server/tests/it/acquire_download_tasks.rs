//! Download tasks: the admin reimport guard behind the request history
//! card, the dispatch seam's journal reads and writes, and the worker's
//! retry pass. Scratch journal only; the reimport landing itself is covered
//! by the download import journey.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use droppedneedle::acquire::db::AcquireDb;
use droppedneedle::acquire::dispatch::{Journal, UnifiedDispatch};
use droppedneedle::acquire::downloads::watchdog::RetryPolicy;
use droppedneedle::acquire::downloads::{
    http::downloads_router,
    state::{AttemptState, TaskStatus},
    store::NewTask,
};
use droppedneedle::acquire::landing::{LandingService, LandingSettings, LibrarySlot};
use droppedneedle::acquire::requests::dispatch::{
    DispatchOrigin, DispatchOutcome, DispatchRequest, DownloadDispatch as _,
};
use droppedneedle::acquire::worker::{DownloadWorker, WorkerConfig};
use droppedneedle::ids::UuidGenerator;
use serde_json::Value;
use tower::ServiceExt as _;

const ADMIN: &str = "u-root:admin:Root";
const USER: &str = "u-ada:user:Ada";

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
    let journal = Arc::new(Journal::new(db));
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
            store.transition_task("t-linked", TaskStatus::Failed, now, Some("mount gone"))?;
            store.transition_task("t-bare", TaskStatus::Failed, now, Some("mount gone"))?;
            Ok(())
        })
        .await
        .unwrap();
    journal
}

fn post(uri: &str, identity: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("x-slice-principal", identity)
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap()
}

/// A worker with the landing attached and no download client configured.
fn worker_over(journal: &Arc<Journal>) -> Arc<DownloadWorker> {
    let staging = staging_for(journal);
    let landing = Arc::new(LandingService::new(
        journal.clone(),
        LibrarySlot::default(),
        Arc::new(LandingSettings::default),
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
        )
        .with_landing(landing),
    )
}

async fn send(journal: &Arc<Journal>, req: Request<Body>) -> (StatusCode, Value) {
    let response = downloads_router(worker_over(journal))
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
    let (status, _) = send(&journal, post("/downloads/tasks/t-linked/reimport", USER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = send(&journal, post("/downloads/tasks/t-bare/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&journal, post("/downloads/tasks/t-live/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&journal, post("/downloads/tasks/nope/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&journal, post("/downloads/tasks/t-linked/reimport", ADMIN)).await;
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

    let snapshot = dispatch.task_progress("t-linked").await.unwrap().unwrap();
    assert_eq!(snapshot.status, "failed");
    assert_eq!(snapshot.error_message.as_deref(), Some("mount gone"));
    assert_eq!(snapshot.progress_percent, 0);
    assert!(dispatch.task_progress("nope").await.unwrap().is_none());

    assert!(dispatch.reimportable("t-linked").await.unwrap());
    assert!(!dispatch.reimportable("t-bare").await.unwrap());
    assert!(!dispatch.reimportable("t-live").await.unwrap());
    assert!(!dispatch.reimportable("nope").await.unwrap());
}

/// Dispatch over `journal`, staging under its scratch database directory.
fn dispatch_over(journal: Arc<Journal>) -> UnifiedDispatch {
    let staging = staging_for(&journal);
    UnifiedDispatch::new(
        journal,
        Arc::new(UuidGenerator),
        staging,
        Arc::new(RetryPolicy::default),
    )
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
    let journal = Arc::new(Journal::new(db.clone()));
    let dispatch = dispatch_over(journal.clone());
    let request = DispatchRequest {
        user_id: "u-new".to_owned(),
        kind: "album".to_owned(),
        key: "rg-1".to_owned(),
        artist_name: "artist".to_owned(),
        title: "album".to_owned(),
        origin: DispatchOrigin::User,
        release_mbid: None,
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
        .cancel_task("t-live")
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
    let journal = Arc::new(Journal::new(db.clone()));
    journal
        .run("test.seed", |store| {
            store.insert_task(&task("t-failed"), 1_000.0)?;
            store.transition_task("t-failed", TaskStatus::Failed, 1_000.0, Some("no peers"))
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
