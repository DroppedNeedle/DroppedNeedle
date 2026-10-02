//! 5xx-leak brief: every server-fault path answers the fixed generic
//! envelope naming the request id, and no leak marker (path, host, secret)
//! reaches the wire.

mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get as route_get,
};
use droppedneedle::{create_app, handlers::test_hooks::LEAK_MARKERS, middleware::request_scope};
use tower::ServiceExt as _;

async fn get(uri: &str, request_id: Option<&str>) -> (StatusCode, String, String) {
    let (status, headers, text) = get_full(uri, request_id).await;
    let header_id = headers
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    (status, header_id, text)
}

async fn get_full(
    uri: &str,
    request_id: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut builder = Request::builder().uri(uri);
    if let Some(id) = request_id {
        builder = builder.header("x-request-id", id);
    }
    let response = create_app(common::hooked_state())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    (status, headers, text)
}

fn assert_fixed_envelope(text: &str, expected_id: &str) {
    let json: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "error": {
                "code": "INTERNAL_ERROR",
                "message": "Internal server error",
                "details": {"error_id": expected_id},
            }
        })
    );
    for marker in LEAK_MARKERS {
        assert!(!text.contains(marker), "leaked {marker}");
    }
}

#[tokio::test]
async fn typed_server_fault_stays_generic_and_names_request_id() {
    let (status, header_id, text) = get("/__test__/typed-error", Some("leak-1")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header_id, "leak-1");
    assert_fixed_envelope(&text, "leak-1");
}

#[tokio::test]
async fn panicking_handler_still_answers_the_envelope() {
    let (status, header_id, text) = get("/__test__/panic", Some("leak-2")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header_id, "leak-2");
    assert_fixed_envelope(&text, "leak-2");
}

#[tokio::test]
async fn raw_500_is_rewritten_to_the_envelope() {
    let (status, header_id, text) = get("/__test__/raw-500", Some("leak-3")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header_id, "leak-3");
    assert_fixed_envelope(&text, "leak-3");
}

#[tokio::test]
async fn raw_503_keeps_status_and_retry_after_with_generic_body() {
    let (status, headers, text) = get_full("/__test__/raw-503", Some("leak-503")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers.get("x-request-id").unwrap(), "leak-503");
    assert_eq!(headers.get("retry-after").unwrap(), "120");
    assert_fixed_envelope(&text, "leak-503");
}

#[tokio::test]
async fn compat_5xx_keeps_protocol_body_status_and_retry_after() {
    // m4: `/subsonic` + `/jellyfin` 5xx responses are exempt from the
    // native-envelope rewrite (their shapes are protocol-pinned); native
    // paths still rewrite. Status and `Retry-After` survive on both.
    async fn raw_503() -> impl IntoResponse {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, "7")],
            "protocol-shaped failure",
        )
    }
    let app = Router::new()
        .route("/subsonic/rest/__test500", route_get(raw_503))
        .route("/jellyfin/__test500", route_get(raw_503))
        .route("/native/__test500", route_get(raw_503))
        .layer(middleware::from_fn_with_state(
            common::hooked_state(),
            request_scope,
        ));
    for uri in ["/subsonic/rest/__test500", "/jellyfin/__test500"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(response.headers().get("retry-after").unwrap(), "7", "{uri}");
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), b"protocol-shaped failure", "{uri}");
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/native/__test500")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers().get("retry-after").unwrap(), "7");
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_fixed_envelope(core::str::from_utf8(&body).unwrap(), common::FIXED_ID);
}

#[tokio::test]
async fn minted_request_id_is_used_when_caller_sends_none() {
    let (status, header_id, text) = get("/__test__/typed-error", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header_id, common::FIXED_ID);
    assert_fixed_envelope(&text, common::FIXED_ID);
}
