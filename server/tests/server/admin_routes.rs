//! Admin-route briefs: the library image refresh, queue stats, quota admin
//! and checkpoint health. Each route's posture is in `auth_e2e`'s matrix.
//!
//! The router mounts directly with sessions injected; the boot-loop check
//! rides the full app. Scratch runtimes only - no network, no production database.

use crate::common::Probe;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::admin::AdminSetup;
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::db::{DbRuntime, Lane};
use droppedneedle_kernel::events::{EventHub, Subscription};
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
    quota: Arc<droppedneedle_acquire::requests::quota::QuotaLedger>,
    /// The hub the image refresh publishes its `cache.sync` frames on.
    events: EventHub,
    jobs: droppedneedle::bootstrap::jobs::JobsSetup,
    /// The count the library bumps when a scan ends.
    scans: droppedneedle::bootstrap::jobs::ScansEnded,
    _scratch: crate::common::ScratchDir,
}

impl Rig {
    async fn open(tag: &str) -> Self {
        Self::open_with(tag, Arc::new(crate::common::bundles::NothingToWarm)).await
    }

    async fn open_with(
        tag: &str,
        precache: Arc<dyn droppedneedle_kernel::jobs::precache_phases::PrecacheSources>,
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
            )
            .expect("quota ledger"),
        );
        let providers = Arc::new(droppedneedle_providers::Providers::build(
            &platform,
            crate::common::closed_outbound().providers(),
            droppedneedle_kernel::db::cache::CacheDb::scratch(),
        ));
        let events = platform.events.clone();
        let scans = droppedneedle::bootstrap::jobs::ScansEnded::default();
        let jobs = crate::common::bundles::jobs_at(
            &platform,
            runtime.checkpoint().clone(),
            precache,
            scans.clone(),
        );
        let admin = AdminSetup::new(
            rig.deps.clone(),
            quota.clone(),
            providers,
            runtime.db().clone(),
            std::sync::Arc::new(droppedneedle_kernel::db::BackupService::new(
                &dir.join("app.db"),
                &dir.join("backups"),
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            )),
            runtime.checkpoint().clone(),
            jobs.precache_trigger(),
            crate::common::bundles::covers_at(&platform),
            droppedneedle::appearance::store::AppearanceStore::new(
                runtime.db().clone(),
                platform.clock.clone(),
            ),
        );
        Self {
            runtime,
            admin,
            rig,
            admin_id: admin_user.id,
            user_id: plain_user.id,
            quota,
            events,
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

/// A failed checkpoint pass reaches the admin as a fixed code, never as
/// failure text that could carry a path.
#[tokio::test]
async fn queue_stats_reports_a_failed_checkpoint_as_a_code() {
    let mut rig = Rig::open("checkpoint-failed").await;
    let dir = rig._scratch.to_path_buf();
    let missing = dir.join("gone").join("app.db");
    let failing = droppedneedle_kernel::db::CheckpointService::new(
        &missing,
        rig.runtime.read_activity().clone(),
        Arc::default(),
        Arc::default(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    assert!(failing.run_once().error.is_some());
    rig.admin.checkpoint = failing;

    let (status, payload) = call(rig.admin_app(), "GET", "/api/v3/admin/queue-stats", None).await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(payload["checkpoint"]["error"], "CHECKPOINT_FAILED");
    assert!(
        !payload.to_string().contains(&*dir.to_string_lossy()),
        "{payload}"
    );
}

/// A call a provider client sends is counted on the admin's provider
/// stats under its source, the priority its caller stated and its outcome.
#[tokio::test]
async fn provider_stats_count_a_call_by_source_priority_and_outcome() {
    use droppedneedle_providers::lastfm::LastFmCredentials;
    use droppedneedle_providers::test_support::ScriptedWire;
    use droppedneedle_providers::{Providers, RequestPriority};

    let mut rig = Rig::open("provider-stats").await;
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        "http://127.0.0.1:9/lastfm/",
        200,
        &[("content-type", "application/json")],
        include_str!("../fixtures/lastfm/artist_getinfo.json"),
    );
    let providers = Arc::new(Providers::unpaced(wire));
    rig.admin.providers = providers.clone();
    let creds = LastFmCredentials {
        api_key: "instance-key".to_owned(),
        ..LastFmCredentials::default()
    };
    let info = providers
        .lastfm
        .artist_info(&creds, "Radiohead", None, RequestPriority::UserInitiated)
        .await
        .expect("the recorded answer reads");
    assert!(info.is_some());

    let (status, payload) =
        call(rig.admin_app(), "GET", "/api/v3/admin/provider-stats", None).await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    let rows: Vec<&Value> = payload["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .filter(|row| row["source"] == "lastfm")
        .collect();
    assert_eq!(rows.len(), 1, "{payload}");
    assert_eq!(rows[0]["priority"], "user_initiated");
    assert_eq!(rows[0]["outcome"], "ok");
    assert_eq!(rows[0]["calls"], 1);
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
            // Recorded once the run is in its albums phase, before the gate.
            self.record(format!("album-waiting:{}", album.release_group_mbid));
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
    let rig = Rig::open_with("scan-burst", library.clone()).await;
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

/// The refresh status as any signed-in user reads it.
async fn refresh_status(rig: &Rig) -> Value {
    let (status, body) = call(rig.user_app(), "GET", "/api/v3/cache/sync/status", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Wait until a run's `cache.sync` frames go live and then idle again.
/// Frames replayed from before the run are passed over until a live one.
async fn run_ended(frames: &mut Subscription) {
    let mut live = false;
    while let Some(frame) = frames.next().await {
        if frame.name() != "cache.sync" {
            continue;
        }
        let progress: Value = serde_json::from_str(frame.data()).expect("frame decodes");
        if progress["is_syncing"] == true {
            live = true;
        } else if live {
            return;
        }
    }
    panic!("the event hub closed before the run ended");
}

/// The library image refresh end to end through its routes: progress any
/// user can read, one run at a time, a curator-only cancel that leaves the
/// status idle and frees the job name, and a rerun that walks every phase,
/// discovery as the admin who started it.
#[tokio::test]
async fn library_image_refresh_runs_once_reports_and_cancels() {
    let library = Arc::new(ScriptedLibrary {
        calls: std::sync::Mutex::new(Vec::new()),
        recorded: tokio::sync::Notify::new(),
        album_gate: tokio::sync::Semaphore::new(0),
    });
    let rig = Rig::open_with("image-refresh", library.clone()).await;
    let idle = refresh_status(&rig).await;
    assert_eq!(idle["is_syncing"], Value::Bool(false));

    let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/precache/run", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokio::time::timeout(
        Duration::from_secs(30),
        library.wait_called(&format!("album-waiting:{FIRST}")),
    )
    .await
    .expect("the run reaches its albums phase");
    let live = refresh_status(&rig).await;
    assert_eq!(live["phase"], "albums", "{live}");
    assert_eq!(live["is_syncing"], Value::Bool(true));
    assert_eq!(live["processed_artists"], Value::from(2));
    assert_eq!(live["total_albums"], Value::from(1));
    assert!(library.called(&format!("artist:{BETA}")));
    assert!(library.called(&format!("discovery:{}:{ALPHA}", rig.admin_id)));

    let (status, body) = call(rig.admin_app(), "POST", "/api/v3/admin/precache/run", None).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a second run while one is live: {body}"
    );
    assert_eq!(body["error"]["code"], Value::from("CONFLICT"));

    let (status, _) = call(rig.user_app(), "POST", "/api/v3/cache/sync/cancel", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(rig.admin_app(), "POST", "/api/v3/cache/sync/cancel", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], Value::from("cancelled"));
    let stopped = refresh_status(&rig).await;
    assert_eq!(stopped["is_syncing"], Value::Bool(false));
    assert_eq!(stopped["error_message"], Value::Null);
    assert!(!library.called(&format!("album:{FIRST}")));

    library.album_gate.add_permits(8);
    let mut frames = rig.events.subscribe(&rig.admin_id);
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
    tokio::time::timeout(Duration::from_secs(30), run_ended(&mut frames))
        .await
        .expect("the rerun ends");
    let done = refresh_status(&rig).await;
    assert_eq!(done["is_syncing"], Value::Bool(false));
    assert_eq!(done["error_message"], Value::Null);
    assert!(library.called(&format!("album:{FIRST}")));
    assert!(
        !library.called(&format!("audiodb:{ALPHA}")),
        "cached AudioDB answers are skipped"
    );
}

/// A user's quota override over one rig: set and cleared durably,
/// out-of-bounds values refused by field, curators shown exempt, a user
/// missing from this database answered 404, and stored rows reloaded into
/// the ledger.
#[tokio::test]
async fn quota_overrides_save_validate_and_reload() {
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
    let stored: Option<(Option<i64>, Option<i64>, Option<i64>)> = rig.runtime.db().maybe_row(
        "SELECT request_quota_count, request_quota_days, storage_quota_gb
         FROM user_quotas WHERE user_id = ?1",
        rusqlite::params![&rig.user_id],
    );
    assert_eq!(stored, Some((Some(12), Some(30), Some(9))));

    // All-null clears back to pure inherit.
    let (status, body) = call(rig.admin_app(), "PUT", &uri, Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quota_override"]["request_quota_count"], Value::Null);
    assert_eq!(body["effective_request_quota_count"], Value::from(0));
    let stored: Option<(String,)> = rig.runtime.db().maybe_row(
        "SELECT user_id FROM user_quotas WHERE user_id = ?1",
        rusqlite::params![&rig.user_id],
    );
    assert_eq!(stored, None, "all-null clears the row");

    for (body, field) in [
        (json!({"request_quota_days": 0}), "request_quota_days"),
        (
            json!({"request_quota_count": 1_000_001}),
            "request_quota_count",
        ),
        (json!({"storage_quota_gb": 1_000_001}), "storage_quota_gb"),
    ] {
        let (status, payload) = call(rig.admin_app(), "PUT", &uri, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {payload}");
        assert_eq!(
            payload["error"]["code"],
            Value::from("INVALID_INPUT"),
            "{field}"
        );
        assert!(
            payload["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(field)),
            "{payload}"
        );
    }

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
        assert_eq!(body["exempt"], Value::from(exempt), "exempt {user_id}");
    }

    // A user the user store knows but whose row is not in this database is
    // refused by the foreign key.
    let elsewhere = AuthRig::new("quota-unmirrored-users").await;
    let user = elsewhere.seed_user("ursula", Role::User).await;
    let admin = AdminSetup {
        users: elsewhere.deps.clone(),
        ..rig.admin.clone()
    };
    let app = with_test_principal(
        droppedneedle::admin::routes()
            .with_state(admin)
            .into_router(),
        test_principal(&rig.admin_id, Role::Admin, "sess-1", SessionKind::Standard),
    );
    let unmirrored = format!("/api/v3/admin/users/{}/quota", user.id);
    let (status, body) = call(
        app,
        "PUT",
        &unmirrored,
        Some(json!({"request_quota_count": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unmirrored user: {body}");
    assert_eq!(body["error"]["code"], Value::from("NOT_FOUND"));

    lane_seed(
        &rig,
        "INSERT INTO user_quotas
             (user_id, request_quota_count, request_quota_days, storage_quota_gb)
         VALUES (?1, 12, 30, 9)",
        &[&rig.user_id],
    )
    .await;
    let loaded = rig.quota.reload_overrides().await.expect("reload runs");
    assert_eq!(loaded, 1);
    let effective = rig
        .quota
        .effective_quota(&rig.user_id)
        .expect("effective reads");
    assert_eq!(effective.request_count, 12);
    assert_eq!(effective.request_days, 30);
    assert_eq!(effective.storage_gb, 9);
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
    lane_seed(
        &rig,
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
         VALUES ('ar1', 'A', 'a', 'person', 0, 0)",
        &[],
    )
    .await;
    lane_seed(
        &rig,
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
           album_artist_id, grouping_source, created_at, updated_at)
         VALUES ('al1', 'r1', 'g', 'T', 't', 'ar1', 'automatic', 0, 0)",
        &[],
    )
    .await;
    lane_seed(
        &rig,
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
           path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
           album_title, album_title_folded, file_format, ingest_source, imported_at,
           membership_source, availability, download_task_id)
         VALUES ('t1', 'al1', 'r1', '/music/A/one.flac', 'A/one.flac', 'h1', 4096, 0, 's',
             'One', 'one', 'T', 't', 'flac', 'scan', 0, 'automatic', 'indexed', 'task-1'),
                ('t2', 'al1', 'r1', '/music/A/two.flac', 'A/two.flac', 'h2', 2048, 0, 's',
             'Two', 'two', 'T', 't', 'flac', 'scan', 0, 'automatic', 'missing', 'task-1')",
        &[],
    )
    .await;

    let uri = format!("/api/v3/admin/users/{}/quota", rig.user_id);
    let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["requests_in_window"], Value::from(3));
    // Storage is the figure the quota enforces: the user's downloaded files
    // still in the library, not the size a download task recorded.
    assert_eq!(body["storage_bytes"], Value::from(4096));
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
