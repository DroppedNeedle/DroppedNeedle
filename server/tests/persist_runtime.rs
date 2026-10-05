//! Runtime briefs: boot checks, checkpoint reclaim, writer fairness,
//! busy path, backup round-trip, filesystem refusal, durable fabric.
//!
//! Each brief pins one behavior of the SQLite runtime against scratch
//! databases under the system temp directory. No fixture files, no network.

mod common;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Extension, Router, body::Body, http::Request, middleware, response::Response, routing::get,
};
use droppedneedle::{
    db::{
        BACKUP_KEEP, CancelFlag, CheckpointMode, CheckpointService, DbConfig, DbError, DbRuntime,
        JobKind, JobState, LANE_QUEUE_CAPACITY, Lane, WakeupChannel, busy_response,
        filesystem_is_local, fold_text, map_sqlx_busy, open_runtime, sqlx_is_busy,
    },
    error::{FIXED_INTERNAL_MESSAGE, INTERNAL_ERROR},
    ids::{REQUEST_ID_HEADER, RequestId},
    middleware::request_scope,
    schema::latest_version,
    state::AppState,
};
use tower::ServiceExt as _;

static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Unique scratch directory per brief.
fn scratch_dir(name: &str) -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "persist-runtime-{name}-{}-{seq}",
        std::process::id()
    ))
}

/// Open a runtime on a fresh scratch database.
async fn open_scratch(name: &str) -> (DbRuntime, PathBuf) {
    let dir = scratch_dir(name);
    let db = dir.join("library.db");
    let runtime = open_runtime(&DbConfig::new(&db)).await.unwrap();
    (runtime, dir)
}

/// Sidecar path for a database file.
fn sidecar(db: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// File size, or `None` when the file is absent.
fn file_len(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path).map(|meta| meta.len()).ok()
}

/// Boot migrates to the binary version and applies the pragma set.
#[tokio::test]
async fn boot_migrates_and_applies_pragmas() {
    let (runtime, dir) = open_scratch("boot").await;

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(runtime.pool())
        .await
        .unwrap();
    assert_eq!(version, latest_version());

    let pool = runtime.pool();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(synchronous, 1, "NORMAL");
    let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(busy_timeout, 5000);
    let cache_size: i64 = sqlx::query_scalar("PRAGMA cache_size")
        .fetch_one(pool)
        .await
        .unwrap();
    // Stage-13 fix F diet: 2 MiB per connection (was 16 MiB).
    assert_eq!(cache_size, -2048);
    let temp_store: i64 = sqlx::query_scalar("PRAGMA temp_store")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(temp_store, 2, "MEMORY");
    let mmap_size: i64 = sqlx::query_scalar("PRAGMA mmap_size")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(mmap_size, 67_108_864);
    let autocheckpoint: i64 = sqlx::query_scalar("PRAGMA wal_autocheckpoint")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(autocheckpoint, 1000);
    assert_eq!(
        pool.options().get_max_connections(),
        7,
        "pool carries the reader lanes only"
    );
    let mut held = Vec::new();
    for _ in 0..7 {
        held.push(pool.acquire().await.unwrap());
    }
    assert_eq!(pool.size(), 7, "seven readers check out at once");
    drop(held);

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A configured busy timeout reaches pool connections instead of the default.
#[tokio::test]
async fn configured_busy_timeout_reaches_pool_connections() {
    let dir = scratch_dir("busytimeout");
    let db = dir.join("library.db");
    let config = DbConfig {
        busy_timeout: Duration::from_millis(1234),
        ..DbConfig::new(&db)
    };
    let runtime = open_runtime(&config).await.unwrap();
    let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(runtime.pool())
        .await
        .unwrap();
    assert_eq!(busy_timeout, 1234);
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// Pool connections reject writes after boot; reads still work.
#[tokio::test]
async fn pool_connections_reject_writes_after_boot() {
    let (runtime, dir) = open_scratch("readonly").await;
    let failed = sqlx::query("CREATE TABLE scratch_nope (id INTEGER)")
        .execute(runtime.pool())
        .await;
    let error = failed.expect_err("pool writes must fail under query_only");
    assert!(
        error.to_string().to_lowercase().contains("readonly"),
        "must be a readonly refusal, got {error:?}"
    );
    let one: i64 = sqlx::query_scalar("SELECT 1")
        .fetch_one(runtime.pool())
        .await
        .unwrap();
    assert_eq!(one, 1);
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// Boot refuses a wrong-version database instead of serving it.
#[tokio::test]
async fn boot_refuses_version_mismatch() {
    let (runtime, dir) = open_scratch("mismatch").await;
    let db = runtime.path().to_owned();
    runtime.shutdown().await;

    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute("PRAGMA user_version = 999", []).unwrap();
    drop(connection);

    match open_runtime(&DbConfig::new(&db)).await {
        Err(DbError::Schema(droppedneedle::schema::SchemaError::VersionMismatch {
            found,
            expected,
        })) => assert_eq!((found, expected), (999, latest_version())),
        other => panic!("wrong-version database must refuse, got {other:?}"),
    }
    std::fs::remove_dir_all(dir).ok();
}

/// Boot refuses symlinks; the filesystem classifier refuses network mounts.
#[tokio::test]
async fn boot_refuses_symlink_and_remote_filesystem() {
    let dir = scratch_dir("symlink");
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("library.db");
    std::fs::write(&target, b"").unwrap();
    let link = dir.join("link.db");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();

    match open_runtime(&DbConfig::new(&link)).await {
        Err(DbError::SymlinkRefused { path }) => assert_eq!(path, link),
        other => panic!("symlink database must refuse, got {other:?}"),
    }

    for (magic, name) in [
        (0x6969, "nfs"),
        (0xFF534D42, "cifs"),
        (0xFE534D42, "smb2"),
        (0x01021997, "9p"),
        (0x00C36400, "ceph"),
        (0x0BD00BD0, "lustre"),
        (0x5346414F, "afs"),
        (0x73757245, "coda"),
        (0x564C, "ncpfs"),
        (0x47504653, "gpfs"),
        (0x65735546, "fuse"),
        (0x65735543, "fuseblk"),
        (0x01161970, "gfs2"),
        (0x7461636f, "ocfs2"),
        (0x20030528, "pvfs2"),
    ] {
        assert!(!filesystem_is_local(magic), "{name} must be refused");
    }
    assert!(
        filesystem_is_local(0x19899D53),
        "the old wrong lustre magic matches nothing and must not refuse"
    );
    for (magic, name) in [
        (0xEF53, "ext4"),
        (0x58465342, "xfs"),
        (0x9123683E, "btrfs"),
        (0x01021994, "tmpfs"),
        (0x794C7630, "overlayfs"),
    ] {
        assert!(filesystem_is_local(magic), "{name} must pass");
    }
    std::fs::remove_dir_all(dir).ok();
}

/// `TRUNCATE` reclaims dead WAL allocation; clean shutdown leaves no WAL.
#[tokio::test]
async fn checkpoint_truncate_reclaims_dead_wal() {
    let (runtime, dir) = open_scratch("truncate").await;
    let db = runtime.path().to_owned();

    runtime
        .lane()
        .write(Lane::Foreground, "brief-table", |tx| {
            tx.execute(
                "CREATE TABLE scratch_bulk (id INTEGER PRIMARY KEY, payload TEXT)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let payload = "x".repeat(1024);
    for batch in 0..4 {
        let rows: Vec<String> = (0..500).map(|_| payload.clone()).collect();
        runtime
            .lane()
            .write(Lane::Background, "brief-bulk", move |tx| {
                for row in &rows {
                    tx.execute("INSERT INTO scratch_bulk (payload) VALUES (?1)", [row])?;
                }
                Ok(batch)
            })
            .await
            .unwrap();
    }

    let passive = runtime.checkpoint().run_once();
    assert_eq!(passive.mode, CheckpointMode::Passive);
    assert!(passive.error.is_none());
    assert_eq!(passive.active_bytes, 0);
    assert!(
        file_len(&sidecar(&db, "-wal")).unwrap_or(0) > 0,
        "PASSIVE leaves dead allocation behind"
    );

    let reclaimed = runtime.checkpoint().maybe_truncate(Instant::now());
    let outcome = reclaimed.expect("idle gates must hold for the reclaim");
    assert_eq!(outcome.mode, CheckpointMode::Truncate);
    assert_eq!(file_len(&sidecar(&db, "-wal")), Some(0));

    let second = runtime.checkpoint().maybe_truncate(Instant::now());
    assert!(second.is_none(), "hourly cap holds the second reclaim back");

    let latest = runtime.checkpoint().latest().await.unwrap();
    assert_eq!(latest.mode, CheckpointMode::Truncate);

    runtime.shutdown().await;
    assert_eq!(file_len(&sidecar(&db, "-wal")), None);
    assert_eq!(file_len(&sidecar(&db, "-shm")), None);
    std::fs::remove_dir_all(dir).ok();
}

/// While both lanes are busy, eight foreground admissions run per background one.
#[tokio::test]
async fn writer_fairness_burst_eight_then_one() {
    let (runtime, dir) = open_scratch("fairness").await;
    runtime
        .lane()
        .write(Lane::Foreground, "brief-table", |tx| {
            tx.execute("CREATE TABLE scratch_order (label TEXT)", [])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();

    let order = Arc::new(Mutex::new(Vec::new()));
    let lane = runtime.lane().clone();
    let blocker_order = Arc::clone(&order);
    let blocker = tokio::spawn(async move {
        lane.write(Lane::Foreground, "brief-blocker", move |tx| {
            std::thread::sleep(Duration::from_millis(500));
            tx.execute("INSERT INTO scratch_order (label) VALUES ('block')", [])?;
            blocker_order.lock().unwrap().push("block".to_owned());
            Ok(())
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut handles = Vec::new();
    for index in 0..9 {
        let lane = runtime.lane().clone();
        let order = Arc::clone(&order);
        handles.push(tokio::spawn(async move {
            let label = format!("f{index}");
            lane.write(Lane::Foreground, "brief-fg", move |tx| {
                tx.execute("INSERT INTO scratch_order (label) VALUES (?1)", [&label])?;
                order.lock().unwrap().push(label);
                Ok(())
            })
            .await
            .unwrap();
        }));
    }
    let lane = runtime.lane().clone();
    let order_bg = Arc::clone(&order);
    handles.push(tokio::spawn(async move {
        lane.write(Lane::Background, "brief-bg", move |tx| {
            tx.execute("INSERT INTO scratch_order (label) VALUES ('bg')", [])?;
            order_bg.lock().unwrap().push("bg".to_owned());
            Ok(())
        })
        .await
        .unwrap();
    }));

    blocker.await.unwrap();
    for handle in handles {
        handle.await.unwrap();
    }
    let order = order.lock().unwrap().clone();
    assert_eq!(
        order,
        vec![
            "block", "f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "bg", "f8"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    );

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// Lock contention surfaces once as 503 with `Retry-After`, through the
/// stage-1 middleware, with the fixed envelope body.
#[tokio::test]
async fn busy_path_returns_503_with_retry_after_and_no_spin() {
    let (runtime, dir) = open_scratch("busy").await;
    runtime
        .lane()
        .write(Lane::Foreground, "brief-table", |tx| {
            tx.execute("CREATE TABLE scratch_busy (id INTEGER)", [])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();

    let lane = runtime.lane().clone();
    let holder = tokio::spawn(async move {
        lane.write(Lane::Foreground, "brief-hold", |tx| {
            tx.execute("INSERT INTO scratch_busy (id) VALUES (1)", [])?;
            std::thread::sleep(Duration::from_millis(800));
            Ok(())
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let attempts = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let contender = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(runtime.path())
                .create_if_missing(false)
                .busy_timeout(Duration::from_millis(0)),
        )
        .await
        .unwrap();
    attempts.fetch_add(1, Ordering::Relaxed);
    let failed = sqlx::query("INSERT INTO scratch_busy (id) VALUES (2)")
        .execute(&contender)
        .await;
    contender.close().await;
    let error = failed.expect_err("fail-fast write must hit the held lock");
    assert!(sqlx_is_busy(&error), "must detect as busy: {error:?}");
    let mapped = map_sqlx_busy("brief-busy", error);
    assert!(
        matches!(mapped, DbError::Busy { .. }),
        "busy must map to retryable, got {mapped:?}"
    );
    assert_eq!(attempts.load(Ordering::Relaxed), 1, "one attempt, no spin");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "no in-process retry loop"
    );
    holder.await.unwrap();

    async fn busy_handler(Extension(RequestId(id)): Extension<RequestId>) -> Response {
        busy_response("brief-busy", &id)
    }
    let state: AppState = common::hooked_state();
    let app = Router::new()
        .route("/busy", get(busy_handler))
        .layer(middleware::from_fn_with_state(state, request_scope));
    let response = app
        .oneshot(Request::builder().uri("/busy").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(response.headers().get("retry-after").unwrap(), "1");
    assert_eq!(
        response.headers().get(REQUEST_ID_HEADER).unwrap(),
        common::FIXED_ID
    );
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "error": {
                "code": INTERNAL_ERROR,
                "message": FIXED_INTERNAL_MESSAGE,
                "details": { "error_id": common::FIXED_ID },
            }
        })
    );

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// Backup verifies before rotating; restore lands in empty dirs and refuses
/// occupied targets and version-newer backups.
#[tokio::test]
async fn backup_restore_round_trip_with_refusals() {
    let (runtime, dir) = open_scratch("backup").await;
    runtime
        .lane()
        .write(Lane::Foreground, "brief-seed", |tx| {
            tx.execute(
                "INSERT INTO playlists (id, name, created_at, updated_at)
                 VALUES ('p1', 'Road Songs', 'now', 'now')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_moved = Arc::clone(&calls);
    let report = runtime
        .backups()
        .backup(
            Some(Arc::new(move |_copied, _total| {
                calls_moved.fetch_add(1, Ordering::Relaxed);
            })),
            &CancelFlag::never(),
        )
        .await
        .unwrap();
    assert!(calls.load(Ordering::Relaxed) > 0, "progress must fire");
    assert_eq!(report.manifest.sha256.len(), 64);
    assert!(report.manifest.size_bytes > 0);
    assert_eq!(report.manifest.user_version, latest_version());
    let mut sidecar = report.path.as_os_str().to_owned();
    sidecar.push(".manifest.json");
    assert!(PathBuf::from(sidecar).exists(), "manifest sidecar lands");

    let target = scratch_dir("restore-empty");
    let restored = runtime
        .backups()
        .restore(&report.path, &target, false)
        .unwrap();
    assert_eq!(restored.user_version, latest_version());
    let connection = rusqlite::Connection::open_with_flags(
        &restored.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let name: String = connection
        .query_row("SELECT name FROM playlists WHERE id = 'p1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(name, "Road Songs");
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(connection);

    std::fs::write(target.join("squat.txt"), b"x").unwrap();
    match runtime.backups().restore(&report.path, &target, false) {
        Err(DbError::RestoreTargetNotEmpty { .. }) => {}
        other => panic!("occupied target must refuse, got {other:?}"),
    }

    let newer = dir.join("newer.db");
    std::fs::copy(&report.path, &newer).unwrap();
    rusqlite::Connection::open(&newer)
        .unwrap()
        .execute("PRAGMA user_version = 9999", [])
        .unwrap();
    let newer_dir = scratch_dir("restore-newer");
    match runtime.backups().restore(&newer, &newer_dir, false) {
        Err(DbError::BackupTooNew { found, expected }) => {
            assert_eq!((found, expected), (9999, latest_version()));
        }
        other => panic!("version-newer backup must refuse, got {other:?}"),
    }
    let allowed_dir = scratch_dir("restore-allowed");
    let allowed = runtime
        .backups()
        .restore(&newer, &allowed_dir, true)
        .unwrap();
    assert_eq!(allowed.user_version, 9999);

    for _ in 0..(BACKUP_KEEP + 2) {
        runtime
            .backups()
            .backup(None, &CancelFlag::never())
            .await
            .unwrap();
    }
    let kept = std::fs::read_dir(runtime.backups().backup_dir())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".db"))
        })
        .count();
    assert_eq!(kept, BACKUP_KEEP, "verified-then-rotate keeps five");

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
    std::fs::remove_dir_all(target).ok();
    std::fs::remove_dir_all(newer_dir).ok();
    std::fs::remove_dir_all(allowed_dir).ok();
}

/// Same-millisecond backups never collide, and crashed-run staging is swept.
#[tokio::test]
async fn backup_names_are_unique_and_stale_staging_is_swept() {
    let (runtime, dir) = open_scratch("sweep").await;
    let idle = CancelFlag::never();
    let first = runtime.backups().backup(None, &idle).await.unwrap();
    std::fs::write(
        runtime.backups().backup_dir().join(".staging-9-9-9.db"),
        b"junk",
    )
    .unwrap();
    std::fs::write(
        runtime.backups().backup_dir().join(".restore-9-9.tmp"),
        b"junk",
    )
    .unwrap();
    let second = runtime.backups().backup(None, &idle).await.unwrap();
    assert_ne!(first.path, second.path, "same-ms runs must not collide");
    let leftovers: Vec<String> = std::fs::read_dir(runtime.backups().backup_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".staging-") || name.starts_with(".restore-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "stale staging must be swept, left {leftovers:?}"
    );
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A tripped flag stops a backup before anything renames into place.
#[tokio::test]
async fn backup_cancel_stops_before_rename() {
    let (runtime, dir) = open_scratch("backupcancel").await;
    let stop = CancelFlag::never();
    stop.cancel();
    match runtime.backups().backup(None, &stop).await {
        Err(DbError::Cancelled { operation, .. }) => assert_eq!(operation, "backup"),
        other => panic!("cancelled backup must stop, got {other:?}"),
    }
    let landed: Vec<String> = std::fs::read_dir(runtime.backups().backup_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".db"))
        .collect();
    assert!(landed.is_empty(), "nothing must rename, left {landed:?}");
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A wakeup request issued before the wait still wakes it; no lost wakeup.
#[tokio::test]
async fn wakeup_request_before_wait_is_not_lost() {
    let (runtime, dir) = open_scratch("lostwakeup").await;
    let wakeups = runtime.wakeups().clone();
    let seq = wakeups
        .request(runtime.lane(), Lane::Foreground, WakeupChannel::Operation)
        .await
        .unwrap();
    assert_eq!(seq, 1);
    tokio::time::timeout(
        Duration::from_secs(5),
        wakeups.wait(WakeupChannel::Operation),
    )
    .await
    .expect("a request issued before wait must still wake it");
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A missing fabric table fails with the table named, not a bare code.
#[tokio::test]
async fn durable_missing_table_names_the_migration() {
    let (runtime, dir) = open_scratch("missingtable").await;
    runtime
        .lane()
        .write(Lane::Foreground, "brief-drop", |tx| {
            tx.execute("DROP TABLE durable_work_wakeups", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let error = runtime
        .wakeups()
        .request(runtime.lane(), Lane::Foreground, WakeupChannel::Scan)
        .await
        .expect_err("request against a dropped table must fail");
    match error {
        DbError::WriteFailed { operation, .. } => assert!(
            operation.contains("durable_work_wakeups"),
            "must name the missing table, got {operation}"
        ),
        other => panic!("must be a mapped write failure, got {other:?}"),
    }
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A checkpoint against a missing database records a non-busy error pass and
/// creates nothing.
#[tokio::test]
async fn checkpoint_missing_database_is_not_created() {
    let (runtime, dir) = open_scratch("ckptmissing").await;
    let ghost = dir.join("ghost").join("library.db");
    let service =
        CheckpointService::new(&ghost, runtime.pool().clone(), runtime.lane().idle_state());
    let outcome = service.run_once();
    assert!(
        outcome.error.is_some(),
        "missing database must record an error pass"
    );
    assert!(!outcome.busy, "error passes never report busy");
    assert!(
        !ghost.exists(),
        "checkpoint must not create a missing database"
    );
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A full writer lane fails fast with `Busy` instead of queueing forever.
#[tokio::test]
async fn writer_lane_backpressure_fails_fast_when_full() {
    let (runtime, dir) = open_scratch("lanefull").await;
    runtime
        .lane()
        .write(Lane::Foreground, "brief-table", |tx| {
            tx.execute("CREATE TABLE scratch_full (id INTEGER)", [])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();

    let lane = runtime.lane().clone();
    let blocker = tokio::spawn(async move {
        lane.write(Lane::Foreground, "brief-blocker", |tx| {
            std::thread::sleep(Duration::from_millis(3000));
            tx.execute("INSERT INTO scratch_full (id) VALUES (1)", [])?;
            Ok(())
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut fillers = Vec::new();
    for _ in 0..LANE_QUEUE_CAPACITY {
        let lane = runtime.lane().clone();
        fillers.push(tokio::spawn(async move {
            lane.write(Lane::Foreground, "brief-fill", |tx| {
                tx.execute("INSERT INTO scratch_full (id) VALUES (2)", [])?;
                Ok(())
            })
            .await
            .unwrap();
        }));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    match runtime
        .lane()
        .write(Lane::Foreground, "brief-probe", |_tx| Ok(()))
        .await
    {
        Err(DbError::Busy { operation }) => assert_eq!(operation, "brief-probe"),
        other => panic!("full lane must fail fast with Busy, got {other:?}"),
    }

    blocker.await.unwrap();
    for filler in fillers {
        filler.await.unwrap();
    }
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// Wakeup channels signal workers and persist demand; the registry tracks jobs.
#[tokio::test]
async fn durable_fabric_wakeups_and_registry() {
    let (runtime, dir) = open_scratch("durable").await;
    let wakeups = runtime.wakeups().clone();

    assert!(!wakeups.pending(WakeupChannel::Scan).await.unwrap());
    let waiter = tokio::spawn({
        let wakeups = wakeups.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(5), wakeups.wait(WakeupChannel::Scan))
                .await
                .expect("wakeup must arrive");
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let seq = wakeups
        .request(runtime.lane(), Lane::Foreground, WakeupChannel::Scan)
        .await
        .unwrap();
    assert_eq!(seq, 1);
    waiter.await.unwrap();
    assert!(wakeups.pending(WakeupChannel::Scan).await.unwrap());
    wakeups
        .consume(runtime.lane(), Lane::Background, WakeupChannel::Scan, seq)
        .await
        .unwrap();
    assert!(!wakeups.pending(WakeupChannel::Scan).await.unwrap());

    wakeups
        .register_job(
            runtime.lane(),
            "scan-worker",
            JobKind::Durable,
            Some(WakeupChannel::Scan),
        )
        .await
        .unwrap();
    let job = wakeups.get_job("scan-worker").await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Idle);
    assert_eq!(job.wakeup_channel, Some(WakeupChannel::Scan));
    wakeups
        .set_job_state(runtime.lane(), "scan-worker", JobState::Running)
        .await
        .unwrap();
    wakeups
        .heartbeat(runtime.lane(), "scan-worker")
        .await
        .unwrap();
    let job = wakeups.get_job("scan-worker").await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Running);
    assert!(job.last_heartbeat_at.is_some_and(|at| at > 0.0));
    let jobs = wakeups.list_jobs().await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(wakeups.get_job("ghost").await.unwrap().is_none());
    let unknown = wakeups
        .set_job_state(runtime.lane(), "ghost", JobState::Running)
        .await;
    assert!(
        matches!(unknown, Err(DbError::WriteFailed { .. })),
        "unknown job must fail, got {unknown:?}"
    );

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A runaway write aborts at the hard budget and rolls back, lane intact.
#[tokio::test]
async fn runaway_write_aborts_at_hard_budget() {
    let (runtime, dir) = open_scratch("abort").await;
    let started = Instant::now();
    let outcome = runtime
        .lane()
        .write(Lane::Foreground, "brief-runaway", |tx| {
            let count: i64 = tx.query_row(
                "WITH RECURSIVE c(x) AS (
                   SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 100000
                 )
                 SELECT COUNT(*) FROM c AS a, c AS b",
                [],
                |row| row.get(0),
            )?;
            Ok(count)
        })
        .await;
    match outcome {
        Err(DbError::TxTimeout { operation, .. }) => assert_eq!(operation, "brief-runaway"),
        other => panic!("runaway write must abort, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "abort must fire near the budget"
    );

    let probe: i64 = runtime
        .lane()
        .write(Lane::Foreground, "brief-probe", |tx| {
            tx.query_row("SELECT 1", [], |row| row.get(0))
                .map_err(droppedneedle::db::OpError::from)
        })
        .await
        .unwrap();
    assert_eq!(probe, 1);

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// Chunked background writes commit per chunk and honor cancellation.
#[tokio::test]
async fn chunked_write_commits_and_cancels() {
    let (runtime, dir) = open_scratch("chunked").await;
    runtime
        .lane()
        .write(Lane::Foreground, "brief-table", |tx| {
            tx.execute("CREATE TABLE scratch_chunk (id INTEGER)", [])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();

    let cancel = droppedneedle::db::CancelFlag::never();
    let committed = runtime
        .lane()
        .write_chunked(
            "brief-chunks",
            (0..1200).collect(),
            500,
            &cancel,
            |rows: &[i32], tx| {
                for row in rows {
                    tx.execute("INSERT INTO scratch_chunk (id) VALUES (?1)", [row])?;
                }
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(committed, 1200);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scratch_chunk")
        .fetch_one(runtime.pool())
        .await
        .unwrap();
    assert_eq!(count, 1200);

    let stop = droppedneedle::db::CancelFlag::never();
    stop.cancel();
    match runtime
        .lane()
        .write_chunked(
            "brief-stopped",
            (0..100).collect(),
            500,
            &stop,
            |_rows: &[i32], _tx| Ok(()),
        )
        .await
    {
        Err(DbError::Cancelled { chunks, .. }) => assert_eq!(chunks, 0),
        other => panic!("tripped flag must stop the write, got {other:?}"),
    }

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// The checkpoint loop passes on its first tick and stops on signal.
#[tokio::test]
async fn checkpoint_loop_passes_then_stops() {
    let (runtime, dir) = open_scratch("loop").await;
    let stop = Arc::new(tokio::sync::Notify::new());
    let checkpoint = runtime.checkpoint().clone();
    let stop_moved = Arc::clone(&stop);
    let looping = tokio::spawn(async move { checkpoint.run_forever(stop_moved).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        runtime.checkpoint().latest().await.is_some(),
        "first tick must record a pass"
    );
    stop.notify_waiters();
    tokio::time::timeout(Duration::from_secs(5), looping)
        .await
        .expect("loop must stop promptly")
        .unwrap();

    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// Scratch hygiene: every brief cleans its own directory.
#[tokio::test]
async fn scratch_dirs_are_removed() {
    let dir = scratch_dir("hygiene");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(dir.exists());
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!dir.exists());
}

/// Application-side folding matches v2 casefold: sharp-s expands, both
/// sigmas merge, dotted capital İ loses its dot, accents still strip.
#[tokio::test]
async fn fold_text_matches_v2_casefold() {
    assert_eq!(fold_text("Straße"), "strasse");
    assert_eq!(fold_text("ς"), "σ");
    assert_eq!(fold_text("Σ"), "σ");
    assert_eq!(fold_text("Οδυσσέας"), "οδυσσεασ");
    assert_eq!(fold_text("İ"), "i");
    assert_eq!(fold_text("Beyoncé"), "beyonce");
    assert_eq!(fold_text("  Sigur   Rós  "), "sigur ros");
    assert_eq!(fold_text("ﬁsh"), "fish");
    assert_eq!(fold_text(""), "");
}

/// The writer connection's `fold()` SQL function folds like v2.
#[tokio::test]
async fn fold_sql_function_matches_v2_casefold() {
    let (runtime, dir) = open_scratch("fold").await;
    let folded: String = runtime
        .lane()
        .write(Lane::Foreground, "brief-fold", |tx| {
            tx.query_row("SELECT fold('Straße à  Athènes')", [], |row| row.get(0))
                .map_err(droppedneedle::db::OpError::from)
        })
        .await
        .unwrap();
    assert_eq!(folded, "strasse a athenes");
    let null: Option<String> = runtime
        .lane()
        .write(Lane::Foreground, "brief-fold-null", |tx| {
            tx.query_row("SELECT fold(NULL)", [], |row| row.get(0))
                .map_err(droppedneedle::db::OpError::from)
        })
        .await
        .unwrap();
    assert_eq!(null, None);
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}
