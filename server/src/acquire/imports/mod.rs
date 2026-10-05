//! Imports and acquisition health: Lidarr read-only import, Spotify
//! OAuth/playlist import, and the acquisition health smoke.
//!
//! The semantics are ported from v2; every quirk below cites its v2 call
//! site. The Lidarr surface is read-only import: monitored artists become
//! follows keyed on the MusicBrainz MBID, and this module issues no Lidarr
//! management call of any kind.
//!
//! Layout: `lidarr` holds the two-endpoint client plus the import service,
//! `spotify` holds OAuth, playlist listing, and the populate worker,
//! `jobs` holds the `spotify:import` durable-job key plus the minimal local
//! [`jobs::SpotifyImportExecutor`] seam onto the downloads state machine
//! (the in-memory implementation spawns a task), `health`
//! holds the Free/slskd/Usenet smoke plus the per-source release gates,
//! `handlers` owns the native routes, and `mocks` carries the loopback mock
//! servers the tests run against. The app mounts
//! [`handlers::imports_gated_router`] inside the session gate and the
//! OAuth callback outside it.

pub mod error;
pub mod handlers;
pub mod health;
pub mod jobs;
pub mod lidarr;
#[cfg(any(test, feature = "test-support"))]
pub mod mocks;
pub mod models;
pub mod spotify;
