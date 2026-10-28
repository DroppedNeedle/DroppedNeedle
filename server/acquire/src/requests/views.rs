//! View and approval handlers: active, history, wanted, the request and
//! personal-mix approval queues, and the personal-mix refresh route. The
//! auto-download approvals are the follows' routes.
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
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidQuery;

use super::error::RequestsError;
use super::http::{decided_reply, mix_reply};
use super::models::{
    ActionResponse, ActiveCountResponse, ActiveRequestsResponse, HistoryQuery, HistoryResponse,
    KindQuery, PersonalMixApprovalsResponse, RefreshResponse, RequestKind, WantedActionResponse,
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
    tag = "requests",
    path = "/api/v3/requests/active",
    responses((status = 200, body = ActiveRequestsResponse))
)]
pub async fn active_handler(
    State(state): State<RequestsState>,
    User(principal): User,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(RequestsService::new(&state).active(&principal).await?))
}

/// Live-ask count. `GET /api/v3/requests/active/count`.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/requests/active/count",
    responses((status = 200, body = ActiveCountResponse))
)]
pub async fn active_count_handler(
    State(state): State<RequestsState>,
    User(principal): User,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        RequestsService::new(&state)
            .active_count(&principal)
            .await?,
    ))
}

/// Paged history. `GET /api/v3/requests/history`. Admins see every row.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/requests/history",
    params(HistoryQuery),
    responses((status = 200, body = HistoryResponse))
)]
pub async fn history_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        RequestsService::new(&state)
            .history(&principal, &query)
            .await?,
    ))
}

/// Rows waiting for review. `GET /api/v3/requests/approvals`. Admin only.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/requests/approvals",
    responses((status = 200, body = ActiveRequestsResponse))
)]
pub async fn approvals_handler(
    State(state): State<RequestsState>,
    admin: Admin,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        RequestsService::new(&state)
            .pending_approvals(&admin)
            .await?,
    ))
}

/// Pending-review badge count across all three queues.
/// `GET /api/v3/requests/approvals/count`. Admin only.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/requests/approvals/count",
    responses((status = 200, body = ActiveCountResponse))
)]
pub async fn approvals_count_handler(
    State(state): State<RequestsState>,
    admin: Admin,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        RequestsService::new(&state)
            .pending_approval_count(&admin)
            .await?,
    ))
}

/// Approve one waiting ask and dispatch it.
/// `POST /api/v3/requests/approvals/{musicbrainz_id}/approve`. Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/approvals/{musicbrainz_id}/approve",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID"), KindQuery),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_handler(
    State(state): State<RequestsState>,
    admin: Admin,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = resolve_kind(&query)?;
    Ok(Json(decided_reply(
        RequestsService::new(&state)
            .approve_request(&admin, &musicbrainz_id, kind)
            .await?,
    )))
}

/// Reject one waiting ask.
/// `POST /api/v3/requests/approvals/{musicbrainz_id}/reject`. Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/approvals/{musicbrainz_id}/reject",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID"), KindQuery),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_handler(
    State(state): State<RequestsState>,
    admin: Admin,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = resolve_kind(&query)?;
    Ok(Json(decided_reply(
        RequestsService::new(&state)
            .reject_request(&admin, &musicbrainz_id, kind)
            .await?,
    )))
}

/// Wanted watches plus the still-retrying set.
/// `GET /api/v3/requests/wanted`. Admins see every row.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/requests/wanted",
    responses((status = 200, body = WantedResponse))
)]
pub async fn wanted_handler(
    State(state): State<RequestsState>,
    User(principal): User,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(RequestsService::new(&state).wanted(&principal).await?))
}

/// Pause one wanted watch. `POST /api/v3/requests/wanted/{musicbrainz_id}/stop`.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/wanted/{musicbrainz_id}/stop",
    params(("musicbrainz_id" = String, Path, description = "Release-group MBID")),
    responses((status = 200, body = WantedActionResponse))
)]
pub async fn wanted_stop_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    Path(musicbrainz_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
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
    tag = "requests",
    path = "/api/v3/requests/wanted/{musicbrainz_id}/resume",
    params(("musicbrainz_id" = String, Path, description = "Release-group MBID")),
    responses((status = 200, body = WantedActionResponse))
)]
pub async fn wanted_resume_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    Path(musicbrainz_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
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
    tag = "requests",
    path = "/api/v3/requests/wanted/{musicbrainz_id}/seen",
    params(("musicbrainz_id" = String, Path, description = "Release-group MBID")),
    responses((status = 200, body = WantedActionResponse))
)]
pub async fn wanted_seen_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    Path(musicbrainz_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        RequestsService::new(&state)
            .wanted_seen(&principal, &musicbrainz_id)
            .await?,
    ))
}

/// Pending personal-mix approvals.
/// `GET /api/v3/requests/personal-mix-approvals`. Admin only.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/requests/personal-mix-approvals",
    responses((status = 200, body = PersonalMixApprovalsResponse))
)]
pub async fn mix_approvals_handler(
    State(state): State<RequestsState>,
    admin: Admin,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        RequestsService::new(&state).mix_approvals(&admin).await?,
    ))
}

/// Approve one mix auto-request grant.
/// `POST /api/v3/requests/personal-mix-approvals/{user_id}/approve`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/personal-mix-approvals/{user_id}/approve",
    params(("user_id" = String, Path, description = "Requesting user id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn approve_mix_handler(
    State(state): State<RequestsState>,
    admin: Admin,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(mix_reply(
        RequestsService::new(&state)
            .approve_mix(&admin, &user_id)
            .await?,
    )))
}

/// Reject one mix auto-request ask.
/// `POST /api/v3/requests/personal-mix-approvals/{user_id}/reject`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/personal-mix-approvals/{user_id}/reject",
    params(("user_id" = String, Path, description = "Requesting user id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn reject_mix_handler(
    State(state): State<RequestsState>,
    admin: Admin,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(mix_reply(
        RequestsService::new(&state)
            .reject_mix(&admin, &user_id)
            .await?,
    )))
}

/// Revoke one mix auto-request grant.
/// `POST /api/v3/requests/personal-mix-approvals/{user_id}/revoke`.
/// Admin only.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/personal-mix-approvals/{user_id}/revoke",
    params(("user_id" = String, Path, description = "Requesting user id")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn revoke_mix_handler(
    State(state): State<RequestsState>,
    admin: Admin,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(mix_reply(
        RequestsService::new(&state)
            .revoke_mix(&admin, &user_id)
            .await?,
    )))
}

/// Refresh one user's personal mix.
/// `POST /api/v3/requests/personal-mix/refresh`. Repeat calls while a build
/// runs answer `already_running`; unlinked users get 400, and a server
/// without the mix builder 409.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/personal-mix/refresh",
    responses(
        (status = 200, body = RefreshResponse),
        (status = 400, description = "ListenBrainz is not connected"),
        (status = 409, description = "Weekly Mix is not available on this server"),
    )
)]
pub async fn refresh_mix_handler(
    State(state): State<RequestsState>,
    User(principal): User,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        RequestsService::new(&state)
            .refresh_personal_mix(&principal)
            .await?,
    ))
}
