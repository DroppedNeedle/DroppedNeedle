//! Stage-7 acquire journeys through the real app: request → approve →
//! land, wanted watch → candidate → auto-download, drop → quarantine →
//! resolve, follow toggle → approval → armed follow, plus the 0002
//! migration check. Scratch databases and sandbox dirs only; the worker
//! loop itself never runs here (landing is simulated through the journal,
//! which is exactly what the loop would write).

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
}

impl E2e {
    async fn open(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "acquire-journey-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir creates");
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
        let (stage6, _worker) = droppedneedle::stage6::Stage6Setup::build(
            &self.db_path,
            &app_config,
            auth.users.clone(),
            Arc::clone(&self.crypto),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            connect_apps.clone(),
            Some(library.root_source()),
        )
        .expect("stage6 bundle builds");
        let acquire = AcquireSetup::build(
            &self.db_path,
            &app_config,
            auth.users.clone(),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.store),
            &mut reads.collections,
        )
        .expect("acquire bundle builds");
        let compat = droppedneedle::compat::CompatSetup::build(
            auth.users.clone(),
            Arc::clone(&self.crypto),
            stage6.playback.clone(),
            stage6.stream.engine.clone(),
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
            stage6,
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
    // up in stage 8).
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
// Journey 2: wanted watch → loopback candidate → auto-download dispatch.
// The candidate comes from the in-repo slskd mock over HTTP; nothing
// touches the live network.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acquire_wanted_watch_auto_downloads() {
    use droppedneedle::acquire::flows::loops::{
        LoopState, WantedDeps, WantedSettings, wanted_tick,
    };
    use droppedneedle::acquire::flows::seams::MemoryTicks;
    use droppedneedle::acquire::flows::stores::{
        LibraryPresence, RequestLedger, RequestRow, WantedStore, Watch,
    };
    use droppedneedle::acquire::search::FanoutSearch;
    use droppedneedle::acquire::slskd::{
        DownloadPolicy, MOCK_API_KEY, MockSlskd, ReqwestSlskdHttp, SlskdClient, SlskdRepository,
    };
    use droppedneedle::acquire::usenet::newznab::NewznabIndexer;
    use droppedneedle::acquire::usenet::prowlarr::ProwlarrIndexer;

    let e2e = E2e::open("wanted").await;
    let (app, acquire) = e2e.build();
    let (_admin, _molly, molly_id) = seed_users(&app).await;

    // slskd mock with its canned peer set, behind the production fan-out.
    let mock = MockSlskd::start().await.expect("mock starts");
    let mount = std::env::temp_dir().join(format!(
        "acquire-journey-slskd-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let transport = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), MOCK_API_KEY);
    let repo = Arc::new(SlskdRepository::new(
        SlskdClient::new(transport),
        &mock.base_url(),
        MOCK_API_KEY,
        mount,
        DownloadPolicy::default(),
    ));
    let search = Arc::new(FanoutSearch::new(
        Some(repo),
        Arc::new(NewznabIndexer::new(
            Vec::new(),
            Duration::from_secs(300),
            Duration::from_secs(60),
            Duration::from_secs(300),
            Duration::from_secs(5),
        )),
        Arc::new(ProwlarrIndexer::new(
            None,
            Vec::new(),
            false,
            Duration::from_secs(300),
            Duration::from_secs(300),
            Duration::from_secs(5),
        )),
        droppedneedle::runtime_config::sections::UsenetBackendSetting::default(),
        Duration::from_secs(5),
    ));
    assert!(search.any_configured());

    // A failed ask enrols a watch; the loop finds the mock candidate.
    let now = 1_700_000_000i64;
    let ledger = Arc::new(RequestLedger::new());
    ledger.upsert(RequestRow {
        mbid: "rg-green".to_owned(),
        kind: "album".to_owned(),
        user_id: molly_id.clone(),
        artist: "Massive Attack".to_owned(),
        title: "Blue Lines".to_owned(),
        status: "failed".to_owned(),
        task_id: None,
        generation: 1,
        completed_at: None,
    });
    let watches = Arc::new(WantedStore::new());
    watches.enrol(Watch {
        rg_mbid: "rg-green".to_owned(),
        user_id: molly_id.clone(),
        artist: "Massive Attack".to_owned(),
        title: "Blue Lines".to_owned(),
        first_release_date: None,
        quiet_streak: 0,
        next_check_at: now,
    });
    let ticks = Arc::new(MemoryTicks::new());
    let deps = WantedDeps {
        settings: Arc::new(WantedSettings::default),
        watches,
        ledger,
        search,
        downloads: acquire.dispatch.clone(),
        library: Arc::new(LibraryPresence::new()),
        ticks: ticks.clone(),
    };
    let mut state = LoopState::new();
    let summary = wanted_tick(now, &mut state, &deps).await;
    assert_eq!(summary.dispatched, 1, "candidate auto-downloads");
    let tasks = journal_tasks(&e2e.db_path);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].1, "queued");
    let manifest = acquire.staging_root.join(&tasks[0].0).join("manifest.json");
    assert!(manifest.is_file(), "wanted dispatch stages a manifest");
}

// ---------------------------------------------------------------------------
// Journey 3: drop folder import → quarantine → resolve by hand.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acquire_drop_quarantine_resolve() {
    use droppedneedle::acquire::flows::operations::{
        DropImportDeps, DropJob, ResolveDecision, create_drop_job, process_drop_job,
        quarantine_dir, resolve_quarantined_item,
    };
    use droppedneedle::acquire::flows::seams::{
        ManualClock, MemoryTicks, ScriptedVerify, VerifyVerdict,
    };

    struct MoveAside {
        staging_root: PathBuf,
    }
    impl droppedneedle::acquire::flows::seams::LibraryOrganise for MoveAside {
        fn organise(&self, job_id: &str, staged_path: &str) -> Result<String, String> {
            let dest = self
                .staging_root
                .join("resolved")
                .join(job_id)
                .join("track.flac");
            std::fs::create_dir_all(dest.parent().expect("resolve parent"))
                .map_err(|error| format!("resolve dir: {error}"))?;
            std::fs::rename(staged_path, &dest).map_err(|error| format!("resolve: {error}"))?;
            Ok(dest.to_string_lossy().into_owned())
        }
    }

    let e2e = E2e::open("drop").await;
    let (_app, acquire) = e2e.build();
    let staging = acquire.staging_root.clone();
    let incoming = staging.join("_incoming");
    std::fs::create_dir_all(&incoming).expect("incoming creates");
    let upload = incoming.join("track.flac");
    std::fs::write(&upload, b"fake audio").expect("upload writes");

    // Adopt the upload as a drop job through the real bundle stores.
    let mut job: DropJob = create_drop_job(
        e2e.runtime.wakeups(),
        e2e.runtime.lane(),
        &acquire.flows.ops,
        &staging,
        1,
        "u-molly",
        &[("track.flac".to_owned(), upload.clone())],
        1_700_000_000,
    )
    .await
    .expect("drop job creates");
    assert!(!upload.exists(), "adoption moves the upload into staging");

    // A bad-source verdict quarantines the file.
    let verify = Arc::new(ScriptedVerify::new());
    verify.set(
        "track.flac",
        VerifyVerdict::BadSource("fake rip".to_owned()),
    );
    let clock = Arc::new(ManualClock::new(1_700_000_000));
    let deps = DropImportDeps {
        verify,
        organise: Arc::new(MoveAside {
            staging_root: staging.clone(),
        }),
        quarantine: acquire.flows.quarantine.clone(),
        ledger: acquire.flows.ledger.clone(),
        ticks: Arc::new(MemoryTicks::new()),
        clock,
    };
    process_drop_job(
        e2e.runtime.wakeups(),
        e2e.runtime.lane(),
        &acquire.flows.ops,
        &deps,
        &staging,
        &mut job,
        None,
    )
    .await
    .expect("drop processes");
    assert!(
        matches!(
            job.items[0].outcome,
            Some(droppedneedle::acquire::flows::operations::DropItemOutcome::Quarantined(_))
        ),
        "bad source quarantines"
    );
    assert!(
        quarantine_dir(&staging).join("track.flac").is_file()
            || acquire.flows.quarantine.get("drop-1:track.flac").is_some(),
        "quarantine holds the file"
    );

    // A hand resolve moves it out and clears the registry.
    let resolved =
        resolve_quarantined_item(&deps, &mut job, "track.flac", ResolveDecision::Match, None)
            .expect("resolve runs");
    assert!(resolved, "match resolves the item");
    assert!(
        matches!(
            job.items[0].outcome,
            Some(droppedneedle::acquire::flows::operations::DropItemOutcome::Resolved(_))
        ),
        "item resolves"
    );
    assert!(
        staging
            .join("resolved")
            .join("drop-1")
            .join("track.flac")
            .is_file(),
        "resolved file lands in the staging area"
    );
    assert!(acquire.flows.quarantine.get("drop-1:track.flac").is_none());
}

// ---------------------------------------------------------------------------
// Journey 4: follow toggle → approval queue → armed follow. The collections
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
// Migration 0002: applies cleanly on top of 0001, and reruns idempotently.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migration_0002_download_idempotency() {
    let dir = std::env::temp_dir().join(format!(
        "acquire-migration-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir creates");
    let db_path = dir.join("migrate.db");

    // Baseline only, exactly as a pre-stage-7 database would hold it.
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
        .expect("0002 applies on top of 0001");
    let conn = rusqlite::Connection::open(&db_path).expect("scratch reopens");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version reads");
    assert_eq!(version, latest_version());
    let table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE name = 'download_idempotency_keys'",
            [],
            |row| row.get(0),
        )
        .expect("table lookup runs");
    assert_eq!(table.as_deref(), Some("download_idempotency_keys"));
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
    let _ = std::fs::remove_dir_all(&dir);
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

    let journal = Arc::new(Journal::memory().expect("memory journal"));
    journal
        .with_store(|store| {
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

    let missing = "fedcba9876543210fedcba9876543210";
    let err = source
        .enqueue(missing, 0)
        .await
        .expect_err("unknown task rejects");
    assert!(matches!(err, SourceError::Rejected(_)));
}
