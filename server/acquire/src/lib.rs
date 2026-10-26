//! Acquisition: requests, downloads, sources, flows, imports, landing.
//!
//! Seven areas plus a unification layer:
//!
//! - [`requests`] serves the user-facing ask/approval/wanted surface.
//! - [`downloads`] owns the durable task journal, task plans, watchdog
//!   math, recovery classification, and quarantine.
//! - [`slskd`] and [`usenet`] own the source clients and indexers, which
//!   all fail with one [`client_error::ClientError`].
//! - [`flows`] runs the wanted watcher, follow poll, upgrade sweep and
//!   request-status sweep.
//! - [`follows`] owns the follows, their auto-download approvals and the
//!   follow poll's cursors.
//! - [`drop_import`] takes uploaded music into the library.
//! - [`imports`] serves Lidarr/Spotify import plus the health smoke.
//! - [`landing`] verifies a finished download, matches it to the
//!   requested release, and publishes it into the library or holds it.
//! - [`intake`] places matched files in the library, the same way for
//!   every door they come through (a download, an upload, a held file).
//! - [`target`] works out what a task fetches (its edition, tracklist and
//!   wanted tracks; a single track is fetched as part of its album), with
//!   [`edition`] answering which edition the library has chosen.
//! - [`search_jobs`] runs manual album searches: every source's ranked
//!   candidates for one album, and the pick that starts a download.
//!
//! The `db`, `dispatch`, `search`, `sources`, `settings`, `probes`,
//! `readiness`, `worker`, and `wiring` modules unify the per-area seams onto one
//! production spelling each (one [`dispatch::DownloadDispatch`] starts
//! every download, every source is one
//! [`downloads::sources::DownloadSource`] in the worker's list, and so on)
//! and assemble the [`wiring::AcquireSetup`] bundle the app mounts.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod client_error;
pub mod db;
pub mod dispatch;
pub mod downloads;
pub mod drop_import;
pub mod edition;
pub mod flows;
pub mod follows;
pub mod imports;
pub mod intake;
pub mod landing;
pub mod library_ready;
pub mod library_removal;
pub mod plugin_events;
pub mod plugin_source;
pub mod probes;
pub mod readiness;
pub mod reason;
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
    vec![downloads::sql::registered(), requests::sql::registered()]
}
