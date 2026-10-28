//! The discovery batch routes. Errors are the requests errors, mapped in
//! [`crate::requests::http`].

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use serde::Deserialize;

use super::DiscoveryBatches;
use super::models::{
    DiscoveryBatchCreate, DiscoveryBatchDetail, DiscoveryBatchListResponse,
    DiscoveryBatchRemoveResult,
};
use crate::requests::RequestsState;

/// Batch removal params.
#[derive(Debug, Deserialize)]
pub struct RemoveBatchQuery {
    /// Also remove/cancel the batch's albums (default true).
    pub remove_albums: Option<bool>,
}

/// Create a discovery batch: every album the library lacks and nobody
/// has requested yet is filed as an album request, under the caller's
/// quota and approval rules.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/batches",
    request_body = DiscoveryBatchCreate,
    responses(
        (status = 202, description = "Batch created", body = DiscoveryBatchDetail),
        (status = 400, description = "Empty or oversized batch, or every album is already owned or requested"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Storage full (STORAGE_FULL)"),
        (status = 429, description = "Request quota spent (QUOTA_EXCEEDED)"),
        (status = 409, description = "Acquisition is not running (NOT_AVAILABLE)"),
    )
)]
pub async fn create_batch(
    State(state): State<RequestsState>,
    User(user): User,
    ValidJson(body): ValidJson<DiscoveryBatchCreate>,
) -> Result<(StatusCode, Json<DiscoveryBatchDetail>), ApiError> {
    DiscoveryBatches::new(&state)
        .create(&user, &body)
        .await
        .map(|detail| (StatusCode::ACCEPTED, Json(detail)))
        .map_err(ApiError::from)
}

/// The caller's batches, newest first.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/batches",
    responses(
        (status = 200, description = "Batch list", body = DiscoveryBatchListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The batch store failed"),
    )
)]
pub async fn list_batches(
    State(state): State<RequestsState>,
    User(user): User,
) -> Result<Json<DiscoveryBatchListResponse>, ApiError> {
    DiscoveryBatches::new(&state)
        .list(user.user_id())
        .await
        .map(|batches| Json(DiscoveryBatchListResponse { batches }))
        .map_err(ApiError::from)
}

/// One batch with each album's current state. Admins may open any batch;
/// anyone else's reads as missing.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/batches/{batch_id}",
    params(("batch_id" = String, Path, description = "Batch id")),
    responses(
        (status = 200, description = "Batch detail", body = DiscoveryBatchDetail),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown batch id"),
    )
)]
pub async fn get_batch(
    State(state): State<RequestsState>,
    User(user): User,
    Path(batch_id): Path<String>,
) -> Result<Json<DiscoveryBatchDetail>, ApiError> {
    DiscoveryBatches::new(&state)
        .detail(&user, &batch_id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Remove one batch. With `remove_albums` (the default) its pending
/// requests are cancelled and the albums it brought in go to the recycle
/// bin; albums that were already owned or requested are never touched.
#[utoipa::path(
    delete,
    tag = "discover",
    path = "/api/v3/discover/batches/{batch_id}",
    params(
        ("batch_id" = String, Path, description = "Batch id"),
        ("remove_albums" = Option<bool>, Query, description = "Also remove the batch albums"),
    ),
    responses(
        (status = 200, description = "Removal outcome", body = DiscoveryBatchRemoveResult),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown batch id"),
    )
)]
pub async fn remove_batch(
    State(state): State<RequestsState>,
    User(user): User,
    Path(batch_id): Path<String>,
    ValidQuery(query): ValidQuery<RemoveBatchQuery>,
) -> Result<Json<DiscoveryBatchRemoveResult>, ApiError> {
    DiscoveryBatches::new(&state)
        .remove(&user, &batch_id, query.remove_albums.unwrap_or(true))
        .await
        .map(Json)
        .map_err(ApiError::from)
}
