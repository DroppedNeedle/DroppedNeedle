//! Stage-10 admin-route briefs: gating, cache stats/clear, queue and
//! provider stats, quota admin, checkpoint health, and honest unwired 503s.
//!
//! The router mounts directly with sessions injected; health rides the full
//! app. Scratch runtimes only — no network, no production database.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::admin::{AdminDb, AdminSetup};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::auth::users::roles::{Role, SessionKind};
use droppedneedle::db::{DbConfig, DbRuntime, Lane, open_runtime};
use droppedneedle::providers::ProviderCache as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// One scratch deployment: migrated runtime, memory auth, wired admin.
struct Rig {
    /// Held, never read: dropping it would close the pool out from under
    /// the admin handles.
    #[allow(dead_code)]
    runtime: DbRuntime,
    admin: AdminSetup,
    rig: TestRig,
    admin_id: String,
    user_id: String,
    cache: Arc<droppedneedle::providers::InMemoryProviderCache>,
    quota: Arc<droppedneedle::acquire::requests::quota::QuotaLedger>,
    jobs: droppedneedle::jobs::wiring::JobsSetup,
    _scratch: crate::common::ScratchDir,
}

impl Rig {
    async fn open(tag: &str) -> Self {
        let scratch = crate::common::ScratchDir::new(&format!("admin-routes-{tag}"));
        let dir = scratch.to_path_buf();
        let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
            .await
            .expect("scratch runtime opens");
        let rig = TestRig::new().expect("rig builds");
        let admin_user = rig.seed_user("brenda", Role::Admin).await;
        let plain_user = rig.seed_user("molly", Role::User).await;
        let quota = Arc::new(
            droppedneedle::acquire::requests::quota::QuotaLedger::unlimited(
                droppedneedle::acquire::db::AcquireDb::from_runtime(&runtime),
            ),
        );
        let cache = Arc::new(droppedneedle::providers::InMemoryProviderCache::new());
        let providers = Arc::new(droppedneedle::providers::Providers::new(cache.clone()));
        let jobs = droppedneedle::jobs::wiring::JobsSetup::for_tests(rig.deps.clone());
        let admin = AdminSetup::new(rig.deps.clone(), quota.clone(), cache.clone(), providers)
            .with_db(AdminDb::new(runtime.pool().clone(), runtime.lane().clone()))
            .with_backups(droppedneedle::db::BackupService::new(
                &dir.join("app.db"),
                &dir.join("backups"),
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
            _scratch: scratch,
        }
    }

    /// Router with an injected session for `user_id`, or anonymous.
    fn app(&self, user_id: Option<&str>) -> Router {
        let router = self.admin.gated_router();
        match user_id {
            Some(user_id) => {
                let user_id = user_id.to_owned();
                router.layer(axum::middleware::from_fn(
                    move |mut req: Request<Body>, next: axum::middleware::Next| {
                        let user_id = user_id.clone();
                        async move {
                            req.extensions_mut().insert(CurrentSession {
                                user_id,
                                session_id: "sess-1".to_owned(),
                                kind: SessionKind::Standard,
                                transport: Transport::Bearer,
                            });
                            next.run(req).await
                        }
                    },
                ))
            }
            None => router,
        }
    }

    fn admin_app(&self) -> Router {
        self.app(Some(&self.admin_id.clone()))
    }

    fn user_app(&self) -> Router {
        self.app(Some(&self.user_id.clone()))
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
        ("GET", "/admin/backups".to_owned(), None),
        ("POST", "/admin/backups".to_owned(), None),
        (
            "GET",
            "/admin/backups/library-0-0.db/restore-report".to_owned(),
            None,
        ),
        ("GET", "/admin/cache/stats".to_owned(), None),
        ("POST", "/admin/cache/clear".to_owned(), Some(json!({}))),
        ("GET", "/admin/queue-stats".to_owned(), None),
        ("GET", "/admin/provider-stats".to_owned(), None),
        ("GET", format!("/admin/users/{user_id}/quota"), None),
        (
            "PUT",
            format!("/admin/users/{user_id}/quota"),
            Some(json!({})),
        ),
        ("POST", "/admin/precache/run".to_owned(), None),
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
    // A session whose account is gone reads as stale (401), never as its
    // last-known role.
    let (status, payload) = call(
        rig.app(Some("user-ghost")),
        "GET",
        "/admin/queue-stats",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(payload["error"]["code"], Value::from("UNAUTHORIZED"));
}

#[tokio::test]
async fn cache_clear_all_and_per_source() {
    let rig = Rig::open("cache-clear").await;
    rig.cache
        .set_bytes("mb:artist:search:x", vec![1], Duration::from_secs(60))
        .await;
    rig.cache
        .set_bytes("mb:rg:detail:y", vec![2], Duration::from_secs(60))
        .await;
    rig.cache
        .set_bytes("lfm_user:recents", vec![3], Duration::from_secs(60))
        .await;

    let (status, body) = call(
        rig.admin_app(),
        "POST",
        "/admin/cache/clear",
        Some(json!({"scope": "source", "source": "musicbrainz"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cleared_entries"], Value::from(2));
    assert_eq!(body["remaining_entries"], Value::from(1));

    let (status, body) = call(
        rig.admin_app(),
        "POST",
        "/admin/cache/clear",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cleared_entries"], Value::from(1));
    assert_eq!(body["remaining_entries"], Value::from(0));
}

/// Phases that never finish, holding a run open for the 409 brief.
#[derive(Clone)]
struct HangWork;

impl droppedneedle::jobs::precache::PrecacheWork for HangWork {
    fn run(
        &self,
        _progress: droppedneedle::jobs::precache::Progress,
    ) -> droppedneedle::jobs::registry::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { std::future::pending().await })
    }
}

#[tokio::test]
async fn precache_second_run_is_409_while_live() {
    let rig = Rig::open("precache-busy").await;
    // Hold the shared registry name directly: the route must see it.
    let _held = droppedneedle::jobs::precache::spawn_run(
        rig.jobs.registry(),
        HangWork,
        droppedneedle::jobs::precache::PrecacheLimits::new(
            Duration::from_secs(60),
            Duration::from_secs(600),
        ),
    )
    .await
    .expect("held run starts");
    let (status, body) = call(rig.admin_app(), "POST", "/admin/precache/run", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], Value::from("CONFLICT"));

    // Once the live run ends, the route starts again.
    rig.jobs.cancel_all(Duration::from_secs(5)).await;
    let (status, _) = call(rig.admin_app(), "POST", "/admin/precache/run", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn quota_round_trip_set_and_clear() {
    let rig = Rig::open("quota").await;
    mirror_user_row(&rig, &rig.user_id.clone(), "user").await;

    let uri = format!("/admin/users/{}/quota", rig.user_id);
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
    .fetch_optional(rig.runtime.pool())
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
            .fetch_optional(rig.runtime.pool())
            .await
            .expect("cleared row reads");
    assert_eq!(stored, None);
}

#[tokio::test]
async fn quota_rejects_out_of_bounds_overrides() {
    let rig = Rig::open("quota-bounds").await;
    mirror_user_row(&rig, &rig.user_id.clone(), "user").await;
    let uri = format!("/admin/users/{}/quota", rig.user_id);
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
    mirror_user_row(&rig, &rig.user_id.clone(), "user").await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads")
        .as_secs();
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
                 1767225600.0, 1767225600.0)",
        &[&rig.user_id],
    )
    .await;
    lane_seed(
        &rig,
        "INSERT INTO download_tasks
             (id, user_id, release_group_mbid, artist_name, album_title,
              status, total_size_bytes, created_at, updated_at)
         VALUES ('task-2', ?1, 'rg-2', 'artist', 'album', 'failed', 1024,
                 1767225600.0, 1767225600.0)",
        &[&rig.user_id],
    )
    .await;

    let uri = format!("/admin/users/{}/quota", rig.user_id);
    let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["requests_in_window"], Value::from(3));
    // Only the landed task counts toward storage.
    assert_eq!(body["storage_bytes"], Value::from(4096));
}

#[tokio::test]
async fn quota_marks_curators_exempt() {
    let rig = Rig::open("quota-exempt").await;
    mirror_user_row(&rig, &rig.admin_id.clone(), "admin").await;
    let trusted = rig.rig.seed_user("terry", Role::Trusted).await;
    mirror_user_row(&rig, &trusted.id, "trusted").await;
    for (user_id, exempt) in [
        (&rig.admin_id, true),
        (&trusted.id, true),
        (&rig.user_id, false),
    ] {
        let user_id = user_id.clone();
        let uri = format!("/admin/users/{user_id}/quota");
        let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["exempt"], Value::from(exempt), "{user_id}");
    }
}

#[tokio::test]
async fn quota_overrides_reload_into_the_ledger() {
    let rig = Rig::open("quota-reload").await;
    mirror_user_row(&rig, &rig.user_id.clone(), "user").await;
    lane_seed(
        &rig,
        "INSERT INTO user_quotas
             (user_id, request_quota_count, request_quota_days, storage_quota_gb)
         VALUES (?1, 12, 30, 9)",
        &[&rig.user_id],
    )
    .await;
    let loaded = droppedneedle::admin::quota::reload_overrides(rig.runtime.pool(), &rig.quota)
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

/// Mirror a memory-rig user into the scratch database so FK-backed quota
/// writes accept it. Production keeps both in one database; the split is a
/// test-harness artifact.
async fn mirror_user_row(rig: &Rig, user_id: &str, role: &str) {
    lane_seed(
        rig,
        "INSERT INTO auth_users (id, display_name, role, created_at)
         VALUES (?1, ?1, ?2, '2026-01-01T00:00:00Z')",
        &[user_id, role],
    )
    .await;
}

/// One INSERT through the writer lane: the pool is read-only by design, so
/// even test seeding travels the write path.
async fn lane_seed(rig: &Rig, sql: &str, params: &[&str]) {
    let sql = sql.to_owned();
    let params: Vec<String> = params.iter().map(|value| (*value).to_owned()).collect();
    rig.runtime
        .lane()
        .write(Lane::Foreground, "admin-test-seed", move |tx| {
            let refs: Vec<&dyn rusqlite::ToSql> = params
                .iter()
                .map(|value| value as &dyn rusqlite::ToSql)
                .collect();
            tx.execute(&sql, refs.as_slice())?;
            Ok(())
        })
        .await
        .expect("seed writes");
}
