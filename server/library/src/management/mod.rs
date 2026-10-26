//! Library Management jobs: pick albums, artists, roots or tracks, plan
//! every file from the chosen profile (tags from the album's edition,
//! paths from the naming script), show the preview page by page, then
//! apply it through the staged publisher one album at a time. Every
//! file that is left alone carries one reason: a stable code, a plain
//! sentence, and what to do about it. An applied job can be undone the
//! same way: an undo preview, then apply.
//!
//! Layout: `selection` turns a scope into albums and files, `profile`
//! turns a profile into tags and paths, `script` is the naming and
//! tagging language, `planner` plans one album, `store` is the snapshot
//! and plan SQL (the job rows go through [`crate::jobs`]),
//! `reasons` the per-file reasons, and `service` the entry points the
//! handlers and the worker call. The other kinds of job sit next to it:
//! `tag_edit` (the tag editor), `edition_tags` (an edition's tags written
//! from the edition sheet), `restore` (back to the originals, and
//! purging them), `duplicates` (a file whose place is taken), `activation`
//! (the dry run that turns automatic management on), `automatic` (runs
//! after a scan or an import), `after_apply` (bookkeeping and media-server
//! refreshes), `recovery` (writes that need an administrator),
//! `placement` (how safely each library folder can be written to), and
//! `takeover` (albums whose files were changed outside the app, which
//! automatic management then leaves alone).

pub mod activation;
pub mod after_apply;
pub mod automatic;
pub mod custom;
pub mod duplicates;
pub mod edition_tags;
pub mod enrich;
pub mod genre;
pub mod models;
pub mod placement;
pub mod planner;
pub mod profile;
pub mod reasons;
pub mod recovery;
pub mod restore;
pub mod script;
pub mod selection;
pub mod service;
pub mod settings;
pub(crate) mod sql;
pub mod store;
pub mod tag_edit;
pub mod takeover;

use std::time::Duration;

use droppedneedle_kernel::jobs::queue::lease::Owner;
use droppedneedle_kernel::jobs::schedule::SplitMix64;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, CancellationToken, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};

use crate::jobs::MANAGEMENT_WAKEUP;
use models::ManagementError;
use service::Management;

/// How long the worker waits for a wakeup before it looks anyway: a
/// queued job wakes it, while automatic management and media-server
/// refreshes come due with time.
const IDLE_CEILING: Duration = Duration::from_secs(2);

/// After a step the database was too busy for, the worker waits this
/// long, plus up to as much again.
const BUSY_RETRY: Duration = Duration::from_secs(5);

/// What one worker step did.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Worked,
    Idle,
    /// The database was too busy: what the step held went back.
    Busy,
}

/// The management worker's name on the supervisor.
pub const JOB_NAME: &str = "library-management";

/// Start the management worker: plans and applies jobs, starts automatic
/// management, and tells media servers to rescan. It has its own loop so
/// a long preview never holds up re-identification jobs. Its first turn
/// runs at once and requeues the jobs an earlier process left running;
/// after that a queued job wakes it.
pub fn start(management: Management, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
    let mut wake = management.setup.wakeups.subscribe(MANAGEMENT_WAKEUP);
    wake.mark_changed();
    supervisor.on_wakeup(
        JOB_NAME,
        wake,
        Schedule::new(IDLE_CEILING),
        ManagementTick {
            management,
            owners: Owner::new(),
            recovered: false,
            settle: automatic::Settle::default(),
            rng: SplitMix64::new(SplitMix64::seed_from_time()),
        },
    )
}

/// One turn: work steps until none is due. A busy database waits a few
/// seconds and is not a failed turn.
struct ManagementTick {
    management: Management,
    /// Each step claims under an owner never used before.
    owners: Owner,
    /// Whether start recovery ran.
    recovered: bool,
    settle: automatic::Settle,
    rng: SplitMix64,
}

impl Tick for ManagementTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        if !self.recovered {
            self.recovered = recover_at_start(&self.management).await;
        }
        while !ctx.token().is_cancelled() {
            let mut step = tick(&self.management, &self.owners.mint(), ctx.token()).await;
            if step == Step::Idle {
                step = automatic_tick(&self.management, &mut self.settle).await;
            }
            if step == Step::Idle {
                step = refresh_tick(&self.management, &self.owners.mint()).await;
            }
            match step {
                Step::Worked => {}
                Step::Idle => break,
                Step::Busy => {
                    ctx.sleep(BUSY_RETRY + self.rng.below_duration(BUSY_RETRY))
                        .await;
                    break;
                }
            }
        }
        Ok(Next::Continue)
    }
}

/// Requeue the jobs an earlier process left running. False when it did
/// not run; the next turn tries again.
async fn recover_at_start(management: &Management) -> bool {
    let management = management.clone();
    let step = blocking("management start recovery", move || {
        let requeued = management.recover_at_start()?;
        if requeued > 0 {
            tracing::info!(requeued, "management jobs from the previous run requeued");
        }
        Ok(true)
    })
    .await;
    step == Step::Worked
}

/// One worker step on a blocking thread: plan or apply the next queued
/// management job. A shutdown hands the job back to the queue between
/// albums, so the next start picks it up.
async fn tick(management: &Management, worker: &str, shutdown: &CancellationToken) -> Step {
    let management = management.clone().stopping_with(shutdown.clone());
    let worker = worker.to_owned();
    blocking("library management step", move || {
        management.run_next(&worker)
    })
    .await
}

/// Start automatic management for the next album that waits for it, once
/// the catalog has settled.
async fn automatic_tick(management: &Management, settle: &mut automatic::Settle) -> Step {
    let management = management.clone();
    let mut state = std::mem::take(settle);
    let outcome = tokio::task::spawn_blocking(move || {
        let result = management.automatic_step(&mut state);
        (state, result)
    })
    .await;
    match outcome {
        Ok((state, result)) => {
            *settle = state;
            settled("automatic management step", result)
        }
        Err(error) => {
            tracing::error!(%error, "automatic management step panicked");
            Step::Idle
        }
    }
}

/// Send one due media-server refresh.
async fn refresh_tick(management: &Management, worker: &str) -> Step {
    let management = management.clone();
    let worker = worker.to_owned();
    blocking("media server refresh", move || {
        management.deliver_refresh(&worker)
    })
    .await
}

async fn blocking(
    what: &'static str,
    work: impl FnOnce() -> Result<bool, ManagementError> + Send + 'static,
) -> Step {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => settled(what, result),
        Err(error) => {
            tracing::error!(%error, "{what} panicked");
            Step::Idle
        }
    }
}

/// Whether a wait's cause is already logged once where it arises: no
/// recycle bin is known while a setting cannot be read, which the bins'
/// source logs on each change.
pub(crate) fn logged_elsewhere(cause: &str) -> bool {
    cause == crate::mutations::RECYCLE_BIN_UNKNOWN.message
}

/// What a step's answer means for the loop.
fn settled(what: &'static str, result: Result<bool, ManagementError>) -> Step {
    match result {
        Ok(true) => Step::Worked,
        Ok(false) => Step::Idle,
        Err(ManagementError::Busy(cause)) => {
            if logged_elsewhere(&cause) {
                tracing::debug!("{what} waits: {cause}");
            } else {
                tracing::warn!("{what} waits: {cause}");
            }
            Step::Busy
        }
        Err(error) => {
            tracing::warn!(%error, "{what} failed");
            Step::Idle
        }
    }
}
