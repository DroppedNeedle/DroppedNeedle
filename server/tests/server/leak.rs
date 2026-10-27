//! 5xx-leak brief: every server-fault path answers the fixed generic
//! envelope naming the request id, each closed-list answer its own fixed
//! sentence, each compat path its protocol's failure shape, and no leak
//! marker (path, host, secret) reaches the wire.

use crate::common::TestApp;

use axum::http::StatusCode;
use droppedneedle::handlers::test_hooks::LEAK_MARKERS;
use droppedneedle_kernel::{config::AppConfig, error::Unavailable};
use serde_json::json;

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
    let app = TestApp::start_configured("leak", AppConfig::with_test_hooks).await;
    let headers: Vec<(&str, &str)> = request_id
        .map(|id| ("x-request-id", id))
        .into_iter()
        .collect();
    let (status, headers, body) = app.raw("GET", uri, &headers, None).await;
    (status, headers, String::from_utf8(body).unwrap())
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

    let (status, headers, text) =
        get_full("/__test__/typed-error?status=503", Some("leak-1b")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers.get("retry-after").unwrap(), "30");
    assert_fixed_envelope(&text, "leak-1b");
}

/// The closed list as the plan tables it: code, status, fixed sentence. A
/// new pass-through code is an edit to this table.
const CLOSED_LIST: &[(&str, u16, &str)] = &[
    (
        "SERVER_BUSY",
        503,
        "The server is busy. Try again in a moment.",
    ),
    (
        "REMOTE_UNAVAILABLE",
        502,
        "Your media server did not answer or returned an error.",
    ),
    ("UPSTREAM_REFUSED", 502, "GitHub refused the request"),
    (
        "UPSTREAM_RATE_LIMITED",
        503,
        "GitHub is rate limiting this server, try again later",
    ),
    ("UPSTREAM_ERROR", 502, "GitHub could not be reached"),
    (
        "UPSTREAM_UNREADABLE",
        502,
        "GitHub sent an answer DroppedNeedle could not read.",
    ),
    (
        "PROVIDER_UNAVAILABLE",
        503,
        "An outside service is not answering. Try again later.",
    ),
];

#[tokio::test]
async fn closed_list_answers_pass_the_boundary_with_their_sentence() {
    let variants = [
        Unavailable::ServerBusy,
        Unavailable::MediaServer,
        Unavailable::UpstreamRefused,
        Unavailable::UpstreamRateLimited {
            retry_after: Some(42),
        },
        Unavailable::UpstreamError,
        Unavailable::UpstreamUnreadable,
        Unavailable::Provider {
            provider: "listenbrainz",
            retry_after: Some(30),
        },
    ];
    // Written out by hand, never read from the kit: a new variant fails to
    // compile here, and once listed above it needs its row in the table.
    let retry_after_of = |variant: Unavailable| match variant {
        Unavailable::ServerBusy => Some("1"),
        Unavailable::MediaServer
        | Unavailable::UpstreamRefused
        | Unavailable::UpstreamError
        | Unavailable::UpstreamUnreadable => None,
        Unavailable::UpstreamRateLimited { .. } => Some("42"),
        Unavailable::Provider { .. } => Some("30"),
    };
    let kit: Vec<(&str, u16, &str)> = variants
        .iter()
        .map(|variant| {
            let answer = variant.answer();
            (answer.code, answer.status.as_u16(), answer.sentence)
        })
        .collect();
    assert_eq!(kit, CLOSED_LIST);

    let app = TestApp::start_configured("leak-unavailable", AppConfig::with_test_hooks).await;
    for (variant, &(code, status, sentence)) in variants.into_iter().zip(CLOSED_LIST) {
        let retry_after = retry_after_of(variant);
        let request_id = format!("leak-{code}");
        let (got_status, headers, body) = app
            .raw(
                "GET",
                &format!("/__test__/unavailable/{code}"),
                &[("x-request-id", request_id.as_str())],
                None,
            )
            .await;
        assert_eq!(got_status.as_u16(), status, "{code}");
        assert_eq!(headers.get("x-request-id").unwrap(), request_id.as_str());
        assert_eq!(
            headers
                .get("retry-after")
                .map(|value| value.to_str().unwrap()),
            retry_after,
            "{code}"
        );
        let text = String::from_utf8(body).unwrap();
        let mut details = json!({"error_id": request_id});
        if code == "UPSTREAM_RATE_LIMITED" {
            details["retry_after"] = json!(42);
        }
        if code == "PROVIDER_UNAVAILABLE" {
            details["retry_after"] = json!(30);
            details["provider"] = json!("listenbrainz");
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            json!({"error": {"code": code, "message": sentence, "details": details}}),
        );
        for marker in LEAK_MARKERS {
            assert!(!text.contains(marker), "{code} leaked {marker}");
        }
    }
    app.stop().await;
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
async fn compat_5xx_answers_the_protocol_shape_with_status_and_retry_after() {
    let app = TestApp::start_configured("leak-compat", AppConfig::with_test_hooks).await;
    let cases = [
        ("/subsonic/rest/__test__/raw-503?f=json", "application/json"),
        ("/jellyfin/__test__/raw-503", ""),
    ];
    for (uri, content_type) in cases {
        let (status, headers, body) = app.raw("GET", uri, &[], None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(headers.get("retry-after").unwrap(), "120", "{uri}");
        assert_eq!(
            headers.get("access-control-allow-origin").unwrap(),
            "*",
            "{uri}"
        );
        assert!(headers.get("x-request-id").is_some(), "{uri}");
        let text = String::from_utf8(body).unwrap();
        for marker in LEAK_MARKERS {
            assert!(!text.contains(marker), "{uri} leaked {marker}");
        }
        assert!(!text.contains("INTERNAL_ERROR"), "{uri}: {text}");
        if content_type.is_empty() {
            assert!(text.is_empty(), "{uri}: {text}");
            continue;
        }
        assert!(
            headers
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(content_type),
            "{uri}"
        );
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let reply = &json["subsonic-response"];
        assert_eq!(reply["status"], "failed", "{text}");
        assert_eq!(reply["error"]["code"], 0, "{text}");
        assert_eq!(reply["error"]["message"], "Internal server error", "{text}");
    }
    app.stop().await;
}

#[tokio::test]
async fn minted_request_id_is_used_when_caller_sends_none() {
    let (status, header_id, text) = get("/__test__/typed-error", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(uuid::Uuid::parse_str(&header_id).is_ok(), "{header_id}");
    assert_fixed_envelope(&text, &header_id);
}
