//! Library mutation routes: remove an album (admin) or a track (curator),
//! rescan one album (admin), re-enable management on an excluded album
//! (admin), and hand albums the app stepped back from to automatic
//! management again (admin). File removal always goes through the recycle bin; see
//! [`crate::mutations`]. An album's removals whose files are still in the
//! recycle bin are listed, and put back, by curators.

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

use super::blocking;
use super::error::refused;
use crate::mutations::{MutationError, Mutations, RemovalKind};
use crate::scan::models::{Disposition, ScanState};
use crate::wiring::LibraryServices;
use droppedneedle_auth::{Admin, Curator};

/// Mutation routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .admin(routes!(remove_album))
        .curator(routes!(remove_track))
        .admin(routes!(rescan_album))
        .admin(routes!(reenable_management))
        .admin(routes!(hand_back_management))
        .admin(routes!(hand_back_all))
        .curator(routes!(list_album_removals))
        .curator(routes!(put_back_removal))
}

/// One removal of an album whose files can be put back.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlbumRemovalView {
    /// The removal's id, which Put back names.
    pub id: String,
    pub kind: RemovalKind,
    /// When the files were removed, unix milliseconds.
    pub removed_at: i64,
    /// Files whose copies are still in the recycle bin.
    pub files: u32,
    /// A file write of the removal waits for an admin under "Some file
    /// writes need you"; it can be put back once that is settled.
    pub held: bool,
}

/// An album's removals whose files can be put back, newest first.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlbumRemovalsView {
    pub items: Vec<AlbumRemovalView>,
}

/// What a put-back did not bring back.
#[derive(Debug, Serialize, ToSchema)]
pub struct PutBackView {
    /// Files that were not put back because their copy is gone from the
    /// recycle bin or changed there, as paths inside their library folder.
    /// Empty when every file went back.
    pub missing: Vec<String>,
}

/// List the album's removals whose files are still in the recycle bin:
/// whole albums, single tracks and extras, newest first.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/removals",
    params(("album_id" = String, Path, description = "Local album id or release-group MBID")),
    responses(
        (status = 200, description = "Removals that can be put back", body = AlbumRemovalsView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn list_album_removals(
    State(mutations): State<Mutations>,
    _caller: Curator,
    Path(album_id): Path<String>,
) -> Result<Json<AlbumRemovalsView>, ApiError> {
    let listed = blocking(move || mutations.album_removals(&album_id)).await?;
    Ok(Json(AlbumRemovalsView {
        items: listed
            .into_iter()
            .map(|removal| AlbumRemovalView {
                id: removal.id,
                kind: removal.kind,
                removed_at: removal.removed_at,
                files: removal.files,
                held: removal.held,
            })
            .collect(),
    }))
}

/// Put back the files one removal moved to the recycle bin, and list
/// their tracks again with their plays and favourites. Another file at a
/// file's old place refuses the whole put-back; a copy gone from the bin
/// is named and the rest come back.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/removals/{removal_id}/put-back",
    params(
        ("album_id" = String, Path, description = "Local album id or release-group MBID"),
        ("removal_id" = String, Path, description = "The removal's id"),
    ),
    responses(
        (status = 200, description = "Files put back and indexed", body = PutBackView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album, or nothing left to put back"),
        (status = 409, description = "Another file now sits at an old path, a file or track changed, none of the files are left in the recycle bin, or the library is busy; nothing was put back"),
        (status = 422, description = "The files could not be put back; they stay in the recycle bin"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn put_back_removal(
    State(mutations): State<Mutations>,
    caller: Curator,
    Path((album_id, removal_id)): Path<(String, String)>,
) -> Result<Json<PutBackView>, ApiError> {
    let actor = caller.user_id().to_owned();
    let missing = blocking(move || mutations.put_back(&album_id, &removal_id, &actor)).await?;
    Ok(Json(PutBackView { missing }))
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
    pub disposition: Disposition,
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
    State(mutations): State<Mutations>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidQuery(query): ValidQuery<RemoveAlbumQuery>,
) -> Result<Json<RemovalResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let removed = blocking(move || {
        mutations.remove_album_now(&album_id, query.delete_files, query.stop_wanted, &actor)
    })
    .await?;
    Ok(Json(RemovalResponse {
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
    State(mutations): State<Mutations>,
    caller: Curator,
    Path(track_id): Path<String>,
    ValidQuery(query): ValidQuery<RemoveTrackQuery>,
) -> Result<Json<RemovalResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let removed =
        blocking(move || mutations.remove_track_now(&track_id, query.delete_file, &actor)).await?;
    Ok(Json(RemovalResponse {
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
    State(mutations): State<Mutations>,
    caller: Admin,
    Path(album_id): Path<String>,
) -> Result<(StatusCode, Json<AlbumRescanResponse>), ApiError> {
    let actor = caller.user_id().to_owned();
    let result = blocking(move || mutations.rescan_album(&album_id, &actor)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AlbumRescanResponse {
            run_id: result.run_id,
            disposition: result.disposition,
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
    State(mutations): State<Mutations>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<ReenableBody>,
) -> Result<Json<ReenableResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let reenabled = blocking(move || {
        mutations.reenable_management(&album_id, body.expected_exclusion_revision, &actor)
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
    State(mutations): State<Mutations>,
    caller: Admin,
    Path(album_id): Path<String>,
) -> Result<Json<HandBackResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let handed_back = blocking(move || mutations.hand_back_management(&album_id, &actor)).await?;
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
    State(mutations): State<Mutations>,
    caller: Admin,
) -> Result<Json<HandBackAllResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let albums = blocking(move || mutations.hand_back_all(&actor)).await?;
    Ok(Json(HandBackAllResponse { albums }))
}
