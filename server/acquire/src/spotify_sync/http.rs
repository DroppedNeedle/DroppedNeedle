//! The sync routes, and the one mapping of their errors onto `ApiError`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};

use super::grant::{GrantDecided, PlaylistSyncApprovalsResponse};
use super::keep::SpotifyAlbumKept;
use super::models::{PlaylistSync, PlaylistSyncBody};
use super::{PlaylistSyncError, SpotifySync};
use crate::requests::ledger::ApprovalState;
use crate::requests::models::ActionResponse;

/// The playlist did not come from Spotify.
pub const PLAYLIST_NOT_SYNCABLE: &str = "PLAYLIST_NOT_SYNCABLE";
/// The owner has not connected Spotify.
pub const SPOTIFY_NOT_LINKED: &str = "SPOTIFY_NOT_LINKED";
/// The owner's Spotify link cannot read Liked Songs.
pub const SPOTIFY_RECONNECT_NEEDED: &str = "SPOTIFY_RECONNECT_NEEDED";
/// Spotify is turned off on this server.
pub const SPOTIFY_OFF: &str = "SPOTIFY_OFF";
/// The song was not moved off Spotify's album.
pub const ALBUM_NOT_MOVED: &str = "ALBUM_NOT_MOVED";
/// The moved song plays now, or is on another album.
pub const ENTRY_CHANGED: &str = "ENTRY_CHANGED";

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
            PlaylistSyncError::NotMoved => conflict(
                ALBUM_NOT_MOVED,
                "This song is already on the album Spotify lists it on.",
            ),
            PlaylistSyncError::EntryChanged => conflict(
                ENTRY_CHANGED,
                "This song plays now or is on another album, so it stays as it is.",
            ),
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
    Routes::new()
        .user(routes!(get_sync_handler, put_sync_handler))
        .user(routes!(keep_spotify_album_handler))
        .admin(routes!(sync_approvals_handler))
        .admin(routes!(approve_sync_handler))
        .admin(routes!(reject_sync_handler))
        .admin(routes!(revoke_sync_handler))
}

/// Keep a song the sync moved to the artist's own album on the album
/// Spotify lists it on, for the playlist's owner. The sync's waiting
/// request for the moved album is cancelled, and the next pass asks for
/// the song on Spotify's album.
#[utoipa::path(
    post,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/tracks/{track_id}/spotify-album",
    params(
        ("playlist_id" = String, Path, description = "Playlist id"),
        ("track_id" = String, Path, description = "Playlist entry id"),
    ),
    responses(
        (status = 200, description = "The song is on Spotify's album", body = SpotifyAlbumKept),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such playlist the caller owns"),
        (status = 409, description = "The song was not moved (ALBUM_NOT_MOVED), or plays now or is on another album (ENTRY_CHANGED)"),
    )
)]
pub async fn keep_spotify_album_handler(
    State(sync): State<Arc<SpotifySync>>,
    User(user): User,
    Path((playlist_id, track_id)): Path<(String, String)>,
) -> Result<Json<SpotifyAlbumKept>, ApiError> {
    Ok(Json(
        sync.keep_spotify_album(&user, &playlist_id, &track_id)
            .await?,
    ))
}

/// The reply to a decision on a sync's grant.
fn decided(outcome: GrantDecided) -> ActionResponse {
    let (success, message) = match outcome {
        GrantDecided::NoMatch => (false, "No matching approval found"),
        GrantDecided::Approved => (true, "Playlist sync approved"),
        GrantDecided::Rejected => (true, "Playlist sync turned down"),
        GrantDecided::Revoked => (true, "Playlist sync approval taken back"),
    };
    ActionResponse {
        success,
        message: message.to_owned(),
    }
}

/// The synced playlists of plain users waiting for an admin to approve
/// their requests, and the approvals that stand.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/requests/playlist-sync-approvals",
    responses(
        (status = 200, description = "Sync asks and approvals", body = PlaylistSyncApprovalsResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not an admin"),
    )
)]
pub async fn sync_approvals_handler(
    State(sync): State<Arc<SpotifySync>>,
    admin: Admin,
) -> Result<Json<PlaylistSyncApprovalsResponse>, ApiError> {
    Ok(Json(sync.approvals(&admin).await?))
}

/// Approve a synced playlist's requests: its songs are then requested by
/// themselves, inside the owner's quota.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/playlist-sync-approvals/{playlist_id}/approve",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "The decision", body = ActionResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not an admin"),
    )
)]
pub async fn approve_sync_handler(
    State(sync): State<Arc<SpotifySync>>,
    admin: Admin,
    Path(playlist_id): Path<String>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(decided(
        sync.decide(&admin, &playlist_id, ApprovalState::Approved)
            .await?,
    )))
}

/// Turn down a synced playlist's requests. Its new songs are still added.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/playlist-sync-approvals/{playlist_id}/reject",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "The decision", body = ActionResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not an admin"),
    )
)]
pub async fn reject_sync_handler(
    State(sync): State<Arc<SpotifySync>>,
    admin: Admin,
    Path(playlist_id): Path<String>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(decided(
        sync.decide(&admin, &playlist_id, ApprovalState::Rejected)
            .await?,
    )))
}

/// Take back a synced playlist's approval. Its new songs are still added.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/playlist-sync-approvals/{playlist_id}/revoke",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "The decision", body = ActionResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not an admin"),
    )
)]
pub async fn revoke_sync_handler(
    State(sync): State<Arc<SpotifySync>>,
    admin: Admin,
    Path(playlist_id): Path<String>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(decided(
        sync.decide(&admin, &playlist_id, ApprovalState::Revoked)
            .await?,
    )))
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
