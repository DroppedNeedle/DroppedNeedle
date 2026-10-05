//! Shared compat infrastructure: the policy both protocol shims inherit.
//!
//! Everything here is protocol-agnostic on purpose: CORS,
//! case-insensitive paths, rate limits plus auth backoff, log redaction,
//! auth posture, and the advertised extension set. The Subsonic and
//! Jellyfin routers consume these; they never fork them. Kill switches
//! and per-endpoint guards live in each protocol module (a single copy
//! each, so the transcode-hint mapping cannot drift between two
//! spellings).
//!
//! Ported from v2's compat package, cited per item. Where v2 disagreed with itself (the `transcoding` advert),
//! see [`extensions`] for the one documented choice.

pub mod auth;
pub mod cors;
pub mod extensions;
pub mod path_case;
pub mod ratelimit;
pub mod redact;
