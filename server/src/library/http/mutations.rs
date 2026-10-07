//! Library mutation routes: remove an album (admin) or a track (curator),
//! rescan one album (admin), and re-enable management on an excluded
//! album (admin). File removal always goes through the recycle bin; see
//! [`crate::library::mutations`].

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, post},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::auth::{RequireAdmin, RequireCurator};
use super::error::{LibraryError, ValidJson};
use super::models::snake;
use crate::library::mutations::{MutationError, Mutations};
use crate::library::wiring::LibrarySetup;

/// Mutation routes, relative for nesting under `/api/v3`.
pub fn mutations_router() -> Router<LibrarySetup> {
    Router::new()
        .route("/library/albums/{id}", delete(remove_album))
        .route("/library/tracks/{id}", delete(remove_track))
        .route("/library/albums/{album_id}/rescan", post(rescan_album))
        .route(
            "/library/albums/{album_id}/management/re-enable",
            post(reenable_management),
        )
}

/// Album removal options.
#[derive(Debug, Deserialize, IntoParams)]
pub struct RemoveAlbumQuery {
    /// Move the album's files into the recycle bin too. Off by default:
    /// the album only leaves the catalog.
    #[serde(default)]
    pub delete_files: bool,
    /// Stop the album's wanted watch (default). False keeps looking for a
    /// replacement: a fulfilled watch starts watching again.
    #[serde(default = "default_true")]
    pub stop_wanted: bool,
}

/// Track removal options.
#[derive(Debug, Deserialize, IntoParams)]
pub struct RemoveTrackQuery {
    /// Move the track's file into the recycle bin too. On by default, as
    /// in v2 (where it deleted the file outright).
    #[serde(default = "default_true")]
    pub delete_file: bool,
}

fn default_true() -> bool {
    true
}

/// What a removal took out of the catalog.
#[derive(Debug, Serialize, ToSchema)]
pub struct RemovalResponse {
    /// Always true on a 200.
    pub success: bool,
    /// The removed album (canonical local id) or track id.
    pub id: String,
    /// Track ids now gone from the catalog.
    pub removed_track_ids: Vec<String>,
}

/// The rescan request's answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlbumRescanResponse {
    /// Run id (fresh or merged into a waiting run).
    pub run_id: String,
    /// `started`, `queued`, `coalesced`, `expanded` or `conflict`.
    pub disposition: String,
    /// Run state after the request.
    pub state: String,
    /// Why the run waits, when it does.
    pub queued_reason: Option<String>,
}

/// Re-enable body: the exclusion revision the page showed.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ReenableBody {
    /// Row revision of the exclusion being cleared.
    pub expected_exclusion_revision: i64,
}

/// Re-enable answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReenableResponse {
    /// False when the album was not excluded.
    pub reenabled: bool,
}

/// A refused mutation in the shared error envelope, with the action in
/// `details.action`.
pub struct MutationHttpError(MutationError);

impl From<MutationError> for MutationHttpError {
    fn from(error: MutationError) -> Self {
        Self(error)
    }
}

impl IntoResponse for MutationHttpError {
    fn into_response(self) -> Response {
        use crate::error::envelope_response;
        let (status, reason) = match self.0 {
            MutationError::NotFound(reason) => (StatusCode::NOT_FOUND, reason),
            MutationError::Conflict(reason) => (StatusCode::CONFLICT, reason),
            MutationError::Files(reason) => (StatusCode::UNPROCESSABLE_ENTITY, reason),
            MutationError::Store(cause) => return LibraryError::internal(&cause).into_response(),
        };
        envelope_response(
            status,
            reason.code,
            reason.message,
            Some(serde_json::json!({ "action": reason.action })),
        )
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, MutationError> + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause).into_response())?
        .map_err(|error| MutationHttpError(error).into_response())
}

/// Remove an album from the library. With `delete_files` its files move
/// into the recycle bin; nothing is deleted outright. Its pending download
/// retries, held files and blocklist entries go, and its wanted watch
/// stops (or, with `stop_wanted=false`, looks for a replacement).
#[utoipa::path(
    delete,
    path = "/api/v3/library/albums/{id}",
    params(
        ("id" = String, Path, description = "Local album id or release-group MBID"),
        RemoveAlbumQuery,
    ),
    responses(
        (status = 200, description = "Album removed", body = RemovalResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Album not found"),
        (status = 409, description = "Library busy or a file is outside the library folders"),
        (status = 422, description = "A file could not be recycled; nothing changed"),
    )
)]
pub async fn remove_album(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    Query(query): Query<RemoveAlbumQuery>,
) -> Result<Json<RemovalResponse>, Response> {
    let actor = caller.0.user_id;
    let hook = state.removal_hook.get().cloned();
    let removed = blocking(move || {
        Mutations::new(&state).remove_album(&album_id, query.delete_files, &actor)
    })
    .await?;
    // The removal stands; download and wanted cleanup only follow it.
    if let (Some(hook), Some(group)) = (hook, removed.release_group_mbid.as_deref()) {
        hook.album_removed(group, query.stop_wanted).await;
    }
    Ok(Json(RemovalResponse {
        success: true,
        id: removed.id,
        removed_track_ids: removed.track_ids,
    }))
}

/// Remove one track. Its file moves into the recycle bin unless
/// `delete_file=false`.
#[utoipa::path(
    delete,
    path = "/api/v3/library/tracks/{id}",
    params(
        ("id" = String, Path, description = "Local track id"),
        RemoveTrackQuery,
    ),
    responses(
        (status = 200, description = "Track removed", body = RemovalResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Track not found"),
        (status = 409, description = "Library busy or the file is outside the library folders"),
        (status = 422, description = "The file could not be recycled; nothing changed"),
    )
)]
pub async fn remove_track(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(track_id): Path<String>,
    Query(query): Query<RemoveTrackQuery>,
) -> Result<Json<RemovalResponse>, Response> {
    let actor = caller.0.user_id;
    let removed =
        blocking(move || Mutations::new(&state).remove_track(&track_id, query.delete_file, &actor))
            .await?;
    Ok(Json(RemovalResponse {
        success: true,
        id: removed.id,
        removed_track_ids: removed.track_ids,
    }))
}

/// Rescan the folders holding one album's files.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/rescan",
    params(("album_id" = String, Path, description = "Local album id or release-group MBID")),
    responses(
        (status = 202, description = "Rescan requested", body = AlbumRescanResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Album not found or has no files"),
        (status = 409, description = "The library could not start a scan"),
    )
)]
pub async fn rescan_album(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
) -> Result<(StatusCode, Json<AlbumRescanResponse>), Response> {
    let actor = caller.0.user_id;
    let result = blocking(move || Mutations::new(&state).rescan_album(&album_id, &actor)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AlbumRescanResponse {
            run_id: result.run_id,
            disposition: snake(&result.disposition),
            state: snake(&result.state),
            queued_reason: result.queued_reason,
        }),
    ))
}

/// Let library management change an album again after it was excluded.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/management/re-enable",
    params(("album_id" = String, Path, description = "Local album id or release-group MBID")),
    request_body = ReenableBody,
    responses(
        (status = 200, description = "Re-enable answer", body = ReenableResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Album not found"),
        (status = 409, description = "The exclusion changed; reload and retry"),
    )
)]
pub async fn reenable_management(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<ReenableBody>,
) -> Result<Json<ReenableResponse>, Response> {
    let actor = caller.0.user_id;
    let reenabled = blocking(move || {
        Mutations::new(&state).reenable_management(
            &album_id,
            body.expected_exclusion_revision,
            &actor,
        )
    })
    .await?;
    Ok(Json(ReenableResponse { reenabled }))
}
