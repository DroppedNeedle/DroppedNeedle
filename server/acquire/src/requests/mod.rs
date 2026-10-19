//! Request intake: album, track, batch, and edition-acquire asks plus the
//! approval queues behind them.
//!
//! Clean-slate `/api/v3` surface. [`routes`] mounts each handler at the
//! full path its utoipa annotation names, in the group of the `User`,
//! `Curator` or `Admin` extractor it takes.
//!
//! Seams owned elsewhere: [`dispatch::DownloadDispatch`] (the downloads
//! module implements durable fetch; intake only starts, cancels, and
//! polls through it) and [`mix::PersonalMixBuilder`], which the refresh
//! route and the daily sweep both drive. The wanted
//! watcher loop shares the wanted rows through [`sqlite::WantedStore`].

pub mod auth;
pub mod bridges;
pub mod discovery;
pub mod dispatch;
pub mod error;
pub mod http;
pub mod intake;
pub mod ledger;
pub mod mix;
pub mod mix_sources;
pub mod models;
pub mod quota;
pub mod service;
pub mod sqlite;
pub mod state;
pub mod views;

#[cfg(any(test, feature = "test-support"))]
use axum::{Router, routing};
use droppedneedle_kernel::mount::{Routes, routes};

pub use state::RequestsState;

/// Every requests route plus the two approval-read GETs, in one router
/// with no layers, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn requests_routes(state: RequestsState) -> Router {
    routes()
        .with_state(state.clone())
        .into_router()
        .merge(approval_read_routes(state))
}

/// App-facing routes: everything except the two approval-read GETs, which
/// the collections routes serve (same paths, same admin posture; mounting
/// both would collide). Reads and intake take any signed-in user, approval
/// verbs an admin, edition acquire a curator.
pub fn routes() -> Routes<RequestsState> {
    use intake::*;
    use views::*;

    Routes::new()
        .user(routes!(request_album_handler))
        .user(routes!(request_track_handler))
        .user(routes!(request_batch_handler))
        .user(routes!(cancel_batch_handler))
        .user(routes!(active_handler))
        .user(routes!(active_count_handler))
        .user(routes!(cancel_one_handler))
        .user(routes!(retry_one_handler))
        .user(routes!(history_handler))
        .user(routes!(clear_history_handler))
        .admin(routes!(sync_handler))
        .user(routes!(wanted_handler))
        .user(routes!(wanted_stop_handler))
        .user(routes!(wanted_resume_handler))
        .user(routes!(wanted_seen_handler))
        .admin(routes!(approvals_handler))
        .admin(routes!(approvals_count_handler))
        .admin(routes!(approve_handler))
        .admin(routes!(reject_handler))
        .admin(routes!(approve_auto_download_handler))
        .admin(routes!(reject_auto_download_handler))
        .admin(routes!(revoke_auto_download_handler))
        .admin(routes!(approve_auto_download_batch_handler))
        .admin(routes!(reject_auto_download_batch_handler))
        .admin(routes!(mix_approvals_handler))
        .admin(routes!(approve_mix_handler))
        .admin(routes!(reject_mix_handler))
        .admin(routes!(revoke_mix_handler))
        .user(routes!(refresh_mix_handler))
        .curator(routes!(acquire_edition_handler))
}

/// The two approval-read GETs over this module's approval store, kept out
/// of the API document. The app does not mount these (see [`routes`]).
#[cfg(any(test, feature = "test-support"))]
pub fn approval_read_routes(state: RequestsState) -> Router {
    Router::new()
        .route(
            "/api/v3/requests/auto-download-approvals",
            routing::get(views::auto_download_approvals_handler),
        )
        .route(
            "/api/v3/requests/auto-download-approval-batches",
            routing::get(views::auto_download_batches_handler),
        )
        .with_state(state)
}
