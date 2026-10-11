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
//! handlers and the worker call.

pub mod models;
pub mod planner;
pub mod profile;
pub mod reasons;
pub mod script;
pub mod selection;
pub mod service;
pub mod store;

use tokio::sync::watch;

use crate::library::wiring::LibrarySetup;
use service::Management;

/// One worker step on a blocking thread: plan or apply the next queued
/// management job. A shutdown hands the job back to the queue between
/// albums, so the next start picks it up. True when a job was worked on.
pub async fn tick(setup: &LibrarySetup, worker: &str, shutdown: &watch::Receiver<bool>) -> bool {
    let management = Management::new(setup).stopping_with(shutdown.clone());
    let worker = worker.to_owned();
    match tokio::task::spawn_blocking(move || management.run_next(&worker)).await {
        Ok(Ok(worked)) => worked,
        Ok(Err(error)) => {
            tracing::warn!(%error, "library management step failed");
            false
        }
        Err(error) => {
            tracing::error!(%error, "library management step panicked");
            false
        }
    }
}
