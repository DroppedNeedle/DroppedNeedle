//! Acquisition: requests, downloads, sources, flows, imports, landing.
//!
//! Seven areas plus a unification layer:
//!
//! - [`requests`] serves the user-facing ask/approval/wanted surface.
//! - [`downloads`] owns the durable task journal, manifests, watchdog
//!   math, recovery classification, and quarantine.
//! - [`slskd`] and [`usenet`] own the source clients and indexers.
//! - [`flows`] runs the wanted watcher, follow poll, upgrade sweep and
//!   status sync.
//! - [`drop_import`] takes uploaded music into the library.
//! - [`imports`] serves Lidarr/Spotify import plus the health smoke.
//! - [`landing`] verifies a finished download, matches it to the
//!   requested release, and publishes it into the library or holds it.
//! - [`target`] works out what a task fetches (its edition, tracklist and
//!   wanted tracks; a single track is fetched as part of its album), with
//!   [`edition`] answering which edition the library has chosen.
//! - [`search_jobs`] runs manual album searches: every source's ranked
//!   candidates for one album, and the pick that starts a download.
//!
//! The `db`, `dispatch`, `search`, `sources`, `settings`, `probes`,
//! `worker`, and `wiring` modules unify the per-area seams onto one
//! production spelling each (one [`dispatch::DownloadDispatch`] starts
//! every download, one [`search::FanoutSearch`] serves the candidate
//! seam, and so on) and
//! assemble the [`wiring::AcquireSetup`] bundle the app mounts.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod db;
pub mod dispatch;
pub mod downloads;
pub mod drop_import;
pub mod edition;
pub mod flows;
pub mod imports;
pub mod landing;
pub mod library_removal;
pub mod plugin_events;
pub mod plugin_source;
pub mod probes;
pub mod requests;
pub mod search;
pub mod search_jobs;
pub mod settings;
pub mod slskd;
pub mod sources;
pub mod target;
pub mod usenet;
pub mod wiring;
pub mod worker;

pub use wiring::AcquireSetup;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![downloads::sql::registered()]
}
