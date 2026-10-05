//! Collections handlers. Thin: extract, call the service, render.
//!
//! Utoipa annotations carry the full `/api/v3` paths for the contract
//! document; [`super::collections_routes`] mounts them relative to it.

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use super::{
    auth::Principal,
    http::{HttpResult, ValidJson, ValidQuery},
    models::{
        AddTracksBody, AddTracksResponse, ApprovalBatchListResponse,
        AutoDownloadApprovalListResponse, AutoDownloadBody, CheckTracksBody, CheckTracksResponse,
        CoverUploadBody, CoverUploadResponse, CreatePlaylistBody, EditionPinBody,
        EditionPinResponse, FavoriteListResponse, FavoriteQuery, FavoriteStatusResponse,
        FollowBody, FollowStatusResponse, FollowedArtistListResponse, NewReleaseListResponse,
        PlaylistDetail, PlaylistListResponse, PlaylistSummary, PlaylistTrack, RemoveTracksBody,
        RemoveTracksResponse, ReorderBody, ReorderResponse, ResolveSourcesResponse,
        SetFavoriteBody, StatusResponse, UnseenCountResponse, UpdatePlaylistBody, UpdateTrackBody,
        VisibilityBody,
    },
    service::CollectionsService,
    state::CollectionsState,
};

/// A plain "ok" receipt.
fn ok(message: &str) -> StatusResponse {
    StatusResponse {
        status: "ok".to_owned(),
        message: message.to_owned(),
    }
}

/// List visible playlists.
#[utoipa::path(
    get,
    path = "/api/v3/playlists",
    responses(
        (status = 200, description = "Visible playlists", body = PlaylistListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_playlists_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<PlaylistListResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .list_playlists(&caller.user_id)
            .await?,
    ))
}

/// Create a playlist.
#[utoipa::path(
    post,
    path = "/api/v3/playlists",
    request_body = CreatePlaylistBody,
    responses(
        (status = 201, description = "Created playlist", body = PlaylistDetail),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn create_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    ValidJson(body): ValidJson<CreatePlaylistBody>,
) -> HttpResult<(StatusCode, Json<PlaylistDetail>)> {
    let detail = CollectionsService::new(&state)
        .create_playlist(&caller.user_id, &body)
        .await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

/// Read one playlist.
#[utoipa::path(
    get,
    path = "/api/v3/playlists/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Playlist detail", body = PlaylistDetail),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn get_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> HttpResult<Json<PlaylistDetail>> {
    Ok(Json(
        CollectionsService::new(&state)
            .get_playlist(&caller.user_id, &playlist_id)
            .await?,
    ))
}

/// Rename a playlist.
#[utoipa::path(
    put,
    path = "/api/v3/playlists/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = UpdatePlaylistBody,
    responses(
        (status = 200, description = "Renamed playlist", body = PlaylistDetail),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn update_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<UpdatePlaylistBody>,
) -> HttpResult<Json<PlaylistDetail>> {
    Ok(Json(
        CollectionsService::new(&state)
            .update_playlist(&caller.user_id, &playlist_id, &body)
            .await?,
    ))
}

/// Delete a playlist.
#[utoipa::path(
    delete,
    path = "/api/v3/playlists/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Deletion receipt", body = StatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn delete_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> HttpResult<Json<StatusResponse>> {
    CollectionsService::new(&state)
        .delete_playlist(&caller.user_id, &playlist_id)
        .await?;
    Ok(Json(ok("Playlist deleted")))
}

/// Flip playlist visibility.
#[utoipa::path(
    patch,
    path = "/api/v3/playlists/{playlist_id}/visibility",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = VisibilityBody,
    responses(
        (status = 200, description = "Updated summary", body = PlaylistSummary),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn set_visibility_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<VisibilityBody>,
) -> HttpResult<Json<PlaylistSummary>> {
    Ok(Json(
        CollectionsService::new(&state)
            .set_visibility(&caller.user_id, &playlist_id, body.is_public)
            .await?,
    ))
}

/// Add tracks to a playlist.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/tracks",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = AddTracksBody,
    responses(
        (status = 200, description = "Added tracks", body = AddTracksResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn add_tracks_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<AddTracksBody>,
) -> HttpResult<Json<AddTracksResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .add_tracks(&caller.user_id, &playlist_id, &body)
            .await?,
    ))
}

/// Bulk-remove tracks from a playlist.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/tracks/remove",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = RemoveTracksBody,
    responses(
        (status = 200, description = "Removal receipt", body = RemoveTracksResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn remove_tracks_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<RemoveTracksBody>,
) -> HttpResult<Json<RemoveTracksResponse>> {
    let removed = CollectionsService::new(&state)
        .remove_entries(&caller.user_id, &playlist_id, &body.track_ids)
        .await?;
    Ok(Json(RemoveTracksResponse {
        status: "ok".to_owned(),
        message: format!("Removed {removed} track(s)"),
        removed,
    }))
}

/// Remove one track from a playlist.
#[utoipa::path(
    delete,
    path = "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
    params(
        ("playlist_id" = String, Path, description = "Playlist id"),
        ("track_id" = String, Path, description = "Track row id")
    ),
    responses(
        (status = 200, description = "Removal receipt", body = StatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist or track id"),
    )
)]
pub async fn remove_track_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path((playlist_id, track_id)): Path<(String, String)>,
) -> HttpResult<Json<StatusResponse>> {
    CollectionsService::new(&state)
        .remove_entry(&caller.user_id, &playlist_id, &track_id)
        .await?;
    Ok(Json(ok("Track removed")))
}

/// Reorder one track inside a playlist.
#[utoipa::path(
    patch,
    path = "/api/v3/playlists/{playlist_id}/tracks/reorder",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = ReorderBody,
    responses(
        (status = 200, description = "Reorder receipt", body = ReorderResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist or track id"),
    )
)]
pub async fn reorder_track_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<ReorderBody>,
) -> HttpResult<Json<ReorderResponse>> {
    let actual_position = CollectionsService::new(&state)
        .move_entry(
            &caller.user_id,
            &playlist_id,
            &body.track_id,
            body.new_position,
        )
        .await?;
    Ok(Json(ReorderResponse {
        status: "ok".to_owned(),
        message: "Track reordered".to_owned(),
        actual_position,
    }))
}

/// Update one track's source fields.
#[utoipa::path(
    patch,
    path = "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
    params(
        ("playlist_id" = String, Path, description = "Playlist id"),
        ("track_id" = String, Path, description = "Track row id")
    ),
    request_body = UpdateTrackBody,
    responses(
        (status = 200, description = "Updated track", body = PlaylistTrack),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist or track id"),
    )
)]
pub async fn update_track_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path((playlist_id, track_id)): Path<(String, String)>,
    ValidJson(body): ValidJson<UpdateTrackBody>,
) -> HttpResult<Json<PlaylistTrack>> {
    Ok(Json(
        CollectionsService::new(&state)
            .update_track(&caller.user_id, &playlist_id, &track_id, &body)
            .await?,
    ))
}

/// Check which visible playlists hold each queried track.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/check-tracks",
    request_body = CheckTracksBody,
    responses(
        (status = 200, description = "Membership map", body = CheckTracksResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn check_tracks_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    ValidJson(body): ValidJson<CheckTracksBody>,
) -> HttpResult<Json<CheckTracksResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .check_track_membership(&caller.user_id, &body)
            .await?,
    ))
}

/// Resolve each track's known sources.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/resolve-sources",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Source map", body = ResolveSourcesResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn resolve_sources_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> HttpResult<Json<ResolveSourcesResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .resolve_sources(&caller.user_id, &playlist_id)
            .await?,
    ))
}

/// Upload a playlist cover.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/cover",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = CoverUploadBody,
    responses(
        (status = 200, description = "Cover URL", body = CoverUploadResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn upload_cover_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<CoverUploadBody>,
) -> HttpResult<Json<CoverUploadResponse>> {
    let cover_url = CollectionsService::new(&state)
        .upload_cover(&caller.user_id, &playlist_id, &body)
        .await?;
    Ok(Json(CoverUploadResponse { cover_url }))
}

/// Serve playlist cover bytes.
#[utoipa::path(
    get,
    path = "/api/v3/playlists/{playlist_id}/cover",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Cover bytes"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn get_cover_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> HttpResult<Response> {
    let cover = CollectionsService::new(&state)
        .cover(&caller.user_id, &playlist_id)
        .await?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, cover.content_type)],
        cover.bytes,
    )
        .into_response())
}

/// Delete a playlist cover.
#[utoipa::path(
    delete,
    path = "/api/v3/playlists/{playlist_id}/cover",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Removal receipt", body = StatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn remove_cover_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> HttpResult<Json<StatusResponse>> {
    CollectionsService::new(&state)
        .remove_cover(&caller.user_id, &playlist_id)
        .await?;
    Ok(Json(ok("Cover removed")))
}

/// List the caller's favorites.
#[utoipa::path(
    get,
    path = "/api/v3/favorites",
    params(("kind" = Option<String>, Query, description = "Kind filter: album, artist, or track")),
    responses(
        (status = 200, description = "Caller favorites", body = FavoriteListResponse),
        (status = 400, description = "Bad kind filter"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_favorites_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    ValidQuery(query): ValidQuery<FavoriteQuery>,
) -> HttpResult<Json<FavoriteListResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .list_favorites(&caller.user_id, query.kind.as_deref())
            .await?,
    ))
}

/// Favorite or unfavorite one item.
#[utoipa::path(
    put,
    path = "/api/v3/favorites/{kind}/{item_id}",
    params(
        ("kind" = String, Path, description = "Kind: album, artist, or track"),
        ("item_id" = String, Path, description = "Favorited item id")
    ),
    request_body = SetFavoriteBody,
    responses(
        (status = 200, description = "Favorite status", body = FavoriteStatusResponse),
        (status = 400, description = "Bad kind or body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn set_favorite_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path((kind, item_id)): Path<(String, String)>,
    ValidJson(body): ValidJson<SetFavoriteBody>,
) -> HttpResult<Json<FavoriteStatusResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .set_favorite(&caller.user_id, &kind, &item_id, body.favorited, body.name)
            .await?,
    ))
}

/// Read one artist's follow status.
#[utoipa::path(
    get,
    path = "/api/v3/artists/{artist_mbid}/follow-status",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    responses(
        (status = 200, description = "Follow status", body = FollowStatusResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_follow_status_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(artist_mbid): Path<String>,
) -> HttpResult<Json<FollowStatusResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .follow_status(&caller.user_id, &artist_mbid)
            .await?,
    ))
}

/// Follow or unfollow one artist.
#[utoipa::path(
    put,
    path = "/api/v3/artists/{artist_mbid}/follow",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    request_body = FollowBody,
    responses(
        (status = 200, description = "Follow status", body = FollowStatusResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn set_follow_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(artist_mbid): Path<String>,
    ValidJson(body): ValidJson<FollowBody>,
) -> HttpResult<Json<FollowStatusResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .set_follow(
                &caller.user_id,
                &artist_mbid,
                body.followed,
                body.artist_name,
            )
            .await?,
    ))
}

/// Turn auto-download on or off for one artist.
#[utoipa::path(
    put,
    path = "/api/v3/artists/{artist_mbid}/auto-download",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    request_body = AutoDownloadBody,
    responses(
        (status = 200, description = "Follow status", body = FollowStatusResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Artist not followed"),
        (status = 409, description = "Follow before enabling auto-download"),
    )
)]
pub async fn set_auto_download_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(artist_mbid): Path<String>,
    ValidJson(body): ValidJson<AutoDownloadBody>,
) -> HttpResult<Json<FollowStatusResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .set_auto_download(&caller, &artist_mbid, body.enabled)
            .await?,
    ))
}

/// List the caller's followed artists.
#[utoipa::path(
    get,
    path = "/api/v3/following/artists",
    responses(
        (status = 200, description = "Followed artists", body = FollowedArtistListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_followed_artists_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<FollowedArtistListResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .list_followed(&caller.user_id)
            .await?,
    ))
}

/// List new-release sightings for followed artists.
#[utoipa::path(
    get,
    path = "/api/v3/following/new-releases",
    responses(
        (status = 200, description = "New releases", body = NewReleaseListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_new_releases_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<NewReleaseListResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .new_releases(&caller.user_id)
            .await?,
    ))
}

/// List recent-window sightings for followed artists.
#[utoipa::path(
    get,
    path = "/api/v3/following/new-releases/recent",
    responses(
        (status = 200, description = "Recent releases", body = NewReleaseListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_recent_releases_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<NewReleaseListResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .recent_releases(&caller.user_id)
            .await?,
    ))
}

/// Count unseen sightings for the caller.
#[utoipa::path(
    get,
    path = "/api/v3/following/new-releases/unseen-count",
    responses(
        (status = 200, description = "Unseen count", body = UnseenCountResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn unseen_count_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<UnseenCountResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .unseen_count(&caller.user_id)
            .await?,
    ))
}

/// Mark the caller's sightings seen.
#[utoipa::path(
    post,
    path = "/api/v3/following/new-releases/seen",
    responses(
        (status = 200, description = "Zeroed count", body = UnseenCountResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn mark_seen_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<UnseenCountResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .mark_seen(&caller.user_id)
            .await?,
    ))
}

/// List pending auto-download requests.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approvals",
    responses(
        (status = 200, description = "Pending approvals", body = AutoDownloadApprovalListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_approvals_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<AutoDownloadApprovalListResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .list_approvals(&caller)
            .await?,
    ))
}

/// List pending requests grouped per user.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approval-batches",
    responses(
        (status = 200, description = "Approval batches", body = ApprovalBatchListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_approval_batches_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> HttpResult<Json<ApprovalBatchListResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .list_approval_batches(&caller)
            .await?,
    ))
}

/// Read the pin display lane for one album.
#[utoipa::path(
    get,
    path = "/api/v3/library/albums/{album_id}/edition-pin",
    params(("album_id" = String, Path, description = "Album id")),
    responses(
        (status = 200, description = "Pin display", body = EditionPinResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn get_pin_handler(
    State(state): State<CollectionsState>,
    _caller: Principal,
    Path(album_id): Path<String>,
) -> HttpResult<Json<EditionPinResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .edition_pin(&album_id)
            .await?,
    ))
}

/// Pin one edition for an album.
#[utoipa::path(
    put,
    path = "/api/v3/library/albums/{album_id}/edition-pin",
    params(("album_id" = String, Path, description = "Album id")),
    request_body = EditionPinBody,
    responses(
        (status = 200, description = "Pin display", body = EditionPinResponse),
        (status = 400, description = "Unknown edition for this album"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn set_pin_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<EditionPinBody>,
) -> HttpResult<Json<EditionPinResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .set_edition_pin(&caller, &album_id, &body.release_mbid)
            .await?,
    ))
}

/// Clear the pin for an album.
#[utoipa::path(
    delete,
    path = "/api/v3/library/albums/{album_id}/edition-pin",
    params(("album_id" = String, Path, description = "Album id")),
    responses(
        (status = 200, description = "Pin display", body = EditionPinResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn clear_pin_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(album_id): Path<String>,
) -> HttpResult<Json<EditionPinResponse>> {
    Ok(Json(
        CollectionsService::new(&state)
            .clear_edition_pin(&caller, &album_id)
            .await?,
    ))
}
