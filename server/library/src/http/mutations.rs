//! Library mutation routes: remove an album (admin) or a track (curator),
//! rescan one album (admin), re-enable management on an excluded album
//! (admin), and hand albums the app stepped back from to automatic
//! management again (admin). File removal always goes through the recycle bin; see
//! [`crate::mutations`].

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::error::refused;
use super::models::snake;
use crate::mutations::{MutationError, Mutations};
use crate::scan::models::ScanState;
use crate::wiring::LibrarySetup;
use droppedneedle_auth::{Admin, Curator};

/// Mutation routes.
pub fn routes() -> Routes<LibrarySetup> {
    Routes::new()
        .admin(routes!(remove_album))
        .curator(routes!(remove_track))
        .admin(routes!(rescan_album))
        .admin(routes!(reenable_management))
        .admin(routes!(hand_back_management))
        .admin(routes!(hand_back_all))
}

/// Album removal options.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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
#[into_params(parameter_in = Query)]
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
    pub state: ScanState,
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

/// Hand-back answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct HandBackResponse {
    /// False when DroppedNeedle had not stepped back from the album.
    pub handed_back: bool,
}

/// Answer to handing every album back.
#[derive(Debug, Serialize, ToSchema)]
pub struct HandBackAllResponse {
    /// Albums that now wait for their turn to be handed back.
    pub albums: usize,
}

impl From<MutationError> for ApiError {
    fn from(error: MutationError) -> Self {
        match error {
            MutationError::NotFound(reason) => refused(StatusCode::NOT_FOUND, reason),
            MutationError::Conflict(reason) => refused(StatusCode::CONFLICT, reason),
            MutationError::Files(reason) => refused(StatusCode::UNPROCESSABLE_ENTITY, reason),
            MutationError::Store(cause) => ApiError::internal(cause),
            MutationError::Busy(cause) => ApiError::unavailable(Unavailable::ServerBusy, cause),
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, MutationError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::from)
}

/// Remove an album from the library. With `delete_files` its files move
/// into the recycle bin; nothing is deleted outright. Its pending download
/// retries, held files and blocklist entries go, and its wanted watch
/// stops (or, with `stop_wanted=false`, looks for a replacement).
#[utoipa::path(
    delete,
    tag = "library management",
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
    caller: Admin,
    Path(album_id): Path<String>,
    ValidQuery(query): ValidQuery<RemoveAlbumQuery>,
) -> Result<Json<RemovalResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let hook = state.removal_hook.clone();
    let removed = blocking(move || {
        Mutations::new(&state).remove_album(&album_id, query.delete_files, &actor)
    })
    .await?;
    // The removal stands; download and wanted cleanup only follow it.
    if let Some(group) = removed.release_group_mbid.as_deref() {
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
    tag = "library management",
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
    caller: Curator,
    Path(track_id): Path<String>,
    ValidQuery(query): ValidQuery<RemoveTrackQuery>,
) -> Result<Json<RemovalResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
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
    tag = "library management",
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
    caller: Admin,
    Path(album_id): Path<String>,
) -> Result<(StatusCode, Json<AlbumRescanResponse>), ApiError> {
    let actor = caller.user_id().to_owned();
    let result = blocking(move || Mutations::new(&state).rescan_album(&album_id, &actor)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AlbumRescanResponse {
            run_id: result.run_id,
            disposition: snake(&result.disposition),
            state: result.state,
            queued_reason: result.queued_reason,
        }),
    ))
}

/// Let library management change an album again after it was excluded.
#[utoipa::path(
    post,
    tag = "library management",
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
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<ReenableBody>,
) -> Result<Json<ReenableResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
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

/// Have DroppedNeedle manage an album again after it stepped back from it
/// because the files were changed outside. The files are then retagged and
/// renamed from the album's edition, which overwrites those changes.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/management/hand-back",
    params(("album_id" = String, Path, description = "Local album id or release-group MBID")),
    responses(
        (status = 200, description = "Hand-back answer", body = HandBackResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Album not found"),
        (status = 409, description = "The album's files could not be read; it stays unmanaged"),
    )
)]
pub async fn hand_back_management(
    State(state): State<LibrarySetup>,
    caller: Admin,
    Path(album_id): Path<String>,
) -> Result<Json<HandBackResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let handed_back =
        blocking(move || Mutations::new(&state).hand_back_management(&album_id, &actor)).await?;
    Ok(Json(HandBackResponse { handed_back }))
}

/// Have DroppedNeedle manage every album again that it stepped back from.
/// The albums are handed back one after another in the background; until
/// its turn comes an album stays left unmanaged.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/hand-back",
    responses(
        (status = 200, description = "Albums waiting to be handed back", body = HandBackAllResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn hand_back_all(
    State(state): State<LibrarySetup>,
    caller: Admin,
) -> Result<Json<HandBackAllResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let albums = blocking(move || Mutations::new(&state).hand_back_all(&actor)).await?;
    Ok(Json(HandBackAllResponse { albums }))
}
