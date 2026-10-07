//! View and approval handlers: active, history, wanted, the three approval
//! queues, the personal-mix refresh route, and the status-sync sweep.
//!
//! Route posture: reads need any authenticated principal (admins see every
//! row, anyone else sees their own); every approval verb needs an admin;
//! refresh needs any authenticated principal. Handler docs name the full
//! `/api/v3` path for the contract document.

use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};

use super::auth::Principal;
use super::error::RequestsError;
use super::http::{HttpError, ValidQuery};
use super::models::{
    ActionResponse, ActiveCountResponse, ActiveRequestsResponse, ApprovalBatchListResponse,
    AutoDownloadApprovalsResponse, HistoryQuery, HistoryResponse, KindQuery,
    PersonalMixApprovalsResponse, RefreshResponse, RequestKind, SyncResponse, WantedActionResponse,
    WantedResponse,
};
use super::service::RequestsService;
use super::state::RequestsState;

/// Resolve the `kind` query to a request kind. Unknown values are 400.
fn resolve_kind(query: &KindQuery) -> Result<RequestKind, RequestsError> {
    match query.kind.as_deref() {
        None => Ok(RequestKind::Album),
        Some(value) => RequestKind::parse(value).ok_or_else(|| RequestsError::InvalidInput {
            message: "kind must be album or track".to_owned(),
        }),
    }
}

/// Live asks. `GET /api/v3/requests/active`. Admins see every row.
#[utoipa::path(
    get,
    path = "/api/v3/requests/active",
    responses((status = 200, body = ActiveRequestsResponse))
)]
pub async fn active_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(RequestsService::new(&state).active(&principal).await?))
}

/// Live-ask count. `GET /api/v3/requests/active/count`.
#[utoipa::path(
    get,
    path = "/api/v3/requests/active/count",
    responses((status = 200, body = ActiveCountResponse))
)]
pub async fn active_count_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .active_count(&principal)
            .await?,
    ))
}

/// Paged history. `GET /api/v3/requests/history`. Admins see every row.
#[utoipa::path(
    get,
    path = "/api/v3/requests/history",
    responses((status = 200, body = HistoryResponse))
)]
pub async fn history_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .history(&principal, &query)
            .await?,
    ))
}

/// Rows waiting for review. `GET /api/v3/requests/approvals`. Admin only.
#[utoipa::path(
    get,
    path = "/api/v3/requests/approvals",
    responses((status = 200, body = ActiveRequestsResponse))
)]
pub async fn approvals_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .pending_approvals(&principal)
            .await?,
    ))
}

/// Pending-review badge count across all three queues.
/// `GET /api/v3/requests/approvals/count`. Admin only.
#[utoipa::path(
    get,
    path = "/api/v3/requests/approvals/count",
    responses((status = 200, body = ActiveCountResponse))
)]
pub async fn approvals_count_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .pending_approval_count(&principal)
            .await?,
    ))
}

/// Approve one waiting ask and dispatch it.
/// `POST /api/v3/requests/approvals/{musicbrainz_id}/approve`. Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/approvals/{musicbrainz_id}/approve",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, HttpError> {
    let kind = resolve_kind(&query)?;
    Ok(Json(
        RequestsService::new(&state)
            .approve_request(&principal, &musicbrainz_id, kind)
            .await?,
    ))
}

/// Reject one waiting ask.
/// `POST /api/v3/requests/approvals/{musicbrainz_id}/reject`. Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/approvals/{musicbrainz_id}/reject",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, HttpError> {
    let kind = resolve_kind(&query)?;
    Ok(Json(
        RequestsService::new(&state)
            .reject_request(&principal, &musicbrainz_id, kind)
            .await?,
    ))
}

/// Wanted watches plus the still-retrying set.
/// `GET /api/v3/requests/wanted`. Admins see every row.
#[utoipa::path(
    get,
    path = "/api/v3/requests/wanted",
    responses((status = 200, body = WantedResponse))
)]
pub async fn wanted_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(RequestsService::new(&state).wanted(&principal).await?))
}

/// Pause one wanted watch. `POST /api/v3/requests/wanted/{musicbrainz_id}/stop`.
#[utoipa::path(
    post,
    path = "/api/v3/requests/wanted/{musicbrainz_id}/stop",
    params(("musicbrainz_id" = String, Path, description = "Release-group MBID")),
    responses((status = 200, body = WantedActionResponse))
)]
pub async fn wanted_stop_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .wanted_stop(&principal, &musicbrainz_id)
            .await?,
    ))
}

/// Resume one wanted watch.
/// `POST /api/v3/requests/wanted/{musicbrainz_id}/resume`.
#[utoipa::path(
    post,
    path = "/api/v3/requests/wanted/{musicbrainz_id}/resume",
    params(("musicbrainz_id" = String, Path, description = "Release-group MBID")),
    responses((status = 200, body = WantedActionResponse))
)]
pub async fn wanted_resume_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .wanted_resume(&principal, &musicbrainz_id)
            .await?,
    ))
}

/// Clear one watch's unseen candidates.
/// `POST /api/v3/requests/wanted/{musicbrainz_id}/seen`.
#[utoipa::path(
    post,
    path = "/api/v3/requests/wanted/{musicbrainz_id}/seen",
    params(("musicbrainz_id" = String, Path, description = "Release-group MBID")),
    responses((status = 200, body = WantedActionResponse))
)]
pub async fn wanted_seen_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .wanted_seen(&principal, &musicbrainz_id)
            .await?,
    ))
}

/// Pending auto-download approvals.
/// `GET /api/v3/requests/auto-download-approvals`. Admin only.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approvals",
    responses((status = 200, body = AutoDownloadApprovalsResponse))
)]
pub async fn auto_download_approvals_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .auto_download_approvals(&principal)
            .await?,
    ))
}

/// Approve one auto-download grant.
/// `POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve",
    params(
        ("user_id" = String, Path, description = "Requesting user id"),
        ("artist_mbid" = String, Path, description = "Artist MBID"),
    ),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_auto_download_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path((user_id, artist_mbid)): Path<(String, String)>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .approve_auto_download(&principal, &user_id, &artist_mbid)
            .await?,
    ))
}

/// Reject one auto-download ask, keeping the follow.
/// `POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject",
    params(
        ("user_id" = String, Path, description = "Requesting user id"),
        ("artist_mbid" = String, Path, description = "Artist MBID"),
    ),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_auto_download_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path((user_id, artist_mbid)): Path<(String, String)>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .reject_auto_download(&principal, &user_id, &artist_mbid)
            .await?,
    ))
}

/// Revoke one auto-download grant, keeping the follow.
/// `POST /api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke",
    params(
        ("user_id" = String, Path, description = "Requesting user id"),
        ("artist_mbid" = String, Path, description = "Artist MBID"),
    ),
    responses((status = 200, body = ActionResponse))
)]
pub async fn revoke_auto_download_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path((user_id, artist_mbid)): Path<(String, String)>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .revoke_auto_download(&principal, &user_id, &artist_mbid)
            .await?,
    ))
}

/// Pending bulk-approval batches.
/// `GET /api/v3/requests/auto-download-approval-batches`. Admin only.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approval-batches",
    responses((status = 200, body = ApprovalBatchListResponse))
)]
pub async fn auto_download_batches_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .auto_download_batches(&principal)
            .await?,
    ))
}

/// Approve one bulk batch.
/// `POST /api/v3/requests/auto-download-approval-batches/{batch_id}/approve`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/auto-download-approval-batches/{batch_id}/approve",
    params(("batch_id" = String, Path, description = "Batch id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_auto_download_batch_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(batch_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .approve_auto_download_batch(&principal, &batch_id)
            .await?,
    ))
}

/// Reject one bulk batch.
/// `POST /api/v3/requests/auto-download-approval-batches/{batch_id}/reject`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/auto-download-approval-batches/{batch_id}/reject",
    params(("batch_id" = String, Path, description = "Batch id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_auto_download_batch_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(batch_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .reject_auto_download_batch(&principal, &batch_id)
            .await?,
    ))
}

/// Pending personal-mix approvals.
/// `GET /api/v3/requests/personal-mix-approvals`. Admin only.
#[utoipa::path(
    get,
    path = "/api/v3/requests/personal-mix-approvals",
    responses((status = 200, body = PersonalMixApprovalsResponse))
)]
pub async fn mix_approvals_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .mix_approvals(&principal)
            .await?,
    ))
}

/// Approve one mix auto-request grant.
/// `POST /api/v3/requests/personal-mix-approvals/{user_id}/approve`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/personal-mix-approvals/{user_id}/approve",
    params(("user_id" = String, Path, description = "Requesting user id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_mix_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .approve_mix(&principal, &user_id)
            .await?,
    ))
}

/// Reject one mix auto-request ask.
/// `POST /api/v3/requests/personal-mix-approvals/{user_id}/reject`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/personal-mix-approvals/{user_id}/reject",
    params(("user_id" = String, Path, description = "Requesting user id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_mix_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .reject_mix(&principal, &user_id)
            .await?,
    ))
}

/// Revoke one mix auto-request grant.
/// `POST /api/v3/requests/personal-mix-approvals/{user_id}/revoke`.
/// Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/requests/personal-mix-approvals/{user_id}/revoke",
    params(("user_id" = String, Path, description = "Requesting user id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn revoke_mix_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .revoke_mix(&principal, &user_id)
            .await?,
    ))
}

/// Refresh one user's personal mix.
/// `POST /api/v3/requests/personal-mix/refresh`. Repeat calls while a build
/// runs answer `already_running`; unlinked users get 400, and a server
/// without the mix builder 409.
#[utoipa::path(
    post,
    path = "/api/v3/requests/personal-mix/refresh",
    responses(
        (status = 200, body = RefreshResponse),
        (status = 400, description = "ListenBrainz is not connected"),
        (status = 409, description = "Weekly Mix is not available on this server"),
    )
)]
pub async fn refresh_mix_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    Ok(Json(
        RequestsService::new(&state)
            .refresh_personal_mix(&principal)
            .await?,
    ))
}

/// Reconcile live rows with their linked tasks.
/// `POST /api/v3/requests/sync`. One bad row never stops the sweep.
#[utoipa::path(
    post,
    path = "/api/v3/requests/sync",
    responses((status = 200, body = SyncResponse))
)]
pub async fn sync_handler(
    State(state): State<RequestsState>,
    principal: Principal,
) -> Result<impl IntoResponse, HttpError> {
    principal.require_admin()?;
    Ok(Json(
        RequestsService::new(&state).sync_request_statuses().await?,
    ))
}
