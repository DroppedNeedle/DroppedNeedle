//! Acquisition: requests, downloads, sources, flows, imports, landing.
//!
//! Seven areas plus a unification layer:
//!
//! - [`requests`] serves the user-facing ask/approval/wanted surface.
//! - [`downloads`] owns the durable task journal, manifests, watchdog
//!   math, recovery classification, and quarantine.
//! - [`slskd`] and [`usenet`] own the source clients and indexers.
//! - [`flows`] runs the wanted watcher, follow poll, upgrade sweep,
//!   status sync, and the free-music/drop-import operations.
//! - [`imports`] serves Lidarr/Spotify import plus the health smoke.
//! - [`landing`] verifies a finished download, matches it to the
//!   requested release, and publishes it into the library or holds it.
//! - [`target`] works out what a task fetches (its edition, tracklist and
//!   wanted tracks; a single track is fetched as part of its album), with
//!   [`edition`] answering which edition the library has chosen.
//!
//! The `db`, `dispatch`, `search`, `sources`, `settings`, `probes`,
//! `worker`, and `wiring` modules unify the per-area seams onto one
//! production spelling each (a single
//! [`dispatch::UnifiedDispatch`] implements both dispatch traits, one
//! [`search::FanoutSearch`] serves the candidate seam, and so on) and
//! assemble the [`wiring::AcquireSetup`] bundle the app mounts.

pub mod db;
pub mod dispatch;
pub mod downloads;
pub mod edition;
pub mod flows;
pub mod imports;
pub mod landing;
pub mod plugin_events;
pub mod plugin_source;
pub mod probes;
pub mod requests;
pub mod search;
pub mod settings;
pub mod slskd;
pub mod sources;
pub mod target;
pub mod usenet;
pub mod wiring;
pub mod worker;

pub use wiring::AcquireSetup;
