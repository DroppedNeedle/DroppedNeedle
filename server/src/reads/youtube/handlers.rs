//! Thin handlers for the YouTube link routes. Every route is a plain
//! signed-in-user route: links are shared by everyone on the server, as in
//! v2. Status mapping lives in [`render`]; bodies and paths that do not
//! parse answer the shared 400 envelope.

use axum::{
    Json, Router,
    extract::{
        Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};

use crate::error::{
    FIXED_INTERNAL_MESSAGE, FIXED_UPSTREAM_MESSAGE, INTERNAL_ERROR, INVALID_INPUT, NOT_FOUND,
    UPSTREAM_ERROR, busy_response, envelope_response, fault_response,
};
use crate::reads::collections::db::StoreError;
use crate::reads::discover::error::{NOT_CONFIGURED, RATE_LIMITED};
use crate::reads::discover::handlers::AuthenticatedUser;

use super::models::{
    YouTubeLink, YouTubeLinkGenerateRequest, YouTubeLinkResponse, YouTubeLinkUpdateRequest,
    YouTubeManualLinkRequest, YouTubeQuotaStatus, YouTubeTrackLink,
    YouTubeTrackLinkBatchGenerateRequest, YouTubeTrackLinkBatchResponse,
    YouTubeTrackLinkGenerateRequest, YouTubeTrackLinkResponse,
};
use super::service::{LinkError, YouTubeLinks};

type Reply<T> = Result<T, Response>;

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
    path = "/api/v3/youtube/generate",
    request_body = YouTubeLinkGenerateRequest,
    responses(
        (status = 200, description = "Saved link and today's budget", body = YouTubeLinkResponse),
        (status = 400, description = "Bad request body", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "YouTube had no video for the album", body = crate::error::ErrorEnvelope),
        (status = 429, description = "Today's search budget is spent (RATE_LIMITED)", body = crate::error::ErrorEnvelope),
        (status = 502, description = "YouTube failed", body = crate::error::ErrorEnvelope),
        (status = 503, description = "YouTube search is not set up (NOT_CONFIGURED)", body = crate::error::ErrorEnvelope),
    )
)]
pub async fn generate_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    body: Result<Json<YouTubeLinkGenerateRequest>, JsonRejection>,
) -> Reply<Json<YouTubeLinkResponse>> {
    let link = links
        .generate_link(body.map_err(bad_body)?.0)
        .await
        .map_err(|error| render(error, &links))?;
    let quota = links.quota().await;
    Ok(Json(YouTubeLinkResponse { link, quota }))
}

/// One album's saved link. 204 when there is none.
#[utoipa::path(
    get,
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
    let link = links
        .link(&album_id)
        .await
        .map_err(|error| render(error, &links))?;
    Ok(match link {
        Some(link) => Json(link).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

/// Every saved album link, newest first.
#[utoipa::path(
    get,
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
    links
        .links()
        .await
        .map(Json)
        .map_err(|error| render(error, &links))
}

/// Remove an album link and its track links.
#[utoipa::path(
    delete,
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
        .map_err(|error| render(error, &links))
}

/// Edit a saved link: video, names or cover.
#[utoipa::path(
    put,
    path = "/api/v3/youtube/link/{album_id}",
    params(("album_id" = String, Path, description = "Album id")),
    request_body = YouTubeLinkUpdateRequest,
    responses(
        (status = 200, description = "Edited link", body = YouTubeLink),
        (status = 400, description = "Bad body or not a YouTube URL", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No link saved for the album", body = crate::error::ErrorEnvelope),
    )
)]
pub async fn update_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    Path(album_id): Path<String>,
    body: Result<Json<YouTubeLinkUpdateRequest>, JsonRejection>,
) -> Reply<Json<YouTubeLink>> {
    links
        .update_link(&album_id, body.map_err(bad_body)?.0)
        .await
        .map(Json)
        .map_err(|error| render(error, &links))
}

/// Save a video a person pasted, for a known album or a made-up one.
#[utoipa::path(
    post,
    path = "/api/v3/youtube/manual",
    request_body = YouTubeManualLinkRequest,
    responses(
        (status = 200, description = "Saved link", body = YouTubeLink),
        (status = 400, description = "Bad body or not a YouTube URL", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn save_manual_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    body: Result<Json<YouTubeManualLinkRequest>, JsonRejection>,
) -> Reply<Json<YouTubeLink>> {
    links
        .save_manual_link(body.map_err(bad_body)?.0)
        .await
        .map(Json)
        .map_err(|error| render(error, &links))
}

/// Find and save one track's video. A saved one answers without a search.
#[utoipa::path(
    post,
    path = "/api/v3/youtube/generate-track",
    request_body = YouTubeTrackLinkGenerateRequest,
    responses(
        (status = 200, description = "Saved track link and today's budget", body = YouTubeTrackLinkResponse),
        (status = 400, description = "Bad request body", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "YouTube had no video for the track", body = crate::error::ErrorEnvelope),
        (status = 429, description = "Today's search budget is spent (RATE_LIMITED)", body = crate::error::ErrorEnvelope),
        (status = 502, description = "YouTube failed", body = crate::error::ErrorEnvelope),
        (status = 503, description = "YouTube search is not set up (NOT_CONFIGURED)", body = crate::error::ErrorEnvelope),
    )
)]
pub async fn generate_track_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    body: Result<Json<YouTubeTrackLinkGenerateRequest>, JsonRejection>,
) -> Reply<Json<YouTubeTrackLinkResponse>> {
    let track_link = links
        .generate_track_link(body.map_err(bad_body)?.0)
        .await
        .map_err(|error| render(error, &links))?;
    let quota = links.quota().await;
    Ok(Json(YouTubeTrackLinkResponse { track_link, quota }))
}

/// Find and save videos for many tracks. Tracks YouTube has nothing for,
/// or that could not be searched, come back under `failed`.
#[utoipa::path(
    post,
    path = "/api/v3/youtube/generate-tracks",
    request_body = YouTubeTrackLinkBatchGenerateRequest,
    responses(
        (status = 200, description = "Saved track links, failures and today's budget", body = YouTubeTrackLinkBatchResponse),
        (status = 400, description = "Bad request body or too many tracks", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn generate_track_links(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    body: Result<Json<YouTubeTrackLinkBatchGenerateRequest>, JsonRejection>,
) -> Reply<Json<YouTubeTrackLinkBatchResponse>> {
    let (track_links, failed) = links
        .generate_track_links(body.map_err(bad_body)?.0)
        .await
        .map_err(|error| render(error, &links))?;
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
        .map_err(|error| render(error, &links))
}

/// Remove one track link.
#[utoipa::path(
    delete,
    path = "/api/v3/youtube/track-link/{album_id}/{disc_number}/{track_number}",
    params(
        ("album_id" = String, Path, description = "Album id"),
        ("disc_number" = i64, Path, description = "Disc number"),
        ("track_number" = i64, Path, description = "Track position on its disc"),
    ),
    responses(
        (status = 204, description = "Removed, or nothing was saved"),
        (status = 400, description = "Disc or track is not a number", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn delete_track_link(
    State(links): State<YouTubeLinks>,
    _user: AuthenticatedUser,
    path: Result<Path<(String, i64, i64)>, PathRejection>,
) -> Reply<StatusCode> {
    let Path((album_id, disc_number, track_number)) = path.map_err(|cause| {
        envelope_response(
            StatusCode::BAD_REQUEST,
            INVALID_INPUT,
            format!("Invalid path: {cause}"),
            None,
        )
    })?;
    links
        .delete_track_link(&album_id, disc_number, track_number)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| render(error, &links))
}

/// Today's YouTube search budget. A zero budget while search is off.
#[utoipa::path(
    get,
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

/// The shared 400 envelope for a JSON body that does not parse.
fn bad_body(cause: JsonRejection) -> Response {
    envelope_response(
        StatusCode::BAD_REQUEST,
        INVALID_INPUT,
        format!("Invalid request body: {}", cause.body_text()),
        None,
    )
}

/// Map a service failure to its status and envelope. Causes of 5xx answers
/// go to the log under a fresh error id; only the id reaches the wire.
fn render(error: LinkError, links: &YouTubeLinks) -> Response {
    match error {
        LinkError::NotFound(message) => {
            envelope_response(StatusCode::NOT_FOUND, NOT_FOUND, message, None)
        }
        LinkError::Invalid(message) => {
            envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
        }
        LinkError::NotConfigured(message) => envelope_response(
            StatusCode::SERVICE_UNAVAILABLE,
            NOT_CONFIGURED,
            message,
            None,
        ),
        LinkError::Exhausted(message) => {
            envelope_response(StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED, message, None)
        }
        LinkError::Upstream(cause) => {
            let error_id = links.ids().new_id();
            tracing::error!(error_id, %cause, "youtube link request failed upstream");
            fault_response(
                StatusCode::BAD_GATEWAY,
                UPSTREAM_ERROR,
                FIXED_UPSTREAM_MESSAGE,
                &error_id,
            )
        }
        LinkError::Store(StoreError::Busy(operation)) => {
            busy_response(&operation, &links.ids().new_id())
        }
        LinkError::Store(error) => {
            let error_id = links.ids().new_id();
            tracing::error!(error_id, %error, "youtube link request failed");
            fault_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR,
                FIXED_INTERNAL_MESSAGE,
                &error_id,
            )
        }
    }
}
