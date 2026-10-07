//! Live events: the in-process hub, the `GET /api/v3/events/stream` route
//! and the library revision poller.
//!
//! Producers publish typed [`Event`]s into one [`EventHub`]; each signed-in
//! tab holds one Server-Sent Events stream that receives the events meant
//! for everyone plus its own user's notices. The event names and payloads
//! are v2's, so the web UI's listeners work as they did.
//!
//! Who publishes what:
//! - `activity.changed`: [`revisions::run`], from the durable library
//!   revisions (scans bump `scan`, identification bumps `identification`,
//!   catalog writes bump `catalog`).
//! - `snapshot`: the presence registry, on every presence change.
//! - `wanted_*`, `auto_download_enqueued`, `request_imported`: the
//!   acquisition flows, through their tick sink.
//! - `playlist_imported`: the Spotify playlist import.
//! - `download_progress`: the download worker, after each poll of a live
//!   transfer.
//! - `downloads.changed`: [`downloads::run`], from the durable download
//!   activity revision.
//! - `concerts_new`, `personal_mix_refreshed`, `drop_import_updated`,
//!   `free_music_updated`: their features call [`EventHub::notify`] with
//!   the matching [`UserNotice`].
//!
//! Bundles built before the application state hold an [`EventSink`];
//! [`crate::AppState::with_events`] attaches the hub to all of them.

pub mod downloads;
pub mod http;
pub mod hub;
pub mod model;
pub mod revisions;

pub use http::{SessionCheck, router, session_check};
pub use hub::{EventHub, EventSink, Subscription};
pub use model::{
    ActivityChanged, AutoDownloadEnqueued, ConcertsNew, DownloadProgress, DownloadsChanged,
    DropImportUpdated, Event, FreeMusicUpdated, PersonalMixRefreshed, PlaylistImported,
    RequestImported, SearchJobUpdated, UserNotice, WantedNotice, new_event_id,
};
