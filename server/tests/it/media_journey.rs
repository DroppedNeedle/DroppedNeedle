//! Media journeys through the real app (`create_app` over a scratch SQLite
//! database) against the in-repo mock Plex server: save a Plex connection,
//! browse its albums, stream whole bytes, seek a range, then start,
//! progress and stop playback and find the play in history.

use crate::common::ScratchDir;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle::auth::prod::ProdAuth;
use droppedneedle::auth::users::stores::SystemClock;
use droppedneedle::auth::wiring::AuthSetup;
use droppedneedle::config::DEFAULT_PORT;
use droppedneedle::db::{DbConfig, DbRuntime, open_runtime};
use droppedneedle::http_client::HttpClientFactory;
use droppedneedle::ids::{IdGenerator, UuidGenerator};
use droppedneedle::remotes::mocks::{PLEX_TOKEN, canned_stream_bytes, serve_plex};
use droppedneedle::runtime_config::sections::SecuritySettings;
use droppedneedle::runtime_config::{
    ConfigStore, Crypto, Secret, secret_sections::WrappedSettings,
};
use droppedneedle::{AppConfig, AppState, create_app, reads::ReadsSetup};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// Fixed host for every request.
const HOST: &str = "e2e.test";
/// Wrapped shared secret saved into every scratch config.
const TEST_WRAPPED_KEY: &str = "e2e-wrapped-key-1";

/// One scratch deployment: migrated database, production adapters, config.
struct E2e {
    /// Held, never read: dropping it would close the pool out from under
    /// the adapters.
    #[allow(dead_code)]
    runtime: DbRuntime,
    bundle: ProdAuth,
    store: Arc<ConfigStore>,
    crypto: Arc<Crypto>,
    http: HttpClientFactory,
    ids: Arc<UuidGenerator>,
    clock: Arc<SystemClock>,
    db_path: std::path::PathBuf,
    /// Declared last so the database closes before the directory goes.
    _scratch: ScratchDir,
}

impl E2e {
    async fn open(tag: &str) -> Self {
        let dir = ScratchDir::new(&format!("media-journey-{tag}"));
        let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
            .await
            .expect("scratch runtime opens");
        let crypto = Arc::new(Crypto::from_key_bytes(&[7u8; 32]).expect("test key"));
        let store = Arc::new(
            ConfigStore::open(
                &dir.join("config.json"),
                Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            )
            .expect("scratch config opens"),
        );
        let mut security: SecuritySettings = store.get().expect("security section reads");
        security.hibp_check = false;
        store.save(security).expect("hibp switch saves");
        store
            .save_secret(WrappedSettings {
                api_key: Secret::new(TEST_WRAPPED_KEY),
            })
            .expect("wrapped key saves");

        let ids = Arc::new(UuidGenerator);
        let clock = Arc::new(SystemClock);
        let bundle = ProdAuth::new(
            runtime.pool(),
            runtime.lane(),
            Arc::clone(&crypto),
            Arc::clone(&ids) as Arc<dyn IdGenerator>,
            Arc::clone(&clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            &dir.join("avatars"),
        );
        let http = HttpClientFactory::new().expect("http factory builds");
        let db_path = dir.join("app.db");
        Self {
            runtime,
            bundle,
            store,
            crypto,
            http,
            ids,
            clock,
            db_path,
            _scratch: dir,
        }
    }

    /// One router over the scratch database. The journey reuses a single
    /// build so the in-memory remote connections persist across calls.
    fn router(&self) -> Router {
        let auth = AuthSetup::build(
            self.bundle.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.crypto),
            &self.http,
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            "",
        )
        .expect("prod auth bundle builds");
        let reads = ReadsSetup::build(
            self.runtime.pool(),
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            crate::common::reads_inputs(
                Arc::clone(&self.store),
                self.db_path.parent().expect("db dir"),
                self.runtime.lane().clone(),
            ),
            None,
        )
        .with_collections(droppedneedle::reads::collections::db::CollectionsDb::new(
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
        ));
        let mut app_config = AppConfig::new(DEFAULT_PORT);
        app_config.root_app_dir = self
            .db_path
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let library = droppedneedle::library::wiring::LibrarySetup::for_tests(
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
        )
        .expect("library bundle builds");
        let (media, _worker) = droppedneedle::media::MediaSetup::build(
            &self.db_path,
            &app_config,
            auth.users.clone(),
            Arc::clone(&self.crypto),
            &self.http,
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Some(library.root_source()),
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
            Arc::clone(&self.store),
            Arc::new(droppedneedle::remotes::adapter::PlaylistImportSink::new(
                reads.collections.clone(),
            )),
            &droppedneedle::bootstrap::Outbound::live(),
        )
        .expect("media bundle builds");
        let mut reads = reads;
        let acquire = droppedneedle::acquire::AcquireSetup::build(
            droppedneedle::acquire::db::AcquireDb::from_runtime(&self.runtime),
            &app_config,
            auth.users.clone(),
            &self.http,
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.store),
            &mut reads.collections,
            &droppedneedle::bootstrap::Outbound::live(),
        )
        .expect("acquire bundle builds");
        let compat = droppedneedle::compat::CompatSetup::build(
            droppedneedle::compat::setup::CompatDeps::over_reads(
                auth.users.clone(),
                Arc::clone(&self.crypto),
                media.playback.clone(),
                media.stream.engine.clone(),
                library.clone(),
                &reads,
                droppedneedle::compat::settings::LiveSettings::from_config(Arc::clone(&self.store)),
            ),
        );
        let providers = Arc::new(droppedneedle::providers::Providers::with_memory_cache());
        let admin = droppedneedle::admin::AdminSetup::for_tests(
            auth.users.clone(),
            acquire.requests.quota.clone(),
            Arc::new(droppedneedle::providers::InMemoryProviderCache::new()),
            providers.clone(),
        );
        let jobs = droppedneedle::jobs::wiring::JobsSetup::for_tests(auth.users.clone());
        let plugins = droppedneedle::plugins::wiring::PluginsSetup::for_tests(
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            jobs.registry().clone(),
        )
        .expect("test plugins bundle builds");
        let settings = droppedneedle::settings::wiring::SettingsSetup::for_tests(
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            auth.users.clone(),
        )
        .expect("test settings bundle builds");
        let state = AppState::new(
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            self.http.clone(),
            app_config,
            auth,
            reads,
            providers,
            media,
            acquire,
            library,
            compat,
            admin,
            settings,
            jobs,
            plugins,
            droppedneedle::concerts::ConcertsSetup::unwired(),
        );
        create_app(state)
    }
}

/// One JSON request through the real app.
async fn call(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value, HeaderMap) {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = builder.header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .oneshot(builder.body(body).expect("request builds"))
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is json")
    };
    (status, json, headers)
}

/// One request returning raw bytes (stream bodies are not JSON).
async fn call_raw(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = builder.header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .oneshot(builder.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads")
        .to_vec();
    (status, headers, bytes)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[tokio::test]
async fn media_connect_browse_play_seek_stop() {
    let e2e = E2e::open("plex").await;
    let app = e2e.router();

    // Setup: one admin over bearer transport.
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/setup",
        &[],
        Some(json!({
            "username": "e2e-owner",
            "password": "e2e-owner-password-1",
            "display_name": "e2e-owner",
            "transport": "bearer",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let auth = bearer(body["token"].as_str().expect("admin token"));

    // Seed one four-minute catalog track behind the playback lifecycle.
    seed_track(&e2e.runtime).await;

    // Connect: the admin saves the Plex server; the mock accepts its token.
    let plex = serve_plex().await.expect("mock plex serves");
    e2e.store
        .save_secret(
            droppedneedle::runtime_config::secret_sections::PlexConnection {
                plex_url: plex.base_url.clone(),
                plex_token: droppedneedle::runtime_config::Secret::new(PLEX_TOKEN),
                enabled: true,
                ..Default::default()
            },
        )
        .expect("plex settings save");
    let (status, body, _) = call(
        app.clone(),
        "GET",
        "/api/v3/remotes/plex/connection",
        &[("authorization", auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["connected"], true);
    assert_eq!(body["source"], "plex");

    // Browse: the mock catalog answers through the unified adapter.
    let (status, body, _) = call(
        app.clone(),
        "GET",
        "/api/v3/remotes/plex/albums",
        &[("authorization", auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"], 3,
        "mock sections hold exactly three albums: {body}"
    );
    assert_eq!(body["items"][0]["source"], "plex");

    // Play: whole canned bytes with the upstream content type.
    let expected = canned_stream_bytes(b'P');
    let (status, headers, bytes) = call_raw(
        app.clone(),
        "GET",
        "/api/v3/stream/plex/library/parts/1/file.mp3",
        &[("authorization", auth.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("audio/mpeg")
    );
    assert_eq!(bytes, expected);

    // Seek: an exact 100-byte slice with byte-exact framing.
    let (status, headers, bytes) = call_raw(
        app.clone(),
        "GET",
        "/api/v3/stream/plex/library/parts/1/file.mp3",
        &[("authorization", auth.as_str()), ("range", "bytes=100-199")],
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers.get("content-range").and_then(|v| v.to_str().ok()),
        Some("bytes 100-199/1024")
    );
    assert_eq!(bytes, expected[100..200]);

    // Start: the session opens and presence shows one listener.
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/playback/start",
        &[("authorization", auth.as_str())],
        Some(json!({
            "track_id": "journey-track-1",
            "source": "plex",
            "device": "web",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true);
    let (status, body, _) = call(
        app.clone(),
        "GET",
        "/api/v3/now-playing",
        &[("authorization", auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"].as_array().expect("sessions").len(), 1);

    // Progress then stop past threshold: the play counts and clears.
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/playback/progress",
        &[("authorization", auth.as_str())],
        Some(json!({
            "track_id": "journey-track-1",
            "source": "plex",
            "device": "web",
            "position_ms": 120_000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/playback/stop",
        &[("authorization", auth.as_str())],
        Some(json!({
            "track_id": "journey-track-1",
            "source": "plex",
            "device": "web",
            "position_ms": 240_000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["scrobbled"], true);
    let (status, body, _) = call(
        app.clone(),
        "GET",
        "/api/v3/now-playing",
        &[("authorization", auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"].as_array().expect("sessions").len(), 0);

    // Reported: exactly one play-history row for the journey track.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_play_history WHERE track_name = 'Meridian Dawn'",
    )
    .fetch_one(e2e.runtime.pool())
    .await
    .expect("history counts");
    assert_eq!(count, 1);
}

/// Minimal artist/album/track rows behind the playback lifecycle.
async fn seed_track(runtime: &DbRuntime) {
    let seed = [
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('journey-artist-1', 'Aurora Current', 'aurora current', 'group', 0, 0)",
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('journey-album-1', 'root-1', 'aurora current/neon meridian', 'Neon Meridian', \
         'neon meridian', 'journey-artist-1', 'manual', 0, 0)",
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, duration_seconds, ingest_source, \
         imported_at, membership_source) \
         VALUES ('journey-track-1', 'journey-album-1', 'root-1', '/music/meridian.flac', \
         'meridian.flac', 'hash-1', 1024, 0, 'rev-1', 'Meridian Dawn', 'meridian dawn', \
         'Neon Meridian', 'neon meridian', 'flac', 240.0, 'seed', 0, 'manual')",
    ]
    .join(";\n");
    runtime
        .lane()
        .write(
            droppedneedle::db::Lane::Foreground,
            "journey seed",
            move |tx| {
                tx.execute_batch(&seed)
                    .map_err(droppedneedle::db::OpError::from)?;
                Ok(())
            },
        )
        .await
        .expect("seed batch runs");
}
