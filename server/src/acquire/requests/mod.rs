//! Stage-7 request intake: album, track, batch, and edition-acquire asks
//! plus the approval queues behind them.
//!
//! Clean-slate `/api/v3` surface. The router below mounts paths relative to
//! `/api/v3`; wiring nests it there behind the session middleware and swaps
//! the slice-local [`auth::gate`] plus [`auth::Principal`] for the real
//! session gate and role extractors (mirroring the reads collections slice:
//! add a principal-translation layer that resolves the role fresh from the
//! user store). Handler utoipa annotations already carry the full `/api/v3`
//! paths for the contract document.
//!
//! Seams owned elsewhere: [`dispatch::DownloadDispatch`] (the downloads
//! slice implements durable fetch; this slice only starts, cancels, and
//! polls through it), the mix build behind the refresh key, and the wanted
//! watcher loop behind the wanted rows.

pub mod auth;
pub mod bridges;
pub mod dispatch;
pub mod error;
pub mod intake;
pub mod ledger;
pub mod models;
pub mod quota;
pub mod service;
pub mod state;
pub mod views;

use axum::{Router, routing};

pub use state::RequestsState;

/// The requests routes behind the header test gate, for tests that mount
/// them without the session middleware. Route posture per handler docs:
/// reads and intake need any authenticated user, approval verbs need
/// admin, edition acquire needs curator.
#[cfg(any(test, feature = "test-support"))]
pub fn requests_router(state: RequestsState) -> Router {
    requests_routes(state).layer(axum::middleware::from_fn(auth::gate))
}

/// Every requests route without an auth layer, including the two
/// approval-read GETs the app serves from the collections routes instead.
#[cfg(any(test, feature = "test-support"))]
pub fn requests_routes(state: RequestsState) -> Router {
    requests_core_routes(state.clone()).merge(approval_read_routes(state))
}

/// App-facing routes: everything except the two approval-read GETs. The
/// reads collections routes serve those paths (same paths, same admin
/// posture); mounting both would collide.
pub fn requests_core_routes(state: RequestsState) -> Router {
    Router::new()
        .route(
            "/requests/albums",
            routing::post(intake::request_album_handler),
        )
        .route(
            "/requests/tracks",
            routing::post(intake::request_track_handler),
        )
        .route(
            "/requests/batches",
            routing::post(intake::request_batch_handler),
        )
        .route(
            "/requests/batches/cancel",
            routing::post(intake::cancel_batch_handler),
        )
        .route("/requests/active", routing::get(views::active_handler))
        .route(
            "/requests/active/count",
            routing::get(views::active_count_handler),
        )
        .route(
            "/requests/active/{musicbrainz_id}",
            routing::delete(intake::cancel_one_handler),
        )
        .route(
            "/requests/retry/{musicbrainz_id}",
            routing::post(intake::retry_one_handler),
        )
        .route("/requests/history", routing::get(views::history_handler))
        .route(
            "/requests/history/{musicbrainz_id}",
            routing::delete(intake::clear_history_handler),
        )
        .route("/requests/sync", routing::post(views::sync_handler))
        .route("/requests/wanted", routing::get(views::wanted_handler))
        .route(
            "/requests/wanted/{musicbrainz_id}/stop",
            routing::post(views::wanted_stop_handler),
        )
        .route(
            "/requests/wanted/{musicbrainz_id}/resume",
            routing::post(views::wanted_resume_handler),
        )
        .route(
            "/requests/wanted/{musicbrainz_id}/seen",
            routing::post(views::wanted_seen_handler),
        )
        .route(
            "/requests/approvals",
            routing::get(views::approvals_handler),
        )
        .route(
            "/requests/approvals/count",
            routing::get(views::approvals_count_handler),
        )
        .route(
            "/requests/approvals/{musicbrainz_id}/approve",
            routing::post(views::approve_handler),
        )
        .route(
            "/requests/approvals/{musicbrainz_id}/reject",
            routing::post(views::reject_handler),
        )
        .route(
            "/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve",
            routing::post(views::approve_auto_download_handler),
        )
        .route(
            "/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject",
            routing::post(views::reject_auto_download_handler),
        )
        .route(
            "/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke",
            routing::post(views::revoke_auto_download_handler),
        )
        .route(
            "/requests/auto-download-approval-batches/{batch_id}/approve",
            routing::post(views::approve_auto_download_batch_handler),
        )
        .route(
            "/requests/auto-download-approval-batches/{batch_id}/reject",
            routing::post(views::reject_auto_download_batch_handler),
        )
        .route(
            "/requests/personal-mix-approvals",
            routing::get(views::mix_approvals_handler),
        )
        .route(
            "/requests/personal-mix-approvals/{user_id}/approve",
            routing::post(views::approve_mix_handler),
        )
        .route(
            "/requests/personal-mix-approvals/{user_id}/reject",
            routing::post(views::reject_mix_handler),
        )
        .route(
            "/requests/personal-mix-approvals/{user_id}/revoke",
            routing::post(views::revoke_mix_handler),
        )
        .route(
            "/requests/personal-mix/refresh",
            routing::post(views::refresh_mix_handler),
        )
        .route(
            "/albums/{album_id}/edition/acquire",
            routing::post(intake::acquire_edition_handler),
        )
        .with_state(state)
}

/// The two approval-read GETs over this module's approval store. The app
/// does not mount these (see [`requests_core_routes`]).
#[cfg(any(test, feature = "test-support"))]
pub fn approval_read_routes(state: RequestsState) -> Router {
    Router::new()
        .route(
            "/requests/auto-download-approvals",
            routing::get(views::auto_download_approvals_handler),
        )
        .route(
            "/requests/auto-download-approval-batches",
            routing::get(views::auto_download_batches_handler),
        )
        .with_state(state)
}
