//! The loop supervisor: every background loop in the server runs on it.
//!
//! A loop is a name, a pacing [`Schedule`] and a tick. The supervisor owns
//! everything around the tick: the boot hold-off, jitter, backoff while
//! ticks fail, the heartbeat, the liveness row in `durable_loops`, panic
//! capture, and the stop. One [`CancellationToken`] stops everything; each
//! loop runs under a child of it, so one loop can be cancelled alone.
//!
//! - [`Supervisor::periodic`] ticks after the hold-off, then after every
//!   `next_delay`.
//! - [`Supervisor::on_wakeup`] ticks when its watch channel changes, when
//!   the schedule's interval (the idle ceiling) passes, or after a backoff
//!   when the last tick failed.
//! - [`Supervisor::one_shot`] runs once under a name; a second start while
//!   the name is live answers [`AlreadyRunning`].
//!
//! The blocking variants run each tick on a blocking thread, for ticks
//! that work synchronously against SQLite.
//!
//! A tick error is logged with the loop's name and counts toward backoff;
//! its fixed code is written to the row, never the error's text. A panic
//! marks the row failed and frees the name, so the next start runs clean.
//! Settings are read inside the tick, never captured when the loop starts.
//!
//! Rows are written by one writer task that keeps the latest row per loop
//! and flushes them together as a background write. Loops never wait on
//! the database: a busy write is retried later, and a slow lane never
//! delays a tick. The rows are cleared at every start.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::schedule::{Schedule, SplitMix64};
use crate::clock::Clock;
use crate::db::durable::{self, JobRegistryRows, JobState, LoopRecord};
use crate::db::{Db, DbError};

/// The least time the last rows get at shutdown, whatever the loops used
/// of the grace.
const LAST_ROWS_BOUND: Duration = Duration::from_secs(1);

/// Row code of a loop that panicked.
pub const PANICKED: &str = "PANICKED";
/// Row code of a loop still running when its grace ran out.
pub const ABORTED: &str = "ABORTED";

/// What a tick asks of its loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Wait for the next turn.
    Continue,
    /// End the loop cleanly (a plugin that was turned off).
    Finish,
}

/// A failed tick: a fixed code for the row, a detail for the log only.
#[derive(Debug, Clone)]
pub struct TickError {
    code: &'static str,
    detail: String,
}

impl TickError {
    /// A failure with its row code and the cause to log.
    pub fn new(code: &'static str, detail: impl fmt::Display) -> Self {
        Self {
            code,
            detail: detail.to_string(),
        }
    }

    /// The code written to the row.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for TickError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

/// What one tick returns.
pub type TickResult = Result<Next, TickError>;

/// One turn of a loop. State a loop keeps between turns lives on `self`.
pub trait Tick: Send + 'static {
    /// Run one turn.
    fn tick(&mut self, ctx: &TickCtx) -> impl Future<Output = TickResult> + Send;
}

/// A closure as a tick, for loops that keep no state between turns.
pub struct FnTick<F>(pub F);

impl<F, Fut> Tick for FnTick<F>
where
    F: FnMut(TickCtx) -> Fut + Send + 'static,
    Fut: Future<Output = TickResult> + Send,
{
    fn tick(&mut self, ctx: &TickCtx) -> impl Future<Output = TickResult> + Send {
        (self.0)(ctx.clone())
    }
}

/// A synchronous tick run on a blocking thread. A panic there carries on
/// into the loop's task, where it marks the row failed.
struct BlockingTick<F>(Option<F>);

impl<F> Tick for BlockingTick<F>
where
    F: FnMut(&TickCtx) -> TickResult + Send + 'static,
{
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let Some(mut work) = self.0.take() else {
            return Ok(Next::Finish);
        };
        let turn = ctx.clone();
        match tokio::task::spawn_blocking(move || {
            let result = work(&turn);
            (work, result)
        })
        .await
        {
            Ok((work, result)) => {
                self.0 = Some(work);
                result
            }
            Err(join) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
            Err(_) => Ok(Next::Finish),
        }
    }
}

/// What a running tick sees: its loop's name and stop, and its heartbeat.
#[derive(Clone)]
pub struct TickCtx {
    name: Arc<str>,
    token: CancellationToken,
    book: Arc<RowBook>,
}

impl TickCtx {
    /// The loop's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Cancelled when the loop must stop. Long ticks select on it.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Record a heartbeat now, for ticks that run long.
    pub fn heartbeat(&self) {
        self.book.beat(&self.name);
    }

    /// Sleep unless the loop is stopped first. False once it is stopped.
    pub async fn sleep(&self, delay: Duration) -> bool {
        sleep_unless(&self.token, delay).await
    }
}

impl fmt::Debug for TickCtx {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TickCtx")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Starting failed because the name is already live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyRunning;

impl fmt::Display for AlreadyRunning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("job is already running")
    }
}

impl std::error::Error for AlreadyRunning {}

struct Live {
    id: u64,
    token: CancellationToken,
    handle: JoinHandle<()>,
}

struct Inner {
    clock: Arc<dyn Clock>,
    token: CancellationToken,
    runtime: Handle,
    live: Mutex<HashMap<String, Live>>,
    next_id: AtomicU64,
    book: Arc<RowBook>,
    writer_stop: CancellationToken,
    writer: Mutex<Option<JoinHandle<()>>>,
}

/// The one loop supervisor. Cloneable; clones share everything.
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Inner>,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Supervisor").finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Clear the rows of the last start and begin writing this start's.
    /// Cancelling `token` stops every loop.
    pub async fn start(
        db: Db,
        clock: Arc<dyn Clock>,
        token: CancellationToken,
    ) -> Result<Self, DbError> {
        durable::clear_loops(&db).await?;
        let book = Arc::new(RowBook::new(Arc::clone(&clock)));
        let writer_stop = CancellationToken::new();
        let runtime = Handle::current();
        let writer = runtime.spawn(write_rows(db, Arc::clone(&book), writer_stop.clone()));
        Ok(Self {
            inner: Arc::new(Inner {
                clock,
                token,
                runtime,
                live: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                book,
                writer_stop,
                writer: Mutex::new(Some(writer)),
            }),
        })
    }

    /// The wall clock the supervisor stamps rows with.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.inner.clock
    }

    /// The token that stops every loop.
    pub fn token(&self) -> &CancellationToken {
        &self.inner.token
    }

    /// Rows for loops that still write their own liveness. Only a started
    /// supervisor hands these out, so none is written before the start
    /// clears the table.
    pub fn job_rows(&self) -> JobRegistryRows {
        JobRegistryRows::new(Arc::clone(&self.inner.clock))
    }

    /// Tick on `schedule` until stopped.
    pub fn periodic(
        &self,
        name: impl Into<String>,
        schedule: Schedule,
        tick: impl Tick,
    ) -> Result<(), AlreadyRunning> {
        self.launch(name.into(), move |ctx| run_periodic(ctx, schedule, tick))
    }

    /// [`Supervisor::periodic`] with a synchronous tick.
    pub fn periodic_blocking<F>(
        &self,
        name: impl Into<String>,
        schedule: Schedule,
        tick: F,
    ) -> Result<(), AlreadyRunning>
    where
        F: FnMut(&TickCtx) -> TickResult + Send + 'static,
    {
        self.periodic(name, schedule, BlockingTick(Some(tick)))
    }

    /// Tick when `wake` changes, at the schedule's interval when nothing
    /// does, and after a backoff when the last tick failed. A receiver that
    /// has already changed ticks at once. The loop ends when every sender
    /// is gone.
    pub fn on_wakeup(
        &self,
        name: impl Into<String>,
        wake: watch::Receiver<u64>,
        schedule: Schedule,
        tick: impl Tick,
    ) -> Result<(), AlreadyRunning> {
        self.launch(name.into(), move |ctx| {
            run_on_wakeup(ctx, wake, schedule, tick)
        })
    }

    /// [`Supervisor::on_wakeup`] with a synchronous tick.
    pub fn on_wakeup_blocking<F>(
        &self,
        name: impl Into<String>,
        wake: watch::Receiver<u64>,
        schedule: Schedule,
        tick: F,
    ) -> Result<(), AlreadyRunning>
    where
        F: FnMut(&TickCtx) -> TickResult + Send + 'static,
    {
        self.on_wakeup(name, wake, schedule, BlockingTick(Some(tick)))
    }

    /// Run `run` once under `name`. An error is logged and fails the row.
    pub fn one_shot<F, Fut>(&self, name: impl Into<String>, run: F) -> Result<(), AlreadyRunning>
    where
        F: FnOnce(TickCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), TickError>> + Send + 'static,
    {
        self.launch(name.into(), move |ctx| async move {
            let name = Arc::clone(&ctx.name);
            run(ctx).await.map_err(|error| {
                tracing::error!(loop_name = %name, code = error.code, detail = %error.detail, "background job failed");
                error.code
            })
        })
    }

    /// True while the name has a live task.
    pub fn is_running(&self, name: &str) -> bool {
        self.live().contains_key(name)
    }

    /// Names with live tasks, sorted.
    pub fn running_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.live().keys().cloned().collect();
        names.sort();
        names
    }

    /// Stop one loop: cancel it and wait up to `grace`, then abort it.
    /// The name is free at once. Unknown names are a no-op.
    pub async fn cancel(&self, name: &str, grace: Duration) {
        let Some(live) = self.live().remove(name) else {
            return;
        };
        live.token.cancel();
        self.join(name, live.handle, Instant::now() + grace).await;
    }

    /// Stop everything: cancel the token, give every loop one shared
    /// `grace`, abort what is left, then write the last rows.
    pub async fn shutdown(&self, grace: Duration) {
        self.inner.token.cancel();
        let deadline = Instant::now() + grace;
        let live: Vec<(String, Live)> = self.live().drain().collect();
        futures_util::future::join_all(
            live.into_iter()
                .map(|(name, live)| async move { self.join(&name, live.handle, deadline).await }),
        )
        .await;
        self.inner.writer_stop.cancel();
        let writer = self
            .inner
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut writer) = writer
            && tokio::time::timeout_at(deadline.max(Instant::now() + LAST_ROWS_BOUND), &mut writer)
                .await
                .is_err()
        {
            tracing::warn!("loop rows were still being written at the shutdown bound");
            writer.abort();
        }
    }

    async fn join(&self, name: &str, mut handle: JoinHandle<()>, deadline: Instant) {
        if tokio::time::timeout_at(deadline, &mut handle)
            .await
            .is_err()
        {
            tracing::warn!(loop_name = %name, "background loop did not stop in time; aborting it");
            handle.abort();
            if !self.is_running(name) {
                self.inner.book.end(name, JobState::Stopped, Some(ABORTED));
            }
        }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, HashMap<String, Live>> {
        self.inner
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Start `body` under `name`: the row says running, the body runs on
    /// its own task so a panic lands here, and the end is recorded.
    fn launch<B, Fut>(&self, name: String, body: B) -> Result<(), AlreadyRunning>
    where
        B: FnOnce(TickCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), &'static str>> + Send + 'static,
    {
        let mut live = self.live();
        if live.contains_key(&name) {
            return Err(AlreadyRunning);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let token = self.inner.token.child_token();
        let ctx = TickCtx {
            name: Arc::from(name.as_str()),
            token: token.clone(),
            book: Arc::clone(&self.inner.book),
        };
        self.inner.book.start(&name);
        let inner = Arc::clone(&self.inner);
        let runtime = self.inner.runtime.clone();
        let owned = name.clone();
        let handle = self.inner.runtime.spawn(async move {
            let task = runtime.spawn(body(ctx));
            let _abort = AbortOnDrop(task.abort_handle());
            let (state, code) = match task.await {
                Ok(Ok(())) => (JobState::Stopped, None),
                Ok(Err(code)) => (JobState::Failed, Some(code)),
                Err(join) if join.is_panic() => {
                    tracing::error!(loop_name = %owned, "background loop panicked");
                    (JobState::Failed, Some(PANICKED))
                }
                Err(_) => (JobState::Stopped, Some(ABORTED)),
            };
            let mut live = inner
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // A loop started again under this name after a cancel owns the
            // row now; this run's end is not written over it.
            match live.get(&owned) {
                Some(entry) if entry.id != id => {}
                _ => {
                    live.remove(&owned);
                    inner.book.end(&owned, state, code);
                }
            }
        });
        live.insert(name, Live { id, token, handle });
        Ok(())
    }
}

/// Aborts a task when dropped, so aborting a loop aborts its body.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn sleep_unless(token: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        biased;
        () = token.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Record a tick's result on the row and the failure count. False when the
/// loop should end.
fn settle(ctx: &TickCtx, result: TickResult, failures: &mut u32) -> bool {
    match result {
        Ok(next) => {
            *failures = 0;
            ctx.book.tick_done(&ctx.name, None);
            next == Next::Continue && !ctx.token.is_cancelled()
        }
        Err(error) => {
            *failures = failures.saturating_add(1);
            tracing::warn!(
                loop_name = %ctx.name,
                code = error.code,
                failures = *failures,
                detail = %error.detail,
                "background loop tick failed; backing off"
            );
            ctx.book.tick_done(&ctx.name, Some(error.code));
            !ctx.token.is_cancelled()
        }
    }
}

async fn run_periodic(
    ctx: TickCtx,
    schedule: Schedule,
    mut tick: impl Tick,
) -> Result<(), &'static str> {
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    if !ctx.sleep(schedule.first_delay(&mut rng)).await {
        return Ok(());
    }
    let mut failures = 0_u32;
    loop {
        let result = tick.tick(&ctx).await;
        if !settle(&ctx, result, &mut failures) {
            return Ok(());
        }
        if !ctx.sleep(schedule.next_delay(&mut rng, failures)).await {
            return Ok(());
        }
    }
}

async fn run_on_wakeup(
    ctx: TickCtx,
    mut wake: watch::Receiver<u64>,
    schedule: Schedule,
    mut tick: impl Tick,
) -> Result<(), &'static str> {
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    if !ctx.sleep(schedule.first_delay(&mut rng)).await {
        return Ok(());
    }
    let mut failures = 0_u32;
    loop {
        if failures == 0 {
            match wake.has_changed() {
                Err(_) => return Ok(()),
                Ok(true) => {}
                Ok(false) => {
                    let ceiling = schedule.interval();
                    tokio::select! {
                        biased;
                        () = ctx.token.cancelled() => return Ok(()),
                        changed = wake.changed() => {
                            if changed.is_err() {
                                return Ok(());
                            }
                        }
                        () = idle(ceiling) => {}
                    }
                }
            }
        }
        // Seen before the tick, so a change during the tick wakes the next.
        wake.mark_unchanged();
        let result = tick.tick(&ctx).await;
        if !settle(&ctx, result, &mut failures) {
            return Ok(());
        }
        let gap = if failures == 0 {
            schedule.min_delay()
        } else {
            schedule.retry_delay(&mut rng, failures)
        };
        if !ctx.sleep(gap).await {
            return Ok(());
        }
    }
}

/// Wait out the idle ceiling; `Duration::MAX` means there is none.
async fn idle(ceiling: Duration) {
    if ceiling == Duration::MAX {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(ceiling).await;
    }
}

/// The latest row of every loop, and which ones the database has not seen.
struct RowBook {
    clock: Arc<dyn Clock>,
    state: Mutex<BookState>,
    wake: Notify,
}

#[derive(Default)]
struct BookState {
    rows: HashMap<String, LoopRecord>,
    dirty: BTreeSet<String>,
}

impl RowBook {
    fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            state: Mutex::new(BookState::default()),
            wake: Notify::new(),
        }
    }

    fn change(&self, name: &str, edit: impl FnOnce(&mut LoopRecord, i64)) {
        let now = self.clock.now_ms();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let row = state
                .rows
                .entry(name.to_owned())
                .or_insert_with(|| LoopRecord {
                    name: name.to_owned(),
                    state: JobState::Running,
                    last_heartbeat_at: None,
                    last_error: None,
                    updated_at: now,
                });
            edit(row, now);
            row.updated_at = now;
            state.dirty.insert(name.to_owned());
        }
        self.wake.notify_one();
    }

    fn start(&self, name: &str) {
        self.change(name, |row, _| {
            row.state = JobState::Running;
            row.last_error = None;
        });
    }

    fn beat(&self, name: &str) {
        self.change(name, |row, now| row.last_heartbeat_at = Some(now));
    }

    fn tick_done(&self, name: &str, error: Option<&'static str>) {
        self.change(name, |row, now| {
            row.last_heartbeat_at = Some(now);
            row.last_error = error.map(str::to_owned);
        });
    }

    fn end(&self, name: &str, state: JobState, code: Option<&'static str>) {
        self.change(name, |row, now| {
            row.state = state;
            row.last_heartbeat_at = Some(now);
            if let Some(code) = code {
                row.last_error = Some(code.to_owned());
            }
        });
    }

    fn take_dirty(&self) -> Vec<LoopRecord> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let names = std::mem::take(&mut state.dirty);
        names
            .iter()
            .filter_map(|name| state.rows.get(name).cloned())
            .collect()
    }

    fn mark_dirty(&self, rows: &[LoopRecord]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.dirty.extend(rows.iter().map(|row| row.name.clone()));
    }
}

/// Write changed rows as they come, in one background write per flush. A
/// failed write keeps its rows and tries again after a jittered backoff.
/// `stop` ends it after one last flush.
async fn write_rows(db: Db, book: Arc<RowBook>, stop: CancellationToken) {
    let retry = Schedule::new(Duration::ZERO)
        .with_jitter(Duration::from_millis(250))
        .with_backoff(Duration::from_millis(250), Duration::from_secs(30));
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    let mut failures = 0_u32;
    loop {
        let stopping = tokio::select! {
            biased;
            () = book.wake.notified() => false,
            () = stop.cancelled() => true,
        };
        let rows = book.take_dirty();
        if !rows.is_empty() {
            match durable::put_loops(&db, rows.clone()).await {
                Ok(()) => failures = 0,
                Err(DbError::LaneClosed) => return,
                Err(error) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!(%error, failures, "loop rows not written; retrying");
                    book.mark_dirty(&rows);
                    if !stopping {
                        // Woken early by a stop, the loop still flushes once.
                        sleep_unless(&stop, retry.next_delay(&mut rng, failures)).await;
                        book.wake.notify_one();
                    }
                }
            }
        }
        if stopping {
            return;
        }
    }
}
