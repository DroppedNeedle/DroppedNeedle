//! Imports and acquisition health.
//!
//! Coverage of the Lidarr read-only import (monitored artists
//! become follows), the Spotify OAuth/playlist import plus settings rows,
//! the Spotify import jobs, and the acquisition health smoke with
//! its independent per-source release gates. Every test runs against the
//! in-repo loopback mocks or scripted probes; nothing touches a live
//! Lidarr, Spotify, slskd, SABnzbd, or indexer. The Spotify import runs on
//! the production stores over the rig's database and settings.

use crate::common;
use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use droppedneedle_acquire::imports;

use std::sync::Arc;

use crate::common::bundles::quiet_ticks;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_acquire::clients::LiveClients;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::worker::SourceList;
use droppedneedle_acquire::downloads::worker::WorkerSettings;
use droppedneedle_acquire::follows::Follows;
use droppedneedle_acquire::probes::{LiveProbes, ProbeCache, refresh_probes};
use droppedneedle_acquire::readiness::SourceReadiness;
use droppedneedle_acquire::settings::{ConfigLidarrSettings, ConfigSpotifySettings};
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::wiring::ClientLives;
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_catalog::reads::collections::db::CollectionsDb;
use droppedneedle_catalog::reads::collections::models::PlaylistTrack;
use droppedneedle_catalog::reads::collections::store::PlaylistStore;
use droppedneedle_kernel::jobs::queue::{Handlers, JobQueue, start_runners};
use droppedneedle_kernel::runtime_config::secret_sections::SpotifySettings as SpotifySection;
use droppedneedle_kernel::runtime_config::secret_sections::{
    DownloadClients, LidarrImportConnection, SabnzbdConnection, SlskdConnection,
};
use droppedneedle_kernel::runtime_config::sections::{MbSourceMode, MusicBrainzSettings};
use droppedneedle_kernel::runtime_config::{ConfigError, Secret};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_testkit::imports::{
    LIDARR_KEY, LIDARR_MBID_ALL, LIDARR_MBID_NONE, MockRecorder, SPOTIFY_PLAYLIST_NO_TRACK_ID,
    SPOTIFY_TOKEN, serve_lidarr, serve_spotify,
};
use droppedneedle_testkit::musicbrainz::MockMusicBrainz;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
use droppedneedle_testkit::usenet::{SabnzbdMock, serve_loopback};
use imports::handlers::{ImportsDeps, imports_router};
use imports::jobs::{SPEC, SpotifyImportJobs, SpotifyImports};
use imports::lidarr::{LidarrClient, LidarrImportService};
use imports::models::{LidarrConnectionSettings, SpotifySettings};
use imports::spotify::{ImportedTrack, SpotifyImportService};
use imports::spotify_albums::AlbumResolver;
use imports::spotify_client::{SpotifyClient, TokenGrant, allowed_cover_url};
use imports::spotify_oauth::{
    PendingAuth, SPOTIFY_CALLBACK_PATH, SPOTIFY_LEGACY_CALLBACK_PATH, SpotifyConnection,
    redirect_uri,
};
use imports::spotify_store::{CollectionsPlaylistBridge, SqliteSpotifyLinks, SqliteSpotifyStates};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// Fixed error id for leak-envelope assertions. A valid UUID.
const FIXED_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Id generator returning one fixed value.
#[derive(Debug, Clone)]
struct FixedIdGenerator;

impl droppedneedle_kernel::ids::IdGenerator for FixedIdGenerator {
    fn new_id(&self) -> String {
        FIXED_ID.to_owned()
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

fn no_redirect_client() -> reqwest::Client {
    droppedneedle_kernel::http_client::HttpClientFactory::new()
        .expect("http factory builds")
        .no_redirect()
        .clone()
}

/// Fully wired module bundle for tests: the Spotify import on the
/// production stores over the auth rig's database and settings, Lidarr on
/// the rig's follows and settings, health on the live probes over the
/// rig's settings.
#[allow(dead_code)]
struct Rig {
    deps: ImportsDeps,
    spotify_links: Arc<SqliteSpotifyLinks>,
    playlists: PlaylistStore,
    clients: Arc<LiveClients>,
    probe_cache: Arc<ProbeCache>,
    worker_settings: Arc<WorkerSettings>,
}

impl Rig {
    /// One probe pass over the saved settings, as the refresh loop runs it.
    async fn refresh_probes(&self) {
        refresh_probes(&self.probe_cache, &self.clients.current().probe_inputs).await;
    }
}

/// The release group the MusicBrainz mock proves _Neon Meridian_ is.
const NEON_MERIDIAN_GROUP: &str = "5907a1b0-0000-4000-8000-000000000b01";

fn rig_for(auth: &AuthRig, api_base: &str, accounts_base: &str, cover_cdn: Option<&str>) -> Rig {
    rig_with(auth, api_base, accounts_base, cover_cdn, None)
}

fn rig_with(
    auth: &AuthRig,
    api_base: &str,
    accounts_base: &str,
    cover_cdn: Option<&str>,
    plugins: Option<Arc<PluginHost>>,
) -> Rig {
    let http = http_client();
    let platform = &auth.platform;
    let db = AcquireDb::new(platform.db.clone(), platform.clock.clone());
    let playlists = PlaylistStore::new(CollectionsDb::new(
        platform.db.clone(),
        platform.clock.clone(),
    ));
    let lidarr_settings = Arc::new(ConfigLidarrSettings::new(platform.settings.clone()));
    let follows = Follows::new(db.clone());
    let spotify_settings = Arc::new(ConfigSpotifySettings::new(platform.settings.clone()));
    let spotify_states = Arc::new(SqliteSpotifyStates::new(db.clone()));
    let spotify_links = Arc::new(SqliteSpotifyLinks::new(db.clone(), platform.crypto.clone()));
    let spotify_playlists = Arc::new(CollectionsPlaylistBridge::new(
        playlists.clone(),
        db.clone(),
    ));
    let providers = droppedneedle_providers::Providers::unpaced(Arc::new(
        droppedneedle_providers::ReqwestWire::new(&platform.http),
    ));
    let resolver = Arc::new(AlbumResolver::new(&providers, platform.settings.live()));
    let spotify_client = SpotifyClient::new(
        http.clone(),
        no_redirect_client(),
        api_base,
        accounts_base,
        cover_cdn,
    );
    let service = Arc::new(SpotifyImportService::new(
        spotify_client.clone(),
        spotify_settings.clone(),
        spotify_links.clone(),
        spotify_playlists.clone(),
        resolver.clone(),
    ));
    let import_jobs = SpotifyImportJobs::new(service, platform.events.clone());
    let queue = JobQueue::new(platform.db.clone(), platform.clock.clone());
    let spotify_imports = SpotifyImports::new(platform.db.clone(), queue.clone());
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    let clients = Arc::new(LiveClients::new(
        platform.http.shared().clone(),
        ClientLives::new(&platform.settings),
        platform.config.slskd_downloads_path.clone(),
        journal.clone(),
        Arc::new(Targets::new(journal, None)),
        plugins,
    ));
    let probe_cache = Arc::new(ProbeCache::new(clients.clone()));
    let probes = Arc::new(LiveProbes::new(probe_cache.clone()));
    let worker_settings = Arc::new(WorkerSettings::new(&platform.settings, clients.clone()));
    let deps = ImportsDeps {
        lidarr: LidarrClient::new(http),
        lidarr_settings,
        follows,
        spotify: spotify_client,
        spotify_settings,
        spotify_states,
        spotify_links: spotify_links.clone(),
        spotify_playlists,
        resolver,
        spotify_imports,
        probes: Arc::new(LiveProbes::bundle(&probes)),
        readiness: SourceReadiness::new(
            SourceList::Live(worker_settings.clone()),
            probe_cache.clone(),
        ),
        ids: Arc::new(FixedIdGenerator),
        base_path: String::new(),
        trusted_proxies: Default::default(),
    };
    start_runners(
        &queue,
        &platform.supervisor,
        Handlers::default().with(SPEC, Arc::new(import_jobs)),
    )
    .expect("import runner starts");
    Rig {
        deps,
        spotify_links,
        playlists,
        clients,
        probe_cache,
        worker_settings,
    }
}

/// The rows on one playlist, in order.
async fn playlist_rows(playlists: &PlaylistStore, playlist_id: &str) -> Vec<PlaylistTrack> {
    playlists.tracks(playlist_id).await.expect("rows read")
}

/// The row ids on one playlist, in order.
async fn track_ids(playlists: &PlaylistStore, playlist_id: &str) -> Vec<String> {
    playlist_rows(playlists, playlist_id)
        .await
        .into_iter()
        .map(|row| row.id)
        .collect()
}

fn authed_app(deps: ImportsDeps, user_id: &str, role: Role) -> Router {
    let principal = test_principal(user_id, role, "sess-1", SessionKind::Standard);
    with_test_principal(imports_router(deps), principal)
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

/// Save the Lidarr connection to the mock in the rig's settings.
async fn seed_lidarr(auth: &AuthRig, base_url: &str) {
    auth.platform
        .settings
        .save_secret(LidarrImportConnection {
            url: base_url.to_owned(),
            api_key: Secret::new(LIDARR_KEY.to_owned()),
        })
        .await
        .expect("lidarr settings save");
}

async fn seed_rig() -> (AuthRig, String, String) {
    let rig = AuthRig::new("imports").await;
    let admin = rig.seed_user("root", Role::Admin).await;
    let user = rig.seed_user("ada", Role::User).await;
    (rig, admin.id, user.id)
}

// --- Pure-semantics tests: MBID, URL, track-count, image, redirect URI ---

#[test]
fn redirect_uri_prefers_origin_and_mounts_base_path_once() {
    assert_eq!(
        redirect_uri(
            SPOTIFY_CALLBACK_PATH,
            "https://music.example.com",
            "http://app.test",
            ""
        ),
        "https://music.example.com/api/v3/acquire/spotify/auth/callback"
    );
    assert_eq!(
        redirect_uri(SPOTIFY_CALLBACK_PATH, "", "http://app.test/", ""),
        "http://app.test/api/v3/acquire/spotify/auth/callback"
    );
    assert_eq!(
        redirect_uri(SPOTIFY_CALLBACK_PATH, "", "http://app.test", "/needle"),
        "http://app.test/needle/api/v3/acquire/spotify/auth/callback"
    );
    assert_eq!(
        redirect_uri(
            SPOTIFY_CALLBACK_PATH,
            "https://music.example.com/needle",
            "http://app.test",
            "/needle"
        ),
        "https://music.example.com/needle/api/v3/acquire/spotify/auth/callback"
    );
}

#[test]
fn cover_allowlist_blocks_non_cdn_and_plain_http() {
    let allowed = |url: &str| allowed_cover_url(url, None).is_some();
    assert!(allowed("https://i.scdn.co/image/abc"));
    assert!(!allowed("http://i.scdn.co/image/abc"));
    assert!(!allowed("https://evil.example.com/x.jpg"));
    assert!(!allowed("not a url"));
    // A Spotify answer naming a local service is never fetched.
    assert!(!allowed("http://127.0.0.1:1/x"));
    assert!(!allowed("https://localhost/x"));
}

// --- Lidarr import tests ---

/// The Lidarr base URL is the user's and can hold a secret of its own (a
/// proxy path, basic-auth userinfo). A dead Lidarr must not put it in
/// the detail the import and the probe log.
#[tokio::test]
async fn lidarr_transport_errors_never_carry_the_url() {
    const MARKER: &str = "MARKERKEY7f3a91";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let base = format!("http://127.0.0.1:{port}/{MARKER}");

    let err = LidarrClient::new(http_client())
        .system_status(&base, "key")
        .await
        .expect_err("refused");
    let shown = format!("{err:?}");
    assert!(shown.contains("Unavailable"), "{shown}");
    assert!(!shown.contains(MARKER), "{shown}");
}

#[tokio::test]
async fn lidarr_import_becomes_follows_with_counts() {
    let (auth, admin_id, _) = seed_rig().await;
    let (server, _) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(&auth, "http://127.0.0.1:1", "http://127.0.0.1:1", None);
    seed_lidarr(&auth, &server.base_url).await;
    let payload = json!({"selected_mbids": [LIDARR_MBID_ALL, LIDARR_MBID_NONE]});

    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/import",
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
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/import",
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["imported"], 0);
    assert_eq!(body["already_following"], 2);
    assert_eq!(body["auto_download_enabled"], 0);

    // Candidates now read as already following.
    let (status, body) = get_json(
        authed_app(bundle.deps, &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/artists",
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
    let (auth, _, user_id) = seed_rig().await;
    let (lidarr, _) = serve_lidarr().await.expect("mock serves");
    seed_lidarr(&auth, &lidarr.base_url).await;
    let follows = Follows::new(AcquireDb::new(
        auth.platform.db.clone(),
        auth.platform.clock.clone(),
    ));
    let service = LidarrImportService::new(
        LidarrClient::new(http_client()),
        Arc::new(ConfigLidarrSettings::new(auth.platform.settings.clone())),
        follows.clone(),
    );
    let response = service
        .import_artists(&user_id, false, &[LIDARR_MBID_ALL.to_owned()])
        .await
        .expect("import works");
    assert_eq!(response.imported, 1);
    assert_eq!(response.auto_download_enabled, 1);
    let batches = follows.batch_list().await.expect("batches read");
    assert_eq!(batches.count, 1);
    assert_eq!(
        Some(batches.batches[0].batch_id.clone()),
        response.approval_batch_id
    );
    let status = follows
        .status(&user_id, LIDARR_MBID_ALL)
        .await
        .expect("status reads");
    assert!(status.auto_download);
    assert_eq!(
        serde_json::to_value(status.auto_download_state).unwrap(),
        "pending"
    );
}

#[tokio::test]
async fn lidarr_config_round_trip_masks_and_normalizes() {
    let (auth, admin_id, _) = seed_rig().await;
    let (server, _) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(&auth, "http://127.0.0.1:1", "http://127.0.0.1:1", None);
    let pasted = format!("{}/api/v1", server.base_url);

    let (status, body) = put_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/config",
        json!({"url": pasted, "api_key": LIDARR_KEY}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["url"], server.base_url);
    assert_eq!(body["api_key"], "********");

    // Saving the masked sentinel preserves the stored key: the probe still
    // authenticates afterwards.
    let (status, _) = put_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/config",
        json!({"url": server.base_url, "api_key": "********"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_json(
        authed_app(bundle.deps, &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/test",
        json!({"url": server.base_url, "api_key": "********"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["valid"], true);
}

#[tokio::test]
async fn lidarr_surface_issues_only_the_two_sanctioned_gets() {
    let (auth, admin_id, _) = seed_rig().await;
    let (server, recorder) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(&auth, "http://127.0.0.1:1", "http://127.0.0.1:1", None);
    seed_lidarr(&auth, &server.base_url).await;

    let _ = post_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/test",
        json!({"url": server.base_url, "api_key": LIDARR_KEY}),
    )
    .await;
    let _ = get_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/artists",
    )
    .await;
    let _ = post_json(
        authed_app(bundle.deps, &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/import",
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
    let (auth, admin_id, user_id) = seed_rig().await;
    let (server, _) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(&auth, "http://127.0.0.1:1", "http://127.0.0.1:1", None);
    seed_lidarr(&auth, &server.base_url).await;

    let (status, body) = get_json(
        anon_app(bundle.deps.clone()),
        "/api/v3/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    // The admin still passes.
    let (status, _) = get_json(
        authed_app(bundle.deps, &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// --- Spotify tests ---

/// The MusicBrainz mock, saved as the rig's MusicBrainz source.
async fn serve_musicbrainz(auth: &AuthRig) -> MockMusicBrainz {
    let mock = MockMusicBrainz::start().await.expect("mock serves");
    auth.config
        .save(MusicBrainzSettings {
            source_mode: MbSourceMode::Mirror,
            selected_source_mode: MbSourceMode::Mirror,
            api_url: mock.base_url(),
            rate_limit: 0.0,
            ..MusicBrainzSettings::default()
        })
        .await
        .expect("source saves");
    mock
}

/// Seed the admin Spotify app via the settings route.
async fn seed_spotify_app(bundle_deps: ImportsDeps, admin_id: &str) {
    let (status, body) = put_json(
        authed_app(bundle_deps, admin_id, Role::Admin),
        "/api/v3/acquire/spotify/settings",
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
    assert_eq!(body["client_secret"], "********");
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

/// Start the authorize step for `user_id`; answers the authorize URL and
/// its state token.
async fn authorize(bundle_deps: ImportsDeps, user_id: &str) -> (String, String) {
    let (status, body) = get_json(
        authed_app(bundle_deps, user_id, Role::User),
        "/api/v3/acquire/spotify/auth/url",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let auth_url = body["auth_url"].as_str().expect("auth url").to_owned();
    assert!(auth_url.starts_with("https://accounts.spotify.com/authorize?"));
    assert!(auth_url.contains("test-client"));
    let state = state_from_auth_url(&auth_url);
    (auth_url, state)
}

/// Link `user_id` through the real authorize + callback flow.
async fn link_spotify(bundle_deps: ImportsDeps, recorder: &MockRecorder, user_id: &str) {
    link_spotify_via(
        bundle_deps,
        recorder,
        user_id,
        "/api/v3/acquire/spotify/auth/callback",
    )
    .await;
}

/// [`link_spotify`] with the callback answered at `callback`. Authorize
/// must name the URI the mock app lists, as Spotify requires.
async fn link_spotify_via(
    bundle_deps: ImportsDeps,
    recorder: &MockRecorder,
    user_id: &str,
    callback: &str,
) {
    let (auth_url, state) = authorize(bundle_deps.clone(), user_id).await;
    let listed = recorder
        .registered_redirect_uri()
        .replace(':', "%3A")
        .replace('/', "%2F");
    assert!(
        auth_url.contains(&format!("redirect_uri={listed}&")),
        "authorize names an unlisted URI: {auth_url}"
    );
    let callback = format!("{callback}?code=good-code&state={state}");
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
    for _ in 0..500 {
        let (status, body) = get_json(
            authed_app(deps.clone(), user_id, Role::User),
            &format!("/api/v3/acquire/spotify/jobs/{spotify_id}"),
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
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, _) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/spotify/settings",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);

    seed_spotify_app(bundle.deps.clone(), &admin_id).await;

    let (status, body) = put_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/spotify/settings",
        json!({
            "client_id": "test-client",
            "client_secret": "********",
            "enabled": true,
            "spotify_redirect_origin": "not-a-url",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_INPUT");

    // Non-admins cannot read the app settings.
    let (status, _) = get_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/settings",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The redirect URI derives through the single source of truth.
    let (status, body) = get_json(
        authed_app(bundle.deps, &admin_id, Role::Admin),
        "/api/v3/acquire/spotify/redirect-uri",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["redirect_uri"],
        "http://app.test/api/v3/acquire/spotify/auth/callback"
    );
}

/// A GET as a reverse proxy at `peer` would forward it: the browser asked
/// for `https://music.example`, the proxy reached the server as `app.test`.
fn forwarded_get(path: &str, peer: &str) -> Request<Body> {
    let peer: std::net::SocketAddr = peer.parse().expect("peer parses");
    Request::get(path)
        .header("host", "app.test")
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", "music.example")
        .extension(axum::extract::ConnectInfo(peer))
        .body(Body::empty())
        .expect("request builds")
}

/// The callback address the admin page shows and the one the authorize
/// link carries, which must be the same.
async fn callback_addresses(deps: &ImportsDeps, admin_id: &str, peer: &str) -> String {
    let (status, shown) = request_json(
        authed_app(deps.clone(), admin_id, Role::Admin),
        forwarded_get("/api/v3/acquire/spotify/redirect-uri", peer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let shown = shown["redirect_uri"].as_str().expect("address").to_owned();
    let (status, link) = request_json(
        authed_app(deps.clone(), admin_id, Role::Admin),
        forwarded_get("/api/v3/acquire/spotify/auth/url", peer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let encoded = shown.replace(':', "%3A").replace('/', "%2F");
    let link = link["auth_url"].as_str().expect("auth url");
    assert!(
        link.contains(&format!("redirect_uri={encoded}&")),
        "authorize names another address than {shown}: {link}"
    );
    shown
}

/// The callback address follows the address the browser used, as a trusted
/// proxy reports it. Forwarded headers from anyone else count for nothing.
#[tokio::test]
async fn spotify_callback_address_follows_a_trusted_proxy_only() {
    const TRUSTED: &str = "127.0.0.1:4000";
    const STRANGER: &str = "203.0.113.9:4000";
    let (auth, admin_id, _) = seed_rig().await;
    let (api, accounts, covers, _) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;

    assert_eq!(
        callback_addresses(&bundle.deps, &admin_id, TRUSTED).await,
        "https://music.example/api/v3/acquire/spotify/auth/callback"
    );
    assert_eq!(
        callback_addresses(&bundle.deps, &admin_id, STRANGER).await,
        "http://app.test/api/v3/acquire/spotify/auth/callback"
    );

    let mut mounted = bundle.deps.clone();
    mounted.base_path = "/needle".to_owned();
    assert_eq!(
        callback_addresses(&mounted, &admin_id, TRUSTED).await,
        "https://music.example/needle/api/v3/acquire/spotify/auth/callback"
    );

    // No address to work out: an honest 400 that names the field, no guess.
    for path in [
        "/api/v3/acquire/spotify/redirect-uri",
        "/api/v3/acquire/spotify/auth/url",
    ] {
        let (status, body) = request_json(
            authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
            Request::get(path).body(Body::empty()).expect("builds"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["error"]["code"], "INVALID_INPUT");
        let message = body["error"]["message"].as_str().unwrap_or("");
        assert!(message.contains("Redirect Origin"), "saw {message}");
    }

    let (status, _) = put_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/spotify/settings",
        json!({
            "client_id": "test-client",
            "client_secret": "********",
            "enabled": true,
            "spotify_redirect_origin": "https://stated.example",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for peer in [TRUSTED, STRANGER] {
        assert_eq!(
            callback_addresses(&bundle.deps, &admin_id, peer).await,
            "https://stated.example/api/v3/acquire/spotify/auth/callback"
        );
    }
}

#[tokio::test]
async fn spotify_callback_rejects_error_and_replayed_state() {
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, _) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;

    // Provider-side error redirects plain.
    let response = anon_app(bundle.deps.clone())
        .oneshot(
            Request::get("/api/v3/acquire/spotify/auth/callback?error=access_denied")
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
            Request::get("/api/v3/acquire/spotify/auth/callback?code=good-code&state=nope")
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
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/auth/url",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let state = state_from_auth_url(body["auth_url"].as_str().unwrap_or(""));
    let response = anon_app(bundle.deps.clone())
        .oneshot(
            Request::get(format!(
                "/api/v3/acquire/spotify/auth/callback?code=bad-code&state={state}"
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
                "/api/v3/acquire/spotify/auth/callback?code=good-code&state={state}"
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
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, recorder) = serve_spotify().await.expect("mock serves");
    let _musicbrainz = serve_musicbrainz(&auth).await;
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &recorder, &user_id).await;

    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists/sp-playlist-1/import",
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

    // The `is_local` row never lands. MusicBrainz proves the first album
    // by its track's ISRC, so it carries its release group. The second
    // album's namesake carries another ISRC: a name alone proves nothing,
    // so it stays unresolved.
    let rows = playlist_rows(&bundle.playlists, &playlist_id).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].track_name, "Midnight Drive");
    assert_eq!(rows[0].artist_name, "Aurora Current");
    assert_eq!(rows[0].album_id.as_deref(), Some(NEON_MERIDIAN_GROUP));
    assert_eq!(
        rows[0].cover_url.as_deref(),
        Some(format!("/api/v3/covers/release-group/{NEON_MERIDIAN_GROUP}?size=250").as_str())
    );
    assert_eq!(rows[0].duration, Some(183.0));
    assert_eq!(rows[1].track_name, "Glass Tides");
    assert_eq!(rows[1].album_id, None);
    assert_eq!(rows[1].duration, Some(201.0));

    // The cover persisted best-effort.
    let cover = bundle
        .playlists
        .cover(&playlist_id)
        .await
        .expect("cover reads");
    assert!(cover.is_some());

    // Re-import answers the same record and replaces the rows wholesale.
    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists/sp-playlist-1/import",
        json!({"name": "Neon Nights"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlist_id"], playlist_id);
    let done = poll_job_done(bundle.deps.clone(), &user_id, "sp-playlist-1").await;
    assert_eq!(done["state"], "done");
    assert_eq!(
        playlist_rows(&bundle.playlists, &playlist_id).await.len(),
        2
    );

    // Playlists now carry the imported mapping.
    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlists"][0]["imported_playlist_id"], playlist_id);
}

/// A re-import that fails while its tracks are written leaves the playlist
/// as the last good import wrote it. The rows are replaced in one
/// transaction, so nothing of the failed write lands.
#[tokio::test]
async fn spotify_failed_reimport_keeps_the_old_tracks() {
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, recorder) = serve_spotify().await.expect("mock serves");
    let _musicbrainz = serve_musicbrainz(&auth).await;
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &recorder, &user_id).await;

    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists/sp-playlist-1/import",
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
    let before = track_ids(&bundle.playlists, &playlist_id).await;
    assert_eq!(before.len(), 2);

    // Fail the insert of the second track, after the first one is written.
    AcquireDb::new(auth.platform.db.clone(), auth.platform.clock.clone())
        .write("test.fail_second_track", |tx| {
            tx.execute(
                "CREATE TRIGGER fail_second_track BEFORE INSERT ON playlist_tracks \
                 WHEN NEW.track_name = 'Glass Tides' \
                 BEGIN SELECT RAISE(ABORT, 'injected'); END",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("trigger installs");

    let (status, _) = post_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists/sp-playlist-1/import",
        json!({"name": "Neon Nights"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let done = poll_job_done(bundle.deps.clone(), &user_id, "sp-playlist-1").await;
    assert_eq!(done["state"], "error", "{done}");
    assert_eq!(track_ids(&bundle.playlists, &playlist_id).await, before);
}

#[tokio::test]
async fn spotify_tracks_come_from_items_never_legacy_tracks() {
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, recorder) = serve_spotify().await.expect("mock serves");
    let _musicbrainz = serve_musicbrainz(&auth).await;
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &recorder, &user_id).await;

    let (status, _) = post_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists/sp-playlist-1/import",
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

/// A playlist entry whose track comes without its id is an answer the
/// import does not understand: the import fails and writes no rows, never
/// a playlist that quietly lost the track.
#[tokio::test]
async fn spotify_track_without_id_fails_the_import() {
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, recorder) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &recorder, &user_id).await;

    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        &format!("/api/v3/acquire/spotify/playlists/{SPOTIFY_PLAYLIST_NO_TRACK_ID}/import"),
        json!({"name": "Nameless"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let playlist_id = body["playlist_id"]
        .as_str()
        .expect("playlist id")
        .to_owned();

    let done = poll_job_done(bundle.deps, &user_id, SPOTIFY_PLAYLIST_NO_TRACK_ID).await;
    assert_eq!(done["state"], "error", "{done}");
    assert!(
        playlist_rows(&bundle.playlists, &playlist_id)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn spotify_cover_fetch_refuses_redirects_and_non_images() {
    let (_, _, covers, _) = serve_spotify().await.expect("mock serves");
    let client = SpotifyClient::new(
        http_client(),
        no_redirect_client(),
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
        Some(&covers.base_url),
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
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, recorder) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &recorder, &user_id).await;

    // Age the link row into expiry; the next list must refresh, not 401.
    let mut link = linked(&bundle.spotify_links, &user_id).await;
    link.expires_at_unix = 1;
    link.access_token = "stale-token".to_owned();
    bundle
        .spotify_links
        .upsert(&user_id, &link)
        .await
        .expect("store writes");

    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlists"].as_array().map(Vec::len), Some(2));
    let refreshed = linked(&bundle.spotify_links, &user_id).await;
    assert_eq!(refreshed.access_token, SPOTIFY_TOKEN);
    let calls = recorder.snapshot();
    assert!(
        calls
            .spotify_grants
            .iter()
            .any(|grant| grant == "refresh_token")
    );
}

/// The redirect URI must name a mounted route, and the v2 path that
/// migrated Spotify apps registered must keep answering.
#[tokio::test]
async fn spotify_callback_paths_are_mounted_in_the_app() {
    let app = common::TestApp::start("spotify-callbacks").await;
    for path in [SPOTIFY_CALLBACK_PATH, SPOTIFY_LEGACY_CALLBACK_PATH] {
        let uri = format!("{path}?error=access_denied");
        let (status, _, _) = app.raw("GET", &uri, &[], None).await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{path}");
    }
}

/// One user's stored link, which must exist.
async fn linked(links: &SqliteSpotifyLinks, user_id: &str) -> SpotifyConnection {
    links
        .get(user_id)
        .await
        .expect("store reads")
        .expect("link row")
}

/// An app the v2 import carried lists only the v2 callback. Without the
/// import's marker the exchange fails the way Spotify fails it; with the
/// marker, authorize and exchange both name the v2 URI and the link lands.
/// Disconnect then removes it.
#[tokio::test]
async fn spotify_v2_registered_app_links_and_disconnects() {
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, covers, recorder) = serve_spotify().await.expect("mock serves");
    let bundle = rig_for(
        &auth,
        &api.base_url,
        &accounts.base_url,
        Some(&covers.base_url),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    recorder.register_redirect_uri(&format!("http://app.test{SPOTIFY_LEGACY_CALLBACK_PATH}"));

    let (_, state) = authorize(bundle.deps.clone(), &user_id).await;
    let response = anon_app(bundle.deps.clone())
        .oneshot(
            Request::get(format!(
                "/api/v3/acquire/spotify/auth/callback?code=good-code&state={state}"
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

    auth.config
        .update_secret::<SpotifySection, ConfigError, _>(|stored| {
            let mut section = stored.into_inner();
            section.legacy_callback = true;
            Ok(section)
        })
        .await
        .expect("marker saves");
    link_spotify_via(
        bundle.deps.clone(),
        &recorder,
        &user_id,
        SPOTIFY_LEGACY_CALLBACK_PATH,
    )
    .await;
    linked(&bundle.spotify_links, &user_id).await;

    let response = authed_app(bundle.deps, &user_id, Role::User)
        .oneshot(
            Request::delete("/api/v3/me/connections/spotify")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let gone = bundle
        .spotify_links
        .get(&user_id)
        .await
        .expect("store reads");
    assert!(gone.is_none());
}

/// The SQLite stores keep single-use, expiring states with their redirect
/// URI; sealed links that a late token refresh cannot bring back; imported
/// playlists as real playlist rows, replaced wholesale on re-import.
#[tokio::test]
async fn spotify_sqlite_stores_persist_and_expire() {
    let db = droppedneedle_acquire::db::AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "Una", "user").await.expect("user row");

    let states = SqliteSpotifyStates::new(db.clone());
    let pending = PendingAuth {
        user_id: "u1".to_owned(),
        redirect_uri: "http://app.test/cb".to_owned(),
    };
    states
        .store_state("s1", &pending)
        .await
        .expect("store writes");
    assert_eq!(
        states.consume_state("s1").await.expect("store reads"),
        Some(pending.clone())
    );
    assert_eq!(states.consume_state("s1").await.expect("store reads"), None);
    db.write("test.expired_state", |tx| {
        tx.execute(
            "INSERT INTO spotify_auth_requests VALUES ('old', 'u1', 'http://app.test/cb', 1)",
            [],
        )?;
        Ok(())
    })
    .await
    .expect("seed writes");
    assert_eq!(
        states.consume_state("old").await.expect("store reads"),
        None
    );

    let crypto = Arc::new(
        droppedneedle_kernel::runtime_config::crypto::Crypto::from_key_bytes(&[7; 32])
            .expect("key"),
    );
    let links = SqliteSpotifyLinks::new(db.clone(), crypto);
    let link = SpotifyConnection {
        access_token: "ACCESS-SECRET".to_owned(),
        refresh_token: "REFRESH-SECRET".to_owned(),
        expires_at_unix: 1_900_000_000,
        username: "Una".to_owned(),
        spotify_user_id: "una-sp".to_owned(),
    };
    links.upsert("u1", &link).await.expect("store writes");
    assert_eq!(
        links.get("u1").await.expect("store reads"),
        Some(link.clone())
    );
    let stored: String = sqlx::query_scalar(
        "SELECT connection_data FROM user_connections WHERE user_id = 'u1' AND service = 'spotify'",
    )
    .fetch_one(db.db().sqlx())
    .await
    .expect("row reads");
    assert!(!stored.contains("SECRET"), "link is sealed at rest");
    assert!(links.remove("u1").await.expect("store writes"));
    assert!(
        !links
            .update_tokens_if_present("u1", &link)
            .await
            .expect("store writes")
    );
    assert_eq!(links.get("u1").await.expect("store reads"), None);

    let collections = droppedneedle_catalog::reads::collections::db::CollectionsDb::new(
        db.db().clone(),
        db.clock().clone(),
    );
    let bridge = CollectionsPlaylistBridge::new(
        droppedneedle_catalog::reads::collections::store::PlaylistStore::new(collections),
        db.clone(),
    );
    let id = bridge
        .create("u1", "Neon", "spotify:p1")
        .await
        .expect("creates");
    assert_eq!(
        bridge
            .create("u1", "Neon", "spotify:p1")
            .await
            .expect("creates"),
        id
    );
    let track = ImportedTrack {
        track_name: "Midnight Drive".to_owned(),
        artist_name: "Aurora Current".to_owned(),
        album_name: "Neon Meridian".to_owned(),
        album_id: String::new(),
        source_type: String::new(),
        track_number: Some(1),
        disc_number: Some(1),
        duration: Some(183),
        cover_url: None,
    };
    for _ in 0..2 {
        bridge
            .replace_tracks(&id, &[track.clone(), track.clone()])
            .await
            .expect("replaces");
    }
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playlist_tracks WHERE playlist_id = ?1")
            .bind(&id)
            .fetch_one(db.db().sqlx())
            .await
            .expect("count reads");
    assert_eq!(rows, 2);
    assert_eq!(
        bridge.source_refs_for("u1").await.expect("reads"),
        vec![("spotify:p1".to_owned(), id)]
    );
}

// --- Health smoke + per-source gate tests ---

async fn health_rig() -> (AuthRig, String, String, Rig) {
    let (auth, admin_id, user_id) = seed_rig().await;
    let bundle = rig_for(&auth, "http://127.0.0.1:1", "http://127.0.0.1:1", None);
    (auth, admin_id, user_id, bundle)
}

fn gate<'a>(body: &'a Value, source: &str) -> &'a Value {
    body["gates"]
        .as_array()
        .expect("gates array")
        .iter()
        .find(|gate| gate["source"] == source)
        .unwrap_or_else(|| panic!("{source} gate present"))
}

/// Save an enabled slskd connection at `url`.
async fn save_slskd(auth: &AuthRig, url: &str) {
    auth.config
        .save_secret(SlskdConnection {
            enabled: true,
            url: url.to_owned(),
            api_key: MOCK_API_KEY.into(),
            ..SlskdConnection::default()
        })
        .await
        .expect("slskd connection saves");
}

/// Serve the SABnzbd mock and save an enabled connection to it.
async fn serve_sabnzbd(auth: &AuthRig) -> (SabnzbdMock, tokio::task::JoinHandle<()>) {
    let mock = SabnzbdMock::new();
    let (url, server) = serve_loopback(mock.router())
        .await
        .expect("mock sabnzbd serves");
    auth.config
        .save_secret(DownloadClients {
            sabnzbd: SabnzbdConnection {
                enabled: true,
                url,
                api_key: "sab-key".into(),
                category: "music".to_owned(),
                ..SabnzbdConnection::default()
            },
        })
        .await
        .expect("sabnzbd connection saves");
    (mock, server)
}

/// A loopback address nothing listens on.
async fn closed_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn health_smoke_is_not_ready_with_no_source() {
    let (_auth, _, user_id, bundle) = health_rig().await;
    bundle.refresh_probes().await;

    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id, Role::User),
        "/api/v3/acquire/health",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "error");
    assert_eq!(body["ready"], false);
    assert_eq!(body["ready_via"], json!([]));
    assert_eq!(gate(&body, "slskd")["open"], false);
    assert_eq!(gate(&body, "sabnzbd")["open"], false);
    assert_eq!(gate(&body, "newznab")["open"], false);
    assert_eq!(gate(&body, "lidarr_import")["open"], false);
}

#[tokio::test]
async fn slskd_gate_greens_independently() {
    let (auth, _, user_id, bundle) = health_rig().await;
    let slskd = MockSlskd::start().await.expect("mock slskd starts");
    save_slskd(&auth, &slskd.base_url()).await;
    bundle.refresh_probes().await;

    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id, Role::User),
        "/api/v3/acquire/health",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gate(&body, "slskd")["open"], true);
    assert_eq!(gate(&body, "sabnzbd")["open"], false);
    assert_eq!(gate(&body, "newznab")["open"], false);
    assert_eq!(gate(&body, "lidarr_import")["open"], false);
    assert_eq!(body["ready_via"], json!(["soulseek"]));
}

#[tokio::test]
async fn health_degrades_when_an_enabled_path_is_down() {
    let (auth, _, user_id, bundle) = health_rig().await;
    save_slskd(&auth, &closed_url().await).await;
    let _sabnzbd = serve_sabnzbd(&auth).await;
    bundle.refresh_probes().await;

    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id, Role::User),
        "/api/v3/acquire/health",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["ready"], true);
    assert_eq!(body["ready_via"], json!(["usenet"]));
}

#[tokio::test]
async fn slskd_status_readable_by_any_user() {
    let (auth, _, user_id, bundle) = health_rig().await;
    let slskd = MockSlskd::start().await.expect("mock slskd starts");
    save_slskd(&auth, &slskd.base_url()).await;
    bundle.refresh_probes().await;

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/slskd/status",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], true);
    assert_eq!(body["reachable"], true);
    assert_eq!(body["version"], "0.25.1.0");

    let (status, _) = get_json(anon_app(bundle.deps), "/api/v3/acquire/slskd/status").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sabnzbd_status_is_admin_only_with_mount_hint() {
    let (auth, admin_id, user_id, bundle) = health_rig().await;
    let (sabnzbd, _server) = serve_sabnzbd(&auth).await;
    {
        let mut state = sabnzbd.state();
        state.categories = vec!["music".to_owned(), "audiobooks".to_owned()];
        state.complete_dir = "/completed".to_owned();
    }
    bundle.refresh_probes().await;

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/sabnzbd/status",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["valid"], true);
    assert_eq!(body["version"], "5.0.4");
    assert_eq!(body["categories"], json!(["music", "audiobooks"]));
    assert_eq!(body["complete_dir"], "/completed");

    let (status, body) = get_json(
        authed_app(bundle.deps, &user_id, Role::User),
        "/api/v3/acquire/sabnzbd/status",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");
}

/// A download setup the readiness table covers.
#[derive(Debug, Clone, Copy)]
enum SourceSetup {
    Nothing,
    SlskdOnly,
    SabnzbdOnlyAndDown,
    PluginOnly,
}

/// An idle plugin host with one enabled download-client plugin, `toy`.
fn plugin_host_with_download_client(auth: &AuthRig, dir: &std::path::Path) -> Arc<PluginHost> {
    let plugin_dir = dir.join("plugins/toy");
    std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "[plugin]\n\
         name = \"toy\"\n\
         version = \"1.0.0\"\n\
         api_version = 2\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"download_client\"]\n\
         [[capability]]\n\
         id = \"download_client\"\n\
         source = \"toy\"\n\
         display_name = \"Toy\"\n",
    )
    .expect("manifest");
    let platform = &auth.platform;
    let library = crate::common::library::library_core(
        platform,
        JobQueue::new(platform.db.clone(), platform.clock.clone()),
    );
    let host = PluginHost::new(
        dir.join("plugins"),
        platform.settings.clone(),
        Arc::new(crate::common::plugin_fakes::FakeLauncher::new()),
        droppedneedle_plugins::wiring::tick_store(platform),
        library,
    );
    host.load_all();
    host.update_settings("toy", true, std::collections::HashMap::new())
        .expect("plugin enabled");
    host
}

/// What each consumer of the readiness answers for one setup: the health
/// smoke, Discover's download-client flag, and whether a manual search
/// starts. Requests and the download queue read the same answer as
/// search, through the worker's readiness.
async fn readiness_answers(setup: SourceSetup) -> (Vec<bool>, Vec<String>) {
    use droppedneedle_acquire::downloads::worker::DownloadWorker;
    use droppedneedle_acquire::requests::quota::QuotaLedger;
    use droppedneedle_acquire::search_jobs::store::JobStore;
    use droppedneedle_acquire::search_jobs::{AlbumSearch, SearchJobError, SearchJobs};
    use droppedneedle_catalog::reads::discover::content::IntegrationSettings;

    let (auth, _, user_id) = seed_rig().await;
    let scratch = crate::common::ScratchDir::new("readiness-table");
    let mut _servers = Vec::new();
    let plugins = match setup {
        SourceSetup::Nothing => None,
        SourceSetup::SlskdOnly => {
            let slskd = MockSlskd::start().await.expect("mock slskd starts");
            save_slskd(&auth, &slskd.base_url()).await;
            _servers.push(slskd);
            None
        }
        SourceSetup::SabnzbdOnlyAndDown => {
            auth.config
                .save_secret(DownloadClients {
                    sabnzbd: SabnzbdConnection {
                        enabled: true,
                        url: closed_url().await,
                        api_key: "sab-key".into(),
                        ..SabnzbdConnection::default()
                    },
                })
                .await
                .expect("sabnzbd connection saves");
            None
        }
        SourceSetup::PluginOnly => Some(plugin_host_with_download_client(&auth, &scratch)),
    };
    let rig = rig_with(
        &auth,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
        None,
        plugins,
    );
    rig.refresh_probes().await;
    let readiness = rig.deps.readiness.clone();

    let (_, health) = get_json(
        authed_app(rig.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/health",
    )
    .await;
    let discover = IntegrationSettings::new(&auth.config, Arc::new(readiness.clone()))
        .status()
        .download_client;

    let platform = &auth.platform;
    let db = AcquireDb::new(platform.db.clone(), platform.clock.clone());
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let targets = Arc::new(Targets::new(journal.clone(), None));
    let worker = Arc::new(DownloadWorker::new(
        journal,
        rig.worker_settings.clone(),
        readiness.clone(),
        platform.events.clone(),
        Default::default(),
    ));
    let jobs = Arc::new(SearchJobs::new(
        JobStore::new(db.clone()),
        worker,
        targets,
        Arc::new(QuotaLedger::unlimited(db)),
        platform.events.clone(),
    ));
    let principal = test_principal(&user_id, Role::User, "sess-1", SessionKind::Standard);
    let search = match jobs
        .start(
            &principal,
            AlbumSearch {
                artist_name: "Aurora Current".to_owned(),
                album_title: "Neon Meridian".to_owned(),
                year: None,
                release_group_mbid: None,
                release_mbid: None,
            },
        )
        .await
    {
        Ok(_) => true,
        Err(SearchJobError::Conflict(_)) => false,
        Err(other) => panic!("search start failed: {other:?}"),
    };

    let reachable = readiness
        .now()
        .reachable
        .iter()
        .map(ToString::to_string)
        .collect();
    (
        vec![
            health["ready"].as_bool().expect("ready flag"),
            discover,
            search,
        ],
        reachable,
    )
}

/// Whether music can be acquired has one answer. A source counts once it
/// is turned on, whether or not it answers right now, and a plugin
/// download client counts like slskd and SABnzbd. The health smoke,
/// Discover's "set up a download client" prompt and manual search agree
/// on every setup.
#[tokio::test]
async fn every_consumer_agrees_on_whether_music_can_be_acquired() {
    let table = [
        (SourceSetup::Nothing, false, Vec::<&str>::new()),
        (SourceSetup::SlskdOnly, true, vec!["soulseek"]),
        (SourceSetup::SabnzbdOnlyAndDown, true, Vec::new()),
        (SourceSetup::PluginOnly, true, vec!["plugin:toy"]),
    ];
    for (setup, can_acquire, reachable) in table {
        let (answers, reached) = readiness_answers(setup).await;
        assert_eq!(
            answers,
            vec![can_acquire; answers.len()],
            "{setup:?}: health, Discover and search"
        );
        assert_eq!(reached, reachable, "{setup:?}: reachable sources");
    }
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
