//! Compat wiring: mounts, kill switches, and the edge through `create_app`.
//!
//! The protocol suites pin dispatch behavior against fixture seams and the
//! journeys run lifecycles through layered fixture routers; these tests pin
//! the production assembly instead: both routers mount outside the `/api`
//! session gate with the shared layers, kill switches default OFF, and the
//! compat auth posture (never the session gate) answers compat paths.

use crate::common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{hooked_state, hooked_state_with_compat};
use droppedneedle::create_app;
use tower::ServiceExt as _;

async fn call(
    subsonic: bool,
    jellyfin: bool,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let app = create_app(hooked_state_with_compat(subsonic, jellyfin));
    oneshot(app, method, uri, headers, body).await
}

async fn oneshot(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(bytes) => Body::from(bytes),
        None => Body::empty(),
    };
    let request = builder.body(body).expect("request builds");
    let response = app.oneshot(request).await.expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads")
        .to_vec();
    (status, headers, bytes)
}

fn header(headers: &axum::http::HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

#[tokio::test]
async fn kill_switches_default_off() {
    // Subsonic disabled: failed envelope code 0, before auth.
    let (status, _, bytes) =
        call(false, false, "GET", "/subsonic/rest/ping?f=json", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["subsonic-response"]["status"], "failed");
    assert_eq!(body["subsonic-response"]["error"]["code"], 0);

    // Jellyfin disabled: 404 on every route, before handler lookup,
    // with empty bodies (no existence or reason leaks).
    let (status, _, body) = call(
        false,
        false,
        "GET",
        "/jellyfin/System/Info/Public",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
    let (status, _, body) = call(
        false,
        false,
        "GET",
        "/jellyfin/Users/x/Views",
        &[("X-Emby-Token", "whatever")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
}

#[tokio::test]
async fn for_tests_bundle_defaults_both_protocols_off() {
    // `CompatSetup::for_tests` ships kill switches OFF with no
    // `with_enabled` call: both protocols refuse exactly like an
    // explicit (false, false).
    let app = create_app(hooked_state());
    let (status, _, bytes) =
        oneshot(app.clone(), "GET", "/subsonic/rest/ping?f=json", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["subsonic-response"]["status"], "failed");
    assert_eq!(body["subsonic-response"]["error"]["code"], 0);
    let (status, _, body) = oneshot(app, "GET", "/jellyfin/System/Info/Public", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
}

#[tokio::test]
async fn compat_mounts_outside_the_session_gate() {
    // Enabled public endpoints answer with no credentials at all: the
    // `/api` session gate would 401 these.
    let (status, _, _) = call(
        true,
        true,
        "GET",
        "/subsonic/rest/getOpenSubsonicExtensions",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(true, true, "GET", "/jellyfin/System/Info/Public", &[], None).await;
    assert_eq!(status, StatusCode::OK);

    // Authed compat paths answer with compat auth posture, never a
    // session challenge: Subsonic envelopes code 10, Jellyfin 401s empty.
    let (status, _, bytes) = call(true, true, "GET", "/subsonic/rest/ping?f=json", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["subsonic-response"]["error"]["code"], 10);
    let (status, _, body) = call(true, true, "GET", "/jellyfin/Users/x/Views", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.is_empty());
}

#[tokio::test]
async fn compat_cors_and_preflight() {
    // Every compat response carries `*` with creds off.
    let (_, headers, _) = call(true, true, "GET", "/jellyfin/System/Info/Public", &[], None).await;
    assert_eq!(header(&headers, "access-control-allow-origin"), "*");
    assert!(headers.get("access-control-allow-credentials").is_none());
    let (_, headers, _) = call(
        true,
        true,
        "GET",
        "/subsonic/rest/getOpenSubsonicExtensions",
        &[],
        None,
    )
    .await;
    assert_eq!(header(&headers, "access-control-allow-origin"), "*");

    // Preflights short-circuit 204 before auth.
    let (status, headers, body) =
        call(true, true, "OPTIONS", "/jellyfin/Users/x/Views", &[], None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    assert_eq!(header(&headers, "access-control-allow-origin"), "*");
}

#[tokio::test]
async fn compat_redispatch_is_case_insensitive() {
    // `/SUBSONIC/...` redispatches exactly like `/subsonic/...`: the
    // public endpoint answers 200 with compat CORS, not the native 404.
    let (status, headers, _) = call(
        true,
        true,
        "GET",
        "/SUBSONIC/rest/getOpenSubsonicExtensions",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "access-control-allow-origin"), "*");

    // Uppercase preflights short-circuit 204 pre-auth too.
    let (status, headers, body) =
        call(true, true, "OPTIONS", "/SUBSONIC/rest/ping", &[], None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    assert_eq!(header(&headers, "access-control-allow-origin"), "*");
}

#[tokio::test]
async fn compat_native_fallbacks_carry_cors() {
    // Unknown compat paths 404 natively but still stamp compat CORS, so
    // callers cannot distinguish "disabled protocol" from "no such route"
    // by the presence of `*`.
    for (subsonic, jellyfin) in [(true, true), (false, false)] {
        let (status, headers, _) = call(
            subsonic,
            jellyfin,
            "GET",
            "/jellyfin/NoSuchRoute",
            &[],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            header(&headers, "access-control-allow-origin"),
            "*",
            "subsonic={subsonic} jellyfin={jellyfin}"
        );
    }
    // Non-compat 404s stay CORS-free.
    let (status, headers, _) = call(true, true, "GET", "/nope", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(headers.get("access-control-allow-origin").is_none());
}

#[test]
fn access_logging_records_path_only() {
    // T-M6 posture tripwire: compat puts app-password secrets in the query
    // string, so queries must never reach logs. Request URIs may only be
    // read via `.path()` (spans, routing), `.query()` (param parsing), or
    // `.host()` (origin checks); any full-target read or URI-bearing log
    // line fails this test.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut dirs = vec![root];
    let mut violations = Vec::new();
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(&dir).expect("src lists");
        for entry in entries {
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
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                if line.contains("uri().path_and_query") || line.contains("uri().to_string()") {
                    violations.push(format!("{}:{}: {line}", path.display(), number + 1));
                }
                if line.contains("tracing::") && line.contains("uri") {
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

#[tokio::test]
async fn compat_layered_limit_smoke_per_protocol() {
    // t-m8: one layered smoke per protocol through `create_app` — the
    // public bucket trips in the shared layers, and the reject carries
    // compat CORS plus the protocol's 429 shape. Public endpoints only,
    // so no auth denial pollutes the buckets (pure token-bucket trip).
    let app = create_app(hooked_state_with_compat(true, true));
    let mut saw_ok = false;
    let mut saw_reject = false;
    for _ in 0..100 {
        let (status, headers, bytes) = oneshot(
            app.clone(),
            "GET",
            "/subsonic/rest/getOpenSubsonicExtensions?f=json",
            &[],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        if body["subsonic-response"]["status"] == "ok" {
            saw_ok = true;
        } else {
            assert_eq!(body["subsonic-response"]["error"]["code"], 0);
            assert!(
                headers.get("retry-after").is_some(),
                "subsonic reject carries Retry-After"
            );
            assert_eq!(
                header(&headers, "access-control-allow-origin"),
                "*",
                "subsonic reject carries compat CORS"
            );
            saw_reject = true;
        }
        if saw_ok && saw_reject {
            break;
        }
    }
    assert!(saw_ok && saw_reject, "subsonic bucket tripped");

    let app = create_app(hooked_state_with_compat(true, true));
    let mut saw_ok = false;
    let mut saw_reject = false;
    for _ in 0..100 {
        let (status, headers, body) = oneshot(
            app.clone(),
            "GET",
            "/jellyfin/System/Info/Public",
            &[],
            None,
        )
        .await;
        if status == StatusCode::OK {
            saw_ok = true;
        } else {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert!(body.is_empty(), "jellyfin reject is empty");
            assert!(
                headers.get("retry-after").is_some(),
                "jellyfin reject carries Retry-After"
            );
            assert_eq!(
                header(&headers, "access-control-allow-origin"),
                "*",
                "jellyfin reject carries compat CORS"
            );
            saw_reject = true;
        }
        if saw_ok && saw_reject {
            break;
        }
    }
    assert!(saw_ok && saw_reject, "jellyfin bucket tripped");
}

#[tokio::test]
async fn compat_paths_are_case_insensitive() {
    // Feishin posts lowercase Jellyfin paths: the edge canonicalizes
    // before routing, so a bad-credential login 401s instead of 404ing.
    let (status, _, _) = call(
        true,
        true,
        "POST",
        "/jellyfin/users/authenticatebyname",
        &[("content-type", "application/json")],
        Some(br#"{"Username":"alice","Pw":"nope"}"#.to_vec()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Unknown methods on Subsonic answer code 0, never the native 405.
    let (status, _, bytes) = call(
        true,
        true,
        "DELETE",
        "/subsonic/rest/ping?f=json",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["subsonic-response"]["error"]["code"], 0);
}
