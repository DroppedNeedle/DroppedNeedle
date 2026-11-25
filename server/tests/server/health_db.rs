//! `/health` reads the database. A write that holds the writer lane past
//! its `stuck_after` fails the check with the busy envelope, its request id
//! and nothing internal; once the write lets go, the check passes again with
//! the usual body.

use std::{sync::Arc, time::Duration};

use axum::{Router, body::Body, http::Request, http::StatusCode, routing::get};
use droppedneedle::handlers::{self, HealthState};
use droppedneedle_kernel::{
    clock::SystemClock,
    db::{DbConfig, DbError, Lane, LaneLimits, open_runtime},
    ids::REQUEST_ID_HEADER,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::{ScratchDir, TestApp, migrated_db};

const HOLD: &str = "health-db-held-write";

async fn check(app: &Router) -> (StatusCode, String, Value) {
    let response = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let request_id = response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, request_id, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn a_stuck_write_fails_the_health_check_until_it_lets_go() {
    let dir = ScratchDir::new("health-db");
    let path = dir.join("app.db");
    migrated_db(&path);
    let runtime = open_runtime(
        &DbConfig::new(&path, Arc::new(SystemClock)).with_lane_limits(LaneLimits {
            stuck_after: Duration::from_millis(300),
            ..LaneLimits::patient()
        }),
    )
    .await
    .unwrap();
    let scope = TestApp::start("health-db-scope").await;
    let app = scope.request_scope(
        Router::new()
            .route("/health", get(handlers::health))
            .with_state(HealthState::Serving(runtime.db().clone())),
    );
    let healthy = json!({"status": "ok", "message": "DroppedNeedle backend running"});
    let (status, _, body) = check(&app).await;
    assert_eq!((status, &body), (StatusCode::OK, &healthy));

    // A write that holds its turn without running SQL: no budget aborts it,
    // as a commit on a disk that stopped answering is not aborted.
    let (inside, entered) = tokio::sync::oneshot::channel();
    let (let_go, held) = std::sync::mpsc::channel::<()>();
    let db = runtime.db().clone();
    let holder = std::thread::spawn(move || {
        db.write_blocking(Lane::Background, HOLD, move |_| {
            let _ = inside.send(());
            let _ = held.recv();
            Ok::<_, DbError>(())
        })
    });
    entered.await.unwrap();
    let (request_id, body) = scope
        .until("the held write fails the health check", || async {
            let (status, request_id, body) = check(&app).await;
            (status == StatusCode::SERVICE_UNAVAILABLE).then_some((request_id, body))
        })
        .await;
    assert_eq!(
        body,
        json!({
            "error": {
                "code": "SERVER_BUSY",
                "message": "The server is busy. Try again in a moment.",
                "details": {"error_id": request_id},
            }
        })
    );
    assert!(!body.to_string().contains(HOLD), "{body}");

    let_go.send(()).unwrap();
    holder.join().unwrap().unwrap();
    let (status, _, body) = check(&app).await;
    assert_eq!((status, &body), (StatusCode::OK, &healthy));

    drop(app);
    scope.stop().await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_busy_read_pool_still_answers_healthy_quickly() {
    let dir = ScratchDir::new("health-db-busy");
    let path = dir.join("app.db");
    migrated_db(&path);
    let mut config =
        DbConfig::new(&path, Arc::new(SystemClock)).with_lane_limits(LaneLimits::patient());
    config.readers = 1;
    let runtime = open_runtime(&config).await.unwrap();
    let scope = TestApp::start("health-db-busy-scope").await;
    let app = scope.request_scope(
        Router::new()
            .route("/health", get(handlers::health))
            .with_state(HealthState::Serving(runtime.db().clone())),
    );

    // The one request connection is held; a plain request read would wait
    // 5 s for it, as long as the container's health check waits.
    let (inside, entered) = tokio::sync::oneshot::channel();
    let (let_go, held) = std::sync::mpsc::channel::<()>();
    let db = runtime.db().clone();
    let reader = tokio::spawn(async move {
        db.read("health-db-held-read", move |_| {
            let _ = inside.send(());
            let _ = held.recv();
            Ok::<_, DbError>(())
        })
        .await
    });
    entered.await.unwrap();

    let started = std::time::Instant::now();
    let (status, _, _) = check(&app).await;
    let took = started.elapsed();
    assert_eq!(status, StatusCode::OK);
    assert!(took < Duration::from_secs(3), "health took {took:?}");

    let_go.send(()).unwrap();
    reader.await.unwrap().unwrap();
    drop(app);
    scope.stop().await;
    runtime.shutdown().await;
}
