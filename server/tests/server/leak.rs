//! 5xx-leak brief: every server-fault path answers the fixed generic
//! envelope naming the request id, each closed-list answer its own fixed
//! sentence, each compat path its protocol's failure shape, and no leak
//! marker (path, host, secret) reaches the wire.

use crate::common::TestApp;

use axum::http::StatusCode;
use droppedneedle::handlers::test_hooks::LEAK_MARKERS;
use droppedneedle_kernel::{config::AppConfig, error::Unavailable};
use serde_json::json;

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

/// One app with the hooks: a typed fault, a raw 500 or 503 and a panic each
/// leave as the fixed envelope naming the request id (the caller's, or one
/// minted when it sent none), a 503 keeping its status and `Retry-After`.
#[tokio::test]
async fn every_native_5xx_leaves_as_the_fixed_envelope() {
    let app = TestApp::start_configured("leak", AppConfig::with_test_hooks).await;
    let rows: [(&str, Option<&str>, StatusCode, Option<&str>); 6] = [
        (
            "/__test__/typed-error",
            Some("leak-1"),
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
        ),
        (
            "/__test__/typed-error?status=503",
            Some("leak-1b"),
            StatusCode::SERVICE_UNAVAILABLE,
            Some("30"),
        ),
        (
            "/__test__/raw-500",
            Some("leak-3"),
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
        ),
        (
            "/__test__/raw-503",
            Some("leak-503"),
            StatusCode::SERVICE_UNAVAILABLE,
            Some("120"),
        ),
        (
            "/__test__/typed-error",
            None,
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
        ),
        (
            "/__test__/panic",
            Some("leak-2"),
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
        ),
    ];
    for (uri, sent_id, status, retry_after) in rows {
        let sent: Vec<(&str, &str)> = sent_id.map(|id| ("x-request-id", id)).into_iter().collect();
        let (got_status, headers, body) = app.raw("GET", uri, &sent, None).await;
        let text = String::from_utf8(body).unwrap();
        assert_eq!(got_status, status, "{uri} {sent_id:?}");
        let header_id = headers
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        match sent_id {
            Some(id) => assert_eq!(header_id, id, "{uri}"),
            None => assert!(
                uuid::Uuid::parse_str(&header_id).is_ok(),
                "{uri} minted {header_id}"
            ),
        }
        if let Some(wait) = retry_after {
            assert_eq!(headers.get("retry-after").unwrap(), wait, "{uri}");
        }
        assert_fixed_envelope(&text, &header_id);
    }
    app.stop().await;
}

/// One closed-list answer as the boundary must write it: code, status,
/// fixed sentence and the `Retry-After` the test variant carries.
type Golden = (&'static str, u16, &'static str, Option<&'static str>);

/// The closed list, written out by hand and never read from the kit. The
/// match has no wildcard, so a new variant fails to compile until its row
/// is written here.
fn golden(variant: &Unavailable) -> Golden {
    match variant {
        Unavailable::ServerBusy => (
            "SERVER_BUSY",
            503,
            "The server is busy. Try again in a moment.",
            Some("1"),
        ),
        Unavailable::MediaServer => (
            "REMOTE_UNAVAILABLE",
            502,
            "Your media server did not answer or returned an error.",
            None,
        ),
        Unavailable::UpstreamRefused => {
            ("UPSTREAM_REFUSED", 502, "GitHub refused the request", None)
        }
        Unavailable::UpstreamRateLimited { .. } => (
            "UPSTREAM_RATE_LIMITED",
            503,
            "GitHub is rate limiting this server, try again later",
            Some("42"),
        ),
        Unavailable::UpstreamError => ("UPSTREAM_ERROR", 502, "GitHub could not be reached", None),
        Unavailable::UpstreamUnreadable => (
            "UPSTREAM_UNREADABLE",
            502,
            "GitHub sent an answer DroppedNeedle could not read.",
            None,
        ),
        Unavailable::Provider { .. } => (
            "PROVIDER_UNAVAILABLE",
            503,
            "An outside service is not answering. Try again later.",
            Some("30"),
        ),
        Unavailable::UpgradeRequired => (
            "UPGRADE_REQUIRED",
            503,
            "DroppedNeedle needs its upgrade from v2. Open the web page to run it.",
            None,
        ),
    }
}

#[tokio::test]
async fn closed_list_answers_pass_the_boundary_with_their_sentence() {
    // One of each variant, beside the match above that gives its row.
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
        Unavailable::UpgradeRequired,
    ];
    for variant in &variants {
        let answer = variant.answer();
        let (code, status, sentence, _) = golden(variant);
        assert_eq!(
            (answer.code, answer.status.as_u16(), answer.sentence),
            (code, status, sentence)
        );
    }

    let app = TestApp::start_configured("leak-unavailable", AppConfig::with_test_hooks).await;
    for variant in variants {
        let (code, status, sentence, retry_after) = golden(&variant);
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
