//! Compat wiring through the production `create_app`: kill switches, mounts
//! outside the `/api` session gate, CORS, case-insensitive paths, the
//! layered rate limits, and the rule that request queries never reach logs.

use crate::common::{hooked_state, hooked_state_with_compat};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle::create_app;
use serde_json::Value;
use tower::ServiceExt as _;

async fn oneshot(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Body::from(body.to_vec()))
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads")
        .to_vec();
    (status, headers, bytes)
}

fn enabled() -> Router {
    create_app(hooked_state_with_compat(true, true))
}

fn subsonic_code(bytes: &[u8]) -> Value {
    let body: Value = serde_json::from_slice(bytes).expect("json");
    body["subsonic-response"]["error"]["code"].clone()
}

fn cors(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn kill_switches_default_off() {
    for app in [
        create_app(hooked_state()),
        create_app(hooked_state_with_compat(false, false)),
    ] {
        // Subsonic answers code 0 before auth; Jellyfin 404s every route
        // with an empty body, so nothing leaks about existence.
        let (status, _, bytes) =
            oneshot(app.clone(), "GET", "/subsonic/rest/ping?f=json", &[], b"").await;
        assert_eq!(
            (status, subsonic_code(&bytes)),
            (StatusCode::OK, Value::from(0))
        );
        for (uri, headers) in [
            ("/jellyfin/System/Info/Public", &[][..]),
            (
                "/jellyfin/Users/x/Views",
                &[("X-Emby-Token", "whatever")][..],
            ),
        ] {
            let (status, _, body) = oneshot(app.clone(), "GET", uri, headers, b"").await;
            assert_eq!((status, body.len()), (StatusCode::NOT_FOUND, 0), "{uri}");
        }
    }
}

#[tokio::test]
async fn compat_mounts_outside_the_session_gate_with_cors() {
    // Public endpoints answer with no credentials (the `/api` gate would
    // 401), with `*` CORS and credentials off, including uppercase paths.
    for uri in [
        "/subsonic/rest/getOpenSubsonicExtensions",
        "/SUBSONIC/rest/getOpenSubsonicExtensions",
        "/jellyfin/System/Info/Public",
    ] {
        let (status, headers, _) = oneshot(enabled(), "GET", uri, &[], b"").await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(cors(&headers), Some("*"), "{uri}");
        assert!(headers.get("access-control-allow-credentials").is_none());
    }
    // Authed paths answer in the compat posture, never a session challenge.
    let (_, _, bytes) = oneshot(enabled(), "GET", "/subsonic/rest/ping?f=json", &[], b"").await;
    assert_eq!(subsonic_code(&bytes), Value::from(10));
    let (status, _, body) = oneshot(enabled(), "GET", "/jellyfin/Users/x/Views", &[], b"").await;
    assert_eq!((status, body.len()), (StatusCode::UNAUTHORIZED, 0));

    // Preflights short-circuit 204 before auth.
    for uri in ["/jellyfin/Users/x/Views", "/SUBSONIC/rest/ping"] {
        let (status, headers, body) = oneshot(enabled(), "OPTIONS", uri, &[], b"").await;
        assert_eq!((status, body.len()), (StatusCode::NO_CONTENT, 0), "{uri}");
        assert_eq!(cors(&headers), Some("*"), "{uri}");
    }
    // Unknown compat paths 404 with CORS whether or not the protocol is on;
    // non-compat 404s carry none.
    for app in [
        enabled(),
        create_app(hooked_state_with_compat(false, false)),
    ] {
        let (status, headers, _) = oneshot(app, "GET", "/jellyfin/NoSuchRoute", &[], b"").await;
        assert_eq!((status, cors(&headers)), (StatusCode::NOT_FOUND, Some("*")));
    }
    let (status, headers, _) = oneshot(enabled(), "GET", "/nope", &[], b"").await;
    assert_eq!((status, cors(&headers)), (StatusCode::NOT_FOUND, None));

    // Feishin posts lowercase Jellyfin paths: a bad login is a 401, not 404.
    let (status, _, _) = oneshot(
        enabled(),
        "POST",
        "/jellyfin/users/authenticatebyname",
        &[("content-type", "application/json")],
        br#"{"Username":"alice","Pw":"nope"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Unknown methods on Subsonic answer code 0, never a native 405.
    let (_, _, bytes) = oneshot(enabled(), "DELETE", "/subsonic/rest/ping?f=json", &[], b"").await;
    assert_eq!(subsonic_code(&bytes), Value::from(0));
}

#[tokio::test]
async fn layered_limits_reject_in_each_protocol_shape() {
    // Subsonic rejects with a code-0 envelope over HTTP 200, Jellyfin with
    // an empty 429; both carry Retry-After and compat CORS.
    for uri in [
        "/subsonic/rest/getOpenSubsonicExtensions?f=json",
        "/jellyfin/System/Info/Public",
    ] {
        let app = enabled();
        let mut rejected = false;
        for _ in 0..100 {
            let (status, headers, body) = oneshot(app.clone(), "GET", uri, &[], b"").await;
            let is_reject = if uri.starts_with("/subsonic") {
                assert_eq!(status, StatusCode::OK);
                !subsonic_code(&body).is_null()
            } else {
                status == StatusCode::TOO_MANY_REQUESTS
            };
            if is_reject {
                if uri.starts_with("/subsonic") {
                    assert_eq!(subsonic_code(&body), Value::from(0));
                } else {
                    assert!(body.is_empty());
                }
                assert!(headers.get("retry-after").is_some(), "{uri}");
                assert_eq!(cors(&headers), Some("*"), "{uri}");
                rejected = true;
                break;
            }
        }
        assert!(rejected, "{uri}: the public bucket never tripped");
    }
}

/// Compat clients put app passwords in the query string, so source code may
/// only read a request URI's path, query or host, and never log a URI.
#[test]
fn access_logging_records_path_only() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut dirs = vec![root];
    let mut violations = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("src lists") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("source reads");
            for (number, line) in source.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let full_target =
                    line.contains("uri().path_and_query") || line.contains("uri().to_string()");
                let logged = line.contains("tracing::") && line.contains("uri");
                if full_target || logged {
                    violations.push(format!("{}:{}: {line}", path.display(), number + 1));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "full-target URI reads:\n{}",
        violations.join("\n")
    );
}
