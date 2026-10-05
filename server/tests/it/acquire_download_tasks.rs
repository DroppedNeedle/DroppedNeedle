//! Download-task HTTP briefs: the admin reimport behind the request
//! history card, plus the dispatch seam's journal reads that feed the
//! request views. Scratch journal only; the worker never runs here.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use droppedneedle::acquire::dispatch::{Journal, UnifiedDispatch};
use droppedneedle::acquire::downloads::{
    http::downloads_router, state::TaskStatus, store::NewTask,
};
use droppedneedle::acquire::requests::dispatch::DownloadDispatch as _;
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

/// Scratch journal with one linked failed task (`t-linked`), one bare
/// failed task (`t-bare`), and one live task (`t-live`).
fn journal() -> Arc<Journal> {
    let journal = Arc::new(Journal::memory().unwrap());
    journal
        .with_store(|store| {
            let now = now_f64();
            for id in ["t-linked", "t-bare", "t-live"] {
                store.insert_task(&task(id), now)?;
            }
            store.link_candidate("t-linked", "peer", "job-1", 2, now)?;
            store.link_candidate("t-live", "peer", "job-1", 0, now)?;
            store.transition_task("t-linked", TaskStatus::Failed, now, Some("mount gone"))?;
            store.transition_task("t-bare", TaskStatus::Failed, now, Some("mount gone"))?;
            Ok::<_, droppedneedle::acquire::downloads::store::StoreError>(())
        })
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

async fn send(journal: &Arc<Journal>, req: Request<Body>) -> (StatusCode, Value) {
    let response = downloads_router(journal.clone())
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

// An admin reimport puts a linked failed task back in line; a second
// call finds it live and answers 404.
#[tokio::test]
async fn reimport_requeues_then_reports_live() {
    let journal = journal();
    let (status, body) = send(&journal, post("/downloads/tasks/t-linked/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["success"], true);
    assert_eq!(body["status"], "queued");

    let row = journal
        .with_store(|store| store.get_task("t-linked"))
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TaskStatus::Queued);
    assert!(row.error_message.is_none());

    let (status, _) = send(&journal, post("/downloads/tasks/t-linked/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// Non-admins are refused; missing and unlinked tasks answer 404.
#[tokio::test]
async fn reimport_guards_role_and_link() {
    let journal = journal();
    let (status, _) = send(&journal, post("/downloads/tasks/t-linked/reimport", USER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = send(&journal, post("/downloads/tasks/t-bare/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&journal, post("/downloads/tasks/t-live/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&journal, post("/downloads/tasks/nope/reimport", ADMIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// The dispatch seam reads progress and the reimport guard from the
// journal, so the request views render live task data.
#[tokio::test]
async fn dispatch_reads_progress_and_guard_from_journal() {
    let journal = journal();
    let staging = std::env::temp_dir().join(format!("dn-reimport-{}", std::process::id()));
    let dispatch = UnifiedDispatch::new(journal, Arc::new(UuidGenerator), staging);

    let snapshot = dispatch.task_progress("t-linked").unwrap();
    assert_eq!(snapshot.status, "failed");
    assert_eq!(snapshot.error_message.as_deref(), Some("mount gone"));
    assert_eq!(snapshot.progress_percent, 0);
    assert!(dispatch.task_progress("nope").is_none());

    assert!(dispatch.reimportable("t-linked"));
    assert!(!dispatch.reimportable("t-bare"));
    assert!(!dispatch.reimportable("t-live"));
    assert!(!dispatch.reimportable("nope"));
}
