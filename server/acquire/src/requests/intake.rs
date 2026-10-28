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
use droppedneedle_auth::{Curator, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

use super::error::RequestsError;
use super::http::{batch_cancel_reply, decided_reply};
use super::models::{
    ActionResponse, AlbumIntake, BatchCancelBody, BatchCancelResponse, BatchIntake,
    BatchIntakeResponse, ClearHistoryResponse, EditionAcquireQuery, EditionAcquireResponse,
    EditionFetchResponse, IntakeResponse, KindQuery, RequestKind, TrackIntake, TrackIntakeResponse,
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
    tag = "requests",
    path = "/api/v3/requests/albums",
    request_body = AlbumIntake,
    responses((status = 202, body = IntakeResponse))
)]
pub async fn request_album_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    ValidJson(body): ValidJson<AlbumIntake>,
) -> Result<impl IntoResponse, ApiError> {
    let response = RequestsService::new(&state)
        .request_album(&principal, &body)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Ask for one exact recording. `POST /api/v3/requests/tracks`. Same
/// approval gate as albums. Answers 202.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/tracks",
    request_body = TrackIntake,
    responses((status = 202, body = TrackIntakeResponse))
)]
pub async fn request_track_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    ValidJson(body): ValidJson<TrackIntake>,
) -> Result<impl IntoResponse, ApiError> {
    let response = RequestsService::new(&state)
        .request_track(&principal, &body)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Ask for a batch of albums. `POST /api/v3/requests/batches`. At most 500
/// rows; dupes, live rows, and unresolvable rows skip. Answers 202.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/batches",
    request_body = BatchIntake,
    responses((status = 202, body = BatchIntakeResponse))
)]
pub async fn request_batch_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    ValidJson(body): ValidJson<BatchIntake>,
) -> Result<impl IntoResponse, ApiError> {
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
    tag = "requests",
    path = "/api/v3/requests/batches/cancel",
    request_body = BatchCancelBody,
    responses((status = 200, body = BatchCancelResponse))
)]
pub async fn cancel_batch_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    ValidJson(body): ValidJson<BatchCancelBody>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = match body.kind.as_deref() {
        None => RequestKind::Album,
        Some(value) => RequestKind::parse(value).ok_or_else(|| RequestsError::InvalidInput {
            message: "kind must be album or track".to_owned(),
        })?,
    };
    let response = RequestsService::new(&state)
        .cancel_batch(&principal, &body.musicbrainz_ids, kind)
        .await?;
    Ok(Json(batch_cancel_reply(response)))
}

/// Cancel one ask. `DELETE /api/v3/requests/active/{musicbrainz_id}`.
/// Co-requesters detach with the shared row continuing; owners cancel.
#[utoipa::path(
    delete,
    tag = "requests",
    path = "/api/v3/requests/active/{musicbrainz_id}",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID"), KindQuery),
    responses((status = 200, body = ActionResponse))
)]
pub async fn cancel_one_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = resolve_kind(&query)?;
    let response = RequestsService::new(&state)
        .cancel_one(&principal, &musicbrainz_id, kind)
        .await?;
    Ok(Json(decided_reply(response)))
}

/// Retry one terminal ask. `POST /api/v3/requests/retry/{musicbrainz_id}`.
/// Plain users rejoin the queue unless the generation already carries
/// approval provenance.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/retry/{musicbrainz_id}",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID"), KindQuery),
    responses((status = 200, body = ActionResponse))
)]
pub async fn retry_one_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = resolve_kind(&query)?;
    let response = RequestsService::new(&state)
        .retry_request(&principal, &musicbrainz_id, kind)
        .await?;
    Ok(Json(decided_reply(response)))
}

/// Take a single from its album.
/// `POST /api/v3/requests/tracks/{musicbrainz_id}/from-album`. For a failed
/// track request whose download found only the album's version of the
/// song: it is fetched again, and the album's track imports as the song
/// when it is the right track. A requester or an admin.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/requests/tracks/{musicbrainz_id}/from-album",
    params(("musicbrainz_id" = String, Path, description = "Recording MBID")),
    responses(
        (status = 200, body = ActionResponse),
        (status = 403, description = "Another user's request"),
        (status = 404, description = "No track request for this recording"),
        (status = 409, description = "No album version to take, or the request needs approval"),
    )
)]
pub async fn from_album_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    Path(musicbrainz_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let response = RequestsService::new(&state)
        .take_album_version(&principal, &musicbrainz_id)
        .await?;
    Ok(Json(decided_reply(response)))
}

/// Clear one history row. `DELETE /api/v3/requests/history/{musicbrainz_id}`.
/// Admins delete the row; users dismiss it from their own view.
#[utoipa::path(
    delete,
    tag = "requests",
    path = "/api/v3/requests/history/{musicbrainz_id}",
    params(("musicbrainz_id" = String, Path, description = "Album or recording MBID"), KindQuery),
    responses((status = 200, body = ClearHistoryResponse))
)]
pub async fn clear_history_handler(
    State(state): State<RequestsState>,
    User(principal): User,
    Path(musicbrainz_id): Path<String>,
    ValidQuery(query): ValidQuery<KindQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = resolve_kind(&query)?;
    let response = RequestsService::new(&state)
        .clear_history_item(&principal, &musicbrainz_id, kind)
        .await?;
    Ok(Json(response))
}

/// Fill the selected edition's missing tracks and upgrade its below-cutoff
/// owned tracks, in one download; with `replace_versions`, also fetch this
/// edition's version of each track the copy holds in another version.
/// `POST /api/v3/albums/{album_id}/edition/acquire`. Curator only. Never
/// retags existing files.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/albums/{album_id}/edition/acquire",
    params(("album_id" = String, Path, description = "Release-group MBID"), EditionAcquireQuery),
    responses(
        (status = 200, body = EditionAcquireResponse),
        (status = 400, description = "Not enough disk space, or not in the library"),
    )
)]
pub async fn acquire_edition_handler(
    State(state): State<RequestsState>,
    curator: Curator,
    Path(album_id): Path<String>,
    ValidQuery(query): ValidQuery<EditionAcquireQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let response = RequestsService::new(&state)
        .acquire_edition(
            &curator,
            &album_id,
            query.album_id.as_deref(),
            query.replace_versions.unwrap_or(true),
        )
        .await?;
    Ok(Json(response))
}

/// The album's current or last edition fetch: its download, and where each
/// track it asked for is. `latest` is absent when there is none. Retry is a
/// new acquire, which asks only for what is still missing; Stop cancels
/// the download. `GET /api/v3/albums/{album_id}/edition/acquire`. Curator
/// only.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/albums/{album_id}/edition/acquire",
    params(("album_id" = String, Path, description = "Release-group MBID")),
    responses(
        (status = 200, body = EditionFetchResponse),
        (status = 400, description = "Not a MusicBrainz id"),
    )
)]
pub async fn edition_fetch_handler(
    State(state): State<RequestsState>,
    _curator: Curator,
    Path(album_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let response = RequestsService::new(&state)
        .edition_fetch(&album_id)
        .await?;
    Ok(Json(response))
}
