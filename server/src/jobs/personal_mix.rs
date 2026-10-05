//! Personal-mix refresh loop: rebuild every user's weekly mix daily.
//!
//! Once a day (after a 5-minute boot delay so startup settles first) the loop
//! asks the mixer to rebuild all users' mixes. One user's failure never
//! blocks the others: that isolation lives in the mixer, and the loop only
//! sees a whole-cycle result for backoff. The per-user refresh route
//! (`POST /personal-mix/refresh`) lives in `acquire::requests`; this module owns just the
//! background pass.

use std::sync::Arc;
use std::time::Duration;

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::{Schedule, SplitMix64, sleep_or_stop};

/// Registered name of the refresh loop.
pub const JOB_NAME: &str = "personal-mix-refresh";

/// v2 cadence: daily, first pass 5 minutes after boot.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(86_400);
/// v2 boot delay, unchanged.
pub const INITIAL_DELAY: Duration = Duration::from_secs(300);

/// Default spread on the daily cadence.
pub const DEFAULT_JITTER: Duration = Duration::from_secs(300);

/// The mixer behind a seam. Implementations rebuild every eligible user's mix
/// and isolate per-user failures internally.
pub trait PersonalMixer: Send + Sync + 'static {
    /// Rebuild all users' mixes. `Err` carries the cycle failure for the log
    /// and backoff; partial per-user progress stays kept.
    fn run_for_all_users(&self) -> BoxFuture<'_, Result<(), String>>;
}

/// The cadence: daily with a 5-minute spread and a slow backoff
/// (failures here usually mean a provider is down, not a blip).
pub fn default_schedule() -> Schedule {
    Schedule::new(REFRESH_INTERVAL)
        .with_initial_delay(INITIAL_DELAY)
        .with_jitter(DEFAULT_JITTER)
        .with_backoff(Duration::from_secs(60), Duration::from_secs(3600))
}

/// Spawn the loop on a registry.
pub async fn spawn_on<S, M>(
    registry: &JobRegistry<S>,
    mixer: M,
    schedule: Schedule,
) -> Result<(), super::registry::AlreadyRunning>
where
    S: RegistryStore,
    M: PersonalMixer,
{
    let mixer = Arc::new(mixer);
    registry
        .spawn(JOB_NAME, JobKind::Ephemeral, None, move |ctx| {
            let mixer = Arc::clone(&mixer);
            async move { run(ctx, mixer.as_ref(), &schedule).await }
        })
        .await
}

/// Run until stop fires. The first cycle waits out the boot delay; failures
/// back off and never exit.
pub async fn run<S, M>(ctx: JobCtx<S>, mixer: &M, schedule: &Schedule) -> JobExit
where
    S: RegistryStore,
    M: PersonalMixer,
{
    if sleep_or_stop(schedule.initial_delay(), ctx.stop()).await {
        return JobExit::Stopped;
    }
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    let mut failures = 0_u32;
    loop {
        match mixer.run_for_all_users().await {
            Ok(()) => failures = 0,
            Err(cause) => {
                failures = failures.saturating_add(1);
                tracing::error!(%cause, "personal mix refresh failed");
            }
        }
        ctx.heartbeat().await;
        if sleep_or_stop(schedule.next_delay(&mut rng, failures), ctx.stop()).await {
            return JobExit::Stopped;
        }
    }
}
