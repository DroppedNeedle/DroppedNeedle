//! Jellyfin compat shim: PascalCase shapes, real statuses. Of the library,
//! only album and artist pictures and the library tile open without
//! sign-in; album and artist ids are made with the data key.
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
//! (the views and records the adapters answer). The handlers live in `router`
//! (state, registration, shared helpers), `system`, `browse`, `images`,
//! `audio`, `lyrics`, `playstate` and `playlists`; `query` holds the item-type and
//! short-list paging helpers.
//!
//! Wiring: this module lives in the tree under
//! [`compat`](crate) and mounts through
//! [`CompatSetup`](crate::setup::CompatSetup), which serves [`routes`]
//! outside the `/api/*` session gate (compat carries its own app-password
//! auth) behind the shared CORS + limits layers. The routes call the
//! app-password store, the catalog-backed
//! [`JellyfinLibrary`](crate::adapters::jellyfin_library::JellyfinLibrary)
//! and [`CatalogIds`](crate::adapters::jellyfin_library::CatalogIds),
//! the [`GatewayAudio`](crate::adapters::audio::GatewayAudio) (which
//! falls back to `streaming_source` plugins on a local miss), and the
//! playback adapter. Not in this module, on purpose: rate limiting, CORS,
//! case-insensitive path handling and access-log redaction (the shared
//! compat edge in [`compat::http`](crate::http)).

mod audio;
mod browse;
pub mod builders;
mod images;
mod lyrics;
pub mod models;
pub mod params;
mod playlists;
mod playstate;
mod query;
mod router;
pub mod seams;
mod system;

pub(crate) use router::QueryTokens;
pub use router::{Access, JellyfinState, ROUTES, Route, Verb, is_public, routes};
pub use seams::JellyfinSettings;
