//! Stage-6 remote sources: one adapter for Jellyfin, Navidrome, and Plex (R1).
//!
//! The v2 backend carried three parallel browse stacks (~86 endpoints);
//! this slice collapses them to one adapter surface and one set of native
//! `/api/v3` routes with unified shapes. [`RemoteHandle`](adapter::RemoteHandle)
//! is the single browse entry point: per-source adapters translate their
//! server's payloads into the [`models`] views, and handlers resolve a
//! handle per request from the caller's stored connection.
//!
//! Live-version-cited quirks ported from v2 (each cited at its call site):
//! Jellyfin 10.11 `MediaBrowser` auth (issue #151), the Navidrome 0.62.0
//! single-folder probe (repeated/unknown `musicFolderId`), the Plex
//! http-to-https base upgrade and playlist-composite fallback, and the
//! Subsonic/Jellyfin/Plex envelope and pagination rules.
//!
//! Layout: `adapter` holds the unified surface plus the import sink,
//! `jellyfin`/`navidrome`/`plex` hold the source clients, `connections`
//! and `folders` hold per-user credentials (sealed with the stage-2
//! secrets core) and Navidrome folder preferences, `handlers` owns the
//! routes, and `mocks` carries the loopback mock servers the parity
//! briefs run against. Wiring: the integrator mounts
//! [`remotes_router`](handlers::remotes_router) inside the session gate.

pub mod adapter;
pub mod connections;
pub mod error;
pub mod folders;
pub mod handlers;
pub mod jellyfin;
#[cfg(any(test, feature = "test-support"))]
pub mod mocks;
pub mod models;
pub mod navidrome;
pub mod plex;
pub mod reader;
