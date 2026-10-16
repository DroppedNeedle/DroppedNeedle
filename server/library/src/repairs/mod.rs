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
//! run by the operations loop in short turns, never during a scan, and
//! controlled like any other job. A repair check changes nothing; Apply
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

use crate::wiring::LibrarySetup;
use service::Repairs;

/// One worker turn on a blocking thread: the store is synchronous SQLite.
/// True when a repair job was worked on. Never runs during a scan.
pub async fn tick(setup: &LibrarySetup, repairs: &Repairs, worker: &str) -> bool {
    let (setup, repairs, worker) = (setup.clone(), repairs.clone(), worker.to_owned());
    let handle = tokio::runtime::Handle::current();
    let outcome = tokio::task::spawn_blocking(move || {
        if !setup.coordinator.current().is_empty() {
            return Ok(None);
        }
        handle.block_on(repairs.run_next(&worker))
    })
    .await;
    match outcome {
        Ok(Ok(Some(job))) => {
            tracing::debug!(
                job_id = job.id,
                state = job.state.as_str(),
                code = job.terminal_code.as_deref().unwrap_or(""),
                "repair turn finished"
            );
            true
        }
        Ok(Ok(None)) => false,
        Ok(Err(error)) => {
            tracing::warn!(%error, "repair turn failed");
            false
        }
        Err(error) => {
            tracing::error!(%error, "repair turn panicked");
            false
        }
    }
}
