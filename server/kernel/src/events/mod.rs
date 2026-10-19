//! Live events: the in-process hub and the loops that turn database
//! commits into events. The `GET /api/v3/events/stream` route lives in
//! `auth::session::event_stream`.
//!
//! Producers publish typed [`Event`]s into one [`EventHub`]; each signed-in
//! tab holds one Server-Sent Events stream that receives the events meant
//! for everyone plus its own user's notices. The event names and payloads
//! are v2's, so the web UI's listeners work as they did.
//!
//! Who publishes what:
//! - `activity.changed`: [`revisions::start`], from the durable library
//!   revisions (scans bump `scan`, identification bumps `identification`,
//!   catalog writes bump `catalog`), read after the database commits.
//! - `snapshot`: the presence registry, on every presence change.
//! - `wanted_*`, `auto_download_enqueued`, `request_imported`: the
//!   acquisition flows, through their tick sink.
//! - `playlist_imported`: the Spotify playlist import.
//! - `download_progress`: the download worker, after each poll of a live
//!   transfer.
//! - `downloads.changed`: [`downloads::start`], from the durable download
//!   activity revision.
//! - `cache.sync`: the library image precache, as it moves.
//! - `concerts_new`, `personal_mix_refreshed`, `drop_import_updated`:
//!   their features call [`EventHub::notify`] with the matching
//!   [`UserNotice`].
//!
//! The hub is made once, in the [`Platform`](crate::platform::Platform),
//! and every publisher takes it when it is built.

pub mod downloads;
mod follow;
pub mod hub;
pub mod model;
pub mod revisions;

pub use hub::{EventHub, Subscription};

/// The event loops on the supervisor, always on from boot.
pub const LOOPS: &[&str] = &[revisions::JOB_NAME, downloads::JOB_NAME];
pub use model::{
    ActivityChanged, AutoDownloadEnqueued, CacheSyncProgress, ConcertsNew, DownloadProgress,
    DownloadsChanged, DropImportUpdated, Event, NowPlayingEntry, NowPlayingSnapshot,
    PersonalMixRefreshed, PlaylistImported, RequestImported, SearchJobUpdated, StreamEvent,
    UserNotice, WantedNotice, new_event_id,
};
