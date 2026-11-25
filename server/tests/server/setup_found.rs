//! The setup guide's look at the services on the network: each candidate
//! is asked once, keyless, and named only from what answered at its own
//! fixed address; a hostile answer reads as something else or no answer,
//! within the look's time, and is never followed.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use droppedneedle::setup::SetupCandidates;
use droppedneedle_testkit::slskd::MockSlskd;
use droppedneedle_testkit::usenet::{ServerGuard, serve_loopback};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::common::{TestApp, closed_outbound};

/// Every request a scripted server took: its path and query, and its
/// header names.
type Seen = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// Serve `router` on loopback, recording each request, and answer its
/// base as `http://localhost:<port>` so the probe's resolver runs.
async fn scripted(router: Router, seen: &Seen) -> (String, ServerGuard) {
    let log = seen.clone();
    let router = router.layer(middleware::from_fn(move |request: Request, next: Next| {
        let log = log.clone();
        async move {
            let target = request
                .uri()
                .path_and_query()
                .map(ToString::to_string)
                .unwrap_or_default();
            let names = request
                .headers()
                .keys()
                .map(|name| name.as_str().to_owned())
                .collect();
            log.lock().unwrap().push((target, names));
            next.run(request).await
        }
    }));
    let (base, guard) = serve_loopback(router).await.unwrap();
    (base.replace("127.0.0.1", "localhost"), guard)
}

async fn found(app: &TestApp) -> Value {
    let admin = app.login_admin().await;
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/setup/found",
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn service<'a>(body: &'a Value, kind: &str) -> &'a Value {
    body["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|service| service["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} in {body}"))
}

#[tokio::test]
async fn the_look_names_each_service_from_its_own_keyless_answer() {
    let seen: Seen = Arc::default();
    let slskd = MockSlskd::start().await.unwrap();
    let (sabnzbd, _sabnzbd) = scripted(
        Router::new().route(
            "/api",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    "Access denied - Hostname verification failed: \
                     https://sabnzbd.org/hostname-check",
                )
            }),
        ),
        &seen,
    )
    .await;
    let (jellyfin, _jellyfin) = scripted(
        Router::new().route(
            "/System/Info/Public",
            get(|| async {
                axum::Json(json!({
                    "ServerName": "den",
                    "Version": "10.9.11",
                    "ProductName": "Jellyfin Server",
                    "Id": "5a1b2c",
                    "StartupWizardCompleted": true,
                }))
            }),
        ),
        &seen,
    )
    .await;
    let (navidrome, _navidrome) = scripted(
        Router::new().route(
            "/rest/ping",
            get(|| async {
                axum::Json(json!({"subsonic-response": {
                    "status": "failed",
                    "version": "1.16.1",
                    "type": "navidrome",
                    "serverVersion": "0.53.3 (13af8ed4)",
                    "openSubsonic": true,
                    "error": {"code": 10, "message": "Required parameter is missing: u"},
                }}))
            }),
        ),
        &seen,
    )
    .await;
    let mut outbound = closed_outbound();
    outbound.setup_candidates = SetupCandidates {
        slskd: slskd.base_url().replace("127.0.0.1", "localhost"),
        sabnzbd,
        jellyfin,
        navidrome,
        // A cloud metadata address, which the probe never asks.
        nzbget: "http://169.254.169.254:80".to_owned(),
        ..SetupCandidates::all_at("http://127.0.0.1:9")
    };
    let app = TestApp::start_with("setup-found", outbound).await;

    let body = found(&app).await;
    assert_eq!(service(&body, "slskd")["outcome"], "found", "{body}");
    let sabnzbd = service(&body, "sabnzbd");
    assert_eq!(sabnzbd["outcome"], "found", "{body}");
    assert_eq!(sabnzbd["note"], "sabnzbd_host_not_whitelisted");
    let jellyfin = service(&body, "jellyfin");
    assert_eq!(jellyfin["outcome"], "found", "{body}");
    assert_eq!(jellyfin["version"], "10.9.11");
    let navidrome = service(&body, "navidrome");
    assert_eq!(navidrome["outcome"], "found", "{body}");
    assert_eq!(navidrome["version"], "0.53.3");
    assert_eq!(service(&body, "nzbget")["outcome"], "not_found", "{body}");
    assert_eq!(service(&body, "nzbget")["url"], "http://169.254.169.254:80");
    assert_eq!(service(&body, "plex")["outcome"], "no_answer", "{body}");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "one request per scripted service: {seen:?}");
    for (target, names) in &seen {
        for refused in ["authorization", "cookie", "x-api-key", "x-plex-token"] {
            assert!(
                !names.iter().any(|name| name == refused),
                "{target}: {names:?}"
            );
        }
        let query = target.split_once('?').map_or("", |(_, query)| query);
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let key = pair.split('=').next().unwrap_or_default();
            assert!(
                !["apikey", "u", "p", "t", "s"].contains(&key),
                "{target} carries {key}"
            );
        }
    }

    let user = app.login_user("plainsetup").await;
    let (status, _) = app
        .call(
            "GET",
            "/api/v3/setup/found",
            &[("authorization", user.authorization.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A server that answers its headers, then one byte a second.
async fn dripping() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = [0u8; 4096];
                let _ = socket.read(&mut head).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n")
                    .await;
                for _ in 0..30 {
                    if socket.write_all(b" ").await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            });
        }
    });
    (format!("http://localhost:{port}"), task)
}

#[tokio::test]
async fn a_hostile_answer_is_never_taken_or_followed() {
    let seen: Seen = Arc::default();
    let followed: Seen = Arc::default();
    let (elsewhere, _elsewhere) = scripted(
        Router::new().fallback(|| async { axum::Json(json!({"status": "OK"})) }),
        &followed,
    )
    .await;
    let (drip, drip_task) = dripping().await;
    let (big, _big) = scripted(
        Router::new().fallback(|| async {
            Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from_stream(futures_util::stream::iter(
                    (0..160).map(|_| Ok::<_, std::io::Error>(vec![b' '; 64 * 1024])),
                )))
                .unwrap()
        }),
        &seen,
    )
    .await;
    let location = format!("{elsewhere}/ping");
    let (redirect, _redirect) = scripted(
        Router::new().fallback(move || {
            let location = location.clone();
            async move { (StatusCode::FOUND, [(header::LOCATION, location)]).into_response() }
        }),
        &seen,
    )
    .await;
    let (junk, _junk) = scripted(
        Router::new().fallback(|| async { "{\"status\": \"OK\"" }),
        &seen,
    )
    .await;
    let mut outbound = closed_outbound();
    outbound.setup_candidates = SetupCandidates {
        slskd: drip,
        jellyfin: big,
        lidarr: redirect,
        prowlarr: junk,
        ..SetupCandidates::all_at("http://127.0.0.1:9")
    };
    let app = TestApp::start_with("setup-found-hostile", outbound).await;
    let admin = app.login_admin().await;

    let started = Instant::now();
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/setup/found",
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    let took = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(took < Duration::from_secs(4), "the look took {took:?}");
    assert_eq!(service(&body, "slskd")["outcome"], "no_answer", "{body}");
    for kind in ["jellyfin", "lidarr", "prowlarr"] {
        assert_eq!(service(&body, kind)["outcome"], "something_else", "{body}");
    }
    assert!(
        followed.lock().unwrap().is_empty(),
        "the redirect was followed"
    );
    drip_task.abort();
}
