//! The writer lane: every write in the process takes a turn on one connection.
//!
//! SQLite serializes writers regardless, so this module makes the
//! serialization explicit and fair. Request-path writes wait in the
//! foreground queue, job writes in the background queue; while both are busy
//! the lane grants eight foreground turns, then one background turn, then
//! repeats. That is the v2 `PriorityWriteLock(foreground_burst=8)` rule; the
//! decision itself is the pure [`decide_lane`] function so the fairness test
//! pins it without threads.
//!
//! The lane owns the single write connection. A turn is a [`Turn`]: it holds
//! the connection for one `IMMEDIATE` transaction and, when dropped for any
//! reason, puts the connection back and hands the turn to the next waiter.
//! A blocking caller ([`WriteLane::write_blocking`]) receives its turn over
//! a channel and runs its closure on its own thread, so the closure may
//! borrow. An async caller's closure ([`WriteLane::write`]) is queued and
//! runs on the lane's own thread; the waiting future only receives the
//! answer. A turn therefore never depends on a runtime polling anything, and
//! the lane holds no runtime handle.
//!
//! The lane's mutex guards bookkeeping only and is never held while a
//! closure runs. A closure that panics rolls back and its turn passes on.
//! The lane's thread is not restarted: if it ever died, blocking writes
//! would carry on and every async write would answer `LaneClosed`.
//! Closures are synchronous (`FnOnce`, never `async`), so awaiting inside a
//! write transaction is unrepresentable: there is no async API on the write
//! path to await with.
//!
//! Nothing commits but the lane's own commit. A closure holds the
//! connection, so a raw `COMMIT` is within its reach, and when the abort
//! interrupts a writing statement SQLite rolls the whole transaction back
//! and returns to autocommit, where each later statement would commit alone.
//! A commit hook refuses every commit except the one the lane issues after
//! the closure returns, and a write whose transaction ended inside its
//! closure commits nothing, whatever the closure returned.
//!
//! Every transaction carries a 2 s hard abort through
//! the SQLite progress handler, foreground ones a 250 ms soft budget
//! (logged); chunked background work commits at most 500 rows at a time with
//! a cancellation check between chunks.

use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rusqlite::{Connection, Transaction, TransactionBehavior};
use tokio::sync::oneshot;

use super::{
    error::{DbError, rusqlite_is_busy},
    fold::register_fold,
    scope::{self, LaneId},
};

/// Foreground admissions granted before one background admission while both
/// lanes are busy. The v2 `foreground_burst=8` value, unchanged.
pub const FOREGROUND_BURST: u32 = 8;
/// Maximum queued admissions per lane. Past this, a write fails fast with
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
/// Upper bounds, in milliseconds, of the buckets [`WaitTimes`] counts into.
/// 250 ms is the wait the architecture's trip condition is stated against.
pub const WAIT_BUCKET_BOUNDS_MS: [u64; 7] = [1, 5, 25, 100, 250, 1_000, 5_000];

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

/// Shared liveness counters the checkpoint gate reads without taking the
/// lane's mutex.
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

/// How long admissions waited for their turn since the lane opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaitTimes {
    /// Turns granted.
    pub admissions: u64,
    /// The longest any admission waited.
    pub longest: Duration,
    /// Admissions per bucket of [`WAIT_BUCKET_BOUNDS_MS`]; the last entry
    /// counts waits past the largest bound.
    pub buckets: [u64; WAIT_BUCKET_BOUNDS_MS.len() + 1],
}

impl WaitTimes {
    fn record(&mut self, waited: Duration) {
        self.admissions += 1;
        self.longest = self.longest.max(waited);
        let bucket = WAIT_BUCKET_BOUNDS_MS
            .iter()
            .position(|bound| waited <= Duration::from_millis(*bound))
            .unwrap_or(WAIT_BUCKET_BOUNDS_MS.len());
        self.buckets[bucket] += 1;
    }
}

/// What one transaction did, before its failure is mapped for the caller.
struct Outcome<R> {
    value: Result<R, Failure>,
    changes: u64,
    elapsed: Duration,
}

/// Why a transaction committed nothing.
enum Failure {
    /// The closure returned an error, or `BEGIN` or the lane's commit failed.
    Op(OpError),
    /// The abort fired while the closure ran.
    TimedOut,
    /// The transaction ended inside the closure. Carries the closure's own
    /// error, if it returned one.
    Ended(Option<String>),
    /// The connection could not be made ready for the turn.
    Connection(DbError),
}

/// A queued async write: runs its transaction on the turn it is given and
/// answers its caller.
type AsyncWrite = Box<dyn FnOnce(Turn) + Send>;

/// Who a turn goes to.
enum Grantee {
    /// A caller parked in `write_blocking`, which runs its own closure.
    Blocking(mpsc::Sender<Turn>),
    /// A boxed async write, which the lane's thread runs.
    Async(AsyncWrite),
}

struct Waiter {
    name: &'static str,
    queued_at: Instant,
    grantee: Grantee,
}

/// An async write and the turn it was granted, on its way to the lane's
/// thread.
struct Granted {
    write: AsyncWrite,
    turn: Turn,
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

/// The single writer. A cloneable handle over shared state; every write in
/// the process takes its turn through it. Clones are cheap; every module
/// shares one lane through `AppState`.
#[derive(Clone)]
pub struct WriteLane {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for WriteLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteLane")
            .field("db_path", &self.shared.db_path)
            .finish_non_exhaustive()
    }
}

struct Shared {
    id: LaneId,
    db_path: PathBuf,
    state: Mutex<State>,
    /// Signalled when no turn is out and nobody waits.
    drained: Condvar,
    idle: Arc<LaneIdle>,
}

/// The lane's bookkeeping. Locked briefly, never while a closure runs.
struct State {
    foreground: VecDeque<Waiter>,
    background: VecDeque<Waiter>,
    /// The write connection while no turn is out. Gone after shutdown.
    connection: Option<WriterConnection>,
    turn_out: bool,
    grants: u32,
    bg_was_pending: bool,
    closed: bool,
    /// The way to the thread that runs async writes.
    runner: Option<mpsc::Sender<Granted>>,
    runner_thread: Option<JoinHandle<()>>,
    waits: WaitTimes,
}

impl State {
    /// Take the next waiter in fairness order.
    fn next_waiter(&mut self) -> Option<Waiter> {
        let fg_pending = !self.foreground.is_empty();
        let bg_pending = !self.background.is_empty();
        let (choice, grants) = decide_lane(
            fg_pending,
            bg_pending,
            self.grants,
            bg_pending && !self.bg_was_pending,
        );
        self.bg_was_pending = bg_pending;
        self.grants = grants;
        match choice? {
            Lane::Foreground => self.foreground.pop_front(),
            Lane::Background => self.background.pop_front(),
        }
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue a waiter and, if the lane is free, grant it at once.
    fn enqueue(
        self: &Arc<Self>,
        lane: Lane,
        name: &'static str,
        grantee: Grantee,
    ) -> Result<(), DbError> {
        // A write issued inside a closure on this lane would wait behind
        // that closure's turn, which cannot end until the write returns.
        if scope::inside_write_on(self.id) {
            return Err(DbError::NestedWrite {
                operation: name.to_owned(),
            });
        }
        // A refused closure is dropped by the caller, after the lock.
        let mut state = self.state();
        if state.closed {
            drop(state);
            return Err(DbError::LaneClosed);
        }
        let queue = match lane {
            Lane::Foreground => &mut state.foreground,
            Lane::Background => &mut state.background,
        };
        if queue.len() >= LANE_QUEUE_CAPACITY {
            drop(state);
            return Err(DbError::Busy {
                operation: name.to_owned(),
            });
        }
        queue.push_back(Waiter {
            name,
            queued_at: Instant::now(),
            grantee,
        });
        self.idle.queued.fetch_add(1, Ordering::AcqRel);
        drop(state);
        self.grant_next();
        Ok(())
    }

    /// Hand the turn to the next waiter, if the lane is free and anyone
    /// waits. A waiter that cannot take its turn gives the connection back
    /// here and the loop moves on, so passing a turn never recurses.
    fn grant_next(self: &Arc<Self>) {
        loop {
            let (waiter, connection, runner, waited) = {
                let mut state = self.state();
                if state.turn_out {
                    return;
                }
                let Some(connection) = state.connection.take() else {
                    return;
                };
                let Some(waiter) = state.next_waiter() else {
                    state.connection = Some(connection);
                    self.idle.active.store(false, Ordering::Release);
                    self.drained.notify_all();
                    return;
                };
                state.turn_out = true;
                self.idle.active.store(true, Ordering::Release);
                self.idle.queued.fetch_sub(1, Ordering::AcqRel);
                let waited = waiter.queued_at.elapsed();
                state.waits.record(waited);
                (waiter, connection, state.runner.clone(), waited)
            };
            tracing::debug!(
                operation = waiter.name,
                waited_ms = waited.as_millis() as u64,
                "writer turn granted"
            );
            let turn = Turn {
                shared: Arc::clone(self),
                connection: Some(connection),
            };
            let refused = match waiter.grantee {
                Grantee::Blocking(parked) => parked.send(turn).err().map(|unsent| unsent.0),
                Grantee::Async(write) => match runner {
                    Some(runner) => runner
                        .send(Granted { write, turn })
                        .err()
                        .map(|unsent| unsent.0.turn),
                    None => Some(turn),
                },
            };
            let Some(mut turn) = refused else {
                return;
            };
            tracing::error!(
                operation = waiter.name,
                "writer turn could not be delivered; passing it on"
            );
            if let Some(connection) = turn.connection.take() {
                self.take_back(connection);
            }
        }
    }

    /// Put the connection back at the end of a turn.
    fn take_back(&self, connection: WriterConnection) {
        let mut state = self.state();
        state.connection = Some(connection);
        state.turn_out = false;
    }

    /// Refuse new writes, wait for queued ones to finish, close the
    /// connection and stop the lane's thread. False if that thread did not
    /// stop cleanly.
    fn close(&self) -> bool {
        let (connection, runner, runner_thread) = {
            let mut state = self.state();
            state.closed = true;
            while state.turn_out || !state.foreground.is_empty() || !state.background.is_empty() {
                state = self
                    .drained
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            (
                state.connection.take(),
                state.runner.take(),
                state.runner_thread.take(),
            )
        };
        drop(connection);
        drop(runner);
        runner_thread.is_none_or(|thread| thread.join().is_ok())
    }
}

/// One turn on the write connection. Dropping it, for any reason, puts the
/// connection back and passes the turn on, so a turn cannot be lost.
struct Turn {
    shared: Arc<Shared>,
    connection: Option<WriterConnection>,
}

impl Turn {
    fn transact<R>(&mut self, op: impl FnOnce(&Transaction) -> Result<R, OpError>) -> Outcome<R> {
        match self.connection.as_mut() {
            Some(connection) => connection.transact(op),
            None => Outcome {
                value: Err(Failure::Op(OpError::Abort(
                    "the turn holds no connection".to_owned(),
                ))),
                changes: 0,
                elapsed: Duration::ZERO,
            },
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            self.shared.take_back(connection);
            self.shared.grant_next();
        }
    }
}

impl WriteLane {
    /// Open the writer on an existing database file: the write connection
    /// with its pragmas set, and the thread that runs async writes. Needs no
    /// runtime.
    pub fn open(db_path: &Path) -> Result<Self, DbError> {
        let id = LaneId::next();
        let connection = WriterConnection::open(db_path, id)?;
        let (runner, granted) = mpsc::channel();
        let runner_thread = std::thread::Builder::new()
            .name("droppedneedle-writer".to_owned())
            .spawn(move || run_async_writes(granted))
            .map_err(DbError::Io)?;
        Ok(Self {
            shared: Arc::new(Shared {
                id,
                db_path: db_path.to_owned(),
                state: Mutex::new(State {
                    foreground: VecDeque::new(),
                    background: VecDeque::new(),
                    connection: Some(connection),
                    turn_out: false,
                    grants: 0,
                    bg_was_pending: false,
                    closed: false,
                    runner: Some(runner),
                    runner_thread: Some(runner_thread),
                    waits: WaitTimes::default(),
                }),
                drained: Condvar::new(),
                idle: Arc::new(LaneIdle::default()),
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

    /// How long admissions have waited for their turn so far.
    pub fn wait_times(&self) -> WaitTimes {
        self.shared.state().waits
    }

    /// Run one synchronous closure inside one `IMMEDIATE` transaction.
    ///
    /// The closure receives the transaction and returns a value; commit
    /// follows a returned `Ok`, rollback follows `Err`, an abort or a panic.
    /// The closure is plain `FnOnce`: holding the lock across an await is
    /// impossible because there is nothing to await with. It is queued on
    /// the first poll and runs on the lane's thread when its turn comes,
    /// whether or not this future is still polled. A full lane fails fast
    /// with `Busy` rather than queueing without bound.
    pub async fn write<F, R>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, DbError>
    where
        F: FnOnce(&Transaction) -> Result<R, OpError> + Send + 'static,
        R: Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let write: AsyncWrite = Box::new(move |mut turn| {
            let outcome = catch_unwind(AssertUnwindSafe(|| turn.transact(op)));
            // The turn goes back before the caller hears, so a caller that
            // was just answered finds the lane idle.
            drop(turn);
            if outcome.is_err() {
                tracing::error!(operation = name, "write closure panicked; rolled back");
            }
            let _ = reply.send(outcome.ok());
        });
        self.shared.enqueue(lane, name, Grantee::Async(write))?;
        match answer.await {
            Ok(Some(outcome)) => finish(lane, name, outcome),
            Ok(None) => Err(DbError::WriteFailed {
                operation: name.to_owned(),
                cause: "the write closure panicked".to_owned(),
            }),
            Err(_) => Err(DbError::LaneClosed),
        }
    }

    /// [`WriteLane::write`] for synchronous code: wait for the turn on the
    /// calling thread and run the closure there, so it may borrow from the
    /// caller. A panic in the closure rolls back, passes the turn on and
    /// continues up the caller's thread.
    ///
    /// This parks the calling thread until its turn. Call it from a blocking
    /// thread, not from async code. Inside a write closure on this lane it
    /// is refused with `NestedWrite`; a closure that hands the write to
    /// another thread and waits for it is not seen, and hangs the lane.
    pub fn write_blocking<F, R>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, DbError>
    where
        F: FnOnce(&Transaction) -> Result<R, OpError>,
    {
        let (parked, granted) = mpsc::channel();
        self.shared.enqueue(lane, name, Grantee::Blocking(parked))?;
        let mut turn = granted.recv().map_err(|_| DbError::LaneClosed)?;
        let outcome = turn.transact(op);
        drop(turn);
        finish(lane, name, outcome)
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

    /// Drain and stop: refuse new writes, let queued ones finish, then
    /// close the connection and join the lane's thread. Idempotent: the
    /// first call wins, later calls find nothing to close.
    pub async fn shutdown(&self) {
        let shared = Arc::clone(&self.shared);
        let closed = tokio::task::spawn_blocking(move || shared.close()).await;
        if !matches!(closed, Ok(true)) {
            tracing::error!("writer thread did not stop cleanly");
        }
    }
}

/// The lane's thread: run each granted async write. The write drops its
/// turn itself, which passes the lane on.
fn run_async_writes(granted: mpsc::Receiver<Granted>) {
    for Granted { write, turn } in granted {
        write(turn);
    }
}

/// Map one transaction's outcome for its caller and log a slow foreground
/// write.
fn finish<R>(lane: Lane, name: &'static str, outcome: Outcome<R>) -> Result<R, DbError> {
    if lane == Lane::Foreground && outcome.elapsed > FOREGROUND_SOFT_BUDGET {
        tracing::warn!(
            operation = name,
            elapsed_ms = outcome.elapsed.as_millis() as u64,
            changes = outcome.changes,
            "foreground write exceeded its soft budget"
        );
    }
    match outcome.value {
        Ok(value) => Ok(value),
        Err(Failure::TimedOut) => Err(DbError::TxTimeout {
            operation: name.to_owned(),
            changes: outcome.changes,
        }),
        Err(Failure::Ended(cause)) => {
            tracing::error!(
                operation = name,
                cause = cause.as_deref().unwrap_or("the closure returned no error"),
                "write transaction was rolled back inside its closure"
            );
            Err(DbError::TransactionEnded {
                operation: name.to_owned(),
            })
        }
        Err(Failure::Connection(error)) => Err(error),
        Err(Failure::Op(OpError::Abort(cause))) => Err(DbError::WriteFailed {
            operation: name.to_owned(),
            cause,
        }),
        Err(Failure::Op(OpError::Sql(error))) => Err(map_write_error(name, outcome.changes, error)),
    }
}

/// Open a dedicated rusqlite connection with the same pragma set as the
/// reader pool: WAL, `synchronous=NORMAL`, foreign keys on, the 5 s busy
/// timeout, and the shared mmap, temp-store, cache and autocheckpoint sizes.
/// Every rusqlite handle in the process goes through here so none of them
/// drifts from the factory's settings.
pub fn open_connection(path: &Path) -> Result<Connection, DbError> {
    let connection = Connection::open(path)?;
    apply_connection_pragmas(&connection)?;
    Ok(connection)
}

/// [`open_connection`] with `synchronous=FULL`, for the connections that
/// write the publish journal. A journal row has to be on disk before the
/// file move it describes: under `NORMAL` a power cut can lose the last
/// commits in the WAL, and the move would outlive its row. Each commit
/// costs a sync, so nothing else opens a connection this way.
pub fn open_journal_connection(path: &Path) -> Result<Connection, DbError> {
    let connection = open_connection(path)?;
    connection.execute_batch("PRAGMA synchronous=FULL;")?;
    Ok(connection)
}

/// The canonical pragma set and busy timeout on one rusqlite connection,
/// for the rare handle not opened from a path (an in-memory test store).
pub fn apply_connection_pragmas(connection: &Connection) -> Result<(), DbError> {
    connection.busy_timeout(crate::db::BUSY_TIMEOUT)?;
    // Cache 2 MiB, matching the reader pool. A 16 MiB writer cache stayed
    // resident after every 100k scan; 2 MiB keeps scan throughput identical.
    connection.execute_batch(&format!(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA mmap_size={mmap};
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size={cache};
         PRAGMA wal_autocheckpoint={checkpoint};",
        mmap = crate::db::MMAP_SIZE,
        cache = crate::db::CACHE_SIZE_KIB,
        checkpoint = crate::db::WAL_AUTOCHECKPOINT,
    ))?;
    Ok(())
}

/// The write connection, the abort clock its progress handler reads and
/// the watch its commit and rollback hooks report to.
struct WriterConnection {
    lane: LaneId,
    path: PathBuf,
    connection: Connection,
    clock: Arc<AbortClock>,
    watch: Arc<TxWatch>,
}

impl WriterConnection {
    fn open(path: &Path, lane: LaneId) -> Result<Self, DbError> {
        let connection = open_connection(path)?;
        register_fold(&connection)?;
        let clock = Arc::new(AbortClock::new());
        let handler_clock = Arc::clone(&clock);
        connection.progress_handler(PROGRESS_OPS, Some(move || handler_clock.check()));
        // Verified against SQLite 3.46.0 (libsqlite3-sys 0.30.1, rusqlite
        // 0.32.1): a commit hook that refuses rolls back an autocommit
        // statement and a raw `COMMIT`, and a refused `RELEASE` at depth
        // zero leaves the connection out of autocommit, so the rollback
        // flag decides whether the transaction ended, not `is_autocommit()`
        // alone.
        let watch = Arc::new(TxWatch::default());
        let commit_watch = Arc::clone(&watch);
        connection.commit_hook(Some(move || commit_watch.refuses_commit()));
        let rollback_watch = Arc::clone(&watch);
        connection.rollback_hook(Some(move || rollback_watch.note_rollback()));
        Ok(Self {
            lane,
            path: path.to_owned(),
            connection,
            clock,
            watch,
        })
    }

    /// One turn, one transaction. The abort clock is armed for exactly the
    /// transaction; commit follows `Ok`, rollback follows anything else,
    /// a panic included.
    fn transact<R>(&mut self, op: impl FnOnce(&Transaction) -> Result<R, OpError>) -> Outcome<R> {
        let started = Instant::now();
        if let Err(error) = self.start_in_autocommit() {
            return Outcome {
                value: Err(Failure::Connection(error)),
                changes: 0,
                elapsed: started.elapsed(),
            };
        }
        let before = self.connection.total_changes();
        let armed = self.clock.arm(WRITE_HARD_BUDGET);
        let value = run_transaction(&mut self.connection, &armed, &self.watch, self.lane, op);
        drop(armed);
        Outcome {
            value,
            changes: self.connection.total_changes().saturating_sub(before),
            elapsed: started.elapsed(),
        }
    }

    /// A turn starts on a connection with no transaction open. One left
    /// open (a rollback that failed at the end of the last turn) is rolled
    /// back here, and if that fails too the connection is replaced.
    fn start_in_autocommit(&mut self) -> Result<(), DbError> {
        if self.connection.is_autocommit() {
            return Ok(());
        }
        let Err(error) = self.connection.execute_batch("ROLLBACK") else {
            tracing::warn!("write connection held an open transaction; rolled it back");
            return Ok(());
        };
        tracing::error!(
            cause = short_cause(&error),
            "write connection could not roll back; opening a new one"
        );
        *self = Self::open(&self.path, self.lane)?;
        Ok(())
    }
}

/// Run the closure between the lane's `BEGIN IMMEDIATE` and the lane's
/// commit. The abort and the transaction's own fate are judged first:
/// either one discards what the closure returned.
fn run_transaction<R>(
    connection: &mut Connection,
    armed: &Armed<'_>,
    watch: &TxWatch,
    lane: LaneId,
    op: impl FnOnce(&Transaction) -> Result<R, OpError>,
) -> Result<R, Failure> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| Failure::Op(error.into()))?;
    watch.begin();
    let value = {
        let _inside = scope::enter_write(lane);
        op(&transaction)
    };
    if armed.fired() {
        return Err(Failure::TimedOut);
    }
    if watch.rolled_back() || transaction.is_autocommit() {
        let cause = match value {
            Ok(_) => None,
            Err(OpError::Sql(error)) => Some(short_cause(&error)),
            Err(OpError::Abort(cause)) => Some(cause),
        };
        return Err(Failure::Ended(cause));
    }
    let value = value.map_err(Failure::Op)?;
    let _allowed = watch.allow_commit();
    transaction
        .commit()
        .map_err(|error| Failure::Op(error.into()))?;
    Ok(value)
}

/// What the commit and rollback hooks on the write connection share with
/// the lane. The hooks run on whichever thread executes the statement.
#[derive(Default)]
struct TxWatch {
    /// True only while the lane issues its own commit.
    commit_allowed: AtomicBool,
    /// Set by any whole rollback since the lane's `BEGIN`. A rollback to a
    /// savepoint does not count.
    rolled_back: AtomicBool,
}

impl TxWatch {
    /// The commit hook's answer: true turns the commit into a rollback.
    fn refuses_commit(&self) -> bool {
        !self.commit_allowed.load(Ordering::Acquire)
    }

    fn note_rollback(&self) {
        self.rolled_back.store(true, Ordering::Release);
    }

    fn begin(&self) {
        self.rolled_back.store(false, Ordering::Release);
    }

    fn rolled_back(&self) -> bool {
        self.rolled_back.load(Ordering::Acquire)
    }

    /// Let the lane's commit through until the guard is dropped.
    fn allow_commit(&self) -> CommitAllowed<'_> {
        self.commit_allowed.store(true, Ordering::Release);
        CommitAllowed(self)
    }
}

struct CommitAllowed<'a>(&'a TxWatch);

impl Drop for CommitAllowed<'_> {
    fn drop(&mut self) {
        self.0.commit_allowed.store(false, Ordering::Release);
    }
}

/// Deadline the SQLite progress handler reads, on the monotonic clock so a
/// wall-clock step can neither abort a healthy write nor spare a stuck one.
struct AbortClock {
    epoch: Instant,
    /// Microseconds after `epoch`; `u64::MAX` while disarmed.
    deadline_micros: AtomicU64,
    /// True once the handler has aborted a statement in this transaction.
    fired: AtomicBool,
}

impl AbortClock {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            deadline_micros: AtomicU64::new(u64::MAX),
            fired: AtomicBool::new(false),
        }
    }

    fn now_micros(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    /// Start the budget. The returned guard disarms the clock when dropped,
    /// on every way out of the transaction.
    fn arm(&self, budget: Duration) -> Armed<'_> {
        let budget = u64::try_from(budget.as_micros()).unwrap_or(u64::MAX);
        self.fired.store(false, Ordering::Release);
        self.deadline_micros
            .store(self.now_micros().saturating_add(budget), Ordering::Release);
        Armed(self)
    }

    /// The progress handler's answer: true aborts the running statement.
    fn check(&self) -> bool {
        let expired = self.now_micros() > self.deadline_micros.load(Ordering::Acquire);
        if expired {
            self.fired.store(true, Ordering::Release);
        }
        expired
    }
}

struct Armed<'a>(&'a AbortClock);

impl Armed<'_> {
    /// True if the abort has fired since the clock was armed.
    fn fired(&self) -> bool {
        self.0.fired.load(Ordering::Acquire)
    }
}

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        self.0.deadline_micros.store(u64::MAX, Ordering::Release);
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
