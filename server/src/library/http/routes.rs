//! Library routes: relative paths for nesting under `/api/v3`.

use axum::{
    Router,
    routing::{get, post},
};

use super::handlers;
use crate::library::wiring::LibrarySetup;

/// Library router. The wiring mounts this inside the session gate
/// with the principal-translation layer.
pub fn library_router(state: LibrarySetup) -> Router {
    Router::new()
        .route(
            "/library/roots",
            get(handlers::list_roots).post(handlers::add_root),
        )
        .route("/library/scan", post(handlers::trigger_scan))
        .route("/library/scan/runs", get(handlers::list_runs))
        .route("/library/scan/runs/{id}", get(handlers::get_run))
        .route("/library/identify", post(handlers::enqueue_identify))
        .route("/library/reviews", get(handlers::list_reviews))
        .route(
            "/library/reviews/{id}/approve",
            post(handlers::approve_review),
        )
        .route(
            "/library/reviews/{id}/reject",
            post(handlers::reject_review),
        )
        .route("/library/manage/preview", post(handlers::manage_preview))
        .route("/library/manage/apply", post(handlers::manage_apply))
        .route("/library/manage/undo", post(handlers::manage_undo))
        .route(
            "/library/manage/baseline/restore",
            post(handlers::baseline_restore),
        )
        .merge(super::scan::routes())
        .merge(super::operations::operations_router())
        .with_state(state.clone())
        .merge(super::contrib::router(state))
}
