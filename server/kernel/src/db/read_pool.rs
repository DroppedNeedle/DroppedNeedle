//! The read pools: two fixed sets of read-only connections.
//!
//! Request reads share one pool, long reads (a scan window, an export) have
//! a small one of their own, so a long read can never take the connection a
//! request needs. The connections open at boot, after the writer exists, and
//! stay open: there is no reaping, no health check and no resizing, because
//! a SQLite connection to a local file does not go stale.
//!
//! One closure is one checkout and one snapshot. The pool wraps each closure
//! in a deferred transaction, so its statements see one state, and holds the
//! connection for the closure's whole length. That is why a request read
//! runs one statement per closure: a page of three statements queues three
//! short times, and a cheap read never waits behind the whole of an
//! expensive one.
//!
//! Each pool has one queue, in arrival order, shared by async and blocking
//! callers. A waiter holds nothing: no connection, no thread, no snapshot. A
//! returned connection goes straight to the front waiter, so the idle list
//! is only ever non-empty when nobody waits and nobody can jump the queue.
//! The pool's mutex guards bookkeeping only and is never held while a
//! closure runs or a waiter is answered.
//!
//! The pool holds no runtime handle. Returning a connection completes a
//! channel and nothing else, so it is safe from any thread and from any
//! drop, including the drop of a blocking task a runtime never ran.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak, mpsc},
    time::{Duration, Instant},
};

use rusqlite::{Connection, OpenFlags, TransactionState};
use tokio::sync::oneshot;

use super::{
    error::DbError,
    factory::{CACHE_SIZE_KIB, MMAP_SIZE},
    scope::{self, LaneId},
    writer::{CancelFlag, HOLDER_LOG_EVERY},
};

/// Connections for long reads. Two, so a scan and an export can run side by
/// side, and apart from the request pool, so neither can take a connection a
/// request is waiting for.
pub const LONG_READERS: usize = 2;
/// Prepared statements kept per connection. Hot reads use `prepare_cached`.
const STATEMENT_CACHE: usize = 128;
/// How often a long read that is waiting for a connection looks at its
/// cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// The id of no checkout: a connection that comes to the pool from the pass,
/// which opened it, and not back from a read.
const NO_CHECKOUT: u64 = u64::MAX;

/// Why a waiting read was answered without a connection.
#[derive(Debug, Clone, Copy)]
enum Refused {
    /// A request read waited past the limit.
    Busy,
    /// The pool closed.
    Closed,
}

impl Refused {
    fn into_error(self, name: &'static str) -> DbError {
        match self {
            Self::Busy => DbError::Busy {
                operation: name.to_owned(),
            },
            Self::Closed => DbError::LaneClosed,
        }
    }
}

type Answer = Result<Checkout, Refused>;

/// Where a waiter's connection is sent.
enum Grantee {
    /// A future in `read`.
    Async(oneshot::Sender<Answer>),
    /// A thread parked in `read_blocking` or `read_long`.
    Blocking(mpsc::Sender<Answer>),
}

struct Waiter {
    id: u64,
    name: &'static str,
    queued_at: Instant,
    grantee: Grantee,
}

impl Waiter {
    /// Answer the waiter. A waiter that is gone hands the answer back.
    fn answer(self, answer: Answer) -> Option<Answer> {
        match self.grantee {
            Grantee::Async(waiting) => waiting.send(answer).err(),
            Grantee::Blocking(parked) => parked.send(answer).err().map(|unsent| unsent.0),
        }
    }
}

/// A connection that is out of the pool.
struct Out {
    id: u64,
    name: &'static str,
    since: Instant,
    /// When the pool's pass last named it in the log.
    logged_at: Option<Instant>,
}

/// The pool's bookkeeping. Locked briefly, never while a closure runs.
struct State {
    /// Last in, first out: the warmest connection goes out next.
    idle: Vec<Connection>,
    waiters: VecDeque<Waiter>,
    out: Vec<Out>,
    /// Connections that had to be closed and could not be opened again. The
    /// pool's pass tries again every tick; until it succeeds the pool is
    /// that much smaller and callers queue for the connections left.
    missing: usize,
    closed: bool,
    next_id: u64,
}

impl State {
    fn check_out(&mut self, id: u64, name: &'static str) {
        self.out.push(Out {
            id,
            name,
            since: Instant::now(),
            logged_at: None,
        });
    }

    fn check_in(&mut self, id: u64) {
        if let Some(at) = self.out.iter().position(|out| out.id == id) {
            self.out.swap_remove(at);
        }
    }
}

/// One pool of read-only connections and the queue in front of it.
struct Pool {
    label: &'static str,
    /// The database these connections read, as the per-thread record of
    /// open closures knows it.
    database: LaneId,
    path: PathBuf,
    busy_timeout: Duration,
    /// How long a waiter waits before it is answered `Busy`. `None` for the
    /// long pool, whose waiters wait until served or cancelled.
    wait_limit: Option<Duration>,
    state: Mutex<State>,
    /// Signalled when a connection comes back to a closed pool.
    returned: Condvar,
}

/// What a caller found when it asked for a connection.
enum Admitted<W> {
    Now(Checkout),
    Queued(u64, W),
}

impl Pool {
    fn open(
        label: &'static str,
        database: LaneId,
        path: &Path,
        size: usize,
        busy_timeout: Duration,
        wait_limit: Option<Duration>,
    ) -> Result<Arc<Self>, DbError> {
        let idle = (0..size)
            .map(|_| open_reader(path, busy_timeout))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(Self {
            label,
            database,
            path: path.to_owned(),
            busy_timeout,
            wait_limit,
            state: Mutex::new(State {
                idle,
                waiters: VecDeque::new(),
                out: Vec::with_capacity(size),
                missing: 0,
                closed: false,
                next_id: 0,
            }),
            returned: Condvar::new(),
        }))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take an idle connection, or join the back of the queue as the waiter
    /// `wait` makes.
    fn admit<W>(
        self: &Arc<Self>,
        name: &'static str,
        wait: impl FnOnce() -> (Grantee, W),
    ) -> Result<Admitted<W>, DbError> {
        // A read inside a write closure would see the state from before the
        // transaction. One inside a read closure would see a second
        // snapshot, and readers that each wait for a second connection
        // stall each other.
        if scope::inside_write_on(self.database) {
            return Err(DbError::ReadInsideWrite {
                operation: name.to_owned(),
            });
        }
        if scope::inside_read_on(self.database) {
            return Err(DbError::InsideRead {
                operation: name.to_owned(),
            });
        }
        let mut state = self.state();
        if state.closed {
            return Err(DbError::LaneClosed);
        }
        let id = state.next_id;
        state.next_id += 1;
        if let Some(connection) = state.idle.pop() {
            state.check_out(id, name);
            return Ok(Admitted::Now(self.checkout(connection, id, name)));
        }
        let (grantee, waiting) = wait();
        state.waiters.push_back(Waiter {
            id,
            name,
            queued_at: Instant::now(),
            grantee,
        });
        Ok(Admitted::Queued(id, waiting))
    }

    fn checkout(self: &Arc<Self>, connection: Connection, id: u64, name: &'static str) -> Checkout {
        Checkout {
            pool: Arc::clone(self),
            connection: Some(connection),
            id,
            name,
        }
    }

    /// A connection for a future: wait as a future, holding nothing.
    async fn acquire(self: &Arc<Self>, name: &'static str) -> Result<Checkout, DbError> {
        let granted = match self.admit(name, || {
            let (waiting, granted) = oneshot::channel();
            (Grantee::Async(waiting), granted)
        })? {
            Admitted::Now(checkout) => return Ok(checkout),
            Admitted::Queued(_, granted) => granted,
        };
        // No timer here: the pool's pass answers a waiter that is overdue,
        // on real time, so a runtime whose clock is paused changes nothing.
        match granted.await {
            Ok(Ok(checkout)) => Ok(checkout),
            Ok(Err(refused)) => Err(refused.into_error(name)),
            Err(_) => Err(DbError::LaneClosed),
        }
    }

    /// A connection for a parked thread. A request waiter times itself and
    /// hears `Busy` at the limit; a waiter with a cancel flag looks at it
    /// between waits and hears `ReadCancelled`.
    fn acquire_blocking(
        self: &Arc<Self>,
        name: &'static str,
        cancel: Option<&CancelFlag>,
    ) -> Result<Checkout, DbError> {
        let (id, granted) = match self.admit(name, || {
            let (parked, granted) = mpsc::channel();
            (Grantee::Blocking(parked), granted)
        })? {
            Admitted::Now(checkout) => return Ok(checkout),
            Admitted::Queued(id, granted) => (id, granted),
        };
        let queued_at = Instant::now();
        loop {
            let left = self
                .wait_limit
                .map(|limit| limit.saturating_sub(queued_at.elapsed()));
            let wait = match (left, cancel) {
                (Some(left), Some(_)) => left.min(CANCEL_POLL),
                (Some(left), None) => left,
                (None, _) => CANCEL_POLL,
            };
            match granted.recv_timeout(wait) {
                Ok(Ok(checkout)) => return Ok(checkout),
                Ok(Err(refused)) => return Err(refused.into_error(name)),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(DbError::LaneClosed),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let gave_up = if cancel.is_some_and(CancelFlag::is_cancelled) {
                DbError::ReadCancelled {
                    operation: name.to_owned(),
                }
            } else if self
                .wait_limit
                .is_some_and(|limit| queued_at.elapsed() >= limit)
            {
                DbError::Busy {
                    operation: name.to_owned(),
                }
            } else {
                continue;
            };
            if self.forget(id) {
                return Err(gave_up);
            }
            // Not in the queue any more: an answer was on its way as this
            // waiter gave up. Whoever took it out sends at once.
            return match granted.recv() {
                Ok(Ok(checkout)) => Ok(checkout),
                Ok(Err(refused)) => Err(refused.into_error(name)),
                Err(_) => Err(DbError::LaneClosed),
            };
        }
    }

    /// Take a waiter that gave up out of the queue. False if it is no longer
    /// there.
    fn forget(&self, id: u64) -> bool {
        let mut state = self.state();
        let at = state.waiters.iter().position(|waiter| waiter.id == id);
        at.and_then(|at| state.waiters.remove(at)).is_some()
    }

    /// Take a connection back from the checkout `id` and pass it to the
    /// front waiter. A waiter that is gone hands it straight back and the
    /// loop tries the next one, so a return never recurses. `None` is a
    /// connection that was closed and could not be replaced.
    fn give_back(self: &Arc<Self>, mut returning: Option<Connection>, mut id: u64) {
        loop {
            let (waiter, connection) = {
                let mut state = self.state();
                if state.closed {
                    drop(state);
                    // Closed here, by the thread that brought it back, and
                    // only then counted as back: shutdown waits for it.
                    drop(returning.take());
                    self.state().check_in(id);
                    self.returned.notify_all();
                    return;
                }
                state.check_in(id);
                let Some(connection) = returning.take() else {
                    state.missing += 1;
                    return;
                };
                let Some(waiter) = state.waiters.pop_front() else {
                    state.idle.push(connection);
                    return;
                };
                state.check_out(waiter.id, waiter.name);
                (waiter, connection)
            };
            let granted = self.checkout(connection, waiter.id, waiter.name);
            id = waiter.id;
            match waiter.answer(Ok(granted)) {
                None => return,
                Some(unsent) => {
                    returning = unsent
                        .ok()
                        .and_then(|mut checkout| checkout.connection.take());
                }
            }
        }
    }

    /// Make sure a connection carries no transaction to its next caller. One
    /// that will not leave its transaction is closed and replaced.
    ///
    /// Autocommit alone does not say so. SQLite keeps the read transaction
    /// open for as long as any statement on the connection is still being
    /// stepped, and `COMMIT` succeeds and turns autocommit back on all the
    /// same; rusqlite puts a cached statement back without resetting it. So
    /// a closure that leaked a cursor would hand its snapshot to every later
    /// read here and pin the WAL for good. The transaction state sees it.
    /// A closure that leaks the `Statement` itself, not only its rows,
    /// leaves a handle that cannot be closed: no later read sees its
    /// snapshot, but the WAL stays pinned until the process ends.
    /// Verified against SQLite 3.46.0 (rusqlite 0.32.1).
    fn cleaned(&self, connection: Connection, name: &'static str) -> Option<Connection> {
        let ended = if connection.is_autocommit() {
            Ok(0)
        } else {
            connection
                .prepare_cached("COMMIT")
                .and_then(|mut commit| commit.execute([]))
        };
        if connection.is_autocommit()
            && matches!(
                connection.transaction_state(None),
                Ok(TransactionState::None)
            )
        {
            return Some(connection);
        }
        tracing::error!(
            operation = name,
            pool = self.label,
            cause = ended.err().map(|error| short_cause(&error)),
            "a pooled read left a transaction open; replacing its connection"
        );
        drop(connection);
        match open_reader(&self.path, self.busy_timeout) {
            Ok(connection) => Some(connection),
            Err(error) => {
                tracing::error!(
                    pool = self.label,
                    %error,
                    "a read connection could not be opened again"
                );
                None
            }
        }
    }

    /// The waiter can no longer be served: its future was dropped.
    fn is_gone(waiter: &Waiter) -> bool {
        matches!(&waiter.grantee, Grantee::Async(waiting) if waiting.is_closed())
    }

    /// The waiter is a future that has waited past the limit. A parked
    /// thread times itself.
    fn is_overdue(&self, waiter: &Waiter) -> bool {
        matches!(waiter.grantee, Grantee::Async(_))
            && self
                .wait_limit
                .is_some_and(|limit| waiter.queued_at.elapsed() >= limit)
    }

    /// One pass, on real time: answer the futures that have waited past the
    /// limit, forget the ones whose caller is gone, name a read that has
    /// held its connection for 30 s, then every 30 s, and try once to open
    /// a connection the pool is missing. A parked thread times itself and
    /// is left alone.
    fn pass(self: &Arc<Self>) {
        let (gone, overdue, held, missing) = {
            let mut state = self.state();
            let (mut gone, mut overdue) = (Vec::new(), Vec::new());
            let done = |waiter: &Waiter| Self::is_gone(waiter) || self.is_overdue(waiter);
            if state.waiters.iter().any(done) {
                for waiter in std::mem::take(&mut state.waiters) {
                    if Self::is_gone(&waiter) {
                        gone.push(waiter);
                    } else if self.is_overdue(&waiter) {
                        overdue.push(waiter);
                    } else {
                        state.waiters.push_back(waiter);
                    }
                }
            }
            let mut held = Vec::new();
            for out in &mut state.out {
                let age = out.since.elapsed();
                let due = age >= HOLDER_LOG_EVERY
                    && out
                        .logged_at
                        .is_none_or(|at| at.elapsed() >= HOLDER_LOG_EVERY);
                if due {
                    out.logged_at = Some(Instant::now());
                    held.push((out.name, age));
                }
            }
            (gone, overdue, held, state.missing > 0 && !state.closed)
        };
        drop(gone);
        // Logged before anyone is answered.
        for (operation, age) in held {
            tracing::warn!(
                operation,
                pool = self.label,
                held_ms = age.as_millis() as u64,
                "a read has held its connection for a long time; it pins the WAL"
            );
        }
        if let Some(oldest) = overdue.first() {
            tracing::warn!(
                refused = overdue.len(),
                oldest = oldest.name,
                pool = self.label,
                "reads waited past their limit for a connection; answering busy"
            );
        }
        for waiter in overdue {
            drop(waiter.answer(Err(Refused::Busy)));
        }
        // Only this pass reopens, one connection a tick, so two threads
        // never open the same one. The failure that made it missing was
        // logged then; a tick that fails again says nothing.
        if missing && let Ok(connection) = open_reader(&self.path, self.busy_timeout) {
            self.state().missing -= 1;
            self.give_back(Some(connection), NO_CHECKOUT);
            tracing::info!(pool = self.label, "a read connection was opened again");
        }
    }

    /// Refuse new reads, answer every waiter and close the idle connections.
    fn shut(&self) {
        let (idle, unserved) = {
            let mut state = self.state();
            state.closed = true;
            let idle = std::mem::take(&mut state.idle);
            let unserved: Vec<Waiter> = state.waiters.drain(..).collect();
            (idle, unserved)
        };
        drop(idle);
        for waiter in unserved {
            drop(waiter.answer(Err(Refused::Closed)));
        }
    }

    /// Wait until `deadline` for the connections of a shut pool that are
    /// still out, and name the reads that hold one at the end. One that
    /// comes back later is closed by the thread returning it.
    fn wait_for_readers(&self, deadline: Option<Instant>) {
        let mut state = self.state();
        while !state.out.is_empty() {
            let Some(deadline) = deadline else {
                state = self
                    .returned
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .returned
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let stuck: Vec<_> = state
            .out
            .iter()
            .map(|out| (out.name, out.since.elapsed()))
            .collect();
        drop(state);
        for (operation, held) in stuck {
            tracing::error!(
                operation,
                pool = self.label,
                held_ms = held.as_millis() as u64,
                "shutdown gave up on a read that still holds its connection"
            );
        }
    }

    fn oldest(&self) -> Option<OldestRead> {
        let state = self.state();
        let oldest = state.out.iter().min_by_key(|out| out.since)?;
        Some(OldestRead {
            name: oldest.name,
            pool: self.label,
            age: oldest.since.elapsed(),
        })
    }
}

/// One connection out of its pool. Dropping it, for any reason, ends the
/// read transaction and passes the connection on, so one cannot be lost.
struct Checkout {
    pool: Arc<Pool>,
    connection: Option<Connection>,
    id: u64,
    name: &'static str,
}

impl Checkout {
    /// Run one closure inside one deferred transaction: in WAL mode the
    /// snapshot is taken at its first statement and held to its end.
    fn run<R, E>(self, op: impl FnOnce(&Connection) -> Result<R, E>) -> Result<R, E>
    where
        E: From<DbError>,
    {
        let Some(connection) = self.connection.as_ref() else {
            return Err(DbError::LaneClosed.into());
        };
        connection
            .prepare_cached("BEGIN DEFERRED")
            .and_then(|mut begin| begin.execute([]))
            .map_err(DbError::Rusqlite)?;
        let _inside = scope::enter_read(self.pool.database);
        op(connection)
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let connection = self.pool.cleaned(connection, self.name);
            self.pool.give_back(connection, self.id);
        }
    }
}

/// The read that has held a pooled connection longest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OldestRead {
    /// The read's name.
    pub name: &'static str,
    /// Which pool its connection is from.
    pub pool: &'static str,
    /// How long it has held the connection.
    pub age: Duration,
}

/// What the read pools are doing, for the checkpoint's reclaim gate and the
/// lane's line about held bulk writes. It keeps neither pool alive.
#[derive(Debug, Clone)]
pub struct ReadActivity {
    pools: Weak<ReadPools>,
}

impl ReadActivity {
    fn each<T>(&self, of: impl Fn(&Pool) -> T) -> Vec<T> {
        self.pools
            .upgrade()
            .map(|pools| vec![of(&pools.requests), of(&pools.long)])
            .unwrap_or_default()
    }

    /// Pooled connections that are out, across both pools.
    pub fn out(&self) -> usize {
        self.each(|pool| pool.state().out.len()).into_iter().sum()
    }

    /// Callers waiting for a connection, across both pools.
    pub fn waiting(&self) -> usize {
        self.each(|pool| pool.state().waiters.len())
            .into_iter()
            .sum()
    }

    /// True when every pooled connection is back in its pool.
    pub fn is_idle(&self) -> bool {
        self.out() == 0
    }

    /// The read that has held its connection longest, if any is out.
    pub fn oldest(&self) -> Option<OldestRead> {
        self.each(Pool::oldest)
            .into_iter()
            .flatten()
            .max_by_key(|read| read.age)
    }
}

/// The request pool and the long pool of one database.
pub(crate) struct ReadPools {
    requests: Arc<Pool>,
    long: Arc<Pool>,
}

impl ReadPools {
    /// Open both pools on an existing database file, whose writer is already
    /// open. `wait` is how long a request read waits for a connection.
    pub(crate) fn open(
        path: &Path,
        database: LaneId,
        readers: usize,
        busy_timeout: Duration,
        wait: Duration,
    ) -> Result<Arc<Self>, DbError> {
        Ok(Arc::new(Self {
            requests: Pool::open(
                "requests",
                database,
                path,
                readers.max(1),
                busy_timeout,
                Some(wait),
            )?,
            long: Pool::open("long", database, path, LONG_READERS, busy_timeout, None)?,
        }))
    }

    pub(crate) fn activity(self: &Arc<Self>) -> ReadActivity {
        ReadActivity {
            pools: Arc::downgrade(self),
        }
    }

    /// The pools' pass, once a watch tick.
    pub(crate) fn pass(&self) {
        self.requests.pass();
        self.long.pass();
    }

    /// Close both pools, waiting until `deadline` for reads still running.
    /// Both are shut before either is waited for, so no read is admitted
    /// and no waiter is left parked while the other pool's readers finish.
    /// Blocking.
    pub(crate) fn close(&self, deadline: Option<Instant>) {
        self.requests.shut();
        self.long.shut();
        self.requests.wait_for_readers(deadline);
        self.long.wait_for_readers(deadline);
    }

    pub(crate) async fn read<R, E, F>(&self, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Connection) -> Result<R, E> + Send + 'static,
        R: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        let checkout = self.requests.acquire(name).await?;
        // Started from this task, never from a drop. If the runtime is going
        // down and drops the closure unrun, the checkout inside it passes
        // its connection on. A closure that has started is the runtime's to
        // wait for: dropping a runtime joins its blocking threads.
        match tokio::task::spawn_blocking(move || checkout.run(op)).await {
            Ok(result) => result,
            Err(stopped) if stopped.is_panic() => {
                tracing::error!(operation = name, "read closure panicked");
                Err(DbError::ReadPanicked {
                    operation: name.to_owned(),
                }
                .into())
            }
            Err(_) => Err(DbError::LaneClosed.into()),
        }
    }

    pub(crate) fn read_blocking<R, E, F>(&self, name: &'static str, op: F) -> Result<R, E>
    where
        F: FnOnce(&Connection) -> Result<R, E>,
        E: From<DbError>,
    {
        self.requests.acquire_blocking(name, None)?.run(op)
    }

    pub(crate) fn read_long<R, E, F>(
        &self,
        name: &'static str,
        cancel: &CancelFlag,
        op: F,
    ) -> Result<R, E>
    where
        F: FnOnce(&Connection) -> Result<R, E>,
        E: From<DbError>,
    {
        self.long.acquire_blocking(name, Some(cancel))?.run(op)
    }
}

/// Open one read-only connection. No pragma that only matters to a
/// connection that commits is set: `synchronous`, `wal_autocheckpoint`,
/// `foreign_keys`. The connection is used by one thread at a time (it is
/// `Send` and not `Sync`), so SQLite's own mutex is left out.
fn open_reader(path: &Path, busy_timeout: Duration) -> Result<Connection, DbError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(busy_timeout)?;
    connection.execute_batch(&format!(
        "PRAGMA query_only=ON;
         PRAGMA mmap_size={MMAP_SIZE};
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size={CACHE_SIZE_KIB};"
    ))?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(connection)
}

/// One line of cause without SQL text or a path.
fn short_cause(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(code, _) => format!("sqlite error {code:?}"),
        other => other.to_string().chars().take(120).collect(),
    }
}
