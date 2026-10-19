//! Library reads: catalog, local-library browse, genres, lyrics.
//!
//! Clean-slate `/api/v3` read handlers over the
//! 0001 baseline `local_*` tables plus `library_user_favorites` for the
//! caller's favorite flags. These routes read existing tables only and
//! sit behind the caller's session; membership and track resolution are
//! POSTs only because their id lists ride in a body.
//!
//! Layout mirrors the users routes: `handlers` are thin, `services` hold the
//! domain logic and return [`services::LibraryFailure`], `stores` defines
//! the ports, `sqlite` implements them over the reader pool, and `memory`
//! carries the test fakes. Status mapping lives only in `error`.
//!
//! Wiring: the app mounts [`routes`], which also registers the paths in
//! the API document.

pub mod error;
pub mod file_lyrics;
pub mod handlers;
pub mod lookups;
pub mod memory;
pub mod models;
#[cfg(test)]
mod perf_tests;
pub mod player;
pub mod services;
pub mod sqlite;
pub mod stores;
pub mod tags;

use std::sync::Arc;

use droppedneedle_kernel::mount::{Routes, routes};
use stores::{FavoriteReads, LibraryCatalog, LibraryLookups, LyricsPort, TrackTagReader};

/// Every dependency the library reads need, injected by constructor.
#[derive(Clone)]
pub struct LibraryDeps {
    /// Catalog reads over the `local_*` tables.
    pub catalog: Arc<dyn LibraryCatalog>,
    /// Favorite flags and counts for the caller.
    pub favorites: Arc<dyn FavoriteReads>,
    /// Stored-lyrics reads; the provider fetch sits behind this port too.
    pub lyrics: Arc<dyn LyricsPort>,
    /// Membership, album status, track resolution and stats extras.
    pub lookups: Arc<dyn LibraryLookups>,
    /// The download policy album status reads its upgrade cutoff from, per
    /// call; `None` shows no upgrades.
    pub upgrade_policy: Option<
        droppedneedle_kernel::runtime_config::Live<
            droppedneedle_kernel::runtime_config::sections::DownloadPolicy,
        >,
    >,
    /// Tag reads from the files on disk (admin tag view).
    pub tags: Arc<dyn TrackTagReader>,
}

/// The library read routes, at their full `/api/v3` paths. Every handler
/// takes `User`, except the tag view, which takes `Admin`. The two POSTs
/// (membership, resolve-tracks) are reads that take their ids in a body.
pub fn routes() -> Routes<LibraryDeps> {
    use handlers::*;

    Routes::new()
        .user(routes!(list_albums))
        .user(routes!(get_album))
        .user(routes!(album_status))
        .user(routes!(membership))
        .user(routes!(album_mbids))
        .admin(routes!(track_tags))
        .user(routes!(resolve_tracks))
        .user(routes!(list_album_tracks))
        .user(routes!(list_album_copies))
        .user(routes!(list_artists))
        .user(routes!(get_artist))
        .user(routes!(list_artist_albums))
        .user(routes!(list_artist_appearances))
        .user(routes!(list_tracks))
        .user(routes!(get_track))
        .user(routes!(get_lyrics))
        .user(routes!(get_stats))
        .user(routes!(list_recently_added))
        .user(routes!(list_genres))
        .user(routes!(list_genre_tracks))
        .user(routes!(browse_albums))
        .user(routes!(match_album))
        .user(routes!(search_library))
        .user(routes!(recent_albums))
        .user(routes!(list_decades))
        .user(routes!(list_suggestions))
}
