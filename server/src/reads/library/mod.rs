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
//! Wiring: this module is mounted as `reads::library` under `/api/v3`
//! with [`library_router`], and its paths are registered in the utoipa
//! document.

pub mod error;
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

use crate::auth::users::UsersDeps;
use crate::ids::IdGenerator;
use axum::{
    Router,
    routing::{get, post},
};

use stores::{
    FavoriteReads, LibraryCatalog, LibraryLookups, LyricsPort, TrackTagReader, UpgradePolicySource,
};

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
    /// Upgrade cutoff for album status, read per call.
    pub upgrade_policy: UpgradePolicySource,
    /// Tag reads from the files on disk (admin tag view).
    pub tags: Arc<dyn TrackTagReader>,
    /// Auth bundle, used only to resolve the caller for favorite flags.
    pub auth: UsersDeps,
    /// Fresh ids for 5xx error ids.
    pub ids: Arc<dyn IdGenerator>,
}

/// Authenticated read routes. The session middleware authenticates before
/// these run; every handler takes the library user extractor, so every
/// route 401s anonymously and admits any signed-in role. The two POSTs
/// (membership, resolve-tracks) are reads that take their ids in a body.
pub fn library_router(deps: LibraryDeps) -> Router {
    Router::new()
        .route("/library/albums", get(handlers::list_albums))
        .route("/library/albums/{id}", get(handlers::get_album))
        .route("/library/albums/{id}/status", get(handlers::album_status))
        .route("/library/membership", post(handlers::membership))
        .route("/library/mbids", get(handlers::album_mbids))
        .route("/library/tracks/{id}/tags", get(handlers::track_tags))
        .route("/library/resolve-tracks", post(handlers::resolve_tracks))
        .route(
            "/library/albums/{id}/tracks",
            get(handlers::list_album_tracks),
        )
        .route(
            "/library/albums/{id}/copies",
            get(handlers::list_album_copies),
        )
        .route("/library/artists", get(handlers::list_artists))
        .route("/library/artists/{id}", get(handlers::get_artist))
        .route(
            "/library/artists/{id}/albums",
            get(handlers::list_artist_albums),
        )
        .route(
            "/library/artists/{id}/appearances",
            get(handlers::list_artist_appearances),
        )
        .route("/library/tracks", get(handlers::list_tracks))
        .route("/library/tracks/{id}", get(handlers::get_track))
        .route("/library/tracks/{id}/lyrics", get(handlers::get_lyrics))
        .route("/library/stats", get(handlers::get_stats))
        .route(
            "/library/recently-added",
            get(handlers::list_recently_added),
        )
        .route("/library/genres", get(handlers::list_genres))
        .route(
            "/library/genres/{name}/tracks",
            get(handlers::list_genre_tracks),
        )
        .route("/local-library/albums", get(handlers::browse_albums))
        .route(
            "/local-library/albums/match/{mbid}",
            get(handlers::match_album),
        )
        .route("/local-library/search", get(handlers::search_library))
        .route("/local-library/recent", get(handlers::recent_albums))
        .route("/local-library/decades", get(handlers::list_decades))
        .route(
            "/local-library/suggestions",
            get(handlers::list_suggestions),
        )
        .with_state(deps)
}
