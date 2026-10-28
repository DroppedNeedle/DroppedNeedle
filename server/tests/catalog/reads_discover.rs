//! Discover routes through the production app: the session challenge,
//! input validation, and fixed 5xx envelopes that name an outage's service
//! and wait. Plus the live charts: "your top" read for the calling user's
//! own link, and Last.fm reading the instance key on every call.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{Json, Router, extract::Query, http::StatusCode, routing::get};
use droppedneedle_catalog::reads::discover::models::{ListenSource, StatsRange};
use droppedneedle_catalog::reads::discover::services::ReadsDeps;
use droppedneedle_catalog::reads::discover::wiring::{deps_over, live_network};
use droppedneedle_kernel::http_client::HttpClientFactory;
use droppedneedle_media::playback::listening::{
    ListenBrainzLinkStore as _, MemoryListenBrainzLinkStore,
};
use droppedneedle_providers::{ProviderEndpoints, Providers, ReqwestWire};
use serde_json::{Value, json};

use crate::common::TestApp;
use crate::common::auth::AuthRig;

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("<no-code>")
}

/// Serve `stub` on a free local port; answers its origin.
async fn serve(stub: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    origin
}

// ---------------------------------------------------------------------------
// The routes: session challenge and input validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_routes_need_a_session_and_check_their_input() {
    let app = TestApp::start("reads-discover-input").await;

    // Every discover route needs a session. GET /now-playing is absent
    // here: playback serves it, and the standing auth matrix pins its 401.
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
        let (status, headers, bytes) = app.raw(method, uri, &[], body).await;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert_eq!(error_code(&body), "UNAUTHORIZED", "{method} {uri}");
        assert_eq!(headers["www-authenticate"], "Bearer", "{method} {uri}");
    }

    let user = app.login_user("discover-input").await;
    let auth = [("authorization", user.authorization.as_str())];

    // A malformed body stays in the error envelope.
    let (status, _, bytes) = app
        .raw_bytes(
            "POST",
            "/api/v3/discover/activity",
            &[
                ("authorization", user.authorization.as_str()),
                ("content-type", "application/json"),
            ],
            b"{not json".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error_code(&body), "INVALID_INPUT");

    // An unknown range is a 400, not a silent fallback.
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/home/trending/artists?range=last_eon",
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    // The range is a query value; the old path pair is gone.
    let (status, _, _) = app
        .raw(
            "GET",
            "/api/v3/home/trending/artists/this_month",
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    app.stop().await;
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

/// A stand-in for every outside service. ListenBrainz's sitewide artists
/// chart answers 503 with `Retry-After: 30`, its release-group chart 400;
/// everything else answers 503. Every body carries
/// the leak markers.
async fn failing_stub() -> String {
    let outage = || async {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", "30")],
            leak_cause(),
        )
    };
    serve(
        Router::new()
            .route("/listenbrainz/1/stats/sitewide/artists", get(outage))
            .route(
                "/listenbrainz/1/stats/sitewide/release-groups",
                get(|| async { (StatusCode::BAD_REQUEST, leak_cause()) }),
            )
            .fallback(outage),
    )
    .await
}

#[tokio::test]
async fn failures_answer_the_fixed_body_and_an_outage_names_its_service() {
    let origin = failing_stub().await;
    let app = TestApp::start_with(
        "reads-discover-failures",
        crate::common::outbound_at(&origin),
    )
    .await;
    let user = app.login_user("failures").await;

    // A chart ListenBrainz refuses, and a store that fails, both answer 500
    // with the fixed body naming the request id and nothing of the cause.
    // They go first: the outage below rests ListenBrainz for a while.
    app.db()
        .execute_batch("DROP TABLE catalog_ignored_releases")
        .expect("ignore ledger dropped");
    for (n, uri) in [
        "/api/v3/home/popular/albums?source=listenbrainz",
        "/api/v3/discover/queue/ignored",
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = format!("fault-{n}");
        let (status, _, bytes) = app
            .raw(
                "GET",
                uri,
                &[
                    ("authorization", user.authorization.as_str()),
                    ("x-request-id", request_id.as_str()),
                ],
                None,
            )
            .await;
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}: {text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"error": {
                "code": "INTERNAL_ERROR",
                "message": "Internal server error",
                "details": {"error_id": request_id},
            }}),
            "{uri}"
        );
        for marker in LEAK_MARKERS.iter().chain(&["catalog_ignored_releases"]) {
            assert!(!text.contains(marker), "leaked {marker} on {uri}");
        }
    }

    // ListenBrainz is down: the client hears which service and how long to
    // wait, with the request id and nothing of the provider's body.
    let (status, headers, bytes) = app
        .raw(
            "GET",
            "/api/v3/home/trending/artists?source=listenbrainz",
            &[
                ("authorization", user.authorization.as_str()),
                ("x-request-id", "outage-1"),
            ],
            None,
        )
        .await;
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert_eq!(headers["retry-after"], "30");
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"error": {
            "code": "PROVIDER_UNAVAILABLE",
            "message": "An outside service is not answering. Try again later.",
            "details": {"error_id": "outage-1", "provider": "listenbrainz", "retry_after": 30},
        }}),
    );
    for marker in LEAK_MARKERS {
        assert!(!text.contains(marker), "leaked {marker}");
    }

    app.stop().await;
}

// ---------------------------------------------------------------------------
// Live charts
// ---------------------------------------------------------------------------

/// Discover over the live listening reads of `providers`, with the users'
/// ListenBrainz `links`.
fn live_deps(
    auth: &AuthRig,
    dir: &std::path::Path,
    providers: Providers,
    links: MemoryListenBrainzLinkStore,
) -> ReadsDeps {
    let inputs = crate::discover_queue::discover_inputs(auth, dir, providers, links);
    let network = live_network(&inputs, &auth.config);
    deps_over(inputs, &auth.config, network)
}

/// Your-top charts read the calling user's own ListenBrainz link.
#[tokio::test]
async fn your_top_reads_the_callers_own_link() {
    let paths: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = paths.clone();
    let origin = serve(Router::new().fallback(move |uri: axum::http::Uri| {
        let seen = seen.clone();
        async move {
            seen.lock().unwrap().push(uri.path().to_owned());
            let rows: Vec<Value> = (1..=4)
                .map(|n| {
                    json!({
                        "release_group_name": format!("Album {n}"),
                        "artist_name": "Someone",
                        "listen_count": 10 - n,
                        "release_group_mbid": format!("{n:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
                    })
                })
                .collect();
            Json(json!({"payload": {"release_groups": rows}}))
        }
    }))
    .await;
    let http = HttpClientFactory::new().unwrap();
    let providers = Providers::unpaced_at(
        Arc::new(ReqwestWire::new(&http)),
        ProviderEndpoints {
            listenbrainz: format!("{origin}/listenbrainz"),
            ..ProviderEndpoints::closed()
        },
    );
    let auth = AuthRig::new("your-top").await;
    let links = MemoryListenBrainzLinkStore::new(auth.deps.crypto.clone());
    links.save("user-8", "lb-eight", "token-8").await.unwrap();
    links.save("user-9", "lb-nine", "token-9").await.unwrap();
    let dir = crate::common::ScratchDir::new("your-top");
    let charts = live_deps(&auth, &dir, providers, links).charts;

    let page = charts
        .your_top_albums(
            "user-9",
            StatsRange::AllTime,
            3,
            0,
            ListenSource::Listenbrainz,
        )
        .await
        .unwrap();
    assert_eq!(page.range_key, StatsRange::AllTime);
    assert_eq!(page.items.len(), 3);
    assert!(page.has_more);
    assert_eq!(
        *paths.lock().unwrap(),
        ["/listenbrainz/1/stats/user/lb-nine/release-groups"]
    );
}

/// Last.fm charts read the instance key on every call: no key answers
/// "not configured", a key saved later serves the chart with it. While
/// ListenBrainz is down, the ListenBrainz chart falls back to it.
#[tokio::test]
async fn lastfm_charts_read_the_instance_key_per_call() {
    use droppedneedle_catalog::reads::discover::ports::ProviderFailure;

    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = seen.clone();
    let app = Router::new().route(
        "/",
        get(move |Query(query): Query<HashMap<String, String>>| {
            let record = record.clone();
            async move {
                record
                    .lock()
                    .unwrap()
                    .push(query.get("api_key").cloned().unwrap_or_default());
                Json(json!({"artists": {"artist": [
                    {"name": "One", "mbid": "a74b1b7f-71a5-4011-9441-d0b5e4122711", "playcount": "30"},
                    {"name": "Two", "mbid": "", "playcount": "20"},
                    {"name": "Three", "playcount": "10"},
                ]}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let http = HttpClientFactory::new().unwrap();
    let providers = Providers::unpaced_at(
        Arc::new(ReqwestWire::new(&http)),
        ProviderEndpoints {
            lastfm: base,
            ..ProviderEndpoints::closed()
        },
    );
    let auth = AuthRig::new("charts").await;
    let dir = crate::common::ScratchDir::new("charts");
    let links = MemoryListenBrainzLinkStore::new(auth.deps.crypto.clone());
    let charts = live_deps(&auth, &dir, providers, links).charts;

    let off = charts
        .trending_artists("user-1", StatsRange::ThisWeek, 2, 0, ListenSource::Lastfm)
        .await
        .unwrap_err();
    assert!(matches!(off, ProviderFailure::NotConfigured(_)), "{off:?}");
    assert!(seen.lock().unwrap().is_empty(), "no key, no call");

    auth.set_lastfm_instance_key("instance-key");
    let page = charts
        .trending_artists("user-1", StatsRange::ThisWeek, 2, 0, ListenSource::Lastfm)
        .await
        .unwrap();
    let names: Vec<&str> = page.items.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["One", "Two"]);
    assert!(page.has_more);
    assert_eq!(
        page.items[1].musicbrainz_id, None,
        "artists without an MBID stay"
    );

    // ListenBrainz is not answering: a user who can read Last.fm gets its
    // chart instead, from the read already kept.
    let fallback = charts
        .trending_artists(
            "user-1",
            StatsRange::ThisWeek,
            2,
            0,
            ListenSource::Listenbrainz,
        )
        .await
        .unwrap();
    assert_eq!(fallback.items[0].name, "One");
    assert_eq!(*seen.lock().unwrap(), ["instance-key"], "one Last.fm read");
}
