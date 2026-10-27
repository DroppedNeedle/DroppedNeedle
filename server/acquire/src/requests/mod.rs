//! Request intake: album, track, batch, and edition-acquire asks plus the
//! approval queues behind them.
//!
//! Clean-slate `/api/v3` surface. [`routes`] mounts each handler at the
//! full path its utoipa annotation names, in the group of the `User`,
//! `Curator` or `Admin` extractor it takes.
//!
//! Reached elsewhere: [`crate::dispatch::DownloadDispatch`] (intake only
//! starts, cancels, and reads downloads through it) and
//! [`mix::PersonalMixBuilder`], which the refresh
//! route and the daily sweep both drive. The wanted
//! watcher loop shares the wanted rows through [`sqlite::WantedStore`].

pub mod auth;
pub mod discovery;
pub mod error;
pub mod http;
pub mod intake;
pub mod ledger;
pub mod mix;
pub mod mix_sources;
pub mod models;
pub mod quota;
pub mod service;
pub mod settle;
pub(crate) mod sql;
pub mod sqlite;
pub mod state;
pub mod views;

#[cfg(any(test, feature = "test-support"))]
use axum::Router;
use droppedneedle_kernel::mount::{Routes, routes};

pub use state::RequestsState;

/// Every requests route plus the follows' auto-download approval routes,
/// in one router with no layers, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn requests_routes(state: RequestsState) -> Router {
    let follows = crate::follows::routes()
        .with_state(state.follows.clone())
        .into_router();
    routes().with_state(state).into_router().merge(follows)
}

/// The requests routes. Reads and intake take any signed-in user, approval
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
        .user(routes!(from_album_handler))
        .user(routes!(history_handler))
        .user(routes!(clear_history_handler))
        .user(routes!(wanted_handler))
        .user(routes!(wanted_stop_handler))
        .user(routes!(wanted_resume_handler))
        .user(routes!(wanted_seen_handler))
        .admin(routes!(approvals_handler))
        .admin(routes!(approvals_count_handler))
        .admin(routes!(approve_handler))
        .admin(routes!(reject_handler))
        .admin(routes!(mix_approvals_handler))
        .admin(routes!(approve_mix_handler))
        .admin(routes!(reject_mix_handler))
        .admin(routes!(revoke_mix_handler))
        .user(routes!(refresh_mix_handler))
        .curator(routes!(acquire_edition_handler))
        .curator(routes!(edition_fetch_handler))
}
