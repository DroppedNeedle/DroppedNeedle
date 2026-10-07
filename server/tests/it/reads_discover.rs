//! Discover routes over the discover ports: the session challenge, input
//! validation, batch isolation between users, redacted now-playing rows,
//! and fixed 5xx envelopes.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::session::store::SessionKind;
use droppedneedle::auth::users::memory::with_test_principal;
use droppedneedle::ids::IdGenerator;
use droppedneedle::reads::discover::fakes::{
    FakeBatches, FakeCharts, FakeContent, FakeNowPlaying, FakePreviews, FakeQueues, FakeRadio,
    FakeYouTube, ManualClock,
};
use droppedneedle::reads::discover::services::ReadsDeps;
use serde_json::{Value, json};
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Fixed id generator so error ids assert stably.
#[derive(Debug, Clone)]
struct FixedIds;

impl IdGenerator for FixedIds {
    fn new_id(&self) -> String {
        "123e4567-e89b-12d3-a456-426614174000".to_owned()
    }
}

struct Rig {
    deps: ReadsDeps,
    _clock: ManualClock,
    content: Arc<FakeContent>,
    charts: Arc<FakeCharts>,
}

fn rig() -> Rig {
    rig_with_youtube(true)
}

fn rig_with_youtube(configured: bool) -> Rig {
    let clock = ManualClock::new(1_790_000_000);
    let content = Arc::new(FakeContent::new(clock.clone()));
    let charts = Arc::new(FakeCharts::new());
    let youtube: Arc<dyn droppedneedle::reads::discover::ports::YouTubeSource> = if configured {
        Arc::new(FakeYouTube::configured())
    } else {
        Arc::new(FakeYouTube::unconfigured())
    };
    let deps = ReadsDeps {
        content: content.clone(),
        queues: Arc::new(FakeQueues),
        batches: Arc::new(FakeBatches::new(clock.clone())),
        charts: charts.clone(),
        previews: Arc::new(FakePreviews),
        youtube,
        radio: Arc::new(FakeRadio),
        now_playing: Arc::new(FakeNowPlaying),
        ids: Arc::new(FixedIds),
        clock: Arc::new(clock.clone()),
    };
    Rig {
        deps,
        _clock: clock,
        content,
        charts,
    }
}

fn principal(user_id: &str) -> CurrentSession {
    CurrentSession {
        user_id: user_id.to_owned(),
        session_id: "sess-test".to_owned(),
        kind: SessionKind::Standard,
        transport: Transport::Bearer,
    }
}

fn app(rig: &Rig, user: Option<&str>) -> Router {
    let router = Router::new().nest(
        "/api/v3",
        droppedneedle::reads::discover::reads_router(rig.deps.clone()),
    );
    match user {
        Some(id) => with_test_principal(router, principal(id)),
        None => router,
    }
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, headers)
}

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("<no-code>")
}

// ---------------------------------------------------------------------------
// Auth matrix: every discover route needs a session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_discover_read_is_401_with_bearer_challenge() {
    let rig = rig();
    // GET /now-playing is absent here: playback serves it from the
    // live playback registry, and the standing auth matrix pins its 401.
    for (method, uri, body) in [
        ("GET", "/api/v3/discover", None),
        ("GET", "/api/v3/home", None),
        (
            "POST",
            "/api/v3/discover/batches",
            Some(json!({"name": "x", "items": []})),
        ),
        (
            "GET",
            "/api/v3/home/trending/artists?range=this_month",
            None,
        ),
    ] {
        let (status, body, headers) = call(app(&rig, None), method, uri, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert_eq!(error_code(&body), "UNAUTHORIZED", "{method} {uri}");
        assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer");
    }
}

#[tokio::test]
async fn malformed_body_stays_in_the_envelope() {
    let rig = rig();
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/discover/activity")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let response = app(&rig, Some("user-1")).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error_code(&body), "INVALID_INPUT");
}

// ---------------------------------------------------------------------------
// Discover + home shapes
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Range-pair redesign
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_range_is_400_not_a_silent_fallback() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/home/trending/artists?range=last_eon",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");
}

#[tokio::test]
async fn legacy_range_path_pair_is_gone() {
    let rig = rig();
    let (status, _, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/home/trending/artists/this_month",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Radio + suggestions
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Batches
// ---------------------------------------------------------------------------

#[tokio::test]
async fn foreign_batch_reads_as_missing() {
    let rig = rig();
    let (_, created, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/batches",
        Some(json!({
            "name": "Mine",
            "items": [{"release_group_mbid": "rg-1"}],
        })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    for (method, uri) in [
        ("GET", format!("/api/v3/discover/batches/{id}")),
        ("DELETE", format!("/api/v3/discover/batches/{id}")),
    ] {
        let (status, body, _) = call(app(&rig, Some("user-2")), method, &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(error_code(&body), "NOT_FOUND");
    }
    let (status, listed, _) = call(
        app(&rig, Some("user-2")),
        "GET",
        "/api/v3/discover/batches",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["batches"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// YouTube + previews
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Now playing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn now_playing_snapshot_keeps_redacted_rows() {
    // The app mounts GET /now-playing from playback, so this pins the
    // retired reads handler through its own
    // router until the dead plumbing is deleted.
    let rig = rig();
    let router = Router::new().nest(
        "/api/v3",
        droppedneedle::reads::discover::now_playing_router(rig.deps.clone()),
    );
    let router = with_test_principal(router, principal("user-1"));
    let (status, body, _) = call(router, "GET", "/api/v3/now-playing", None).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["track_name"], "Roads");
    assert_eq!(sessions[1]["redacted"], true);
    assert_eq!(sessions[1]["track_name"], "");
    assert_eq!(sessions[1]["progress_ms"], 10_000);
}

// ---------------------------------------------------------------------------
// Leaks: 5xx bodies are fixed strings plus an error id
// ---------------------------------------------------------------------------

/// Markers that must never reach a response body: a path, a host, a token.
const LEAK_MARKERS: &[&str] = &[
    "/srv/secrets/discover.key",
    "charts.internal.example.com",
    "dn-discover-marker-7a22",
];

fn leak_cause() -> String {
    format!(
        "dial {} via {} with {}",
        LEAK_MARKERS[0], LEAK_MARKERS[1], LEAK_MARKERS[2]
    )
}

#[tokio::test]
async fn failing_content_renders_fixed_500_without_leaks() {
    let rig = rig();
    rig.content.fail_content(&leak_cause());
    let (status, body, _) = call(app(&rig, Some("user-1")), "GET", "/api/v3/discover", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error_code(&body), "INTERNAL_ERROR");
    assert_eq!(body["error"]["message"], "Internal server error");
    assert_eq!(
        body["error"]["details"]["error_id"],
        "123e4567-e89b-12d3-a456-426614174000"
    );
    let raw = body.to_string();
    for marker in LEAK_MARKERS {
        assert!(!raw.contains(marker), "leaked {marker}");
    }
}

#[tokio::test]
async fn failing_charts_render_fixed_502_without_leaks() {
    // Charts failures are provider failures: 502 with the fixed message.
    // The fake routes content failures through 500 and chart failures
    // through 502 to pin both mappings.
    let rig = rig();
    rig.charts.fail_charts(&leak_cause());
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/home/trending/artists",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "UPSTREAM_ERROR");
    assert_eq!(body["error"]["message"], "Upstream service error");
    let raw = body.to_string();
    for marker in LEAK_MARKERS {
        assert!(!raw.contains(marker), "leaked {marker}");
    }
}

/// Your-top charts are fetched for the calling user only.
#[tokio::test]
async fn your_top_is_per_user() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-9")),
        "GET",
        "/api/v3/home/your-top/albums?range=all_time&limit=3",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["range_key"], "all_time");
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    assert_eq!(
        rig.charts.calls(),
        vec!["your_top:user-9:all_time:listenbrainz"]
    );
}
