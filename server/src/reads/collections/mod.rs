//! Collections: playlists, favorites, follows, approval reads, pins.
//!
//! Layout: `handlers` parse and render, `service` holds the rules and
//! returns the domain [`error::CollectionsError`], `store` reads and
//! writes SQLite, `http` is the one place an error meets a status code.
//! The router mounts paths relative to `/api/v3`; the app nests it there
//! behind the session middleware and builds [`auth::Principal`] from the
//! session.

pub mod auth;
pub mod db;
pub mod error;
pub mod handlers;
pub mod http;
pub mod models;
pub mod service;
pub mod state;
pub mod store;

use axum::{Router, routing};

pub use state::CollectionsState;

/// The collections routes behind the header test gate, for tests that
/// mount them without the session middleware. Route posture per handler
/// docs: playlists, favorites and follows need any authenticated user,
/// approvals need admin, pin writes need curator.
#[cfg(any(test, feature = "test-support"))]
pub fn collections_router(state: CollectionsState) -> Router {
    collections_routes(state).layer(axum::middleware::from_fn(auth::gate))
}

/// Build the collections routes without an auth layer. The app mounts this
/// inside the session gate with a principal-translation layer.
/// Registration order is specific routes first, then parameterized ones.
pub fn collections_routes(state: CollectionsState) -> Router {
    Router::new()
        .route(
            "/playlists",
            routing::get(handlers::list_playlists_handler).post(handlers::create_playlist_handler),
        )
        .route(
            "/playlists/check-tracks",
            routing::post(handlers::check_tracks_handler),
        )
        .route(
            "/playlists/{playlist_id}",
            routing::get(handlers::get_playlist_handler)
                .put(handlers::update_playlist_handler)
                .delete(handlers::delete_playlist_handler),
        )
        .route(
            "/playlists/{playlist_id}/visibility",
            routing::patch(handlers::set_visibility_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks",
            routing::post(handlers::add_tracks_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks/remove",
            routing::post(handlers::remove_tracks_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks/reorder",
            routing::patch(handlers::reorder_track_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks/{track_id}",
            routing::patch(handlers::update_track_handler).delete(handlers::remove_track_handler),
        )
        .route(
            "/playlists/{playlist_id}/resolve-sources",
            routing::post(handlers::resolve_sources_handler),
        )
        .route(
            "/playlists/{playlist_id}/cover",
            routing::post(handlers::upload_cover_handler)
                .get(handlers::get_cover_handler)
                .delete(handlers::remove_cover_handler),
        )
        .route("/favorites", routing::get(handlers::list_favorites_handler))
        .route(
            "/favorites/{kind}/{item_id}",
            routing::put(handlers::set_favorite_handler),
        )
        .route(
            "/artists/{artist_mbid}/follow-status",
            routing::get(handlers::get_follow_status_handler),
        )
        .route(
            "/artists/{artist_mbid}/follow",
            routing::put(handlers::set_follow_handler),
        )
        .route(
            "/artists/{artist_mbid}/auto-download",
            routing::put(handlers::set_auto_download_handler),
        )
        .route(
            "/following/artists",
            routing::get(handlers::list_followed_artists_handler),
        )
        .route(
            "/following/new-releases",
            routing::get(handlers::list_new_releases_handler),
        )
        .route(
            "/following/new-releases/recent",
            routing::get(handlers::list_recent_releases_handler),
        )
        .route(
            "/following/new-releases/unseen-count",
            routing::get(handlers::unseen_count_handler),
        )
        .route(
            "/following/new-releases/seen",
            routing::post(handlers::mark_seen_handler),
        )
        .route(
            "/requests/auto-download-approvals",
            routing::get(handlers::list_approvals_handler),
        )
        .route(
            "/requests/auto-download-approval-batches",
            routing::get(handlers::list_approval_batches_handler),
        )
        .route(
            "/library/albums/{album_id}/edition-pin",
            routing::get(handlers::get_pin_handler)
                .put(handlers::set_pin_handler)
                .delete(handlers::clear_pin_handler),
        )
        .with_state(state)
}
