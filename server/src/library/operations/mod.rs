//! Library operation jobs: long-running administrator work an
//! administrator can watch, pause, resume, and stop.
//!
//! Jobs live in `library_operation_jobs`. The worker here runs explicit
//! re-identifications: an administrator asks for another look at one
//! album, the job scores every candidate release, and waits until the
//! administrator picks one (or keeps the album as it is). The same module
//! undoes an automatic edition and searches MusicBrainz releases for the
//! edition finder.
//!
//! Layout: `models` holds the types, `control` the pause, resume, and stop
//! rules, `store` the job SQL, `decisions` the administrator's catalog
//! decisions, `reidentify` the candidate evaluation, and `service` the
//! entry points the HTTP handlers and the loop call.

pub mod control;
pub mod decisions;
pub mod models;
pub mod reidentify;
pub mod service;
pub mod store;

use std::time::Duration;

use tokio::sync::watch;

use crate::library::wiring::LibrarySetup;
use service::Operations;

/// How often the worker looks for due jobs.
const POLL: Duration = Duration::from_secs(2);

/// Spawn the operations worker loop: one job at a time, never while a
/// scan runs (the scan may be changing the very files a job reads).
pub fn spawn_loop(
    setup: &LibrarySetup,
    shutdown: watch::Receiver<bool>,
) -> (&'static str, tokio::task::JoinHandle<()>) {
    let setup = setup.clone();
    (
        "library-operations",
        tokio::spawn(run_loop(setup, shutdown)),
    )
}

async fn run_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    let ops = Operations::new(&setup);
    let worker = uuid::Uuid::new_v4().to_string();
    loop {
        if *shutdown.borrow() {
            break;
        }
        if tick(&setup, &ops, &worker).await {
            continue;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(POLL) => {}
        }
    }
}

/// One step on a blocking thread: the store is synchronous SQLite. True
/// when a job was worked on.
pub async fn tick(setup: &LibrarySetup, ops: &Operations, worker: &str) -> bool {
    let (setup, ops, worker) = (setup.clone(), ops.clone(), worker.to_owned());
    let handle = tokio::runtime::Handle::current();
    let outcome = tokio::task::spawn_blocking(move || {
        if !setup.coordinator.current().is_empty() {
            return Ok(None);
        }
        handle.block_on(ops.run_next(&worker))
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
