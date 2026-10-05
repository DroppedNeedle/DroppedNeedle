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
//! `SortBy` allowlist), [`builders`] (view → DTO shaping), [`routes`] (axum
//! handlers + registration), [`seams`] (boundary traits + memory fakes).
//!
//! Wiring: this module lives in the tree under
//! [`compat`](crate::compat) and mounts through
//! [`CompatSetup`](crate::compat::setup::CompatSetup), which nests the
//! [`router`] under `/jellyfin` outside the `/api/*` session gate (compat
//! carries its own app-password auth) behind the shared CORS + limits
//! layers. Production binds the seams to the app-password store, the
//! in-memory [`seams::MemoryLibrary`], the
//! [`GatewayStream`](crate::compat::adapters::engines::GatewayStream), and
//! the playback adapter. Not in this module, on purpose: rate
//! limiting, CORS, case-insensitive path handling, access-log redaction
//! (the shared compat edge in [`compat::http`](crate::compat::http)), and
//! the plugin-stream fallback (not wired yet).

pub mod builders;
pub mod models;
pub mod params;
pub mod routes;
pub mod seams;

pub use routes::{JellyfinState, router};
pub use seams::JellyfinSettings;
