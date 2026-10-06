//! Media journeys through the real app (`create_app` over a scratch SQLite
//! database) against the in-repo mock Plex server: save a Plex connection,
//! browse its albums, stream whole bytes, seek a range, then start,
//! progress and stop playback and find the play in history. Also the
//! transcode length estimate and the concurrent-stream caps.

use crate::common::ScratchDir;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle::auth::prod::ProdAuth;
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::roles::SessionKind;
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
use droppedneedle::stream::gateway::{Gateway, RemoteMedia, RemoteReader};
use droppedneedle::stream::leases::{DIRECT_PRINCIPAL_LIMIT, DIRECT_WAIT_TIMEOUT};
use droppedneedle::stream::routes::{
    AudioSource, OpenMedia, StreamEngine, StreamFault, StreamOpen, StreamParams, StreamState,
    stream_routes,
};
use droppedneedle::stream::transcode::{
    StreamPlan, TranscodeBody, TranscodeError, TranscodeSettings, Transcoder,
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
        let wrapped_api_key = self
            .store
            .get_raw::<WrappedSettings>()
            .expect("wrapped settings read")
            .api_key
            .expose()
            .to_owned();
        let reads = ReadsSetup::build(
            self.runtime.pool(),
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            wrapped_api_key,
            None,
        )
        .with_collections(droppedneedle::reads::collections::db::CollectionsDb::new(
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
        ));
        let connect_apps: droppedneedle::runtime_config::sections::ConnectApps =
            self.store.get().unwrap_or_default();
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
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            connect_apps.clone(),
            Some(library.root_source()),
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
            Arc::clone(&self.store),
            Arc::new(droppedneedle::remotes::adapter::PlaylistImportSink::new(
                reads.collections.clone(),
            )),
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

/// Scripted transcoded landing behind the real stream routes.
struct TranscodedEngine {
    media: OpenMedia,
    seen: Arc<std::sync::Mutex<Vec<StreamParams>>>,
}

impl StreamEngine for TranscodedEngine {
    async fn open(&self, request: StreamOpen) -> Result<OpenMedia, StreamFault> {
        self.seen
            .lock()
            .expect("seen unlocks")
            .push(request.params.clone());
        if request.source == AudioSource::Plex && request.key == "library/parts/1/file.mp3" {
            Ok(self.media.clone())
        } else {
            Err(StreamFault::NotFound)
        }
    }
}

/// Stream routes with a stashed `user-1` session.
fn stream_app<E: StreamEngine + Send + Sync + 'static>(engine: E) -> Router {
    let state = StreamState {
        engine: Arc::new(engine),
        ids: Arc::new(UuidGenerator) as Arc<dyn IdGenerator>,
    };
    stream_routes(state).layer(axum::middleware::from_fn(
        |mut req: Request<Body>, next: axum::middleware::Next| async move {
            req.extensions_mut().insert(CurrentSession {
                user_id: "user-1".to_owned(),
                session_id: "sess-1".to_owned(),
                kind: SessionKind::Standard,
                transport: Transport::Bearer,
            });
            next.run(req).await
        },
    ))
}

fn stream_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test]
async fn media_transcode_estimate_round_trip() {
    // Why scripted: the production gateway only transcodes local files, and
    // it never emits an estimate (track durations are unknown at that
    // layer), so the estimated-length landing is pinned here through the
    // real routes with a scripted transcoded engine.
    let bytes = b"fake-transcoded-mp3-bytes".to_vec();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let engine = TranscodedEngine {
        media: OpenMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: bytes.len() as u64,
            transcoded: true,
            estimated_len: Some(48_000),
            bytes: bytes.clone(),
        },
        seen: Arc::clone(&seen),
    };
    let app = stream_app(engine);

    let (status, headers, body) = call_raw(
        app.clone(),
        "GET",
        "/stream/plex/library/parts/1/file.mp3?format=mp3&estimate_content_length=true",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(
        stream_header(&headers, "content-type"),
        Some("audio/mpeg".to_owned())
    );
    assert_eq!(
        stream_header(&headers, "accept-ranges"),
        Some("none".to_owned())
    );
    assert_eq!(
        stream_header(&headers, "cache-control"),
        Some("no-store".to_owned())
    );
    assert_eq!(
        stream_header(&headers, "content-length"),
        Some("48000".to_owned())
    );
    {
        let seen = seen.lock().expect("seen unlocks");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].format, Some("mp3".to_owned()));
        assert!(seen[0].estimate_content_length);
    }

    let (status, headers, body) = call_raw(
        app.clone(),
        "GET",
        "/stream/plex/library/parts/1/file.mp3?format=mp3",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(
        stream_header(&headers, "accept-ranges"),
        Some("none".to_owned())
    );
    assert_eq!(
        stream_header(&headers, "cache-control"),
        Some("no-store".to_owned())
    );
    assert_eq!(
        stream_header(&headers, "content-length"),
        Some("25".to_owned()),
        "without the flag the length is the actual body, not the estimate"
    );
}

/// Transcoder that never runs: the caps test only opens remote keys.
struct NeverBody;

impl TranscodeBody for NeverBody {
    async fn next_chunk(
        &mut self,
        _is_disconnected: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Option<Vec<u8>>, TranscodeError> {
        Ok(None)
    }

    async fn close(&mut self) {}
}

struct NeverTranscoder;

impl Transcoder for NeverTranscoder {
    type Body = NeverBody;

    async fn stream(
        &self,
        _source_path: &Path,
        _plan: &StreamPlan,
        _principal: &str,
    ) -> Result<NeverBody, TranscodeError> {
        Err(TranscodeError::Capacity)
    }
}

/// Remote reader serving one canned object.
struct CannedRemote {
    media: RemoteMedia,
}

impl RemoteReader for CannedRemote {
    async fn fetch(
        &self,
        _source: AudioSource,
        _key: &str,
        _user_id: &str,
    ) -> Result<RemoteMedia, StreamFault> {
        Ok(self.media.clone())
    }
}

/// Remote reader that holds every fetch until released, so the direct-gate
/// slots stay occupied while the N+1th request arrives. Clones share one
/// gate, so the test holds a control handle beside the engine's copy.
#[derive(Clone)]
struct BlockingRemote {
    inner: Arc<BlockingRemoteInner>,
}

struct BlockingRemoteInner {
    media: RemoteMedia,
    entries: AtomicUsize,
    release: tokio::sync::Notify,
}

impl BlockingRemote {
    fn new(media: RemoteMedia) -> Self {
        Self {
            inner: Arc::new(BlockingRemoteInner {
                media,
                entries: AtomicUsize::new(0),
                release: tokio::sync::Notify::new(),
            }),
        }
    }

    async fn wait_for_entries(&self, want: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while self.inner.entries.load(Ordering::SeqCst) < want {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("holders reach the remote");
    }
}

impl RemoteReader for BlockingRemote {
    async fn fetch(
        &self,
        _source: AudioSource,
        _key: &str,
        _user_id: &str,
    ) -> Result<RemoteMedia, StreamFault> {
        self.inner.entries.fetch_add(1, Ordering::SeqCst);
        self.inner.release.notified().await;
        Ok(self.inner.media.clone())
    }
}

#[tokio::test]
async fn media_concurrent_streams_under_caps() {
    assert_eq!(
        DIRECT_PRINCIPAL_LIMIT, 8,
        "the eight join arms below track the per-principal cap"
    );
    let bytes: Vec<u8> = (0..1024).map(|index| (index % 251) as u8).collect();
    let media = RemoteMedia {
        content_type: "audio/mpeg".to_owned(),
        bytes: bytes.clone(),
    };
    let scratch = ScratchDir::new("media-caps");
    let root = scratch.to_path_buf();

    // Under the caps, N concurrent reads all succeed.
    let engine = Gateway::new(
        root.clone(),
        CannedRemote {
            media: media.clone(),
        },
        NeverTranscoder,
        TranscodeSettings::default(),
        false,
    );
    let app = stream_app(engine);
    let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
    );
    for (index, (status, headers, body)) in [r0, r1, r2, r3, r4, r5, r6, r7].into_iter().enumerate()
    {
        if index % 2 == 0 {
            assert_eq!(status, StatusCode::OK, "reader {index}");
            assert_eq!(body, bytes, "reader {index}");
        } else {
            assert_eq!(status, StatusCode::PARTIAL_CONTENT, "reader {index}");
            assert_eq!(body, bytes[0..10], "reader {index}");
            assert_eq!(
                stream_header(&headers, "content-range"),
                Some("bytes 0-9/1024".to_owned()),
                "reader {index}"
            );
        }
    }

    // With all N slots held, the N+1th request waits out the gate and 429s.
    // The leases release before the responses send, so the 429 only shows
    // while the N holders are still inside their opens.
    let blocking = BlockingRemote::new(media.clone());
    let engine = Gateway::new(
        root,
        blocking.clone(),
        NeverTranscoder,
        TranscodeSettings::default(),
        false,
    );
    let app = stream_app(engine);
    let mut holders = Vec::new();
    for _ in 0..DIRECT_PRINCIPAL_LIMIT {
        let app = app.clone();
        holders.push(tokio::spawn(async move {
            call_raw(app, "GET", "/stream/plex/part-1", &[]).await
        }));
    }
    blocking.wait_for_entries(DIRECT_PRINCIPAL_LIMIT).await;
    let (status, headers, body) = tokio::time::timeout(
        DIRECT_WAIT_TIMEOUT + std::time::Duration::from_secs(10),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
    )
    .await
    .expect("the N+1th request answers");
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(stream_header(&headers, "retry-after"), Some("1".to_owned()));
    let envelope: Value = serde_json::from_slice(&body).expect("429 envelope is json");
    assert_eq!(envelope["error"]["code"], "STREAM_CAPACITY_EXHAUSTED");

    blocking.inner.release.notify_waiters();
    let [h0, h1, h2, h3, h4, h5, h6, h7]: [_; 8] = holders.try_into().expect("eight holders");
    let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(h0, h1, h2, h3, h4, h5, h6, h7);
    for result in [r0, r1, r2, r3, r4, r5, r6, r7] {
        let (status, _, body) = result.expect("holder completes");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, bytes);
    }
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
