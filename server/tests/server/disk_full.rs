//! A server fault on a full disk answers 507 `DISK_FULL`, decided from the
//! free space the boundary reads on the data and temp folders, never from
//! the error's text. With room left the same fault is the fixed 500.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
    routing::get,
};
use droppedneedle::middleware::{Boundary, request_scope};
use droppedneedle_kernel::{error::ApiError, ids::REQUEST_ID_HEADER};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::TestApp;

const MB: u64 = 1024 * 1024;

/// The 503 a busy outside service might answer, with its own wait.
async fn waiting_fault() -> ApiError {
    ApiError::fault(StatusCode::SERVICE_UNAVAILABLE, "write failed")
        .with_header(header::RETRY_AFTER, HeaderValue::from_static("30"))
}

#[allow(clippy::panic)]
async fn panicking() -> ApiError {
    panic!("write failed: No space left on device");
}

async fn failing(app: &TestApp, free: u64, route: &str) -> (StatusCode, String, bool, Value) {
    let boundary = Boundary::new(
        app.state().ids.clone(),
        Arc::new(move || Some(free)),
        |_, _, _, status, _| axum::response::IntoResponse::into_response(status),
    );
    let router = Router::new()
        .route(
            "/api/v3/fails",
            get(|| async { ApiError::internal("write failed: No space left on device") }),
        )
        .route("/api/v3/waits", get(waiting_fault))
        .route("/api/v3/panics", get(panicking))
        .layer(axum::middleware::from_fn_with_state(
            boundary,
            request_scope,
        ));
    let response = router
        .oneshot(Request::get(route).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let request_id = response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    let retry_after = response.headers().contains_key(header::RETRY_AFTER);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        request_id,
        retry_after,
        serde_json::from_slice(&body).unwrap(),
    )
}

fn disk_full(request_id: &str) -> Value {
    json!({"error": {
        "code": "DISK_FULL",
        "message": "The server's disk is full. An admin needs to free some space.",
        "details": {"error_id": request_id},
    }})
}

#[tokio::test]
async fn a_fault_on_a_full_disk_answers_disk_full_and_otherwise_the_fixed_500() {
    let app = TestApp::start("disk-full").await;

    for route in ["/api/v3/fails", "/api/v3/waits", "/api/v3/panics"] {
        let (status, request_id, retry_after, body) = failing(&app, 10 * MB, route).await;
        assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{route}");
        assert_eq!(body, disk_full(&request_id), "{route}");
        assert!(!retry_after, "{route} kept its Retry-After");
    }

    let (status, request_id, _, body) = failing(&app, 10 * 1024 * MB, "/api/v3/fails").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    assert_eq!(body["error"]["details"]["error_id"], request_id);

    // The app's own boundary reads the real disk.
    let boundary = Boundary::from_state(app.state());
    let free = tokio::task::spawn_blocking(move || boundary.free_space_now())
        .await
        .unwrap();
    assert!(free.is_some(), "the app's boundary has no free-space probe");

    app.stop().await;
}
