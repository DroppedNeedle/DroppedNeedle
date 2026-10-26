//! Library operation jobs: long-running administrator work an
//! administrator can watch, pause, resume, and stop.
//!
//! Jobs are claimed, kept and recovered through [`crate::jobs`]. The
//! worker here runs re-identifications: a person retries a review, and the
//! job hands the album to the identification queue, which settles it by
//! its usual rules. The same module undoes an automatic edition and
//! searches MusicBrainz releases for the edition finder.
//!
//! Layout: `models` holds the types, `control` the pause, resume, and stop
//! rules, `store` the re-identification SQL, `decisions` the administrator's catalog
//! decisions, `reidentify` the candidate evaluation, and `service` the
//! entry points the HTTP handlers and the loop call.

pub mod choice;
pub mod control;
pub mod decisions;
mod hand_match;
pub mod history;
pub mod models;
pub mod plan;
pub mod port;
pub mod reasons;
pub mod reidentify;
pub mod reviews;
pub mod service;
pub mod status;
pub mod store;

use std::time::Duration;

use droppedneedle_kernel::jobs::queue::lease::Owner;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};

use crate::jobs::OPERATIONS_WAKEUP;
use crate::wiring::LibrarySetup;
use service::Operations;

/// How long the worker waits for a wakeup before it looks anyway: a
/// queued re-identification wakes it, while edition remaps are found by
/// looking.
const IDLE_CEILING: Duration = Duration::from_secs(2);

/// The operations worker's name on the supervisor.
pub const JOB_NAME: &str = "library-operations";

/// Start the operations worker (re-identifications, then edition
/// remaps): one job at a time, never while a scan runs (the scan
/// may be changing the very files a job reads). Its first turn runs at
/// once and requeues the re-identifications an earlier process left
/// running.
pub(crate) fn start(
    setup: &LibrarySetup,
    ops: Operations,
    supervisor: &Supervisor,
) -> Result<(), AlreadyRunning> {
    let mut wake = setup.wakeups.subscribe(OPERATIONS_WAKEUP);
    wake.mark_changed();
    supervisor.on_wakeup(
        JOB_NAME,
        wake,
        Schedule::new(IDLE_CEILING),
        OperationsTick {
            ops,
            owners: Owner::new(),
            setup: setup.clone(),
            recovered: false,
        },
    )
}

/// One turn: work steps until none is due.
struct OperationsTick {
    setup: LibrarySetup,
    ops: Operations,
    /// Each step claims under an owner never used before.
    owners: Owner,
    /// Whether start recovery ran.
    recovered: bool,
}

impl Tick for OperationsTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        if !self.recovered {
            self.recovered = recover_at_start(&self.ops).await;
        }
        while !ctx.token().is_cancelled() {
            if !(tick(&self.setup, &self.ops, &self.owners.mint()).await
                || remap_tick(&self.setup, &self.ops).await)
            {
                break;
            }
        }
        Ok(Next::Continue)
    }
}

/// Requeue the re-identifications an earlier process left running. False
/// when it did not run; the next turn tries again.
async fn recover_at_start(ops: &Operations) -> bool {
    let ops = ops.clone();
    match blocking("re-identification start recovery", move || {
        ops.recover_at_start()
    })
    .await
    {
        Some(requeued) => {
            if requeued > 0 {
                tracing::info!(
                    requeued,
                    "re-identifications from the previous run requeued"
                );
            }
            true
        }
        None => false,
    }
}

/// One step on a blocking thread: the store is synchronous SQLite. True
/// when a job was worked on.
pub async fn tick(setup: &LibrarySetup, ops: &Operations, worker: &str) -> bool {
    let (setup, ops, worker) = (setup.clone(), ops.clone(), worker.to_owned());
    let stepped = blocking("library operation step", move || {
        if !setup.coordinator.current().is_empty() {
            return Ok(None);
        }
        ops.run_next(&worker)
    })
    .await;
    match stepped {
        Some(Some(job)) => {
            tracing::info!(
                job_id = job.id,
                state = job.state.as_str(),
                code = job.terminal_code.as_deref().unwrap_or(""),
                "library operation step finished"
            );
            true
        }
        _ => false,
    }
}

/// Run `work` on a blocking thread. `None` when it failed or panicked,
/// logged as `what`.
async fn blocking<T: Send + 'static>(
    what: &'static str,
    work: impl FnOnce() -> Result<T, models::OperationError> + Send + 'static,
) -> Option<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, "{what} failed");
            None
        }
        Err(error) => {
            tracing::error!(%error, "{what} panicked");
            None
        }
    }
}

/// Place the files of one album whose chosen edition still waits for them.
/// True when one was handled.
async fn remap_tick(setup: &LibrarySetup, ops: &Operations) -> bool {
    let (setup, ops) = (setup.clone(), ops.clone());
    let handle = tokio::runtime::Handle::current();
    let outcome = tokio::task::spawn_blocking(move || {
        if !setup.coordinator.current().is_empty() {
            return Ok(false);
        }
        droppedneedle_kernel::db::block_on(&handle, "operations_remap", ops.remap_next())?
    })
    .await;
    match outcome {
        Ok(Ok(handled)) => handled,
        Ok(Err(error)) => {
            tracing::warn!(%error, "placing files on a chosen edition failed");
            false
        }
        Err(error) => {
            tracing::error!(%error, "placing files on a chosen edition panicked");
            false
        }
    }
}
