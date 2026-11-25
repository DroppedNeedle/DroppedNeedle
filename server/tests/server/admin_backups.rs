//! Backup briefs: list, run, restore reports, rotation, room and the
//! nightly loop.
//!
//! Everything runs against scratch runtimes (migrated temp databases) with
//! the admin router mounted directly and sessions injected - no network, no
//! production database.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::admin::AdminSetup;
use droppedneedle::admin::backups::NightlyBackup;
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::db::undo_dir::{Stored, UNDO_DIR_NAME, UndoDir};
use droppedneedle_kernel::db::{BackupService, DbRuntime, RETENTION};
use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_kernel::test_support::ManualClock;
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
        Self::open_with_space(tag, droppedneedle_kernel::disk::free_bytes).await
    }

    /// A rig whose backup service reads free space through `probe`.
    async fn open_with_space(
        tag: &str,
        probe: fn(&std::path::Path) -> std::io::Result<u64>,
    ) -> Self {
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
            )
            .expect("quota ledger"),
        );
        let providers = Arc::new(droppedneedle_providers::Providers::build(
            &platform,
            crate::common::closed_outbound().providers(),
            droppedneedle_kernel::db::cache::CacheDb::scratch(),
        ));
        let backup_dir = dir.join("backups");
        let jobs = crate::common::bundles::jobs_at(
            &platform,
            runtime.checkpoint().clone(),
            Arc::new(crate::common::bundles::NothingToWarm),
            droppedneedle::bootstrap::jobs::ScansEnded::default(),
        );
        let admin = AdminSetup::new(
            rig.deps.clone(),
            quota,
            providers,
            runtime.db().clone(),
            std::sync::Arc::new(
                droppedneedle_kernel::db::BackupService::new(
                    &dir.join("app.db"),
                    &backup_dir,
                    std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
                    false,
                )
                .with_space_probe(probe),
            ),
            runtime.checkpoint().clone(),
            platform.supervisor.clone(),
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
    assert!(name.ends_with(".db.gz"), "{name}");
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
    assert_eq!(list["backups"][0]["kind"], Value::from("manual"));
    assert!(list["backups"][0]["created_at_unix"].as_u64().is_some());
    assert_eq!(
        list["keep"],
        serde_json::json!({"daily": 7, "weekly": 4, "pre_upgrade": 3, "manual": 3})
    );

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
        "schema-floor",
    ] {
        assert!(names.contains(&expected), "{names:?}");
    }
    // The copy the check unpacked is gone.
    let left = files(&rig.dir.join("backups"));
    assert!(left.iter().all(|file| !file.starts_with('.')), "{left:?}");
}

/// Every name in a folder, sorted.
fn files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("folder reads")
        .map(|entry| {
            entry
                .expect("entry reads")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
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
async fn backup_rotation_keeps_the_newest_taken_by_hand() {
    let rig = Rig::open("rotation").await;
    for _ in 0..RETENTION.manual + 1 {
        let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
        assert_eq!(status, StatusCode::CREATED);
        next_millisecond().await;
    }
    let (status, list) = call(rig.admin_app(), "GET", "/api/v3/admin/backups", None).await;
    assert_eq!(status, StatusCode::OK);
    let backups = list["backups"].as_array().expect("list");
    assert_eq!(backups.len(), RETENTION.manual);
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
    assert_eq!(files.len(), RETENTION.manual * 2, "{files:?}");
}

/// Too little free space: "Back up now" answers 409 with what it needs,
/// stages nothing and keeps every backup.
#[tokio::test]
async fn back_up_now_without_room_is_refused_and_keeps_every_backup() {
    let rig = Rig::open_with_space("room", |_| Ok(5 * 1024 * 1024)).await;
    let backups = rig.dir.join("backups");
    BackupService::new(
        &rig.dir.join("app.db"),
        &backups,
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        false,
    )
    .backup(
        droppedneedle_kernel::db::BackupKind::Manual,
        None,
        &droppedneedle_kernel::db::CancelFlag::never(),
    )
    .await
    .expect("a backup with room runs");
    let before = files(&backups);

    let (status, body) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], Value::from("BACKUP_NO_ROOM"));
    assert_eq!(body["error"]["details"]["free_mb"], Value::from(5));
    assert!(
        body["error"]["details"]["needed_mb"]
            .as_u64()
            .expect("needed")
            > 64,
        "{body}"
    );
    assert_eq!(files(&backups), before, "the folder changed");
}

/// A nightly backup under a day old means none is due, after a restart too:
/// what is due is read from the backups folder alone. A day after the
/// first, the next is taken.
#[tokio::test]
async fn a_nightly_backup_is_taken_once_a_day() {
    let scratch = crate::common::ScratchDir::new("admin-backups-nightly");
    let dir = scratch.to_path_buf();
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
        .build()
        .await;
    let clock = ManualClock::at(1_767_225_600_000);
    let backups = dir.join("backups");
    let service = || {
        Arc::new(BackupService::new(
            &dir.join("app.db"),
            &backups,
            Arc::new(clock.clone()),
            false,
        ))
    };
    let nightly_count = || {
        files(&backups)
            .iter()
            .filter(|name| name.ends_with(".db.gz"))
            .count()
    };
    let token = CancellationToken::new();
    const HOUR: i64 = 60 * 60 * 1000;

    let mut nightly = NightlyBackup::new(service(), platform.settings.live());
    nightly.pass(&token).await.expect("the first pass backs up");
    assert_eq!(nightly_count(), 1);

    clock.advance(23 * HOUR);
    let mut nightly = NightlyBackup::new(service(), platform.settings.live());
    nightly.pass(&token).await.expect("a pass after a restart");
    assert_eq!(nightly_count(), 1, "a backup under a day old was doubled");

    clock.advance(HOUR);
    nightly.pass(&token).await.expect("a pass a day later");
    assert_eq!(nightly_count(), 2);
    drop(runtime);
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
        false,
    );
    let restored = service
        .restore(&backup, &target, "library.db", false)
        .unwrap();
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
    assert_eq!(left, vec!["library.db", "library.db.lock", UNDO_DIR_NAME]);

    std::fs::remove_dir(pool.file_path(&lost).unwrap()).unwrap();
    conn.execute("DELETE FROM library_publish_blob_refs", [])
        .unwrap();
    for _ in 0..RETENTION.manual {
        next_millisecond().await;
        let (status, _) = call(rig.admin_app(), "POST", "/api/v3/admin/backups", None).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    assert!(
        !pool.contains(&kept),
        "the pool kept a copy no backup names"
    );
}
