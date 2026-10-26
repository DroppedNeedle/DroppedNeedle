//! Identity repairs, identity preparations, catalog cleanup and scan
//! diagnostics.
//!
//! - An identity repair rechecks albums matched automatically (or carried
//!   over from v2) against MusicBrainz. Its report says, per album, whether
//!   the match still holds; Apply removes the matches whose files fit
//!   nothing on the release, and the album is matched again.
//! - An identity preparation gets albums ready for Library Management:
//!   each needs an exact release with every file placed on its track.
//!   Apply places the files where the release supports it and uses a
//!   suggested edition where the album has none. With automatic edition
//!   acceptance on, a suggestion that passes v2's gate is applied during
//!   the check itself, as a confirmed automatic match.
//! - Catalog cleanup tidies album rows: once after an upgrade from v2,
//!   then after each scan that ends (for the albums it touched) and after
//!   identification finishes with an album.
//! - Play filing gives plays that name no track the track, album and
//!   artist they were plays of, when exactly one track fits: at a server
//!   start and after each scan that ends.
//!
//! All four are operation jobs (`library_operation_jobs`, kind `repair`)
//! run one at a time by their own loop, never during a scan, and
//! controlled like any other job. They are claimed, kept and recovered
//! through [`crate::jobs`], and a queued or resumed job wakes the loop.
//! A repair check changes nothing; Apply
//! and automatic acceptance do, and each edition they change can be
//! undone from the album's edition panel (an automatic acceptance also
//! from the readiness report). Detaches, placements and catalog cleanup
//! write an audit row in `library_catalog_actions`.
//!
//! Layout: `models` and `reasons` hold the types and the wording,
//! `classify` the per-album verdicts, `store` the SQL, `apply` and
//! `hygiene` the catalog changes, `plays` the play filing, `service` the
//! entry points, and `diagnostics` the scan run export.

pub mod apply;
pub mod classify;
pub mod diagnostics;
pub mod hygiene;
pub mod models;
pub mod plays;
pub mod reasons;
pub mod service;
pub mod store;

use std::time::Duration;

use droppedneedle_kernel::jobs::queue::lease::Owner;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};

use crate::jobs::REPAIRS_WAKEUP;
use crate::operations::models::OperationError;
use service::RepairWorker;

/// How long the worker waits for a wakeup before it looks anyway: a
/// queued or resumed job wakes it, while the cleanups and play filings
/// that follow a scan or an identification are found by looking.
const IDLE_CEILING: Duration = Duration::from_secs(2);

/// The repairs worker's name on the supervisor.
pub const JOB_NAME: &str = "library-repairs";

/// Start the repairs worker on its own loop, so a long check never holds
/// up re-identifications. Its first turn runs at once and requeues the
/// repair jobs an earlier process left running; after that a queued job
/// wakes it.
pub fn start(worker: RepairWorker, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
    let mut wake = worker.wakeups().subscribe(REPAIRS_WAKEUP);
    wake.mark_changed();
    supervisor.on_wakeup(
        JOB_NAME,
        wake,
        Schedule::new(IDLE_CEILING),
        RepairsTick {
            worker,
            owners: Owner::new(),
            recovered: false,
        },
    )
}

/// One turn: work jobs until none is due.
struct RepairsTick {
    /// Kept across turns: it remembers which scans it already followed up.
    worker: RepairWorker,
    /// Each job is claimed under an owner never used before.
    owners: Owner,
    /// Whether start recovery ran.
    recovered: bool,
}

impl Tick for RepairsTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let worker = self.worker.stopping_with(ctx.token().clone());
        if !self.recovered {
            self.recovered = recover_at_start(&worker).await;
        }
        while !ctx.token().is_cancelled() {
            if !tick(&worker, &self.owners.mint()).await {
                break;
            }
        }
        Ok(Next::Continue)
    }
}

/// Requeue the repair jobs an earlier process left running. False when it
/// did not run; the next turn tries again.
async fn recover_at_start(worker: &RepairWorker) -> bool {
    let worker = worker.clone();
    match blocking(move || Ok(Some(worker.recover_at_start()?))).await {
        Some(requeued) => {
            if requeued > 0 {
                tracing::info!(requeued, "repair jobs from the previous run requeued");
            }
            true
        }
        None => false,
    }
}

/// Claim the next due repair job and work on it until it is done, waits,
/// or is halted. True when a job was worked on. Never runs during a scan.
pub async fn tick(repairs: &RepairWorker, worker: &str) -> bool {
    let (repairs, worker) = (repairs.clone(), worker.to_owned());
    let handle = tokio::runtime::Handle::current();
    let worked = blocking(move || {
        if repairs.scanning() {
            return Ok(None);
        }
        droppedneedle_kernel::db::block_on(&handle, "repairs_turn", repairs.run_next(&worker))?
    })
    .await;
    match worked {
        Some(job) => {
            tracing::debug!(
                job_id = job.id,
                state = job.state.as_str(),
                code = job.terminal_code.as_deref().unwrap_or(""),
                "repair job left the worker"
            );
            true
        }
        None => false,
    }
}

/// Run `work` on a blocking thread: the store is synchronous SQLite.
/// `None` when it had nothing, failed or panicked; a failure is logged.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<Option<T>, OperationError> + Send + 'static,
) -> Option<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            tracing::warn!(%error, "repair step failed");
            None
        }
        Err(error) => {
            tracing::error!(%error, "repair step panicked");
            None
        }
    }
}
