//! Collections: playlists, favorites, follows, pins.
//!
//! Clean-slate `/api/v3` surface. The router below mounts paths relative to
//! `/api/v3`; the app nests it there behind the session middleware and
//! builds [`auth::Principal`] from the session. Handler utoipa annotations
//! carry the full `/api/v3` paths for the contract document.

pub mod approvals;
pub mod auth;
pub mod error;
pub mod favorites;
pub mod follows;
pub mod models;
pub mod pins;
pub mod playlists;
pub mod state;

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
            routing::get(playlists::list_playlists_handler)
                .post(playlists::create_playlist_handler),
        )
        .route(
            "/playlists/check-tracks",
            routing::post(playlists::check_tracks_handler),
        )
        .route(
            "/playlists/{playlist_id}",
            routing::get(playlists::get_playlist_handler)
                .put(playlists::update_playlist_handler)
                .delete(playlists::delete_playlist_handler),
        )
        .route(
            "/playlists/{playlist_id}/visibility",
            routing::patch(playlists::set_visibility_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks",
            routing::post(playlists::add_tracks_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks/remove",
            routing::post(playlists::remove_tracks_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks/reorder",
            routing::patch(playlists::reorder_track_handler),
        )
        .route(
            "/playlists/{playlist_id}/tracks/{track_id}",
            routing::patch(playlists::update_track_handler).delete(playlists::remove_track_handler),
        )
        .route(
            "/playlists/{playlist_id}/resolve-sources",
            routing::post(playlists::resolve_sources_handler),
        )
        .route(
            "/playlists/{playlist_id}/cover",
            routing::post(playlists::upload_cover_handler)
                .get(playlists::get_cover_handler)
                .delete(playlists::remove_cover_handler),
        )
        .route(
            "/favorites",
            routing::get(favorites::list_favorites_handler),
        )
        .route(
            "/favorites/{kind}/{item_id}",
            routing::put(favorites::set_favorite_handler),
        )
        .route(
            "/artists/{artist_mbid}/follow-status",
            routing::get(follows::get_follow_status_handler),
        )
        .route(
            "/artists/{artist_mbid}/follow",
            routing::put(follows::set_follow_handler),
        )
        .route(
            "/artists/{artist_mbid}/auto-download",
            routing::put(follows::set_auto_download_handler),
        )
        .route(
            "/following/artists",
            routing::get(follows::list_followed_artists_handler),
        )
        .route(
            "/following/new-releases",
            routing::get(follows::list_new_releases_handler),
        )
        .route(
            "/following/new-releases/recent",
            routing::get(follows::list_recent_releases_handler),
        )
        .route(
            "/following/new-releases/unseen-count",
            routing::get(follows::unseen_count_handler),
        )
        .route(
            "/following/new-releases/seen",
            routing::post(follows::mark_seen_handler),
        )
        .route(
            "/requests/auto-download-approvals",
            routing::get(approvals::list_approvals_handler),
        )
        .route(
            "/requests/auto-download-approval-batches",
            routing::get(approvals::list_approval_batches_handler),
        )
        .route(
            "/library/albums/{album_id}/edition-pin",
            routing::get(pins::get_pin_handler)
                .put(pins::set_pin_handler)
                .delete(pins::clear_pin_handler),
        )
        .with_state(state)
}
