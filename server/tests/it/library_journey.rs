//! Stage-8 E2E: library journeys through the real app.
//!
//! Everything here runs against `create_app` with the production
//! SQLite bundle over scratch databases plus the library test bundle
//! (scripted identify providers, memory stores, publish cell under a
//! sandbox root). No test touches the network, the production
//! database, or the real library: every root is a fresh sandbox
//! under the temp dir, planted from read-only fixture copies.
//!
//! Journeys (each owns a scratch dir, parallel-safe):
//!
//! - `library_journey_scan_identify_review_organize_undo`: add root →
//!   scan → identify → review → approve → auto-organize → Undo, with
//!   a scan purity pin (zero byte writes across scan+identify).
//! - `library_journey_retag_apply_baseline_restore`: manual retag
//!   preview → Apply → baseline restore.
//! - `library_loops_start_and_stop`: every background loop starts
//!   over the real bundle and shuts down cleanly.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common::ScratchDir;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle::auth::prod::ProdAuth;
use droppedneedle::auth::session::cookies::COOKIE_NAME;
use droppedneedle::auth::users::stores::SystemClock;
use droppedneedle::auth::wiring::AuthSetup;
use droppedneedle::config::DEFAULT_PORT;
use droppedneedle::db::{DbConfig, DbRuntime, open_runtime};
use droppedneedle::http_client::HttpClientFactory;
use droppedneedle::ids::{IdGenerator, UuidGenerator};
use droppedneedle::library::identify::models::{
    CandidateEvidence, EvidenceClass, RecallResult, TrackEvidence,
};
use droppedneedle::library::wiring::LibrarySetup;
use droppedneedle::runtime_config::sections::SecuritySettings;
use droppedneedle::runtime_config::{
    ConfigStore, Crypto, Secret, secret_sections::WrappedSettings,
};
use droppedneedle::{AppConfig, AppState, create_app, reads::ReadsSetup};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;

/// Fixed host for every request.
const HOST: &str = "e2e.test";
/// Wrapped shared secret saved into every scratch config.
const TEST_WRAPPED_KEY: &str = "lib-journey-wrapped-key-1";

/// One scratch deployment: migrated database, production adapters,
/// library test bundle, and the app router over them.
struct Lib {
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
    dir: PathBuf,
    /// Dropped last: the scratch directory goes away with the test.
    _scratch: ScratchDir,
    db_path: PathBuf,
    library: LibrarySetup,
}

impl Lib {
    async fn open(tag: &str) -> Self {
        let scratch = ScratchDir::new(&format!("lib-journey-{tag}"));
        let dir = scratch.to_path_buf();
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
        // The library bundle builds once here and is shared with the
        // app: memory stores must survive across calls.
        let users = AuthSetup::build(
            bundle.clone(),
            Arc::clone(&store),
            Arc::clone(&crypto),
            &http,
            Arc::clone(&ids) as Arc<dyn IdGenerator>,
            Arc::clone(&clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            "",
        )
        .expect("prod auth bundle builds")
        .users;
        let library = LibrarySetup::for_tests_at(
            users,
            Arc::clone(&ids) as Arc<dyn IdGenerator>,
            &db_path,
            Arc::clone(&store),
        )
        .expect("library bundle builds");
        Self {
            runtime,
            bundle,
            store,
            crypto,
            http,
            ids,
            clock,
            dir,
            _scratch: scratch,
            db_path,
            library,
        }
    }

    /// The app router over this deployment's bundles.
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
        let (media, _worker) = droppedneedle::media::MediaSetup::build(
            &self.db_path,
            &app_config,
            auth.users.clone(),
            Arc::clone(&self.crypto),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            connect_apps.clone(),
            Some(self.library.root_source()),
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
                self.library.clone(),
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
            self.library.clone(),
            compat,
            admin,
            settings,
            jobs,
            plugins,
        );
        create_app(state)
    }
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    let _headers: HeaderMap = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is json")
    };
    let _ = COOKIE_NAME;
    (status, json)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn setup_admin(app: Router, username: &str, password: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/v3/auth/setup",
        &[],
        Some(json!({
            "username": username,
            "password": password,
            "display_name": username,
            "transport": "bearer",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "setup must succeed: {body}");
    body["token"].as_str().expect("admin token").to_owned()
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library")
}

/// Copy one committed fixture into a sandbox root. The fixture dir is
/// only ever read; the sandbox copy is the scan target.
fn plant(root: &Path, relative: &str, fixture: &str) -> PathBuf {
    let dest = root.join(relative);
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
    std::fs::copy(fixtures_dir().join(fixture), &dest).expect("copy fixture");
    dest
}

/// Byte-level snapshot of a root for the purity pin: relative path
/// to (size, mtime_ns, sha256), matching the scan briefs. mtime is
/// pinned because a writer that "restores" content still moves it.
fn snapshot_tree(root: &Path) -> std::collections::BTreeMap<String, (u64, i64, String)> {
    let mut snapshot = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("read sandbox dir");
        for entry in entries {
            let entry = entry.expect("sandbox entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("sandbox prefix")
                .to_string_lossy()
                .into_owned();
            // Publisher metadata lives under the hidden prefix; the
            // purity pin covers media files only.
            if relative
                .split('/')
                .any(|part| part.starts_with(".droppedneedle-management-"))
            {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read sandbox file");
            let meta = std::fs::metadata(&path).expect("stat sandbox file");
            let mtime_ns = droppedneedle::library::scan::mtime_ns_from_metadata(&meta);
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hex: String = hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            snapshot.insert(relative, (bytes.len() as u64, mtime_ns, hex));
        }
    }
    snapshot
}

/// Drive supervisor ticks until no run is current (cap 100).
async fn drain_scans(library: &LibrarySetup) {
    library.scan_startup_recovery().await;
    for _ in 0..100 {
        library.supervisor_tick().await;
        if library.coordinator.current().is_empty() {
            break;
        }
    }
    assert!(
        library.coordinator.current().is_empty(),
        "scans drain to idle"
    );
}

/// Scripted recall with two tied pre-scored candidates: the attempt
/// must file an ambiguous review, never guess.
fn ambiguous_recall(track_a: &str, track_b: &str) -> RecallResult {
    let evidence = |track: &str, recording: &str, release_track: &str| TrackEvidence {
        local_track_id: track.to_owned(),
        classification: EvidenceClass::Supported,
        evidence_kinds: vec!["embedded_recording_mbid".to_owned()],
        recording_mbid: Some(recording.to_owned()),
        release_track_mbid: Some(release_track.to_owned()),
    };
    RecallResult {
        candidates: vec![
            CandidateEvidence {
                candidate_key: "rg-1:rel-1".to_owned(),
                release_group_mbid: "rg-1".to_owned(),
                release_mbid: Some("rel-1".to_owned()),
                album_title: "Journey Album".to_owned(),
                album_artist_name: "Journey Artist".to_owned(),
                track_evidence: vec![evidence(track_a, "rec-a", "rt-a")],
                score: 0.9,
                margin: 0.0,
                reason_code: "SUPPORTED".to_owned(),
            },
            CandidateEvidence {
                candidate_key: "rg-1:rel-2".to_owned(),
                release_group_mbid: "rg-1".to_owned(),
                release_mbid: Some("rel-2".to_owned()),
                album_title: "Journey Album".to_owned(),
                album_artist_name: "Journey Artist".to_owned(),
                track_evidence: vec![evidence(track_b, "rec-b", "rt-b")],
                score: 0.8,
                margin: 0.0,
                reason_code: "SUPPORTED".to_owned(),
            },
        ],
        fingerprint_support: std::collections::HashMap::new(),
        provider_deferred: false,
        failure_code: None,
    }
}

fn read_title(path: &Path) -> String {
    droppedneedle::library::tags::read::read_tag_only(
        path,
        droppedneedle::library::tags::AudioFormat::Flac,
    )
    .expect("tags read")
    .title
}

#[tokio::test]
async fn library_journey_scan_identify_review_organize_undo() {
    let lib = Lib::open("organize").await;
    let admin = setup_admin(lib.router(), "owner", "owner-password-1").await;
    let auth = bearer(&admin);
    let headers = [("authorization", auth.as_str())];

    // Plant two tracks in one album dir, then add the root.
    let music = lib.dir.join("music");
    let file_a = plant(&music, "album-a/01.flac", "management_full.flac");
    let file_b = plant(&music, "album-a/02.flac", "management_full.flac");
    let title_a_before = read_title(&file_a);
    let title_b_before = read_title(&file_b);
    let bytes_before = snapshot_tree(&music);
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/roots",
        &headers,
        Some(json!({"id": "music", "path": music.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Scan, then drive the supervisor until idle.
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/scan",
        &headers,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["run_id"].as_str().expect("run id").to_owned();
    drain_scans(&lib.library).await;
    let (status, body) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/scan/runs/{run_id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["run"]["state"], json!("completed"));
    assert_eq!(
        body["run"]["counters"]["indexed_count"].as_i64(),
        Some(2),
        "exactly the two planted tracks index"
    );
    let files = body["files"].as_array().expect("run files");
    assert_eq!(files.len(), 2);
    let track_a = files
        .iter()
        .find(|file| file["relative_path"] == json!("album-a/01.flac"))
        .and_then(|file| file["track_id"].as_str())
        .expect("track a id")
        .to_owned();
    let track_b = files
        .iter()
        .find(|file| file["relative_path"] == json!("album-a/02.flac"))
        .and_then(|file| file["track_id"].as_str())
        .expect("track b id")
        .to_owned();

    // Purity: scan plus watcher work wrote zero media bytes.
    lib.library.watcher_tick().await;
    assert_eq!(snapshot_tree(&music), bytes_before);
    assert_eq!(read_title(&file_a), title_a_before);
    assert_eq!(read_title(&file_b), title_b_before);

    // Identification keys on the catalog album the reads API serves.
    let (status, body) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/tracks/{track_a}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let album_id = body["album_id"].as_str().expect("album id").to_owned();
    let album_key = album_id.as_str();

    // Scripted providers tie two candidates: the scan-enqueued job
    // must file an ambiguous review, never guess.
    lib.library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&track_a, &track_b));
    let attempted = lib.library.identify_tick().await;
    assert_eq!(attempted, 1, "scan-enqueued job runs once");
    // Purity extends past identify: tag reads and probes wrote zero
    // media bytes too.
    assert_eq!(
        snapshot_tree(&music),
        bytes_before,
        "identify reads wrote files"
    );
    let (status, body) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/reviews?album_id={album_key}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reviews = body["reviews"].as_array().expect("reviews");
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0]["reason_code"], json!("AMBIGUOUS_CANDIDATES"));
    assert_eq!(
        reviews[0]["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        2
    );
    let review_id = reviews[0]["id"].as_str().expect("review id").to_owned();

    // A manual re-enqueue is accepted (job queued); drained with
    // empty recall so it terminates without filing again.
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/identify",
        &headers,
        Some(json!({"album_id": album_key, "kind": "manual"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    lib.library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(RecallResult::default());
    assert_eq!(lib.library.identify_tick().await, 1);

    // Curator approval seals a manual identity.
    let (status, body) = call(
        lib.router(),
        "POST",
        &format!("/api/v3/library/reviews/{review_id}/approve"),
        &headers,
        Some(json!({"candidate_key": "rg-1:rel-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["review"]["state"], json!("approved"));
    assert_eq!(body["identity"]["release_mbid"], json!("rel-1"));
    assert_eq!(body["identity"]["decision_source"], json!("manual"));

    // Auto-organize: preview, apply, and the file moves with new tags.
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/manage/preview",
        &headers,
        Some(json!({
            "kind": "organize",
            "album_id": album_key,
            "items": [{
                "root_id": "music",
                "rel_path": "album-a/01.flac",
                "dest_rel": "organized/01.flac",
                "managed_updates": {"title": ["Organized Title"]},
            }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["preview_token"].as_str().expect("token").to_owned();
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/manage/apply",
        &headers,
        Some(json!({"preview_token": token})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], json!("committed"));
    let bundle_id = body["bundle_id"].as_str().expect("bundle").to_owned();
    let moved = music.join("organized/01.flac");
    assert!(moved.is_file(), "organized file lands");
    assert!(!file_a.exists(), "source leaves after commit");
    assert_eq!(read_title(&moved), "Organized Title");

    // Undo restores the exact before state: path and tags.
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/manage/undo",
        &headers,
        Some(json!({"bundle_id": bundle_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restored"], json!([track_a]));
    assert!(file_a.is_file(), "source returns after undo");
    assert!(!moved.exists(), "destination clears after undo");
    assert_eq!(read_title(&file_a), title_a_before);
}

#[tokio::test]
async fn library_journey_retag_apply_baseline_restore() {
    let lib = Lib::open("retag").await;
    let admin = setup_admin(lib.router(), "owner", "owner-password-1").await;
    let auth = bearer(&admin);
    let headers = [("authorization", auth.as_str())];

    let music = lib.dir.join("music");
    let file = plant(&music, "album-b/01.flac", "management_full.flac");
    let original_title = read_title(&file);
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/roots",
        &headers,
        Some(json!({"id": "music", "path": music.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/scan",
        &headers,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["run_id"].as_str().expect("run id").to_owned();
    drain_scans(&lib.library).await;
    let (status, body) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/scan/runs/{run_id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let track_id = body["files"][0]["track_id"]
        .as_str()
        .expect("track id")
        .to_owned();

    // Identify through a tied review, then approve the winner.
    let (status, body) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/tracks/{track_id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let album_id = body["album_id"].as_str().expect("album id").to_owned();
    let album_key = album_id.as_str();
    lib.library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&track_id, &track_id));
    assert_eq!(lib.library.identify_tick().await, 1);
    let (status, body) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/reviews?album_id={album_key}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let review_id = body["reviews"][0]["id"]
        .as_str()
        .expect("review")
        .to_owned();
    let (status, _) = call(
        lib.router(),
        "POST",
        &format!("/api/v3/library/reviews/{review_id}/approve"),
        &headers,
        Some(json!({"candidate_key": "rg-1:rel-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Manual retag preview, then apply. The apply holds the
    // filesystem write lease, which the revision bump proves.
    let rev_before = lib.library.fs.revision("music");
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/manage/preview",
        &headers,
        Some(json!({
            "kind": "retag",
            "album_id": album_key,
            "items": [{
                "root_id": "music",
                "rel_path": "album-b/01.flac",
                "managed_updates": {"title": ["Retagged Title"]},
            }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["preview_token"].as_str().expect("token").to_owned();
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/manage/apply",
        &headers,
        Some(json!({"preview_token": token})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(read_title(&file), "Retagged Title");
    assert!(
        lib.library.fs.revision("music") > rev_before,
        "apply holds the write lease across refresh plus publish"
    );

    // Baseline restore returns the pre-management tags.
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/manage/baseline/restore",
        &headers,
        Some(json!({"track_ids": [track_id]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restored"], json!([track_id]));
    assert_eq!(read_title(&file), original_title);
}

#[tokio::test]
async fn library_loops_start_and_stop() {
    let lib = Lib::open("loops").await;
    lib.library
        .run_recovery()
        .await
        .expect("recovery runs clean");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let loops = lib.library.spawn_loops(shutdown_rx);
    assert_eq!(loops.len(), 5);
    let names: Vec<&str> = loops.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        vec![
            "library-scan",
            "library-watcher",
            "library-identify",
            "library-contrib",
            "library-publish",
        ]
    );
    // One tick proves the loops are alive; shutdown must be prompt.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    shutdown_tx.send(true).expect("shutdown sends");
    for (name, handle) in loops {
        tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .unwrap_or_else(|_| panic!("{name} loop stops promptly"))
            .expect("loop joins");
    }
}

/// Library roots live in the settings, so a restart comes back with the
/// same roots under the same ids, and the next scan finds nothing new.
#[tokio::test]
async fn library_roots_survive_a_restart() {
    use droppedneedle::library::scan::{EffectivePolicy, ScanKind, ScanRequest, ScanTrigger};

    let scratch = ScratchDir::new("lib-restart");
    let dir = scratch.to_path_buf();
    let db_path = dir.join("app.db");
    let connection = droppedneedle::db::open_connection(&db_path).expect("scratch db opens");
    droppedneedle::schema::apply_migrations_blocking(&connection).expect("migrations apply");
    drop(connection);
    let open_config = || {
        Arc::new(
            ConfigStore::open(
                &dir.join("config.json"),
                Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            )
            .expect("config opens"),
        )
    };
    let users = || {
        droppedneedle::auth::wiring::AuthSetup::for_tests()
            .expect("test auth builds")
            .users
    };
    let ids = || Arc::new(UuidGenerator) as Arc<dyn IdGenerator>;
    let music = dir.join("music");
    plant(&music, "album/01.flac", "flac_full_01.flac");

    let first = LibrarySetup::for_tests_at(users(), ids(), &db_path, open_config())
        .expect("first bundle builds");
    first
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    drain_scans(&first).await;
    drop(first);

    let second = LibrarySetup::for_tests_at(users(), ids(), &db_path, open_config())
        .expect("second bundle builds");
    let registry = second.live_registry();
    assert!(registry.enabled());
    assert_eq!(registry.roots().len(), 1);
    assert_eq!(registry.roots()[0].id, "music");
    assert_eq!(registry.roots()[0].path, music);
    // The same directory, or one inside it, cannot become a second root.
    for overlapping in [music.clone(), music.join("album")] {
        let refused = second.add_root(
            None,
            overlapping.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        );
        assert!(
            matches!(
                refused,
                Err(droppedneedle::library::service::ServiceError::Conflict { .. })
            ),
            "overlapping root refused"
        );
    }
    let result = second
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    drain_scans(&second).await;
    let (run, _, _) = second.coordinator.snapshot(&result.run_id).expect("run");
    assert_eq!(run.counters.get("new_count").copied(), Some(0));
    assert_eq!(run.counters.get("unchanged_count").copied(), Some(1));
}

/// Identification state is durable: a job the scan queued survives a
/// restart, and a curator's approval stays a manual identity after the
/// next one.
#[tokio::test]
async fn identification_survives_restarts() {
    use droppedneedle::library::identify::models::DecisionSource;
    use droppedneedle::library::identify::stores::IdentityStore as _;
    use droppedneedle::library::scan::{CatalogStore as _, EffectivePolicy};

    let scratch = ScratchDir::new("lib-identify-restart");
    let dir = scratch.to_path_buf();
    let db_path = dir.join("app.db");
    let connection = droppedneedle::db::open_connection(&db_path).expect("scratch db opens");
    droppedneedle::schema::apply_migrations_blocking(&connection).expect("migrations apply");
    drop(connection);
    let config = Arc::new(
        ConfigStore::open(
            &dir.join("config.json"),
            Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
        )
        .expect("config opens"),
    );
    let bundle = || {
        LibrarySetup::for_tests_at(
            droppedneedle::auth::wiring::AuthSetup::for_tests()
                .expect("test auth builds")
                .users,
            Arc::new(UuidGenerator) as Arc<dyn IdGenerator>,
            &db_path,
            Arc::clone(&config),
        )
        .expect("bundle builds")
    };
    let music = dir.join("music");
    plant(&music, "album/01.flac", "flac_full_01.flac");

    let first = bundle();
    first
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    drain_scans(&first).await;
    let track = first
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = first.scan_store.album_for_track(&track).expect("album");
    drop(first);

    // The scan-queued job is still there after a restart.
    let second = bundle();
    second.run_recovery().await.expect("recovery runs");
    second
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&track, &track));
    assert_eq!(second.identify_tick().await, 1, "queued job survives");
    let review = second.pending_reviews(&album);
    assert_eq!(review.len(), 1);
    second
        .approve_review(&review[0].id, "curator", "rg-1:rel-1")
        .expect("review approves");
    drop(second);

    let third = bundle();
    let identity = third
        .identify_store
        .album_identity(&album)
        .expect("identity survives");
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.release_mbid.as_deref(), Some("rel-1"));
}
