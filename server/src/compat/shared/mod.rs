//! Shared compat infrastructure: the policy both protocol shims inherit.
//!
//! Stage 9 slice. Everything here is protocol-agnostic on purpose: CORS,
//! case-insensitive paths, rate limits plus auth backoff, log redaction,
//! auth posture, and the advertised extension set. The Subsonic and
//! Jellyfin routers consume these; they never fork them. Kill switches
//! and per-endpoint guards live in the slices themselves (the one-time
//! shared duplicates are deleted, not forked: a single copy each, so the
//! transcode-hint mapping cannot drift between two spellings).
//!
//! Provenance is v2 `backend/api/compat/` plus `stage0-compat.md`, cited
//! per item. Where v2 disagreed with itself (the `transcoding` advert),
//! see [`extensions`] for the one documented choice.

pub mod auth;
pub mod cors;
pub mod extensions;
pub mod path_case;
pub mod ratelimit;
pub mod redact;
