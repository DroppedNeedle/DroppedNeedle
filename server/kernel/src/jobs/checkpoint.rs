//! WAL checkpoint loop: the steady-state 30 s `PASSIVE` pass plus reclaim.
//!
//! The pass itself lives in [`crate::db::checkpoint`]; this module runs it
//! on the supervisor under [`JOB_NAME`], spreading the cadence with a little
//! jitter so a fleet restarting together does not checkpoint in lockstep. A
//! pass blocked by a lock is a normal outcome; a pass that failed, or a
//! planner statistics refresh that failed, fails the tick, so the loop backs
//! off until passes succeed again.

use std::sync::Arc;
use std::time::Duration;

use super::BoxFuture;
use super::schedule::Schedule;
use super::supervisor::{AlreadyRunning, FnTick, Next, Supervisor, TickError};
use crate::db::checkpoint::{CHECKPOINT_CADENCE, CheckpointService};

/// Loop name.
pub const JOB_NAME: &str = "wal-checkpoint";

/// Row code of a failed checkpoint tick.
pub const FAILED: &str = "CHECKPOINT_FAILED";

/// Most jitter added to each wait.
pub const DEFAULT_JITTER: Duration = Duration::from_secs(5);

/// One checkpoint cycle behind a seam, so the loop can run without a
/// database in tests.
pub trait CheckpointRunner: Send + Sync + 'static {
    /// Run one pass (and the reclaim and statistics checks that ride it).
    /// An error is the cause, for the log.
    fn cycle(&self) -> BoxFuture<'_, Result<(), String>>;
}

impl CheckpointRunner for CheckpointService {
    fn cycle(&self) -> BoxFuture<'_, Result<(), String>> {
        let service = self.clone();
        Box::pin(async move {
            let pass = tokio::task::spawn_blocking(move || {
                let pass = service.run_once();
                service.maybe_truncate(std::time::Instant::now());
                pass
            })
            .await
            .map_err(|error| format!("checkpoint pass task failed to join: {error}"))?;
            // Planner statistics ride the same maintenance loop; the check
            // is throttled inside, so most passes return at once.
            if let Some(analyze) = self.analyze() {
                analyze
                    .run_if_due()
                    .await
                    .map_err(|error| format!("planner statistics refresh failed: {error}"))?;
            }
            match pass.error {
                Some(error) => Err(format!("checkpoint pass failed: {error}")),
                None => Ok(()),
            }
        })
    }
}

/// Default pacing: the checkpoint cadence plus a little jitter.
pub fn default_schedule() -> Schedule {
    Schedule::new(CHECKPOINT_CADENCE).with_jitter(DEFAULT_JITTER)
}

/// Start the loop: a pass at once, then one per schedule turn.
pub fn spawn_on<R: CheckpointRunner>(
    supervisor: &Supervisor,
    runner: R,
    schedule: Schedule,
) -> Result<(), AlreadyRunning> {
    let runner = Arc::new(runner);
    supervisor.periodic(
        JOB_NAME,
        schedule,
        FnTick(move |_| {
            let runner = Arc::clone(&runner);
            async move {
                runner
                    .cycle()
                    .await
                    .map(|()| Next::Continue)
                    .map_err(|cause| TickError::new(FAILED, cause))
            }
        }),
    )
}
