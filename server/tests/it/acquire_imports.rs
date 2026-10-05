//! Imports and acquisition health.
//!
//! Coverage of the Lidarr read-only import (monitored artists
//! become follows), the Spotify OAuth/playlist import plus settings rows,
//! the `spotify:import` durable job, and the acquisition health smoke with
//! its independent per-source release gates. Every test runs against the
//! in-repo loopback mocks or scripted probes; nothing touches a live
//! Lidarr, Spotify, slskd, SABnzbd, or indexer.

use droppedneedle::acquire::imports;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::auth::users::roles::{Role, SessionKind};
use imports::handlers::{ImportsDeps, imports_router};
use imports::health::{
    ClientProbe, HealthProbes, ScriptedFree, ScriptedLidarr, ScriptedNewznab, ScriptedSabnzbd,
    ScriptedSlskd,
};
use imports::jobs::{JobRegistry, QueuedSpotifyImport, TaskExecutor};
use imports::lidarr::{
    FollowStore as _, LidarrClient, LidarrImportService, MemoryApprovalSink, MemoryFollowStore,
    MemoryLidarrSettings,
};
use imports::mocks::{
    LIDARR_KEY, LIDARR_MBID_ALL, LIDARR_MBID_NONE, SPOTIFY_TOKEN, serve_lidarr, serve_spotify,
};
use imports::models::{LidarrConnectionSettings, SpotifySettings};
use imports::spotify::{
    FixedMbidResolver, MemoryPlaylistIndex, MemorySpotifyConnections, MemorySpotifySettings,
    MemorySpotifyStates, MemoryTrackSink, PlaylistTrackSink as _, SpotifyClient,
    SpotifyConnectionStore as _, SpotifyImportService, is_allowed_cover_url, redirect_uri,
};
use imports::spotify::{SpotifyConnection, TokenGrant};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// Fixed error id for leak-envelope assertions. A valid UUID.
const FIXED_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Id generator returning one fixed value.
#[derive(Debug, Clone)]
struct FixedIdGenerator;

impl droppedneedle::ids::IdGenerator for FixedIdGenerator {
    fn new_id(&self) -> String {
        FIXED_ID.to_owned()
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

fn no_redirect_client() -> reqwest::Client {
    droppedneedle::http_client::HttpClientFactory::new()
        .expect("http factory builds")
        .no_redirect()
        .clone()
}

/// Fully wired module bundle over memory stores for tests.
#[allow(dead_code)]
struct Rig {
    deps: ImportsDeps,
    lidarr_settings: Arc<MemoryLidarrSettings>,
    follows: Arc<MemoryFollowStore>,
    approvals: Arc<MemoryApprovalSink>,
    spotify_settings: Arc<MemorySpotifySettings>,
    spotify_links: Arc<MemorySpotifyConnections>,
    playlists: Arc<MemoryPlaylistIndex>,
    tracks: Arc<MemoryTrackSink>,
    resolver: Arc<FixedMbidResolver>,
    slskd: Arc<ScriptedSlskd>,
    sabnzbd: Arc<ScriptedSabnzbd>,
    newznab: Arc<ScriptedNewznab>,
    lidarr_probe: Arc<ScriptedLidarr>,
    free_probe: Arc<ScriptedFree>,
}

fn rig_for(rig: &TestRig, lidarr_base: &str, api_base: &str, accounts_base: &str) -> Rig {
    let http = http_client();
    let lidarr_settings = Arc::new(MemoryLidarrSettings::new());
    let follows = Arc::new(MemoryFollowStore::new());
    let approvals = Arc::new(MemoryApprovalSink::new());
    let spotify_settings = Arc::new(MemorySpotifySettings::new());
    let spotify_states = Arc::new(MemorySpotifyStates::new());
    let spotify_links = Arc::new(MemorySpotifyConnections::new());
    let playlists = Arc::new(MemoryPlaylistIndex::new());
    let tracks = Arc::new(MemoryTrackSink::new());
    let resolver = Arc::new(FixedMbidResolver::new());
    let spotify_client =
        SpotifyClient::new(http.clone(), no_redirect_client(), api_base, accounts_base);
    let service = Arc::new(SpotifyImportService::new(
        spotify_client.clone(),
        spotify_settings.clone(),
        spotify_links.clone(),
        playlists.clone(),
        tracks.clone(),
        resolver.clone(),
    ));
    let jobs = Arc::new(JobRegistry::new());
    let runner = service.clone();
    let executor = Arc::new(TaskExecutor::new(
        jobs.clone(),
        move |job: QueuedSpotifyImport| {
            let service = runner.clone();
            tokio::spawn(async move {
                let result = service
                    .populate_playlist(&job.user_id, &job.spotify_playlist_id, &job.playlist_id)
                    .await
                    .map_err(|error| match error {
                        imports::spotify::SpotifyError::NotLinked => {
                            "Spotify account not linked".to_owned()
                        }
                        imports::spotify::SpotifyError::Unavailable(_) => {
                            "Failed to fetch playlist from Spotify".to_owned()
                        }
                    });
                (job.playlist_id.clone(), result)
            })
        },
    ));
    let slskd = Arc::new(ScriptedSlskd::new());
    let sabnzbd = Arc::new(ScriptedSabnzbd::new());
    let newznab = Arc::new(ScriptedNewznab::new());
    let lidarr_probe = Arc::new(ScriptedLidarr::new());
    let free_probe = Arc::new(ScriptedFree::new());
    let _ = lidarr_base;
    let deps = ImportsDeps {
        http: http.clone(),
        lidarr: LidarrClient::new(http),
        lidarr_settings: lidarr_settings.clone(),
        follows: follows.clone(),
        approvals: approvals.clone(),
        spotify: spotify_client,
        spotify_settings: spotify_settings.clone(),
        spotify_states,
        spotify_links: spotify_links.clone(),
        playlists: playlists.clone(),
        tracks: tracks.clone(),
        resolver: resolver.clone(),
        jobs,
        executor,
        probes: Arc::new(HealthProbes {
            slskd: slskd.clone(),
            sabnzbd: sabnzbd.clone(),
            newznab: newznab.clone(),
            lidarr: lidarr_probe.clone(),
            free: free_probe.clone(),
        }),
        auth: rig.deps.clone(),
        ids: Arc::new(FixedIdGenerator),
        base_path: String::new(),
    };
    Rig {
        deps,
        lidarr_settings,
        follows,
        approvals,
        spotify_settings,
        spotify_links,
        playlists,
        tracks,
        resolver,
        slskd,
        sabnzbd,
        newznab,
        lidarr_probe,
        free_probe,
    }
}

fn authed_app(deps: ImportsDeps, user_id: &str) -> Router {
    let user_id = user_id.to_owned();
    imports_router(deps).layer(axum::middleware::from_fn(
        move |mut req: Request<Body>, next: axum::middleware::Next| {
            let user_id = user_id.clone();
            async move {
                req.extensions_mut().insert(CurrentSession {
                    user_id,
                    session_id: "sess-1".to_owned(),
                    kind: SessionKind::Standard,
                    transport: Transport::Bearer,
                });
                next.run(req).await
            }
        },
    ))
}

fn anon_app(deps: ImportsDeps) -> Router {
    imports_router(deps)
}

async fn request_json(app: Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.oneshot(request).await.expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

async fn get_json(app: Router, path: &str) -> (StatusCode, Value) {
    request_json(
        app,
        Request::get(path)
            .header("host", "app.test")
            .body(Body::empty())
            .expect("request builds"),
    )
    .await
}

async fn post_json(app: Router, path: &str, payload: Value) -> (StatusCode, Value) {
    request_json(
        app,
        Request::post(path)
            .header("host", "app.test")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request builds"),
    )
    .await
}

async fn put_json(app: Router, path: &str, payload: Value) -> (StatusCode, Value) {
    request_json(
        app,
        Request::put(path)
            .header("host", "app.test")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request builds"),
    )
    .await
}

async fn seed_rig() -> (TestRig, String, String) {
    let rig = TestRig::new().expect("rig builds");
    let admin = rig.seed_user("root", Role::Admin).await;
    let user = rig.seed_user("ada", Role::User).await;
    (rig, admin.id, user.id)
}

fn open_client(version: &str, message: &str) -> ClientProbe {
    ClientProbe {
        enabled: true,
        configured: true,
        reachable: true,
        version: Some(version.to_owned()),
        message: message.to_owned(),
    }
}

// --- Pure-semantics tests: MBID, URL, track-count, image, redirect URI ---

#[test]
fn redirect_uri_prefers_origin_and_mounts_base_path_once() {
    assert_eq!(
        redirect_uri("https://music.example.com", "http://app.test", ""),
        "https://music.example.com/api/v3/spotify/auth/callback"
    );
    assert_eq!(
        redirect_uri("", "http://app.test/", ""),
        "http://app.test/api/v3/spotify/auth/callback"
    );
    assert_eq!(
        redirect_uri("", "http://app.test", "/needle"),
        "http://app.test/needle/api/v3/spotify/auth/callback"
    );
    assert_eq!(
        redirect_uri(
            "https://music.example.com/needle",
            "http://app.test",
            "/needle"
        ),
        "https://music.example.com/needle/api/v3/spotify/auth/callback"
    );
}

#[test]
fn cover_allowlist_blocks_non_cdn_and_plain_http() {
    assert!(is_allowed_cover_url("https://i.scdn.co/image/abc"));
    assert!(!is_allowed_cover_url("http://i.scdn.co/image/abc"));
    assert!(!is_allowed_cover_url("https://evil.example.com/x.jpg"));
    assert!(!is_allowed_cover_url("not a url"));
}

// --- Lidarr import tests ---

#[tokio::test]
async fn lidarr_import_becomes_follows_with_counts() {
    let (rig, admin_id, _) = seed_rig().await;
    let (server, _) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(
        &rig,
        &server.base_url,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    );
    bundle.lidarr_settings.seed(&server.base_url, LIDARR_KEY);
    let payload = json!({"selected_mbids": [LIDARR_MBID_ALL, LIDARR_MBID_NONE]});

    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/lidarr-import/import",
        payload.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["imported"], 2);
    assert_eq!(body["already_following"], 0);
    assert_eq!(body["skipped_invalid"], 0);
    assert_eq!(body["auto_download_enabled"], 1);
    assert_eq!(body["approval_batch_id"], Value::Null);

    // Re-import never re-arms or double-counts: disjoint pre-existing subset.
    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/lidarr-import/import",
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["imported"], 0);
    assert_eq!(body["already_following"], 2);
    assert_eq!(body["auto_download_enabled"], 0);

    // Candidates now read as already following.
    let (status, body) = get_json(
        authed_app(bundle.deps, &admin_id),
        "/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["artists"][0]["already_following"], true);
}

#[tokio::test]
async fn lidarr_non_admin_mirror_opens_approval_batch_at_service_level() {
    // The HTTP surface is admin-only, so the non-admin branch is briefed at
    // the service seam: the ordered writes still mirror intent, but only
    // behind an approval batch id.
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    let settings = Arc::new(MemoryLidarrSettings::new());
    settings.seed(&lidarr.base_url, LIDARR_KEY);
    let follows = Arc::new(MemoryFollowStore::new());
    let approvals = Arc::new(MemoryApprovalSink::new());
    let service = LidarrImportService::new(
        LidarrClient::new(http_client()),
        settings,
        follows.clone(),
        approvals.clone(),
    );
    let response = service
        .import_artists("u-9", false, &[LIDARR_MBID_ALL.to_owned()])
        .await
        .expect("import works");
    assert_eq!(response.imported, 1);
    assert_eq!(response.auto_download_enabled, 1);
    assert!(response.approval_batch_id.is_some());
    assert_eq!(approvals.batches().len(), 1);
    assert!(follows.auto_download_intent("u-9", &LIDARR_MBID_ALL.to_lowercase()));
}

#[tokio::test]
async fn lidarr_config_round_trip_masks_and_normalizes() {
    let (rig, admin_id, _) = seed_rig().await;
    let (server, _) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(
        &rig,
        &server.base_url,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    );
    let pasted = format!("{}/api/v1", server.base_url);

    let (status, body) = put_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/lidarr-import/config",
        json!({"url": pasted, "api_key": LIDARR_KEY}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["url"], server.base_url);
    assert_eq!(body["api_key"], "lidarr****");

    // Saving the masked sentinel preserves the stored key: the probe still
    // authenticates afterwards.
    let (status, _) = put_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/lidarr-import/config",
        json!({"url": server.base_url, "api_key": "lidarr****"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_json(
        authed_app(bundle.deps, &admin_id),
        "/acquire/lidarr-import/test",
        json!({"url": server.base_url, "api_key": "lidarr****"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["valid"], true);
}

#[tokio::test]
async fn lidarr_surface_issues_only_the_two_sanctioned_gets() {
    let (rig, admin_id, _) = seed_rig().await;
    let (server, recorder) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(
        &rig,
        &server.base_url,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    );
    bundle.lidarr_settings.seed(&server.base_url, LIDARR_KEY);

    let _ = post_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/lidarr-import/test",
        json!({"url": server.base_url, "api_key": LIDARR_KEY}),
    )
    .await;
    let _ = get_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/lidarr-import/artists",
    )
    .await;
    let _ = post_json(
        authed_app(bundle.deps, &admin_id),
        "/acquire/lidarr-import/import",
        json!({"selected_mbids": [LIDARR_MBID_ALL]}),
    )
    .await;

    let calls = recorder.snapshot();
    assert!(!calls.lidarr_paths.is_empty());
    for path in &calls.lidarr_paths {
        assert!(
            path == "/api/v1/system/status" || path == "/api/v1/artist",
            "sanctioned paths only, saw {path}"
        );
    }
    for key in &calls.lidarr_keys {
        assert_eq!(key, LIDARR_KEY);
    }
}

#[tokio::test]
async fn lidarr_routes_gate_anonymous_and_non_admin() {
    let (rig, admin_id, user_id) = seed_rig().await;
    let (server, _) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(
        &rig,
        &server.base_url,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    );
    bundle.lidarr_settings.seed(&server.base_url, LIDARR_KEY);

    let (status, body) = get_json(
        anon_app(bundle.deps.clone()),
        "/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &user_id),
        "/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    // The admin still passes.
    let (status, _) = get_json(
        authed_app(bundle.deps, &admin_id),
        "/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// --- Spotify tests ---

/// Seed the admin Spotify app via the settings route.
async fn seed_spotify_app(bundle_deps: ImportsDeps, admin_id: &str) {
    let (status, body) = put_json(
        authed_app(bundle_deps, admin_id),
        "/acquire/spotify/settings",
        json!({
            "client_id": "test-client",
            "client_secret": "test-secret",
            "enabled": true,
            "spotify_redirect_origin": "",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["client_id"], "test-client");
    assert_eq!(body["client_secret"], "spotify****");
}

/// Pull the `state` token out of an authorize URL.
fn state_from_auth_url(auth_url: &str) -> String {
    auth_url
        .split('?')
        .nth(1)
        .unwrap_or("")
        .split('&')
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "state").then(|| value.to_owned())
        })
        .expect("state param present")
}

/// Link `user_id` through the real authorize + callback flow.
async fn link_spotify(bundle_deps: ImportsDeps, user_id: &str) {
    let (status, body) = get_json(
        authed_app(bundle_deps.clone(), user_id),
        "/acquire/spotify/auth/url",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let auth_url = body["auth_url"].as_str().expect("auth url");
    assert!(auth_url.starts_with("https://accounts.spotify.com/authorize?"));
    assert!(auth_url.contains("test-client"));
    let state = state_from_auth_url(auth_url);
    let callback = format!("/acquire/spotify/auth/callback?code=good-code&state={state}");
    let response = anon_app(bundle_deps)
        .oneshot(
            Request::get(callback)
                .header("host", "app.test")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        location.ends_with("/profile?spotify=connected"),
        "saw {location}"
    );
}

async fn poll_job_done(deps: ImportsDeps, user_id: &str, spotify_id: &str) -> Value {
    for _ in 0..100 {
        let (status, body) = get_json(
            authed_app(deps.clone(), user_id),
            &format!("/acquire/spotify/jobs/{spotify_id}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if body["state"] != "running" {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("job never finished");
}

#[tokio::test]
async fn spotify_settings_round_trip_and_reject_bad_origin() {
    let (rig, admin_id, user_id) = seed_rig().await;
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    let (api, accounts, _, _) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(&rig, &lidarr.base_url, &api.base_url, &accounts.base_url);

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/spotify/settings",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);

    seed_spotify_app(bundle.deps.clone(), &admin_id).await;

    let (status, body) = put_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/spotify/settings",
        json!({
            "client_id": "test-client",
            "client_secret": "spotify****",
            "enabled": true,
            "spotify_redirect_origin": "not-a-url",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_INPUT");

    // Non-admins cannot read the app settings.
    let (status, _) = get_json(
        authed_app(bundle.deps.clone(), &user_id),
        "/acquire/spotify/settings",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The redirect URI derives through the single source of truth.
    let (status, body) = get_json(
        authed_app(bundle.deps, &admin_id),
        "/acquire/spotify/redirect-uri",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["redirect_uri"],
        "http://app.test/api/v3/spotify/auth/callback"
    );
}

#[tokio::test]
async fn spotify_callback_rejects_error_and_replayed_state() {
    let (rig, admin_id, user_id) = seed_rig().await;
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    let (api, accounts, _, _) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(&rig, &lidarr.base_url, &api.base_url, &accounts.base_url);
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;

    // Provider-side error redirects plain.
    let response = anon_app(bundle.deps.clone())
        .oneshot(
            Request::get("/acquire/spotify/auth/callback?error=access_denied")
                .header("host", "app.test")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);

    // Unknown state names its reason.
    let response = anon_app(bundle.deps.clone())
        .oneshot(
            Request::get("/acquire/spotify/auth/callback?code=good-code&state=nope")
                .header("host", "app.test")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(
        location.ends_with("/profile?spotify=error&reason=state"),
        "saw {location}"
    );

    // Bad code names the token reason.
    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &user_id),
        "/acquire/spotify/auth/url",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let state = state_from_auth_url(body["auth_url"].as_str().unwrap_or(""));
    let response = anon_app(bundle.deps.clone())
        .oneshot(
            Request::get(format!(
                "/acquire/spotify/auth/callback?code=bad-code&state={state}"
            ))
            .header("host", "app.test")
            .body(Body::empty())
            .expect("request builds"),
        )
        .await
        .expect("router answers");
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(
        location.ends_with("/profile?spotify=error&reason=token"),
        "saw {location}"
    );

    // Replaying the consumed state fails closed.
    let response = anon_app(bundle.deps)
        .oneshot(
            Request::get(format!(
                "/acquire/spotify/auth/callback?code=good-code&state={state}"
            ))
            .header("host", "app.test")
            .body(Body::empty())
            .expect("request builds"),
        )
        .await
        .expect("router answers");
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(
        location.ends_with("/profile?spotify=error&reason=state"),
        "saw {location}"
    );
}

#[tokio::test]
async fn spotify_import_answers_fast_then_populates_two_tracks() {
    let (rig, admin_id, user_id) = seed_rig().await;
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    let (api, accounts, _, _) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(&rig, &lidarr.base_url, &api.base_url, &accounts.base_url);
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &user_id).await;
    bundle
        .resolver
        .seed("Aurora Current", "Neon Meridian", "rg-mbid-1");

    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &user_id),
        "/acquire/spotify/playlists/sp-playlist-1/import",
        json!({"name": "Neon Nights"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let playlist_id = body["playlist_id"]
        .as_str()
        .expect("playlist id")
        .to_owned();

    let done = poll_job_done(bundle.deps.clone(), &user_id, "sp-playlist-1").await;
    assert_eq!(done["state"], "done");
    assert_eq!(done["playlist_id"], playlist_id);
    assert_eq!(done["track_count"], 2);

    // The `is_local` row never lands; the resolved album carries its MBID.
    let rows = bundle.tracks.tracks(&playlist_id);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].track_name, "Midnight Drive");
    assert_eq!(rows[0].artist_name, "Aurora Current");
    assert_eq!(rows[0].album_id, "rg-mbid-1");
    assert_eq!(rows[0].duration, Some(183));
    assert_eq!(rows[1].track_name, "Glass Tides");
    assert_eq!(rows[1].album_id, "");
    assert_eq!(rows[1].duration, Some(201));

    // The cover persisted best-effort.
    assert!(bundle.tracks.cover(&playlist_id).is_some());

    // Re-import answers the same record and replaces the rows wholesale.
    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &user_id),
        "/acquire/spotify/playlists/sp-playlist-1/import",
        json!({"name": "Neon Nights"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlist_id"], playlist_id);
    let done = poll_job_done(bundle.deps.clone(), &user_id, "sp-playlist-1").await;
    assert_eq!(done["state"], "done");
    assert_eq!(bundle.tracks.tracks(&playlist_id).len(), 2);

    // Playlists now carry the imported mapping.
    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id),
        "/acquire/spotify/playlists",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlists"][0]["imported_playlist_id"], playlist_id);
}

#[tokio::test]
async fn spotify_tracks_come_from_items_never_legacy_tracks() {
    let (rig, admin_id, user_id) = seed_rig().await;
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    let (api, accounts, _, recorder) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(&rig, &lidarr.base_url, &api.base_url, &accounts.base_url);
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &user_id).await;

    let (status, _) = post_json(
        authed_app(bundle.deps.clone(), &user_id),
        "/acquire/spotify/playlists/sp-playlist-1/import",
        json!({"name": "Neon Nights"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let _ = poll_job_done(bundle.deps, &user_id, "sp-playlist-1").await;

    let calls = recorder.snapshot();
    assert!(
        calls
            .spotify_paths
            .iter()
            .any(|path| path.ends_with("/items"))
    );
    assert!(
        !calls
            .spotify_paths
            .iter()
            .any(|path| path == "LEGACY_TRACKS_PATH"),
        "the deprecated /tracks path must never be called"
    );
    assert!(
        calls
            .spotify_grants
            .iter()
            .any(|grant| grant == "authorization_code")
    );
}

#[tokio::test]
async fn spotify_cover_fetch_refuses_redirects_and_non_images() {
    let (_, _, _) = seed_rig().await;
    let (_, _, covers, _) = serve_spotify().await.expect("mock serves");
    let client = SpotifyClient::new(
        http_client(),
        no_redirect_client(),
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    );

    let direct = format!("{}/covers/neon.jpg", covers.base_url);
    let fetched = client.fetch_cover(&direct).await.expect("fetch works");
    assert!(fetched.is_some());
    assert_eq!(
        fetched.map(|(_, content_type)| content_type).as_deref(),
        Some("image/jpeg")
    );

    let redirect = format!("{}/covers/redirect.jpg", covers.base_url);
    assert_eq!(
        client.fetch_cover(&redirect).await.expect("fetch works"),
        None
    );

    let text = format!("{}/covers/not-an-image.jpg", covers.base_url);
    assert_eq!(client.fetch_cover(&text).await.expect("fetch works"), None);
}

#[tokio::test]
async fn spotify_expired_tokens_refresh_transparently() {
    let (rig, admin_id, user_id) = seed_rig().await;
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    let (api, accounts, _, recorder) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(&rig, &lidarr.base_url, &api.base_url, &accounts.base_url);
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &user_id).await;

    // Age the link row into expiry; the next list must refresh, not 401.
    let mut link = bundle.spotify_links.get(&user_id).expect("link row");
    link.expires_at_unix = 1;
    link.access_token = "stale-token".to_owned();
    bundle.spotify_links.upsert(&user_id, &link);

    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id),
        "/acquire/spotify/playlists",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlists"].as_array().map(Vec::len), Some(2));
    let refreshed = bundle.spotify_links.get(&user_id).expect("link row");
    assert_eq!(refreshed.access_token, SPOTIFY_TOKEN);
    let calls = recorder.snapshot();
    assert!(
        calls
            .spotify_grants
            .iter()
            .any(|grant| grant == "refresh_token")
    );
}

// --- Health smoke + per-source gate tests ---

async fn seed_scripted_bases() -> (TestRig, String, String, Rig) {
    let (rig, admin_id, user_id) = seed_rig().await;
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    let (api, accounts, _, _) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(&rig, &lidarr.base_url, &api.base_url, &accounts.base_url);
    (rig, admin_id, user_id, bundle)
}

fn gate<'a>(body: &'a Value, source: &str) -> &'a Value {
    body["gates"]
        .as_array()
        .expect("gates array")
        .iter()
        .find(|gate| gate["source"] == source)
        .unwrap_or_else(|| panic!("{source} gate present"))
}

#[tokio::test]
async fn health_smoke_defaults_to_free_ready() {
    let (_, _, user_id, bundle) = seed_scripted_bases().await;

    let (status, body) = get_json(authed_app(bundle.deps, &user_id), "/acquire/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ready"], true);
    assert_eq!(body["ready_via"], json!(["free"]));
    assert_eq!(gate(&body, "slskd")["open"], false);
    assert_eq!(gate(&body, "sabnzbd")["open"], false);
    assert_eq!(gate(&body, "newznab")["open"], false);
    assert_eq!(gate(&body, "lidarr_import")["open"], false);
}

#[tokio::test]
async fn slskd_gate_greens_independently() {
    let (_, _, user_id, bundle) = seed_scripted_bases().await;
    bundle.slskd.set(open_client("1.4.3", "slskd 1.4.3"));

    let (status, body) = get_json(authed_app(bundle.deps, &user_id), "/acquire/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gate(&body, "slskd")["open"], true);
    assert_eq!(gate(&body, "sabnzbd")["open"], false);
    assert_eq!(gate(&body, "newznab")["open"], false);
    assert_eq!(gate(&body, "lidarr_import")["open"], false);
    assert_eq!(body["ready_via"], json!(["free", "slskd"]));
}

#[tokio::test]
async fn health_degrades_when_an_enabled_path_is_down() {
    let (_, _, user_id, bundle) = seed_scripted_bases().await;
    bundle.slskd.set(ClientProbe {
        enabled: true,
        configured: true,
        reachable: false,
        version: None,
        message: "slskd is unreachable".to_owned(),
    });

    let (status, body) = get_json(authed_app(bundle.deps, &user_id), "/acquire/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["ready"], true);
    assert_eq!(body["ready_via"], json!(["free"]));
}

#[tokio::test]
async fn slskd_status_readable_by_any_user() {
    let (_, _, user_id, bundle) = seed_scripted_bases().await;
    bundle.slskd.set(open_client("1.4.3", "slskd 1.4.3"));

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &user_id),
        "/acquire/slskd/status",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], true);
    assert_eq!(body["reachable"], true);
    assert_eq!(body["version"], "1.4.3");

    let (status, _) = get_json(anon_app(bundle.deps), "/acquire/slskd/status").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sabnzbd_status_is_admin_only_with_mount_hint() {
    let (_, admin_id, user_id, bundle) = seed_scripted_bases().await;
    bundle.sabnzbd.set(
        open_client("4.3.2", "SABnzbd 4.3.2"),
        vec!["music".to_owned(), "audiobooks".to_owned()],
        Some("/completed".to_owned()),
    );

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &admin_id),
        "/acquire/sabnzbd/status",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["valid"], true);
    assert_eq!(body["version"], "4.3.2");
    assert_eq!(body["categories"], json!(["music", "audiobooks"]));
    assert_eq!(body["complete_dir"], "/completed");

    let (status, body) =
        get_json(authed_app(bundle.deps, &user_id), "/acquire/sabnzbd/status").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");
}

/// Stored secrets never surface in Debug: connection keys, the Spotify
/// client secret, per-user tokens, and token grants all render redacted.
#[test]
fn import_secrets_never_logged() {
    let lidarr = LidarrConnectionSettings {
        url: "http://lidarr:8686".to_owned(),
        api_key: "LIDARR-SUPERSECRET".to_owned(),
    };
    let debug = format!("{lidarr:?}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");

    let spotify = SpotifySettings {
        client_id: "id".to_owned(),
        client_secret: "SPOTIFY-SUPERSECRET".to_owned(),
        enabled: true,
        spotify_redirect_origin: String::new(),
    };
    let debug = format!("{spotify:?}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");

    let connection = SpotifyConnection {
        access_token: "ACCESS-SUPERSECRET".to_owned(),
        refresh_token: "REFRESH-SUPERSECRET".to_owned(),
        expires_at_unix: 1,
        username: "u".to_owned(),
        spotify_user_id: "s".to_owned(),
    };
    let debug = format!("{connection:?}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");

    let grant = TokenGrant {
        access_token: "ACCESS-SUPERSECRET".to_owned(),
        refresh_token: Some("REFRESH-SUPERSECRET".to_owned()),
        expires_in_secs: 3600,
    };
    let debug = format!("{grant:?}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");
}
