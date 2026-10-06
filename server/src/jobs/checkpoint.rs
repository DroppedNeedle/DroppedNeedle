//! WAL checkpoint loop: the steady-state 30 s `PASSIVE` pass plus reclaim.
//!
//! The pass itself lives in [`crate::db::checkpoint`]; this module is the
//! registry half. It spawns the loop under [`JOB_NAME`], beats
//! a heartbeat per pass, and spreads the cadence with a little jitter so a
//! fleet restarting together does not checkpoint in lockstep. Checkpoint
//! passes are infallible by construction (every outcome, busy locks included,
//! is a recorded row, never an error), so the loop carries no backoff: each
//! cycle runs and the next waits out the schedule.

use std::sync::Arc;
use std::time::Duration;

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::{Schedule, SplitMix64, sleep_or_stop};
use crate::db::checkpoint::{CHECKPOINT_CADENCE, CheckpointService};

/// Registered name of the checkpoint loop.
pub const JOB_NAME: &str = "wal-checkpoint";

/// Default spread on the 30 s cadence.
pub const DEFAULT_JITTER: Duration = Duration::from_secs(5);

/// One checkpoint cycle behind a seam, so tests run the loop without a
/// database. Production binds [`CheckpointService`].
pub trait CheckpointRunner: Send + Sync + 'static {
    /// A single bounded `PASSIVE` pass plus a reclaim check when due.
    fn cycle(&self) -> BoxFuture<'_, ()>;
}

impl CheckpointRunner for CheckpointService {
    fn cycle(&self) -> BoxFuture<'_, ()> {
        let service = self.clone();
        Box::pin(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                let pass = service.run_once();
                let reclaim = service.maybe_truncate(std::time::Instant::now());
                (pass, reclaim.is_some())
            })
            .await;
            if outcome.is_err() {
                tracing::error!("checkpoint pass task failed to join");
            }
            // Planner statistics ride the same maintenance loop; the check
            // is throttled inside, so most passes return at once.
            if let Some(analyze) = self.analyze()
                && let Err(error) = analyze.run_if_due().await
            {
                tracing::warn!(%error, "planner statistics refresh failed; retrying later");
            }
        })
    }
}

/// A cycle counter for tests: records each pass without touching SQLite.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default, Clone)]
pub struct FakeCheckpointRunner {
    passes: Arc<std::sync::Mutex<u64>>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeCheckpointRunner {
    /// A runner that has run nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many cycles ran.
    pub fn passes(&self) -> u64 {
        *self
            .passes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl CheckpointRunner for FakeCheckpointRunner {
    fn cycle(&self) -> BoxFuture<'_, ()> {
        let passes = Arc::clone(&self.passes);
        Box::pin(async move {
            let mut guard = passes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = guard.saturating_add(1);
        })
    }
}

/// The cadence: GH-293's 30 s with a 5 s jitter spread.
pub fn default_schedule() -> Schedule {
    Schedule::new(CHECKPOINT_CADENCE).with_jitter(DEFAULT_JITTER)
}

/// Spawn the loop on a registry. The first pass runs at once (v2's loop
/// ticks immediately too); later passes wait out the schedule.
pub async fn spawn_on<S, R>(
    registry: &JobRegistry<S>,
    runner: R,
    schedule: Schedule,
) -> Result<(), super::registry::AlreadyRunning>
where
    S: RegistryStore,
    R: CheckpointRunner,
{
    let runner = Arc::new(runner);
    registry
        .spawn(JOB_NAME, JobKind::Ephemeral, None, move |ctx| {
            let runner = Arc::clone(&runner);
            async move { run(ctx, runner.as_ref(), &schedule).await }
        })
        .await
}

/// Run until stop fires. Every pass heartbeats; nothing here can fail, so
/// the loop always exits `Stopped`.
pub async fn run<S, R>(ctx: JobCtx<S>, runner: &R, schedule: &Schedule) -> JobExit
where
    S: RegistryStore,
    R: CheckpointRunner,
{
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    loop {
        runner.cycle().await;
        ctx.heartbeat().await;
        if sleep_or_stop(schedule.next_delay(&mut rng, 0), ctx.stop()).await {
            return JobExit::Stopped;
        }
    }
}
