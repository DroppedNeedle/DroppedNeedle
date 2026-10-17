//! Envelope-shape brief: unknown routes render the shared error envelope
//! with a SCREAMING_SNAKE code, and the `__test__` failure hooks stay out of
//! production-like apps.

use crate::common::TestApp;

use axum::http::StatusCode;

/// Status, JSON body and the request id the app answered with.
async fn request(
    app: &TestApp,
    method: &str,
    uri: &str,
) -> (StatusCode, serde_json::Value, String) {
    let (status, headers, body) = app.raw(method, uri, &[], None).await;
    let request_id = headers
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (status, json, request_id)
}

/// The id the app minted, which is a UUID from its generator.
fn assert_minted(request_id: &str) {
    assert!(uuid::Uuid::parse_str(request_id).is_ok(), "{request_id}");
}

#[tokio::test]
async fn unknown_route_renders_not_found_envelope() {
    let app = TestApp::start("envelope-404").await;
    let (status, json, request_id) = request(&app, "GET", "/api/v3/nope").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_minted(&request_id);
    assert_eq!(
        json,
        serde_json::json!({"error": {"code": "NOT_FOUND", "message": "Not found", "details": null}})
    );
}

#[tokio::test]
async fn error_codes_are_screaming_snake() {
    let app = TestApp::start("envelope-code").await;
    let (_, json, _) = request(&app, "GET", "/missing").await;
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
    let app = TestApp::start("envelope-405").await;
    let (status, json, request_id) = request(&app, "POST", "/health").await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_minted(&request_id);
    assert_eq!(
        json,
        serde_json::json!({"error": {"code": "METHOD_NOT_ALLOWED", "message": "Method not allowed", "details": null}})
    );
}

#[tokio::test]
async fn test_hooks_are_absent_without_opt_in() {
    let app = TestApp::start("envelope-no-hooks").await;
    for uri in [
        "/__test__/typed-error",
        "/__test__/panic",
        "/__test__/raw-500",
        "/__test__/raw-503",
        "/__test__/unavailable/SERVER_BUSY",
        "/subsonic/rest/__test__/raw-503",
        "/jellyfin/__test__/raw-503",
    ] {
        let (status, json, _) = request(&app, "GET", uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(json["error"]["code"], "NOT_FOUND", "{uri}");
    }
}
