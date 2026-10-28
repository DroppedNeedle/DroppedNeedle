//! Imports and acquisition health: Lidarr read-only import, Spotify
//! OAuth/playlist import, and the acquisition health smoke.
//!
//! The Lidarr surface is read-only import: monitored artists become
//! follows keyed on the MusicBrainz MBID, and this module issues no Lidarr
//! management call of any kind.
//!
//! Layout: `lidarr` holds the two-endpoint client plus the import service,
//! `spotify_oauth` holds the OAuth half (states, links, app settings),
//! `spotify_client` the Web API client over the wire shapes in
//! `spotify_models`, `spotify` the playlist listing and
//! the populate worker, `spotify_albums` the MusicBrainz release group of
//! each imported album, `spotify_store` holds the SQLite stores behind it (links, authorize
//! states, imported playlists),
//! `jobs` runs each playlist import as a job on the jobs table, `health`
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
pub mod spotify_albums;
pub mod spotify_client;
pub mod spotify_models;
pub mod spotify_oauth;
pub mod spotify_store;
pub(crate) mod sql;
