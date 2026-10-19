//! Discover warm cycle: every 90 seconds, ask discover to rebuild what
//! recent activity says is due (v2 `warm_discover_home_periodically`).
//!
//! The decisions (who is due, what to rebuild, when to try again) live in
//! `reads::discover`; this module only owns the loop on the supervisor.
//! Activity recorded by the page also starts a pass for that user right
//! away, so the loop is the backstop, not the only trigger.

use std::sync::Arc;
use std::time::Duration;

use super::BoxFuture;
use super::schedule::Schedule;
use super::supervisor::{AlreadyRunning, FnTick, Next, Supervisor};

/// Name of the warm loop.
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

/// The cadence, with a little spread.
pub fn default_schedule() -> Schedule {
    Schedule::new(INTERVAL)
        .with_initial_delay(INITIAL_DELAY)
        .with_jitter(Duration::from_secs(5))
}

/// Start the loop.
pub fn spawn_on(
    supervisor: &Supervisor,
    demand: Arc<dyn DemandTick>,
    schedule: Schedule,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        JOB_NAME,
        schedule,
        FnTick(move |_| {
            let demand = Arc::clone(&demand);
            async move {
                demand.tick().await;
                Ok(Next::Continue)
            }
        }),
    )
}
