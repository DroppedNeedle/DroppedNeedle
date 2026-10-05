//! The dedicated writer lane: two queues, one transaction at a time.
//!
//! SQLite serializes writers regardless, so this module makes the
//! serialization explicit and fair. Request-path writes arrive on the
//! foreground lane, job writes on the background lane; while both lanes are
//! busy the scheduler grants eight foreground admissions, then one
//! background admission, then repeats. That is the v2
//! `PriorityWriteLock(foreground_burst=8)` rule, ported to an async
//! scheduler; the decision itself is the pure [`decide_lane`] function so
//! the fairness test pins it without threads.
//!
//! One admission runs one transaction on the single writer connection, which
//! lives on its own thread. The admitted closure is synchronous
//! (`FnOnce`, never `async`), so awaiting inside a write transaction is
//! unrepresentable: there is no async API on the write path to await with.
//! Foreground transactions carry a 250 ms soft budget (logged) and a 2 s
//! hard abort through the SQLite progress handler; background work commits
//! in chunks of at most 500 rows with a cancellation check between chunks.

use std::{
    any::Any,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, Transaction, TransactionBehavior};
use tokio::sync::{mpsc, oneshot};

use super::{
    error::{DbError, rusqlite_is_busy},
    fold::register_fold,
};

/// Foreground admissions granted before one background admission while both
/// lanes are busy. The v2 `foreground_burst=8` value, unchanged.
pub const FOREGROUND_BURST: u32 = 8;
/// Maximum queued admissions per lane. Past this, `write` fails fast with
/// `Busy` (HTTP 503 with `Retry-After` at the handler layer) instead of
/// growing memory without bound.
pub const LANE_QUEUE_CAPACITY: usize = 128;
/// Foreground transactions slower than this log a warning with their name.
pub const FOREGROUND_SOFT_BUDGET: Duration = Duration::from_millis(250);
/// Hard abort for any single write transaction, enforced by the SQLite
/// progress handler. Four times the busy horizon, so a stuck writer fails
/// before it can wedge every waiter twice over.
pub const WRITE_HARD_BUDGET: Duration = Duration::from_secs(2);
/// Progress-handler granularity: the abort check runs every 10k VM ops.
const PROGRESS_OPS: std::os::raw::c_int = 10_000;
/// Largest chunk a background write commits at once.
pub const BACKGROUND_CHUNK_ROWS: usize = 500;
/// Disarmed abort clock: no deadline within a Louvre lifetime.
const DISARMED_MS: u64 = u64::MAX;

/// Which queue a write admission waits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Request path. Never gated by backpressure, never starved by jobs.
    Foreground,
    /// Background jobs. Yields to foreground bursts and to WAL backpressure.
    Background,
}

/// Failure from inside a write closure. The transaction rolls back either
/// way; the two cases differ only in what the caller learns.
#[derive(Debug)]
pub enum OpError {
    /// The SQLite call failed.
    Sql(rusqlite::Error),
    /// The closure refused the write for its own reason.
    Abort(String),
}

impl From<rusqlite::Error> for OpError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

/// Pure fairness step, mirroring `PriorityWriteLock.acquire`.
///
/// `bg_fresh` is true when the background lane went from empty to busy since
/// the last decision; like v2, a newly arrived background waiter resets the
/// burst counter instead of cutting the line. Returns the lane to admit, if
/// any, and the updated burst counter.
pub fn decide_lane(
    fg_pending: bool,
    bg_pending: bool,
    grants: u32,
    bg_fresh: bool,
) -> (Option<Lane>, u32) {
    let grants = if bg_fresh { 0 } else { grants };
    if fg_pending && (!bg_pending || grants < FOREGROUND_BURST) {
        (Some(Lane::Foreground), grants.saturating_add(1))
    } else if bg_pending && (!fg_pending || grants >= FOREGROUND_BURST) {
        (Some(Lane::Background), 0)
    } else {
        (None, grants)
    }
}

/// Shared liveness counters the checkpoint gate reads without touching the
/// scheduler.
#[derive(Debug, Default)]
pub struct LaneIdle {
    queued: AtomicUsize,
    active: AtomicBool,
}

impl LaneIdle {
    /// True when no admission is queued or running.
    pub fn is_idle(&self) -> bool {
        self.queued.load(Ordering::Acquire) == 0 && !self.active.load(Ordering::Acquire)
    }
}

type BoxedValue = Box<dyn Any + Send>;
type BoxedOp = Box<dyn FnOnce(&Transaction) -> Result<BoxedValue, OpError> + Send>;

struct QueuedOp {
    name: &'static str,
    op: BoxedOp,
    reply: oneshot::Sender<LaneOutcome>,
}

struct LaneOutcome {
    value: Result<BoxedValue, OpError>,
    changes: u64,
    elapsed: Duration,
}

/// Cooperative stop flag for chunked background writes and backups.
/// Cloneable so a flag can cross into blocking tasks; clones share one trip.
#[derive(Debug, Default, Clone)]
pub struct CancelFlag {
    stop: Arc<AtomicBool>,
}

impl CancelFlag {
    /// A flag that never trips, for tests and one-shot jobs.
    pub fn never() -> Self {
        Self::default()
    }

    /// Trip the flag; the running chunk commits, then the write stops.
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// The single writer. A cloneable handle over shared state: the scheduler
/// task and the writer thread are owned jointly, and every write in the
/// process funnels through them. Clones are cheap; stages share one lane
/// through `AppState`.
#[derive(Clone, Debug)]
pub struct WriteLane {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    db_path: PathBuf,
    foreground: std::sync::Mutex<Option<mpsc::Sender<QueuedOp>>>,
    background: std::sync::Mutex<Option<mpsc::Sender<QueuedOp>>>,
    idle: Arc<LaneIdle>,
    scheduler: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl WriteLane {
    /// Open the writer on an existing database file. Spawns the scheduler on
    /// the current runtime and the writer thread behind it; returns once the
    /// writer connection is open with its pragmas set. Call from async code.
    pub fn open(db_path: &Path) -> Result<Self, DbError> {
        let (foreground_tx, foreground_rx) = mpsc::channel(LANE_QUEUE_CAPACITY);
        let (background_tx, background_rx) = mpsc::channel(LANE_QUEUE_CAPACITY);
        let (exec_tx, exec_rx) = std::sync::mpsc::channel::<QueuedOp>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), DbError>>();
        let idle = Arc::new(LaneIdle::default());
        let worker_idle = Arc::clone(&idle);

        let path = db_path.to_owned();
        let thread = std::thread::Builder::new()
            .name("droppedneedle-writer".to_owned())
            .spawn(move || writer_loop(&path, exec_rx, ready_tx))
            .map_err(DbError::Io)?;

        let scheduler = tokio::spawn(scheduler_loop(
            foreground_rx,
            background_rx,
            exec_tx,
            worker_idle,
        ));
        ready_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| DbError::LaneClosed)??;

        Ok(Self {
            shared: Arc::new(Shared {
                db_path: db_path.to_owned(),
                foreground: std::sync::Mutex::new(Some(foreground_tx)),
                background: std::sync::Mutex::new(Some(background_tx)),
                idle,
                scheduler: std::sync::Mutex::new(Some(scheduler)),
                thread: std::sync::Mutex::new(Some(thread)),
            }),
        })
    }

    /// Database file this lane writes to.
    pub fn db_path(&self) -> &Path {
        &self.shared.db_path
    }

    /// Shared idle counters for the checkpoint TRUNCATE gate.
    pub fn idle_state(&self) -> Arc<LaneIdle> {
        Arc::clone(&self.shared.idle)
    }

    /// Run one synchronous closure inside one `IMMEDIATE` transaction.
    ///
    /// The closure receives the transaction and returns a value; commit
    /// follows a returned `Ok`, rollback follows `Err` or an abort. The
    /// closure is plain `FnOnce`: holding the lock across an await is
    /// impossible because there is nothing to await with. A full lane fails
    /// fast with `Busy` rather than queueing without bound.
    pub async fn write<F, R>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, DbError>
    where
        F: FnOnce(&Transaction) -> Result<R, OpError> + Send + 'static,
        R: Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let boxed: BoxedOp = Box::new(move |tx| op(tx).map(|value| Box::new(value) as BoxedValue));
        let queued = QueuedOp {
            name,
            op: boxed,
            reply: reply_tx,
        };
        self.shared.idle.queued.fetch_add(1, Ordering::AcqRel);
        let sender = match lane {
            Lane::Foreground => self
                .shared
                .foreground
                .lock()
                .ok()
                .and_then(|guard| guard.clone()),
            Lane::Background => self
                .shared
                .background
                .lock()
                .ok()
                .and_then(|guard| guard.clone()),
        };
        match sender {
            Some(sender) => match sender.try_send(queued) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.shared.idle.queued.fetch_sub(1, Ordering::AcqRel);
                    return Err(DbError::Busy {
                        operation: name.to_owned(),
                    });
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.shared.idle.queued.fetch_sub(1, Ordering::AcqRel);
                    return Err(DbError::LaneClosed);
                }
            },
            None => {
                self.shared.idle.queued.fetch_sub(1, Ordering::AcqRel);
                return Err(DbError::LaneClosed);
            }
        }
        let outcome = reply_rx.await.map_err(|_| DbError::LaneClosed)?;
        if lane == Lane::Foreground && outcome.elapsed > FOREGROUND_SOFT_BUDGET {
            tracing::warn!(
                operation = name,
                elapsed_ms = outcome.elapsed.as_millis() as u64,
                changes = outcome.changes,
                "foreground write exceeded its soft budget"
            );
        }
        match outcome.value {
            Ok(value) => match value.downcast::<R>() {
                Ok(typed) => Ok(*typed),
                Err(_) => Err(DbError::WriteFailed {
                    operation: name.to_owned(),
                    cause: "result type mismatch".to_owned(),
                }),
            },
            Err(OpError::Abort(cause)) => Err(DbError::WriteFailed {
                operation: name.to_owned(),
                cause,
            }),
            Err(OpError::Sql(error)) => Err(map_write_error(name, outcome.changes, error)),
        }
    }

    /// Commit `items` in chunks of at most 500 rows with a cancellation
    /// check between chunks. Each chunk is one background admission; returns
    /// the rows committed. A tripped flag stops after the current chunk.
    pub async fn write_chunked<T, F>(
        &self,
        name: &'static str,
        mut items: Vec<T>,
        chunk_rows: usize,
        cancel: &CancelFlag,
        chunk: F,
    ) -> Result<usize, DbError>
    where
        T: Send + 'static,
        F: Fn(&[T], &Transaction) -> Result<(), OpError> + Send + Sync + 'static,
    {
        let chunk = Arc::new(chunk);
        let width = chunk_rows.clamp(1, BACKGROUND_CHUNK_ROWS);
        let mut committed = 0;
        let mut chunks = 0;
        while !items.is_empty() {
            if cancel.is_cancelled() {
                return Err(DbError::Cancelled {
                    operation: name.to_owned(),
                    chunks,
                });
            }
            let take = width.min(items.len());
            let owned: Vec<T> = items.drain(..take).collect();
            let total = owned.len();
            let writer = Arc::clone(&chunk);
            let started = Instant::now();
            self.write(Lane::Background, name, move |tx| {
                writer(&owned, tx)?;
                Ok(())
            })
            .await?;
            let elapsed = started.elapsed();
            if elapsed > Duration::from_secs(1) {
                tracing::warn!(
                    operation = name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    rows = total,
                    "background chunk exceeded its one-second budget"
                );
            }
            committed += total;
            chunks += 1;
        }
        Ok(committed)
    }

    /// Drain and stop: close both lanes, let queued work finish, then
    /// join the scheduler and the writer thread. Idempotent: the first call
    /// wins, later calls find nothing to join.
    pub async fn shutdown(&self) {
        if let Ok(mut guard) = self.shared.foreground.lock() {
            drop(guard.take());
        }
        if let Ok(mut guard) = self.shared.background.lock() {
            drop(guard.take());
        }
        let scheduler = self
            .shared
            .scheduler
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(scheduler) = scheduler {
            let _ = scheduler.await;
        }
        let thread = self
            .shared
            .thread
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(thread) = thread {
            let joined = tokio::task::spawn_blocking(move || thread.join()).await;
            if !matches!(joined, Ok(Ok(()))) {
                tracing::error!("writer thread did not stop cleanly");
            }
        }
    }
}

/// Pick admissions in fairness order and run each against the writer thread.
async fn scheduler_loop(
    mut foreground_rx: mpsc::Receiver<QueuedOp>,
    mut background_rx: mpsc::Receiver<QueuedOp>,
    exec_tx: std::sync::mpsc::Sender<QueuedOp>,
    idle: Arc<LaneIdle>,
) {
    let mut grants: u32 = 0;
    let mut bg_was_pending = false;
    loop {
        let fg_pending = !foreground_rx.is_empty();
        let bg_pending = !background_rx.is_empty();
        let (choice, next_grants) = decide_lane(
            fg_pending,
            bg_pending,
            grants,
            bg_pending && !bg_was_pending,
        );
        bg_was_pending = bg_pending;
        match choice {
            Some(Lane::Foreground) => {
                grants = next_grants;
                match foreground_rx.recv().await {
                    Some(op) => dispatch(&exec_tx, &idle, op).await,
                    None => {
                        if background_rx.is_closed() {
                            break;
                        }
                    }
                }
            }
            Some(Lane::Background) => {
                grants = next_grants;
                match background_rx.recv().await {
                    Some(op) => dispatch(&exec_tx, &idle, op).await,
                    None => {
                        if foreground_rx.is_closed() {
                            break;
                        }
                    }
                }
            }
            None => {
                if foreground_rx.is_closed() && background_rx.is_closed() {
                    break;
                }
                tokio::select! {
                    biased;
                    arrived = foreground_rx.recv() => match arrived {
                        Some(op) => {
                            grants = grants.saturating_add(1);
                            dispatch(&exec_tx, &idle, op).await;
                        }
                        None => {
                            if background_rx.is_closed() {
                                break;
                            }
                        }
                    },
                    arrived = background_rx.recv() => match arrived {
                        Some(op) => {
                            grants = 0;
                            dispatch(&exec_tx, &idle, op).await;
                        }
                        None => {
                            if foreground_rx.is_closed() {
                                break;
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Run one admission on the writer thread and forward its outcome, tracking
/// liveness for the checkpoint gate. If the thread is gone the caller learns
/// `LaneClosed` instead of hanging.
async fn dispatch(
    exec_tx: &std::sync::mpsc::Sender<QueuedOp>,
    idle: &Arc<LaneIdle>,
    mut op: QueuedOp,
) {
    idle.queued.fetch_sub(1, Ordering::AcqRel);
    idle.active.store(true, Ordering::Release);
    tracing::debug!(operation = op.name, "writer admission dispatched");
    let caller = std::mem::replace(&mut op.reply, oneshot::channel().0);
    let (watch_tx, watch_rx) = oneshot::channel();
    op.reply = watch_tx;
    if exec_tx.send(op).is_err() {
        drop(caller);
        idle.active.store(false, Ordering::Release);
        return;
    }
    match watch_rx.await {
        Ok(outcome) => {
            let _ = caller.send(outcome);
        }
        Err(_) => {
            drop(caller);
        }
    }
    idle.active.store(false, Ordering::Release);
}

/// Writer thread: open the single write connection, arm the abort clock, and
/// run one `IMMEDIATE` transaction per admission until the scheduler leaves.
fn writer_loop(
    path: &Path,
    exec_rx: std::sync::mpsc::Receiver<QueuedOp>,
    ready_tx: std::sync::mpsc::Sender<Result<(), DbError>>,
) {
    let clock = Box::new(AbortClock::new());
    let opened = (|| -> Result<Connection, DbError> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(5000))?;
        // Writer cache 2 MiB, matching the reader pool.
        // The 16 MiB writer cache stayed resident after every 100k scan;
        // 2 MiB keeps scan throughput identical (reindex 42 s either way).
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA mmap_size=67108864;
             PRAGMA temp_store=MEMORY;
             PRAGMA cache_size=-2048;
             PRAGMA wal_autocheckpoint=1000;",
        )?;
        register_fold(&connection)?;
        Ok(connection)
    })();
    let connection = match opened {
        Ok(connection) => connection,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    install_abort_clock(&connection, &clock);
    let _ = ready_tx.send(Ok(()));
    let mut connection = connection;
    for req in exec_rx {
        run_one(&mut connection, &clock, req);
    }
    clear_abort_clock(&connection);
}

/// One admission, one transaction. The abort clock is armed for exactly the
/// transaction body; commit follows `Ok`, rollback follows anything else.
fn run_one(connection: &mut Connection, clock: &AbortClock, req: QueuedOp) {
    let started = Instant::now();
    clock.arm(WRITE_HARD_BUDGET);
    let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(transaction) => transaction,
        Err(error) => {
            clock.disarm();
            let _ = req.reply.send(LaneOutcome {
                value: Err(OpError::Sql(error)),
                changes: 0,
                elapsed: started.elapsed(),
            });
            return;
        }
    };
    let (value, changes) = match (req.op)(&transaction) {
        Ok(value) => match transaction.commit() {
            Ok(()) => (Ok(value), connection.total_changes()),
            Err(error) => (Err(OpError::Sql(error)), connection.total_changes()),
        },
        Err(error) => {
            drop(transaction);
            (Err(error), connection.total_changes())
        }
    };
    clock.disarm();
    let _ = req.reply.send(LaneOutcome {
        value,
        changes,
        elapsed: started.elapsed(),
    });
}

/// Wall-clock deadline the SQLite progress callback reads. Owned by the
/// writer thread; the callback fires only on that thread during a
/// transaction, so no cross-thread lifetime applies.
struct AbortClock {
    deadline_ms: AtomicU64,
}

impl AbortClock {
    fn new() -> Self {
        Self {
            deadline_ms: AtomicU64::new(DISARMED_MS),
        }
    }

    fn arm(&self, budget: Duration) {
        self.deadline_ms.store(
            now_millis().saturating_add(budget.as_millis() as u64),
            Ordering::Release,
        );
    }

    fn disarm(&self) {
        self.deadline_ms.store(DISARMED_MS, Ordering::Release);
    }
}

/// Progress callback: abort the transaction past its deadline. Reads one
/// atomic; the pointer stays valid because the clock outlives the connection
/// that carries the handler.
unsafe extern "C" fn progress_callback(context: *mut std::ffi::c_void) -> std::os::raw::c_int {
    // Safety: installed only by `install_abort_clock` with a live clock.
    let clock = unsafe { &*(context as *const AbortClock) };
    if now_millis() > clock.deadline_ms.load(Ordering::Acquire) {
        1
    } else {
        0
    }
}

/// Attach the abort clock to a writer connection. The clock must outlive the
/// connection; both live in `writer_loop`, the clock in the outer scope.
fn install_abort_clock(connection: &Connection, clock: &AbortClock) {
    let context: *const AbortClock = clock;
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(
            connection.handle(),
            PROGRESS_OPS,
            Some(progress_callback),
            context as *mut std::ffi::c_void,
        );
    }
}

/// Detach the abort clock before the writer connection closes.
fn clear_abort_clock(connection: &Connection) {
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(connection.handle(), 0, None, std::ptr::null_mut());
    }
}

/// Map a failed write to its typed error: interrupts become timeouts, lock
/// contention becomes retryable, the rest names the operation.
fn map_write_error(name: &str, changes: u64, error: rusqlite::Error) -> DbError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && code.code == rusqlite::ffi::ErrorCode::OperationInterrupted
    {
        return DbError::TxTimeout {
            operation: name.to_owned(),
            changes,
        };
    }
    if rusqlite_is_busy(&error) {
        return DbError::Busy {
            operation: name.to_owned(),
        };
    }
    DbError::WriteFailed {
        operation: name.to_owned(),
        cause: short_cause(&error),
    }
}

/// One line of cause: the SQLite code plus its short message, truncated.
/// The message names schema objects (tables, constraints), never bound
/// values, and this text is logged only, never rendered to HTTP callers.
fn short_cause(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(code, message) => {
            let detail: String = message
                .as_deref()
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect();
            if detail.is_empty() {
                format!("sqlite error {code:?}")
            } else {
                format!("sqlite error {code:?}: {detail}")
            }
        }
        other => {
            let text = other.to_string();
            text.chars().take(120).collect()
        }
    }
}

/// Current wall-clock milliseconds for the abort clock.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_eight_then_one_while_both_lanes_busy() {
        let mut grants = 0;
        let mut seen = Vec::new();
        for _ in 0..18 {
            let (lane, next) = decide_lane(true, true, grants, false);
            grants = next;
            seen.push(lane);
        }
        let foreground = Lane::Foreground;
        let background = Lane::Background;
        assert_eq!(
            seen,
            vec![
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(background),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(foreground),
                Some(background),
            ]
        );
    }

    #[test]
    fn lone_lane_drains_without_alternation() {
        let (lane, grants) = decide_lane(true, false, 40, false);
        assert_eq!((lane, grants), (Some(Lane::Foreground), 41));
        let (lane, grants) = decide_lane(false, true, 40, false);
        assert_eq!((lane, grants), (Some(Lane::Background), 0));
        let (lane, grants) = decide_lane(false, false, 7, false);
        assert_eq!((lane, grants), (None, 7));
    }

    #[test]
    fn fresh_background_waiter_resets_the_burst_like_v1() {
        let (lane, grants) = decide_lane(true, true, 8, true);
        assert_eq!((lane, grants), (Some(Lane::Foreground), 1));
        let (lane, grants) = decide_lane(true, true, 8, false);
        assert_eq!((lane, grants), (Some(Lane::Background), 0));
    }
}
