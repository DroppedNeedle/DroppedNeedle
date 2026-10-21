//! Collections: playlists, favorites, ratings, follow reads.
//!
//! Layout: `handlers` parse and render, `service` holds the rules and
//! returns the domain [`error::CollectionsError`], `store` reads and
//! writes SQLite, `http` is the one place an error meets a status code.
//! [`routes`] mounts them behind the session gate, whose principal the
//! handlers read.

pub mod db;
pub mod error;
pub mod handlers;
pub mod http;
pub mod models;
pub mod remote_import;
pub mod service;
pub mod state;
pub mod store;

use droppedneedle_kernel::mount::{Routes, routes};

pub use state::CollectionsState;

/// The collections routes at their full `/api/v3` paths, each for any
/// signed-in user. Following and the auto-download approvals are the
/// follows' routes in acquisition.
pub fn routes() -> Routes<CollectionsState> {
    use handlers::*;

    Routes::new()
        .user(routes!(list_playlists_handler, create_playlist_handler))
        .user(routes!(check_tracks_handler))
        .user(routes!(
            get_playlist_handler,
            update_playlist_handler,
            delete_playlist_handler
        ))
        .user(routes!(set_visibility_handler))
        .user(routes!(add_tracks_handler))
        .user(routes!(remove_tracks_handler))
        .user(routes!(reorder_track_handler))
        .user(routes!(update_track_handler, remove_track_handler))
        .user(routes!(resolve_sources_handler))
        .user(routes!(
            upload_cover_handler,
            get_cover_handler,
            remove_cover_handler
        ))
        .user(routes!(list_favorites_handler))
        .user(routes!(set_favorite_handler))
        .user(routes!(rating_lookup_handler))
        .user(routes!(set_rating_handler))
        .user(routes!(get_follow_status_handler))
        .user(routes!(list_followed_artists_handler))
        .user(routes!(list_new_releases_handler))
        .user(routes!(list_recent_releases_handler))
        .user(routes!(unseen_count_handler))
        .user(routes!(mark_seen_handler))
}
