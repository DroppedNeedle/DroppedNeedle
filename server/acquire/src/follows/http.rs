//! Follow and auto-download approval routes.
//!
//! Following and the auto-download toggle take any signed-in user; the
//! approval list, verdicts and batches take an admin. The follow status a
//! write answers is read back after the write commits.

use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_auth::{Admin, User};
use droppedneedle_catalog::reads::collections::models::FollowStatusResponse;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};

use super::Follows;
use super::models::{
    ApprovalBatchListResponse, AutoDownloadApprovalListResponse, AutoDownloadBody, FollowBody,
};
use crate::requests::ledger::ApprovalState;
use crate::requests::models::ActionResponse;

/// The follow routes and the auto-download approval queue, at their full
/// `/api/v3` paths.
pub fn routes() -> Routes<Follows> {
    Routes::new()
        .user(routes!(set_follow_handler))
        .user(routes!(set_auto_download_handler))
        .admin(routes!(list_approvals_handler))
        .admin(routes!(list_approval_batches_handler))
        .admin(routes!(approve_auto_download_handler))
        .admin(routes!(reject_auto_download_handler))
        .admin(routes!(revoke_auto_download_handler))
        .admin(routes!(approve_auto_download_batch_handler))
        .admin(routes!(reject_auto_download_batch_handler))
}

/// Follow or unfollow one artist.
#[utoipa::path(
    put,
    path = "/api/v3/artists/{artist_mbid}/follow",
    tag = "collections",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    request_body = FollowBody,
    responses(
        (status = 200, description = "Follow status", body = FollowStatusResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn set_follow_handler(
    State(follows): State<Follows>,
    User(caller): User,
    Path(artist_mbid): Path<String>,
    ValidJson(body): ValidJson<FollowBody>,
) -> Result<Json<FollowStatusResponse>, ApiError> {
    Ok(Json(
        follows
            .set_follow(
                caller.user_id(),
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
    tag = "collections",
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
    State(follows): State<Follows>,
    User(caller): User,
    Path(artist_mbid): Path<String>,
    ValidJson(body): ValidJson<AutoDownloadBody>,
) -> Result<Json<FollowStatusResponse>, ApiError> {
    Ok(Json(
        follows
            .set_auto_download(&caller, &artist_mbid, body.enabled)
            .await?,
    ))
}

/// List pending auto-download requests.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approvals",
    tag = "collections",
    responses(
        (status = 200, description = "Pending approvals", body = AutoDownloadApprovalListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_approvals_handler(
    State(follows): State<Follows>,
    _admin: Admin,
) -> Result<Json<AutoDownloadApprovalListResponse>, ApiError> {
    Ok(Json(follows.approval_list().await?))
}

/// List pending requests grouped per user.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approval-batches",
    tag = "collections",
    responses(
        (status = 200, description = "Approval batches", body = ApprovalBatchListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_approval_batches_handler(
    State(follows): State<Follows>,
    _admin: Admin,
) -> Result<Json<ApprovalBatchListResponse>, ApiError> {
    Ok(Json(follows.batch_list().await?))
}

/// Approve one auto-download grant.
/// `POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve",
    params(
        ("user_id" = String, Path, description = "Requesting user id"),
        ("artist_mbid" = String, Path, description = "Artist MBID"),
    ),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_auto_download_handler(
    State(follows): State<Follows>,
    admin: Admin,
    Path((user_id, artist_mbid)): Path<(String, String)>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(
        follows
            .decide(&admin, &user_id, &artist_mbid, ApprovalState::Approved)
            .await?,
    ))
}

/// Reject one auto-download ask, keeping the follow.
/// `POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject",
    params(
        ("user_id" = String, Path, description = "Requesting user id"),
        ("artist_mbid" = String, Path, description = "Artist MBID"),
    ),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_auto_download_handler(
    State(follows): State<Follows>,
    admin: Admin,
    Path((user_id, artist_mbid)): Path<(String, String)>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(
        follows
            .decide(&admin, &user_id, &artist_mbid, ApprovalState::Rejected)
            .await?,
    ))
}

/// Revoke one auto-download grant, keeping the follow.
/// `POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke",
    params(
        ("user_id" = String, Path, description = "Requesting user id"),
        ("artist_mbid" = String, Path, description = "Artist MBID"),
    ),
    responses((status = 200, body = ActionResponse))
)]
pub async fn revoke_auto_download_handler(
    State(follows): State<Follows>,
    admin: Admin,
    Path((user_id, artist_mbid)): Path<(String, String)>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(
        follows
            .decide(&admin, &user_id, &artist_mbid, ApprovalState::Revoked)
            .await?,
    ))
}

/// Approve one bulk batch.
/// `POST /api/v3/requests/auto-download-approval-batches/{batch_id}/approve`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/auto-download-approval-batches/{batch_id}/approve",
    params(("batch_id" = String, Path, description = "Batch id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_auto_download_batch_handler(
    State(follows): State<Follows>,
    admin: Admin,
    Path(batch_id): Path<String>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(
        follows
            .decide_batch(&admin, &batch_id, ApprovalState::Approved)
            .await?,
    ))
}

/// Reject one bulk batch.
/// `POST /api/v3/requests/auto-download-approval-batches/{batch_id}/reject`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/auto-download-approval-batches/{batch_id}/reject",
    params(("batch_id" = String, Path, description = "Batch id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_auto_download_batch_handler(
    State(follows): State<Follows>,
    admin: Admin,
    Path(batch_id): Path<String>,
) -> Result<Json<ActionResponse>, ApiError> {
    Ok(Json(
        follows
            .decide_batch(&admin, &batch_id, ApprovalState::Rejected)
            .await?,
    ))
}
