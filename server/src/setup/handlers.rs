//! The setup guide's route.

use std::sync::Arc;

use axum::{Json, extract::State};
use droppedneedle_auth::Admin;

use super::SetupFinder;
use super::models::SetupFound;

/// What the setup guide found: the services answering on this network and
/// the folders worth suggesting. A look that failed is part of the answer.
#[utoipa::path(
    get,
    tag = "setup",
    path = "/api/v3/setup/found",
    operation_id = "get_setup_found",
    responses((status = 200, description = "What the look found", body = SetupFound))
)]
pub async fn get_setup_found(
    State(setup): State<Arc<SetupFinder>>,
    _caller: Admin,
) -> Json<SetupFound> {
    Json(setup.found().await)
}
