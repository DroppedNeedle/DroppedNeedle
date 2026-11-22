//! Request the songs a playlist names and the library lacks.
//!
//! The playlist's owner presses one button; each album with an entry that
//! has no library file is resolved to a MusicBrainz release group by its
//! title and artist, then filed as a request. A playlist can request,
//! never retire, replace or pin.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_catalog::collections::store::playlists::PlaylistStore;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Serialize;
use utoipa::ToSchema;

use crate::imports::playlist_pull::PlaylistPull;
use crate::imports::spotify_albums::AlbumResolver;
use crate::requests::state::RequestsState;

/// Every dependency the route needs.
#[derive(Clone)]
pub struct PlaylistRequestDeps {
    /// Request intake, with the library handle and the quota ledger.
    pub requests: RequestsState,
    /// Playlists, read as the caller.
    pub playlists: PlaylistStore,
    /// Finds each album's release group.
    pub resolver: Arc<AlbumResolver>,
    /// The hourly pull, for a person's check.
    pub pull: Arc<PlaylistPull>,
    /// The batch a press starts.
    pub acquire: Arc<crate::playlist_acquire::PlaylistAcquire>,
}

/// The route.
pub fn routes() -> Routes<PlaylistRequestDeps> {
    Routes::new()
        .user(routes!(request_missing_handler))
        .user(routes!(acquire_progress_handler))
        .user(routes!(pull_now_handler))
}

/// What one manual check found.
#[derive(Debug, Serialize, ToSchema)]
pub struct PullNowResponse {
    /// Whether the source changed and an import was queued.
    pub changed: bool,
}

/// Check a playlist's source now and queue an import when it changed.
/// `POST /api/v3/playlists/{playlist_id}/pull-now`. Owner only.
#[utoipa::path(
    post,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/pull-now",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses((status = 200, body = PullNowResponse), (status = 404, description = "Not found or not the caller's"))
)]
pub async fn pull_now_handler(
    State(deps): State<PlaylistRequestDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
) -> Result<Json<PullNowResponse>, ApiError> {
    let owned = deps.playlists.owned_by(principal.user_id());
    if owned
        .get(&playlist_id)
        .await
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found());
    }
    let changed = deps
        .pull
        .check_now(&playlist_id)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(PullNowResponse { changed }))
}

/// Request the albums a playlist names that the library lacks.
/// `POST /api/v3/playlists/{playlist_id}/request-missing`. Owner only.
#[utoipa::path(
    post,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/request-missing",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = AcquireBody,
    responses((status = 202, body = crate::playlist_acquire::AcquireState), (status = 404, description = "Not found or not the caller's"))
)]
pub async fn request_missing_handler(
    State(deps): State<PlaylistRequestDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<AcquireBody>,
) -> Result<(StatusCode, Json<crate::playlist_acquire::AcquireState>), ApiError> {
    let owned = deps.playlists.owned_by(principal.user_id());
    if owned
        .get(&playlist_id)
        .await
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found());
    }
    let items = if body.items.is_empty() {
        deps.acquire
            .items_from(principal.user_id(), &playlist_id)
            .await
    } else {
        body.items
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(deps.acquire.start(principal, playlist_id, items).await),
    ))
}

/// One press's batch, or the default list.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct AcquireBody {
    /// What the press asked for; empty asks for everything missing.
    #[serde(default)]
    pub items: Vec<crate::playlist_acquire::AcquireItem>,
}

/// The progress of a press.
/// `GET /api/v3/playlists/{playlist_id}/acquire-progress`. Owner only.
#[utoipa::path(
    get,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/acquire-progress",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses((status = 200, body = crate::playlist_acquire::AcquireState))
)]
pub async fn acquire_progress_handler(
    State(deps): State<PlaylistRequestDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
) -> Result<Json<crate::playlist_acquire::AcquireState>, ApiError> {
    let owned = deps.playlists.owned_by(principal.user_id());
    if owned
        .get(&playlist_id)
        .await
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found());
    }
    Ok(Json(
        deps.acquire.progress(&playlist_id).unwrap_or_default(),
    ))
}
