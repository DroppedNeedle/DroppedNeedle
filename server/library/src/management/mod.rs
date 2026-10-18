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
//! tagging language, `planner` plans one album, `store` is the job SQL,
//! `reasons` the per-file reasons, and `service` the entry points the
//! handlers and the worker call. The other kinds of job sit next to it:
//! `tag_edit` (the tag editor), `restore` (back to the originals, and
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
pub mod store;
pub mod tag_edit;
pub mod takeover;

use std::time::Duration;

use droppedneedle_kernel::jobs::schedule::SplitMix64;
use tokio::sync::watch;

use crate::wiring::LibrarySetup;
use models::ManagementError;
use service::Management;

/// How often the management worker looks for work when idle.
const POLL: Duration = Duration::from_secs(2);

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

/// Spawn the management worker: plans and applies jobs, starts automatic
/// management, and tells media servers to rescan. It has its own loop so
/// a long preview never holds up re-identification jobs.
pub fn spawn_loop(
    setup: &LibrarySetup,
    shutdown: watch::Receiver<bool>,
) -> (&'static str, tokio::task::JoinHandle<()>) {
    let setup = setup.clone();
    (
        "library-management",
        tokio::spawn(run_loop(setup, shutdown)),
    )
}

async fn run_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    let worker = uuid::Uuid::new_v4().to_string();
    let mut settle = automatic::Settle::default();
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    loop {
        if *shutdown.borrow() {
            break;
        }
        let mut step = tick(&setup, &worker, &shutdown).await;
        if step == Step::Idle {
            step = automatic_tick(&setup, &mut settle).await;
        }
        if step == Step::Idle {
            step = refresh_tick(&setup, &worker).await;
        }
        let wait = match step {
            Step::Worked => continue,
            Step::Idle => POLL,
            Step::Busy => BUSY_RETRY + rng.below_duration(BUSY_RETRY),
        };
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// One worker step on a blocking thread: plan or apply the next queued
/// management job. A shutdown hands the job back to the queue between
/// albums, so the next start picks it up.
async fn tick(setup: &LibrarySetup, worker: &str, shutdown: &watch::Receiver<bool>) -> Step {
    let management = Management::new(setup).stopping_with(shutdown.clone());
    let worker = worker.to_owned();
    blocking("library management step", move || {
        management.run_next(&worker)
    })
    .await
}

/// Start automatic management for the next album that waits for it, once
/// the catalog has settled.
async fn automatic_tick(setup: &LibrarySetup, settle: &mut automatic::Settle) -> Step {
    let management = Management::new(setup);
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
async fn refresh_tick(setup: &LibrarySetup, worker: &str) -> Step {
    let management = Management::new(setup);
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

/// What a step's answer means for the loop.
fn settled(what: &'static str, result: Result<bool, ManagementError>) -> Step {
    match result {
        Ok(true) => Step::Worked,
        Ok(false) => Step::Idle,
        Err(error @ ManagementError::Busy(_)) => {
            tracing::warn!(%error, "{what} waits: the database is busy");
            Step::Busy
        }
        Err(error) => {
            tracing::warn!(%error, "{what} failed");
            Step::Idle
        }
    }
}
