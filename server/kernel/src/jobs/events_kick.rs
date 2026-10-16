//! Events kick: the one-shot sweep right after an events settings save.
//!
//! Enabling events takes effect through the watcher loop, but that loop may
//! sleep up to a day before its next slot. The settings save path therefore
//! kicks one immediate sweep. v2 ran that kick on a bare module-global task
//! the registry never saw, so overlapping kicks and restarts went untracked;
//! that gap is the registration fix. v3 kicks through the registry under
//! [`JOB_NAME`]: a kick while one is already running is skipped (the sweep
//! is idempotent and a rare overlap with the periodic loop is harmless, since
//! store writes serialize), and the row records the run either way.

use std::sync::Arc;

use super::events_watcher::EventsWatcher;
use super::registry::{JobExit, JobKind, JobRegistry, RegistryStore};

/// Kick the upcoming-events sweep. Single-flight: overlapping kicks
/// collapse into one, so a burst of saves schedules one sweep.
pub trait EventsKick: Send + Sync {
    /// Request a sweep. Cheap, idempotent under concurrency.
    fn kick(&self);
}

/// No-op kick, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct NoopKick;

#[cfg(any(test, feature = "test-support"))]
impl EventsKick for NoopKick {
    fn kick(&self) {}
}

/// Closure adapter so wiring passes the real sweep with one line.
pub struct FnKick<F> {
    /// Kick closure.
    pub kick_fn: F,
}

impl<F: Fn() + Send + Sync> EventsKick for FnKick<F> {
    fn kick(&self) {
        (self.kick_fn)();
    }
}

/// Registered name of the kick one-shot.
pub const JOB_NAME: &str = "events-kick";

/// What the kick decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KickOutcome {
    /// A sweep started under [`JOB_NAME`].
    Started,
    /// A kicked sweep is still running; this kick was skipped.
    AlreadyRunning,
}

/// Kick one sweep unless one is already running. The sweep no-ops when no
/// source is ready, so kicking on every settings save is safe. Sweep failures
/// log inside the task and still land the row as stopped: the kick fired, it
/// just found nothing worth keeping.
pub async fn kick<S, W>(registry: &JobRegistry<S>, watcher: W) -> KickOutcome
where
    S: RegistryStore,
    W: EventsWatcher,
{
    if registry.is_running(JOB_NAME) {
        return KickOutcome::AlreadyRunning;
    }
    let watcher = Arc::new(watcher);
    let spawned = registry
        .spawn(JOB_NAME, JobKind::Ephemeral, None, move |ctx| {
            let watcher = Arc::clone(&watcher);
            async move {
                if let Err(cause) = watcher.run_sweep(None, Arc::clone(ctx.stop())).await {
                    tracing::error!(%cause, "kicked events sweep failed");
                }
                ctx.heartbeat().await;
                JobExit::Stopped
            }
        })
        .await;
    match spawned {
        Ok(()) => KickOutcome::Started,
        Err(_) => KickOutcome::AlreadyRunning,
    }
}
