//! Jellyfin compat shim: PascalCase shapes, real statuses, anonymous audio.
//!
//! Ported from v2 `backend/api/compat/jellyfin/` (`router.py`, `models.py`,
//! `builders.py`, `auth.py`, `errors.py`, `serialization.py`) and the stage-0
//! matrix (`stage0-compat.md` §§1 preamble + 2). Every client quirk carries
//! its citation in the code; the matrixed clients are Finamp, Jellify, and
//! Manet ONLY — Swiftfin, Infuse, the web client, Kodi, Roku, DLNA,
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
//! layers. Production binds the seams to the stage-3 password store, the
//! honest-memory [`seams::MemoryLibrary`], the stage-6
//! [`GatewayStream`](crate::compat::adapters::engines::GatewayStream), and
//! the stage-6 playback adapter. Deliberately NOT in this module: rate
//! limiting, CORS, case-insensitive path handling, access-log redaction
//! (the shared compat edge in [`compat::http`](crate::compat::http)), and
//! the plugin-stream fallback (remotes work).

pub mod builders;
pub mod models;
pub mod params;
pub mod routes;
pub mod seams;

pub use routes::{JellyfinState, router};
pub use seams::JellyfinSettings;
