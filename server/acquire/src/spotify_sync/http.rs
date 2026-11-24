//! The sync routes, and the one mapping of their errors onto `ApiError`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};

use super::models::{PlaylistSync, PlaylistSyncBody};
use super::{PlaylistSyncError, SpotifySync};

/// The playlist did not come from Spotify.
pub const PLAYLIST_NOT_SYNCABLE: &str = "PLAYLIST_NOT_SYNCABLE";
/// The owner has not connected Spotify.
pub const SPOTIFY_NOT_LINKED: &str = "SPOTIFY_NOT_LINKED";
/// The owner's Spotify link cannot read Liked Songs.
pub const SPOTIFY_RECONNECT_NEEDED: &str = "SPOTIFY_RECONNECT_NEEDED";
/// Spotify is turned off on this server.
pub const SPOTIFY_OFF: &str = "SPOTIFY_OFF";

impl From<PlaylistSyncError> for ApiError {
    fn from(error: PlaylistSyncError) -> Self {
        let conflict = |code, message| ApiError::new(StatusCode::CONFLICT, code, message);
        match error {
            PlaylistSyncError::NotFound => ApiError::not_found(),
            PlaylistSyncError::NotSyncable => conflict(
                PLAYLIST_NOT_SYNCABLE,
                "Only a playlist imported from Spotify can be kept in sync.",
            ),
            PlaylistSyncError::NotLinked => conflict(
                SPOTIFY_NOT_LINKED,
                "Spotify isn't connected. Connect it on your Profile.",
            ),
            PlaylistSyncError::ReconnectNeeded => conflict(
                SPOTIFY_RECONNECT_NEEDED,
                "Reconnect Spotify on your Profile to sync Liked Songs.",
            ),
            PlaylistSyncError::SpotifyOff => {
                conflict(SPOTIFY_OFF, "Spotify is turned off on this server.")
            }
            PlaylistSyncError::Settings(error) => crate::settings::settings_failed(error).into(),
            PlaylistSyncError::Busy => {
                ApiError::unavailable(Unavailable::ServerBusy, "database busy: spotify sync")
            }
            PlaylistSyncError::Internal(cause) => ApiError::internal(cause),
        }
    }
}

/// The sync routes: any signed-in user, answered only for a playlist they
/// own (or, to read it, may edit).
pub fn routes() -> Routes<Arc<SpotifySync>> {
    Routes::new().user(routes!(get_sync_handler, put_sync_handler))
}

/// A playlist's Spotify sync: whether it is on, whether its songs are
/// requested by themselves, and how far it has got.
#[utoipa::path(
    get,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/sync",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "The playlist's sync", body = PlaylistSync),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such playlist the caller may edit"),
    )
)]
pub async fn get_sync_handler(
    State(sync): State<Arc<SpotifySync>>,
    User(user): User,
    Path(playlist_id): Path<String>,
) -> Result<Json<PlaylistSync>, ApiError> {
    Ok(Json(sync.state(&user, &playlist_id).await?))
}

/// Turn a playlist's Spotify sync on or off, for its owner. Turning it on
/// as a plain user asks an admin to approve its requests.
#[utoipa::path(
    put,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/sync",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = PlaylistSyncBody,
    responses(
        (status = 200, description = "The playlist's sync", body = PlaylistSync),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such playlist the caller owns"),
        (status = 409, description = "Not a Spotify playlist (PLAYLIST_NOT_SYNCABLE), Spotify not connected (SPOTIFY_NOT_LINKED), Liked Songs needs a reconnect (SPOTIFY_RECONNECT_NEEDED) or Spotify is off (SPOTIFY_OFF)"),
    )
)]
pub async fn put_sync_handler(
    State(sync): State<Arc<SpotifySync>>,
    User(user): User,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<PlaylistSyncBody>,
) -> Result<Json<PlaylistSync>, ApiError> {
    Ok(Json(sync.set(&user, &playlist_id, &body).await?))
}
