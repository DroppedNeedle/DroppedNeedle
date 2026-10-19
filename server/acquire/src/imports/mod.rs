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
//! `spotify_store` holds the SQLite stores behind it (links, authorize
//! states, imported playlists),
//! `jobs` holds the `spotify:import` durable-job key plus the minimal local
//! [`jobs::SpotifyImportExecutor`] seam onto the downloads state machine
//! (the in-memory implementation spawns a task), `health`
//! holds the slskd/Usenet smoke plus the per-source release gates,
//! and `handlers` owns the native routes; the loopback mock servers the
//! tests run against live in the testkit crate. The app mounts
//! [`handlers::routes`]: the OAuth callback in the public group, the rest
//! behind the session gate.

pub mod error;
pub mod handlers;
pub mod health;
pub mod jobs;
pub mod lidarr;
pub mod models;
pub mod mount;
pub mod spotify;
pub mod spotify_store;
