//! Playback reporting: session lifecycle, native scrobbles, live
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
//! - Warmup cadences (Jellyfin one-shot after 8s; Navidrome/Plex every
//!   four hours after 12s/15s): v2's task schedule
//!
//! ## Wiring
//!
//! Mounted as `playback` under `/api/v3` inside the session gate (next to
//! the reads nest). `GET /now-playing` serves the live presence registry
//! here, not the static snapshot in `reads::discover`. Utoipa paths are
//! registered in `docs.rs`, and the warmup loops spawn in `main.rs` beside
//! the discover refresh loops over the same shutdown watch (handles
//! awaited after axum serves).
//!
//! The 4s presence poll loop (v2 `run_now_playing_presence_loop`) lives in
//! `jobs::presence`; `jobs::media` feeds it this registry and the
//! Jellyfin/Navidrome/Plex session pollers. Scrobble forwarding to Last.fm
//! and ListenBrainz drains through `forwarding`. Not built yet: the SSE
//! fan-out that would consume the registry's generation.

pub mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod forwarding;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod reports;
pub mod scrobble_models;
pub mod services;
pub mod sqlite;
pub mod warmup;

pub use handlers::playback_router;
pub use services::PlaybackDeps;
pub use warmup::{TokioSleeper, WarmupHandles, WarmupScope, WarmupStats, spawn_warmup_loops};
