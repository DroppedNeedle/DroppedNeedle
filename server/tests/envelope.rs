//! Envelope-shape brief: unknown routes render the shared error envelope
//! with a SCREAMING_SNAKE code, and the `__test__` failure hooks stay out of
//! production-like apps.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use droppedneedle::create_app;
use tower::ServiceExt as _;

async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value, String) {
    request(app, Method::GET, uri).await
}

async fn request(
    app: axum::Router,
    method: Method,
    uri: &str,
) -> (StatusCode, serde_json::Value, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (status, json, request_id)
}

#[tokio::test]
async fn unknown_route_renders_not_found_envelope() {
    let (status, json, request_id) = get(create_app(common::hooked_state()), "/api/v3/nope").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(request_id, common::FIXED_ID);
    assert_eq!(
        json,
        serde_json::json!({"error": {"code": "NOT_FOUND", "message": "Not found", "details": null}})
    );
}

#[tokio::test]
async fn error_codes_are_screaming_snake() {
    let (_, json, _) = get(create_app(common::hooked_state()), "/missing").await;
    let code = json["error"]["code"].as_str().unwrap();
    assert!(!code.is_empty());
    assert_eq!(code, code.to_uppercase());
    assert!(
        code.chars()
            .all(|char| char.is_ascii_uppercase() || char == '_')
    );
}

#[tokio::test]
async fn wrong_method_renders_method_not_allowed_envelope() {
    let (status, json, request_id) =
        request(create_app(common::hooked_state()), Method::POST, "/health").await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(request_id, common::FIXED_ID);
    assert_eq!(
        json,
        serde_json::json!({"error": {"code": "METHOD_NOT_ALLOWED", "message": "Method not allowed", "details": null}})
    );
}

#[tokio::test]
async fn test_hooks_are_absent_without_opt_in() {
    for uri in [
        "/__test__/typed-error",
        "/__test__/panic",
        "/__test__/raw-500",
        "/__test__/raw-503",
    ] {
        let (status, json, _) = get(create_app(common::prod_like_state()), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(json["error"]["code"], "NOT_FOUND", "{uri}");
    }
}
