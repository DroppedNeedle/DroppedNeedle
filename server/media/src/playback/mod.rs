//! Playback reporting: session lifecycle, native scrobbles, live
//! presence, and each user's listening settings.
//!
//! The native player reports `start`/`progress`/`stop` for each play; the
//! services resolve the track, keep the bounded session, write presence,
//! attribute the play to its remote server (Jellyfin/Navidrome/Plex), and
//! count a scrobble past threshold. Direct submits and now-playing forwards
//! carry the by-name native path to Last.fm/ListenBrainz. Presence writes
//! and the live snapshot live here too, backed by the in-memory registry.
//! A counted play is written with one scrobble job per service it goes
//! to, so a scrobble survives a restart and a service outage.
//!
//! ## V2 rule map
//!
//! Every behavior is a v2 port; the citations sit beside the code:
//!
//! - Session lifecycle (start resets, rewind restarts, stop thresholds,
//!   once-only submits): v2's compat playback report service
//! - Session-driven scrobbles (client normalization, 5s mixed-report
//!   dedup, presence writes, played-at backdating):
//!   v2's compat scrobble adapter
//! - Native submit/forward (hour dedup, always-record history, 30s gate,
//!   Navidrome delegation, accepted quirks):
//!   v2's scrobble service
//! - Native threshold (omitted position counts, past 90%, within the last
//!   second): v2's Jellyfin router `_should_scrobble`
//! - Presence (45s TTL, owner-keyed visibility with fail-closed redaction,
//!   silent idle reconciles, publish-if-removed sweeps):
//!   v2's now-playing service and poller
//! - Outbound attribution (per-source session reports, logged and
//!   swallowed; per-user fail-closed): the three
//!   v2 per-source playback services
//!
//! ## Wiring
//!
//! [`routes`] puts every route in the user group, inside the session gate.
//! `GET /now-playing` serves the live presence registry here, not the
//! static snapshot in `reads::discover`. [`listening_routes`] serves each
//! user's scrobble preferences and ListenBrainz link, kept by
//! [`listening::ListeningStore`].
//!
//! The 4s presence poll loop (v2 `run_now_playing_presence_loop`) lives in
//! `jobs::presence`; `remotes::presence_feed` feeds it this registry and the
//! Jellyfin/Navidrome/Plex session pollers. Scrobble jobs and now-playing
//! reports to Last.fm and ListenBrainz are sent by `forwarding`. Not built
//! yet: the SSE
//! fan-out that would consume the registry's generation.

pub mod error;
pub mod forwarding;
pub mod handlers;
pub mod listening;
pub mod listening_routes;
pub mod models;
pub mod plugin_events;
pub mod ports;
pub mod presence;
pub mod report;
pub mod reports;
pub mod rules;
pub mod scrobble_models;
pub mod sessions;
pub(crate) mod sql;
pub mod sqlite;

use std::sync::Arc;

use droppedneedle_auth::users::stores::LastFmSwitch;
use droppedneedle_kernel::clock::Clock;

pub use handlers::routes;

use ports::{PlayEvents, RemoteReporters, ScrobbleSinks};
use presence::PresenceRegistry;
use report::{MixedDedup, ScrobbleDedup};
use sessions::SessionStore;
use sqlite::PlaybackStore;

/// Everything the reporting services need, built once and cloned into
/// handlers. The database side is the one concrete store; only what lies
/// outside the process sits behind a trait. The session, presence, and
/// dedup state is process-local by design (v2 single-process invariant).
#[derive(Clone)]
pub struct PlaybackDeps {
    /// Tracks, play history, listening preferences and display names.
    pub store: Arc<PlaybackStore>,
    /// External now-playing sinks (Last.fm / ListenBrainz).
    pub sinks: Arc<dyn ScrobbleSinks>,
    /// The admin's Last.fm switch: plays go to Last.fm only while it is on.
    pub lastfm_switch: LastFmSwitch,
    /// Outbound remote attribution (Jellyfin / Navidrome / Plex).
    pub remotes: Arc<dyn RemoteReporters>,
    /// Bounded playback sessions.
    pub sessions: SessionStore,
    /// Live presence registry.
    pub presence: PresenceRegistry,
    /// Scrobble name dedup.
    pub dedup: ScrobbleDedup,
    /// Mixed-report dedup.
    pub mixed: MixedDedup,
    /// Clock for TTLs and timestamps.
    pub clock: Arc<dyn Clock>,
    /// Listeners for accepted plays (plugins).
    pub events: Arc<dyn PlayEvents>,
}
