//! Stage-10 backup-UX briefs: list, run, restore reports, rotation, and the
//! pre-upgrade safety net.
//!
//! Everything runs against scratch runtimes (migrated temp databases) with
//! the admin router mounted directly and sessions injected — no network, no
//! production database.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::admin::{AdminDb, AdminSetup};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::auth::users::roles::{Role, SessionKind};
use droppedneedle::db::{BACKUP_KEEP, DbConfig, DbRuntime, open_runtime};
use serde_json::Value;
use tower::ServiceExt as _;

/// One scratch deployment: migrated runtime, memory auth, wired admin.
struct Rig {
    /// Held, never read: dropping it would close the pool out from under
    /// the admin handles.
    #[allow(dead_code)]
    runtime: DbRuntime,
    admin: AdminSetup,
    admin_id: String,
    dir: std::path::PathBuf,
    _scratch: crate::common::ScratchDir,
}

impl Rig {
    async fn open(tag: &str) -> Self {
        let scratch = crate::common::ScratchDir::new(&format!("admin-backups-{tag}"));
        let dir = scratch.to_path_buf();
        let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
            .await
            .expect("scratch runtime opens");
        let rig = TestRig::new().expect("rig builds");
        let admin_user = rig.seed_user("brenda", Role::Admin).await;
        let quota = Arc::new(droppedneedle::acquire::requests::quota::QuotaLedger::unlimited());
        let cache = Arc::new(droppedneedle::providers::InMemoryProviderCache::new());
        let providers = Arc::new(droppedneedle::providers::Providers::new(cache.clone()));
        let backup_dir = dir.join("backups");
        let admin = AdminSetup::new(rig.deps.clone(), quota, cache, providers)
            .with_db(AdminDb::new(runtime.pool().clone(), runtime.lane().clone()))
            .with_backups(droppedneedle::db::BackupService::new(
                &dir.join("app.db"),
                &backup_dir,
            ))
            .with_checkpoint(runtime.checkpoint().clone());
        Self {
            runtime,
            admin,
            admin_id: admin_user.id,
            dir,
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
                                session_id: "sess-admin-1".to_owned(),
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

#[tokio::test]
async fn backup_run_then_list_then_report() {
    let rig = Rig::open("journey").await;
    let (status, run) = call(rig.admin_app(), "POST", "/admin/backups", None).await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let name = run["name"].as_str().expect("run names the backup");
    assert!(name.ends_with(".db"), "{name}");
    let sha = run["sha256"].as_str().expect("run reports sha");
    assert_eq!(sha.len(), 64, "sha256 hex length");
    assert!(run["size_bytes"].as_u64().expect("size") > 0);
    assert_eq!(
        run["user_version"].as_i64(),
        Some(droppedneedle::schema::latest_version())
    );
    // Manifest sidecar sits beside the backup on disk.
    let sidecar = rig
        .dir
        .join("backups")
        .join(format!("{name}.manifest.json"));
    assert!(sidecar.is_file(), "manifest sidecar exists");

    let (status, list) = call(rig.admin_app(), "GET", "/admin/backups", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["backups"].as_array().expect("list").len(), 1);
    assert_eq!(list["backups"][0]["name"], run["name"]);
    assert_eq!(list["backups"][0]["sha256"], run["sha256"]);
    assert!(list["backups"][0]["created_at_unix"].as_u64().is_some());

    let uri = format!("/admin/backups/{name}/restore-report");
    let (status, report) = call(rig.admin_app(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["backup"], run["name"]);
    assert_eq!(report["ok"], Value::Bool(true));
    assert_eq!(report["restorable"], Value::Bool(true));
    let checks = report["checks"].as_array().expect("checks");
    assert!(
        checks
            .iter()
            .all(|check| check["passed"] == Value::Bool(true)),
        "{checks:?}"
    );
    let names: Vec<&str> = checks
        .iter()
        .filter_map(|check| check["name"].as_str())
        .collect();
    for expected in [
        "file-present",
        "manifest-present",
        "manifest-size",
        "manifest-sha256",
        "integrity-check",
        "schema-version",
    ] {
        assert!(names.contains(&expected), "{names:?}");
    }
}

#[tokio::test]
async fn backup_rotation_keeps_five() {
    let rig = Rig::open("rotation").await;
    for _ in 0..BACKUP_KEEP + 1 {
        let (status, _) = call(rig.admin_app(), "POST", "/admin/backups", None).await;
        assert_eq!(status, StatusCode::CREATED);
        // Distinct names sort by wall clock; a breath between runs keeps
        // the millisecond stamps apart.
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let (status, list) = call(rig.admin_app(), "GET", "/admin/backups", None).await;
    assert_eq!(status, StatusCode::OK);
    let backups = list["backups"].as_array().expect("list");
    assert_eq!(backups.len(), BACKUP_KEEP);
    // Oldest first: names ascend.
    let names: Vec<&str> = backups
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    // Stale manifests rotate with their backups: no orphans left.
    let mut files: Vec<String> = std::fs::read_dir(rig.dir.join("backups"))
        .expect("backup dir reads")
        .map(|entry| {
            entry
                .expect("entry reads")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    files.sort();
    assert_eq!(files.len(), BACKUP_KEEP * 2, "{files:?}");
}

#[tokio::test]
async fn restore_report_rejects_unsafe_names() {
    let rig = Rig::open("names").await;
    for name in ["no-suffix", "..%2Fapp.db", "sub%2Flibrary.db", ".hidden.db"] {
        let uri = format!("/admin/backups/{name}/restore-report");
        let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert_eq!(body["error"]["code"], Value::from("INVALID_INPUT"));
    }
}

#[tokio::test]
async fn restore_report_catches_a_tampered_backup() {
    let rig = Rig::open("tamper").await;
    let (status, run) = call(rig.admin_app(), "POST", "/admin/backups", None).await;
    assert_eq!(status, StatusCode::CREATED);
    let name = run["name"].as_str().expect("run names the backup");
    let path = rig.dir.join("backups").join(name);
    let mut bytes = std::fs::read(&path).expect("backup reads");
    // Flip the magic byte: the file stops being a database at all, so both
    // the hash and the integrity checks must fail.
    bytes[0] ^= 0xff;
    std::fs::write(&path, bytes).expect("tamper writes");

    let uri = format!("/admin/backups/{name}/restore-report");
    let (status, report) = call(rig.admin_app(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["ok"], Value::Bool(false));
    assert_eq!(report["restorable"], Value::Bool(false));
    let checks = report["checks"].as_array().expect("checks");
    let sha = checks
        .iter()
        .find(|check| check["name"] == "manifest-sha256")
        .expect("sha check runs");
    assert_eq!(sha["passed"], Value::Bool(false));
}

#[tokio::test]
async fn pre_upgrade_backup_runs_when_stale_and_skips_when_current() {
    let scratch = crate::common::ScratchDir::new("admin-preupgrade");
    let dir = scratch.to_path_buf();
    std::fs::create_dir_all(&dir).expect("scratch dir builds");
    let latest = droppedneedle::schema::latest_version();
    assert!(latest >= 1, "a migration exists to be stale against");

    // A stale database: valid SQLite, older stamp.
    let db_path = dir.join("app.db");
    {
        let connection = rusqlite::Connection::open(&db_path).expect("stale db opens");
        connection
            .execute_batch(&format!("PRAGMA user_version = {};", latest - 1))
            .expect("stale stamp writes");
    }
    let backup_dir = dir.join("backups");
    let report = droppedneedle::admin::backups::ensure_pre_upgrade_backup(&db_path, &backup_dir)
        .await
        .expect("pre-upgrade backup runs");
    assert!(report.is_some(), "stale stamp takes a backup");
    // Repeated pre-upgrade runs (no migration between them) still rotate.
    for _ in 0..BACKUP_KEEP + 2 {
        droppedneedle::admin::backups::ensure_pre_upgrade_backup(&db_path, &backup_dir)
            .await
            .expect("repeat pre-upgrade runs");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let kept = std::fs::read_dir(&backup_dir)
        .expect("backup dir reads")
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".db"))
        })
        .count();
    assert_eq!(kept, BACKUP_KEEP);

    // A current database: no backup, no directory touched.
    let rig = Rig::open("current").await;
    let current_dir = rig.dir.join("backups-current");
    let report = droppedneedle::admin::backups::ensure_pre_upgrade_backup(
        &rig.dir.join("app.db"),
        &current_dir,
    )
    .await
    .expect("current check runs");
    assert!(report.is_none(), "current stamp skips the backup");
    assert!(
        !current_dir.exists(),
        "no backup directory is created for a current database"
    );
}

#[tokio::test]
async fn pre_upgrade_backup_skips_a_fresh_database_but_not_a_legacy_one() {
    let scratch = crate::common::ScratchDir::new("admin-preupgrade-fresh");
    let dir = scratch.to_path_buf();
    std::fs::create_dir_all(&dir).expect("scratch dir builds");

    // Fresh file: stamp 0, no tables — exactly what a first boot holds.
    let db_path = dir.join("app.db");
    {
        let connection = rusqlite::Connection::open(&db_path).expect("fresh db opens");
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .expect("stamp writes");
    }
    let backup_dir = dir.join("backups");
    let report = droppedneedle::admin::backups::ensure_pre_upgrade_backup(&db_path, &backup_dir)
        .await
        .expect("fresh check runs");
    assert!(report.is_none(), "fresh database skips the backup");
    assert!(
        !backup_dir.exists(),
        "no backup directory is created for a fresh database"
    );

    // Stamp 0 with real tables is legacy data, not a fresh boot: it still
    // gets its safety net.
    let legacy_path = dir.join("legacy.db");
    {
        let connection = rusqlite::Connection::open(&legacy_path).expect("legacy db opens");
        connection
            .execute_batch("CREATE TABLE keepsakes (id TEXT PRIMARY KEY); PRAGMA user_version = 0;")
            .expect("legacy shape writes");
    }
    let legacy_backup_dir = dir.join("backups-legacy");
    let report =
        droppedneedle::admin::backups::ensure_pre_upgrade_backup(&legacy_path, &legacy_backup_dir)
            .await
            .expect("legacy check runs");
    assert!(
        report.is_some(),
        "stamp-0 database with tables still backs up"
    );
}
