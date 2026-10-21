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
//! Who waits and who is refused. A foreground write is refused with `Busy`
//! when 128 are already waiting, and again once it has waited 5 s for its
//! turn: a request does not wait for ever behind a stuck write. A background
//! write waits for its turn however long that takes; it is refused only past
//! 4,096 waiting, which means a producer is not awaiting its writes.
//!
//! A bulk write ([`Lane::Bulk`]) is a background write for fairness and
//! budget, and the one class the WAL can hold: while the checkpoint service
//! says the WAL is too large or stalled ([`WalHold`]), no bulk turn is
//! granted. Requests, job writes and durable writes carry on, including the
//! job loop's own heartbeats. The hold ends by itself: while a bulk write is
//! held, a relief thread runs a checkpoint pass every second, and the pass
//! that clears the state starts the held writes. Shutdown lifts the hold.
//!
//! A durable write ([`WriteLane::write_durable`]) is a blocking write whose
//! commit is synced to disk before it returns. It keeps its place in the
//! queue it names but waits like a background write: neither the 128 bound
//! nor the 5 s limit applies to it.
//!
//! Each class has a time budget, enforced through the SQLite progress
//! handler: a foreground transaction is logged past 250 ms and aborted at
//! 2 s, a background one logged past 1 s and aborted at 5 s. The abort only
//! fires inside SQLite. For a closure stuck anywhere else there is the
//! [`LaneWatch`] thread, which the database runtime starts: once a second it
//! answers the foreground writes that have waited too long and names in the
//! log a write that has held the lane past twice its budget. Chunked bulk
//! work commits at most 500 rows at a time, each chunk its own turn, with a
//! cancellation check between chunks.

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
use tokio::sync::{oneshot, watch};

use super::{
    error::{ChunkedError, DbError, rusqlite_is_busy},
    read_pool::OldestRead,
    scope::{self, LaneId},
};

/// Foreground admissions granted before one background admission while both
/// lanes are busy. The v2 `foreground_burst=8` value, unchanged.
pub const FOREGROUND_BURST: u32 = 8;
/// Foreground writes that may wait at once. Past this, a write fails fast
/// with `Busy` (HTTP 503 with `Retry-After` at the handler layer): a request
/// is better told to retry than parked behind 128 others.
pub const FOREGROUND_QUEUE_CAPACITY: usize = 128;
/// Writes that may wait at once in each class that waits for its turn: job
/// writes, bulk writes and durable writes, each counted on its own. A
/// producer that awaits its writes has one outstanding, so this is only
/// reached by one that spawns writes without awaiting them; it then fails
/// loudly instead of growing memory without bound. Counting per class means
/// a backlog of held bulk writes cannot get a job's state write or a journal
/// write refused.
pub const BACKGROUND_QUEUE_CAPACITY: usize = 4_096;
/// Foreground transactions slower than this log a warning with their name.
/// A request-path write is a handful of rows; a quarter of a second means
/// something is wrong with it.
pub const FOREGROUND_SOFT_BUDGET: Duration = Duration::from_millis(250);
/// Hard abort for a foreground transaction, enforced by the SQLite progress
/// handler. Well under the 5 s busy horizon, so one runaway request cannot
/// push the requests waiting behind it past their own limit.
pub const FOREGROUND_HARD_BUDGET: Duration = Duration::from_secs(2);
/// Background transactions slower than this log a warning with their name.
/// Job writes carry more rows than a request does; past a second they hold
/// requests up for long enough to notice.
pub const BACKGROUND_SOFT_BUDGET: Duration = Duration::from_secs(1);
/// Hard abort for a background transaction. The busy horizon: a request
/// never waits behind one job write for longer than it could wait on a lock.
/// A write that cannot fit is chunked.
pub const BACKGROUND_HARD_BUDGET: Duration = Duration::from_secs(5);
/// How long a foreground write waits for its turn before it is answered
/// `Busy`. The busy horizon again: what a request already gets when another
/// connection holds the file lock that long.
pub const FOREGROUND_WAIT_LIMIT: Duration = Duration::from_secs(5);
/// How often the watch thread wakes. A second keeps the `Busy` answer within
/// a second of the limit and costs nothing while the lane is idle.
pub const WATCH_TICK: Duration = Duration::from_secs(1);
/// How long a test database waits for a turn, a connection or a write's
/// end before giving up: long enough that a loaded host only slows a test.
#[cfg(any(test, feature = "test-support"))]
pub const TEST_PATIENCE: Duration = Duration::from_secs(60);
/// How often a write that still holds the lane is named in the log again.
pub const HOLDER_LOG_EVERY: Duration = Duration::from_secs(30);
/// Progress-handler granularity: the abort check runs every 10k VM ops.
const PROGRESS_OPS: std::os::raw::c_int = 10_000;
/// Largest chunk a bulk write commits at once.
pub const BACKGROUND_CHUNK_ROWS: usize = 500;
/// Upper bounds, in milliseconds, of the buckets [`WaitTimes`] counts into.
/// 250 ms is the wait the architecture's trip condition is stated against.
pub const WAIT_BUCKET_BOUNDS_MS: [u64; 7] = [1, 5, 25, 100, 250, 1_000, 5_000];

/// The class of a write: which queue it waits in and what may hold it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Request path. Never held by the WAL, never starved by jobs.
    Foreground,
    /// Job state: heartbeats, transitions, outbox rows, the download
    /// journal. Yields to foreground bursts and is never held by the WAL.
    Background,
    /// Many rows at once: the chunked writes. Shares the background side of
    /// the fairness rule and the background budget, and is the one class
    /// that waits while the WAL is too large or stalled.
    Bulk,
}

/// The class of a durable write. A durable write is never held by the WAL,
/// so it has no bulk class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableLane {
    /// Request path.
    Foreground,
    /// Job state.
    Background,
}

/// The transaction of a durable write: its commit is synced to disk before
/// [`WriteLane::write_durable`] returns. Only the lane makes one, so code
/// that takes `&Durable` runs only inside a durable write, and a row such
/// code writes cannot be written by an ordinary one. It derefs to the
/// transaction for everything else.
pub struct Durable<'a> {
    tx: &'a Transaction<'a>,
}

impl<'a> Durable<'a> {
    fn new(tx: &'a Transaction<'a>) -> Self {
        Self { tx }
    }
}

impl<'a> std::ops::Deref for Durable<'a> {
    type Target = Transaction<'a>;

    fn deref(&self) -> &Transaction<'a> {
        self.tx
    }
}

impl From<DurableLane> for Lane {
    fn from(lane: DurableLane) -> Self {
        match lane {
            DurableLane::Foreground => Self::Foreground,
            DurableLane::Background => Self::Background,
        }
    }
}

impl Lane {
    fn label(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::Bulk => "bulk",
        }
    }
}

/// Whether bulk writes are held back because the WAL cannot shrink. The
/// checkpoint service sets it after every pass that did not fail; the lane
/// reads it each time it picks the next write.
#[derive(Debug, Default)]
pub struct WalHold {
    on: AtomicBool,
}

impl WalHold {
    /// True while bulk writes must wait.
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    pub(crate) fn set(&self, on: bool) {
        self.on.store(on, Ordering::Release);
    }

    /// Set the hold by hand, where a test has no full WAL to show.
    #[cfg(any(test, feature = "test-support"))]
    pub fn force(&self, on: bool) {
        self.set(on);
    }
}

/// What one relief pass tells the lane, for its line about held writes.
#[derive(Debug, Default)]
pub struct ReliefPass {
    /// The checkpoint's error, if the pass failed.
    pub error: Option<String>,
    /// The pooled read that has been open longest: an open read is what
    /// keeps the WAL from shrinking.
    pub oldest_read: Option<OldestRead>,
}

/// What a waiting write counts as against the queue bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// An ordinary foreground write: the only class refused at 128.
    Request,
    /// An ordinary background write.
    Job,
    /// A bulk write.
    Bulk,
    /// A durable write, in either queue.
    Durable,
}

impl Class {
    const COUNT: usize = 4;

    fn of(lane: Lane, durable: bool) -> Self {
        match lane {
            _ if durable => Self::Durable,
            Lane::Foreground => Self::Request,
            Lane::Background => Self::Job,
            Lane::Bulk => Self::Bulk,
        }
    }

    fn capacity(self) -> usize {
        match self {
            Self::Request => FOREGROUND_QUEUE_CAPACITY,
            Self::Job | Self::Bulk | Self::Durable => BACKGROUND_QUEUE_CAPACITY,
        }
    }
}

/// The time one class of write may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// A transaction slower than this is logged.
    pub warn: Duration,
    /// A transaction still running SQL at this point is aborted.
    pub abort: Duration,
}

/// The lane's time limits. Production runs on the defaults, which are the
/// constants above; these are not settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneLimits {
    /// Budget of a foreground transaction.
    pub foreground: Budget,
    /// Budget of a background transaction.
    pub background: Budget,
    /// How long a foreground write waits for its turn before `Busy`.
    pub foreground_wait: Duration,
    /// How often the watch thread wakes.
    pub watch_tick: Duration,
}

impl Default for LaneLimits {
    fn default() -> Self {
        Self {
            foreground: Budget {
                warn: FOREGROUND_SOFT_BUDGET,
                abort: FOREGROUND_HARD_BUDGET,
            },
            background: Budget {
                warn: BACKGROUND_SOFT_BUDGET,
                abort: BACKGROUND_HARD_BUDGET,
            },
            foreground_wait: FOREGROUND_WAIT_LIMIT,
            watch_tick: WATCH_TICK,
        }
    }
}

impl LaneLimits {
    /// Limits for a test database on a loaded host: a minute to wait for
    /// a turn and to run, so a starved test runs slowly instead of hearing
    /// `Busy` or a budget abort. A test that checks a limit sets its own.
    #[cfg(any(test, feature = "test-support"))]
    pub fn patient() -> Self {
        Self {
            foreground: Budget {
                warn: FOREGROUND_SOFT_BUDGET,
                abort: TEST_PATIENCE,
            },
            background: Budget {
                warn: BACKGROUND_SOFT_BUDGET,
                abort: TEST_PATIENCE,
            },
            foreground_wait: TEST_PATIENCE,
            watch_tick: WATCH_TICK,
        }
    }

    fn budget(&self, lane: Lane) -> Budget {
        match lane {
            Lane::Foreground => self.foreground,
            Lane::Background | Lane::Bulk => self.background,
        }
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

    /// Writes waiting for a turn.
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
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
struct Outcome<R, E> {
    value: Result<R, Failure<E>>,
    changes: u64,
    elapsed: Duration,
}

/// Why a transaction committed nothing.
enum Failure<E> {
    /// The closure returned its own error.
    Op(E),
    /// `BEGIN`, the lane's commit or the durable pragma failed.
    Sql(rusqlite::Error),
    /// The abort fired while the closure ran.
    TimedOut,
    /// The transaction ended inside the closure. Carries the closure's own
    /// error, if it returned one.
    Ended(Option<String>),
    /// The connection could not be made ready for the turn.
    Connection(DbError),
}

/// Why a queued write was answered without a turn.
#[derive(Debug, Clone, Copy)]
enum Refused {
    /// A foreground write waited past its limit.
    Busy,
    /// Shutdown gave up before the write's turn came.
    Closed,
    /// A chunk's flag tripped while the chunk waited.
    Cancelled,
}

impl Refused {
    fn into_error(self, name: &'static str) -> DbError {
        match self {
            Self::Busy => DbError::Busy {
                operation: name.to_owned(),
            },
            Self::Closed => DbError::LaneClosed,
            // The chunked write that hears this answers with the rows it
            // committed.
            Self::Cancelled => DbError::Cancelled {
                operation: name.to_owned(),
                chunks: 0,
            },
        }
    }
}

/// Why a write failed, kept typed inside the lane so a chunked write can
/// tell a cancelled chunk from a failed one. Callers get `E`.
enum TxError<E> {
    /// The lane's own failure.
    Lane(DbError),
    /// The closure's own error.
    Op(E),
}

impl<E: From<DbError>> TxError<E> {
    fn into_caller(self) -> E {
        match self {
            Self::Lane(error) => E::from(error),
            Self::Op(error) => error,
        }
    }
}

impl<E> From<DbError> for TxError<E> {
    fn from(error: DbError) -> Self {
        Self::Lane(error)
    }
}

/// What a queued async write tells its caller.
enum Answer<R, E> {
    Ran(Outcome<R, E>),
    Panicked,
    Refused(Refused),
}

/// A queued async write: given a turn it runs its transaction, and either
/// way it answers its caller.
type AsyncWrite = Box<dyn FnOnce(Result<Turn, Refused>) + Send>;

/// Who a turn goes to.
enum Grantee {
    /// A caller parked in `write_blocking`, which runs its own closure.
    Blocking(mpsc::Sender<Result<Turn, Refused>>),
    /// A boxed async write, which the lane's thread runs.
    Async(AsyncWrite),
}

struct Waiter {
    name: &'static str,
    lane: Lane,
    queued_at: Instant,
    /// A durable write. It is never answered `Busy` for waiting too long:
    /// its caller may be between two steps of a publish.
    durable: bool,
    /// The flag of the chunked write this is a chunk of. Once it trips the
    /// relief thread answers the waiter instead of leaving it to wait out a
    /// hold that may last as long as a reader does.
    cancel: Option<CancelFlag>,
    grantee: Grantee,
}

impl Waiter {
    fn class(&self) -> Class {
        Class::of(self.lane, self.durable)
    }

    /// Answer the caller without a turn. Runs the caller's own code (it
    /// drops the write closure), so never call it with the lane's mutex held.
    fn refuse(self, why: Refused) {
        match self.grantee {
            Grantee::Blocking(parked) => {
                let _ = parked.send(Err(why));
            }
            Grantee::Async(write) => write(Err(why)),
        }
    }
}

/// Answer each waiter without a turn. Refusing one drops its closure, which
/// runs the caller's code; one that panics is logged and the rest are still
/// answered, each with `why`.
fn refuse_all(waiters: impl IntoIterator<Item = Waiter>, why: Refused) {
    for waiter in waiters {
        let operation = waiter.name;
        if catch_unwind(AssertUnwindSafe(|| waiter.refuse(why))).is_err() {
            tracing::error!(
                operation,
                "refusing a queued write panicked; the others are still answered"
            );
        }
    }
}

/// The write that holds the turn.
struct Holder {
    name: &'static str,
    lane: Lane,
    since: Instant,
    /// When the watch thread last named it in the log.
    logged_at: Option<Instant>,
}

/// An async write and the turn it was granted, on its way to the lane's
/// thread.
struct Granted {
    write: AsyncWrite,
    turn: Turn,
}

/// Cooperative stop flag for chunked bulk writes and backups.
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
/// the process takes its turn through it. Only [`super::Db`] holds one, so
/// nothing outside this module reaches it.
#[derive(Clone)]
pub(in crate::db) struct WriteLane {
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
    limits: LaneLimits,
    state: Mutex<State>,
    /// Signalled when no turn is out and nobody waits.
    drained: Condvar,
    idle: Arc<LaneIdle>,
    hold: Arc<WalHold>,
    /// Moves after each commit that changed a row. Taken when the lane
    /// closes, so every receiver sees the end. Kept apart from `state`:
    /// announcing a commit never touches the turn-taking.
    commits: Mutex<Option<watch::Sender<u64>>>,
}

/// The lane's bookkeeping. Locked briefly, never while a closure runs.
struct State {
    foreground: VecDeque<Waiter>,
    /// Background and bulk writes in one queue, in arrival order, so
    /// neither starves the other.
    background: VecDeque<Waiter>,
    /// Waiters per [`Class`], across both queues.
    waiting: [usize; Class::COUNT],
    /// The write connection while no turn is out. Gone after shutdown.
    connection: Option<WriterConnection>,
    /// The write a turn is out to.
    holder: Option<Holder>,
    grants: u32,
    bg_was_pending: bool,
    closed: bool,
    /// Shutdown returned while a turn was still out: the connection is
    /// closed when that turn ends, not kept.
    gave_up: bool,
    /// The way to the thread that runs async writes.
    runner: Option<mpsc::Sender<Granted>>,
    runner_thread: Option<JoinHandle<()>>,
    waits: WaitTimes,
    /// When a full queue was last logged. A producer that is not awaiting
    /// its writes is refused at its own rate, which is no rate to log at.
    full_logged_at: Option<Instant>,
    /// When the bulk writes held by the WAL were last named in the log.
    held_logged_at: Option<Instant>,
}

impl State {
    /// Take the next waiter in fairness order. While `bulk_held`, a bulk
    /// write is stepped over where it stands: it keeps its place, and it
    /// does not count as pending, or the background side could be picked
    /// with nothing in it that may run. A job write or a durable write
    /// behind it is reached all the same.
    fn next_waiter(&mut self, bulk_held: bool) -> Option<Waiter> {
        let fg_pending = !self.foreground.is_empty();
        let bg_at = self
            .background
            .iter()
            .position(|waiter| !(bulk_held && waiter.class() == Class::Bulk));
        let bg_pending = bg_at.is_some();
        let (choice, grants) = decide_lane(
            fg_pending,
            bg_pending,
            self.grants,
            bg_pending && !self.bg_was_pending,
        );
        self.bg_was_pending = bg_pending;
        self.grants = grants;
        let waiter = match choice? {
            Lane::Foreground => self.foreground.pop_front(),
            Lane::Background | Lane::Bulk => self.background.remove(bg_at?),
        }?;
        self.waiting[waiter.class() as usize] -= 1;
        Some(waiter)
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn commits(&self) -> MutexGuard<'_, Option<watch::Sender<u64>>> {
        self.commits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tell every listener a commit changed rows. Nothing to tell once the
    /// lane has closed.
    fn announce_commit(&self) {
        if let Some(commits) = self.commits().as_ref() {
            commits.send_modify(|count| *count = count.wrapping_add(1));
        }
    }

    /// Queue a waiter and, if the lane is free, grant it at once.
    fn enqueue(
        self: &Arc<Self>,
        lane: Lane,
        name: &'static str,
        durable: bool,
        cancel: Option<CancelFlag>,
        grantee: Grantee,
    ) -> Result<(), DbError> {
        // A write issued inside a closure on this lane would wait behind
        // that closure's turn, which cannot end until the write returns.
        if scope::inside_write_on(self.id) {
            return Err(DbError::NestedWrite {
                operation: name.to_owned(),
            });
        }
        // A write issued inside a read closure would wait for its turn
        // while that closure pins the WAL.
        if scope::inside_read_on(self.id) {
            return Err(DbError::InsideRead {
                operation: name.to_owned(),
            });
        }
        // A refused closure is dropped by the caller, after the lock.
        let mut state = self.state();
        if state.closed {
            drop(state);
            return Err(DbError::LaneClosed);
        }
        // Each class is counted on its own. A durable write is not a
        // request to turn away at 128, and writes of one class, held bulk
        // writes above all, cannot fill the bound of another.
        let class = Class::of(lane, durable);
        let capacity = class.capacity();
        if state.waiting[class as usize] >= capacity {
            let log = class != Class::Request
                && state
                    .full_logged_at
                    .is_none_or(|at| at.elapsed() >= HOLDER_LOG_EVERY);
            if log {
                state.full_logged_at = Some(Instant::now());
            }
            drop(state);
            if log {
                tracing::error!(
                    operation = name,
                    lane = lane.label(),
                    durable,
                    waiting = capacity,
                    "write queue is full; a producer is not awaiting its writes"
                );
            }
            return Err(DbError::Busy {
                operation: name.to_owned(),
            });
        }
        state.waiting[class as usize] += 1;
        let queue = match lane {
            Lane::Foreground => &mut state.foreground,
            Lane::Background | Lane::Bulk => &mut state.background,
        };
        queue.push_back(Waiter {
            name,
            lane,
            queued_at: Instant::now(),
            durable,
            cancel,
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
                if state.holder.is_some() {
                    return;
                }
                let Some(connection) = state.connection.take() else {
                    return;
                };
                // A closed lane holds nothing back: shutdown lifts the hold.
                let bulk_held = !state.closed && self.hold.is_on();
                let Some(waiter) = state.next_waiter(bulk_held) else {
                    state.connection = Some(connection);
                    self.idle.active.store(false, Ordering::Release);
                    self.drained.notify_all();
                    return;
                };
                state.holder = Some(Holder {
                    name: waiter.name,
                    lane: waiter.lane,
                    since: Instant::now(),
                    logged_at: None,
                });
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
                Grantee::Blocking(parked) => {
                    parked.send(Ok(turn)).err().and_then(|unsent| unsent.0.ok())
                }
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

    /// Put the connection back at the end of a turn. After a shutdown that
    /// gave up on this turn there is nobody to keep it for, so it is closed
    /// and the lane is marked idle here: no later grant will do it.
    fn take_back(&self, connection: WriterConnection) {
        let mut state = self.state();
        state.holder = None;
        if state.gave_up {
            self.idle.active.store(false, Ordering::Release);
            self.drained.notify_all();
            drop(state);
            drop(connection);
        } else {
            state.connection = Some(connection);
        }
    }

    /// Refuse new writes, let queued ones finish for up to `grace`, close
    /// the connection and stop the lane's thread. Past `grace` the writes
    /// still queued are answered `LaneClosed` and the write holding the turn
    /// is left to end by itself. False if the lane's thread did not stop
    /// cleanly.
    fn close(self: &Arc<Self>, grace: Duration) -> bool {
        // A grace too long to add to the clock is no bound at all.
        let deadline = Instant::now().checked_add(grace);
        self.state().closed = true;
        // Closing lifted the WAL hold. A bulk write it held on an idle lane
        // is started here, before the wait below, which would otherwise sit
        // out the whole grace for it.
        self.grant_next();
        let (connection, runner, runner_thread, unserved, stuck) = {
            let mut state = self.state();
            while state.holder.is_some()
                || !state.foreground.is_empty()
                || !state.background.is_empty()
            {
                let Some(deadline) = deadline else {
                    state = self
                        .drained
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                    continue;
                };
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                state = self
                    .drained
                    .wait_timeout(state, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            let mut unserved: Vec<Waiter> = state.foreground.drain(..).collect();
            unserved.extend(state.background.drain(..));
            state.waiting = [0; Class::COUNT];
            self.idle.queued.fetch_sub(unserved.len(), Ordering::AcqRel);
            let stuck = state
                .holder
                .as_ref()
                .map(|holder| (holder.name, holder.since.elapsed()));
            state.gave_up = stuck.is_some();
            (
                state.connection.take(),
                state.runner.take(),
                state.runner_thread.take(),
                unserved,
                stuck,
            )
        };
        drop(connection);
        drop(runner);
        drop(self.commits().take());
        let refused = unserved.len();
        refuse_all(unserved, Refused::Closed);
        match stuck {
            // The lane's thread may be the one that is stuck. It ends by
            // itself once its write returns, so it is not waited for.
            Some((operation, held)) => {
                tracing::error!(
                    operation,
                    held_ms = held.as_millis() as u64,
                    refused,
                    "shutdown gave up on a write that still holds the lane"
                );
                true
            }
            None => runner_thread.is_none_or(|thread| thread.join().is_ok()),
        }
    }

    /// One pass of the watch thread: answer the foreground writes that have
    /// waited past their limit, and name a write that has held the turn past
    /// twice its budget.
    fn watch_tick(&self) {
        let (overdue, stuck) = {
            let mut state = self.state();
            let mut overdue = Vec::new();
            // Arrival order is age order, so the overdue ones are in front,
            // with the durable writes among them left where they are.
            let mut at = 0;
            while let Some(waiter) = state.foreground.get(at) {
                if waiter.durable {
                    at += 1;
                } else if waiter.queued_at.elapsed() >= self.limits.foreground_wait {
                    overdue.extend(state.foreground.remove(at));
                } else {
                    break;
                }
            }
            self.idle.queued.fetch_sub(overdue.len(), Ordering::AcqRel);
            state.waiting[Class::Request as usize] -= overdue.len();
            let waiting = (state.foreground.len(), state.background.len());
            let stuck = state.holder.as_mut().and_then(|holder| {
                let held = holder.since.elapsed();
                let due = held > self.limits.budget(holder.lane).abort.saturating_mul(2)
                    && holder
                        .logged_at
                        .is_none_or(|at| at.elapsed() >= HOLDER_LOG_EVERY);
                due.then(|| {
                    holder.logged_at = Some(Instant::now());
                    (holder.name, holder.lane, held)
                })
            });
            (overdue, stuck.map(|stuck| (stuck, waiting)))
        };
        // Logged before the refusals, which run the callers' code: a panic
        // there must not cost the line that names the holder.
        if let Some(((operation, lane, held), (foreground, background))) = stuck {
            tracing::error!(
                operation,
                lane = lane.label(),
                held_ms = held.as_millis() as u64,
                foreground_waiting = foreground,
                background_waiting = background,
                "a write has held the lane well past its budget"
            );
        }
        if let Some(oldest) = overdue.first() {
            tracing::warn!(
                refused = overdue.len(),
                oldest = oldest.name,
                waited_ms = oldest.queued_at.elapsed().as_millis() as u64,
                "foreground writes waited past their limit; answering busy"
            );
        }
        refuse_all(overdue, Refused::Busy);
    }

    /// One pass of the relief thread, which has work only while a bulk write
    /// waits. Chunks whose chunked write was cancelled are taken out of the
    /// queue and answered, so a cancelled write never waits out a hold. Then,
    /// while the WAL hold is on, run a checkpoint pass, which is what can
    /// end it; once the hold is off, whoever ended it, start the writes it
    /// kept waiting. The pass needs nothing from the lane: it has its own
    /// connection, and a held write is waiting for a turn, not inside a
    /// transaction.
    fn relief_tick(self: &Arc<Self>, checkpoint: &dyn Fn() -> ReliefPass) {
        let (stopped, waiting) = {
            let mut state = self.state();
            if state.closed || state.waiting[Class::Bulk as usize] == 0 {
                return;
            }
            let mut stopped = Vec::new();
            let mut at = 0;
            while let Some(waiter) = state.background.get(at) {
                if waiter.cancel.as_ref().is_some_and(CancelFlag::is_cancelled) {
                    stopped.extend(state.background.remove(at));
                } else {
                    at += 1;
                }
            }
            for waiter in &stopped {
                state.waiting[waiter.class() as usize] -= 1;
            }
            self.idle.queued.fetch_sub(stopped.len(), Ordering::AcqRel);
            (stopped, state.waiting[Class::Bulk as usize] > 0)
        };
        // Answered after the lock: refusing a write drops its closure.
        refuse_all(stopped, Refused::Cancelled);
        if !waiting {
            return;
        }
        let pass = if self.hold.is_on() {
            checkpoint()
        } else {
            ReliefPass::default()
        };
        if !self.hold.is_on() {
            self.grant_next();
            return;
        }
        // A long hold is not silent: a reader that never ends, or a pass
        // that keeps failing, parks bulk writes until shutdown.
        let held = {
            let mut state = self.state();
            let oldest = state
                .background
                .iter()
                .find(|waiter| waiter.class() == Class::Bulk)
                .map(|waiter| waiter.queued_at.elapsed())
                .filter(|waited| *waited >= HOLDER_LOG_EVERY);
            let due = state
                .held_logged_at
                .is_none_or(|at| at.elapsed() >= HOLDER_LOG_EVERY);
            oldest.filter(|_| due).map(|oldest| {
                state.held_logged_at = Some(Instant::now());
                (state.waiting[Class::Bulk as usize], oldest)
            })
        };
        if let Some((held, oldest)) = held {
            tracing::warn!(
                held,
                oldest_waited_ms = oldest.as_millis() as u64,
                last_pass_error = pass.error.as_deref().unwrap_or("none"),
                oldest_read = pass.oldest_read.map_or("none", |read| read.name),
                oldest_read_pool = pass.oldest_read.map_or("none", |read| read.pool),
                oldest_read_ms = pass
                    .oldest_read
                    .map_or(0, |read| read.age.as_millis() as u64),
                "bulk writes are still held: the WAL cannot shrink"
            );
        }
    }
}

/// One turn on the write connection. Dropping it, for any reason, puts the
/// connection back and passes the turn on, so a turn cannot be lost.
struct Turn {
    shared: Arc<Shared>,
    connection: Option<WriterConnection>,
}

impl Turn {
    /// Run the turn's transaction and log what its caller may no longer be
    /// there to hear: a queued write runs even when its future was dropped,
    /// so these lines are written here, on the thread that ran it.
    fn transact<R, E: std::fmt::Display>(
        &mut self,
        lane: Lane,
        budget: Budget,
        name: &'static str,
        durable: bool,
        op: impl FnOnce(&Transaction) -> Result<R, E>,
    ) -> Outcome<R, E> {
        let Some(connection) = self.connection.as_mut() else {
            return Outcome {
                value: Err(Failure::Connection(DbError::WriteFailed {
                    operation: name.to_owned(),
                    cause: "the turn holds no connection".to_owned(),
                })),
                changes: 0,
                elapsed: Duration::ZERO,
            };
        };
        let outcome = connection.transact(budget, name, durable, op);
        // `Ok` only once the lane's commit went through: a rollback, an
        // abort or a panic never gets here with one.
        if outcome.value.is_ok() && outcome.changes > 0 {
            self.shared.announce_commit();
        }
        // `elapsed` runs from before `BEGIN`, so it includes the wait for
        // the file lock, which the abort budget does not count, and for a
        // durable write the sync at commit.
        if outcome.elapsed > budget.warn {
            tracing::warn!(
                operation = name,
                lane = lane.label(),
                durable,
                elapsed_ms = outcome.elapsed.as_millis() as u64,
                changes = outcome.changes,
                "write exceeded its soft budget"
            );
        }
        if let Err(Failure::Ended(cause)) = &outcome.value {
            tracing::error!(
                operation = name,
                cause = cause.as_deref().unwrap_or("the closure returned no error"),
                "write transaction was rolled back inside its closure"
            );
        }
        outcome
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
    /// Open the writer on an existing database file with `limits`: the
    /// write connection with its pragmas set, and the thread that runs
    /// async writes. Needs no runtime.
    ///
    /// A lane opened here has no watch thread: until [`WriteLane::watch`] is
    /// started, a foreground write waits for its turn without limit and a
    /// stuck holder is not logged. The database runtime starts one for its
    /// lane, which is the only lane production opens. Nothing sets the WAL
    /// hold on a lane without a checkpoint service either.
    #[cfg(test)]
    pub fn open_with(db_path: &Path, limits: LaneLimits) -> Result<Self, DbError> {
        Self::open_limited(db_path, limits)
    }

    /// The writer with `limits`, as the runtime and the test handles open it.
    pub(crate) fn open_limited(db_path: &Path, limits: LaneLimits) -> Result<Self, DbError> {
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
                limits,
                state: Mutex::new(State {
                    foreground: VecDeque::new(),
                    background: VecDeque::new(),
                    waiting: [0; Class::COUNT],
                    connection: Some(connection),
                    holder: None,
                    grants: 0,
                    bg_was_pending: false,
                    closed: false,
                    gave_up: false,
                    runner: Some(runner),
                    runner_thread: Some(runner_thread),
                    waits: WaitTimes::default(),
                    full_logged_at: None,
                    held_logged_at: None,
                }),
                drained: Condvar::new(),
                idle: Arc::new(LaneIdle::default()),
                hold: Arc::new(WalHold::default()),
                commits: Mutex::new(Some(watch::channel(0).0)),
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

    /// The hold on this lane's bulk writes, for the checkpoint service to
    /// set.
    pub fn wal_hold(&self) -> Arc<WalHold> {
        Arc::clone(&self.shared.hold)
    }

    /// Moves after each write that committed a changed row; closed once
    /// the lane has shut down.
    pub(crate) fn commits(&self) -> watch::Receiver<u64> {
        match self.shared.commits().as_ref() {
            Some(commits) => commits.subscribe(),
            None => watch::channel(0).1,
        }
    }

    /// The database this lane writes, as the per-thread record of open
    /// closures knows it.
    pub(crate) fn database(&self) -> LaneId {
        self.shared.id
    }

    /// How long admissions have waited for their turn so far.
    #[cfg(test)]
    pub fn wait_times(&self) -> WaitTimes {
        self.shared.state().waits
    }

    /// One pass of the watch thread, on the calling thread.
    #[cfg(test)]
    pub(in crate::db) fn watch_pass(&self) {
        self.shared.watch_tick();
    }

    /// Start the watch for this lane: two threads that wake on real time,
    /// not a runtime's clock, so paused time in a test neither trips nor
    /// stalls them. They stop when the returned handle is stopped or
    /// dropped, or when the lane is gone.
    ///
    /// The watch thread answers the foreground writes that have waited too
    /// long and names a stuck holder. The relief thread calls `checkpoint`,
    /// one blocking checkpoint pass that returns its error if it failed and
    /// the oldest pooled read, once a tick while a bulk write is held by the
    /// WAL, and starts the held writes when the hold is off. Writes held for
    /// 30 s or more are counted in the log, at most every 30 s, with that
    /// error and that read. A pass takes as long as the checkpoint takes,
    /// which is why it has a thread of its own: it never delays the watch
    /// thread's answers.
    pub fn watch(
        &self,
        checkpoint: impl Fn() -> ReliefPass + Send + 'static,
    ) -> Result<LaneWatch, DbError> {
        let tick = self.shared.limits.watch_tick;
        let watch = Ticker::start("droppedneedle-db-watch", tick, &self.shared, |shared| {
            shared.watch_tick();
        })?;
        let relief = Ticker::start(
            "droppedneedle-db-relief",
            tick,
            &self.shared,
            move |shared| {
                shared.relief_tick(&checkpoint);
            },
        )?;
        Ok(LaneWatch {
            tickers: vec![watch, relief],
        })
    }

    /// Run one synchronous closure inside one `IMMEDIATE` transaction.
    ///
    /// The closure receives the transaction and returns a value; commit
    /// follows a returned `Ok` if the transaction is still the lane's,
    /// rollback follows `Err`, an abort or a panic.
    /// The closure is plain `FnOnce`: holding the lock across an await is
    /// impossible because there is nothing to await with. It is queued on
    /// the first poll and runs on the lane's thread when its turn comes,
    /// whether or not this future is still polled.
    ///
    /// A background write waits for its turn. A foreground write is answered
    /// `Busy` when the foreground queue is full, or when it has waited past
    /// the limit, in which case it never runs.
    ///
    /// The closure returns the caller's own error type `E`, which rolls the
    /// transaction back. The lane's own failures (`Busy`, `TxTimeout`,
    /// `TransactionEnded`, `NestedWrite`, `LaneClosed`, a panic) reach the
    /// caller through `E::from`. After `TxTimeout` or `TransactionEnded`
    /// whatever the closure returned is discarded.
    pub async fn write<R, E, F>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Transaction) -> Result<R, E> + Send + 'static,
        R: Send + 'static,
        E: From<DbError> + std::fmt::Display + Send + 'static,
    {
        self.write_queued(lane, name, None, op)
            .await
            .map_err(TxError::into_caller)
    }

    /// [`WriteLane::write`], for a chunk too: `cancel` is the flag of the
    /// chunked write it belongs to.
    async fn write_queued<R, E, F>(
        &self,
        lane: Lane,
        name: &'static str,
        cancel: Option<CancelFlag>,
        op: F,
    ) -> Result<R, TxError<E>>
    where
        F: FnOnce(&Transaction) -> Result<R, E> + Send + 'static,
        R: Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let budget = self.shared.limits.budget(lane);
        let (reply, answer) = oneshot::channel();
        let write: AsyncWrite = Box::new(move |granted| {
            let answer = match granted {
                Ok(mut turn) => {
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        turn.transact(lane, budget, name, false, op)
                    }));
                    // The turn goes back before the caller hears, so a
                    // caller that was just answered finds the lane idle.
                    drop(turn);
                    match outcome {
                        Ok(outcome) => Answer::Ran(outcome),
                        Err(_) => {
                            tracing::error!(
                                operation = name,
                                "write closure panicked; rolled back"
                            );
                            Answer::Panicked
                        }
                    }
                }
                Err(refused) => Answer::Refused(refused),
            };
            let _ = reply.send(answer);
        });
        self.shared
            .enqueue(lane, name, false, cancel, Grantee::Async(write))?;
        match answer.await {
            Ok(Answer::Ran(outcome)) => finish(name, outcome),
            Ok(Answer::Panicked) => Err(TxError::Lane(DbError::WriteFailed {
                operation: name.to_owned(),
                cause: "the write closure panicked".to_owned(),
            })),
            Ok(Answer::Refused(refused)) => Err(TxError::Lane(refused.into_error(name))),
            Err(_) => Err(TxError::Lane(DbError::LaneClosed)),
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
    pub fn write_blocking<R, E, F>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Transaction) -> Result<R, E>,
        E: From<DbError> + std::fmt::Display,
    {
        self.write_parked(lane, name, false, None, op)
            .map_err(TxError::into_caller)
    }

    /// [`WriteLane::write_blocking`] whose commit is on disk before it
    /// returns. The connection runs at `synchronous=FULL` for this one
    /// transaction and goes back to `NORMAL` before the turn passes on, also
    /// when the closure fails or panics.
    ///
    /// The guarantee: when this returns `Ok` after changing at least one
    /// row, that commit and every commit before it have been synced to the
    /// WAL. It has two edges. A call that changes nothing writes no frame
    /// and syncs nothing, so it is not a barrier for earlier commits. And if
    /// the connection cannot be put back to `NORMAL`, it stays at `FULL`,
    /// which is the safe side, and the failure is logged.
    ///
    /// Each call costs one sync while the lane is held, so this is for rows
    /// that must outlive a power cut (a journal row before the file move it
    /// describes) and nothing else. The closure receives the transaction as
    /// a [`Durable`], which code that must only write durably asks for.
    ///
    /// A durable write is never answered `Busy` for waiting too long: it
    /// waits for its turn like a background write, in the queue `lane`
    /// names. The turn is never refused, but the write can still hear
    /// `Busy`: from the file lock at `BEGIN` while another connection
    /// writes, and past 4,096 durable writes waiting. Once shutdown starts,
    /// every new durable call is refused `LaneClosed` at once, and one
    /// still parked when shutdown gives up hears the same; a caller between
    /// two journal steps treats either as "stop here and recover at the
    /// next start".
    ///
    /// A durable write is never held by the WAL, so its lane has no bulk
    /// class.
    ///
    /// The time budget covers less of this write than of an ordinary one.
    /// Setting the pragma and `BEGIN` run before the abort clock starts. The
    /// sync at `COMMIT` runs inside it but cannot be interrupted, so a write
    /// stuck on a disk that does not answer is not aborted: it shows only in
    /// the log line that names a write holding the lane past twice its
    /// budget.
    pub fn write_durable<R, E, F>(
        &self,
        lane: DurableLane,
        name: &'static str,
        op: F,
    ) -> Result<R, E>
    where
        F: FnOnce(&Durable<'_>) -> Result<R, E>,
        E: From<DbError> + std::fmt::Display,
    {
        self.write_parked(lane.into(), name, true, None, |tx| op(&Durable::new(tx)))
            .map_err(TxError::into_caller)
    }

    /// Wait for the turn on the calling thread and run the closure there.
    fn write_parked<R, E, F>(
        &self,
        lane: Lane,
        name: &'static str,
        durable: bool,
        cancel: Option<CancelFlag>,
        op: F,
    ) -> Result<R, TxError<E>>
    where
        F: FnOnce(&Transaction) -> Result<R, E>,
        E: std::fmt::Display,
    {
        let budget = self.shared.limits.budget(lane);
        let (parked, granted) = mpsc::channel();
        self.shared
            .enqueue(lane, name, durable, cancel, Grantee::Blocking(parked))?;
        let mut turn = match granted.recv() {
            Ok(Ok(turn)) => turn,
            Ok(Err(refused)) => return Err(TxError::Lane(refused.into_error(name))),
            Err(_) => return Err(TxError::Lane(DbError::LaneClosed)),
        };
        let outcome = turn.transact(lane, budget, name, durable, op);
        drop(turn);
        finish(name, outcome)
    }

    /// Commit `items` in chunks of at most 500 rows. Each chunk is one bulk
    /// turn, so requests and job writes get in between, and while the WAL
    /// hold is on the next chunk waits: a long bulk job stops at a chunk
    /// boundary, never inside a transaction. Returns the rows committed.
    ///
    /// Each chunk commits on its own and the lane never undoes one, so any
    /// prefix of the chunks may be all that lands. A write that stops short
    /// answers [`ChunkedError`] with the rows that committed: `Failed` when
    /// a chunk failed and rolled back, `Cancelled` when the flag stopped it.
    ///
    /// A tripped flag lets the chunk that is running commit and stops the
    /// write there with `Cancelled`. The flag is read before each chunk is
    /// queued and again when its turn comes. A chunk that is waiting, behind
    /// the WAL hold or behind other writes, is not left to wait it out: on a
    /// lane with a watch, which the database runtime's lane has, the relief
    /// thread takes a cancelled chunk out of the queue within a tick (a
    /// second) and the write returns `Cancelled` without that chunk having
    /// run.
    ///
    /// Dropping the future instead of tripping the flag gets no answer: the
    /// chunk in flight still runs to commit or rollback and no later chunk
    /// starts. A caller that needs the count trips the flag and awaits.
    pub async fn write_chunked<T, E, F>(
        &self,
        name: &'static str,
        mut items: Vec<T>,
        chunk_rows: usize,
        cancel: &CancelFlag,
        chunk: F,
    ) -> Result<usize, ChunkedError<E>>
    where
        T: Send + 'static,
        E: From<DbError> + std::fmt::Display + Send + 'static,
        F: Fn(&[T], &Transaction) -> Result<(), E> + Send + Sync + 'static,
    {
        let chunk = Arc::new(chunk);
        let width = chunk_rows.clamp(1, BACKGROUND_CHUNK_ROWS);
        let mut committed = 0;
        while !items.is_empty() {
            if cancel.is_cancelled() {
                return Err(ChunkedError::Cancelled { rows: committed });
            }
            let take = width.min(items.len());
            let owned: Vec<T> = items.drain(..take).collect();
            let total = owned.len();
            let writer = Arc::clone(&chunk);
            let stop = cancel.clone();
            let ran = self
                .write_queued(Lane::Bulk, name, Some(cancel.clone()), move |tx| {
                    if stop.is_cancelled() {
                        return Ok(false);
                    }
                    writer(&owned, tx)?;
                    Ok(true)
                })
                .await;
            chunk_answer(ran, committed)?;
            committed += total;
        }
        Ok(committed)
    }

    /// [`WriteLane::write_chunked`] for synchronous code: each chunk waits
    /// for its turn on the calling thread and runs there, so `items` and the
    /// closure may borrow from the caller. Call it from a blocking thread.
    pub fn write_chunked_blocking<T, E, F>(
        &self,
        name: &'static str,
        items: &[T],
        chunk_rows: usize,
        cancel: &CancelFlag,
        mut chunk: F,
    ) -> Result<usize, ChunkedError<E>>
    where
        E: From<DbError> + std::fmt::Display,
        F: FnMut(&[T], &Transaction) -> Result<(), E>,
    {
        let width = chunk_rows.clamp(1, BACKGROUND_CHUNK_ROWS);
        let mut committed = 0;
        for rows in items.chunks(width) {
            if cancel.is_cancelled() {
                return Err(ChunkedError::Cancelled { rows: committed });
            }
            let ran = self.write_parked(Lane::Bulk, name, false, Some(cancel.clone()), |tx| {
                if cancel.is_cancelled() {
                    return Ok(false);
                }
                chunk(rows, tx)?;
                Ok(true)
            });
            chunk_answer(ran, committed)?;
            committed += rows.len();
        }
        Ok(committed)
    }

    /// Drain and stop: refuse new writes, let queued ones finish for up to
    /// `grace`, then close the connection and join the lane's thread. A
    /// write that still holds the lane at the bound is named in the log and
    /// left to end by itself; the writes queued behind it are answered
    /// `LaneClosed`. The first call closes the lane and a later one finds
    /// nothing to close, unless the first gave up on a write that is still
    /// running: a later call then waits for that write, up to `grace` again.
    pub async fn shutdown(&self, grace: Duration) {
        let shared = Arc::clone(&self.shared);
        let closed = tokio::task::spawn_blocking(move || shared.close(grace)).await;
        if !matches!(closed, Ok(true)) {
            tracing::error!("writer thread did not stop cleanly");
        }
    }
}

/// The handle of a lane's watch and relief threads. Stopping or dropping it
/// ends them.
#[derive(Debug)]
pub struct LaneWatch {
    tickers: Vec<Ticker>,
}

impl LaneWatch {
    /// Another thread that starts and stops with the watch.
    pub(crate) fn add(&mut self, ticker: Ticker) {
        self.tickers.push(ticker);
    }

    /// Stop the threads and wait for them. The watch thread is at most one
    /// pass of bookkeeping away from noticing, the relief thread at most one
    /// checkpoint pass, so call this from a thread that may block. That
    /// wait has no bound of its own: a pass stuck on a disk that does not
    /// answer holds it. The runtime's shutdown reclaims the WAL straight
    /// after and would be stuck on the same disk, so nothing waits here
    /// that would not wait there, and the process's own exit bound covers
    /// both.
    pub fn stop(self) {
        let (stops, threads): (Vec<_>, Vec<_>) = self
            .tickers
            .into_iter()
            .map(|ticker| (ticker.stop, ticker.thread))
            .unzip();
        drop(stops);
        for thread in threads {
            if thread.join().is_err() {
                tracing::error!("database watch thread did not stop cleanly");
            }
        }
    }
}

/// A thread that runs one pass over some shared state every tick, for as
/// long as that state is alive.
#[derive(Debug)]
pub(crate) struct Ticker {
    /// Never sent on: the thread ends when this is dropped.
    stop: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl Ticker {
    pub(crate) fn start<S: Send + Sync + 'static>(
        name: &'static str,
        tick: Duration,
        shared: &Arc<S>,
        pass: impl Fn(&Arc<S>) + Send + 'static,
    ) -> Result<Self, DbError> {
        let shared = Arc::downgrade(shared);
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(tick) {
                    let Some(shared) = shared.upgrade() else {
                        return;
                    };
                    // A pass can run a caller's code: it drops the closures
                    // of the writes it refuses. One that panics must not end
                    // the thread.
                    if catch_unwind(AssertUnwindSafe(|| pass(&shared))).is_err() {
                        tracing::error!(
                            thread = name,
                            "database watch pass panicked; it carries on"
                        );
                    }
                }
            })
            .map_err(DbError::Io)?;
        Ok(Self { stop, thread })
    }
}

/// The lane's thread: run each granted async write. The write drops its
/// turn itself, which passes the lane on.
fn run_async_writes(granted: mpsc::Receiver<Granted>) {
    for Granted { write, turn } in granted {
        write(Ok(turn));
    }
}

/// What one chunk's write means for the chunked write: carry on after a
/// chunk that committed, or stop with the rows committed before it. A chunk
/// that saw the flag commits nothing and is not counted.
fn chunk_answer<E: From<DbError>>(
    ran: Result<bool, TxError<E>>,
    rows: usize,
) -> Result<(), ChunkedError<E>> {
    match ran {
        Ok(true) => Ok(()),
        Ok(false) | Err(TxError::Lane(DbError::Cancelled { .. })) => {
            Err(ChunkedError::Cancelled { rows })
        }
        Err(error) => Err(ChunkedError::Failed {
            rows,
            error: error.into_caller(),
        }),
    }
}

/// Map one transaction's outcome for its caller.
fn finish<R, E>(name: &'static str, outcome: Outcome<R, E>) -> Result<R, TxError<E>> {
    let lane = match outcome.value {
        Ok(value) => return Ok(value),
        Err(Failure::Op(error)) => return Err(TxError::Op(error)),
        Err(Failure::TimedOut) => DbError::TxTimeout {
            operation: name.to_owned(),
            changes: outcome.changes,
        },
        Err(Failure::Ended(_)) => DbError::TransactionEnded {
            operation: name.to_owned(),
        },
        Err(Failure::Connection(error)) => error,
        Err(Failure::Sql(error)) => map_write_error(name, outcome.changes, error),
    };
    Err(TxError::Lane(lane))
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

/// Prepared statements the write connection keeps. A scan window runs a few
/// dozen distinct statements per file; rusqlite's default of 16 would
/// prepare most of them again for every file.
const STATEMENT_CACHE: usize = 128;

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
        connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
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
    fn transact<R, E: std::fmt::Display>(
        &mut self,
        budget: Budget,
        name: &'static str,
        durable: bool,
        op: impl FnOnce(&Transaction) -> Result<R, E>,
    ) -> Outcome<R, E> {
        let started = Instant::now();
        if let Err(error) = self.start_in_autocommit() {
            return Outcome {
                value: Err(Failure::Connection(error)),
                changes: 0,
                elapsed: started.elapsed(),
            };
        }
        let before = self.connection.total_changes();
        let (clock, watch, lane) = (&self.clock, &self.watch, self.lane);
        let value = if durable {
            // The guard is dropped when this arm ends or unwinds, after the
            // transaction inside it has committed or rolled back.
            match FullSync::set(&mut self.connection, name) {
                Ok(full) => {
                    run_transaction(&mut *full.connection, clock, budget.abort, watch, lane, op)
                }
                Err(error) => Err(Failure::Sql(error)),
            }
        } else {
            run_transaction(&mut self.connection, clock, budget.abort, watch, lane, op)
        };
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
            // A durable turn whose rollback failed could not leave full sync
            // either. A durable turn sets it again right after this.
            if let Err(error) = self.connection.execute_batch("PRAGMA synchronous=NORMAL") {
                tracing::error!(
                    cause = short_cause(&error),
                    "write connection could not leave full sync; every commit syncs until it does"
                );
            }
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

/// The write connection at `synchronous=FULL` for one durable transaction.
/// Dropping it puts `NORMAL` back. SQLite refuses to change the level
/// inside a transaction (verified against SQLite 3.46.0), so both changes
/// happen outside the lane's `BEGIN` and commit.
struct FullSync<'a> {
    connection: &'a mut Connection,
    operation: &'static str,
}

impl<'a> FullSync<'a> {
    fn set(connection: &'a mut Connection, operation: &'static str) -> rusqlite::Result<Self> {
        connection.execute_batch("PRAGMA synchronous=FULL")?;
        Ok(Self {
            connection,
            operation,
        })
    }
}

impl Drop for FullSync<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.connection.execute_batch("PRAGMA synchronous=NORMAL") {
            tracing::error!(
                operation = self.operation,
                cause = short_cause(&error),
                "write connection could not leave full sync; every commit syncs until it does"
            );
        }
    }
}

/// Run the closure between the lane's `BEGIN IMMEDIATE` and the lane's
/// commit. The abort and the transaction's own fate are judged first:
/// either one discards what the closure returned.
fn run_transaction<R, E: std::fmt::Display>(
    connection: &mut Connection,
    clock: &AbortClock,
    abort_after: Duration,
    watch: &TxWatch,
    lane: LaneId,
    op: impl FnOnce(&Transaction) -> Result<R, E>,
) -> Result<R, Failure<E>> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(Failure::Sql)?;
    // Armed after `BEGIN`: the budget counts the work, not the wait for the
    // file lock.
    let armed = clock.arm(abort_after);
    watch.begin();
    let value = {
        let _inside = scope::enter_write(lane);
        op(&transaction)
    };
    if armed.fired() {
        return Err(Failure::TimedOut);
    }
    if watch.rolled_back() || transaction.is_autocommit() {
        return Err(Failure::Ended(value.err().map(|error| error.to_string())));
    }
    let value = value.map_err(Failure::Op)?;
    let _allowed = watch.allow_commit();
    transaction.commit().map_err(Failure::Sql)?;
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
