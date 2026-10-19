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

use super::BoxFuture;
use super::schedule::Schedule;
use super::supervisor::{AlreadyRunning, FnTick, Next, Supervisor, TickError};

/// Name of the refresh loop.
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

impl<T: PersonalMixer + ?Sized> PersonalMixer for Arc<T> {
    fn run_for_all_users(&self) -> BoxFuture<'_, Result<(), String>> {
        T::run_for_all_users(self)
    }
}

/// The cadence: daily with a 5-minute spread and a slow backoff
/// (failures here usually mean a provider is down, not a blip).
pub fn default_schedule() -> Schedule {
    Schedule::new(REFRESH_INTERVAL)
        .with_initial_delay(INITIAL_DELAY)
        .with_jitter(DEFAULT_JITTER)
        .with_backoff(Duration::from_secs(60), Duration::from_secs(3600))
}

/// Row code of a failed refresh.
pub const FAILED: &str = "PERSONAL_MIX_FAILED";

/// Start the loop. The first cycle waits out the boot delay; failures back
/// off and never end it.
pub fn spawn_on<M: PersonalMixer>(
    supervisor: &Supervisor,
    mixer: M,
    schedule: Schedule,
) -> Result<(), AlreadyRunning> {
    let mixer = Arc::new(mixer);
    supervisor.periodic(
        JOB_NAME,
        schedule,
        FnTick(move |_| {
            let mixer = Arc::clone(&mixer);
            async move {
                mixer
                    .run_for_all_users()
                    .await
                    .map(|()| Next::Continue)
                    .map_err(|cause| TickError::new(FAILED, cause))
            }
        }),
    )
}
