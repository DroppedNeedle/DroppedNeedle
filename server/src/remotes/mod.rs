//! Remote sources: one adapter for Jellyfin, Navidrome, and Plex.
//!
//! The v2 backend carried three parallel browse stacks (~86 endpoints);
//! v3 collapses them to one adapter surface and one set of native
//! `/api/v3` routes with unified shapes. [`RemoteHandle`](adapter::RemoteHandle)
//! is the single browse entry point: per-source adapters translate their
//! server's payloads into the [`models`] views.
//!
//! Live-version-cited quirks ported from v2 (each cited at its call site):
//! Jellyfin 10.11 `MediaBrowser` auth (issue #151), the Navidrome 0.62.0
//! single-folder probe (repeated/unknown `musicFolderId`), the Plex
//! http-to-https base upgrade and playlist-composite fallback, and the
//! Subsonic/Jellyfin/Plex envelope and pagination rules.
//!
//! Layout: `handlers` parse and render; `service` resolves the caller's
//! connection and runs the call; `connections` holds the admin servers
//! (read from settings per call) and the per-user links (sealed rows in
//! `user_connections`); `folders` holds the Navidrome folder preferences;
//! `adapter` holds the unified surface plus the import sink;
//! `jellyfin`/`navidrome`/`plex` hold the source clients; `analytics`
//! summarizes listening history; `reader` serves the stream gateway; and
//! `mocks` carries the loopback mock servers the tests run against.
//! `MediaSetup` mounts [`remotes_router`](handlers::remotes_router)
//! inside the session gate.

pub mod adapter;
pub mod analytics;
pub mod connections;
pub mod error;
pub mod folders;
pub mod handlers;
pub mod jellyfin;
pub mod jellyfin_models;
#[cfg(any(test, feature = "test-support"))]
pub mod mocks;
pub mod models;
pub mod navidrome;
pub mod plex;
pub mod reader;
pub mod service;
