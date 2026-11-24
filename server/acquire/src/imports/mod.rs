//! Imports: Lidarr read-only import, Spotify OAuth/playlist import, and the
//! download client status reads.
//!
//! The Lidarr surface is read-only import: monitored artists become
//! follows keyed on the MusicBrainz MBID, and this module issues no Lidarr
//! management call of any kind.
//!
//! Layout: `lidarr` holds the two-endpoint client plus the import service,
//! `spotify_oauth` holds the OAuth half (states, links, app settings),
//! `spotify_client` the user's tokens over the providers' Spotify client,
//! `spotify` the playlist listing and
//! the populate worker, `spotify_albums` the MusicBrainz release group of
//! each imported album, `spotify_store` holds the SQLite stores behind it (links, authorize
//! states, imported playlists),
//! `jobs` runs each playlist import as a job on the jobs table, `health`
//! holds the slskd and SABnzbd status reads,
//! and `handlers` owns the native routes; the loopback mock servers the
//! tests run against live in the testkit crate. The app mounts
//! [`handlers::routes`]: the OAuth callback in the public group, the rest
//! behind the session gate.

/// The Spotify ref a Liked Songs import runs under, and the source of its
/// playlist (`spotify:liked`). Spotify's playlist ids are 22 base-62
/// characters, so it never names a playlist; the playlist import refuses
/// it.
pub const LIKED_SONGS_REF: &str = "liked";

/// The name of the Liked Songs playlist.
pub const LIKED_SONGS_NAME: &str = "Liked Songs";

pub mod error;
pub mod handlers;
pub mod health;
pub mod jobs;
pub mod lidarr;
pub mod models;
pub mod mount;
pub mod playlist_identify;
pub mod spotify;
pub mod spotify_albums;
pub mod spotify_client;
pub mod spotify_oauth;
pub mod spotify_store;
pub(crate) mod sql;
