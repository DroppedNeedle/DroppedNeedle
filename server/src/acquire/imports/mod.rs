//! Stage-7 imports + acquisition-health slice: Lidarr read-only import,
//! Spotify OAuth/playlist import, and the acquisition health smoke.
//!
//! The v2 backend (`backend/`) owns the exact semantics ported here; every
//! quirk below cites its v2 call site. The Lidarr surface is read-only
//! import only: monitored artists become follows keyed on the MusicBrainz
//! MBID, and the management-integration tombstone still holds, so this
//! slice issues no Lidarr management call of any kind.
//!
//! Layout: `lidarr` holds the two-endpoint client plus the import service,
//! `spotify` holds OAuth, playlist listing, and the populate worker,
//! `jobs` holds the `spotify:import` durable-job key plus the minimal local
//! [`jobs::SpotifyImportExecutor`] seam onto the downloads state machine
//! (owned by the downloads slice; the memory impl spawns a task), `health`
//! holds the Free/slskd/Usenet smoke plus the per-source release gates,
//! `handlers` owns the native routes, and `mocks` carries the loopback mock
//! servers the briefs run against. Wiring: the integrator mounts
//! [`handlers::imports_router`] inside the session gate.

pub mod error;
pub mod handlers;
pub mod health;
pub mod jobs;
pub mod lidarr;
pub mod mocks;
pub mod models;
pub mod spotify;
