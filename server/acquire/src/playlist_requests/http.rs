//! The playlist request routes, and the one mapping of their errors onto
//! `ApiError`.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};

use super::models::{PlaylistRequestBody, PlaylistRequestResult, PlaylistRequestState};
use super::{PlaylistRequestError, PlaylistRequests};
use crate::requests::models::TrackIntakeResponse;

/// The song has no album found to ask for.
pub const NO_ALBUM_YET: &str = "NO_ALBUM_YET";
/// The song is not on its album's tracklist.
pub const SONG_NOT_ON_ALBUM: &str = "SONG_NOT_ON_ALBUM";
/// The song has a library file or a source it plays from.
pub const ALREADY_PLAYABLE: &str = "ALREADY_PLAYABLE";

impl From<PlaylistRequestError> for ApiError {
    fn from(error: PlaylistRequestError) -> Self {
        match error {
            PlaylistRequestError::NotFound => ApiError::not_found(),
            PlaylistRequestError::NoAlbumYet => ApiError::new(
                StatusCode::CONFLICT,
                NO_ALBUM_YET,
                "No album was found for this song yet",
            ),
            PlaylistRequestError::SongNotOnAlbum => ApiError::new(
                StatusCode::CONFLICT,
                SONG_NOT_ON_ALBUM,
                "This song is not on its album's tracklist",
            ),
            PlaylistRequestError::AlreadyPlayable => ApiError::new(
                StatusCode::CONFLICT,
                ALREADY_PLAYABLE,
                "This song can already be played",
            ),
            PlaylistRequestError::Requests(error) => error.into(),
            PlaylistRequestError::Catalog(error) => error.into(),
        }
    }
}

/// The playlist request routes: any signed-in user, answered only for a
/// playlist they own or may edit.
pub fn routes() -> Routes<PlaylistRequests> {
    Routes::new()
        .user(routes!(request_missing_handler))
        .user(routes!(request_song_handler))
        .user(routes!(request_state_handler))
}

/// Ask for the albums of the playlist's songs the library lacks, or with
/// `entry_id` for that entry's album, under the caller's approval and
/// quota. Albums owned, already asked for or rejected before are skipped;
/// one press files at most 200, and a plain user's what their quota has
/// room for.
#[utoipa::path(
    post,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/request-missing",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = PlaylistRequestBody,
    responses(
        (status = 202, description = "What the press filed", body = PlaylistRequestResult),
        (status = 400, description = "The idempotency key is not a UUID, or another press's"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Storage full (STORAGE_FULL)"),
        (status = 404, description = "No such playlist or entry the caller may edit"),
        (status = 409, description = "The entry has no album yet (NO_ALBUM_YET) or can already be played (ALREADY_PLAYABLE)"),
        (status = 429, description = "Request quota spent (QUOTA_EXCEEDED)"),
    )
)]
pub async fn request_missing_handler(
    State(state): State<PlaylistRequests>,
    User(user): User,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<PlaylistRequestBody>,
) -> Result<(StatusCode, Json<PlaylistRequestResult>), ApiError> {
    let result = state.request_missing(&user, &playlist_id, &body).await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

/// Ask for one entry's song alone, on its album.
#[utoipa::path(
    post,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/tracks/{track_id}/request-song",
    params(
        ("playlist_id" = String, Path, description = "Playlist id"),
        ("track_id" = String, Path, description = "Playlist entry id"),
    ),
    responses(
        (status = 202, description = "The song's request", body = TrackIntakeResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Storage full (STORAGE_FULL)"),
        (status = 404, description = "No such playlist or entry the caller may edit"),
        (status = 409, description = "No album yet (NO_ALBUM_YET), not on its album (SONG_NOT_ON_ALBUM) or already playable (ALREADY_PLAYABLE)"),
        (status = 429, description = "Request quota spent (QUOTA_EXCEEDED)"),
    )
)]
pub async fn request_song_handler(
    State(state): State<PlaylistRequests>,
    User(user): User,
    Path((playlist_id, track_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<TrackIntakeResponse>), ApiError> {
    let result = state.request_song(&user, &playlist_id, &track_id).await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

/// What each of the playlist's missing songs waits for, and the
/// playlist's Spotify import while it runs or after it failed.
#[utoipa::path(
    get,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/request-state",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "The missing songs' states", body = PlaylistRequestState),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such playlist the caller may edit"),
    )
)]
pub async fn request_state_handler(
    State(state): State<PlaylistRequests>,
    User(user): User,
    Path(playlist_id): Path<String>,
) -> Result<Json<PlaylistRequestState>, ApiError> {
    Ok(Json(state.state(&user, &playlist_id).await?))
}
