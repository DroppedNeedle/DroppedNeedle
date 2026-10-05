//! Health-shape brief: `/health` keeps the exact v2 payload, answers JSON,
//! and resolves request ids (echoing the caller's, minting otherwise).

use crate::common;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::create_app;
use tower::ServiceExt as _;

#[tokio::test]
async fn health_keeps_v2_shape_and_mints_request_id() {
    let app = create_app(common::hooked_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
    assert_eq!(
        response.headers().get("x-request-id").unwrap(),
        common::FIXED_ID
    );

    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"status": "ok", "message": "DroppedNeedle backend running"})
    );
}

#[tokio::test]
async fn health_echoes_caller_request_id() {
    let app = create_app(common::hooked_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("x-request-id", "caller-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-request-id").unwrap(), "caller-1");
}
