//! Compat wiring through the production app: kill switches, mounts
//! outside the `/api` session gate, CORS, case-insensitive paths, the
//! layered rate limits, and the rule that request queries never reach logs.

use crate::common::TestApp;

use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;

async fn oneshot(
    app: &TestApp,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    app.raw_bytes(method, uri, headers, body.to_vec()).await
}

/// The app with both protocols turned on in Settings.
async fn enabled(tag: &str) -> TestApp {
    let app = TestApp::start(tag).await;
    app.set_compat(true, true).await;
    app
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
    let app = TestApp::start("compat-off").await;
    for saved in [false, true] {
        if saved {
            app.set_compat(false, false).await;
        }
        // Subsonic answers code 0 before auth; Jellyfin 404s every route
        // with an empty body, so nothing leaks about existence.
        let (status, _, bytes) = oneshot(&app, "GET", "/subsonic/rest/ping?f=json", &[], b"").await;
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
            let (status, _, body) = oneshot(&app, "GET", uri, headers, b"").await;
            assert_eq!((status, body.len()), (StatusCode::NOT_FOUND, 0), "{uri}");
        }
    }
}

#[tokio::test]
async fn compat_mounts_outside_the_session_gate_with_cors() {
    let on = enabled("compat-mounts").await;
    // Public endpoints answer with no credentials (the `/api` gate would
    // 401), with `*` CORS and credentials off, including uppercase paths.
    for uri in [
        "/subsonic/rest/getOpenSubsonicExtensions",
        "/SUBSONIC/rest/getOpenSubsonicExtensions",
        "/jellyfin/System/Info/Public",
    ] {
        let (status, headers, _) = oneshot(&on, "GET", uri, &[], b"").await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(cors(&headers), Some("*"), "{uri}");
        assert!(headers.get("access-control-allow-credentials").is_none());
    }
    // Authed paths answer in the compat posture, never a session challenge.
    let (_, _, bytes) = oneshot(&on, "GET", "/subsonic/rest/ping?f=json", &[], b"").await;
    assert_eq!(subsonic_code(&bytes), Value::from(10));
    let (status, _, body) = oneshot(&on, "GET", "/jellyfin/Users/x/Views", &[], b"").await;
    assert_eq!((status, body.len()), (StatusCode::UNAUTHORIZED, 0));

    // Preflights short-circuit 204 before auth.
    for uri in ["/jellyfin/Users/x/Views", "/SUBSONIC/rest/ping"] {
        let (status, headers, body) = oneshot(&on, "OPTIONS", uri, &[], b"").await;
        assert_eq!((status, body.len()), (StatusCode::NO_CONTENT, 0), "{uri}");
        assert_eq!(cors(&headers), Some("*"), "{uri}");
    }
    // Unknown compat paths 404 with CORS whether or not the protocol is on;
    // non-compat 404s carry none.
    let (status, headers, _) = oneshot(&on, "GET", "/jellyfin/NoSuchRoute", &[], b"").await;
    assert_eq!((status, cors(&headers)), (StatusCode::NOT_FOUND, Some("*")));
    let (status, headers, _) = oneshot(&on, "GET", "/nope", &[], b"").await;
    assert_eq!((status, cors(&headers)), (StatusCode::NOT_FOUND, None));

    // Feishin posts lowercase Jellyfin paths: the login handler answers
    // instead of a 404, refusing the unknown account with its 401.
    let (status, _, _) = oneshot(
        &on,
        "POST",
        "/jellyfin/users/authenticatebyname",
        &[("content-type", "application/json")],
        br#"{"Username":"alice","Pw":"nope"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Unknown methods on Subsonic answer code 0, never a native 405.
    let (_, _, bytes) = oneshot(&on, "DELETE", "/subsonic/rest/ping?f=json", &[], b"").await;
    assert_eq!(subsonic_code(&bytes), Value::from(0));

    on.set_compat(false, false).await;
    let (status, headers, _) = oneshot(&on, "GET", "/jellyfin/NoSuchRoute", &[], b"").await;
    assert_eq!((status, cors(&headers)), (StatusCode::NOT_FOUND, Some("*")));
}

#[tokio::test]
async fn layered_limits_reject_in_each_protocol_shape() {
    // Subsonic rejects with a code-0 envelope over HTTP 200, Jellyfin with
    // an empty 429; both carry Retry-After and compat CORS.
    for uri in [
        "/subsonic/rest/getOpenSubsonicExtensions?f=json",
        "/jellyfin/System/Info/Public",
    ] {
        let app = enabled("compat-limits").await;
        let mut rejected = false;
        for _ in 0..100 {
            let (status, headers, body) = oneshot(&app, "GET", uri, &[], b"").await;
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
    // Artwork skips the public bucket: a cover grid is dozens of anonymous
    // image requests at once and must not see 429s.
    let app = enabled("compat-artwork").await;
    for _ in 0..60 {
        let (status, _, _) =
            oneshot(&app, "GET", "/jellyfin/Items/abc/Images/Primary", &[], b"").await;
        assert_ne!(status, StatusCode::TOO_MANY_REQUESTS);
    }
}

/// An IPv6 client is keyed by its /64, as the native limiter keys it:
/// another address in the same /64 gets no fresh public bucket and no
/// fresh sign-in backoff, while another /64 is another client. The
/// addresses come through the trusted loopback proxy.
#[tokio::test]
async fn ipv6_clients_share_limits_across_their_64() {
    let app = enabled("compat-ipv6").await;
    let ask = |uri: &'static str, client: &'static str| {
        let app = &app;
        async move {
            let (_, headers, body) =
                oneshot(app, "GET", uri, &[("x-forwarded-for", client)], b"").await;
            (subsonic_code(&body), headers.get("retry-after").is_some())
        }
    };
    let public = "/subsonic/rest/getOpenSubsonicExtensions?f=json";
    let limited = (Value::from(0), true);
    let mut drained = false;
    for _ in 0..100 {
        if ask(public, "2001:db8:1:2::a").await == limited {
            drained = true;
            break;
        }
    }
    assert!(drained, "the public bucket never tripped");
    assert_eq!(ask(public, "2001:db8:1:2::b").await, limited);
    assert_eq!(ask(public, "2001:db8:1:3::a").await, (Value::Null, false));

    let ping = "/subsonic/rest/ping?f=json";
    for _ in 0..5 {
        assert_eq!(ask(ping, "2001:db8:5:6::1").await, (Value::from(10), false));
    }
    assert_eq!(ask(ping, "2001:db8:5:6::2").await, limited);
    assert_eq!(ask(ping, "2001:db8:5:7::1").await, (Value::from(10), false));
}

#[tokio::test]
async fn api_bodies_compress_when_the_client_accepts() {
    let app = enabled("compat-gzip").await;
    let uri = "/subsonic/rest/getOpenSubsonicExtensions?f=json";
    let (_, headers, body) = oneshot(&app, "GET", uri, &[("accept-encoding", "gzip")], b"").await;
    assert_eq!(
        headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    let mut plain = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&body[..]), &mut plain)
        .expect("gzip body");
    assert!(plain.contains("songLyrics"), "{plain}");
    let (_, headers, _) = oneshot(&app, "GET", uri, &[], b"").await;
    assert!(headers.get("content-encoding").is_none());
}

/// Compat clients put app passwords in the query string, so source code may
/// only read a request URI's path, query or host, and never log a URI.
#[test]
fn access_logging_records_path_only() {
    // Every shipping crate of the workspace: each member's `src`, except the
    // testkit's mock servers.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("manifest");
    let manifest: toml::Table = manifest.parse().expect("manifest parses");
    let members = manifest["workspace"]["members"]
        .as_array()
        .expect("workspace members");
    let mut dirs: Vec<_> = members
        .iter()
        .map(|member| member.as_str().expect("member path"))
        .filter(|member| *member != "testkit")
        .map(|member| manifest_dir.join(member).join("src"))
        .collect();
    assert!(dirs.len() > 1, "the workspace lists its crates");
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
                // Match `uri` as an identifier so words like "during" pass.
                let names_uri = line
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .any(|word| {
                        word == "uri" || word.ends_with("_uri") || word.starts_with("uri_")
                    });
                let logged = line.contains("tracing::") && names_uri;
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
