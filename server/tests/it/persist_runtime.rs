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
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use axum::{
    Extension, Router, body::Body, http::Request, middleware, response::Response, routing::get,
};
use droppedneedle::{middleware::request_scope, state::AppState};
use droppedneedle_kernel::{
    db::{
        BACKGROUND_QUEUE_CAPACITY, BACKUP_KEEP, Budget, CancelFlag, CheckpointMode, Db, DbConfig,
        DbError, DbRuntime, FOREGROUND_QUEUE_CAPACITY, LONG_READERS, Lane, LaneLimits, OpError,
        WakeupChannel, WriteLane, block_on, filesystem_is_local, map_sqlx_busy, open_runtime,
        sqlx_is_busy,
    },
    error::{FIXED_INTERNAL_MESSAGE, INTERNAL_ERROR, busy_response},
    ids::{REQUEST_ID_HEADER, RequestId},
    jobs::{JobRegistry, checkpoint, registry::DurableRegistryStore},
    schema::latest_version,
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

/// How long a brief's shutdown lets queued writes finish: far longer than
/// any of them takes.
const DRAIN: Duration = Duration::from_secs(10);

/// A lane on a scratch database holding one table, with no pool beside it.
fn scratch_lane(name: &str) -> (WriteLane, ScratchDir) {
    scratch_lane_with(name, LaneLimits::default())
}

/// [`scratch_lane`] with time limits a brief can reach quickly.
fn scratch_lane_with(name: &str, limits: LaneLimits) -> (WriteLane, ScratchDir) {
    let dir = scratch_dir(name);
    let db = dir.join("library.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("CREATE TABLE scratch_turns (label TEXT)", [])
        .unwrap();
    let lane = WriteLane::open_with(&db, limits).unwrap();
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

/// Wait until `count` writes are queued on the lane, and fail if they never
/// are.
fn wait_queued(lane: &WriteLane, count: usize) {
    let waiting = lane.idle_state();
    let deadline = Instant::now() + Duration::from_secs(10);
    while waiting.queued() < count {
        assert!(
            Instant::now() < deadline,
            "{count} writes must be waiting, found {}",
            waiting.queued()
        );
        std::thread::yield_now();
    }
}

/// Run `work` on its own thread and fail if it has not finished in ten
/// seconds, so a write that is wrongly kept waiting fails its brief instead
/// of hanging it.
fn within<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(work());
    });
    result
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("{what}"))
}

/// Poll `ready` until it holds, and fail if it does not within ten seconds.
async fn until<F, Fut>(what: &str, mut ready: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready().await {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
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
        Err(DbError::Schema(droppedneedle_kernel::schema::SchemaError::VersionMismatch {
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
    for _ in 0..FOREGROUND_QUEUE_CAPACITY {
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
    for _ in 0..200 {
        let lane = runtime.lane().clone();
        fillers.push(Box::pin(async move {
            lane.write(Lane::Background, "brief-job", |tx| {
                tx.execute("INSERT INTO scratch_full (id) VALUES (3)", [])?;
                Ok(())
            })
            .await
            .unwrap();
        }));
    }
    admit(&mut fillers[FOREGROUND_QUEUE_CAPACITY..]).await;

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
/// The abort interrupts a writing statement, which rolls the transaction
/// back; a closure that swallows that and writes one more row leaves no row,
/// and the caller hears the timeout whatever the closure returned. The
/// timeout reports the rows that transaction changed, not the connection's
/// running total.
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
            let _interrupted = tx.execute(
                "WITH RECURSIVE c(x) AS (
                   SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 100000
                 )
                 INSERT INTO scratch_turns (label)
                 SELECT COUNT(*) FROM c AS a, c AS b",
                [],
            );
            let _refused = insert_label(tx, "tail");
            Ok(())
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

    lane.shutdown(DRAIN).await;
}

/// Run short queries inside a write until `span` of real time has gone.
fn burn(tx: &rusqlite::Transaction, span: Duration) -> Result<(), OpError> {
    let started = Instant::now();
    while started.elapsed() < span {
        let _rows: i64 = tx.query_row(
            "WITH RECURSIVE c(x) AS (
               SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 20000
             )
             SELECT COUNT(*) FROM c",
            [],
            |row| row.get(0),
        )?;
    }
    Ok(())
}

/// Each class has its own budget, here scaled down from 2 s and 5 s. A
/// write that outlasts the foreground budget is rolled back on the
/// foreground lane and commits on the background lane; a background write
/// past its own budget is rolled back.
#[tokio::test]
async fn background_budget_outlasts_the_foreground_one() {
    let foreground_abort = Duration::from_millis(100);
    let (lane, _dir) = scratch_lane_with(
        "class-budgets",
        LaneLimits {
            foreground: Budget {
                warn: foreground_abort,
                abort: foreground_abort,
            },
            background: Budget {
                warn: Duration::from_secs(1),
                abort: Duration::from_secs(1),
            },
            ..LaneLimits::default()
        },
    );
    let request = lane
        .write(Lane::Foreground, "brief-request", move |tx| {
            burn(tx, foreground_abort * 3)?;
            insert_label(tx, "request")
        })
        .await;
    assert!(
        matches!(&request, Err(DbError::TxTimeout { operation, .. }) if operation == "brief-request"),
        "a foreground write past its budget is aborted, got {request:?}"
    );
    lane.write(Lane::Background, "brief-job", move |tx| {
        burn(tx, foreground_abort * 3)?;
        insert_label(tx, "job")
    })
    .await
    .unwrap();

    let runaway = lane
        .write(Lane::Background, "brief-runaway", |tx| {
            insert_label(tx, "runaway")?;
            burn(tx, Duration::from_secs(60))
        })
        .await;
    assert!(
        matches!(&runaway, Err(DbError::TxTimeout { operation, changes: 1 }) if operation == "brief-runaway"),
        "a background write past its own budget is aborted, got {runaway:?}"
    );
    assert_eq!(labels(&lane), ["job"]);
    lane.shutdown(DRAIN).await;
}

/// A foreground write that has waited past the limit behind a held lane is
/// answered `Busy` and never runs, whether its caller is parked on a thread
/// or awaiting on a runtime whose clock is paused. A background write in the
/// same position commits once the holder is done, and so does a durable
/// foreground write that queued before the refused ones.
#[tokio::test(start_paused = true)]
async fn foreground_wait_past_its_limit_is_refused() {
    let dir = scratch_dir("foreground-wait");
    let db = dir.join("library.db");
    common::migrated_db(&db);
    let config = DbConfig::new(&db).with_lane_limits(LaneLimits {
        foreground_wait: Duration::from_millis(50),
        watch_tick: Duration::from_millis(10),
        ..LaneLimits::default()
    });
    // The lane comes from the production runtime, so the watch at work here
    // is the one that runtime starts. The pool's timeouts run on a runtime's
    // clock, so it is opened and closed on one whose clock is not paused.
    let unpaused = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let runtime = std::thread::scope(|scope| {
        scope
            .spawn(|| unpaused.block_on(open_runtime(&config)))
            .join()
    })
    .unwrap()
    .unwrap();
    let lane = runtime.lane().clone();
    lane.write_blocking(Lane::Foreground, "brief-table", |tx| {
        tx.execute("CREATE TABLE scratch_turns (label TEXT)", [])?;
        Ok(())
    })
    .unwrap();
    let running = lane.clone();
    let (held, inside, let_go) = hold();
    let holder = tokio::spawn(async move {
        running
            .write(Lane::Background, "brief-held", move |tx| {
                held.wait();
                insert_label(tx, "held")
            })
            .await
            .unwrap();
    });
    inside.await.unwrap();

    let journal = lane.clone();
    let durable = std::thread::spawn(move || {
        journal.write_durable(Lane::Foreground, "brief-durable", |tx| {
            insert_label(tx, "durable")
        })
    });
    wait_queued(&lane, 1);
    let parked = lane.clone();
    let (answered, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = parked.write_blocking(Lane::Foreground, "brief-parked", |tx| {
            insert_label(tx, "parked")
        });
        let _ = answered.send(outcome);
    });
    let mut request = Box::pin(lane.write(Lane::Foreground, "brief-request", |tx| {
        insert_label(tx, "request")
    }));
    let mut job = Box::pin(lane.write(Lane::Background, "brief-job", |tx| insert_label(tx, "job")));
    std::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        assert!(job.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;

    let refused = answer
        .recv_timeout(Duration::from_secs(10))
        .expect("a foreground write past its limit must be answered");
    assert!(
        matches!(&refused, Err(DbError::Busy { operation }) if operation == "brief-parked"),
        "a parked foreground caller hears Busy, got {refused:?}"
    );
    let refused = request.await;
    assert!(
        matches!(&refused, Err(DbError::Busy { operation }) if operation == "brief-request"),
        "an awaiting foreground caller hears Busy, got {refused:?}"
    );

    let_go.send(()).unwrap();
    holder.await.unwrap();
    job.await.unwrap();
    let waited = durable.join().unwrap();
    assert!(
        waited.is_ok(),
        "a durable write waits out the limit, got {waited:?}"
    );
    assert_eq!(labels(&lane), ["held", "durable", "job"]);
    std::thread::spawn(move || unpaused.block_on(runtime.shutdown()))
        .join()
        .unwrap();
}

fn synchronous(tx: &rusqlite::Transaction) -> Result<i64, OpError> {
    Ok(tx.query_row("PRAGMA synchronous", [], |row| row.get(0))?)
}

/// A durable write runs with `synchronous=FULL` (2) and the write after it
/// is back at `NORMAL` (1), also when the durable closure failed or
/// panicked.
#[test]
fn durable_write_syncs_fully_and_the_next_write_does_not() {
    let (lane, _dir) = scratch_lane("durable");
    let ordinary = || {
        lane.write_blocking(Lane::Foreground, "brief-ordinary", synchronous)
            .unwrap()
    };
    assert_eq!(ordinary(), 1);

    let inside = lane
        .write_durable(Lane::Background, "brief-durable", |tx| {
            insert_label(tx, "durable")?;
            synchronous(tx)
        })
        .unwrap();
    assert_eq!((inside, ordinary()), (2, 1));

    let inside = std::cell::Cell::new(0);
    let failed = lane.write_durable(
        Lane::Background,
        "brief-durable-failed",
        |tx| -> Result<(), OpError> {
            inside.set(synchronous(tx)?);
            Err(OpError::Abort("brief: a durable closure fails".to_owned()))
        },
    );
    assert!(matches!(failed, Err(DbError::WriteFailed { .. })));
    assert_eq!((inside.get(), ordinary()), (2, 1));

    inside.set(0);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lane.write_durable(
            Lane::Background,
            "brief-durable-panic",
            |tx| -> Result<(), OpError> {
                inside.set(synchronous(tx)?);
                panic!("brief: a durable closure panics")
            },
        )
    }));
    assert!(unwound.is_err());
    assert_eq!((inside.get(), ordinary()), (2, 1));
    assert_eq!(labels(&lane), ["durable"]);
}

/// Shutdown behind a write that does not return gives up at its bound: it
/// comes back, and the writes still queued, from a thread and from an async
/// caller, are answered `LaneClosed` and never run.
#[test]
fn shutdown_gives_up_on_a_holder_that_does_not_return() {
    let (lane, _dir) = scratch_lane("shutdown-stuck");
    let (held, inside, let_go) = hold();
    let holder_lane = lane.clone();
    let holder = std::thread::spawn(move || {
        holder_lane.write_blocking(Lane::Foreground, "brief-stuck", move |tx| {
            held.wait();
            insert_label(tx, "stuck")
        })
    });
    inside.blocking_recv().unwrap();

    let mut cx = Context::from_waker(Waker::noop());
    let mut queued = Box::pin(lane.write(Lane::Background, "brief-queued", |tx| {
        insert_label(tx, "queued")
    }));
    assert!(queued.as_mut().poll(&mut cx).is_pending());
    let parked_lane = lane.clone();
    let parked = std::thread::spawn(move || {
        parked_lane.write_blocking(Lane::Foreground, "brief-parked", |tx| {
            insert_label(tx, "parked")
        })
    });
    wait_queued(&lane, 2);

    let (down_tx, down) = std::sync::mpsc::channel();
    let closing = lane.clone();
    std::thread::spawn(move || {
        current_thread_runtime().block_on(closing.shutdown(Duration::from_millis(100)));
        let _ = down_tx.send(());
    });
    down.recv_timeout(Duration::from_secs(10))
        .expect("shutdown must give up on a write that does not return");
    let refused = queued.as_mut().poll(&mut cx);
    assert!(
        matches!(refused, Poll::Ready(Err(DbError::LaneClosed))),
        "a queued async write hears LaneClosed, got {refused:?}"
    );
    let refused = parked.join().unwrap();
    assert!(
        matches!(refused, Err(DbError::LaneClosed)),
        "a parked write hears LaneClosed, got {refused:?}"
    );

    let_go.send(()).unwrap();
    holder.join().unwrap().unwrap();
    assert_eq!(labels(&lane), ["stuck"]);
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
    lane.shutdown(DRAIN).await;
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
                current_thread_runtime().block_on(lane.shutdown(DRAIN));
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
    lane.shutdown(DRAIN).await;
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
        lane.shutdown(DRAIN).await;
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
    lane.shutdown(DRAIN).await;
}

/// True for the error SQLite returns when a commit hook refuses a commit.
fn commit_refused(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_COMMITHOOK
    )
}

/// A raw `COMMIT` inside a closure is refused and ends the transaction.
/// What the closure writes afterwards, here inside a savepoint it leaves
/// open, is not committed either, and the caller is told.
#[test]
fn raw_commit_in_a_closure_commits_nothing() {
    let (lane, _dir) = scratch_lane("raw-commit");
    let mut refused = false;
    let outcome = lane.write_blocking(Lane::Foreground, "brief-raw-commit", |tx| {
        insert_label(tx, "before")?;
        refused = tx
            .execute_batch("COMMIT")
            .as_ref()
            .is_err_and(commit_refused);
        tx.execute_batch("SAVEPOINT tail")?;
        insert_label(tx, "tail")
    });
    assert!(
        matches!(&outcome, Err(DbError::TransactionEnded { operation }) if operation == "brief-raw-commit"),
        "the caller is told the transaction ended, got {outcome:?}"
    );
    assert!(refused, "the closure sees the hook's refusal");
    assert!(labels(&lane).is_empty(), "nothing was committed");

    lane.write_blocking(Lane::Foreground, "brief-probe", |tx| {
        insert_label(tx, "next")
    })
    .unwrap();
    assert_eq!(labels(&lane), ["next"]);
}

/// A closure that writes to its own lane is refused at once, on the
/// caller's thread and on the lane's, because it could only wait behind
/// itself. A write to another database from inside a closure goes through.
#[tokio::test]
async fn nested_write_is_refused_on_the_same_lane_only() {
    let (lane, _dir) = scratch_lane("nested");
    let (other, _other_dir) = scratch_lane("nested-other");
    let nested = lane
        .write_blocking(Lane::Foreground, "brief-outer", |tx| {
            insert_label(tx, "outer")?;
            other
                .write_blocking(Lane::Foreground, "brief-other", |tx| {
                    insert_label(tx, "other")
                })
                .map_err(|error| OpError::Abort(error.to_string()))?;
            Ok(lane.write_blocking(Lane::Foreground, "brief-nested", |tx| {
                insert_label(tx, "nested")
            }))
        })
        .unwrap();
    assert!(
        matches!(&nested, Err(DbError::NestedWrite { operation }) if operation == "brief-nested"),
        "a nested blocking write is refused, got {nested:?}"
    );

    let inner = lane.clone();
    let nested = lane
        .write(Lane::Foreground, "brief-outer-async", move |tx| {
            insert_label(tx, "outer-async")?;
            Ok(
                inner.write_blocking(Lane::Foreground, "brief-nested", |tx| {
                    insert_label(tx, "nested")
                }),
            )
        })
        .await
        .unwrap();
    assert!(
        matches!(nested, Err(DbError::NestedWrite { .. })),
        "a nested write on the lane's thread is refused, got {nested:?}"
    );

    assert_eq!(labels(&lane), ["outer", "outer-async"]);
    assert_eq!(labels(&other), ["other"]);
    lane.shutdown(DRAIN).await;
    other.shutdown(DRAIN).await;
}

/// Synchronous code waits on a future through `db::block_on`. Outside a
/// write closure it runs the future; inside one it refuses without polling
/// it, so no closure holds the lane across a network call.
#[test]
fn block_on_is_refused_inside_a_write_closure() {
    let (lane, _dir) = scratch_lane("block-on");
    let runtime = current_thread_runtime();
    let handle = runtime.handle();
    assert_eq!(block_on(handle, "brief-outside", async { 7 }).unwrap(), 7);

    let mut polled = false;
    let inside = lane
        .write_blocking(Lane::Foreground, "brief-holder", |tx| {
            insert_label(tx, "holder")?;
            Ok(block_on(handle, "brief-inside", async { polled = true }))
        })
        .unwrap();
    assert!(
        matches!(&inside, Err(DbError::WaitInsideWrite { operation }) if operation == "brief-inside"),
        "a wait inside a write closure is refused, got {inside:?}"
    );
    assert!(!polled, "the refused future never ran");
    assert_eq!(block_on(handle, "brief-after", async { 7 }).unwrap(), 7);
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

    let cancel = droppedneedle_kernel::db::CancelFlag::never();
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

    let stop = droppedneedle_kernel::db::CancelFlag::never();
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

/// The blocking chunked write commits whole chunks, stops after the chunk
/// in which its flag tripped, and waits at a chunk boundary while the WAL
/// hold is on.
#[test]
fn blocking_chunked_write_stops_at_chunk_boundaries() {
    let (lane, _dir) = scratch_lane("chunked-blocking");
    let rows: Vec<String> = (0..5).map(|row| format!("row-{row}")).collect();
    let write_rows = |rows: &[String], tx: &rusqlite::Transaction| {
        rows.iter().try_for_each(|row| insert_label(tx, row))
    };

    let committed = lane
        .write_chunked_blocking("brief-chunks", &rows, 2, &CancelFlag::never(), write_rows)
        .unwrap();
    assert_eq!(committed, 5);
    assert_eq!(labels(&lane), rows);

    let cancel = CancelFlag::never();
    let stopped = lane.write_chunked_blocking("brief-cancelled", &rows, 2, &cancel, |rows, tx| {
        cancel.cancel();
        write_rows(rows, tx)
    });
    assert!(
        matches!(stopped, Err(DbError::Cancelled { chunks: 1, .. })),
        "the chunk that was running commits and the write stops, got {stopped:?}"
    );
    assert_eq!(labels(&lane).len(), 7);

    let hold = lane.wal_hold();
    let (chunked, bulk) = (lane.clone(), rows.clone());
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let raised = chunked.wal_hold();
        let outcome = chunked.write_chunked_blocking(
            "brief-held",
            &bulk,
            2,
            &CancelFlag::never(),
            |rows, tx| {
                raised.force(true);
                write_rows(rows, tx)
            },
        );
        let _ = done.send(outcome);
    });
    // The first chunk commits whole, and the second then waits its turn.
    let deadline = Instant::now() + Duration::from_secs(10);
    while labels(&lane).len() < 9 {
        assert!(Instant::now() < deadline, "the first chunk must commit");
        std::thread::yield_now();
    }
    wait_queued(&lane, 1);
    assert_eq!(labels(&lane).len(), 9, "the second chunk has not started");
    assert!(result.try_recv().is_err());

    // A scratch lane has no relief thread. The end of any turn starts the
    // next write, and each chunk that runs raises the hold again.
    for whole in [11, 12] {
        hold.force(false);
        lane.write_blocking(Lane::Foreground, "brief-nudge", |_tx| Ok(()))
            .unwrap();
        if whole < 12 {
            wait_queued(&lane, 1);
            assert_eq!(labels(&lane).len(), whole);
        }
    }
    let committed = result
        .recv_timeout(Duration::from_secs(10))
        .expect("the chunked write finishes once the hold is off");
    assert_eq!(committed.unwrap(), 5);
    assert_eq!(labels(&lane).len(), 12);
}

/// With the WAL hold on, a bulk write waits and every other class goes past
/// it: a durable write queued behind it (also one that names the bulk
/// class), a job write and a request's write. A full backlog of held bulk
/// writes refuses more bulk writes and nothing else. Shutdown lifts the hold
/// and the held writes commit.
#[test]
fn held_bulk_write_lets_every_other_class_past_until_shutdown() {
    let (lane, _dir) = scratch_lane("bulk-held");
    lane.wal_hold().force(true);
    let parked = lane.clone();
    let (bulk_done, bulk) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome =
            parked.write_blocking(Lane::Bulk, "brief-bulk", |tx| insert_label(tx, "bulk"));
        let _ = bulk_done.send(outcome);
    });
    wait_queued(&lane, 1);

    let others = lane.clone();
    within("only a bulk write is held", move || {
        others
            .write_durable(Lane::Background, "brief-durable", |tx| {
                insert_label(tx, "durable")
            })
            .unwrap();
        others
            .write_durable(Lane::Bulk, "brief-durable-bulk", |tx| {
                insert_label(tx, "durable-bulk")
            })
            .unwrap();
        others
            .write_blocking(Lane::Background, "brief-job", |tx| insert_label(tx, "job"))
            .unwrap();
        others
            .write_blocking(Lane::Foreground, "brief-request", |tx| {
                insert_label(tx, "request")
            })
            .unwrap();
    });
    assert_eq!(labels(&lane), ["durable", "durable-bulk", "job", "request"]);
    assert!(bulk.try_recv().is_err(), "the bulk write is still held");
    assert!(
        !lane.idle_state().is_idle(),
        "a held write keeps the WAL reclaim gate shut"
    );

    let mut cx = Context::from_waker(Waker::noop());
    let mut backlog: Vec<_> = (1..BACKGROUND_QUEUE_CAPACITY)
        .map(|_| Box::pin(lane.write(Lane::Bulk, "brief-backlog", |_tx| Ok(()))))
        .collect();
    for write in &mut backlog {
        assert!(write.as_mut().poll(&mut cx).is_pending());
    }
    let mut over = Box::pin(lane.write(Lane::Bulk, "brief-over", |_tx| Ok(())));
    let refused = over.as_mut().poll(&mut cx);
    assert!(
        matches!(refused, Poll::Ready(Err(DbError::Busy { .. }))),
        "one bulk write too many is refused, got {refused:?}"
    );
    let job = lane.clone();
    within(
        "a bulk backlog does not refuse or hold a job write",
        move || {
            job.write_blocking(Lane::Background, "brief-job-late", |tx| {
                insert_label(tx, "job-late")
            })
        },
    )
    .unwrap();

    let closing = lane.clone();
    within("shutdown lifts the hold", move || {
        current_thread_runtime().block_on(closing.shutdown(DRAIN));
    });
    let held = bulk
        .recv_timeout(Duration::from_secs(10))
        .expect("the held bulk write is answered at shutdown");
    assert!(held.is_ok(), "shutdown lets a held write run, got {held:?}");
    for write in &mut backlog {
        assert!(matches!(write.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
    }
    assert_eq!(
        labels(&lane),
        [
            "durable",
            "durable-bulk",
            "job",
            "request",
            "job-late",
            "bulk"
        ]
    );
}

/// A runtime that holds bulk writes at 64 KiB of WAL and checks every
/// 20 ms, so a brief fills the WAL with kilobytes and waits milliseconds.
async fn small_wal_runtime(name: &str) -> (DbRuntime, ScratchDir) {
    let dir = scratch_dir(name);
    let db = dir.join("library.db");
    common::migrated_db(&db);
    let config = DbConfig::new(&db)
        .with_wal_water(64 * 1024, 16 * 1024)
        .with_lane_limits(LaneLimits {
            watch_tick: Duration::from_millis(20),
            ..LaneLimits::default()
        });
    (open_runtime(&config).await.unwrap(), dir)
}

/// Pin the WAL with a reader, write past the high water behind it and let
/// the checkpoint service see it, which puts the hold on. The WAL cannot
/// shrink until the reader that is returned is dropped.
async fn fill_wal_behind_a_reader(runtime: &DbRuntime) -> rusqlite::Connection {
    let lane = runtime.lane();
    lane.write(Lane::Foreground, "brief-tables", |tx| {
        tx.execute_batch(
            "CREATE TABLE scratch_turns (label TEXT); CREATE TABLE scratch_fill (payload TEXT);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let reader = rusqlite::Connection::open_with_flags(
        runtime.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let pinned: i64 = reader
        .query_row("SELECT count(*) FROM scratch_fill", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pinned, 0);
    lane.write(Lane::Background, "brief-fill", |tx| {
        let payload = "x".repeat(1024);
        for _ in 0..256 {
            tx.execute("INSERT INTO scratch_fill (payload) VALUES (?1)", [&payload])?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let service = runtime.checkpoint().clone();
    let full = tokio::task::spawn_blocking(move || service.run_once())
        .await
        .unwrap();
    assert!(
        full.suspended,
        "a WAL past the high water holds bulk writes, got {full:?}"
    );
    reader
}

/// Under the runtime the app opens, with the checkpoint loop on its 30 s
/// cadence: once a pinned reader has let the WAL grow past the high water, a
/// bulk write waits while a job heartbeat and a request's write commit. When
/// the reader lets go the bulk write commits within seconds, long before the
/// loop's next pass.
#[tokio::test]
async fn full_wal_holds_bulk_writes_only_until_it_can_shrink() {
    let (runtime, _dir) = small_wal_runtime("wal-hold").await;
    let lane = runtime.lane().clone();
    let registry = JobRegistry::new(DurableRegistryStore::new(
        runtime.wakeups().clone(),
        lane.clone(),
    ));
    checkpoint::spawn_on(
        &registry,
        runtime.checkpoint().clone(),
        checkpoint::default_schedule(),
    )
    .await
    .unwrap();
    // The loop passes once at start and beats its heart. Its next pass is
    // most of a minute away, so nothing below can be its doing.
    until("the checkpoint loop finishes its first pass", || async {
        runtime
            .wakeups()
            .get_job(checkpoint::JOB_NAME)
            .await
            .unwrap()
            .is_some_and(|job| job.last_heartbeat_at.is_some())
    })
    .await;
    let reader = fill_wal_behind_a_reader(&runtime).await;
    let full = runtime.checkpoint().latest().await.unwrap();

    let mut bulk = Box::pin(lane.write(Lane::Bulk, "brief-bulk", |tx| insert_label(tx, "bulk")));
    std::future::poll_fn(|cx| {
        assert!(bulk.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let limit = Duration::from_secs(10);
    tokio::time::timeout(
        limit,
        runtime.wakeups().heartbeat(&lane, checkpoint::JOB_NAME),
    )
    .await
    .expect("a job heartbeat is not held")
    .unwrap();
    tokio::time::timeout(
        limit,
        lane.write(Lane::Foreground, "brief-request", |tx| {
            insert_label(tx, "request")
        }),
    )
    .await
    .expect("a request's write is not held")
    .unwrap();
    until(
        "a relief pass runs while the reader pins the WAL",
        || async {
            let latest = runtime.checkpoint().latest().await;
            latest.is_some_and(|pass| pass.at > full.at)
        },
    )
    .await;
    assert!(
        runtime.checkpoint().bulk_held(),
        "a pass that cannot shrink the WAL leaves the hold on"
    );
    assert_eq!(labels(&lane), ["request"]);

    drop(reader);
    tokio::time::timeout(Duration::from_secs(5), bulk)
        .await
        .expect("the hold ends once the WAL can shrink, without the loop's next pass")
        .unwrap();
    assert_eq!(labels(&lane), ["request", "bulk"]);
    registry.cancel_all(Duration::from_secs(5)).await;
    runtime.shutdown().await;
}

/// A checkpoint pass that loses the checkpoint lock to another checkpoint
/// learns nothing about the WAL. It leaves the hold on: the reader still
/// pins a WAL that is past the high water.
#[tokio::test]
async fn checkpoint_pass_that_loses_the_lock_leaves_the_hold_on() {
    let (runtime, _dir) = small_wal_runtime("wal-hold-busy").await;
    let reader = fill_wal_behind_a_reader(&runtime).await;

    // A blocking checkpoint that waits for the reader keeps the checkpoint
    // lock for as long as it waits. An attempt that lands while a pass
    // below has the lock is itself answered with a log count of -1 and
    // holds nothing, so it is made again until it gets the lock.
    let db = runtime.path().to_owned();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.busy_timeout(Duration::from_secs(20)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let held_the_lock = loop {
            let log_frames = connection
                .query_row("PRAGMA wal_checkpoint(RESTART)", [], |row| {
                    row.get::<_, i64>(1)
                })
                .unwrap();
            if log_frames >= 0 || Instant::now() >= deadline {
                break log_frames >= 0;
            }
        };
        let _ = done.send(held_the_lock);
    });
    let service = runtime.checkpoint().clone();
    let lost = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let pass = service.run_relief();
            if pass.busy {
                return pass;
            }
            assert!(
                Instant::now() < deadline,
                "a pass must lose the checkpoint lock, got {pass:?}"
            );
            std::thread::yield_now();
        }
    })
    .await
    .unwrap();
    assert_eq!(lost.active_bytes, -1, "nothing was measured: {lost:?}");
    assert!(lost.suspended, "the hold stays on, got {lost:?}");
    assert!(runtime.checkpoint().bulk_held());

    drop(reader);
    let checkpointed = finished
        .recv_timeout(Duration::from_secs(20))
        .expect("the blocking checkpoint ends once the reader is gone");
    assert!(checkpointed, "the blocking checkpoint never got the lock");
    runtime.shutdown().await;
}

/// A chunked write whose chunk is held by the WAL stops within moments of
/// its flag tripping. It does not wait for the hold to end, it writes
/// nothing, and it leaves nothing queued.
#[tokio::test]
async fn cancelled_chunked_write_does_not_wait_out_the_hold() {
    let (runtime, _dir) = small_wal_runtime("wal-hold-cancel").await;
    let reader = fill_wal_behind_a_reader(&runtime).await;
    let lane = runtime.lane().clone();

    let cancel = CancelFlag::never();
    let (chunked, flag) = (lane.clone(), cancel.clone());
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rows = ["one", "two", "three"];
        let outcome = chunked.write_chunked_blocking("brief-held", &rows, 2, &flag, |rows, tx| {
            rows.iter().try_for_each(|row| insert_label(tx, row))
        });
        let _ = done.send(outcome);
    });
    wait_queued(&lane, 1);
    assert!(result.try_recv().is_err(), "the first chunk is held");

    cancel.cancel();
    let stopped = result
        .recv_timeout(Duration::from_secs(5))
        .expect("a cancelled chunk is answered while the hold is still on");
    assert!(
        matches!(stopped, Err(DbError::Cancelled { chunks: 0, .. })),
        "the held chunk never ran, got {stopped:?}"
    );
    assert!(runtime.checkpoint().bulk_held());
    assert_eq!(lane.idle_state().queued(), 0);
    assert!(labels(&lane).is_empty());

    drop(reader);
    runtime.shutdown().await;
}

/// Planner statistics do not run while the WAL hold is on, and the skipped
/// run does not use up the ten-minute check interval.
#[tokio::test]
async fn planner_statistics_wait_out_the_wal_hold() {
    let (runtime, _dir) = open_scratch("analyze-held").await;
    let analyze = runtime.checkpoint().analyze().unwrap().clone();
    let hold = runtime.lane().wal_hold();

    hold.force(true);
    assert!(!analyze.run_if_due().await.unwrap());
    hold.force(false);
    assert!(
        analyze.run_if_due().await.unwrap(),
        "the first check after the hold runs the refresh"
    );
    runtime.shutdown().await;
}

/// The writer connection's `fold()` SQL function folds like v2.
#[tokio::test]
async fn fold_sql_function_matches_v2_casefold() {
    let (runtime, dir) = open_scratch("fold").await;
    let folded: String = runtime
        .lane()
        .write(Lane::Foreground, "brief-fold", |tx| {
            tx.query_row("SELECT fold('Straße à  Athènes')", [], |row| row.get(0))
                .map_err(droppedneedle_kernel::db::OpError::from)
        })
        .await
        .unwrap();
    assert_eq!(folded, "strasse a athenes");
    let null: Option<String> = runtime
        .lane()
        .write(Lane::Foreground, "brief-fold-null", |tx| {
            tx.query_row("SELECT fold(NULL)", [], |row| row.get(0))
                .map_err(droppedneedle_kernel::db::OpError::from)
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
    use droppedneedle_media::playback::ports::{PlayHistory as _, PlayRecord};
    use droppedneedle_media::playback::sqlite::PlaybackDb;

    let (runtime, dir) = open_scratch("play-busy").await;
    let db_path = dir.join("library.db");
    let history =
        PlaybackDb::open(&db_path, Arc::new(droppedneedle_kernel::ids::UuidGenerator)).unwrap();
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

/// A wait for a pooled connection far longer than any brief keeps a reader
/// waiting.
const PATIENT: Duration = Duration::from_secs(60);

/// A runtime with one table to read. `readers` sizes the request pool, a
/// request read hears `Busy` after `read_wait`, and the pools' pass runs
/// every 10 ms.
async fn read_runtime(name: &str, readers: u32, read_wait: Duration) -> (DbRuntime, ScratchDir) {
    read_runtime_with(name, readers, read_wait, DRAIN).await
}

/// [`read_runtime`] with the grace its shutdown gives writes and reads.
async fn read_runtime_with(
    name: &str,
    readers: u32,
    read_wait: Duration,
    grace: Duration,
) -> (DbRuntime, ScratchDir) {
    let dir = scratch_dir(name);
    let db = dir.join("library.db");
    common::migrated_db(&db);
    let mut config = DbConfig::new(&db)
        .with_read_wait(read_wait)
        .with_shutdown_grace(grace)
        .with_lane_limits(LaneLimits {
            watch_tick: Duration::from_millis(10),
            ..LaneLimits::default()
        });
    config.readers = readers;
    let runtime = open_runtime(&config).await.unwrap();
    runtime
        .db()
        .write(Lane::Foreground, "brief-table", |tx| {
            tx.execute("CREATE TABLE scratch_turns (label TEXT)", [])?;
            Ok(())
        })
        .await
        .unwrap();
    (runtime, dir)
}

fn count_labels(connection: &rusqlite::Connection) -> Result<i64, DbError> {
    Ok(connection.query_row("SELECT count(*) FROM scratch_turns", [], |row| row.get(0))?)
}

fn runtime_with_clock() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A blocking read that keeps its pooled connection until it is released.
/// A brief that fails first drops it, which releases too.
struct HeldRead<T> {
    let_go: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<T>,
}

impl<T> HeldRead<T> {
    /// Let the read return, wait until its connection is back and give what
    /// its probe saw.
    fn release(self) -> T {
        drop(self.let_go);
        self.thread.join().unwrap()
    }
}

/// Start `count` reads on the request pool, or the long one, that each run
/// `probe` and then hold their connection. Returns once every one is inside
/// its closure, so they hold `count` connections at once.
fn hold_reads<T: Send + 'static>(
    db: &Db,
    long: bool,
    count: usize,
    probe: fn(&rusqlite::Connection) -> T,
) -> Vec<HeldRead<T>> {
    (0..count)
        .map(|_| {
            let db = db.clone();
            let (entered, inside) = std::sync::mpsc::channel();
            let (let_go, release) = std::sync::mpsc::channel::<()>();
            let thread = std::thread::spawn(move || {
                let read = |connection: &rusqlite::Connection| -> Result<T, DbError> {
                    let seen = probe(connection);
                    let _ = entered.send(());
                    let _ = release.recv();
                    Ok(seen)
                };
                if long {
                    db.read_long("brief-held", &CancelFlag::never(), read)
                } else {
                    db.read_blocking("brief-held", read)
                }
                .unwrap()
            });
            inside
                .recv_timeout(Duration::from_secs(10))
                .expect("a held read must get a connection");
            HeldRead { let_go, thread }
        })
        .collect()
}

/// A write statement through a pooled connection fails, whichever way the
/// connection is reached, and changes nothing.
#[tokio::test]
async fn pooled_reads_refuse_writes() {
    fn write(connection: &rusqlite::Connection) -> Result<usize, DbError> {
        Ok(connection.execute("INSERT INTO scratch_turns (label) VALUES ('nope')", [])?)
    }
    let (runtime, _dir) = read_runtime("read-only", 7, PATIENT).await;
    let db = runtime.db();
    let refused = [
        db.read("brief-read", write).await,
        db.read_blocking("brief-read-blocking", write),
        db.read_long("brief-read-long", &CancelFlag::never(), write),
    ];
    for outcome in refused {
        assert!(
            matches!(outcome, Err(DbError::Rusqlite(_))),
            "SQLite refuses the write, got {outcome:?}"
        );
    }
    assert_eq!(db.read_blocking("brief-count", count_labels).unwrap(), 0);
    runtime.shutdown().await;
}

/// Once `write` has returned, a read on any connection of either pool sees
/// the row, although every one of them has read the table before it.
#[tokio::test]
async fn every_pooled_connection_sees_a_returned_write() {
    let (runtime, _dir) = read_runtime("read-your-writes", 7, PATIENT).await;
    let db = runtime.db();
    // Held all at once, so each connection of the pool serves one.
    let on_every_connection = |long, connections| -> Vec<i64> {
        let held = hold_reads(db, long, connections, |connection| {
            count_labels(connection).unwrap()
        });
        assert_eq!(runtime.read_activity().out(), connections);
        held.into_iter().map(HeldRead::release).collect()
    };
    let pools = [(false, 7), (true, LONG_READERS)];
    for (long, connections) in pools {
        assert_eq!(on_every_connection(long, connections), vec![0; connections]);
    }
    db.write(Lane::Foreground, "brief-row", |tx| insert_label(tx, "row"))
        .await
        .unwrap();
    for (long, connections) in pools {
        assert_eq!(on_every_connection(long, connections), vec![1; connections]);
    }
    runtime.shutdown().await;
}

/// The statements of one read closure see one state, although a write
/// commits between them. A read started afterwards sees the new row.
#[tokio::test]
async fn read_closure_sees_one_state_across_a_write() {
    let (runtime, _dir) = read_runtime("read-snapshot", 7, PATIENT).await;
    let db = runtime.db();
    let (held, inside, let_go) = hold();
    let reader = db.clone();
    let reading = tokio::spawn(async move {
        reader
            .read(
                "brief-two-statements",
                move |connection| -> Result<(i64, i64), DbError> {
                    let before = count_labels(connection)?;
                    held.wait();
                    Ok((before, count_labels(connection)?))
                },
            )
            .await
    });
    inside.await.unwrap();
    db.write(Lane::Foreground, "brief-between", |tx| {
        insert_label(tx, "between")
    })
    .await
    .unwrap();
    let_go.send(()).unwrap();
    let seen = tokio::time::timeout(Duration::from_secs(10), reading)
        .await
        .expect("the read returns once it is let go")
        .unwrap()
        .unwrap();
    assert_eq!(seen, (0, 0), "both statements see the state of the first");
    assert_eq!(db.read("brief-after", count_labels).await.unwrap(), 1);
    runtime.shutdown().await;
}

/// With every request connection out, reads queue in the order they came,
/// whether they await or park a thread, and each connection that comes back
/// goes to the one in front.
#[tokio::test]
async fn waiting_reads_are_served_in_arrival_order() {
    let (runtime, _dir) = read_runtime("read-order", 7, PATIENT).await;
    let db = runtime.db();
    let activity = runtime.read_activity();
    let mut held = hold_reads(db, false, 7, |_| ());

    let order = Arc::new(Mutex::new(Vec::new()));
    // Each queued read reports in and then keeps its connection, so the
    // next one in line runs only when the brief frees another.
    let mut let_gos = Vec::new();
    let mut queued = |label: &'static str| {
        let (let_go, release) = std::sync::mpsc::channel::<()>();
        let_gos.push(let_go);
        let order = Arc::clone(&order);
        move |_: &rusqlite::Connection| -> Result<(), DbError> {
            order.lock().unwrap().push(label);
            let _ = release.recv();
            Ok(())
        }
    };
    let (reader, op) = (db.clone(), queued("async-1"));
    let first = tokio::spawn(async move { reader.read("brief-async-1", op).await });
    until("the first read waits", || async { activity.waiting() == 1 }).await;
    let (reader, op) = (db.clone(), queued("blocking"));
    let second = std::thread::spawn(move || reader.read_blocking("brief-blocking", op));
    until("the second read waits", || async {
        activity.waiting() == 2
    })
    .await;
    let (reader, op) = (db.clone(), queued("async-2"));
    let third = tokio::spawn(async move { reader.read("brief-async-2", op).await });
    until("the third read waits", || async { activity.waiting() == 3 }).await;

    let arrival = ["async-1", "blocking", "async-2"];
    for served in 1..=arrival.len() {
        held.pop().unwrap().release();
        until("the read in front runs", || async {
            order.lock().unwrap().len() == served
        })
        .await;
        assert_eq!(*order.lock().unwrap(), arrival[..served]);
        assert_eq!(activity.waiting(), arrival.len() - served);
    }

    drop(let_gos);
    first.await.unwrap().unwrap();
    second.join().unwrap().unwrap();
    third.await.unwrap().unwrap();
    held.into_iter().for_each(HeldRead::release);
    runtime.shutdown().await;
}

/// With every request connection out, a read that has waited to the limit
/// hears `Busy`, whether its caller is parked on a thread or awaiting on a
/// runtime whose clock is paused. Waiting gave neither of them a connection.
#[test]
fn waiting_read_past_its_limit_hears_busy() {
    let opener = runtime_with_clock();
    let limit = Duration::from_millis(50);
    let (runtime, _dir) = opener.block_on(read_runtime("read-busy", 7, limit));
    let db = runtime.db();
    let held = hold_reads(db, false, 7, |_| ());

    let parked = db.clone();
    let refused = within("a parked read past its limit must be answered", move || {
        parked.read_blocking("brief-parked", count_labels)
    });
    assert!(
        matches!(&refused, Err(DbError::Busy { operation }) if operation == "brief-parked"),
        "a parked reader hears Busy, got {refused:?}"
    );
    let awaiting = db.clone();
    let (refused, waited) = within(
        "an awaiting read past its limit must be answered",
        move || {
            let paused = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .start_paused(true)
                .build()
                .unwrap();
            let asked = Instant::now();
            let refused = paused.block_on(awaiting.read("brief-awaiting", count_labels));
            (refused, asked.elapsed())
        },
    );
    assert!(
        matches!(&refused, Err(DbError::Busy { operation }) if operation == "brief-awaiting"),
        "an awaiting reader hears Busy, got {refused:?}"
    );
    // A timer on the paused clock would have fired at once. The answer came
    // from the pools' pass, which counts real time.
    assert!(
        waited >= limit,
        "the limit is counted on real time, answered after {waited:?}"
    );
    let activity = runtime.read_activity();
    assert_eq!((activity.out(), activity.waiting()), (7, 0));

    held.into_iter().for_each(HeldRead::release);
    opener.block_on(runtime.shutdown());
}

/// Long reads have their own connections: with both out, a request read is
/// served at once, and a third long read waits until its flag trips.
#[tokio::test]
async fn full_long_pool_delays_no_request_and_stops_at_its_flag() {
    let (runtime, _dir) = read_runtime("read-long", 7, PATIENT).await;
    let db = runtime.db();
    let activity = runtime.read_activity();
    let held = hold_reads(db, true, LONG_READERS, |_| ());

    let count = tokio::time::timeout(
        Duration::from_secs(10),
        db.read("brief-request", count_labels),
    )
    .await
    .expect("a request read does not wait for the long pool")
    .unwrap();
    assert_eq!(count, 0);

    let cancel = CancelFlag::never();
    let (flag, reader) = (cancel.clone(), db.clone());
    let third = std::thread::spawn(move || reader.read_long("brief-third", &flag, count_labels));
    until("the third long read waits", || async {
        activity.waiting() == 1
    })
    .await;
    cancel.cancel();
    let stopped = within("a cancelled long read must return", move || {
        third.join().unwrap()
    });
    assert!(
        matches!(&stopped, Err(DbError::ReadCancelled { operation }) if operation == "brief-third"),
        "a long read stops at its flag, got {stopped:?}"
    );
    assert_eq!(activity.waiting(), 0);

    held.into_iter().for_each(HeldRead::release);
    runtime.shutdown().await;
}

/// A closure may not reach back into the database it is running on. A
/// pooled read inside a write closure is refused, and so are a write,
/// another pooled read and `db::block_on` inside a read closure. Each is
/// refused at once: the request pool here has one connection, which the
/// outer read holds.
#[test]
fn closures_may_not_reach_back_into_the_database() {
    let opener = runtime_with_clock();
    let (runtime, _dir) = opener.block_on(read_runtime("read-nesting", 1, Duration::from_secs(1)));
    let db = runtime.db();
    let handle = opener.handle();
    let never = CancelFlag::never();

    let in_write = db
        .write_blocking(Lane::Foreground, "brief-write", |tx| {
            insert_label(tx, "row")?;
            Ok([
                db.read_blocking("brief-read", count_labels),
                db.read_long("brief-long", &never, count_labels),
            ])
        })
        .unwrap();
    for refused in in_write {
        assert!(
            matches!(refused, Err(DbError::ReadInsideWrite { .. })),
            "a pooled read inside a write closure is refused, got {refused:?}"
        );
    }

    let (write, reads, wait) = db
        .read_blocking("brief-outer", |_| -> Result<_, DbError> {
            Ok((
                db.write_blocking(Lane::Foreground, "brief-nested-write", |tx| {
                    insert_label(tx, "nested")
                }),
                [
                    db.read_blocking("brief-nested-read", count_labels),
                    db.read_long("brief-nested-long", &never, count_labels),
                ],
                block_on(handle, "brief-nested-wait", async {}),
            ))
        })
        .unwrap();
    assert!(
        matches!(&write, Err(DbError::InsideRead { operation }) if operation == "brief-nested-write"),
        "a write inside a read closure is refused, got {write:?}"
    );
    for refused in reads {
        assert!(
            matches!(refused, Err(DbError::InsideRead { .. })),
            "a pooled read inside a read closure is refused, got {refused:?}"
        );
    }
    assert!(
        matches!(&wait, Err(DbError::InsideRead { operation }) if operation == "brief-nested-wait"),
        "a wait inside a read closure is refused, got {wait:?}"
    );
    assert_eq!(db.read_blocking("brief-count", count_labels).unwrap(), 1);
    opener.block_on(runtime.shutdown());
}

/// A read whose closure panics gives its connection back, and one whose
/// closure leaves a transaction of its own open does not hand it on:
/// afterwards every connection of the pool sees the current state.
#[tokio::test]
async fn failed_read_closure_leaves_its_connection_clean() {
    let (runtime, _dir) = read_runtime("read-panic", 7, PATIENT).await;
    let db = runtime.db();
    let panicked = db
        .read("brief-panic", |connection| -> Result<i64, DbError> {
            count_labels(connection)?;
            panic!("brief: an async read closure panics")
        })
        .await;
    assert!(
        matches!(&panicked, Err(DbError::ReadPanicked { operation }) if operation == "brief-panic"),
        "an async caller gets a typed error, got {panicked:?}"
    );
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        db.read_blocking(
            "brief-panic-blocking",
            |connection| -> Result<i64, DbError> {
                count_labels(connection)?;
                panic!("brief: a blocking read closure panics")
            },
        )
    }));
    assert!(unwound.is_err(), "a blocking caller's panic is its own");
    db.read_blocking(
        "brief-own-transaction",
        |connection| -> Result<i64, DbError> {
            connection.execute_batch("COMMIT; BEGIN")?;
            count_labels(connection)
        },
    )
    .unwrap();
    assert_eq!(runtime.read_activity().out(), 0);

    db.write(Lane::Foreground, "brief-row", |tx| insert_label(tx, "row"))
        .await
        .unwrap();
    let held = hold_reads(db, false, 7, |connection| count_labels(connection).unwrap());
    let seen: Vec<i64> = held.into_iter().map(HeldRead::release).collect();
    assert_eq!(seen, vec![1; 7]);
    runtime.shutdown().await;
}

/// A runtime dropped with one read running and two queued shuts down, and
/// the pool then serves a reader on another thread.
#[test]
fn read_pool_outlives_a_runtime_dropped_with_reads_in_it() {
    let opener = runtime_with_clock();
    let (runtime, _dir) = opener.block_on(read_runtime("read-runtime-dropped", 1, PATIENT));
    let db = runtime.db();
    let activity = runtime.read_activity();

    let first = runtime_with_clock();
    let let_go = first.block_on(async {
        let (held, inside, let_go) = hold();
        let running = db.clone();
        tokio::spawn(async move {
            let _ = running
                .read("brief-running", move |connection| {
                    held.wait();
                    count_labels(connection)
                })
                .await;
        });
        inside.await.unwrap();
        for _ in 0..2 {
            let queued = db.clone();
            tokio::spawn(async move {
                let _ = queued.read("brief-queued", count_labels).await;
            });
        }
        until("two reads wait behind the running one", || async {
            activity.waiting() == 2
        })
        .await;
        let_go
    });

    let (down_tx, down) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(first);
        let _ = down_tx.send(());
    });
    // A runtime waits for a blocking closure it has started, so the running
    // read is let go. The two queued ones go down with the runtime.
    let_go.send(()).unwrap();
    down.recv_timeout(Duration::from_secs(10))
        .expect("the runtime must shut down with reads in the pool");

    let reader = db.clone();
    let seen = within("a blocking read must get a connection", move || {
        reader.read_blocking("brief-after", count_labels)
    });
    assert_eq!(seen.unwrap(), 0);
    assert_eq!((activity.out(), activity.waiting()), (0, 0));
    opener.block_on(runtime.shutdown());
}

/// The `TRUNCATE` reclaim holds back while a pooled read is open, on either
/// pool, and runs once the reads have ended.
#[tokio::test]
async fn truncate_gate_waits_for_pooled_reads() {
    let (runtime, _dir) = read_runtime("read-gate", 7, PATIENT).await;
    let db = runtime.db();
    db.write(Lane::Foreground, "brief-row", |tx| insert_label(tx, "row"))
        .await
        .unwrap();
    let passive = runtime.checkpoint().run_once();
    assert_eq!(passive.active_bytes, 0);

    let probe = |connection: &rusqlite::Connection| count_labels(connection).unwrap();
    let request = hold_reads(db, false, 1, probe).remove(0);
    let long = hold_reads(db, true, 1, probe).remove(0);
    let gate = || runtime.checkpoint().maybe_truncate(Instant::now());
    assert!(gate().is_none(), "an open request read holds the reclaim");
    request.release();
    assert!(gate().is_none(), "an open long read holds the reclaim");
    long.release();
    let reclaimed = gate().expect("the reclaim runs once the reads have ended");
    assert_eq!(reclaimed.mode, CheckpointMode::Truncate);
    assert_eq!(file_len(&sidecar(runtime.path(), "-wal")), Some(0));
    runtime.shutdown().await;
}

/// A clean shutdown after reads on both pools leaves no `-wal` or `-shm`,
/// and a handle that outlives it is refused.
#[tokio::test]
async fn shutdown_after_pooled_reads_leaves_no_wal() {
    let (runtime, _dir) = read_runtime("read-shutdown", 7, PATIENT).await;
    let path = runtime.path().to_owned();
    let db = runtime.db().clone();
    db.write(Lane::Foreground, "brief-row", |tx| insert_label(tx, "row"))
        .await
        .unwrap();
    assert_eq!(db.read("brief-read", count_labels).await.unwrap(), 1);
    assert_eq!(db.read_blocking("brief-blocking", count_labels).unwrap(), 1);
    let long = db.read_long("brief-long", &CancelFlag::never(), count_labels);
    assert_eq!(long.unwrap(), 1);

    runtime.shutdown().await;
    assert_eq!(file_len(&sidecar(&path, "-wal")), None);
    assert_eq!(file_len(&sidecar(&path, "-shm")), None);
    let late = db.read_blocking("brief-late", count_labels);
    assert!(
        matches!(late, Err(DbError::LaneClosed)),
        "a read after shutdown is refused, got {late:?}"
    );
}

/// A closure that leaks a cursor it has stepped leaves a read transaction
/// that the pool's own commit cannot end. That connection is replaced, not
/// handed on: afterwards every connection of the pool sees a later write.
#[tokio::test]
async fn leaked_cursor_does_not_pin_its_connection() {
    let (runtime, _dir) = read_runtime("read-leaked-cursor", 7, PATIENT).await;
    let db = runtime.db();
    db.write(Lane::Foreground, "brief-first", |tx| {
        insert_label(tx, "first")
    })
    .await
    .unwrap();
    db.read_blocking("brief-leak", |connection| -> Result<(), DbError> {
        let mut statement = connection.prepare_cached("SELECT label FROM scratch_turns")?;
        let mut rows = statement.query([])?;
        rows.next()?;
        std::mem::forget(rows);
        Ok(())
    })
    .unwrap();
    assert_eq!(runtime.read_activity().out(), 0);

    db.write(Lane::Foreground, "brief-second", |tx| {
        insert_label(tx, "second")
    })
    .await
    .unwrap();
    let held = hold_reads(db, false, 7, |connection| count_labels(connection).unwrap());
    let seen: Vec<i64> = held.into_iter().map(HeldRead::release).collect();
    assert_eq!(seen, vec![2; 7]);
    assert_eq!(runtime.read_activity().out(), 0);
    runtime.shutdown().await;
}

/// A waiting read whose future is dropped loses its place: the connection
/// that comes back goes to the read behind it and is not lost.
#[tokio::test]
async fn dropped_waiting_read_gives_its_place_to_the_next() {
    let (runtime, _dir) = read_runtime("read-dropped-waiter", 1, PATIENT).await;
    let db = runtime.db();
    let activity = runtime.read_activity();
    let mut held = hold_reads(db, false, 1, |_| ());

    let mut gone = Box::pin(db.read("brief-gone", count_labels));
    let pending = std::future::poll_fn(|cx| Poll::Ready(gone.as_mut().poll(cx).is_pending())).await;
    assert!(pending, "a read behind a held connection cannot finish");
    let reader = db.clone();
    let behind = tokio::spawn(async move { reader.read("brief-behind", count_labels).await });
    until("two reads wait", || async { activity.waiting() == 2 }).await;

    drop(gone);
    held.pop().unwrap().release();
    let seen = tokio::time::timeout(Duration::from_secs(10), behind)
        .await
        .expect("the read behind the dropped one is served")
        .unwrap()
        .unwrap();
    assert_eq!(seen, 0);
    assert_eq!((activity.out(), activity.waiting()), (0, 0));
    runtime.shutdown().await;
}

/// Shutdown waits for a read that is still running, returns once it has
/// ended without waiting out the grace, and leaves no `-wal` or `-shm`.
#[tokio::test]
async fn shutdown_waits_for_a_read_in_flight() {
    let (runtime, _dir) = read_runtime_with("read-in-flight", 7, PATIENT, PATIENT).await;
    let path = runtime.path().to_owned();
    let db = runtime.db().clone();
    db.write(Lane::Foreground, "brief-row", |tx| insert_label(tx, "row"))
        .await
        .unwrap();
    let mut held = hold_reads(&db, false, 1, |connection| {
        count_labels(connection).unwrap()
    });

    let asked = Instant::now();
    let shutting = tokio::spawn(runtime.shutdown());
    until("shutdown closes the read pools", || async {
        matches!(
            db.read_blocking("brief-late", count_labels),
            Err(DbError::LaneClosed)
        )
    })
    .await;
    assert!(!shutting.is_finished(), "shutdown waits for the open read");

    assert_eq!(held.pop().unwrap().release(), 1);
    tokio::time::timeout(Duration::from_secs(10), shutting)
        .await
        .expect("shutdown returns once the read has ended")
        .unwrap();
    assert!(asked.elapsed() < PATIENT, "the grace was not waited out");
    assert_eq!(file_len(&sidecar(&path, "-wal")), None);
    assert_eq!(file_len(&sidecar(&path, "-shm")), None);
}

/// Shutdown gives up on a read that is still running when the grace ends.
/// Its connection is closed by its own thread later, and the next start
/// finds what was committed.
#[tokio::test]
async fn shutdown_gives_up_on_a_read_that_does_not_return() {
    let grace = Duration::from_millis(200);
    let (runtime, _dir) = read_runtime_with("read-stuck", 7, PATIENT, grace).await;
    let path = runtime.path().to_owned();
    let db = runtime.db().clone();
    let activity = runtime.read_activity().clone();
    db.write(Lane::Foreground, "brief-row", |tx| insert_label(tx, "row"))
        .await
        .unwrap();
    let mut held = hold_reads(&db, false, 1, |connection| {
        count_labels(connection).unwrap()
    });

    let asked = Instant::now();
    tokio::time::timeout(Duration::from_secs(10), runtime.shutdown())
        .await
        .expect("shutdown returns at the grace with a read still out");
    assert!(
        asked.elapsed() >= grace,
        "the read had the whole grace to end"
    );
    assert_eq!(activity.out(), 1);

    assert_eq!(held.pop().unwrap().release(), 1);
    assert_eq!(activity.out(), 0);
    let next = open_runtime(&DbConfig::new(&path)).await.unwrap();
    assert_eq!(next.db().read("brief-next", count_labels).await.unwrap(), 1);
    next.shutdown().await;
    assert_eq!(file_len(&sidecar(&path, "-wal")), None);
}
