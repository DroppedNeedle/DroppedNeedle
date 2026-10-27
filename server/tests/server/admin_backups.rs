//! Backup briefs: list, run, restore reports, and rotation.
//!
//! Everything runs against scratch runtimes (migrated temp databases) with
//! the admin router mounted directly and sessions injected - no network, no
//! production database.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::admin::{AdminDb, AdminSetup};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::db::undo_dir::{Stored, UNDO_DIR_NAME, UndoDir};
use droppedneedle_kernel::db::{BACKUP_KEEP, BackupService, DbRuntime};
use serde_json::Value;
use tower::ServiceExt as _;

use crate::common::auth::{AuthRig, test_principal, with_test_principal};

/// One scratch deployment: migrated runtime, auth on it, wired admin.
struct Rig {
    /// Held, never read: dropping it would close the pool out from under
    /// the admin handles.
    #[allow(dead_code)]
    runtime: DbRuntime,
    admin: AdminSetup,
    _auth: AuthRig,
    admin_id: String,
    dir: std::path::PathBuf,
    _scratch: crate::common::ScratchDir,
}

impl Rig {
    async fn open(tag: &str) -> Self {
        let scratch = crate::common::ScratchDir::new(&format!("admin-backups-{tag}"));
        let dir = scratch.to_path_buf();
        let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
        let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
            .build()
            .await;
        let rig = AuthRig::on(&platform);
        let admin_user = rig.seed_user("brenda", Role::Admin).await;
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
        let backup_dir = dir.join("backups");
        let admin = AdminSetup::new(rig.deps.clone(), quota, cache, providers)
            .with_db(AdminDb::new(runtime.db().clone()))
            .with_backups(droppedneedle_kernel::db::BackupService::new(
                &dir.join("app.db"),
                &backup_dir,
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ))
            .with_checkpoint(runtime.checkpoint().clone());
        Self {
            runtime,
            admin,
            _auth: rig,
            admin_id: admin_user.id,
            dir,
            _scratch: scratch,
        }
    }

    /// Router signed in as the admin.
    fn admin_app(&self) -> Router {
        with_test_principal(
            droppedneedle::admin::routes()
                .with_state(self.admin.clone())
                .into_router(),
            test_principal(
                &self.admin_id,
                Role::Admin,
                "sess-admin-1",
                SessionKind::Standard,
            ),
        )
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
    let (status, run) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let name = run["name"].as_str().expect("run names the backup");
    assert!(name.ends_with(".db"), "{name}");
    let sha = run["sha256"].as_str().expect("run reports sha");
    assert_eq!(sha.len(), 64, "sha256 hex length");
    assert!(run["size_bytes"].as_u64().expect("size") > 0);
    assert_eq!(
        run["user_version"].as_i64(),
        Some(droppedneedle_kernel::schema::latest_version())
    );
    // Manifest sidecar sits beside the backup on disk.
    let sidecar = rig
        .dir
        .join("backups")
        .join(format!("{name}.manifest.json"));
    assert!(sidecar.is_file(), "manifest sidecar exists");

    let (status, list) = call(rig.admin_app(), "GET", "/api/v3/admin/backups", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["backups"].as_array().expect("list").len(), 1);
    assert_eq!(list["backups"][0]["name"], run["name"]);
    assert_eq!(list["backups"][0]["sha256"], run["sha256"]);
    assert!(list["backups"][0]["created_at_unix"].as_u64().is_some());

    let uri = format!("/api/v3/admin/backups/{name}/restore-report");
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

/// Backup names sort by the wall-clock millisecond they carry. Wait for the
/// next one, so two runs never share a stamp.
async fn next_millisecond() {
    let millis = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or(0)
    };
    let stamp = millis();
    while millis() == stamp {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn backup_rotation_keeps_five() {
    let rig = Rig::open("rotation").await;
    for _ in 0..BACKUP_KEEP + 1 {
        let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
        assert_eq!(status, StatusCode::CREATED);
        next_millisecond().await;
    }
    let (status, list) = call(rig.admin_app(), "GET", "/api/v3/admin/backups", None).await;
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
        .filter(|name| name != UNDO_DIR_NAME)
        .collect();
    files.sort();
    assert_eq!(files.len(), BACKUP_KEEP * 2, "{files:?}");
}

#[tokio::test]
async fn restore_report_rejects_unsafe_names() {
    let rig = Rig::open("names").await;
    for name in ["no-suffix", "..%2Fapp.db", "sub%2Flibrary.db", ".hidden.db"] {
        let uri = format!("/api/v3/admin/backups/{name}/restore-report");
        let (status, body) = call(rig.admin_app(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert_eq!(body["error"]["code"], Value::from("INVALID_INPUT"));
    }
}

#[tokio::test]
async fn restore_report_catches_a_tampered_backup() {
    let rig = Rig::open("tamper").await;
    let (status, run) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
    assert_eq!(status, StatusCode::CREATED);
    let name = run["name"].as_str().expect("run names the backup");
    let path = rig.dir.join("backups").join(name);
    let mut bytes = std::fs::read(&path).expect("backup reads");
    // Flip the magic byte: the file stops being a database at all, so both
    // the hash and the integrity checks must fail.
    bytes[0] ^= 0xff;
    std::fs::write(&path, bytes).expect("tamper writes");

    let uri = format!("/api/v3/admin/backups/{name}/restore-report");
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

/// A backup copies the undo copies its database names into the pool
/// beside the backups, and only those. A restore puts them back beside the
/// database; one the pool lost is listed and the restore still goes
/// ahead. What an interrupted restore left is cleared first. Once no kept
/// backup names a copy, the pool lets it go.
#[tokio::test]
async fn a_backup_carries_the_undo_copies_its_database_names() {
    let rig = Rig::open("undo").await;
    let live = UndoDir::new(rig.dir.join(UNDO_DIR_NAME));
    let hold = live.hold_shared().unwrap();
    let kept = live.put(&hold, b"kept-copy").unwrap();
    let lost = live.put(&hold, b"lost-copy").unwrap();
    let loose = live.put(&hold, b"loose-copy").unwrap();
    drop(hold);
    let conn = rusqlite::Connection::open(rig.dir.join("app.db")).unwrap();
    for (sha256, owner) in [(&kept, "t1"), (&lost, "t2")] {
        conn.execute(
            "INSERT INTO library_publish_blob_refs (sha256, owner_kind, owner_id) \
             VALUES (?1, 'baseline', ?2)",
            rusqlite::params![sha256, owner],
        )
        .unwrap();
    }

    let (status, run) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let name = run["name"]
        .as_str()
        .expect("run names the backup")
        .to_owned();
    let backups = rig.dir.join("backups");
    let pool = UndoDir::new(backups.join(UNDO_DIR_NAME));
    assert!(pool.contains(&kept) && pool.contains(&lost));
    assert!(!pool.contains(&loose), "a copy nothing names was backed up");
    let (status, list) = call(rig.admin_app(), "GET", "/api/v3/admin/backups", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["backups"][0]["undo_bytes"], Value::from(18));

    // The lost copy cannot be read at all: a folder stands at its name.
    assert!(pool.remove(&pool.hold_exclusive().unwrap(), &lost).unwrap());
    std::fs::create_dir(pool.file_path(&lost).unwrap()).unwrap();
    let uri = format!("/api/v3/admin/backups/{name}/restore-report");
    let (status, report) = call(rig.admin_app(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["restorable"], Value::Bool(true));
    let undo_check = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "undo-copies")
        .expect("undo copies are checked")
        .clone();
    assert_eq!(undo_check["passed"], Value::Bool(false), "{undo_check}");

    let target = rig.dir.join("restored");
    std::fs::create_dir_all(target.join(UNDO_DIR_NAME)).unwrap();
    std::fs::write(target.join(".restore-in-progress"), b"").unwrap();
    std::fs::write(target.join(".restore-1-2-3.tmp"), b"half").unwrap();
    let backup = backups.join(&name);
    let service = BackupService::new(
        &backup,
        &backups,
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let restored = service.restore(&backup, &target, false).unwrap();
    assert_eq!(restored.undo_blobs, 1);
    assert_eq!(restored.undo_missing, vec![lost.clone()]);
    assert_eq!(
        UndoDir::new(target.join(UNDO_DIR_NAME))
            .read(&kept)
            .unwrap(),
        Stored::Found(b"kept-copy".to_vec())
    );
    let mut left: Vec<String> = std::fs::read_dir(&target)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, vec!["library.db", UNDO_DIR_NAME]);

    std::fs::remove_dir(pool.file_path(&lost).unwrap()).unwrap();
    conn.execute("DELETE FROM library_publish_blob_refs", [])
        .unwrap();
    for _ in 0..BACKUP_KEEP {
        next_millisecond().await;
        let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    assert!(
        !pool.contains(&kept),
        "the pool kept a copy no backup names"
    );
}
