//! Imports and acquisition health.
//!
//! Coverage of the Lidarr read-only import (monitored artists
//! become follows), the Spotify OAuth/playlist import plus settings rows,
//! the Spotify import jobs, and whether music can be acquired. Every test runs against the
//! in-repo loopback mocks or scripted probes; nothing touches a live
//! Lidarr, Spotify, slskd, SABnzbd, or indexer. The Spotify import runs on
//! the production stores over the rig's database and settings.

use crate::common::Probe;
use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use droppedneedle_acquire::imports;

use std::sync::Arc;
use std::time::Duration;

use crate::common::acquire::FixedAlbums;
use crate::common::bundles::{idle_host, quiet_ticks};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_acquire::clients::LiveClients;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::worker::WorkerSettings;
use droppedneedle_acquire::follows::Follows;
use droppedneedle_acquire::imports::jobs::ImportState;
use droppedneedle_acquire::probes::{LiveProbes, ProbeCache, refresh_probes};
use droppedneedle_acquire::readiness::SourceReadiness;
use droppedneedle_acquire::settings::{ConfigLidarrSettings, ConfigSpotifySettings};
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::test_support::imports_router;
use droppedneedle_acquire::wiring::ClientLives;
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_catalog::collections::db::CollectionsDb;
use droppedneedle_catalog::collections::models::PlaylistTrack;
use droppedneedle_catalog::collections::store::PlaylistStore;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::queue::{Handlers, JobQueue, State, start_runners};
use droppedneedle_kernel::runtime_config::Secret;
use droppedneedle_kernel::runtime_config::secret_sections::SpotifySettings as SpotifySection;
use droppedneedle_kernel::runtime_config::secret_sections::{
    DownloadClients, LidarrImportConnection, SabnzbdConnection, SlskdConnection,
};
use droppedneedle_kernel::runtime_config::sections::{MbSourceMode, MusicBrainzSettings};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_providers::images::check_spotify_cover_url;
use droppedneedle_providers::spotify::TokenGrant;
use droppedneedle_providers::{ProviderEndpoints, Providers, RequestPriority, ReqwestWire};
use droppedneedle_testkit::imports::{
    LIDARR_KEY, LIDARR_MBID_ALL, LIDARR_MBID_NONE, MockRecorder, SPOTIFY_PLAYLIST_NO_TRACK_ID,
    SPOTIFY_TOKEN, serve_lidarr, serve_spotify,
};
use droppedneedle_testkit::musicbrainz::MockMusicBrainz;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
use imports::handlers::ImportsDeps;
use imports::jobs::{SPEC, SpotifyImportJobs, SpotifyImports};
use imports::lidarr::LidarrClient;
use imports::models::{LidarrConnectionSettings, SpotifySettings};
use imports::spotify::{ImportedTrack, SpotifyImportService};
use imports::spotify_albums::AlbumResolver;
use imports::spotify_client::SpotifyClient;
use imports::spotify_oauth::{PendingAuth, SPOTIFY_CALLBACK_PATH, SpotifyConnection};
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

/// The shared providers over the real wire, with Spotify at the given
/// loopback mocks and every other provider closed.
fn providers_at(
    platform: &droppedneedle_kernel::platform::Platform,
    api_base: &str,
    accounts_base: &str,
    cover_cdn: Option<&str>,
) -> Providers {
    Providers::unpaced_at(
        Arc::new(ReqwestWire::new(&platform.http)),
        ProviderEndpoints {
            spotify_api: api_base.to_owned(),
            spotify_accounts: accounts_base.to_owned(),
            spotify_cover_cdn: cover_cdn.map(str::to_owned),
            ..ProviderEndpoints::closed()
        },
    )
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
    plugins: Arc<PluginHost>,
    db: Db,
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
    rig_with(auth, api_base, accounts_base, cover_cdn, None, None)
}

/// [`rig_for`] with plugins (a host that launches nothing when none is
/// given), and with the import job reading its Spotify app settings from
/// `job_settings` instead of the routes' store.
fn rig_with(
    auth: &AuthRig,
    api_base: &str,
    accounts_base: &str,
    cover_cdn: Option<&str>,
    plugins: Option<Arc<PluginHost>>,
    job_settings: Option<Arc<droppedneedle_kernel::runtime_config::ConfigStore>>,
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
    let providers = providers_at(platform, api_base, accounts_base, cover_cdn);
    let resolver = Arc::new(AlbumResolver::new(&providers, platform.settings.live()));
    let spotify_client = SpotifyClient::new(&providers);
    let service = Arc::new(SpotifyImportService::new(
        spotify_client.clone(),
        job_settings.map_or_else(
            || spotify_settings.clone(),
            |store| Arc::new(ConfigSpotifySettings::new(store)),
        ),
        spotify_links.clone(),
        spotify_playlists.clone(),
        resolver.clone(),
    ));
    let import_jobs = SpotifyImportJobs::new(service, platform.events.clone());
    let queue = JobQueue::new(platform.db.clone(), platform.clock.clone());
    let spotify_imports = SpotifyImports::new(platform.db.clone(), queue.clone());
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    let plugins = plugins.unwrap_or_else(|| {
        idle_host(
            platform,
            crate::common::library::library_core(platform, queue.clone()),
        )
    });
    let clients = Arc::new(LiveClients::new(
        platform.http.shared().clone(),
        droppedneedle_providers::Providers::closed().newznab,
        ClientLives::new(&platform.settings),
        platform.config.slskd_downloads_path.clone(),
        journal.clone(),
        Arc::new(Targets::new(journal, Arc::new(FixedAlbums::default()))),
        plugins.clone(),
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
        plugins,
        db: platform.db.clone(),
    }
}

/// The rows on one of the user's playlists, in order.
async fn playlist_rows(
    playlists: &PlaylistStore,
    user_id: &str,
    playlist_id: &str,
) -> Vec<PlaylistTrack> {
    playlists
        .owned_by(user_id)
        .tracks(playlist_id)
        .await
        .expect("rows read")
}

/// The row ids on one of the user's playlists, in order.
async fn track_ids(playlists: &PlaylistStore, user_id: &str, playlist_id: &str) -> Vec<String> {
    playlist_rows(playlists, user_id, playlist_id)
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

// --- Spotify covers ---

/// A Spotify cover is fetched only from the CDN over https, never through a
/// redirect, and only when the answer is an image.
#[tokio::test]
async fn spotify_covers_come_only_from_the_cdn_as_images() {
    let allowed = |url: &str| check_spotify_cover_url(url, None).is_some();
    assert!(allowed("https://i.scdn.co/image/abc"), "allowlist: the CDN");
    assert!(
        !allowed("http://i.scdn.co/image/abc"),
        "allowlist: plain http"
    );
    assert!(
        !allowed("https://evil.example.com/x.jpg"),
        "allowlist: another host"
    );
    assert!(!allowed("not a url"), "allowlist: not a url");
    // A Spotify answer naming a local service is never fetched.
    assert!(!allowed("http://127.0.0.1:1/x"), "allowlist: loopback");
    assert!(!allowed("https://localhost/x"), "allowlist: localhost");

    let (_, _, covers, _) = serve_spotify().await.expect("mock serves");
    let auth = AuthRig::new("imports").await;
    let providers = providers_at(
        &auth.platform,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
        Some(&covers.base_url),
    );
    let client = SpotifyClient::new(&providers);
    let fetch = |url: String| {
        let client = client.clone();
        async move {
            client
                .fetch_cover(&url, RequestPriority::BackgroundSync)
                .await
                .expect("fetch works")
        }
    };

    let direct = format!("{}/covers/neon.jpg", covers.base_url);
    let fetched = fetch(direct).await;
    assert!(fetched.is_some(), "fetch: a direct image");
    assert_eq!(
        fetched.map(|(_, content_type)| content_type).as_deref(),
        Some("image/jpeg"),
        "fetch: a direct image"
    );

    let redirect = format!("{}/covers/redirect.jpg", covers.base_url);
    assert_eq!(fetch(redirect).await, None, "fetch: a redirect");

    let text = format!("{}/covers/not-an-image.jpg", covers.base_url);
    assert_eq!(fetch(text).await, None, "fetch: not an image");
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

/// One mock Lidarr: an admin's import follows once and a re-import never
/// re-arms, and Lidarr only ever sees the two read-only GETs with the stored
/// key.
#[tokio::test]
async fn lidarr_import_follows_once_and_reads_only() {
    let (auth, admin_id, _) = seed_rig().await;
    let (server, recorder) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(&auth, "http://127.0.0.1:1", "http://127.0.0.1:1", None);
    seed_lidarr(&auth, &server.base_url).await;
    let payload = json!({"selected_mbids": [LIDARR_MBID_ALL, LIDARR_MBID_NONE]});

    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/import",
        payload.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "first import");
    assert_eq!(body["imported"], 2, "first import");
    assert_eq!(body["already_following"], 0, "first import");
    assert_eq!(body["skipped_invalid"], 0, "first import");
    assert_eq!(body["auto_download_enabled"], 1, "first import");

    // Re-import never re-arms or double-counts: disjoint pre-existing subset.
    let (status, body) = post_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/import",
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-import");
    assert_eq!(body["imported"], 0, "re-import");
    assert_eq!(body["already_following"], 2, "re-import");
    assert_eq!(body["auto_download_enabled"], 0, "re-import");

    // Candidates now read as already following.
    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/artists",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "candidates");
    assert_eq!(body["artists"][0]["already_following"], true, "candidates");

    // The connection test is the last call; every call was a read-only GET.
    let _ = post_json(
        authed_app(bundle.deps, &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/test",
        json!({"url": server.base_url, "api_key": LIDARR_KEY}),
    )
    .await;
    let calls = recorder.snapshot();
    assert!(!calls.lidarr_paths.is_empty(), "read-only calls");
    for path in &calls.lidarr_paths {
        assert!(
            path == "/api/v1/system/status" || path == "/api/v1/artist",
            "sanctioned paths only, saw {path}"
        );
    }
    for key in &calls.lidarr_keys {
        assert_eq!(key, LIDARR_KEY, "read-only calls");
    }
}

/// A settings store in `dir` whose `section` was copied in from a
/// `config.json` unvalidated, as `stored`, a shape it does not decode to.
fn unreadable_settings(
    dir: &std::path::Path,
    section: &str,
    stored: Value,
) -> Arc<droppedneedle_kernel::runtime_config::ConfigStore> {
    std::fs::write(
        dir.join("config.json"),
        json!({ section: stored }).to_string(),
    )
    .expect("config.json");
    Arc::new(droppedneedle_acquire::test_support::scratch_settings(dir).expect("settings open"))
}

/// The bespoke Lidarr settings routes. Settings that do not read are never
/// an empty connection: the read, a save and a probe with the stored key
/// answer the settings error and Lidarr is never called. Readable ones
/// normalize the pasted address, mask the key, and a save of the mask
/// keeps the stored key for the probe.
#[tokio::test]
async fn lidarr_settings_mask_keep_and_refuse_when_unreadable() {
    let (auth, admin_id, _) = seed_rig().await;
    let (server, recorder) = serve_lidarr().await.expect("mock serves");
    let bundle = rig_for(&auth, "http://127.0.0.1:1", "http://127.0.0.1:1", None);

    let dir = crate::common::ScratchDir::new("lidarr-unreadable");
    let mut unreadable = bundle.deps.clone();
    unreadable.lidarr_settings = Arc::new(ConfigLidarrSettings::new(unreadable_settings(
        &dir,
        "lidarr_import",
        json!({"url": 7}),
    )));
    let broken = || authed_app(unreadable.clone(), &admin_id, Role::Admin);
    let (status, body) = get_json(broken(), "/api/v3/acquire/lidarr-import/config").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unreadable read: {body}");
    assert_eq!(body["error"]["code"], "INVALID_INPUT", "unreadable read");
    let (status, _) = put_json(
        broken(),
        "/api/v3/acquire/lidarr-import/config",
        json!({"url": server.base_url, "api_key": "********"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unreadable save");
    let (status, body) = post_json(
        broken(),
        "/api/v3/acquire/lidarr-import/test",
        json!({"url": server.base_url, "api_key": "********"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unreadable probe: {body}");
    assert_eq!(body["error"]["code"], "INVALID_INPUT", "unreadable probe");
    assert!(
        recorder.snapshot().lidarr_paths.is_empty(),
        "unreadable: Lidarr is never called"
    );

    let pasted = format!("{}/api/v1", server.base_url);
    let (status, body) = put_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/config",
        json!({"url": pasted, "api_key": LIDARR_KEY}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save");
    assert_eq!(body["url"], server.base_url, "save normalizes the address");
    assert_eq!(body["api_key"], "********", "save masks the key");

    // Saving the masked sentinel preserves the stored key: the probe still
    // authenticates afterwards.
    let (status, _) = put_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/config",
        json!({"url": server.base_url, "api_key": "********"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save of the mask");
    let (status, body) = post_json(
        authed_app(bundle.deps, &admin_id, Role::Admin),
        "/api/v3/acquire/lidarr-import/test",
        json!({"url": server.base_url, "api_key": "********"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "probe with the mask");
    assert_eq!(body["valid"], true, "probe with the mask");
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
    // The accounts host is the mock's, so the page is under its address.
    assert!(auth_url.starts_with("http://127.0.0.1:"));
    assert!(auth_url.contains("/authorize?"));
    assert!(auth_url.contains("test-client"));
    let state = state_from_auth_url(&auth_url);
    (auth_url, state)
}

/// Link `user_id` through the real authorize + callback flow. Authorize
/// must name the URI the mock app lists, as Spotify requires.
async fn link_spotify(bundle_deps: ImportsDeps, recorder: &MockRecorder, user_id: &str) {
    let (auth_url, state) = authorize(bundle_deps.clone(), user_id).await;
    let listed = recorder
        .registered_redirect_uri()
        .replace(':', "%3A")
        .replace('/', "%2F");
    assert!(
        auth_url.contains(&format!("redirect_uri={listed}&")),
        "authorize names an unlisted URI: {auth_url}"
    );
    let callback = format!("{SPOTIFY_CALLBACK_PATH}?code=good-code&state={state}");
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

/// Wall-clock bound on a wait for the import runner. The rig's clock is a
/// ManualClock, so tokio's timers run in real time.
const PATIENCE: Duration = Duration::from_secs(30);

/// Re-reads with `read` after each database commit until it answers, so a
/// wait follows the job row instead of a timer. A settings save wakes the
/// queue through a write the commit count may not see, so a short tick
/// re-reads too.
async fn after_commits<T, Fut>(db: &Db, what: &str, mut read: impl FnMut() -> Fut) -> T
where
    Fut: std::future::Future<Output = Option<T>>,
{
    let mut commits = db.commits();
    let wait = async {
        loop {
            commits.borrow_and_update();
            if let Some(found) = read().await {
                return found;
            }
            tokio::select! {
                changed = commits.changed() => changed.expect("the write lane is open"),
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    };
    tokio::time::timeout(PATIENCE, wait)
        .await
        .unwrap_or_else(|_| panic!("{what} within {PATIENCE:?}"))
}

/// The import's state once it stops running.
async fn poll_job_done(rig: &Rig, user_id: &str, spotify_id: &str) -> ImportState {
    let imports = &rig.deps.spotify_imports;
    after_commits(&rig.db, "the import finishes", move || async move {
        match imports.state(user_id, spotify_id).await.expect("job reads") {
            Some(ImportState::Running) | None => None,
            Some(done) => Some(done),
        }
    })
    .await
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

/// The Spotify app settings round trip, and the callback address: a bad
/// origin is refused, the address follows the one the browser used as a
/// trusted proxy reports it, forwarded headers from anyone else count for
/// nothing, and a stated origin wins.
#[tokio::test]
async fn spotify_callback_address_follows_the_origin_or_a_trusted_proxy() {
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

    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/spotify/settings",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings before the app is saved");
    assert_eq!(body["enabled"], false, "settings before the app is saved");

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
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad origin");
    assert_eq!(body["error"]["code"], "INVALID_INPUT", "bad origin");

    // The plain request's host, with no proxy in front.
    let (status, body) = get_json(
        authed_app(bundle.deps.clone(), &admin_id, Role::Admin),
        "/api/v3/acquire/spotify/redirect-uri",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "plain host");
    assert_eq!(
        body["redirect_uri"], "http://app.test/api/v3/acquire/spotify/auth/callback",
        "plain host"
    );

    assert_eq!(
        callback_addresses(&bundle.deps, &admin_id, TRUSTED).await,
        "https://music.example/api/v3/acquire/spotify/auth/callback",
        "trusted proxy"
    );
    assert_eq!(
        callback_addresses(&bundle.deps, &admin_id, STRANGER).await,
        "http://app.test/api/v3/acquire/spotify/auth/callback",
        "stranger"
    );

    let mut mounted = bundle.deps.clone();
    mounted.base_path = "/needle".to_owned();
    assert_eq!(
        callback_addresses(&mounted, &admin_id, TRUSTED).await,
        "https://music.example/needle/api/v3/acquire/spotify/auth/callback",
        "trusted proxy under a base path"
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
        assert_eq!(status, StatusCode::BAD_REQUEST, "no host: {path}");
        assert_eq!(body["error"]["code"], "INVALID_INPUT", "no host: {path}");
        let message = body["error"]["message"].as_str().unwrap_or("");
        assert!(
            message.contains("Redirect Origin"),
            "no host: saw {message}"
        );
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
    assert_eq!(status, StatusCode::OK, "stated origin");
    for peer in [TRUSTED, STRANGER] {
        assert_eq!(
            callback_addresses(&bundle.deps, &admin_id, peer).await,
            "https://stated.example/api/v3/acquire/spotify/auth/callback",
            "stated origin from {peer}"
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

/// An import answers at once, then fills the playlist from the `/items`
/// listing (never the legacy `/tracks`), and a name alone never resolves
/// an album.
#[tokio::test]
async fn spotify_import_populates_tracks_from_items() {
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

    let done = poll_job_done(&bundle, &user_id, "sp-playlist-1").await;
    assert_eq!(done, ImportState::Done);
    assert_eq!(
        playlist_rows(&bundle.playlists, &user_id, &playlist_id)
            .await
            .len(),
        2
    );

    // The `is_local` row never lands. MusicBrainz proves the first album
    // by its track's ISRC, so it carries its release group. The second
    // album's namesake carries another ISRC: a name alone proves nothing,
    // so it stays unresolved.
    let rows = playlist_rows(&bundle.playlists, &user_id, &playlist_id).await;
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
        .owned_by(&user_id)
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
    let done = poll_job_done(&bundle, &user_id, "sp-playlist-1").await;
    assert_eq!(done, ImportState::Done);
    assert_eq!(
        playlist_rows(&bundle.playlists, &user_id, &playlist_id)
            .await
            .len(),
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

    let calls = recorder.snapshot();
    assert!(
        calls
            .spotify_paths
            .iter()
            .any(|path| path.ends_with("/items")),
        "tracks come from /items"
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
            .any(|grant| grant == "authorization_code"),
        "the link used the authorization code grant"
    );
}

/// Spotify app settings that do not read are never an empty app. Starting
/// an import answers the settings error, queues nothing and calls Spotify
/// for nothing. An import queued before they stopped reading waits for
/// them without spending its attempts, and finishes once they are saved.
#[tokio::test]
async fn unreadable_spotify_settings_refuse_new_imports_and_hold_queued_ones() {
    let (auth, admin_id, user_id) = seed_rig().await;
    let (api, accounts, _covers, recorder) = serve_spotify().await.expect("mock serves");
    let _musicbrainz = serve_musicbrainz(&auth).await;
    let dir = crate::common::ScratchDir::new("spotify-import-waits");
    // A secret that is not sealed: the section decodes but does not open,
    // and a save with a new secret mends it.
    let broken = unreadable_settings(
        &dir,
        "spotify_settings",
        json!({"client_id": "test-client", "client_secret": "not sealed"}),
    );
    let bundle = rig_with(
        &auth,
        &api.base_url,
        &accounts.base_url,
        None,
        None,
        Some(broken.clone()),
    );
    seed_spotify_app(bundle.deps.clone(), &admin_id).await;
    link_spotify(bundle.deps.clone(), &recorder, &user_id).await;

    // A new import, with the routes' settings unreadable, is refused.
    let before = recorder.snapshot();
    let refusing_dir = crate::common::ScratchDir::new("spotify-unreadable");
    let mut refusing = bundle.deps.clone();
    refusing.spotify_settings = Arc::new(ConfigSpotifySettings::new(unreadable_settings(
        &refusing_dir,
        "spotify_settings",
        json!({"enabled": "yes"}),
    )));
    let (status, body) = post_json(
        authed_app(refusing.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists/sp-playlist-1/import",
        json!({"name": "Neon Nights"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "new import: {body}");
    assert_eq!(body["error"]["code"], "INVALID_INPUT", "new import");
    let state = refusing
        .spotify_imports
        .state(&user_id, "sp-playlist-1")
        .await
        .expect("job reads");
    assert_eq!(state, None, "new import: nothing queued");
    let after = recorder.snapshot();
    assert_eq!(
        after.spotify_paths, before.spotify_paths,
        "new import: Spotify not called"
    );
    assert_eq!(
        after.spotify_grants, before.spotify_grants,
        "new import: no token asked"
    );

    // A queued import, with only the job's settings unreadable.
    let (status, _) = post_json(
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists/sp-playlist-1/import",
        json!({"name": "Neon Nights"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "queued import");

    // The job is claimed and held while the settings do not read.
    let queue = JobQueue::new(auth.platform.db.clone(), auth.platform.clock.clone());
    let key = imports::jobs::import_job_key(&user_id, "sp-playlist-1");
    let (queue_ref, key_ref) = (&queue, key.as_str());
    let latest_in = move |wanted: State| async move {
        queue_ref
            .latest(imports::jobs::KIND, key_ref)
            .await
            .expect("job reads")
            .filter(|row| row.state == wanted)
    };
    after_commits(&bundle.db, "the import is claimed", || {
        latest_in(State::Running)
    })
    .await;
    let state = bundle
        .deps
        .spotify_imports
        .state(&user_id, "sp-playlist-1")
        .await
        .expect("job reads");
    assert_eq!(state, Some(ImportState::Running), "queued import is held");

    // A save wakes it: it goes back with its attempt returned, then runs.
    broken
        .save_secret(SpotifySection {
            client_id: "test-client".to_owned(),
            client_secret: Secret::new("test-secret".to_owned()),
            enabled: true,
            spotify_redirect_origin: String::new(),
        })
        .await
        .expect("settings save");
    // It goes back with its attempt returned. Due at once it may run
    // before the wait sees it queued, so either is the step after the hold.
    let after = after_commits(&bundle.db, "the job goes back", || async {
        queue_ref
            .latest(imports::jobs::KIND, key_ref)
            .await
            .expect("job reads")
            .filter(|row| matches!(row.state, State::Queued | State::Done))
    })
    .await;
    if after.state == State::Queued {
        assert_eq!(after.attempts, 0, "the wait spent no attempt");
        auth.clock().advance(10_000);
    }
    let done = poll_job_done(&bundle, &user_id, "sp-playlist-1").await;
    assert_eq!(done, ImportState::Done, "queued import finishes");
    let finished = queue_ref
        .latest(imports::jobs::KIND, key_ref)
        .await
        .expect("job reads")
        .expect("the job row");
    assert_eq!(finished.attempts, 1, "the wait spent no attempt");
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
    let done = poll_job_done(&bundle, &user_id, "sp-playlist-1").await;
    assert_eq!(done, ImportState::Done);
    let before = track_ids(&bundle.playlists, &user_id, &playlist_id).await;
    assert_eq!(before.len(), 2);

    // Fail the insert of the second track, after the first one is written.
    AcquireDb::new(auth.platform.db.clone(), auth.platform.clock.clone())
        .write("test.fail_second_track", |tx| {
            tx.execute(
                "CREATE TRIGGER fail_second_track BEFORE INSERT ON catalog_playlist_tracks \
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
    let done = poll_job_done(&bundle, &user_id, "sp-playlist-1").await;
    assert!(matches!(done, ImportState::Error { .. }), "{done:?}");
    assert_eq!(
        track_ids(&bundle.playlists, &user_id, &playlist_id).await,
        before
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

    let done = poll_job_done(&bundle, &user_id, SPOTIFY_PLAYLIST_NO_TRACK_ID).await;
    assert!(matches!(done, ImportState::Error { .. }), "{done:?}");
    assert!(
        playlist_rows(&bundle.playlists, &user_id, &playlist_id)
            .await
            .is_empty()
    );
}

/// One user's stored link, which must exist.
async fn linked(links: &SqliteSpotifyLinks, user_id: &str) -> SpotifyConnection {
    links
        .get(user_id)
        .await
        .expect("store reads")
        .expect("link row")
}

/// A stored link's lifecycle: an expired token refreshes on the next call
/// instead of failing, then the account disconnects and its link goes.
#[tokio::test]
async fn spotify_link_refreshes_and_disconnects() {
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
        authed_app(bundle.deps.clone(), &user_id, Role::User),
        "/api/v3/acquire/spotify/playlists",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "refresh");
    assert_eq!(
        body["playlists"].as_array().map(Vec::len),
        Some(2),
        "refresh"
    );
    let refreshed = linked(&bundle.spotify_links, &user_id).await;
    assert_eq!(refreshed.access_token, SPOTIFY_TOKEN, "refresh");
    let calls = recorder.snapshot();
    assert!(
        calls
            .spotify_grants
            .iter()
            .any(|grant| grant == "refresh_token"),
        "refresh: the refresh grant was used"
    );

    let response = authed_app(bundle.deps, &user_id, Role::User)
        .oneshot(
            Request::delete("/api/v3/me/connections/spotify")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "disconnect");
    let gone = bundle
        .spotify_links
        .get(&user_id)
        .await
        .expect("store reads");
    assert!(gone.is_none(), "disconnect: the link is gone");
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
    let stored: String = db.db().one(
        "SELECT connection_data FROM user_connections WHERE user_id = 'u1' AND service = 'spotify'",
        [],
    );
    assert!(!stored.contains("SECRET"), "link is sealed at rest");
    assert!(links.remove("u1").await.expect("store writes"));
    assert!(
        !links
            .update_tokens_if_present("u1", &link)
            .await
            .expect("store writes")
    );
    assert_eq!(links.get("u1").await.expect("store reads"), None);

    let collections = droppedneedle_catalog::collections::db::CollectionsDb::new(
        db.db().clone(),
        db.clock().clone(),
    );
    let bridge = CollectionsPlaylistBridge::new(
        droppedneedle_catalog::collections::store::PlaylistStore::new(collections),
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
            .replace_tracks("u1", &id, &[track.clone(), track.clone()])
            .await
            .expect("replaces");
    }
    let rows: i64 = db.db().one(
        "SELECT COUNT(*) FROM catalog_playlist_tracks WHERE playlist_id = ?1",
        rusqlite::params![&id],
    );
    assert_eq!(rows, 2);
    assert_eq!(
        bridge.source_refs_for("u1").await.expect("reads"),
        vec![("spotify:p1".to_owned(), id)]
    );
}

// --- Readiness ---

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

/// A loopback address nothing listens on.
async fn closed_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
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

/// What each consumer of the readiness answers for one setup: the
/// readiness itself, Discover's download-client flag, and whether a manual search
/// starts. Requests and the download queue read the same answer as
/// search, through the worker's readiness.
async fn readiness_answers(setup: SourceSetup) -> Vec<bool> {
    use droppedneedle_acquire::downloads::worker::DownloadWorker;
    use droppedneedle_acquire::requests::quota::QuotaLedger;
    use droppedneedle_acquire::search_jobs::store::JobStore;
    use droppedneedle_acquire::search_jobs::{AlbumSearch, SearchJobError, SearchJobs};
    use droppedneedle_catalog::discover::content::IntegrationSettings;

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
        None,
    );
    rig.refresh_probes().await;
    let readiness = SourceReadiness::new(rig.worker_settings.clone());

    let discover = IntegrationSettings::new(&auth.config, Arc::new(readiness.clone()))
        .status()
        .download_client;

    let platform = &auth.platform;
    let db = AcquireDb::new(platform.db.clone(), platform.clock.clone());
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let targets = Arc::new(Targets::new(
        journal.clone(),
        Arc::new(FixedAlbums::default()),
    ));
    let (library, _) = crate::common::library::library_at(platform);
    let landing = Arc::new(droppedneedle_acquire::landing::LandingService::new(
        journal.clone(),
        Arc::new(
            droppedneedle_acquire::landing::library::LibraryLanding::new(
                &library,
                library.imports.releases.clone(),
            ),
        ),
        platform.settings.live(),
        scratch.join("held"),
    ));
    let worker = Arc::new(DownloadWorker::new(
        journal,
        rig.worker_settings.clone(),
        readiness.clone(),
        platform.events.clone(),
        Default::default(),
        droppedneedle_acquire::downloads::worker::WorkerPorts {
            landing,
            plugins: rig.plugins.clone(),
            targets: targets.clone(),
        },
    ));
    let jobs = Arc::new(SearchJobs::new(
        JobStore::new(db.clone()),
        worker,
        targets,
        Arc::new(QuotaLedger::unlimited(db).expect("quota ledger")),
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

    vec![readiness.can_acquire(), discover, search]
}

/// Whether music can be acquired has one answer. A source counts once it
/// is turned on, whether or not it answers right now, and a plugin
/// download client counts like slskd and SABnzbd. Discover's "set up a download client" prompt and manual search agree
/// on every setup.
#[tokio::test]
async fn every_consumer_agrees_on_whether_music_can_be_acquired() {
    let table = [
        (SourceSetup::Nothing, false),
        (SourceSetup::SlskdOnly, true),
        (SourceSetup::SabnzbdOnlyAndDown, true),
        (SourceSetup::PluginOnly, true),
    ];
    for (setup, can_acquire) in table {
        let answers = readiness_answers(setup).await;
        assert_eq!(
            answers,
            vec![can_acquire; answers.len()],
            "{setup:?}: readiness, Discover and search"
        );
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
