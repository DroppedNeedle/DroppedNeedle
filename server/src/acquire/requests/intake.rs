//! Intake mutation handlers: album, track, batch, edition-acquire, and the
//! per-request cancel/retry/clear verbs plus batch-cancel.
//!
//! Route posture: every route needs an authenticated principal; edition
//! acquire needs a curator; approve/reject live in `views` behind admin.
//! Handler docs name the full `/api/v3` path for the contract document.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};

use super::auth::Principal;
use super::error::RequestsError;
use super::http::{HttpError, ValidJson, ValidQuery};
use super::models::{
    ActionResponse, AlbumIntake, BatchCancelBody, BatchCancelResponse, BatchIntake,
    BatchIntakeResponse, ClearHistoryResponse, EditionAcquireResponse, IntakeResponse, KindQuery,
    RequestKind, TrackIntake, TrackIntakeResponse,
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

/// Ask for one album. `POST /api/v3/requests/albums`. Users wait for
/// approval; curators dispatch at once. Duplicate asks attach to the live
/// row and report its status. Answers 202.
#[utoipa::path(
    post,
    path = "/api/v3/requests/albums",
    request_body = AlbumIntake,
    responses((status = 202, body = IntakeResponse))
)]
pub async fn request_album_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    ValidJson(body): ValidJson<AlbumIntake>,
) -> Result<impl IntoResponse, HttpError> {
    let response = RequestsService::new(&state)
        .request_album(&principal, &body)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Ask for one exact recording. `POST /api/v3/requests/tracks`. Same
/// approval gate as albums. Answers 202.
#[utoipa::path(
    post,
    path = "/api/v3/requests/tracks",
    request_body = TrackIntake,
    responses((status = 202, body = TrackIntakeResponse))
)]
pub async fn request_track_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    ValidJson(body): ValidJson<TrackIntake>,
) -> Result<impl IntoResponse, HttpError> {
    let response = RequestsService::new(&state)
        .request_track(&principal, &body)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Ask for a batch of albums. `POST /api/v3/requests/batches`. At most 500
/// rows; dupes, live rows, and unresolvable rows skip. Answers 202.
#[utoipa::path(
    post,
    path = "/api/v3/requests/batches",
    request_body = BatchIntake,
    responses((status = 202, body = BatchIntakeResponse))
)]
pub async fn request_batch_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    ValidJson(body): ValidJson<BatchIntake>,
) -> Result<impl IntoResponse, HttpError> {
    let response = RequestsService::new(&state)
        .request_batch(&principal, &body)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Cancel a batch of asks. `POST /api/v3/requests/batches/cancel`.
/// Non-admins detach from shared rows and cancel their own; admins cancel
/// anything live.
#[utoipa::path(
    post,
    path = "/api/v3/requests/batches/cancel",
    request_body = BatchCancelBody,
    responses((status = 200, body = BatchCancelResponse))
)]
pub async fn cancel_batch_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    ValidJson(body): ValidJson<BatchCancelBody>,
) -> Result<impl IntoResponse, HttpError> {
    let kind = match body.kind.as_deref() {
        None => RequestKind::Album,
        Some(value) => RequestKind::parse(value).ok_or_else(|| RequestsError::InvalidInput {
            message: "kind must be album or track".to_owned(),
        })?,
    };
    let response = RequestsService::new(&state)
        .cancel_batch(&principal, &body.musicbrainz_ids, kind)
        .await?;
    Ok(Json(response))
}

/// Cancel one ask. `DELETE /api/v3/requests/active/{musicbrainz_id}`.
/// Co-requesters detach with the shared row continuing; owners cancel.
#[utoipa::path(
    delete,
    path = "/api/v3/requests/active/{musicbrainz_id}",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn cancel_one_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, HttpError> {
    let kind = resolve_kind(&query)?;
    let response = RequestsService::new(&state)
        .cancel_one(&principal, &musicbrainz_id, kind)
        .await?;
    Ok(Json(response))
}

/// Retry one terminal ask. `POST /api/v3/requests/retry/{musicbrainz_id}`.
/// Plain users rejoin the queue unless the generation already carries
/// approval provenance.
#[utoipa::path(
    post,
    path = "/api/v3/requests/retry/{musicbrainz_id}",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID")),
    responses((status = 200, body = ActionResponse))
)]
pub async fn retry_one_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, HttpError> {
    let kind = resolve_kind(&query)?;
    let response = RequestsService::new(&state)
        .retry_request(&principal, &musicbrainz_id, kind)
        .await?;
    Ok(Json(response))
}

/// Clear one history row. `DELETE /api/v3/requests/history/{musicbrainz_id}`.
/// Admins delete the row; users dismiss it from their own view.
#[utoipa::path(
    delete,
    path = "/api/v3/requests/history/{musicbrainz_id}",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID")),
    responses((status = 200, body = ClearHistoryResponse))
)]
pub async fn clear_history_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, HttpError> {
    let kind = resolve_kind(&query)?;
    let response = RequestsService::new(&state)
        .clear_history_item(&principal, &musicbrainz_id, kind)
        .await?;
    Ok(Json(response))
}

/// Fill the selected edition's missing tracks and upgrade its below-cutoff
/// owned tracks. `POST /api/v3/albums/{album_id}/edition/acquire`.
/// Curator only. Never retags existing files.
#[utoipa::path(
    post,
    path = "/api/v3/albums/{album_id}/edition/acquire",
    params(("album_id" = String, Path, description = "Release-group MBID")),
    responses((status = 200, body = EditionAcquireResponse))
)]
pub async fn acquire_edition_handler(
    State(state): State<RequestsState>,
    principal: Principal,
    Path(album_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    let response = RequestsService::new(&state)
        .acquire_edition(&principal, &album_id)
        .await?;
    Ok(Json(response))
}
