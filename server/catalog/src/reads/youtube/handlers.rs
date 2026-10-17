//! Thin handlers for the YouTube link routes. Every route is a plain
//! signed-in-user route: links are shared by everyone on the server, as in
//! v2. Status mapping lives in `From<LinkError> for ApiError`.

use axum::{
    Json, Router,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};

use crate::reads::collections::db::StoreError;
use crate::reads::discover::error::NOT_CONFIGURED;
use crate::reads::discover::handlers::AuthenticatedUser;
use droppedneedle_kernel::error::{ApiError, NOT_FOUND, RATE_LIMITED, Unavailable};
use droppedneedle_kernel::extract::ValidJson;

use super::models::{
    YouTubeLink, YouTubeLinkGenerateRequest, YouTubeLinkResponse, YouTubeLinkUpdateRequest,
    YouTubeManualLinkRequest, YouTubeQuotaStatus, YouTubeTrackLink,
    YouTubeTrackLinkBatchGenerateRequest, YouTubeTrackLinkBatchResponse,
    YouTubeTrackLinkGenerateRequest, YouTubeTrackLinkResponse,
};
use super::service::{LinkError, YouTubeLinks};

type Reply<T> = Result<T, ApiError>;

/// The YouTube link routes. Mount under `/api/v3` inside the session gate.
pub fn router(links: YouTubeLinks) -> Router {
    Router::new()
        .route("/youtube/generate", post(generate_link))
        .route(
            "/youtube/link/{album_id}",
            get(get_link).put(update_link).delete(delete_link),
        )
        .route("/youtube/links", get(list_links))
        .route("/youtube/manual", post(save_manual_link))
        .route("/youtube/generate-track", post(generate_track_link))
        .route("/youtube/generate-tracks", post(generate_track_links))
        .route("/youtube/track-links/{album_id}", get(list_track_links))
        .route(
            "/youtube/track-link/{album_id}/{disc_number}/{track_number}",
            delete(delete_track_link),
        )
        .route("/youtube/quota", get(get_quota))
        .with_state(links)
}

/// Find and save a full-album video. A saved one answers without a search.
#[utoipa::path(
    post,
    tag = "youtube",
    path = "/api/v3/youtube/generate",
    request_body = YouTubeLinkGenerateRequest,
    responses(
        (status = 200, description = "Saved link and today's budget", body = YouTubeLinkResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "YouTube had no video for the album"),
        (status = 429, description = "Today's search budget is spent (RATE_LIMITED)"),
        (status = 502, description = "YouTube failed"),
        (status = 409, description = "YouTube search is not set up (NOT_CONFIGURED)"),
    )
)]
pub async fn generate_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    ValidJson(body): ValidJson<YouTubeLinkGenerateRequest>,
) -> Reply<Json<YouTubeLinkResponse>> {
    let link = links.generate_link(body).await.map_err(ApiError::from)?;
    let quota = links.quota().await;
    Ok(Json(YouTubeLinkResponse { link, quota }))
}

/// One album's saved link. 204 when there is none.
#[utoipa::path(
    get,
    tag = "youtube",
    path = "/api/v3/youtube/link/{album_id}",
    params(("album_id" = String, Path, description = "Album id")),
    responses(
        (status = 200, description = "Saved link", body = YouTubeLink),
        (status = 204, description = "No link saved for the album"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    Path(album_id): Path<String>,
) -> Reply<Response> {
    let link = links.link(&album_id).await.map_err(ApiError::from)?;
    Ok(match link {
        Some(link) => Json(link).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

/// Every saved album link, newest first.
#[utoipa::path(
    get,
    tag = "youtube",
    path = "/api/v3/youtube/links",
    responses(
        (status = 200, description = "Saved links", body = Vec<YouTubeLink>),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_links(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
) -> Reply<Json<Vec<YouTubeLink>>> {
    links.links().await.map(Json).map_err(ApiError::from)
}

/// Remove an album link and its track links.
#[utoipa::path(
    delete,
    tag = "youtube",
    path = "/api/v3/youtube/link/{album_id}",
    params(("album_id" = String, Path, description = "Album id")),
    responses(
        (status = 204, description = "Removed, or nothing was saved"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn delete_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    Path(album_id): Path<String>,
) -> Reply<StatusCode> {
    links
        .delete_link(&album_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Edit a saved link: video, names or cover.
#[utoipa::path(
    put,
    tag = "youtube",
    path = "/api/v3/youtube/link/{album_id}",
    params(("album_id" = String, Path, description = "Album id")),
    request_body = YouTubeLinkUpdateRequest,
    responses(
        (status = 200, description = "Edited link", body = YouTubeLink),
        (status = 400, description = "Bad body or not a YouTube URL"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No link saved for the album"),
    )
)]
pub async fn update_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<YouTubeLinkUpdateRequest>,
) -> Reply<Json<YouTubeLink>> {
    links
        .update_link(&album_id, body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Save a video a person pasted, for a known album or a made-up one.
#[utoipa::path(
    post,
    tag = "youtube",
    path = "/api/v3/youtube/manual",
    request_body = YouTubeManualLinkRequest,
    responses(
        (status = 200, description = "Saved link", body = YouTubeLink),
        (status = 400, description = "Bad body or not a YouTube URL"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn save_manual_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    ValidJson(body): ValidJson<YouTubeManualLinkRequest>,
) -> Reply<Json<YouTubeLink>> {
    links
        .save_manual_link(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Find and save one track's video. A saved one answers without a search.
#[utoipa::path(
    post,
    tag = "youtube",
    path = "/api/v3/youtube/generate-track",
    request_body = YouTubeTrackLinkGenerateRequest,
    responses(
        (status = 200, description = "Saved track link and today's budget", body = YouTubeTrackLinkResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "YouTube had no video for the track"),
        (status = 429, description = "Today's search budget is spent (RATE_LIMITED)"),
        (status = 502, description = "YouTube failed"),
        (status = 409, description = "YouTube search is not set up (NOT_CONFIGURED)"),
    )
)]
pub async fn generate_track_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    ValidJson(body): ValidJson<YouTubeTrackLinkGenerateRequest>,
) -> Reply<Json<YouTubeTrackLinkResponse>> {
    let track_link = links
        .generate_track_link(body)
        .await
        .map_err(ApiError::from)?;
    let quota = links.quota().await;
    Ok(Json(YouTubeTrackLinkResponse { track_link, quota }))
}

/// Find and save videos for many tracks. Tracks YouTube has nothing for,
/// or that could not be searched, come back under `failed`.
#[utoipa::path(
    post,
    tag = "youtube",
    path = "/api/v3/youtube/generate-tracks",
    request_body = YouTubeTrackLinkBatchGenerateRequest,
    responses(
        (status = 200, description = "Saved track links, failures and today's budget", body = YouTubeTrackLinkBatchResponse),
        (status = 400, description = "Bad request body or too many tracks"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn generate_track_links(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    ValidJson(body): ValidJson<YouTubeTrackLinkBatchGenerateRequest>,
) -> Reply<Json<YouTubeTrackLinkBatchResponse>> {
    let (track_links, failed) = links
        .generate_track_links(body)
        .await
        .map_err(ApiError::from)?;
    let quota = links.quota().await;
    Ok(Json(YouTubeTrackLinkBatchResponse {
        track_links,
        failed,
        quota,
    }))
}

/// One album's track links, in disc then track order.
#[utoipa::path(
    get,
    tag = "youtube",
    path = "/api/v3/youtube/track-links/{album_id}",
    params(("album_id" = String, Path, description = "Album id")),
    responses(
        (status = 200, description = "Saved track links", body = Vec<YouTubeTrackLink>),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_track_links(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    Path(album_id): Path<String>,
) -> Reply<Json<Vec<YouTubeTrackLink>>> {
    links
        .track_links(&album_id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Remove one track link.
#[utoipa::path(
    delete,
    tag = "youtube",
    path = "/api/v3/youtube/track-link/{album_id}/{disc_number}/{track_number}",
    params(
        ("album_id" = String, Path, description = "Album id"),
        ("disc_number" = i64, Path, description = "Disc number"),
        ("track_number" = i64, Path, description = "Track position on its disc"),
    ),
    responses(
        (status = 204, description = "Removed, or nothing was saved"),
        (status = 400, description = "Disc or track is not a number"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn delete_track_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    path: Result<Path<(String, i64, i64)>, PathRejection>,
) -> Reply<StatusCode> {
    let Path((album_id, disc_number, track_number)) =
        path.map_err(|cause| ApiError::invalid_input(format!("Invalid path: {cause}")))?;
    links
        .delete_track_link(&album_id, disc_number, track_number)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Today's YouTube search budget. A zero budget while search is off.
#[utoipa::path(
    get,
    tag = "youtube",
    path = "/api/v3/youtube/quota",
    responses(
        (status = 200, description = "Today's budget", body = YouTubeQuotaStatus),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_quota(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
) -> Json<YouTubeQuotaStatus> {
    Json(links.quota().await)
}

impl From<LinkError> for ApiError {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::NotFound(message) => {
                ApiError::new(StatusCode::NOT_FOUND, NOT_FOUND, message)
            }
            LinkError::Invalid(message) => ApiError::invalid_input(message),
            LinkError::NotConfigured(message) => {
                ApiError::new(StatusCode::CONFLICT, NOT_CONFIGURED, message)
            }
            LinkError::Exhausted(message) => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED, message)
            }
            LinkError::Upstream(cause) => ApiError::fault(StatusCode::BAD_GATEWAY, cause),
            LinkError::Store(StoreError::Busy(operation)) => {
                ApiError::unavailable(Unavailable::ServerBusy, operation)
            }
            LinkError::Store(error) => ApiError::internal(error),
        }
    }
}
