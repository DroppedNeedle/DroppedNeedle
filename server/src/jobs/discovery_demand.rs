//! Discover warm cycle: every 90 seconds, ask discover to rebuild what
//! recent activity says is due (v2 `warm_discover_home_periodically`).
//!
//! The decisions (who is due, what to rebuild, when to try again) live in
//! `reads::discover`; this module only owns the loop on the shared
//! registry. Activity recorded by the page also starts a pass for that
//! user right away, so the loop is the backstop, not the only trigger.

use std::sync::Arc;
use std::time::Duration;

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::{Schedule, SplitMix64, sleep_or_stop};

/// Registered name of the warm loop.
pub const JOB_NAME: &str = "discover-warm-cycle";

/// v2 cadence: a pass every 90 seconds...
pub const INTERVAL: Duration = Duration::from_secs(90);
/// ...starting five seconds after boot.
pub const INITIAL_DELAY: Duration = Duration::from_secs(5);

/// One warm pass behind a seam.
pub trait DemandTick: Send + Sync + 'static {
    /// Rebuild what is due. Failures are logged inside.
    fn tick(&self) -> BoxFuture<'_, ()>;
}

/// No warm cycle (test states).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDemand;

impl DemandTick for NoDemand {
    fn tick(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The discover content's warm pass.
pub struct ContentDemand(pub Arc<dyn crate::reads::discover::ports::DiscoverContent>);

impl DemandTick for ContentDemand {
    fn tick(&self) -> BoxFuture<'_, ()> {
        self.0.run_due_tick()
    }
}

/// The cadence, with a little spread.
pub fn default_schedule() -> Schedule {
    Schedule::new(INTERVAL)
        .with_initial_delay(INITIAL_DELAY)
        .with_jitter(Duration::from_secs(5))
}

/// Spawn the loop on a registry.
pub async fn spawn_on<S>(
    registry: &JobRegistry<S>,
    demand: Arc<dyn DemandTick>,
    schedule: Schedule,
) -> Result<(), super::registry::AlreadyRunning>
where
    S: RegistryStore,
{
    registry
        .spawn(JOB_NAME, JobKind::Ephemeral, None, move |ctx| {
            let demand = Arc::clone(&demand);
            async move { run(ctx, demand.as_ref(), &schedule).await }
        })
        .await
}

/// Run until stop fires.
pub async fn run<S>(ctx: JobCtx<S>, demand: &dyn DemandTick, schedule: &Schedule) -> JobExit
where
    S: RegistryStore,
{
    if sleep_or_stop(schedule.initial_delay(), ctx.stop()).await {
        return JobExit::Stopped;
    }
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    loop {
        demand.tick().await;
        ctx.heartbeat().await;
        if sleep_or_stop(schedule.next_delay(&mut rng, 0), ctx.stop()).await {
            return JobExit::Stopped;
        }
    }
}
