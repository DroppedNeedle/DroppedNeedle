//! Reads-discover briefs: discover + queue + radio + batches, home with
//! the range-pair redesign, and the now-playing snapshot.
//!
//! Each test pins one behavior. The routers run against the slice fakes
//! with the test-principal layer standing in for the sibling session
//! middleware (auth resolution only). Cache-loop briefs drive the manual
//! sleeper: no real waits anywhere.
//!
//! Stage-5 boundary: Fake* ports swap to real providers; canned values pin
//! handler mapping, not provider data.

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
use droppedneedle::reads::discover::refresh::{
    ManualSleeper, RefreshRegistry, RefreshScope, refresh_once, run_refresh_loop,
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
        queues: Arc::new(FakeQueues::new(clock.clone())),
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
// Auth matrix rows: every slice route needs a session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_discover_read_is_401_with_bearer_challenge() {
    let rig = rig();
    for (method, uri, body) in [
        ("GET", "/api/v3/discover", None),
        ("GET", "/api/v3/home", None),
        ("GET", "/api/v3/now-playing", None),
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

#[tokio::test]
async fn discover_shape_carries_sections_and_service_status() {
    let rig = rig();
    let (status, body, _) = call(app(&rig, Some("user-1")), "GET", "/api/v3/discover", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["discover_queue_enabled"], true);
    assert_eq!(body["fresh_releases"]["type"], "album");
    assert_eq!(body["service_status"]["listenbrainz"], "ok");
    assert_eq!(body["section_status"]["fresh_releases"], "ready");
    assert_eq!(
        body["because_you_listen_to"][0]["seed_artist"],
        "Portishead"
    );
    assert_eq!(body["top_picks"]["title"], "Top Picks for You");
    assert_eq!(body["genre_artwork_schema_version"], "v2");
}

#[tokio::test]
async fn home_shape_carries_shelves() {
    let rig = rig();
    let (status, body, _) = call(app(&rig, Some("user-1")), "GET", "/api/v3/home", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["recently_added"]["type"], "album");
    assert_eq!(body["trending_artists"]["type"], "artist");
    assert_eq!(body["integration_status"]["listenbrainz"], true);
    assert_eq!(body["refreshing"], false);
}

#[tokio::test]
async fn integration_status_refines_local_files() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/home/integration-status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["localfiles"], true);
    assert_eq!(body["youtube"], true);
}

#[tokio::test]
async fn genre_detail_carries_library_and_popular_lanes() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/home/genre/trip-hop?limit=3",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["genre"], "trip-hop");
    assert_eq!(body["library"]["artists"].as_array().unwrap().len(), 3);
    assert_eq!(body["popular"]["has_more_artists"], true);
    assert_eq!(body["genre_artwork"]["kind"], "collage");
}

#[tokio::test]
async fn activity_records_and_returns_cursor() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/activity",
        Some(json!({"feature": "queue", "artist_mbid": "artist-mbid-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["generation"], 7);
    assert_eq!(
        rig.content.activities(),
        vec![("user-1".to_owned(), "queue".to_owned())]
    );
}

#[tokio::test]
async fn unknown_activity_feature_is_400() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/activity",
        Some(json!({"feature": "charts"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");
}

#[tokio::test]
async fn refresh_triggers_and_answers_202() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/refresh",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "ok");
    assert_eq!(rig.content.refreshes_for(), vec!["user-1".to_owned()]);
}

// ---------------------------------------------------------------------------
// Range-pair redesign
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trending_range_param_selects_the_page() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/home/trending/artists?range=this_month&limit=5&source=lastfm",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["range_key"], "this_month");
    assert_eq!(body["label"], "This Month");
    assert_eq!(body["items"].as_array().unwrap().len(), 5);
    assert_eq!(body["has_more"], true);
    assert_eq!(rig.charts.calls(), vec!["trending:this_month:lastfm"]);
}

#[tokio::test]
async fn omitted_range_defaults_to_this_week() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/home/popular/albums?limit=2",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["range_key"], "this_week");
    assert_eq!(body["label"], "This Week");
}

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

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queue_lifecycle_generate_then_consume() {
    let rig = rig();
    let (status, generated, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({"force": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(generated["action"], "started");
    assert_eq!(generated["status"], "ready");

    let (status, deck, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deck["queue_id"], generated["queue_id"]);
    assert_eq!(deck["items"].as_array().unwrap().len(), 10);
    assert_eq!(deck["items"][0]["recommendation_reason"], "Fans also like");

    let (status, queue_status, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue/status",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue_status["status"], "ready");
    assert_eq!(queue_status["stale"], false);
}

#[tokio::test]
async fn queue_count_clamps_to_twenty() {
    let rig = rig();
    let (status, deck, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue?count=99",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deck["items"].as_array().unwrap().len(), 20);
}

#[tokio::test]
async fn ignore_lands_in_the_ledger_and_rebuilds() {
    let rig = rig();
    let (status, _, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/queue/ignore",
        Some(json!({
            "release_group_mbid": "rg-mbid-3",
            "artist_mbid": "artist-mbid-3",
            "release_name": "Album 3",
            "artist_name": "Artist 3",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, ledger, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue/ignored",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ledger["items"][0]["release_group_mbid"], "rg-mbid-3");
    // Ignore kicks a rebuild plus a discover refresh.
    let (status, deck, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!deck["queue_id"].as_str().unwrap().is_empty());
    assert_eq!(rig.content.refreshes_for(), vec!["user-1".to_owned()]);
}

#[tokio::test]
async fn validate_echoes_owned_ids() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/queue/validate",
        Some(json!({"release_group_mbids": ["rg-owned-1", "rg-mbid-2"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["in_library"], json!(["rg-owned-1"]));
}

#[tokio::test]
async fn enrich_and_preview_shapes() {
    let rig = rig();
    let (status, enrich, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue/enrich/rg-mbid-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(enrich["country"], "GB");
    assert_eq!(enrich["tags"], json!(["trip-hop"]));

    let (status, preview, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/queue/preview/rg-mbid-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["status"], "available");

    let (status, missing, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/queue/preview/rg-missing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(missing["status"], "not_found");
}

// ---------------------------------------------------------------------------
// Radio + suggestions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn radio_shelf_and_plan_shapes() {
    let rig = rig();
    let (status, shelf, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/radio",
        Some(json!({"seed_type": "artist", "seed_id": "artist-mbid-1", "count": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shelf["radio_seed_type"], "artist");
    assert_eq!(shelf["items"].as_array().unwrap().len(), 3);

    let (status, plan, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/radio/plan",
        Some(json!({
            "seed_type": "artist",
            "seed_id": "artist-mbid-1",
            "mode": "hybrid",
            "count": 4,
            "exclude_recording_mbids": ["recording-0"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["tracks"].as_array().unwrap().len(), 3);
    assert!(plan["title"].as_str().unwrap().contains("artist"));
}

#[tokio::test]
async fn radio_plan_rejects_bad_seed_and_mode() {
    let rig = rig();
    for body in [
        json!({"seed_type": "mood", "seed_id": "x", "mode": "hybrid"}),
        json!({"seed_type": "artist", "mode": "hybrid"}),
        json!({"seed_type": "artist", "seed_id": "x", "mode": "offline"}),
    ] {
        let (status, envelope, _) = call(
            app(&rig, Some("user-1")),
            "POST",
            "/api/v3/discover/radio/plan",
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error_code(&envelope), "INVALID_INPUT");
    }
}

#[tokio::test]
async fn playlist_suggestions_shape() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/playlist-suggestions",
        Some(json!({"playlist_id": "pl-1", "count": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlist_id"], "pl-1");
    assert_eq!(body["suggestions"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["profile"]["track_count"], 12);
}

// ---------------------------------------------------------------------------
// Batches
// ---------------------------------------------------------------------------

#[tokio::test]
async fn batch_crud_round_trip() {
    let rig = rig();
    let (status, created, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/batches",
        Some(json!({
            "name": "Friday haul",
            "source_section": "fresh_releases",
            "items": [
                {"release_group_mbid": "rg-1", "artist_mbid": "a-1",
                 "album_name": "One", "artist_name": "Uno"},
            ],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["items"][0]["outcome"], "requested");

    let (status, listed, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/batches",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["batches"][0]["id"], id);

    let (status, fetched, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        &format!("/api/v3/discover/batches/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["name"], "Friday haul");

    let (status, removed, _) = call(
        app(&rig, Some("user-1")),
        "DELETE",
        &format!("/api/v3/discover/batches/{id}?remove_albums=false"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed["kept"], 1);

    let (status, _, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        &format!("/api/v3/discover/batches/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

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

#[tokio::test]
async fn empty_batch_is_400() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/batches",
        Some(json!({"name": "Empty", "items": []})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");
}

// ---------------------------------------------------------------------------
// YouTube + previews
// ---------------------------------------------------------------------------

#[tokio::test]
async fn youtube_search_marks_cached_hits() {
    let rig = rig();
    let (status, hit, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue/youtube-track-search?artist=Portishead&track=Roads",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hit["cached"], true);
    assert!(hit["embed_url"].as_str().unwrap().contains("embed"));
    assert!(
        hit["video_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("fake-")),
        "canned ids stay visibly fake: {hit}"
    );

    let (status, miss, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/queue/youtube-search?artist=Portishead&album=no-video-here",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(miss["error"], "not_found");
}

#[tokio::test]
async fn youtube_quota_404s_when_unconfigured() {
    let rig_off = rig_with_youtube(false);
    let (status, body, _) = call(
        app(&rig_off, Some("user-1")),
        "GET",
        "/api/v3/discover/queue/youtube-quota",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "NOT_FOUND");

    let rig_on = rig();
    let (status, quota, _) = call(
        app(&rig_on, Some("user-1")),
        "GET",
        "/api/v3/discover/queue/youtube-quota",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(quota["limit"], 10_000);
}

#[tokio::test]
async fn cache_check_dedupes_case_insensitively() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/queue/youtube-cache-check",
        Some(json!({"items": [
            {"artist": "Portishead", "track": "Roads"},
            {"artist": "portishead", "track": "roads"},
            {"artist": "Massive Attack", "track": "Teardrop"},
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["cached"], true);
    assert_eq!(items[1]["cached"], false);
}

#[tokio::test]
async fn cache_check_answers_empty_when_unconfigured() {
    let rig = rig_with_youtube(false);
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/discover/queue/youtube-cache-check",
        Some(json!({"items": [{"artist": "a", "track": "b"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn previews_answer_empty_until_stage_5() {
    // The fake must not invent provider names or preview URLs: both routes
    // answer shaped empties until real providers land behind the port.
    let rig = rig();
    let (status, track, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/track-preview?artist=Portishead&track=Roads",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(track.get("preview_url").is_none_or(Value::is_null));
    assert!(track.get("provider").is_none_or(Value::is_null));

    let (status, album, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/discover/album-preview?artist=Portishead&album=Dummy&count=2",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(album["tracks"].as_array().unwrap().len(), 0);
    assert!(album.get("provider").is_none_or(Value::is_null));
}

#[tokio::test]
async fn youtube_search_misses_when_unconfigured() {
    // An unconfigured fake answers like a miss (the services map `None` to
    // the `not_found` shape), never a canned video id.
    let rig = rig_with_youtube(false);
    for uri in [
        "/api/v3/discover/queue/youtube-search?artist=Portishead&album=Dummy",
        "/api/v3/discover/queue/youtube-track-search?artist=Portishead&track=Roads",
    ] {
        let (status, body, _) = call(app(&rig, Some("user-1")), "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(body["error"], "not_found", "{uri}");
        assert!(body.get("video_id").is_none_or(Value::is_null), "{uri}");
    }
}

// ---------------------------------------------------------------------------
// Now playing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn now_playing_snapshot_keeps_redacted_rows() {
    let rig = rig();
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "GET",
        "/api/v3/now-playing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["track_name"], "Roads");
    assert_eq!(sessions[1]["redacted"], true);
    assert_eq!(sessions[1]["track_name"], "");
    assert_eq!(sessions[1]["progress_ms"], 10_000);
}

// ---------------------------------------------------------------------------
// Leak briefs: 5xx bodies are fixed strings plus an error id
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

// ---------------------------------------------------------------------------
// Cache-loop briefs: single-flight, error-continue, honest intervals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn loop_rebuilds_single_flight_per_scope() {
    let registry = RefreshRegistry::new();
    let runs = Arc::new(std::sync::Mutex::new(0usize));
    assert!(registry.begin(RefreshScope::Discover));
    let skipped = refresh_once(&registry, RefreshScope::Discover, || {
        let runs = runs.clone();
        async move {
            *runs.lock().unwrap() += 1;
            Ok(())
        }
    })
    .await;
    assert!(!skipped);
    assert_eq!(*runs.lock().unwrap(), 0);
    // Other scopes are unaffected by the held claim.
    let ran = refresh_once(&registry, RefreshScope::Home, || {
        let runs = runs.clone();
        async move {
            *runs.lock().unwrap() += 1;
            Ok(())
        }
    })
    .await;
    assert!(ran);
    assert_eq!(*runs.lock().unwrap(), 1);
}

#[tokio::test]
async fn loop_failures_release_the_claim_and_continue() {
    let registry = RefreshRegistry::new();
    let calls = Arc::new(std::sync::Mutex::new(Vec::<&str>::new()));
    let first = refresh_once(&registry, RefreshScope::Home, || {
        let calls = calls.clone();
        async move {
            calls.lock().unwrap().push("fail");
            Err::<(), String>("charts down".to_owned())
        }
    })
    .await;
    assert!(first);
    assert!(!registry.is_live(RefreshScope::Home));
    let second = refresh_once(&registry, RefreshScope::Home, || {
        let calls = calls.clone();
        async move {
            calls.lock().unwrap().push("ok");
            Ok(())
        }
    })
    .await;
    assert!(second);
    assert_eq!(*calls.lock().unwrap(), vec!["fail", "ok"]);
}

#[tokio::test]
async fn loop_uses_honest_intervals_and_stops_on_shutdown() {
    let registry = Arc::new(RefreshRegistry::new());
    let sleeper = ManualSleeper::new();
    let runs = Arc::new(std::sync::Mutex::new(0usize));
    let task_registry = registry.clone();
    let task_sleeper = sleeper.clone();
    let task_runs = runs.clone();
    let task = tokio::spawn(async move {
        run_refresh_loop(task_registry, task_sleeper, RefreshScope::Discover, || {
            let task_runs = task_runs.clone();
            async move {
                *task_runs.lock().unwrap() += 1;
                Ok(())
            }
        })
        .await;
    });
    // First sleep parks on the discover interval; wake it twice, then stop.
    // Progress is polled with yields only: no clock waits anywhere.
    for _ in 0..100 {
        if sleeper.waits() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(sleeper.waits(), 1);
    sleeper.wake();
    for _ in 0..100 {
        if sleeper.requested().len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    sleeper.wake();
    for _ in 0..100 {
        if sleeper.requested().len() == 3 {
            break;
        }
        tokio::task::yield_now().await;
    }
    sleeper.shut_down();
    task.await.unwrap();
    assert_eq!(*runs.lock().unwrap(), 2);
    assert!(
        sleeper
            .requested()
            .iter()
            .all(|each| *each == RefreshScope::Discover.interval())
    );
    assert!(!registry.is_live(RefreshScope::Discover));
}
