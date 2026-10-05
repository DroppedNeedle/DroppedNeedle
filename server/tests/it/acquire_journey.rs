//! Acquire journeys through the real app: request, approve, land; follow
//! toggle, approval, armed follow; plus the migration upgrade check and
//! the usenet re-attach. Scratch databases only; the worker loop never
//! runs here (landing is simulated through the journal).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{Router, http::StatusCode};
use droppedneedle::{
    AppConfig, AppState,
    acquire::AcquireSetup,
    auth::{prod::ProdAuth, users::stores::SystemClock, wiring::AuthSetup},
    config::DEFAULT_PORT,
    create_app,
    db::{DbConfig, open_runtime},
    http_client::HttpClientFactory,
    ids::{IdGenerator, UuidGenerator},
    reads::ReadsSetup,
    runtime_config::{ConfigStore, Crypto, Secret},
    schema::{apply_migrations, latest_version},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const HOST: &str = "test.local";
const RG_MBID: &str = "123e4567-e89b-12d3-a456-426614174000";
const ARTIST_MBID: &str = "223e4567-e89b-12d3-a456-426614174001";

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

struct E2e {
    runtime: droppedneedle::db::DbRuntime,
    bundle: ProdAuth,
    store: Arc<droppedneedle::runtime_config::ConfigStore>,
    crypto: Arc<droppedneedle::runtime_config::Crypto>,
    http: HttpClientFactory,
    ids: Arc<UuidGenerator>,
    clock: Arc<SystemClock>,
    db_path: PathBuf,
    _scratch: crate::common::ScratchDir,
}

impl E2e {
    async fn open(name: &str) -> Self {
        let scratch = crate::common::ScratchDir::new(name);
        let dir = scratch.to_path_buf();
        let db_path = dir.join("app.db");
        let runtime = open_runtime(&DbConfig::new(&db_path))
            .await
            .expect("scratch runtime opens");
        apply_migrations(runtime.pool())
            .await
            .expect("migrations apply");
        let crypto = Arc::new(Crypto::from_key_bytes(&[7u8; 32]).expect("test key"));
        let store = Arc::new(
            ConfigStore::open(
                &dir.join("config.json"),
                Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            )
            .expect("scratch config opens"),
        );
        // Breach screening off via the real knob: setup and user creation
        // must never dial the network in this suite.
        let mut security: droppedneedle::runtime_config::sections::SecuritySettings =
            store.get().expect("security section reads");
        security.hibp_check = false;
        store.save(security).expect("hibp switch saves");
        store
            .save_secret(
                droppedneedle::runtime_config::secret_sections::WrappedSettings {
                    api_key: Secret::new("acquire-journey-wrapped-key"),
                },
            )
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
        Self {
            runtime,
            bundle,
            store,
            crypto,
            http,
            ids,
            clock,
            db_path,
            _scratch: scratch,
        }
    }

    /// One router plus its acquire bundle over the scratch database.
    fn build(&self) -> (Router, AcquireSetup) {
        let auth = AuthSetup::build(
            self.bundle.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.crypto),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            "",
        )
        .expect("prod auth bundle builds");
        let wrapped_api_key = self
            .store
            .get_raw::<droppedneedle::runtime_config::secret_sections::WrappedSettings>()
            .expect("wrapped settings read")
            .api_key
            .expose()
            .to_owned();
        let mut reads = ReadsSetup::build(
            self.runtime.pool(),
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            wrapped_api_key,
            None,
        );
        let mut app_config = AppConfig::new(DEFAULT_PORT);
        app_config.root_app_dir = self
            .db_path
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let connect_apps: droppedneedle::runtime_config::sections::ConnectApps =
            self.store.get().unwrap_or_default();
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
        )
        .expect("media bundle builds");
        let acquire = AcquireSetup::build(
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
            auth.users.clone(),
            Arc::clone(&self.crypto),
            media.playback.clone(),
            media.stream.engine.clone(),
            library.clone(),
            &connect_apps,
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
            acquire.clone(),
            library,
            compat,
            admin,
            settings,
            jobs,
            plugins,
        );
        (create_app(state), acquire)
    }
}

/// One JSON request through the real app.
async fn call(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    use axum::body::Body;
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    builder = builder.header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&value).expect("body serializes"))
        }
        None => Body::empty(),
    };
    let response = app
        .oneshot(builder.body(body).expect("request builds"))
        .await
        .expect("app responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is json")
    };
    (status, json)
}

/// Setup the owner admin plus one plain user; return both Bearer tokens.
async fn seed_users(app: &Router) -> (String, String, String) {
    let (status, body) = call(
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
    let admin = bearer(body["token"].as_str().expect("admin token"));
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v3/admin/users",
        &[("authorization", admin.as_str())],
        Some(json!({"username": "molly", "password": "molly-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let molly_id = body["id"].as_str().expect("user id").to_owned();
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v3/auth/login",
        &[],
        Some(json!({
            "username": "molly",
            "password": "molly-password-1234",
            "transport": "bearer",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let molly = bearer(body["token"].as_str().expect("molly token"));
    (admin, molly, molly_id)
}

/// `(task id, status)` rows in the scratch download journal.
fn journal_tasks(db_path: &PathBuf) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(db_path).expect("journal opens");
    let mut stmt = conn
        .prepare("SELECT id, status FROM download_tasks ORDER BY created_at")
        .expect("tasks query prepares");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("tasks query runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("task rows read")
}

// ---------------------------------------------------------------------------
// Journey 1: user requests an album → admin approves → download lands.
// Scan pickup belongs to stage 8: the journey asserts the staged state
// (journal row completed, manifest present, ask imported), not the library.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acquire_request_approve_land() {
    let e2e = E2e::open("request").await;
    let (app, acquire) = e2e.build();
    let (admin, molly, _molly_id) = seed_users(&app).await;

    // Molly asks; a plain user needs approval.
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v3/requests/albums",
        &[("authorization", molly.as_str())],
        Some(json!({
            "musicbrainz_id": RG_MBID,
            "artist": "Massive Attack",
            "album": "Blue Lines",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["status"], "awaiting_approval");

    // The admin approves: one queued download task lands in the journal.
    let (status, body) = call(
        app.clone(),
        "POST",
        &format!("/api/v3/requests/approvals/{RG_MBID}/approve"),
        &[("authorization", admin.as_str())],
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true);
    let tasks = journal_tasks(&e2e.db_path);
    assert_eq!(tasks.len(), 1, "approval dispatches one task");
    let (task_id, status_text) = &tasks[0];
    assert_eq!(status_text, "queued");
    assert_eq!(task_id.len(), 32, "task ids are 32 hex chars");
    let manifest = acquire.staging_root.join(task_id).join("manifest.json");
    assert!(manifest.is_file(), "dispatch writes the manifest skeleton");

    // The worker lands the download (simulated through the journal: the
    // loop would write exactly these columns, then the scan picks them
    // up later).
    rusqlite::Connection::open(&e2e.db_path)
        .expect("journal opens")
        .execute(
            "UPDATE download_tasks SET status = 'completed', completed_at = 1.0 WHERE id = ?",
            [task_id],
        )
        .expect("task completes");
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v3/requests/sync",
        &[("authorization", admin.as_str())],
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["reconciled"].as_u64().unwrap_or(0) >= 1);

    // Molly's history shows the ask imported.
    let (status, body) = call(
        app.clone(),
        "GET",
        "/api/v3/requests/history",
        &[("authorization", molly.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().expect("history items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "imported");
    assert_eq!(items[0]["task_id"], Value::String(task_id.clone()));
}

// ---------------------------------------------------------------------------
// Journey 2: follow toggle → approval queue → armed follow. The collections
// legs and the requests mutations share one approval store through the
// wiring bridges.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acquire_follow_approval_arms_auto_download() {
    let e2e = E2e::open("follow").await;
    let (app, _acquire) = e2e.build();
    let (admin, molly, molly_id) = seed_users(&app).await;

    // Molly follows, then asks for auto-download (pending for a plain user).
    let (status, body) = call(
        app.clone(),
        "PUT",
        &format!("/api/v3/artists/{ARTIST_MBID}/follow"),
        &[("authorization", molly.as_str())],
        Some(json!({"followed": true, "artist_name": "Massive Attack"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        app.clone(),
        "PUT",
        &format!("/api/v3/artists/{ARTIST_MBID}/auto-download"),
        &[("authorization", molly.as_str())],
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["auto_download_state"], "pending");

    // The collections read leg shows the ask from the shared store.
    let (status, body) = call(
        app.clone(),
        "GET",
        "/api/v3/requests/auto-download-approvals",
        &[("authorization", admin.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 1);
    assert_eq!(body["items"][0]["user_id"], molly_id);

    // The admin approves through the requests mutation; the follow arms.
    let (status, body) = call(
        app.clone(),
        "POST",
        &format!("/api/v3/requests/auto-download-approvals/{molly_id}/{ARTIST_MBID}/approve"),
        &[("authorization", admin.as_str())],
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true);
    let (status, body) = call(
        app.clone(),
        "GET",
        &format!("/api/v3/artists/{ARTIST_MBID}/follow-status"),
        &[("authorization", molly.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["auto_download_state"], "active");
}

// ---------------------------------------------------------------------------
// Migrations apply cleanly on top of the baseline and rerun idempotently.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migrations_upgrade_from_baseline() {
    let dir = crate::common::ScratchDir::new("migrate");
    let db_path = dir.join("migrate.db");

    // Baseline only, as the oldest database would hold it.
    let baseline = include_str!("../../migrations/0001_baseline.sql");
    {
        let conn = rusqlite::Connection::open(&db_path).expect("scratch opens");
        conn.execute_batch(baseline).expect("baseline applies");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version reads");
        assert_eq!(version, 1);
    }
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime opens");
    apply_migrations(runtime.pool())
        .await
        .expect("later migrations apply on top of 0001");
    let conn = rusqlite::Connection::open(&db_path).expect("scratch reopens");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version reads");
    assert_eq!(version, latest_version());
    for name in [
        "download_idempotency_keys",
        "acquire_operations",
        "acquire_follow_cursors",
        "acquire_flow_quarantine",
    ] {
        let table: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .expect("table lookup runs");
        assert_eq!(table.as_deref(), Some(name));
    }
    drop(conn);

    // A second run is a clean no-op (the boot path reruns every start).
    apply_migrations(runtime.pool())
        .await
        .expect("migrations rerun cleanly");
    let conn = rusqlite::Connection::open(&db_path).expect("scratch reopens");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version reads");
    assert_eq!(version, latest_version());
    runtime.shutdown().await;
}

/// Crash-retry usenet enqueue re-attaches instead of double-adding: the
/// deterministic job name is already on the client, so the adapter
/// returns it with zero add calls. With no job present the search still
/// runs and reports no candidate.
#[tokio::test]
async fn usenet_enqueue_reattaches_to_existing_job() {
    use droppedneedle::acquire::dispatch::Journal;
    use droppedneedle::acquire::downloads::sources::{DownloadSource, SourceError};
    use droppedneedle::acquire::downloads::store::NewTask;
    use droppedneedle::acquire::sources::SabnzbdSource;
    use droppedneedle::acquire::usenet::mocks::{SabnzbdMock, serve_loopback};
    use droppedneedle::acquire::usenet::newznab::NewznabIndexer;
    use droppedneedle::acquire::usenet::policy::UsenetPolicy;
    use droppedneedle::acquire::usenet::prowlarr::ProwlarrIndexer;
    use droppedneedle::acquire::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
    use droppedneedle::runtime_config::sections::UsenetBackendSetting;

    let task_id = "0123456789abcdef0123456789abcdef";
    let job_name = format!("droppedneedle-{task_id}-0");
    let mock = SabnzbdMock::new();
    mock.queue_job("nzo-1", &job_name, "Downloading", "100.0", "50.0", "50");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");

    let db = droppedneedle::acquire::db::AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let journal = Arc::new(Journal::new(db));
    journal
        .run("test.seed", |store| {
            store.insert_task(
                &NewTask {
                    id: task_id.to_owned(),
                    user_id: "u1".to_owned(),
                    artist_name: "artist".to_owned(),
                    album_title: "album".to_owned(),
                    release_group_mbid: "rg-1".to_owned(),
                    origin: "user".to_owned(),
                    retry_count: 0,
                },
                1_700_000_000.0,
            )
        })
        .await
        .expect("task inserts");
    let policy = UsenetPolicy::v2_defaults();
    let queue = Arc::new(SabnzbdQueue::new(
        SabnzbdClient::new(
            reqwest::Client::new(),
            &base,
            "SABKEY",
            3,
            Duration::from_millis(1),
        ),
        &base,
        "SABKEY",
        std::env::temp_dir(),
        policy.clone(),
    ));
    let source = SabnzbdSource::new(
        queue,
        Arc::new(NewznabIndexer::new(
            Vec::new(),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )),
        Arc::new(ProwlarrIndexer::new(
            None,
            Vec::new(),
            false,
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )),
        UsenetBackendSetting::default(),
        policy,
        journal,
        None,
        Duration::from_secs(5),
    );
    let handle = source.enqueue(task_id, 0).await.expect("re-attaches");
    assert_eq!(handle.job_name, job_name);
    assert!(mock.state().add_file_requests.is_empty());
    assert!(mock.state().add_url_requests.is_empty());
    assert_eq!(handle.nzo_id, "nzo-1", "the handle keeps SABnzbd's job id");

    // SABnzbd renames the finished job; the poll still finds it by id.
    mock.state().queue_slots.clear();
    mock.history_job(
        "nzo-1",
        &format!("{job_name}.1"),
        "Completed",
        "/downloads/complete/renamed",
        1024,
        "",
    );
    let progress = source.poll(&handle).await.expect("poll answers");
    assert!(
        progress.all_terminal && progress.all_succeeded,
        "{progress:?}"
    );

    let missing = "fedcba9876543210fedcba9876543210";
    let err = source
        .enqueue(missing, 0)
        .await
        .expect_err("unknown task rejects");
    assert!(matches!(err, SourceError::Rejected(_)));
}
