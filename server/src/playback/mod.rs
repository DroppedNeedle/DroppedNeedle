//! Stage-6 playback reporting: session lifecycle, native scrobbles, live
//! presence, and MBID warmup loops.
//!
//! The native player reports `start`/`progress`/`stop` for each play; the
//! services resolve the track, keep the bounded session, write presence,
//! attribute the play to its remote server (Jellyfin/Navidrome/Plex), and
//! count a scrobble past threshold. Direct submits and now-playing forwards
//! carry the by-name native path to Last.fm/ListenBrainz. Presence writes
//! and the live snapshot live here too, backed by the in-memory registry.
//!
//! ## V2 rule map
//!
//! Every behavior is a v2 port; the citations sit beside the code:
//!
//! - Session lifecycle (start resets, rewind restarts, stop thresholds,
//!   once-only submits): `backend/services/compat/playback_report_service.py`
//! - Session-driven scrobbles (client normalization, 5s mixed-report
//!   dedup, presence writes, played-at backdating):
//!   `backend/services/compat/compat_scrobble_adapter.py`
//! - Native submit/forward (hour dedup, always-record history, 30s gate,
//!   Navidrome delegation, accepted quirks):
//!   `backend/services/scrobble_service.py`
//! - Native threshold (omitted position counts, past 90%, within the last
//!   second): `backend/api/compat/jellyfin/router.py::_should_scrobble`
//! - Presence (45s TTL, owner-keyed visibility with fail-closed redaction,
//!   silent idle reconciles, publish-if-removed sweeps):
//!   `backend/services/now_playing_service.py` and `now_playing_poller.py`
//! - Outbound attribution (per-source session reports, logged and
//!   swallowed; per-user fail-closed): the three
//!   `backend/services/*_playback_service.py` modules
//! - Warmup cadences (Jellyfin one-shot after 8s; Navidrome/Plex every
//!   four hours after 12s/15s): `backend/core/tasks.py`
//!
//! ## Wiring
//!
//! Mounted as `playback` under `/api/v3` inside the session gate (next to
//! the reads nest), with the stage-4 static snapshot retired:
//! `now_playing_router` is removed from the `reads_router` merge and
//! `GET /now-playing` serves this slice's live registry (same JSON shape).
//! Utoipa paths are registered in `docs.rs`, and the warmup loops spawn in
//! `main.rs` beside the stage-5 refresh loops over the same shutdown watch
//! (handles awaited after axum serves).
//!
//! Follow-ups deliberately left out: the 4s presence poll loop (v2
//! `run_now_playing_presence_loop`) needs remote-session fetchers that do
//! not exist behind these traits yet, so `reconcile_source` and `sweep`
//! wait for it; the SSE fan-out consumes the registry's generation; and
//! the compat inbound endpoints (Subsonic/Jellyfin reports) will call the
//! session-driven scrobble entry with their own thresholds.

pub mod error;
pub mod fakes;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod reports;
pub mod services;
pub mod sqlite;
pub mod warmup;

pub use handlers::playback_router;
pub use services::PlaybackDeps;
// The standalone briefs below reach the warmup items through their module
// paths; the integrator-facing re-exports stay for the wired build.
#[allow(unused_imports)]
pub use warmup::{TokioSleeper, WarmupHandles, WarmupScope, WarmupStats, spawn_warmup_loops};
