//! Runtime briefs: boot checks, checkpoint reclaim, writer fairness,
//! busy path, backup round-trip, filesystem refusal, durable fabric.
//!
//! Each brief pins one behavior of the SQLite runtime against scratch
//! databases under the system temp directory. No fixture files, no network.

use crate::common;

use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use axum::{
    Extension, Router, body::Body, http::Request, middleware, response::Response, routing::get,
};
use droppedneedle::{
    db::{
        BACKUP_KEEP, CancelFlag, CheckpointMode, DbConfig, DbError, DbRuntime, LANE_QUEUE_CAPACITY,
        Lane, OpError, WakeupChannel, WriteLane, filesystem_is_local, map_sqlx_busy, open_runtime,
        sqlx_is_busy,
    },
    error::{FIXED_INTERNAL_MESSAGE, INTERNAL_ERROR, busy_response},
    ids::{REQUEST_ID_HEADER, RequestId},
    middleware::request_scope,
    schema::latest_version,
    state::AppState,
};
use tower::ServiceExt as _;

use crate::common::ScratchDir;

/// Unique scratch directory per brief, removed on drop.
fn scratch_dir(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("persist-runtime-{name}"))
}

/// Open a runtime on a fresh scratch database.
async fn open_scratch(name: &str) -> (DbRuntime, ScratchDir) {
    let dir = scratch_dir(name);
    let db = dir.join("library.db");
    let runtime = crate::common::scratch_runtime(&db).await;
    (runtime, dir)
}

/// Lets one write hold the lane until the test lets go of it.
struct Hold {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl Hold {
    /// Called inside the write: report in, then block until released. A
    /// test that fails first drops its end, which releases too.
    fn wait(self) {
        let _ = self.entered.send(());
        let _ = self.release.recv();
    }
}

/// A hold, the signal that the write is inside its transaction, and the
/// release.
fn hold() -> (
    Hold,
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, inside) = tokio::sync::oneshot::channel();
    let (let_go, release) = std::sync::mpsc::channel();
    (Hold { entered, release }, inside, let_go)
}

type QueuedWrite = Pin<Box<dyn Future<Output = ()>>>;

/// Poll each write once. `write` queues its closure on the first poll, so
/// afterwards every one is waiting in the lane, in the order given.
async fn admit(writes: &mut [QueuedWrite]) {
    std::future::poll_fn(|cx| {
        for write in writes.iter_mut() {
            assert!(
                write.as_mut().poll(cx).is_pending(),
                "a write behind a held lane cannot finish"
            );
        }
        Poll::Ready(())
    })
    .await;
}

/// A lane on a scratch database holding one table, with no pool beside it.
fn scratch_lane(name: &str) -> (WriteLane, ScratchDir) {
    let dir = scratch_dir(name);
    let db = dir.join("library.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("CREATE TABLE scratch_turns (label TEXT)", [])
        .unwrap();
    let lane = WriteLane::open(&db).unwrap();
    (lane, dir)
}

fn insert_label(tx: &rusqlite::Transaction, label: &str) -> Result<(), OpError> {
    tx.execute("INSERT INTO scratch_turns (label) VALUES (?1)", [label])?;
    Ok(())
}

/// Committed labels in the order they were written.
fn labels(lane: &WriteLane) -> Vec<String> {
    let connection = rusqlite::Connection::open_with_flags(
        lane.db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = connection
        .prepare("SELECT label FROM scratch_turns ORDER BY rowid")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
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
    let dir = scratch_dir("boot");
    let runtime = open_runtime(&DbConfig::new(&dir.join("library.db")))
        .await
        .unwrap();

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
    let (held, inside, let_go) = hold();
    let blocker = tokio::spawn(async move {
        lane.write(Lane::Foreground, "brief-blocker", move |tx| {
            held.wait();
            tx.execute("INSERT INTO scratch_order (label) VALUES ('block')", [])?;
            blocker_order.lock().unwrap().push("block".to_owned());
            Ok(())
        })
        .await
        .unwrap();
    });
    inside.await.unwrap();

    let mut queued: Vec<QueuedWrite> = Vec::new();
    for index in 0..9 {
        let lane = runtime.lane().clone();
        let order = Arc::clone(&order);
        queued.push(Box::pin(async move {
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
    queued.push(Box::pin(async move {
        lane.write(Lane::Background, "brief-bg", move |tx| {
            tx.execute("INSERT INTO scratch_order (label) VALUES ('bg')", [])?;
            order_bg.lock().unwrap().push("bg".to_owned());
            Ok(())
        })
        .await
        .unwrap();
    }));
    admit(&mut queued).await;

    let_go.send(()).unwrap();
    blocker.await.unwrap();
    for write in queued {
        write.await;
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
    let (held, inside, let_go) = hold();
    let holder = tokio::spawn(async move {
        lane.write(Lane::Foreground, "brief-hold", move |tx| {
            tx.execute("INSERT INTO scratch_busy (id) VALUES (1)", [])?;
            held.wait();
            Ok(())
        })
        .await
        .unwrap();
    });
    inside.await.unwrap();

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
    let_go.send(()).unwrap();
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
    let (held, inside, let_go) = hold();
    let blocker = tokio::spawn(async move {
        lane.write(Lane::Foreground, "brief-blocker", move |tx| {
            held.wait();
            tx.execute("INSERT INTO scratch_full (id) VALUES (1)", [])?;
            Ok(())
        })
        .await
        .unwrap();
    });
    inside.await.unwrap();

    let mut fillers: Vec<QueuedWrite> = Vec::new();
    for _ in 0..LANE_QUEUE_CAPACITY {
        let lane = runtime.lane().clone();
        fillers.push(Box::pin(async move {
            lane.write(Lane::Foreground, "brief-fill", |tx| {
                tx.execute("INSERT INTO scratch_full (id) VALUES (2)", [])?;
                Ok(())
            })
            .await
            .unwrap();
        }));
    }
    admit(&mut fillers).await;

    match runtime
        .lane()
        .write(Lane::Foreground, "brief-probe", |_tx| Ok(()))
        .await
    {
        Err(DbError::Busy { operation }) => assert_eq!(operation, "brief-probe"),
        other => panic!("full lane must fail fast with Busy, got {other:?}"),
    }

    let_go.send(()).unwrap();
    blocker.await.unwrap();
    for filler in fillers {
        filler.await;
    }
    runtime.shutdown().await;
    std::fs::remove_dir_all(dir).ok();
}

/// A runaway write aborts at the hard budget and rolls back, lane intact.
/// The timeout reports the rows that transaction changed, not the
/// connection's running total.
#[tokio::test]
async fn runaway_write_aborts_at_hard_budget() {
    let (lane, _dir) = scratch_lane("abort");
    lane.write(Lane::Foreground, "brief-earlier", |tx| {
        for label in ["a", "b", "c", "d", "e"] {
            insert_label(tx, label)?;
        }
        Ok(())
    })
    .await
    .unwrap();

    let started = Instant::now();
    let outcome = lane
        .write(Lane::Foreground, "brief-runaway", |tx| {
            for label in ["x", "y", "z"] {
                insert_label(tx, label)?;
            }
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
        Err(DbError::TxTimeout { operation, changes }) => {
            assert_eq!(operation, "brief-runaway");
            assert_eq!(changes, 3);
        }
        other => panic!("runaway write must abort, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "abort must fire near the budget"
    );

    lane.write(Lane::Foreground, "brief-probe", |tx| {
        insert_label(tx, "after")
    })
    .await
    .unwrap();
    assert_eq!(labels(&lane), ["a", "b", "c", "d", "e", "after"]);

    lane.shutdown().await;
}

/// A blocking write issued on the only runtime thread gets its turn behind
/// a running async write and a queued one: neither needs that thread.
#[tokio::test]
async fn blocking_write_on_the_runtime_thread_takes_its_turn() {
    let (lane, _dir) = scratch_lane("blocking-turn");
    let running = lane.clone();
    let (held, inside, let_go) = hold();
    let holder = tokio::spawn(async move {
        running
            .write(Lane::Foreground, "brief-running", move |tx| {
                held.wait();
                insert_label(tx, "running")
            })
            .await
            .unwrap();
    });
    inside.await.unwrap();
    let queued_lane = lane.clone();
    let mut queued: Vec<QueuedWrite> = vec![Box::pin(async move {
        queued_lane
            .write(Lane::Foreground, "brief-queued", |tx| {
                insert_label(tx, "queued")
            })
            .await
            .unwrap();
    })];
    admit(&mut queued).await;

    let_go.send(()).unwrap();
    let ahead: i64 = lane
        .write_blocking(Lane::Foreground, "brief-blocking", |tx| {
            let ahead = tx.query_row("SELECT COUNT(*) FROM scratch_turns", [], |row| row.get(0))?;
            insert_label(tx, "blocking")?;
            Ok(ahead)
        })
        .unwrap();
    assert_eq!(ahead, 2, "the queued write ran without the runtime thread");

    holder.await.unwrap();
    for write in queued {
        write.await;
    }
    assert_eq!(labels(&lane), ["running", "queued", "blocking"]);
    assert_eq!(lane.wait_times().admissions, 3);
    lane.shutdown().await;
}

/// A blocking write runs on the caller's thread, so its closure may borrow.
/// No runtime is involved.
#[test]
fn blocking_write_borrows_from_its_caller() {
    let (lane, _dir) = scratch_lane("borrow");
    let label = String::from("borrowed");
    let mut written = 0;
    lane.write_blocking(Lane::Foreground, "brief-borrow", |tx| {
        insert_label(tx, &label)?;
        written += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(written, 1);
    assert_eq!(labels(&lane), [label]);
}

/// Shutdown called with one write held and others queued, from threads and
/// from async callers, waits for every accepted write to commit, then
/// returns; a write issued afterwards is refused.
#[test]
fn shutdown_drains_accepted_writes_then_refuses() {
    let (lane, _dir) = scratch_lane("shutdown");
    let (held, inside, let_go) = hold();
    let holder_lane = lane.clone();
    let holder = std::thread::spawn(move || {
        holder_lane.write_blocking(Lane::Foreground, "brief-held", move |tx| {
            held.wait();
            insert_label(tx, "held")
        })
    });
    inside.blocking_recv().unwrap();

    let threads: Vec<_> = (0..4)
        .map(|index| {
            let lane = lane.clone();
            std::thread::spawn(move || {
                let label = format!("thread-{index}");
                let outcome = lane.write_blocking(Lane::Background, "brief-thread", |tx| {
                    insert_label(tx, &label)
                });
                (label, outcome)
            })
        })
        .collect();

    // Async writes keep arriving until one is refused: by then shutdown has
    // closed the lane and is waiting on the held write.
    let mut cx = Context::from_waker(Waker::noop());
    let mut queued = Vec::new();
    let mut down = None;
    loop {
        let label = format!("task-{}", queued.len());
        let row = label.clone();
        let mut write = Box::pin(lane.write(Lane::Foreground, "brief-task", move |tx| {
            insert_label(tx, &row)
        }));
        match write.as_mut().poll(&mut cx) {
            Poll::Pending => queued.push((label, write)),
            Poll::Ready(Err(DbError::LaneClosed)) => break,
            Poll::Ready(Err(DbError::Busy { .. })) => std::thread::yield_now(),
            Poll::Ready(other) => panic!("a write behind a held lane cannot finish: {other:?}"),
        }
        if down.is_none() {
            let (down_tx, down_rx) = std::sync::mpsc::channel();
            let lane = lane.clone();
            std::thread::spawn(move || {
                current_thread_runtime().block_on(lane.shutdown());
                let _ = down_tx.send(());
            });
            down = Some(down_rx);
        }
    }

    let_go.send(()).unwrap();
    down.unwrap()
        .recv_timeout(Duration::from_secs(10))
        .expect("shutdown must return once the accepted writes have run");

    holder.join().unwrap().unwrap();
    let mut accepted = vec!["held".to_owned()];
    for (label, mut write) in queued {
        match write.as_mut().poll(&mut cx) {
            Poll::Ready(Ok(())) => accepted.push(label),
            other => panic!("an accepted write must be answered by shutdown: {other:?}"),
        }
    }
    for thread in threads {
        match thread.join().unwrap() {
            (label, Ok(())) => accepted.push(label),
            (_, Err(DbError::LaneClosed)) => {}
            (label, other) => panic!("{label} must commit or be refused, got {other:?}"),
        }
    }
    let mut written = labels(&lane);
    written.sort();
    accepted.sort();
    assert_eq!(written, accepted);

    let late = lane.write_blocking(Lane::Foreground, "brief-late", |tx| {
        insert_label(tx, "late")
    });
    assert!(
        matches!(late, Err(DbError::LaneClosed)),
        "a closed lane refuses, got {late:?}"
    );
}

/// A queued write whose future is dropped still commits, and the write
/// behind it runs.
#[tokio::test]
async fn queued_write_outlives_its_dropped_future() {
    let (lane, _dir) = scratch_lane("dropped-future");
    let running = lane.clone();
    let (held, inside, let_go) = hold();
    let holder = tokio::spawn(async move {
        running
            .write(Lane::Foreground, "brief-running", move |tx| {
                held.wait();
                insert_label(tx, "running")
            })
            .await
            .unwrap();
    });
    inside.await.unwrap();
    let queued_lane = lane.clone();
    let mut queued: Vec<QueuedWrite> = vec![Box::pin(async move {
        let _ = queued_lane
            .write(Lane::Foreground, "brief-dropped", |tx| {
                insert_label(tx, "dropped")
            })
            .await;
    })];
    admit(&mut queued).await;
    drop(queued);

    let_go.send(()).unwrap();
    holder.await.unwrap();
    lane.write(Lane::Foreground, "brief-after", |tx| {
        insert_label(tx, "after")
    })
    .await
    .unwrap();
    assert_eq!(labels(&lane), ["running", "dropped", "after"]);
    lane.shutdown().await;
}

/// A runtime dropped with one write running and two queued shuts down, the
/// three writes still commit, and the lane serves the next runtime.
#[test]
fn lane_outlives_a_runtime_dropped_with_writes_in_it() {
    let first = current_thread_runtime();
    let (lane, _dir, let_go) = first.block_on(async {
        let (lane, dir) = scratch_lane("runtime-dropped");
        let running = lane.clone();
        let (held, inside, let_go) = hold();
        tokio::spawn(async move {
            let _ = running
                .write(Lane::Foreground, "brief-running", move |tx| {
                    held.wait();
                    insert_label(tx, "running")
                })
                .await;
        });
        inside.await.unwrap();
        for label in ["first", "second"] {
            let queued_lane = lane.clone();
            let mut write = Box::pin(async move {
                let _ = queued_lane
                    .write(Lane::Foreground, "brief-queued", move |tx| {
                        insert_label(tx, label)
                    })
                    .await;
            });
            let pending =
                std::future::poll_fn(|cx| Poll::Ready(write.as_mut().poll(cx).is_pending())).await;
            assert!(pending, "a write behind a held lane cannot finish");
            tokio::spawn(write);
        }
        (lane, dir, let_go)
    });

    let (down_tx, down) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(first);
        let _ = down_tx.send(());
    });
    down.recv_timeout(Duration::from_secs(10))
        .expect("the runtime must shut down with writes in the lane");
    let_go.send(()).unwrap();

    current_thread_runtime().block_on(async {
        lane.write(Lane::Foreground, "brief-after", |tx| {
            insert_label(tx, "after")
        })
        .await
        .unwrap();
        lane.shutdown().await;
    });
    assert_eq!(labels(&lane), ["running", "first", "second", "after"]);
}

/// A closure that panics rolls its rows back and leaves the lane usable.
#[tokio::test]
async fn panicking_write_rolls_back_and_frees_the_lane() {
    let (lane, _dir) = scratch_lane("panic");
    let failed = lane
        .write(
            Lane::Foreground,
            "brief-panic-async",
            |tx| -> Result<(), OpError> {
                insert_label(tx, "async")?;
                panic!("brief: an async closure panics")
            },
        )
        .await;
    assert!(
        matches!(failed, Err(DbError::WriteFailed { .. })),
        "an async caller gets a typed error, got {failed:?}"
    );
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lane.write_blocking(
            Lane::Foreground,
            "brief-panic-blocking",
            |tx| -> Result<(), OpError> {
                insert_label(tx, "blocking")?;
                panic!("brief: a blocking closure panics")
            },
        )
    }));
    assert!(unwound.is_err(), "a blocking caller's panic is its own");

    lane.write(Lane::Foreground, "brief-after", |tx| {
        insert_label(tx, "after")
    })
    .await
    .unwrap();
    assert_eq!(labels(&lane), ["after"]);
    lane.shutdown().await;
}

/// The SQLite behaviour the lane's commit guard rests on, on a bare
/// connection: an interrupted writing statement rolls the whole transaction
/// back to autocommit, and a commit hook that refuses turns both the next
/// autocommit statement and a raw `COMMIT` into a rollback. Verified against
/// SQLite 3.46.0 as bundled by rusqlite 0.32.1.
#[test]
fn commit_hook_vetoes_what_follows_an_interrupted_transaction() {
    let dir = scratch_dir("hook-proof");
    let db = dir.join("proof.db");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute("CREATE TABLE scratch_turns (label TEXT)", [])
        .unwrap();
    let committed = || -> i64 {
        rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM scratch_turns", [], |row| row.get(0))
            .unwrap()
    };
    let commit_refused = |error: rusqlite::Error| match error {
        rusqlite::Error::SqliteFailure(code, _) => {
            code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_COMMITHOOK
        }
        _ => false,
    };

    let abort = Arc::new(AtomicBool::new(false));
    let handler_abort = Arc::clone(&abort);
    connection.progress_handler(100, Some(move || handler_abort.load(Ordering::Acquire)));
    connection.commit_hook(Some(|| true));

    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    connection
        .execute("INSERT INTO scratch_turns (label) VALUES ('before')", [])
        .unwrap();
    abort.store(true, Ordering::Release);
    let interrupted = connection
        .execute(
            "WITH RECURSIVE c(x) AS (
               SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 100000
             )
             INSERT INTO scratch_turns (label) SELECT COUNT(*) FROM c AS a, c AS b",
            [],
        )
        .unwrap_err();
    abort.store(false, Ordering::Release);
    assert!(
        matches!(
            interrupted,
            rusqlite::Error::SqliteFailure(code, _)
                if code.code == rusqlite::ffi::ErrorCode::OperationInterrupted
        ),
        "the writing statement is interrupted, got {interrupted:?}"
    );
    assert!(
        connection.is_autocommit(),
        "the interrupt rolled the whole transaction back"
    );

    let after = connection
        .execute("INSERT INTO scratch_turns (label) VALUES ('after')", [])
        .unwrap_err();
    assert!(
        commit_refused(after),
        "an autocommit statement is refused by the hook"
    );
    assert_eq!(committed(), 0, "nothing after the interrupt stuck");

    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    connection
        .execute("INSERT INTO scratch_turns (label) VALUES ('raw')", [])
        .unwrap();
    let raw = connection.execute_batch("COMMIT").unwrap_err();
    assert!(commit_refused(raw), "a raw COMMIT is refused by the hook");
    assert!(
        connection.is_autocommit(),
        "the refused COMMIT ended the transaction"
    );
    assert_eq!(committed(), 0, "the refused COMMIT rolled back");
}

/// Savepoints inside a write closure work as they do on any transaction:
/// a released one is kept, one rolled back to is not, and the write commits.
#[test]
fn write_closure_may_use_savepoints() {
    let (lane, _dir) = scratch_lane("savepoint");
    lane.write_blocking(Lane::Background, "brief-savepoint", |tx| {
        tx.execute_batch("SAVEPOINT kept")?;
        insert_label(tx, "kept")?;
        tx.execute_batch("RELEASE kept; SAVEPOINT undone")?;
        insert_label(tx, "undone")?;
        tx.execute_batch("ROLLBACK TO undone; RELEASE undone")?;
        insert_label(tx, "tail")
    })
    .unwrap();
    assert_eq!(labels(&lane), ["kept", "tail"]);
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

/// A play reported while another writer holds the database past the busy
/// timeout is retried and recorded, never dropped.
#[tokio::test]
async fn a_play_outlasting_the_busy_timeout_is_still_recorded() {
    use droppedneedle::playback::ports::{PlayHistory as _, PlayRecord};
    use droppedneedle::playback::sqlite::PlaybackDb;

    let (runtime, dir) = open_scratch("play-busy").await;
    let db_path = dir.join("library.db");
    let history = PlaybackDb::open(&db_path, Arc::new(droppedneedle::ids::UuidGenerator)).unwrap();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let holder_path = db_path.clone();
    let holder = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(holder_path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        held_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(5_500));
        connection.execute_batch("COMMIT").unwrap();
    });
    held_rx.recv().unwrap();
    history.record(
        "user-1",
        &PlayRecord {
            track_id: None,
            track_name: "Roads".to_owned(),
            artist_name: "Portishead".to_owned(),
            album_name: None,
            recording_mbid: None,
            release_group_mbid: None,
            duration_ms: None,
            source: None,
            played_at: 1_700_000_000,
        },
    );
    holder.join().unwrap();
    let plays: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_play_history")
        .fetch_one(runtime.pool())
        .await
        .unwrap();
    assert_eq!(plays, 1);
}
