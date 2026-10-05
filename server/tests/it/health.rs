//! `/health` keeps the exact v2 payload, answers JSON, and resolves request
//! ids (minting one, or echoing the caller's).

use crate::common;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::create_app;
use tower::ServiceExt as _;

#[tokio::test]
async fn health_keeps_v2_shape_and_resolves_request_ids() {
    let app = create_app(common::hooked_state());
    let response = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["x-request-id"], common::FIXED_ID);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"status": "ok", "message": "DroppedNeedle backend running"})
    );

    let echoed = app
        .oneshot(
            Request::get("/health")
                .header("x-request-id", "caller-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(echoed.headers()["x-request-id"], "caller-1");
}
