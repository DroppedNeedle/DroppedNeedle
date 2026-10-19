//! One supervised runner per registered kind.
//!
//! A runner's tick holds its own wait and returns only at stop or on an
//! error, so an idle wake writes nothing: the loop row is touched by a
//! heartbeat at most once a minute. On its first turn it requeues every
//! row of its kind an earlier process held. Then it reads when work is
//! next due, claims and runs one job when it is, and otherwise waits for a
//! commit, a wake, the due time (at most a minute away) or the stop.
//!
//! The handler runs on its own task, outside any write, under the kind's
//! run timeout, with a heartbeat every third of the lease. Its answer is
//! written by one owner-checked finish. A finish or put-back the database
//! is too busy for is retried while the lease is kept alive, until the
//! stop, which leaves the row for start recovery. Only a busy claim or read
//! is shrugged off; any other failure ends the tick with a code.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::{
    HANDLER_PANICKED, JobCtx, JobError, JobHandler, JobKind, JobQueue, KindSpec, Lease, Outcome,
    SPREAD_SCALE, TIMED_OUT, live_kinds,
};
use crate::db::DbError;
use crate::jobs::schedule::{Schedule, SplitMix64};
use crate::jobs::supervisor::{
    AlreadyRunning, Next, Supervisor, Tick, TickCtx, TickError, TickResult,
};

/// The most a runner waits before it reads again.
const IDLE_CEILING: Duration = Duration::from_secs(60);
/// The least time between two reads a wake asks for.
const MIN_GAP: Duration = Duration::from_secs(1);
/// How often a runner beats its loop row while idle or busy.
const BEAT_EVERY: Duration = Duration::from_secs(60);
/// Wait after a busy claim, read or write, before the next try.
const BUSY_WAIT: Duration = Duration::from_secs(2);
/// A job put back for busy this many times in a row is logged.
const BUSY_WARN_EVERY: u32 = 10;

/// The one-shot that warns about kinds with live rows and no handler.
pub const KINDS_CHECK: &str = "job-kinds-check";

/// The handlers the builder registered, one per kind.
#[derive(Clone, Default)]
pub struct Handlers(Vec<(KindSpec, Arc<dyn JobHandler>)>);

impl std::fmt::Debug for Handlers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_list()
            .entries(self.0.iter().map(|(spec, _)| spec.kind.name()))
            .finish()
    }
}

impl Handlers {
    /// Add one kind's handler.
    pub fn with(mut self, spec: KindSpec, handler: Arc<dyn JobHandler>) -> Self {
        self.0.push((spec, handler));
        self
    }
}

/// The supervisor name of a kind's runner.
pub fn runner_name(kind: JobKind) -> String {
    format!("job-runner:{}", kind.name())
}

/// Start one runner per registered kind, and a one-shot that warns about
/// kinds with live rows that nothing runs.
pub fn start_runners(
    queue: &JobQueue,
    supervisor: &Supervisor,
    handlers: Handlers,
) -> Result<(), AlreadyRunning> {
    let known: Vec<&'static str> = handlers
        .0
        .iter()
        .map(|(spec, _)| spec.kind.name())
        .collect();
    for (spec, handler) in handlers.0 {
        supervisor.periodic(
            runner_name(spec.kind),
            Schedule::new(Duration::ZERO).with_backoff(Duration::from_secs(1), IDLE_CEILING),
            Runner::new(queue.clone(), spec, handler),
        )?;
    }
    let db = queue.db.clone();
    supervisor.one_shot(KINDS_CHECK, move |_| async move {
        match live_kinds(&db).await {
            Ok(kinds) => {
                for kind in kinds.iter().filter(|kind| !known.contains(&kind.as_str())) {
                    tracing::warn!(kind = %kind, "jobs of this kind wait and nothing runs them");
                }
            }
            Err(error) => tracing::warn!(%error, "job kinds not checked"),
        }
        Ok(())
    })
}

struct Runner {
    queue: JobQueue,
    spec: KindSpec,
    handler: Arc<dyn JobHandler>,
    recovered: bool,
    commits: Option<watch::Receiver<u64>>,
    wake: watch::Receiver<u64>,
    rng: SplitMix64,
    last_beat: Option<Instant>,
    last_read: Option<Instant>,
    last_prune: Option<Instant>,
    busy_streak: HashMap<i64, u32>,
}

/// What one claim came to.
enum Ran {
    /// A job ran (or a busy claim waited): look again at once.
    Again,
    /// Nothing was claimable after all.
    Nothing,
    /// The stop came.
    Stopped,
}

impl Runner {
    fn new(queue: JobQueue, spec: KindSpec, handler: Arc<dyn JobHandler>) -> Self {
        let commits = Some(queue.db.commits());
        let wake = queue.wake.subscribe();
        Self {
            queue,
            spec,
            handler,
            recovered: false,
            commits,
            wake,
            rng: SplitMix64::new(SplitMix64::seed_from_time()),
            last_beat: None,
            last_read: None,
            last_prune: None,
            busy_streak: HashMap::new(),
        }
    }

    fn kind(&self) -> &'static str {
        self.spec.kind.name()
    }

    fn spread(&mut self) -> u32 {
        u32::try_from(self.rng.below(u64::from(SPREAD_SCALE) + 1)).unwrap_or(SPREAD_SCALE)
    }

    fn busy_wait(&mut self) -> Duration {
        BUSY_WAIT + self.rng.below_duration(BUSY_WAIT)
    }

    /// Beat the loop row, at most once a minute.
    fn beat(&mut self, ctx: &TickCtx) {
        if self.last_beat.is_none_or(|at| at.elapsed() >= BEAT_EVERY) {
            ctx.heartbeat();
            self.last_beat = Some(Instant::now());
        }
    }

    /// Wait for a commit, a wake, `delay` or the stop. False at the stop.
    async fn wait(&mut self, ctx: &TickCtx, delay: Duration) -> bool {
        let commits = &mut self.commits;
        let woke = tokio::select! {
            biased;
            () = ctx.token().cancelled() => return false,
            changed = changed(commits) => {
                if changed.is_err() {
                    *commits = None;
                }
                true
            }
            _ = self.wake.changed() => true,
            () = tokio::time::sleep(delay) => false,
        };
        if woke {
            let since = self.last_read.map_or(MIN_GAP, |at| at.elapsed());
            if since < MIN_GAP && !ctx.sleep(MIN_GAP - since).await {
                return false;
            }
        }
        true
    }

    async fn pass(&mut self, ctx: &TickCtx) -> Result<bool, TickError> {
        self.beat(ctx);
        if !self.recovered {
            match self.queue.recover_at_start(&self.spec).await {
                Ok(_) => self.recovered = true,
                Err(error) if error.is_busy() => {
                    let wait = self.busy_wait();
                    return Ok(ctx.sleep(wait).await);
                }
                Err(error) => return Err(TickError::new("JOB_RECOVERY_FAILED", error)),
            }
        }
        if self
            .last_prune
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(3600))
        {
            match self.queue.prune(&self.spec).await {
                Ok(()) => self.last_prune = Some(Instant::now()),
                Err(error) if error.is_busy() => {}
                Err(error) => return Err(TickError::new("JOB_PRUNE_FAILED", error)),
            }
        }
        if let Some(commits) = &mut self.commits {
            commits.borrow_and_update();
        }
        self.wake.borrow_and_update();
        self.last_read = Some(Instant::now());
        let next = match self.queue.next_due(self.spec.kind).await {
            Ok(next) => next,
            Err(DbError::Busy { .. }) => {
                let wait = self.busy_wait();
                return Ok(ctx.sleep(wait).await);
            }
            Err(error) => return Err(TickError::new("JOB_READ_FAILED", error)),
        };
        let now = self.queue.now();
        let mut floor = Duration::ZERO;
        if next.is_some_and(|at| at <= now) {
            match self.run_one(ctx).await? {
                Ran::Again => return Ok(true),
                Ran::Nothing => floor = MIN_GAP,
                Ran::Stopped => return Ok(false),
            }
        }
        let until_due = next.map_or(IDLE_CEILING, |at| {
            Duration::from_millis(u64::try_from(at.saturating_sub(now)).unwrap_or(0))
        });
        let delay = until_due.min(IDLE_CEILING).max(floor);
        Ok(self.wait(ctx, delay).await)
    }

    async fn run_one(&mut self, ctx: &TickCtx) -> Result<Ran, TickError> {
        let claimed = match self.queue.claim(&self.spec).await {
            Ok(Some(claimed)) => claimed,
            Ok(None) => return Ok(Ran::Nothing),
            Err(error) if error.is_busy() => {
                let wait = self.busy_wait();
                return Ok(if ctx.sleep(wait).await {
                    Ran::Again
                } else {
                    Ran::Stopped
                });
            }
            Err(error) => return Err(TickError::new("JOB_CLAIM_FAILED", error)),
        };
        let lease = claimed.lease.clone();
        let attempt = claimed.attempt;
        let job_token = ctx.token().child_token();
        let job = JobCtx {
            claimed,
            spec: self.spec,
            queue: self.queue.clone(),
            token: job_token.clone(),
        };
        let mut task = AbortOnDrop(tokio::spawn(self.handler.run(job)));
        let deadline = Instant::now() + self.spec.run_timeout;
        let beat_every = (self.spec.lease / 3).max(Duration::from_millis(100));
        let mut next_beat = Instant::now() + beat_every;
        let settle = loop {
            tokio::select! {
                biased;
                joined = &mut task.0 => break Settle::Finish(answer(self.kind(), lease.id(), joined)),
                () = ctx.token().cancelled() => {
                    job_token.cancel();
                    break match tokio::time::timeout(self.spec.stop_grace, &mut task.0).await {
                        Ok(joined) => Settle::Finish(answer(self.kind(), lease.id(), joined)),
                        Err(_) => {
                            task.0.abort();
                            Settle::Release
                        }
                    };
                }
                () = tokio::time::sleep_until(deadline) => {
                    job_token.cancel();
                    task.0.abort();
                    tracing::warn!(kind = self.kind(), id = lease.id(), attempt, "job ran past its timeout");
                    break Settle::Finish(Outcome::Retry { code: TIMED_OUT, at: None });
                }
                () = tokio::time::sleep_until(next_beat) => {
                    next_beat = Instant::now() + beat_every;
                    match self.queue.heartbeat(&lease, &self.spec).await {
                        Ok(()) => self.beat(ctx),
                        Err(JobError::LeaseLost) => {
                            job_token.cancel();
                            task.0.abort();
                            tracing::info!(kind = self.kind(), id = lease.id(), "job lease lost; its run is dropped");
                            return Ok(Ran::Again);
                        }
                        Err(error) if error.is_busy() => {}
                        Err(error) => return Err(TickError::new("JOB_HEARTBEAT_FAILED", error)),
                    }
                }
            }
        };
        self.settle(ctx, &lease, attempt, settle).await?;
        Ok(if ctx.token().is_cancelled() {
            Ran::Stopped
        } else {
            Ran::Again
        })
    }

    /// Write a run's answer, retrying on busy only while the lease is kept
    /// alive. Stops trying at the stop; a row nobody holds any more is
    /// final.
    async fn settle(
        &mut self,
        ctx: &TickCtx,
        lease: &Lease,
        attempt: u32,
        settle: Settle,
    ) -> Result<(), TickError> {
        match settle {
            Settle::Finish(Outcome::Fail { code }) => {
                tracing::warn!(
                    kind = self.kind(),
                    id = lease.id(),
                    attempt,
                    code,
                    "job failed for good"
                );
            }
            Settle::Finish(Outcome::Retry { code, .. }) => {
                tracing::info!(
                    kind = self.kind(),
                    id = lease.id(),
                    attempt,
                    code,
                    "job will retry"
                );
            }
            Settle::Finish(Outcome::Busy) => {
                let kind = self.kind();
                let streak = self.busy_streak.entry(lease.id()).or_default();
                *streak += 1;
                if *streak % BUSY_WARN_EVERY == 0 {
                    tracing::warn!(
                        kind,
                        id = lease.id(),
                        times = *streak,
                        "job keeps finding the database busy"
                    );
                }
            }
            _ => {}
        }
        if !matches!(settle, Settle::Finish(Outcome::Busy)) {
            self.busy_streak.remove(&lease.id());
        }
        if self.busy_streak.len() > 1000 {
            self.busy_streak.clear();
        }
        loop {
            let written = match settle {
                Settle::Finish(outcome) => {
                    let spread = self.spread();
                    self.queue.finish(lease, &self.spec, outcome, spread).await
                }
                Settle::Release => self.queue.release(lease).await,
            };
            match written {
                Ok(()) => return Ok(()),
                Err(JobError::LeaseLost) => {
                    tracing::info!(
                        kind = self.kind(),
                        id = lease.id(),
                        "job finished elsewhere; answer dropped"
                    );
                    return Ok(());
                }
                Err(error) if error.is_busy() => {
                    if ctx.token().is_cancelled() {
                        return Ok(());
                    }
                    match self.queue.heartbeat(lease, &self.spec).await {
                        Err(JobError::LeaseLost) => return Ok(()),
                        Err(error) if !error.is_busy() => tracing::warn!(
                            kind = self.kind(),
                            id = lease.id(),
                            %error,
                            "job lease not extended while its answer waits"
                        ),
                        _ => {}
                    }
                    let wait = self.busy_wait();
                    if !ctx.sleep(wait).await {
                        return Ok(());
                    }
                }
                Err(error) => return Err(TickError::new("JOB_FINISH_FAILED", error)),
            }
        }
    }
}

/// What to write once a run ends.
#[derive(Clone, Copy)]
enum Settle {
    /// The handler's answer.
    Finish(Outcome),
    /// Back in the queue at stop, attempt returned.
    Release,
}

impl Tick for Runner {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        while self.pass(ctx).await? {}
        Ok(Next::Continue)
    }
}

/// A handler task's end as an answer: a panic retries with its code.
fn answer(kind: &'static str, id: i64, joined: Result<Outcome, tokio::task::JoinError>) -> Outcome {
    match joined {
        Ok(outcome) => outcome,
        Err(error) => {
            if error.is_panic() {
                tracing::error!(kind, id, "job handler panicked");
            }
            Outcome::Retry {
                code: HANDLER_PANICKED,
                at: None,
            }
        }
    }
}

/// The next change of a commit counter; pending for ever once it closed.
async fn changed(
    commits: &mut Option<watch::Receiver<u64>>,
) -> Result<(), watch::error::RecvError> {
    match commits {
        Some(receiver) => receiver.changed().await,
        None => std::future::pending().await,
    }
}

/// Aborts a handler task when the runner lets go of it.
struct AbortOnDrop(JoinHandle<Outcome>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
