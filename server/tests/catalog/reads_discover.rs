//! Discover routes through the production app: input validation, and
//! fixed 5xx envelopes that name an outage's service
//! and wait. Plus the live charts: "your top" read for the calling user's
//! own link, and Last.fm reading the instance key on every call. And the
//! artists a playlist seeds its suggestions with.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{Json, Router, extract::Query, http::StatusCode, routing::get};
use droppedneedle_auth::Role;
use droppedneedle_catalog::discover::models::{ListenSource, StatsRange};
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_catalog::discover::wiring::{deps_over, live_network};
use droppedneedle_kernel::http_client::HttpClientFactory;
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
// The routes: input validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_routes_check_their_input() {
    let app = TestApp::start("reads-discover-input").await;

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
    app.seed("DROP TABLE catalog_ignored_releases").await;
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
/// ListenBrainz links as the rig's database holds them.
fn live_deps(auth: &AuthRig, dir: &std::path::Path, providers: Providers) -> DiscoverDeps {
    let inputs = crate::discover_queue::discover_inputs(auth, dir, providers);
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
    let links = crate::common::listening_store(&auth.platform);
    for (user, username, token) in [("8", "lb-eight", "token-8"), ("9", "lb-nine", "token-9")] {
        let user = auth.seed_user(user, Role::User).await;
        links
            .save_listenbrainz(&user.id, username, token)
            .await
            .unwrap();
    }
    let dir = crate::common::ScratchDir::new("your-top");
    let charts = live_deps(&auth, &dir, providers).charts;

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
    use droppedneedle_catalog::discover::ports::ProviderFailure;

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
    let charts = live_deps(&auth, &dir, providers).charts;

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

// ---------------------------------------------------------------------------
// Playlist suggestions: the artists a playlist seeds them with
// ---------------------------------------------------------------------------

/// A playlist imported from Jellyfin stores Jellyfin's own artist ids. Each
/// entry seeds suggestions with a MusicBrainz artist id: its own when it is
/// one, else the library artist of a track with its recording id, else the
/// one library artist its name keys to. A special-purpose id and a name two
/// library artists share seed nothing; every entry still counts.
#[tokio::test]
async fn playlist_artists_resolve_media_server_ids_to_musicbrainz() {
    use droppedneedle_catalog::discover::radio::library::RadioLibrary;
    use droppedneedle_kernel::db::{DbError, Lane};

    const BY_RECORDING: &str = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";
    const BY_NAME: &str = "87c5dedd-371d-4a53-9f7f-80522fb7f3cb";
    const NATIVE: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    const RECORDING: &str = "0f5a2b4c-1d3e-4f60-8a7b-9c0d1e2f3a4b";
    const VARIOUS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";
    const BY_TAG: &str = "d1e2f3a4-b5c6-4d7e-8f90-a1b2c3d4e5f6";
    const TAGGED_RECORDING: &str = "2c3d4e5f-6a7b-4c8d-9e0f-1a2b3c4d5e6f";

    let dir = crate::common::ScratchDir::new("playlist-artists");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let entry = |position: i64, artist_id: &str, artist_name: &str, recording: &str| {
        format!(
            "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
             artist_name, album_name, artist_id, recording_mbid, source_type, created_at) \
             VALUES ('e{position}', 'pl1', {position}, 'Song', '{artist_name}', 'Album', \
             '{artist_id}', {recording}, 'jellyfin', 1);"
        )
    };
    let seed = [
        format!(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('user-1', 'Ada', 'user', 0); \
             INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('a1', 'Aurora', 'aurora', 'group', 1, 1), \
             ('a2', 'Björk', 'bjork', 'person', 1, 1), ('a3', 'Twin', 'twin', 'group', 1, 1), \
             ('a4', 'Twin', 'twin', 'group', 1, 1), ('a5', 'Tagged', 'tagged', 'group', 1, 1); \
             INSERT INTO local_artist_external_identities (local_artist_id, provider, \
             provider_artist_id, decision_source, selected_at) VALUES \
             ('a1', 'musicbrainz', '{BY_RECORDING}', 'manual', 1), \
             ('a2', 'musicbrainz', '{BY_NAME}', 'manual', 1), \
             ('a3', 'musicbrainz', 'c0000000-0000-4000-8000-000000000003', 'manual', 1), \
             ('a4', 'musicbrainz', 'c0000000-0000-4000-8000-000000000004', 'manual', 1); \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             VALUES ('al1', 'r1', 'g1', 'First Light', 'first light', 'a1', 'automatic', 1, 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source, embedded_recording_mbid) VALUES ('t1', 'al1', \
             'r1', '/music/t1.flac', 't1.flac', 'h1', 1, 1, 's1', 'Opener', 'opener', \
             'First Light', 'first light', 'flac', 'indexed', 'scan', 1, 'automatic', \
             '{RECORDING}'); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source, embedded_recording_mbid, embedded_artist_mbid) \
             VALUES ('t2', 'al1', 'r1', '/music/t2.flac', 't2.flac', 'h2', 1, 1, 's2', 'Second', \
             'second', 'First Light', 'first light', 'flac', 'indexed', 'scan', 1, 'automatic', \
             '{TAGGED_RECORDING}', '{BY_TAG}'); \
             INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
             VALUES ('t1', 0, 'a1', 'main'), ('t2', 0, 'a5', 'main'); \
             INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id) \
             VALUES ('pl1', 'From Jellyfin', 1, 1, 'user-1');"
        ),
        entry(
            0,
            "4f8a1c2e9b7d4e6f8a1c2e9b7d4e6f8a",
            "Someone Else",
            &format!("'{RECORDING}'"),
        ),
        entry(1, "9d2b3c4e5f6a4b7c8d9e0f1a2b3c4d5e", "BJÖRK ", "NULL"),
        entry(2, VARIOUS, "Various Artists", "NULL"),
        entry(3, "1a2b3c4d5e6f4a7b8c9d0e1f2a3b4c5d", "Twin", "NULL"),
        entry(4, &NATIVE.to_uppercase(), "Native", "NULL"),
        entry(
            5,
            "6e7f8a9b0c1d4e2f8a3b4c5d6e7f8a9b",
            "Not In The Library",
            &format!("'{TAGGED_RECORDING}'"),
        ),
    ]
    .concat();
    runtime
        .db()
        .write(Lane::Foreground, "playlist seed", move |tx| {
            tx.execute_batch(&seed).map_err(DbError::from)
        })
        .await
        .expect("seed");

    let (artists, count) = RadioLibrary::new(runtime.db().clone())
        .playlist_artists("user-1", "pl1")
        .await
        .expect("read")
        .expect("visible to its owner");
    assert_eq!(artists, [BY_RECORDING, BY_NAME, NATIVE, BY_TAG]);
    assert_eq!(count, 6);
}
