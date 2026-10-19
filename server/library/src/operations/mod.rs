//! Library operation jobs: long-running administrator work an
//! administrator can watch, pause, resume, and stop.
//!
//! Jobs live in `library_operation_jobs`. The worker here runs
//! re-identifications: a person retries a review, and the job hands the
//! album to the identification queue, which settles it by its usual
//! rules. The same module undoes an automatic edition and searches
//! MusicBrainz releases for the edition finder.
//!
//! Layout: `models` holds the types, `control` the pause, resume, and stop
//! rules, `store` the job SQL, `decisions` the administrator's catalog
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

use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};

use crate::wiring::LibrarySetup;
use service::Operations;

/// How often the worker looks for due jobs.
const POLL: Duration = Duration::from_secs(2);

/// The operations worker's name on the supervisor.
pub const JOB_NAME: &str = "library-operations";

/// Start the operations worker (re-identifications, then repairs, then
/// edition remaps): one job at a time, never while a scan runs (the scan
/// may be changing the very files a job reads).
pub(crate) fn start(setup: &LibrarySetup, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        JOB_NAME,
        Schedule::new(POLL),
        OperationsTick {
            ops: Operations::new(setup),
            repairs: crate::repairs::service::Repairs::new(setup),
            worker: uuid::Uuid::new_v4().to_string(),
            setup: setup.clone(),
        },
    )
}

/// One turn: work steps until none is due.
struct OperationsTick {
    setup: LibrarySetup,
    ops: Operations,
    repairs: crate::repairs::service::Repairs,
    worker: String,
}

impl Tick for OperationsTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        while !ctx.token().is_cancelled() {
            if !(tick(&self.setup, &self.ops, &self.worker).await
                || crate::repairs::tick(&self.setup, &self.repairs, &self.worker).await
                || remap_tick(&self.setup, &self.ops).await)
            {
                break;
            }
        }
        Ok(Next::Continue)
    }
}

/// One step on a blocking thread: the store is synchronous SQLite. True
/// when a job was worked on.
pub async fn tick(setup: &LibrarySetup, ops: &Operations, worker: &str) -> bool {
    let (setup, ops, worker) = (setup.clone(), ops.clone(), worker.to_owned());
    let outcome = tokio::task::spawn_blocking(move || {
        if !setup.coordinator.current().is_empty() {
            return Ok(None);
        }
        ops.run_next(&worker)
    })
    .await;
    match outcome {
        Ok(Ok(Some(job))) => {
            tracing::info!(
                job_id = job.id,
                state = job.state.as_str(),
                code = job.terminal_code.as_deref().unwrap_or(""),
                "library operation step finished"
            );
            true
        }
        Ok(Ok(None)) => false,
        Ok(Err(error)) => {
            tracing::warn!(%error, "library operation step failed");
            false
        }
        Err(error) => {
            tracing::error!(%error, "library operation step panicked");
            false
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
