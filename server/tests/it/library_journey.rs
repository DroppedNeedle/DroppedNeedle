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
use droppedneedle::library::identify::models::RecallResult;
use droppedneedle::library::matching::{CreditedArtist, Release, ReleaseTrack};
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
        let reads = ReadsSetup::build(
            self.runtime.pool(),
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            crate::common::reads_inputs(
                Arc::clone(&self.store),
                self.db_path.parent().expect("db dir"),
            ),
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
            droppedneedle::concerts::ConcertsSetup::unwired(),
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

/// Two release groups that fit the album's files equally well, built
/// from the facts identification reads: the attempt must file an
/// ambiguous review, never guess. Both answer to the release MBID the
/// files carry (as if MusicBrainz had folded it into each), so the
/// album id cannot pick one.
fn ambiguous_recall(library: &LibrarySetup, album_id: &str) -> RecallResult {
    use droppedneedle::library::identify::stores::FactsSource as _;
    let facts = library
        .identify_store
        .album_facts(album_id)
        .expect("album facts");
    let tagged: Vec<String> = facts
        .tracks
        .iter()
        .filter_map(|track| track.release_mbid.clone())
        .collect();
    let release = |group: &str, id: &str| Release {
        id: id.to_owned(),
        release_group_id: group.to_owned(),
        title: facts.title.clone(),
        artists: vec![CreditedArtist {
            id: "artist-1".to_owned(),
            name: facts.album_artist_name.clone(),
            sort_name: None,
            join: String::new(),
        }],
        tracks: facts
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| ReleaseTrack {
                id: track
                    .release_track_mbid
                    .clone()
                    .unwrap_or_else(|| format!("{id}-rt-{index}")),
                recording_id: track
                    .recording_mbid
                    .clone()
                    .unwrap_or_else(|| format!("rec-{index}")),
                title: track.title.clone(),
                artists: Vec::new(),
                disc: track.disc_number.max(1),
                position: track.track_number,
                absolute_position: index as u32 + 1,
                length_ms: track.duration_secs.map(|seconds| seconds * 1000),
            })
            .collect(),
        old_ids: tagged.clone(),
        ..Release::default()
    };
    RecallResult {
        releases: vec![release("rg-1", "rel-1"), release("rg-2", "rel-2")],
        ..RecallResult::default()
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
        .set_recall(ambiguous_recall(&lib.library, album_key));
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

    // Auto-organize the whole album: preview, apply, and the files move
    // with new tags.
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/manage/preview",
        &headers,
        Some(json!({
            "kind": "organize",
            "album_id": album_key,
            "items": [
                {
                    "root_id": "music",
                    "rel_path": "album-a/01.flac",
                    "dest_rel": "organized/01.flac",
                    "managed_updates": {"title": ["Organized Title"]},
                },
                {
                    "root_id": "music",
                    "rel_path": "album-a/02.flac",
                    "dest_rel": "organized/02.flac",
                    "managed_updates": {"title": ["Organized Title"]},
                },
            ],
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

    // A rescan after the move re-reads the rewritten files and keeps them
    // on the same album, so the curator's identity still applies.
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/scan",
        &headers,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    drain_scans(&lib.library).await;
    {
        use droppedneedle::library::identify::stores::IdentityStore as _;
        use droppedneedle::library::scan::CatalogStore as _;
        for track in [&track_a, &track_b] {
            assert_eq!(
                lib.library.scan_store.album_for_track(track).as_deref(),
                Some(album_key),
                "an organized track stays on its album"
            );
        }
        let identity = lib
            .library
            .identify_store
            .album_identity(album_key)
            .expect("identity stays with the album");
        assert_eq!(identity.release_mbid.as_deref(), Some("rel-1"));
    }

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
    let mut restored: Vec<String> =
        serde_json::from_value(body["restored"].clone()).expect("restored ids");
    restored.sort();
    let mut expected = vec![track_a.clone(), track_b.clone()];
    expected.sort();
    assert_eq!(restored, expected);
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
    let fields_before = droppedneedle::library::tags::read_fields(&file).expect("fields read");
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
        .set_recall(ambiguous_recall(&lib.library, album_key));
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
    // The retag also wrote the approved release's tags.
    let written = droppedneedle::library::tags::read_fields(&file).expect("fields read");
    let release_field = droppedneedle::library::tags::TagField::MusicBrainzReleaseId;
    assert_eq!(written[&release_field], vec!["rel-1".to_owned()]);

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
    assert_eq!(
        droppedneedle::library::tags::read_fields(&file).expect("fields read"),
        fields_before,
        "every field the retag wrote is back"
    );
}

#[tokio::test]
async fn library_loops_start_and_stop() {
    let lib = Lib::open("loops").await;
    lib.library.run_recovery().await;
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

/// Scan controls and the activity feed through the HTTP API: the policy
/// guard, scan kinds, current runs, stop, history, estimates, failed
/// paths, and pausing identification.
#[tokio::test]
async fn library_journey_scan_controls_and_activity() {
    let lib = Lib::open("controls").await;
    let admin = setup_admin(lib.router(), "owner", "owner-password-1").await;
    let auth = bearer(&admin);
    let headers = [("authorization", auth.as_str())];
    let music = lib.dir.join("music");
    plant(&music, "album-a/01.flac", "management_full.flac");
    plant(&music, "album-a/02.flac", "management_full.flac");
    // Not audio at all: its tags cannot be read.
    std::fs::write(music.join("album-a/03.flac"), b"not a flac file").expect("junk file");
    let (status, body) = call(
        lib.router(),
        "POST",
        "/api/v3/library/roots",
        &headers,
        Some(json!({"id": "music", "path": music.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // The revision the settings page shows is the one requests carry.
    let settings = lib
        .store
        .get_masked::<droppedneedle::runtime_config::secret_sections::TypedLibrary>()
        .expect("library settings read")
        .into_inner();
    let revision = droppedneedle::settings::library_policy::resolve(&settings)
        .expect("settings resolve")
        .policy_revision;
    let post = |uri: String, body: Value| {
        let app = lib.router();
        async move { call(app, "POST", &uri, &headers, Some(body)).await }
    };
    let get = |uri: String| {
        let app = lib.router();
        async move { call(app, "GET", &uri, &headers, None).await }
    };

    // Stale settings and unknown scopes are refused before anything queues.
    let (status, _) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"kind": "rescan_files", "expected_policy_revision": "stale"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"scope_ids": ["gone"], "expected_policy_revision": revision}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A queued run shows as current and stops at once.
    let (status, body) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"kind": "incremental", "expected_policy_revision": revision}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let queued_id = body["run_id"].as_str().expect("run id").to_owned();
    let (status, body) = get("/api/v3/library/scan/runs/current".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"]["id"], json!(queued_id));
    let row_revision = body["queued"]["row_revision"].as_u64().expect("revision");
    let (status, _) = post(
        format!("/api/v3/library/scan/runs/{queued_id}/pause"),
        json!({"expected_revision": row_revision + 7}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "a stale revision is refused");
    let (status, body) = post(
        format!("/api/v3/library/scan/runs/{queued_id}/stop"),
        json!({"expected_revision": row_revision}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("cancelled"));

    // A full rescan runs to the end and lands in history.
    let (status, body) = get("/api/v3/library/scan/runs/estimate".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["estimated_file_count"], json!(0));
    let (status, body) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"kind": "rescan_files", "scope_ids": ["music"], "expected_policy_revision": revision}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let run_id = body["run_id"].as_str().expect("run id").to_owned();
    drain_scans(&lib.library).await;
    let (status, body) = get("/api/v3/library/scan/runs/history?limit=1".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], json!(run_id));
    assert_eq!(body["items"][0]["kind"], json!("rescan_files"));
    assert_eq!(body["items"][0]["state"], json!("completed"));
    let cursor = body["next_cursor"].as_str().expect("older page").to_owned();
    let (status, body) = get(format!(
        "/api/v3/library/scan/runs/history?limit=1&cursor={cursor}"
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], json!(queued_id));
    // The unreadable file is listed with its code, a plain reason and an
    // action.
    let (status, body) = get(format!("/api/v3/library/scan/runs/{run_id}/failures")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let failures = body["items"].as_array().expect("failures");
    assert_eq!(failures.len(), 1, "{body}");
    assert_eq!(failures[0]["relative_path"], json!("album-a/03.flac"));
    assert_eq!(failures[0]["failure_code"], json!("TAG_READ_FAILED"));
    assert!(
        failures[0]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("Can't read this file's tags"))
    );
    assert!(
        failures[0]["action"]
            .as_str()
            .is_some_and(|action| !action.is_empty())
    );
    let (status, _) = get("/api/v3/library/scan/runs/nope/failures".into()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = get("/api/v3/library/scan/runs/estimate?scope_ids=music".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["estimated_file_count"], json!(2));

    // The scan queued the album for identification; pausing holds it.
    let (status, body) = get("/api/v3/library/activity".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for stream in ["scan", "identification", "operation", "catalog"] {
        assert!(body["revisions"][stream].is_u64(), "{stream} revision");
    }
    let card = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == json!("identification"))
        .expect("identification card")
        .clone();
    assert_eq!(card["waiting_count"], json!(1));
    let control = card["control_revision"].as_u64().expect("control revision");
    let (status, body) = post(
        "/api/v3/library/identification/pause".into(),
        json!({"expected_revision": control}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("paused"));
    let paused_at = body["row_revision"].as_u64().expect("revision");
    assert_eq!(
        lib.library.identify_tick().await,
        0,
        "paused queue claims nothing"
    );
    let (status, _) = post(
        "/api/v3/library/identification/resume".into(),
        json!({"expected_revision": control}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "a stale switch is refused");
    let (status, body) = post(
        "/api/v3/library/identification/resume".into(),
        json!({"expected_revision": paused_at}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("running"));
    assert_eq!(lib.library.identify_tick().await, 1, "resumed queue runs");
}

/// A bare library bundle over a migrated scratch database, with its
/// music folder (not yet a root).
fn bare_library(tag: &str) -> (ScratchDir, LibrarySetup, PathBuf) {
    let scratch = ScratchDir::new(tag);
    let dir = scratch.to_path_buf();
    let connection =
        droppedneedle::db::open_connection(&dir.join("app.db")).expect("scratch db opens");
    droppedneedle::schema::apply_migrations_blocking(&connection).expect("migrations apply");
    drop(connection);
    let library = open_library(&dir);
    (scratch, library, dir.join("music"))
}

/// A library bundle over the database and settings in `dir`, as a
/// restart would build it.
fn open_library(dir: &Path) -> LibrarySetup {
    let config = Arc::new(
        ConfigStore::open(
            &dir.join("config.json"),
            Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
        )
        .expect("config opens"),
    );
    LibrarySetup::for_tests_at(
        droppedneedle::auth::wiring::AuthSetup::for_tests()
            .expect("test auth builds")
            .users,
        Arc::new(UuidGenerator) as Arc<dyn IdGenerator>,
        &dir.join("app.db"),
        config,
    )
    .expect("bundle builds")
}

/// Add `music` as the only root and scan it.
async fn add_music_root(library: &LibrarySetup, music: &Path) {
    library
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            droppedneedle::library::scan::EffectivePolicy::Automatic,
        )
        .expect("root adds");
    drain_scans(library).await;
}

/// Scan every root again and wait for it.
async fn rescan(library: &LibrarySetup) {
    use droppedneedle::library::scan::{ScanKind, ScanRequest, ScanTrigger};
    let registry = library.live_registry();
    library
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    drain_scans(library).await;
}

/// A file moved by hand keeps its track id on the next scan, so the
/// favorites and play history that name the track follow the file.
#[tokio::test]
async fn moved_file_keeps_its_track() {
    use droppedneedle::library::scan::CatalogStore as _;

    let (_scratch, library, music) = bare_library("lib-move");
    let old = plant(&music, "before/01.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "before/01.flac")
        .expect("track indexed");
    library
        .scan_store
        .execute_batch_for_tests(&format!(
            "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('listener', 'track', '{track}', 0); \
             INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
             artist_name, played_at) VALUES ('play-1', 'listener', '{track}', 'Song', \
             'Artist', '2026-01-01T00:00:00Z');"
        ))
        .expect("favorite and play seed");

    let new = music.join("after/01.flac");
    std::fs::create_dir_all(new.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&old, &new).expect("move by hand");
    rescan(&library).await;

    assert_eq!(
        library.scan_store.track_at("music", "after/01.flac"),
        Some(track.clone()),
        "the moved file continues its track"
    );
    let following = library
        .scan_store
        .query_i64_for_tests(&format!(
            "SELECT (SELECT COUNT(*) FROM library_user_favorites f JOIN local_tracks t \
             ON t.id = f.item_id WHERE t.id = '{track}' AND t.availability = 'indexed') \
             + (SELECT COUNT(*) FROM library_play_history h JOIN local_tracks t \
             ON t.id = h.local_track_id WHERE t.id = '{track}' \
             AND t.relative_path = 'after/01.flac' AND t.availability = 'indexed')"
        ))
        .expect("count reads");
    assert_eq!(following, 2, "favorite and history follow the file");
    assert_eq!(
        library
            .scan_store
            .query_i64_for_tests("SELECT COUNT(*) FROM local_tracks")
            .expect("count reads"),
        1,
        "no second row for the moved file"
    );
}

/// Two copies of one album in different folders stay two albums (the
/// duplicate tools compare them), and moving one copy keeps its own ids.
#[tokio::test]
async fn copies_stay_apart_and_keep_ids_when_moved() {
    use droppedneedle::library::scan::CatalogStore as _;

    let (_scratch, library, music) = bare_library("lib-copies");
    let copy_a = plant(&music, "flac/01.flac", "flac_full_01.flac");
    let copy_b = plant(&music, "lossy/01.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let track_a = library
        .scan_store
        .track_at("music", "flac/01.flac")
        .expect("copy a indexed");
    let track_b = library
        .scan_store
        .track_at("music", "lossy/01.flac")
        .expect("copy b indexed");
    let album_a = library
        .scan_store
        .album_for_track(&track_a)
        .expect("album a");
    let album_b = library
        .scan_store
        .album_for_track(&track_b)
        .expect("album b");
    assert_ne!(album_a, album_b, "copies are separate albums");

    let moved = music.join("archive/01.flac");
    std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&copy_b, &moved).expect("move one copy");
    rescan(&library).await;

    assert_eq!(
        library.scan_store.track_at("music", "archive/01.flac"),
        Some(track_b.clone()),
        "the moved copy keeps its track"
    );
    assert_eq!(library.scan_store.album_for_track(&track_b), Some(album_b));
    assert_eq!(library.scan_store.album_for_track(&track_a), Some(album_a));
    assert!(copy_a.is_file());
}

/// An album whose files went away in an earlier scan is not handed to a
/// new album that happens to share its names: that one is another
/// release, and the old identity stays where it was.
#[tokio::test]
async fn long_missing_album_is_not_taken_over() {
    use droppedneedle::library::scan::CatalogStore as _;
    use droppedneedle::library::tags::TagField;
    use droppedneedle::library::tags::save::{TagEdit, save_tags};

    let (_scratch, library, music) = bare_library("lib-no-takeover");
    let green = plant(&music, "green/01.flac", "flac_full_02.flac");
    // Something stays, so the walk is not an empty mount point.
    plant(&music, "keep/01.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    let old_track = library
        .scan_store
        .track_at("music", "green/01.flac")
        .expect("track indexed");
    let old_album = library
        .scan_store
        .album_for_track(&old_track)
        .expect("album");
    std::fs::remove_file(&green).expect("delete");
    rescan(&library).await;
    assert_eq!(library.scan_store.track_at("music", "green/01.flac"), None);

    // Another release under the same names: other songs, no shared ids.
    let red = plant(&music, "red/01.flac", "flac_compilation_01.flac");
    save_tags(
        &red,
        &[
            TagEdit::new(TagField::Album, vec!["OK Computer".to_owned()]),
            TagEdit::new(TagField::AlbumArtist, vec!["Radiohead".to_owned()]),
        ],
    )
    .expect("renamed");
    rescan(&library).await;

    let new_track = library
        .scan_store
        .track_at("music", "red/01.flac")
        .expect("new track indexed");
    assert_ne!(new_track, old_track);
    let new_album = library
        .scan_store
        .album_for_track(&new_track)
        .expect("album");
    assert_ne!(new_album, old_album, "a long-gone album is not reused");
}

/// A share that comes up empty (unmounted) while a copy of one of its
/// files appears in another root: the guard holds the share back, so its
/// tracks keep their ids and stay available, and the copy gets its own row.
/// A small root that comes up empty is held back the same way.
#[tokio::test]
async fn unmounted_share_keeps_its_tracks() {
    use droppedneedle::library::scan::{CatalogStore as _, EffectivePolicy};

    let (scratch, library, local) = bare_library("lib-share-guard");
    let share = scratch.join("share");
    let shared = plant(&share, "album/01.flac", "flac_full_01.flac");
    // Enough tracks for the mass-missing guard to apply.
    for n in 2..=20 {
        plant(&share, &format!("other/{n:02}.flac"), "flac_no_tags.flac");
    }
    let small = scratch.join("small");
    for n in 1..=3 {
        plant(&small, &format!("{n:02}.flac"), "flac_no_tags.flac");
    }
    std::fs::create_dir_all(&local).expect("local root");
    for (id, path) in [("share", &share), ("local", &local), ("small", &small)] {
        library
            .add_root(
                Some(id.to_owned()),
                path.to_string_lossy().into_owned(),
                EffectivePolicy::Automatic,
            )
            .expect("root adds");
    }
    drain_scans(&library).await;
    let track = library
        .scan_store
        .track_at("share", "album/01.flac")
        .expect("share track indexed");

    // The share goes dark; a copy of one of its files lands locally.
    for root in [&share, &small] {
        std::fs::remove_dir_all(root).expect("unmount");
        std::fs::create_dir_all(root).expect("empty mount point");
    }
    plant(&local, "album/01.flac", "flac_full_01.flac");
    assert!(!shared.exists());
    rescan(&library).await;

    assert_eq!(
        library.scan_store.track_at("share", "album/01.flac"),
        Some(track.clone()),
        "the share's track keeps its id and stays available"
    );
    let copy = library
        .scan_store
        .track_at("local", "album/01.flac")
        .expect("copy indexed");
    assert_ne!(copy, track, "the copy gets its own row");
    assert!(
        library.scan_store.track_at("small", "01.flac").is_some(),
        "an emptied small root keeps its tracks"
    );
    assert_eq!(
        library
            .scan_store
            .query_i64_for_tests("SELECT COUNT(*) FROM local_tracks WHERE availability = 'missing'")
            .expect("count reads"),
        0,
        "nothing is marked missing"
    );
}

/// Retagging every track of an album to a new name keeps the album row,
/// and with it the curator's identity.
#[tokio::test]
async fn retagged_album_keeps_its_identity() {
    use droppedneedle::library::scan::CatalogStore as _;
    use droppedneedle::library::tags::TagField;
    use droppedneedle::library::tags::save::{TagEdit, save_tags};

    let (_scratch, library, music) = bare_library("lib-retag-album");
    // No release MBID here: the album is known by its tagged names.
    let files = [
        plant(&music, "album/01.flac", "flac_full_02.flac"),
        plant(&music, "album/02.flac", "flac_full_02.flac"),
    ];
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .scan_store
        .execute_batch_for_tests(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', 'rg-1', 'manual', 0);"
        ))
        .expect("identity seed");

    for file in &files {
        save_tags(
            file,
            &[TagEdit::new(TagField::Album, vec!["Renamed".to_owned()])],
        )
        .expect("album retagged");
    }
    rescan(&library).await;

    assert_eq!(
        library.scan_store.album_for_track(&track).as_deref(),
        Some(album.as_str()),
        "the retagged album keeps its row"
    );
    assert_eq!(
        library
            .scan_store
            .query_i64_for_tests(&format!(
                "SELECT COUNT(*) FROM local_albums WHERE id = '{album}' AND title = 'Renamed'"
            ))
            .expect("count reads"),
        1,
        "the kept row takes the new name"
    );
    assert_eq!(
        library
            .scan_store
            .query_i64_for_tests("SELECT COUNT(*) FROM local_albums")
            .expect("count reads"),
        1,
        "no second album"
    );
}

/// An approval that cannot seal its identity fails as a whole: the
/// caller gets the error and the review stays pending.
#[tokio::test]
async fn failed_approval_leaves_the_review_pending() {
    use droppedneedle::library::scan::CatalogStore as _;
    use droppedneedle::library::service::ServiceError;

    let (_scratch, library, music) = bare_library("lib-approve-fail");
    plant(&music, "album/01.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library, &album));
    assert_eq!(library.identify_tick().await, 1);
    let review = library.pending_reviews(&album);
    assert_eq!(review.len(), 1);
    library
        .scan_store
        .execute_batch_for_tests(
            "CREATE TRIGGER refuse_identity BEFORE INSERT \
             ON local_album_external_identities BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .expect("trigger installs");

    let refused = library.approve_review(&review[0].id, "curator", "rg-1:rel-1");
    assert!(
        matches!(refused, Err(ServiceError::Internal { .. })),
        "a store failure is an error, not an approval"
    );
    assert_eq!(
        library.pending_reviews(&album).len(),
        1,
        "review stays pending"
    );
}

/// A publish journal for a root that is gone never stops boot: recovery
/// finishes, leaves the bundle as it is for a later pass (its root may
/// come back), and reports it held.
#[tokio::test]
async fn journal_on_a_removed_root_waits() {
    let (scratch, first, music) = bare_library("lib-journal-gone");
    plant(&music, "album/01.flac", "flac_full_01.flac");
    add_music_root(&first, &music).await;
    first
        .scan_store
        .execute_batch_for_tests(
            "INSERT INTO library_publish_journal (id, bundle_id, kind, source_root, \
             source_rel, dest_root, dest_rel, staged, staged_sha256, state) \
             VALUES ('j-1', 'bundle-1', 'audio', 'removed', 'a/01.flac', 'removed', \
             'b/01.flac', '/nowhere/.staged', 'abc', 'cleanup_pending');",
        )
        .expect("journal seed");
    drop(first);

    let library = open_library(&scratch);
    let recovery = library.run_recovery().await;

    assert_eq!(recovery.publish_recoveries.len(), 1);
    assert!(recovery.publish_recoveries[0].1.starts_with("Deferred"));
    assert_eq!(
        library.held_publish_bundles().expect("held reads"),
        vec!["bundle-1".to_owned()]
    );
    assert_eq!(
        library
            .scan_store
            .query_i64_for_tests(
                "SELECT COUNT(*) FROM library_publish_journal \
                 WHERE id = 'j-1' AND state = 'cleanup_pending'"
            )
            .expect("state reads"),
        1,
        "the bundle is left as it was"
    );
    library.publish_tick().expect("maintenance retries");
    assert_eq!(
        library.held_publish_bundles().expect("held reads"),
        vec!["bundle-1".to_owned()]
    );
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
    second.run_recovery().await;
    second
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&second, &album));
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

/// Identify the album by approving the first of two tied candidates.
async fn approve_first_candidate(library: &LibrarySetup, album: &str) {
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(library, album));
    assert_eq!(library.identify_tick().await, 1);
    let review = library.pending_reviews(album);
    library
        .approve_review(&review[0].id, "curator", "rg-1:rel-1")
        .expect("review approves");
}

/// Preview and apply one managed write; returns the bundle id.
fn publish_one(
    library: &LibrarySetup,
    kind: droppedneedle::library::publish::planner::PlanKind,
    album: &str,
    rel_path: &str,
    dest_rel: Option<&str>,
    updates: &[(&str, &str)],
) -> String {
    use droppedneedle::library::manage::PreviewItemInput;
    let sealed = library
        .plan_preview(
            kind,
            album,
            vec![PreviewItemInput {
                root_id: "music".to_owned(),
                rel_path: rel_path.to_owned(),
                dest_rel: dest_rel.map(str::to_owned),
                managed_updates: updates
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), vec![(*value).to_owned()]))
                    .collect(),
            }],
        )
        .expect("preview seals");
    library
        .apply_preview(&sealed.token)
        .expect("apply publishes")
        .bundle_id
}

/// Values a new edit could never carry (a `3/12` track number, a
/// free-text date) come back exactly when a retag is undone.
#[tokio::test]
async fn undo_restores_values_no_new_edit_could_write() {
    use droppedneedle::library::publish::planner::PlanKind;
    use droppedneedle::library::scan::CatalogStore as _;
    use droppedneedle::library::tags::save::{TagEdit, save_tags};
    use droppedneedle::library::tags::{TagField, read_fields};

    let (_scratch, library, music) = bare_library("lib-undo-raw");
    let file = plant(&music, "album/01.flac", "management_full.flac");
    save_tags(
        &file,
        &[
            TagEdit::verbatim(TagField::TrackNumber, vec!["3/12".to_owned()]),
            TagEdit::verbatim(TagField::Date, vec!["circa 1970".to_owned()]),
        ],
    )
    .expect("odd values planted");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    approve_first_candidate(&library, &album).await;
    let before = read_fields(&file).expect("fields read");

    let bundle = publish_one(
        &library,
        PlanKind::SamePath,
        &album,
        "album/01.flac",
        None,
        &[("date", "2001")],
    );
    let retagged = read_fields(&file).expect("fields read");
    assert_eq!(retagged[&TagField::Date], vec!["2001".to_owned()]);
    assert_ne!(retagged[&TagField::TrackNumber], vec!["3/12".to_owned()]);

    library.undo_bundle(&bundle).expect("undo publishes");
    assert_eq!(read_fields(&file).expect("fields read"), before);
}

/// Baseline restore takes a file back to how it was before its first
/// managed write, removing fields a later write added: organize first
/// (no tags written), retag second, restore last.
#[tokio::test]
async fn baseline_restore_removes_fields_added_later() {
    use droppedneedle::library::publish::planner::PlanKind;
    use droppedneedle::library::scan::CatalogStore as _;
    use droppedneedle::library::tags::{TagField, read_fields};

    let (_scratch, library, music) = bare_library("lib-baseline-added");
    let file = plant(&music, "album/01.flac", "flac_full_02.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    approve_first_candidate(&library, &album).await;
    let before = read_fields(&file).expect("fields read");
    assert!(!before.contains_key(&TagField::MusicBrainzReleaseId));

    publish_one(
        &library,
        PlanKind::Move,
        &album,
        "album/01.flac",
        Some("moved/01.flac"),
        &[],
    );
    publish_one(
        &library,
        PlanKind::SamePath,
        &album,
        "moved/01.flac",
        None,
        &[],
    );
    let moved = music.join("moved/01.flac");
    let retagged = read_fields(&moved).expect("fields read");
    assert_eq!(
        retagged[&TagField::MusicBrainzReleaseId],
        vec!["rel-1".to_owned()]
    );

    library
        .baseline_restore(std::slice::from_ref(&track))
        .expect("restore publishes");
    assert!(file.is_file(), "the file is back where it started");
    assert_eq!(read_fields(&file).expect("fields read"), before);
}

/// A release document answers to its merged ids through their own rows,
/// and a refetch replaces every row rather than adding to them.
#[test]
fn release_documents_answer_to_merged_ids() {
    use droppedneedle::library::identify::stores::ReleaseStore as _;
    let (_scratch, library, _music) = bare_library("lib-merged-release");
    let store = &library.identify_store;
    let mut release = Release {
        id: "rel-new".to_owned(),
        title: "Merged".to_owned(),
        old_ids: vec!["REL-OLD".to_owned()],
        ..Release::default()
    };
    store.save_release(&release);
    release.title = "Refetched".to_owned();
    store.save_release(&release);
    for id in ["rel-new", "rel-old"] {
        let found = store.release(id, None).expect("document found");
        assert_eq!(found.id, "rel-new");
        assert_eq!(found.title, "Refetched");
    }
    assert!(store.release("rel-other", None).is_none());
}

/// Contribution journey through the real routes and the SQLite store:
/// draft, duplicate check, seeded editor, the release editor coming back
/// on the v1 callback path, verification, and the album linked.
#[tokio::test]
async fn library_contribution_seed_callback_verify_links_album() {
    use droppedneedle::library::contrib::models::{
        MusicBrainzVerifiedRelease, MusicBrainzVerifiedTrack, VerificationOutcome,
    };

    const RELEASE: &str = "11111111-1111-4111-8111-111111111111";
    const GROUP: &str = "22222222-2222-4222-8222-222222222222";
    // Same title and artist, a different tracklist.
    const WRONG: &str = "44444444-4444-4444-8444-444444444444";
    const ARTIST_MBID: &str = "33333333-3333-4333-8333-333333333333";

    let lib = Lib::open("contrib").await;
    let admin = setup_admin(lib.router(), "owner", "owner-password-1").await;
    let auth = bearer(&admin);
    let headers = [("authorization", auth.as_str())];

    // One indexed two-track album, as a scan leaves it.
    let db = rusqlite::Connection::open(&lib.db_path).expect("db opens");
    db.execute_batch(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('artist-1', 'Test Artist', 'test artist', 'person', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_id, year, grouping_source, created_at, updated_at) \
         VALUES ('album-1', 'music', 'k', 'Test Album', 'test album', 'Test Artist', \
         'artist-1', 2024, 'automatic', 1, 1);",
    )
    .expect("album seeds");
    for (id, number, title) in [("t1", 1, "First Song"), ("t2", 2, "Second Song")] {
        db.execute(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, tag_revision, title, \
             title_folded, artist_name, album_title, album_title_folded, track_number, \
             duration_seconds, file_format, ingest_source, imported_at, membership_source) \
             VALUES (?1, 'album-1', 'music', ?2, ?2, ?1, 1, 1, ?1, ?1, ?3, lower(?3), \
             'Test Artist', 'Test Album', 'test album', ?4, 200.0, 'flac', 'scan', 1, \
             'automatic')",
            rusqlite::params![id, format!("album/{id}.flac"), title, number],
        )
        .expect("track seeds");
    }

    let (status, created) = call(
        lib.router(),
        "POST",
        "/api/v3/library/albums/album-1/contributions",
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["state"], json!("draft"));
    let id = created["id"].as_str().expect("contribution id").to_owned();
    // The album view carries the open contribution for the album page.
    let (status, album) = call(
        lib.router(),
        "GET",
        "/api/v3/library/albums/album-1",
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{album}");
    assert_eq!(album["contribution_id"], json!(id));
    assert_eq!(album["contribution_state"], json!("draft"));
    let (status, ready) = call(
        lib.router(),
        "PUT",
        &format!("/api/v3/library/contributions/{id}/draft"),
        &headers,
        Some(json!({"expected_row_revision": created["row_revision"], "draft": created["draft"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(ready["state"], json!("ready"));

    // A stale revision answers with a code, a sentence and an action.
    let (status, stale) = call(
        lib.router(),
        "POST",
        &format!("/api/v3/library/contributions/{id}/musicbrainz/duplicates"),
        &headers,
        Some(json!({"expected_row_revision": created["row_revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], json!("CONTRIBUTION_CHANGED"));
    assert!(stale["error"]["details"]["action"].is_string());

    let (status, checked) = call(
        lib.router(),
        "POST",
        &format!("/api/v3/library/contributions/{id}/musicbrainz/duplicates"),
        &headers,
        Some(json!({"expected_row_revision": ready["row_revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["state"], json!("ready"));

    let origin = [
        ("authorization", auth.as_str()),
        ("origin", "https://music.example"),
    ];
    let (status, seed) = call(
        lib.router(),
        "POST",
        &format!("/api/v3/library/contributions/{id}/musicbrainz/seed"),
        &origin,
        Some(json!({"expected_row_revision": checked["row_revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{seed}");
    let redirect = seed["fields"]
        .as_array()
        .expect("seed fields")
        .iter()
        .find(|field| field["name"] == json!("redirect_uri"))
        .and_then(|field| field["value"].as_str())
        .expect("redirect uri")
        .to_owned();
    let token = redirect
        .strip_prefix(
            "https://music.example/api/v3/library/contributions/musicbrainz/callback?token=",
        )
        .expect("callback points at this server")
        .to_owned();

    // The editor sends the browser back, here to the v1 path, signed out.
    let back = |token: &str| {
        Request::builder()
            .uri(format!(
                "/api/v1/library/contributions/musicbrainz/callback?token={token}&release_mbid={WRONG}"
            ))
            .header("host", HOST)
            .body(Body::empty())
            .expect("request builds")
    };
    let response = lib.router().oneshot(back(&token)).await.expect("responds");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/library/contributions/{id}?musicbrainz=returned").as_str()
    );
    let reused = lib.router().oneshot(back(&token)).await.expect("responds");
    assert_eq!(
        reused.headers()["location"],
        "/library?musicbrainz=callback-error"
    );

    let providers = lib
        .library
        .test_contrib
        .as_ref()
        .expect("scripted providers");
    let release = |mbid: &str, titles: [&str; 2]| MusicBrainzVerifiedRelease {
        release_mbid: mbid.to_owned(),
        release_group_mbid: GROUP.to_owned(),
        title: "Test Album".to_owned(),
        artist_name: "Test Artist".to_owned(),
        artist_mbid: Some(ARTIST_MBID.to_owned()),
        tracks: titles
            .iter()
            .enumerate()
            .map(|(index, title)| MusicBrainzVerifiedTrack {
                title: (*title).to_owned(),
                position: index as i64 + 1,
                disc_number: 1,
                duration_seconds: Some(200.0),
                recording_mbid: Some(format!("{mbid}-rec-{index}")),
                release_track_mbid: Some(format!("{mbid}-track-{index}")),
            })
            .collect(),
        ..Default::default()
    };
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs_f64()
    };

    // The returned release shares the title but not the tracklist: the
    // matcher refuses it and says why.
    providers.musicbrainz.insert_verification(
        WRONG,
        Ok(Some(release(
            WRONG,
            ["Somebody Else", "Another Tune Entirely"],
        ))),
    );
    let outcome = lib
        .library
        .contrib_worker
        .run_once(now())
        .await
        .expect("worker runs");
    assert_eq!(outcome, Some(VerificationOutcome::NeedsReview));
    let (status, review) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/contributions/{id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{review}");
    assert_eq!(review["state"], json!("needs_review"));
    assert_eq!(
        review["review_reason"]["code"],
        json!("ATTACHMENT_WEAK_FIT")
    );
    assert!(review["review_reason"]["action"].is_string());

    // The curator records the right release; it fits and links.
    let (status, verifying) = call(
        lib.router(),
        "PUT",
        &format!("/api/v3/library/contributions/{id}/musicbrainz/result"),
        &headers,
        Some(json!({
            "expected_row_revision": review["row_revision"],
            "release_id_or_url": RELEASE,
            "replace_existing_result": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verifying}");
    assert_eq!(verifying["state"], json!("verifying"));
    providers.musicbrainz.insert_verification(
        RELEASE,
        Ok(Some(release(RELEASE, ["First Song", "Second Song"]))),
    );
    let outcome = lib
        .library
        .contrib_worker
        .run_once(now())
        .await
        .expect("worker runs");
    assert_eq!(outcome, Some(VerificationOutcome::Linked));
    let (status, linked) = call(
        lib.router(),
        "GET",
        &format!("/api/v3/library/contributions/{id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["state"], json!("linked"));
    let identity: (String, String, String) = db
        .query_row(
            "SELECT release_mbid, release_group_mbid, decision_source \
             FROM local_album_external_identities WHERE local_album_id = 'album-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("album identity committed");
    assert_eq!(identity, (RELEASE.into(), GROUP.into(), "manual".into()));
    let artist: String = db
        .query_row(
            "SELECT provider_artist_id FROM local_artist_external_identities \
             WHERE local_artist_id = 'artist-1'",
            [],
            |row| row.get(0),
        )
        .expect("artist identity committed");
    assert_eq!(artist, ARTIST_MBID);
    // Every file took its track on the release.
    let tracks: Vec<(String, String, String, i64, i64)> = db
        .prepare(
            "SELECT local_track_id, recording_mbid, release_track_mbid, medium_position, \
             release_track_position FROM local_track_external_identities \
             WHERE decision_source = 'manual' ORDER BY local_track_id",
        )
        .expect("query prepares")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query runs")
        .collect::<Result<_, _>>()
        .expect("rows read");
    assert_eq!(
        tracks,
        vec![
            (
                "t1".into(),
                format!("{RELEASE}-rec-0"),
                format!("{RELEASE}-track-0"),
                1,
                1
            ),
            (
                "t2".into(),
                format!("{RELEASE}-rec-1"),
                format!("{RELEASE}-track-1"),
                1,
                2
            ),
        ]
    );
}
