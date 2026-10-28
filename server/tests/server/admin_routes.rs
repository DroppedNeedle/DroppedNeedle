//! Admin-route briefs: gating, cache stats/clear, queue and
//! provider stats, quota admin, checkpoint health, and honest unwired 503s.
//!
//! The router mounts directly with sessions injected; health rides the full
//! app. Scratch runtimes only - no network, no production database.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::admin::{AdminDb, AdminSetup};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::db::{DbRuntime, Lane};
use droppedneedle_providers::{CacheKey, Source};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::{AuthRig, test_principal, with_test_principal};

/// One scratch deployment: migrated runtime, auth on it, wired admin.
struct Rig {
    /// Held: dropping it would close the pool out from under the admin
    /// handles.
    runtime: DbRuntime,
    admin: AdminSetup,
    rig: AuthRig,
    admin_id: String,
    user_id: String,
    cache: droppedneedle_providers::ProviderCacheStore,
    quota: Arc<droppedneedle_acquire::requests::quota::QuotaLedger>,
    jobs: droppedneedle::bootstrap::jobs::JobsSetup,
    /// The count the library bumps when a scan ends.
    scans: droppedneedle::bootstrap::jobs::ScansEnded,
    _scratch: crate::common::ScratchDir,
}

impl Rig {
    async fn open(tag: &str) -> Self {
        Self::open_with(tag, None).await
    }

    async fn open_with(
        tag: &str,
        precache: Option<Arc<dyn droppedneedle_kernel::jobs::precache_phases::PrecacheSources>>,
    ) -> Self {
        let scratch = crate::common::ScratchDir::new(&format!("admin-routes-{tag}"));
        let dir = scratch.to_path_buf();
        let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
        let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
            .build()
            .await;
        let rig = AuthRig::on(&platform);
        let admin_user = rig.seed_user("brenda", Role::Admin).await;
        let plain_user = rig.seed_user("molly", Role::User).await;
        let quota = Arc::new(
            droppedneedle_acquire::requests::quota::QuotaLedger::unlimited(
                droppedneedle_acquire::db::AcquireDb::new(
                    runtime.db().clone(),
                    platform.clock.clone(),
                ),
            ),
        );
        let providers = Arc::new(droppedneedle_providers::Providers::build(
            &platform,
            crate::common::closed_outbound().providers(),
            droppedneedle_kernel::db::cache::CacheDb::scratch(),
        ));
        let cache = providers.cache.clone();
        let scans = droppedneedle::bootstrap::jobs::ScansEnded::default();
        let mut jobs = droppedneedle::bootstrap::jobs::JobsSetup::build(
            &platform,
            runtime.checkpoint().clone(),
            crate::common::bundles::media_jobs(&platform),
            None,
            scans.clone(),
        );
        if let Some(sources) = precache {
            jobs = jobs.with_precache_sources(sources);
        }
        let admin = AdminSetup::new(rig.deps.clone(), quota.clone(), cache.clone(), providers)
            .with_db(AdminDb::new(runtime.db().clone()))
            .with_backups(droppedneedle_kernel::db::BackupService::new(
                &dir.join("app.db"),
                &dir.join("backups"),
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ))
            .with_checkpoint(runtime.checkpoint().clone())
            .with_precache(jobs.precache_trigger());
        Self {
            runtime,
            admin,
            rig,
            admin_id: admin_user.id,
            user_id: plain_user.id,
            cache,
            quota,
            jobs,
            scans,
            _scratch: scratch,
        }
    }

    /// Router signed in as `user_id` with `role`, or anonymous.
    fn app(&self, caller: Option<(&str, Role)>) -> Router {
        let router = droppedneedle::admin::routes()
            .with_state(self.admin.clone())
            .into_router();
        match caller {
            Some((user_id, role)) => with_test_principal(
                router,
                test_principal(user_id, role, "sess-1", SessionKind::Standard),
            ),
            None => router,
        }
    }

    fn admin_app(&self) -> Router {
        self.app(Some((&self.admin_id, Role::Admin)))
    }

    fn user_app(&self) -> Router {
        self.app(Some((&self.user_id, Role::User)))
    }
}

async fn call(app: Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(body.map_or_else(Body::empty, |json| {
            Body::from(serde_json::to_vec(&json).expect("body serializes"))
        }))
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

/// Every admin route with concrete ids.
fn routes(user_id: &str) -> Vec<(&'static str, String, Option<Value>)> {
    vec![
        ("GET", "/api/v3/admin/backups".to_owned(), None),
        ("POST", "/api/v3/admin/backups".to_owned(), None),
        (
            "GET",
            "/api/v3/admin/backups/library-0-0.db/restore-report".to_owned(),
            None,
        ),
        ("GET", "/api/v3/admin/cache/stats".to_owned(), None),
        (
            "POST",
            "/api/v3/admin/cache/clear".to_owned(),
            Some(json!({})),
        ),
        ("GET", "/api/v3/admin/queue-stats".to_owned(), None),
        ("GET", "/api/v3/admin/provider-stats".to_owned(), None),
        ("GET", format!("/api/v3/admin/users/{user_id}/quota"), None),
        (
            "PUT",
            format!("/api/v3/admin/users/{user_id}/quota"),
            Some(json!({})),
        ),
        ("POST", "/api/v3/admin/precache/run".to_owned(), None),
    ]
}

#[tokio::test]
async fn admin_gate_posts_its_posture_on_every_route() {
    let rig = Rig::open("gate").await;
    for (method, uri, body) in routes(&rig.user_id) {
        let (status, payload) = call(rig.app(None), method, &uri, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert_eq!(payload["error"]["code"], Value::from("UNAUTHORIZED"));

        let (status, payload) = call(rig.user_app(), method, &uri, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        assert_eq!(payload["error"]["code"], Value::from("FORBIDDEN"));

        let (status, _) = call(rig.admin_app(), method, &uri, body).await;
        assert!(
            !status.is_server_error()
                && status != StatusCode::UNAUTHORIZED
                && status != StatusCode::FORBIDDEN,
            "{method} {uri} admits the admin, got {status}"
        );
    }
}

/// Any signed-in user reads system health (every user sees the header dot),
/// in v2's wire shape, once a run of provider failures marks a service.
#[tokio::test]
async fn system_health_lists_degraded_services_for_any_user() {
    let rig = Rig::open("system-health").await;
    let (status, payload) = call(rig.user_app(), "GET", "/api/v3/system/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload, json!({"degraded": []}));

    let now = std::time::Instant::now();
    for _ in 0..5 {
        rig.admin.providers.health.record_failure("lastfm", now);
    }
    let (status, payload) = call(rig.user_app(), "GET", "/api/v3/system/health", None).await;
    assert_eq!(status, StatusCode::OK);
    let entry = &payload["degraded"][0];
    assert_eq!(entry["service"], "lastfm");
    assert_eq!(entry["capability"], "music data");
    assert_eq!(entry["severity"], "degraded");
    assert_eq!(entry["fallback"], Value::Null);
    assert!(entry["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(entry["degraded_seconds"].is_u64());
}

/// A failed checkpoint pass reaches the admin as a fixed code, never as
/// failure text that could carry a path.
#[tokio::test]
async fn queue_stats_reports_a_failed_checkpoint_as_a_code() {
    let mut rig = Rig::open("checkpoint-failed").await;
    let dir = rig._scratch.to_path_buf();
    let missing = dir.join("gone").join("app.db");
    let failing = droppedneedle_kernel::db::CheckpointService::new(
        &missing,
        rig.runtime.db().sqlx().clone(),
        Arc::default(),
        Arc::default(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    assert!(failing.run_once().error.is_some());
    rig.admin = rig.admin.clone().with_checkpoint(failing);

    let (status, payload) = call(rig.admin_app(), "GET", "/api/v3/admin/queue-stats", None).await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(payload["checkpoint"]["error"], "CHECKPOINT_FAILED");
    assert!(
        !payload.to_string().contains(&*dir.to_string_lossy()),
        "{payload}"
    );
}

#[tokio::test]
async fn cache_clear_provider_data_then_all() {
    let rig = Rig::open("cache-clear").await;
    let minute = Duration::from_secs(60);
    rig.cache.set_bytes(
        &CacheKey::new(Source::MusicBrainz, &["ns", "artist", "x"]),
        vec![1],
        minute,
    );
    rig.cache.set_bytes(
        &CacheKey::new(Source::LastFm, &["page_recent", "molly", "10"]).personal(),
        vec![2],
        minute,
    );
    // Puts are queued: a waited delete commits after them.
    rig.cache.forget(&[]).await;
    let (status, body) = call(rig.admin_app(), "GET", "/api/v3/admin/cache/stats", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["entries"], Value::from(2), "{body}");
    assert_eq!(body["provider_data_on"], Value::from(true));

    let (status, body) = call(
        rig.admin_app(),
        "POST",
        "/api/v3/admin/cache/clear",
        Some(json!({"scope": "provider_data"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cleared_entries"], Value::from(2));
    assert_eq!(body["remaining_entries"], Value::from(0));

    let (status, _) = call(
        rig.admin_app(),
        "POST",
        "/api/v3/admin/cache/clear",
        Some(json!({"scope": "source", "source": "musicbrainz"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "per-source clears are gone"
    );

    let (status, body) = call(
        rig.admin_app(),
        "POST",
        "/api/v3/admin/cache/clear",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cleared_entries"], Value::from(0));
}

/// Phases that never finish, holding a run open for the 409 brief.
#[derive(Clone)]
struct HangWork;

impl droppedneedle_kernel::jobs::precache::PrecacheWork for HangWork {
    fn run(
        &self,
        _progress: droppedneedle_kernel::jobs::precache::Progress,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { std::future::pending().await })
    }
}

#[tokio::test]
async fn precache_second_run_is_409_while_live() {
    let rig = Rig::open("precache-busy").await;
    // Hold the run's name directly on the supervisor: the route must see it.
    let _held = droppedneedle_kernel::jobs::precache::spawn_run(
        rig.jobs.supervisor(),
        HangWork,
        droppedneedle_kernel::jobs::precache::PrecacheLimits::new(
            Duration::from_secs(60),
            Duration::from_secs(600),
        ),
        |_| {},
    )
    .expect("held run starts");
    let (status, body) = call(rig.admin_app(), "POST", "/api/v3/admin/precache/run", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], Value::from("CONFLICT"));

    // Once the live run ends, the route starts again.
    rig.jobs
        .supervisor()
        .cancel(
            droppedneedle_kernel::jobs::precache::JOB_NAME,
            Duration::from_secs(5),
        )
        .await;
    let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/precache/run", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

const ALPHA: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const BETA: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const FIRST: &str = "cccccccc-0000-4000-8000-000000000003";

/// A two-artist, one-album library for the image refresh. Album warms
/// wait on `album_gate`, so the test can watch a run mid-flight.
struct ScriptedLibrary {
    calls: std::sync::Mutex<Vec<String>>,
    recorded: tokio::sync::Notify,
    album_gate: tokio::sync::Semaphore,
}

impl ScriptedLibrary {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
        self.recorded.notify_waiters();
    }

    /// Wait until `call` has been recorded.
    async fn wait_called(&self, call: &str) {
        loop {
            let recorded = self.recorded.notified();
            if self.called(call) {
                return;
            }
            recorded.await;
        }
    }

    fn called(&self, call: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|seen| seen == call)
    }

    fn times_called(&self, call: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| *seen == call)
            .count()
    }
}

impl droppedneedle_kernel::jobs::precache_phases::PrecacheSources for ScriptedLibrary {
    fn tuning(&self) -> droppedneedle_kernel::jobs::precache_phases::PrecacheTuning {
        droppedneedle_kernel::jobs::precache_phases::PrecacheTuning {
            artist_delay: Duration::ZERO,
            album_delay: Duration::ZERO,
            discovery_delay: Duration::ZERO,
            audiodb_delay: Duration::ZERO,
            ..Default::default()
        }
    }

    fn library(
        &self,
    ) -> droppedneedle_kernel::jobs::BoxFuture<
        '_,
        Result<droppedneedle_kernel::jobs::precache_phases::PrecacheLibrary, String>,
    > {
        use droppedneedle_kernel::jobs::precache_phases::{
            LibraryAlbum, LibraryArtist, PrecacheLibrary,
        };
        Box::pin(async {
            self.record("library".to_owned());
            Ok(PrecacheLibrary {
                artists: vec![
                    LibraryArtist {
                        mbid: ALPHA.to_owned(),
                        name: "Alpha".to_owned(),
                    },
                    LibraryArtist {
                        mbid: BETA.to_owned(),
                        name: "Beta".to_owned(),
                    },
                ],
                albums: vec![LibraryAlbum {
                    release_group_mbid: FIRST.to_owned(),
                    title: "First".to_owned(),
                    artist_name: "Alpha".to_owned(),
                }],
            })
        })
    }

    fn artist_cached<'a>(
        &'a self,
        _artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn warm_artist<'a>(
        &'a self,
        artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async move { self.record(format!("artist:{}", artist.mbid)) })
    }

    fn discovery_cached<'a>(
        &'a self,
        _user_id: &'a str,
        _artist_mbid: &'a str,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn warm_discovery<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async move { self.record(format!("discovery:{user_id}:{artist_mbid}")) })
    }

    fn album_cached<'a>(
        &'a self,
        _album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn warm_album<'a>(
        &'a self,
        album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async move {
            let _permit = self.album_gate.acquire().await;
            self.record(format!("album:{}", album.release_group_mbid));
        })
    }

    fn audiodb_artist_cached<'a>(
        &'a self,
        _artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn warm_audiodb_artist<'a>(
        &'a self,
        artist: &'a droppedneedle_kernel::jobs::precache_phases::LibraryArtist,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async move { self.record(format!("audiodb:{}", artist.mbid)) })
    }

    fn audiodb_album_cached<'a>(
        &'a self,
        _album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn warm_audiodb_album<'a>(
        &'a self,
        album: &'a droppedneedle_kernel::jobs::precache_phases::LibraryAlbum,
    ) -> droppedneedle_kernel::jobs::BoxFuture<'a, ()> {
        Box::pin(async move { self.record(format!("audiodb:{}", album.release_group_mbid)) })
    }
}

/// Scans that end within the quiet wait of each other start one library
/// image refresh, not one each.
#[tokio::test]
async fn a_burst_of_scans_starts_one_image_refresh() {
    let library = Arc::new(ScriptedLibrary {
        calls: std::sync::Mutex::new(Vec::new()),
        recorded: tokio::sync::Notify::new(),
        album_gate: tokio::sync::Semaphore::new(64),
    });
    let rig = Rig::open_with("scan-burst", Some(library.clone())).await;
    tokio::time::pause();
    let trigger = rig.jobs.precache_trigger();
    trigger.spawn_after_scan().expect("the wait loop starts");
    rig.scans.bump();
    tokio::time::sleep(Duration::from_secs(10)).await;
    rig.scans.bump();
    // The run's store reads happen on blocking threads, and the paused clock
    // jumps ahead while they run, so the first run is awaited in wall-clock
    // time. Then well past the quiet wait a second run would need.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while library.times_called("library") == 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    tokio::time::sleep(Duration::from_secs(400)).await;
    assert_eq!(library.times_called("library"), 1);
    tokio::time::resume();
}

/// Poll the status route until `done` holds.
async fn wait_for_status(rig: &Rig, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..500 {
        let (status, body) = call(rig.user_app(), "GET", "/api/v3/cache/sync/status", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if done(&body) {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the refresh status never got there");
}

/// The library image refresh end to end through its routes: progress any
/// user can read, a curator-only cancel that leaves the status idle, and a
/// rerun that walks every phase, discovery as the admin who started it.
#[tokio::test]
async fn library_image_refresh_reports_progress_and_cancels() {
    let library = Arc::new(ScriptedLibrary {
        calls: std::sync::Mutex::new(Vec::new()),
        recorded: tokio::sync::Notify::new(),
        album_gate: tokio::sync::Semaphore::new(0),
    });
    let rig = Rig::open_with("image-refresh", Some(library.clone())).await;
    let idle = wait_for_status(&rig, |_| true).await;
    assert_eq!(idle["is_syncing"], Value::Bool(false));

    let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/precache/run", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let live = wait_for_status(&rig, |body| body["phase"] == "albums").await;
    assert_eq!(live["is_syncing"], Value::Bool(true));
    assert_eq!(live["processed_artists"], Value::from(2));
    assert_eq!(live["total_albums"], Value::from(1));
    assert!(library.called(&format!("artist:{BETA}")));
    assert!(library.called(&format!("discovery:{}:{ALPHA}", rig.admin_id)));

    let (status, _) = call(rig.user_app(), "POST", "/api/v3/cache/sync/cancel", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(rig.admin_app(), "POST", "/api/v3/cache/sync/cancel", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], Value::from("cancelled"));
    let stopped = wait_for_status(&rig, |_| true).await;
    assert_eq!(stopped["is_syncing"], Value::Bool(false));
    assert_eq!(stopped["error_message"], Value::Null);
    assert!(!library.called(&format!("album:{FIRST}")));

    library.album_gate.add_permits(8);
    let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/precache/run", None).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a cancelled run frees the job name"
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        library.wait_called(&format!("audiodb:{FIRST}")),
    )
    .await
    .expect("the run reaches the album's AudioDB warm");
    let done = wait_for_status(&rig, |body| body["is_syncing"] == false).await;
    assert_eq!(done["error_message"], Value::Null);
    assert!(library.called(&format!("album:{FIRST}")));
    assert!(
        !library.called(&format!("audiodb:{ALPHA}")),
        "cached AudioDB answers are skipped"
    );
}

#[tokio::test]
async fn quota_round_trip_set_and_clear() {
    let rig = Rig::open("quota").await;

    let uri = format!("/api/v3/admin/users/{}/quota", rig.user_id);
    let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quota_override"]["request_quota_count"], Value::Null);
    assert_eq!(body["effective_request_quota_days"], Value::from(7));
    assert_eq!(body["requests_in_window"], Value::from(0));
    assert_eq!(body["storage_bytes"], Value::from(0));
    assert_eq!(body["exempt"], Value::Bool(false));

    let (status, body) = call(
        rig.admin_app(),
        "PUT",
        &uri,
        Some(json!({
            "request_quota_count": 12,
            "request_quota_days": 30,
            "storage_quota_gb": 9,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["quota_override"]["request_quota_count"],
        Value::from(12)
    );
    assert_eq!(body["effective_request_quota_count"], Value::from(12));
    assert_eq!(body["effective_request_quota_days"], Value::from(30));
    assert_eq!(body["effective_storage_quota_gb"], Value::from(9));
    // The row persists durably, not just in the ledger.
    let stored: Option<(Option<i64>, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT request_quota_count, request_quota_days, storage_quota_gb
         FROM user_quotas WHERE user_id = ?1",
    )
    .bind(&rig.user_id)
    .fetch_optional(rig.runtime.db().sqlx())
    .await
    .expect("override row reads");
    assert_eq!(stored, Some((Some(12), Some(30), Some(9))));

    // All-null clears back to pure inherit.
    let (status, body) = call(rig.admin_app(), "PUT", &uri, Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quota_override"]["request_quota_count"], Value::Null);
    assert_eq!(body["effective_request_quota_count"], Value::from(0));
    let stored: Option<(String,)> =
        sqlx::query_as("SELECT user_id FROM user_quotas WHERE user_id = ?1")
            .bind(&rig.user_id)
            .fetch_optional(rig.runtime.db().sqlx())
            .await
            .expect("cleared row reads");
    assert_eq!(stored, None);
}

#[tokio::test]
async fn quota_rejects_out_of_bounds_overrides() {
    let rig = Rig::open("quota-bounds").await;
    let uri = format!("/api/v3/admin/users/{}/quota", rig.user_id);
    for (body, field) in [
        (json!({"request_quota_days": 0}), "request_quota_days"),
        (
            json!({"request_quota_count": 1_000_001}),
            "request_quota_count",
        ),
        (json!({"storage_quota_gb": 1_000_001}), "storage_quota_gb"),
    ] {
        let (status, payload) = call(rig.admin_app(), "PUT", &uri, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
        assert_eq!(payload["error"]["code"], Value::from("INVALID_INPUT"));
        assert!(
            payload["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(field)),
            "{payload}"
        );
    }
}

#[tokio::test]
async fn quota_usage_counts_asks_and_bytes() {
    let rig = Rig::open("quota-usage").await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads")
        .as_millis();
    // Asks count from the durable request rows.
    for key in ["ask-1", "ask-2", "ask-3"] {
        lane_seed(
            &rig,
            &format!(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, \
                 artist_name, album_title, requested_at, status, user_id) \
                 VALUES ('{key}', '{key}', 'artist', 'album', {now}, 'pending', ?1)"
            ),
            &[&rig.user_id],
        )
        .await;
        lane_seed(
            &rig,
            &format!(
                "INSERT INTO request_history_requesters \
                 (user_id, musicbrainz_id_lower, requested_at) VALUES (?1, '{key}', {now})"
            ),
            &[&rig.user_id],
        )
        .await;
    }
    lane_seed(
        &rig,
        "INSERT INTO download_tasks
             (id, user_id, release_group_mbid, artist_name, album_title,
              status, total_size_bytes, created_at, updated_at)
         VALUES ('task-1', ?1, 'rg-1', 'artist', 'album', 'completed', 4096,
                 1767225600000, 1767225600000)",
        &[&rig.user_id],
    )
    .await;
    lane_seed(
        &rig,
        "INSERT INTO download_tasks
             (id, user_id, release_group_mbid, artist_name, album_title,
              status, total_size_bytes, created_at, updated_at)
         VALUES ('task-2', ?1, 'rg-2', 'artist', 'album', 'failed', 1024,
                 1767225600000, 1767225600000)",
        &[&rig.user_id],
    )
    .await;

    let uri = format!("/api/v3/admin/users/{}/quota", rig.user_id);
    let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["requests_in_window"], Value::from(3));
    // Only the landed task counts toward storage.
    assert_eq!(body["storage_bytes"], Value::from(4096));
}

#[tokio::test]
async fn quota_marks_curators_exempt() {
    let rig = Rig::open("quota-exempt").await;
    let trusted = rig.rig.seed_user("terry", Role::Trusted).await;
    for (user_id, exempt) in [
        (&rig.admin_id, true),
        (&trusted.id, true),
        (&rig.user_id, false),
    ] {
        let user_id = user_id.clone();
        let uri = format!("/api/v3/admin/users/{user_id}/quota");
        let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["exempt"], Value::from(exempt), "{user_id}");
    }
}

/// An override for a user the user store knows but whose row is not in
/// this database is refused by the foreign key and answers 404.
#[tokio::test]
async fn quota_override_for_an_unmirrored_user_is_not_found() {
    let rig = Rig::open("quota-unmirrored").await;
    let elsewhere = AuthRig::new("quota-unmirrored-users").await;
    let user = elsewhere.seed_user("ursula", Role::User).await;
    let providers = Arc::new(droppedneedle_providers::Providers::closed());
    let admin = AdminSetup::new(
        elsewhere.deps.clone(),
        rig.quota.clone(),
        providers.cache.clone(),
        providers,
    )
    .with_db(AdminDb::new(rig.runtime.db().clone()));
    let app = with_test_principal(
        droppedneedle::admin::routes()
            .with_state(admin)
            .into_router(),
        test_principal(&rig.admin_id, Role::Admin, "sess-1", SessionKind::Standard),
    );
    let uri = format!("/api/v3/admin/users/{}/quota", user.id);
    let (status, body) = call(app, "PUT", &uri, Some(json!({"request_quota_count": 3}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], Value::from("NOT_FOUND"));
}

#[tokio::test]
async fn quota_overrides_reload_into_the_ledger() {
    let rig = Rig::open("quota-reload").await;
    lane_seed(
        &rig,
        "INSERT INTO user_quotas
             (user_id, request_quota_count, request_quota_days, storage_quota_gb)
         VALUES (?1, 12, 30, 9)",
        &[&rig.user_id],
    )
    .await;
    let loaded = droppedneedle::admin::quota::reload_overrides(rig.runtime.db().sqlx(), &rig.quota)
        .await
        .expect("reload runs");
    assert_eq!(loaded, 1);
    let effective = rig
        .quota
        .effective_quota(&rig.user_id)
        .expect("effective reads");
    assert_eq!(effective.request_count, 12);
    assert_eq!(effective.request_days, 30);
    assert_eq!(effective.storage_gb, 9);
}

/// One INSERT through the writer lane: the pool is read-only by design, so
/// even test seeding travels the write path.
async fn lane_seed(rig: &Rig, sql: &str, params: &[&str]) {
    let sql = sql.to_owned();
    let params: Vec<String> = params.iter().map(|value| (*value).to_owned()).collect();
    rig.runtime
        .db()
        .write(Lane::Foreground, "admin-test-seed", move |tx| {
            let refs: Vec<&dyn rusqlite::ToSql> = params
                .iter()
                .map(|value| value as &dyn rusqlite::ToSql)
                .collect();
            tx.execute(&sql, refs.as_slice())?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("seed writes");
}

/// A booted app shows every always-on loop as running in the admin jobs
/// view.
#[tokio::test]
async fn queue_stats_shows_every_boot_loop_running() {
    let app = crate::common::TestApp::start("queue-boot-loops").await;
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];
    app.until("every boot loop running", || async {
        let (_, stats) = app
            .call("GET", "/api/v3/admin/queue-stats", &headers, None)
            .await;
        let jobs = stats["jobs"].as_array()?;
        stats["queues"].as_array()?;
        droppedneedle_kernel::jobs::BOOT_JOBS
            .iter()
            .chain(droppedneedle_acquire::wiring::ACQUIRE_LOOPS)
            .chain(droppedneedle_kernel::events::LOOPS)
            .chain(droppedneedle_library::LOOPS)
            .chain(droppedneedle_media::LOOPS)
            .chain(droppedneedle_compat::LOOPS)
            .all(|name| {
                jobs.iter()
                    .any(|job| job["name"] == *name && job["state"] == "running")
            })
            .then_some(())
    })
    .await;
    app.stop().await;
}
