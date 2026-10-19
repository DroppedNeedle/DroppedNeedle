//! Events kick: the one-shot sweep right after an events settings save.
//!
//! Enabling events takes effect through the watcher loop, but that loop may
//! sleep up to a day before its next slot. The settings save path therefore
//! kicks one immediate sweep. v2 ran that kick on a bare module-global task
//! nothing tracked, so overlapping kicks and restarts went unseen. v3 kicks
//! through the supervisor under [`JOB_NAME`]: a kick while one is already
//! running is skipped (the sweep is idempotent and a rare overlap with the
//! periodic loop is harmless, since store writes serialize), and the row
//! records the run either way.

use super::events_watcher::{EventsWatcher, FAILED};
use super::supervisor::{Supervisor, TickError};

/// The settings-save hook. Firing never blocks the save; implementations
/// start the sweep and return.
pub trait EventsKick: Send + Sync {
    /// Kick one sweep.
    fn kick(&self);
}

/// A kick that does nothing, for test states.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct NoopKick;

#[cfg(any(test, feature = "test-support"))]
impl EventsKick for NoopKick {
    fn kick(&self) {}
}

/// A kick from a closure.
pub struct FnKick<F> {
    /// What a kick runs.
    pub kick_fn: F,
}

impl<F: Fn() + Send + Sync> EventsKick for FnKick<F> {
    fn kick(&self) {
        (self.kick_fn)();
    }
}

/// Name of the kicked sweep while it runs.
pub const JOB_NAME: &str = "events-kick";

/// What one kick did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KickOutcome {
    /// A sweep started.
    Started,
    /// A kicked sweep was already running; this kick was skipped.
    AlreadyRunning,
}

/// Start one full sweep (no catch-up window) under [`JOB_NAME`], unless one
/// is already running.
pub fn kick<W: EventsWatcher>(supervisor: &Supervisor, watcher: W) -> KickOutcome {
    let started = supervisor.one_shot(JOB_NAME, move |ctx| async move {
        watcher
            .run_sweep(None, ctx.token().clone())
            .await
            .map(|_| ())
            .map_err(|cause| TickError::new(FAILED, cause))
    });
    match started {
        Ok(()) => KickOutcome::Started,
        Err(_) => KickOutcome::AlreadyRunning,
    }
}
