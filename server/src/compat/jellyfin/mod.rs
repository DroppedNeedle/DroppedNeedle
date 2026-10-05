//! Jellyfin compat shim: PascalCase shapes, real statuses, anonymous audio.
//!
//! Ported from v2's Jellyfin compat package (router, models, builders,
//! auth, errors, serialization) and its client matrix. Every client quirk
//! carries its citation in the code; the matrixed clients are Finamp,
//! Jellify, and Manet only. Swiftfin, Infuse, the web client, Kodi, Roku, DLNA,
//! QuickConnect flows, PlayOn/remote-control, video/live-TV, and
//! socket-based sessions are explicitly out of contract.
//!
//! Modules: [`models`] (wire DTOs), [`params`] (case-insensitive query +
//! `SortBy` allowlist), [`builders`] (view → DTO shaping), [`seams`]
//! (boundary traits), `fake` (test doubles). The handlers live in `router`
//! (state, registration, shared helpers), `system`, `browse`, `images`,
//! `audio`, `playstate` and `playlists`; `query` holds the pure paging and
//! sort helpers.
//!
//! Wiring: this module lives in the tree under
//! [`compat`](crate::compat) and mounts through
//! [`CompatSetup`](crate::compat::setup::CompatSetup), which nests the
//! [`router`] under `/jellyfin` outside the `/api/*` session gate (compat
//! carries its own app-password auth) behind the shared CORS + limits
//! layers. Production binds the seams to the app-password store, the
//! catalog-backed
//! [`JellyfinLibrary`](crate::compat::adapters::jellyfin_library::JellyfinLibrary)
//! and [`CatalogIds`](crate::compat::adapters::jellyfin_library::CatalogIds),
//! the [`GatewayStream`](crate::compat::adapters::engines::GatewayStream),
//! and the playback adapter. Not in this module, on purpose: rate
//! limiting, CORS, case-insensitive path handling, access-log redaction
//! (the shared compat edge in [`compat::http`](crate::compat::http)), and
//! the plugin-stream fallback (not wired yet).

mod audio;
mod browse;
pub mod builders;
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
mod images;
pub mod models;
pub mod params;
mod playlists;
mod playstate;
mod query;
mod router;
pub mod seams;
mod system;

pub use router::{JellyfinState, router};
pub use seams::JellyfinSettings;
