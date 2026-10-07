//! Held-import and upgrade HTTP: the held list, the per-file actions
//! (import as is, discard, fingerprint re-check, audio preview), the
//! per-download actions (retry or discard what waits on the library,
//! discard a wrong-product verdict), and the quality-upgrade worklist and
//! asks.
//!
//! Every route sits inside the session gate. Held files follow v2's
//! ownership: admins act on every file, everyone else on their own, and
//! someone else's file answers 404. The per-download library actions are
//! admin only; the upgrade routes are for admins and trusted users.

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::held::{BulkSkip, HeldError, HeldImports, Placed, Reverified};
use super::held_rows::HeldRow;
use super::queue_rows::Viewer;
use super::upgrades::{UpgradeAsk, UpgradeError, UpgradeOutcome, Upgrades};
use crate::acquire::flows::seams::DispatchKind;
use crate::acquire::requests::{
    auth::Principal,
    error::RequestsError,
    http::{HttpError, ValidJson, ValidQuery},
};
use crate::stream::routes::{DirectSpan, content_type_for_extension, file_range};

/// One held file.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldImportResponse {
    pub id: i64,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub track_title: Option<String>,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub year: Option<i64>,
    pub original_filename: Option<String>,
    pub file_format: Option<String>,
    pub duration_seconds: Option<f64>,
    pub expected_duration_seconds: Option<f64>,
    /// Stable reason code (`fingerprint_mismatch`, `target_occupied`, ...).
    pub reason: String,
    /// The reason as one plain sentence.
    pub reason_text: Option<String>,
    /// What the person can do about it.
    pub reason_action: Option<String>,
    /// Specifics: which file, which marker.
    pub reason_detail: Option<String>,
    /// `soulseek`, `usenet` or `plugin:<key>`.
    pub source: String,
    pub source_task_id: Option<String>,
    /// `user`, `upgrade`, ... An upgrade's file replaces the library's
    /// copy only when it is better.
    pub origin: String,
    pub created_at: f64,
    /// What the rejecting check saw: the title and artist AcoustID heard
    /// or the file's tags name, and AcoustID's score.
    pub evidence_title: Option<String>,
    pub evidence_artist: Option<String>,
    pub evidence_score: Option<f64>,
    pub management_retry_count: i64,
    pub management_next_retry_at: Option<f64>,
}

impl From<HeldRow> for HeldImportResponse {
    fn from(row: HeldRow) -> Self {
        Self {
            id: row.id,
            release_group_mbid: row.release_group_mbid,
            release_mbid: row.release_mbid,
            release_track_mbid: row.release_track_mbid,
            recording_mbid: row.recording_mbid,
            track_number: row.track_number,
            disc_number: row.disc_number,
            track_title: row.track_title,
            artist_name: row.artist_name,
            album_title: row.album_title,
            year: row.year,
            original_filename: row.original_filename,
            file_format: row.file_format,
            duration_seconds: row.duration_seconds,
            expected_duration_seconds: row.expected_duration_seconds,
            reason: row.reason,
            reason_text: row.reason_text,
            reason_action: row.reason_action,
            reason_detail: row.reason_detail,
            source: row.source,
            source_task_id: row.source_task_id,
            origin: row.origin,
            created_at: row.created_at,
            evidence_title: row.evidence_title,
            evidence_artist: row.evidence_artist,
            evidence_score: row.evidence_score,
            management_retry_count: row.management_retry_count,
            management_next_retry_at: row.management_next_retry_at,
        }
    }
}

/// Held files, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldListResponse {
    pub items: Vec<HeldImportResponse>,
}

/// Held list filter.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HeldListQuery {
    /// Only this album's held files.
    pub release_group_mbid: Option<String>,
}

/// What an import or discard did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldActionResponse {
    /// `imported` or `discarded`.
    pub status: String,
    /// Where the imported file now lives.
    pub final_path: Option<String>,
    /// A note when the outcome needs one (an upgrade's file that was no
    /// better than the library's copy).
    pub message: Option<String>,
}

/// What a fingerprint re-check did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldReverifyResponse {
    /// `imported` when AcoustID now agrees, else `still_held`.
    pub status: String,
    pub final_path: Option<String>,
}

/// Bulk re-check: these held ids, in order, or every held file.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct HeldBulkReverifyRequest {
    pub held_ids: Option<Vec<i64>>,
}

/// One file of a bulk re-check.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldBulkReverifyItem {
    pub held_id: i64,
    /// `imported`, `still_held`, `skipped` or `error`.
    pub status: String,
    pub final_path: Option<String>,
    pub release_group_mbid: Option<String>,
    /// Why a file was skipped or failed.
    pub message: Option<String>,
}

/// A bulk re-check's results.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldBulkReverifyResponse {
    pub results: Vec<HeldBulkReverifyItem>,
}

/// A per-download held action's outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldUnitResponse {
    /// `imported` or `discarded`.
    pub status: String,
    /// Files acted on.
    pub files: usize,
}

/// One album below the quality cutoff.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CutoffUnmetItem {
    pub release_group_mbid: String,
    /// The album's worst file tier.
    pub current_tier: String,
    pub track_count: i64,
    pub artist_name: Option<String>,
    pub artist_mbid: Option<String>,
    pub album_title: Option<String>,
    pub year: Option<i64>,
}

/// The upgrade worklist.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CutoffUnmetResponse {
    /// Empty while upgrades are switched off.
    pub items: Vec<CutoffUnmetItem>,
    /// The tier albums must reach.
    pub cutoff: String,
    pub upgrade_allowed: bool,
}

/// Upgrade one album.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpgradeAlbumRequest {
    pub release_group_mbid: String,
    pub artist_name: String,
    pub album_title: String,
    pub year: Option<i64>,
    pub artist_mbid: Option<String>,
}

/// Upgrade one track.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpgradeTrackRequest {
    pub recording_mbid: String,
    pub artist_name: String,
    pub track_title: String,
    pub album_title: Option<String>,
    pub duration_seconds: Option<f64>,
    pub release_group_mbid: Option<String>,
    pub artist_mbid: Option<String>,
}

/// What an upgrade ask did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpgradeRequestResponse {
    /// `queued`, or `satisfied` when nothing needed queueing (the copy
    /// meets the cutoff, upgrades are off, or it is already downloading).
    pub status: String,
    pub task_id: Option<String>,
}

/// Router state.
#[derive(Clone)]
pub struct ReviewState {
    pub held: HeldImports,
    pub upgrades: Upgrades,
}

fn viewer(principal: &Principal) -> Viewer {
    Viewer {
        user_id: principal.user_id.clone(),
        admin: principal.role.is_admin(),
    }
}

impl From<HeldError> for HttpError {
    fn from(error: HeldError) -> Self {
        HttpError(match error {
            HeldError::NotFound(_) => RequestsError::NotFound,
            HeldError::Refused(message) => RequestsError::Conflict { message },
            HeldError::Unavailable(cause) => RequestsError::internal(&cause),
        })
    }
}

impl From<UpgradeError> for HttpError {
    fn from(error: UpgradeError) -> Self {
        HttpError(match error {
            UpgradeError::Unavailable(cause) => RequestsError::internal(&cause),
        })
    }
}

/// Held ids arrive as path text; a non-number is simply not found.
fn held_id(text: &str) -> Result<i64, HttpError> {
    text.parse().map_err(|_| HttpError(RequestsError::NotFound))
}

fn path_text(placed: &Placed) -> Option<String> {
    match placed {
        Placed::Imported(path) => Some(path.to_string_lossy().into_owned()),
        Placed::KeptExisting => None,
    }
}

const KEPT_EXISTING: &str =
    "Your current copy is as good or better, so it was kept and this file was removed.";

/// Held files the viewer may act on, newest first; one album's with
/// `release_group_mbid`. `GET /api/v3/downloads/held`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/held",
    params(HeldListQuery),
    responses((status = 200, body = HeldListResponse))
)]
pub async fn list_held_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    ValidQuery(query): ValidQuery<HeldListQuery>,
) -> Result<Json<HeldListResponse>, HttpError> {
    let group = query.release_group_mbid.filter(|mbid| !mbid.is_empty());
    let rows = state
        .held
        .list(&viewer(&principal), group.as_deref())
        .await?;
    Ok(Json(HeldListResponse {
        items: rows.into_iter().map(Into::into).collect(),
    }))
}

/// Import a held file as it is, into the album's chosen edition. For an
/// upgrade, the library's copy is replaced only by a better file and goes
/// to the recycle bin. `POST /api/v3/downloads/held/{held_id}/import`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/held/{held_id}/import",
    params(("held_id" = i64, Path, description = "Held file id")),
    responses((status = 200, body = HeldActionResponse))
)]
pub async fn import_held_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<HeldActionResponse>, HttpError> {
    let placed = state
        .held
        .import(&viewer(&principal), held_id(&id)?, &principal.user_id)
        .await?;
    Ok(Json(HeldActionResponse {
        status: "imported".to_owned(),
        message: matches!(placed, Placed::KeptExisting).then(|| KEPT_EXISTING.to_owned()),
        final_path: path_text(&placed),
    }))
}

/// Delete a held file; the album's automatic retry resumes.
/// `POST /api/v3/downloads/held/{held_id}/discard`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/held/{held_id}/discard",
    params(("held_id" = i64, Path, description = "Held file id")),
    responses((status = 200, body = HeldActionResponse))
)]
pub async fn discard_held_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<HeldActionResponse>, HttpError> {
    state
        .held
        .discard(&viewer(&principal), held_id(&id)?)
        .await?;
    Ok(Json(HeldActionResponse {
        status: "discarded".to_owned(),
        final_path: None,
        message: None,
    }))
}

/// Fingerprint an AcoustID-held file again and import it when AcoustID
/// now agrees. `POST /api/v3/downloads/held/{held_id}/reverify`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/held/{held_id}/reverify",
    params(("held_id" = i64, Path, description = "Held file id")),
    responses((status = 200, body = HeldReverifyResponse))
)]
pub async fn reverify_held_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<HeldReverifyResponse>, HttpError> {
    let outcome = state
        .held
        .reverify(&viewer(&principal), held_id(&id)?, &principal.user_id)
        .await?;
    Ok(Json(match outcome {
        Reverified::Imported(placed) => HeldReverifyResponse {
            status: "imported".to_owned(),
            final_path: path_text(&placed),
        },
        Reverified::StillHeld => HeldReverifyResponse {
            status: "still_held".to_owned(),
            final_path: None,
        },
    }))
}

/// Re-check AcoustID holds in bulk (at most 25 checks a call; other holds
/// answer `skipped`). `POST /api/v3/downloads/held/reverify`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/held/reverify",
    request_body = HeldBulkReverifyRequest,
    responses((status = 200, body = HeldBulkReverifyResponse))
)]
pub async fn reverify_bulk_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    ValidJson(body): ValidJson<HeldBulkReverifyRequest>,
) -> Result<Json<HeldBulkReverifyResponse>, HttpError> {
    let items = state
        .held
        .reverify_bulk(&viewer(&principal), body.held_ids, &principal.user_id)
        .await?;
    let results = items
        .into_iter()
        .map(|item| {
            let (status, final_path, message) = match item.outcome {
                Ok(Reverified::Imported(placed)) => ("imported", path_text(&placed), None),
                Ok(Reverified::StillHeld) => ("still_held", None, None),
                Err(BulkSkip::NotFingerprint) => (
                    "skipped",
                    None,
                    Some(
                        "Only files held because AcoustID heard a different recording can be \
                         re-checked."
                            .to_owned(),
                    ),
                ),
                Err(BulkSkip::Failed(message)) => ("error", None, Some(message)),
            };
            HeldBulkReverifyItem {
                held_id: item.held_id,
                status: status.to_owned(),
                final_path,
                release_group_mbid: item.release_group_mbid,
                message,
            }
        })
        .collect();
    Ok(Json(HeldBulkReverifyResponse { results }))
}

/// Play a held file for the review, with byte ranges so the player can
/// seek. `GET /api/v3/downloads/held/{held_id}/audio`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/held/{held_id}/audio",
    params(("held_id" = i64, Path, description = "Held file id")),
    responses(
        (status = 200, description = "The held file", content_type = "application/octet-stream"),
        (status = 206, description = "The requested byte range", content_type = "application/octet-stream"),
        (status = 416, description = "The range lies outside the file")
    )
)]
pub async fn held_audio_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let row = state.held.get(&viewer(&principal), held_id(&id)?).await?;
    let path = std::path::PathBuf::from(&row.held_path);
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return Err(HttpError(RequestsError::NotFound));
    };
    let total = meta.len();
    let Some(span) = DirectSpan::for_request(headers.get(header::RANGE), total) else {
        let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
        if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
        return Ok(response);
    };
    let content_type = path
        .extension()
        .and_then(|ext| content_type_for_extension(&ext.to_string_lossy()))
        .unwrap_or("application/octet-stream");
    let mut response = Response::new(Body::from_stream(file_range(path, span.start, span.len)));
    *response.status_mut() = span.status();
    span.write_headers(response.headers_mut());
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    Ok(response)
}

/// Import every file of a download that waits on the library (a taken
/// destination, an old copy that could not be recycled). Admin only.
/// `POST /api/v3/downloads/held/management/{source_task_id}/retry`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/held/management/{source_task_id}/retry",
    params(("source_task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = HeldUnitResponse))
)]
pub async fn retry_local_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<HeldUnitResponse>, HttpError> {
    principal.require_admin()?;
    let files = state
        .held
        .retry_local(&viewer(&principal), &task_id)
        .await?;
    Ok(Json(HeldUnitResponse {
        status: "imported".to_owned(),
        files,
    }))
}

/// Delete every file of a download that waits on the library. Admin only.
/// `POST /api/v3/downloads/held/management/{source_task_id}/discard`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/held/management/{source_task_id}/discard",
    params(("source_task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = HeldUnitResponse))
)]
pub async fn discard_local_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<HeldUnitResponse>, HttpError> {
    principal.require_admin()?;
    let files = state
        .held
        .discard_local(&viewer(&principal), &task_id)
        .await?;
    Ok(Json(HeldUnitResponse {
        status: "discarded".to_owned(),
        files,
    }))
}

/// Delete every file a download's checks held and clear its wrong-product
/// verdict. `POST /api/v3/downloads/held/verdict/{source_task_id}/discard`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/held/verdict/{source_task_id}/discard",
    params(("source_task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = HeldUnitResponse))
)]
pub async fn discard_verdict_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<HeldUnitResponse>, HttpError> {
    let files = state
        .held
        .discard_verdict(&viewer(&principal), &task_id)
        .await?;
    Ok(Json(HeldUnitResponse {
        status: "discarded".to_owned(),
        files,
    }))
}

/// Albums below the quality cutoff, worst first; empty while upgrades are
/// off. Admins and trusted users. `GET /api/v3/downloads/cutoff-unmet`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/cutoff-unmet",
    responses((status = 200, body = CutoffUnmetResponse))
)]
pub async fn cutoff_unmet_handler(
    State(state): State<ReviewState>,
    principal: Principal,
) -> Result<Json<CutoffUnmetResponse>, HttpError> {
    principal.require_curator()?;
    let view = state.upgrades.cutoff_unmet().await?;
    Ok(Json(CutoffUnmetResponse {
        items: view
            .items
            .into_iter()
            .map(|item| CutoffUnmetItem {
                release_group_mbid: item.rg_mbid,
                current_tier: item.current_tier.to_owned(),
                track_count: item.track_count,
                artist_name: Some(item.artist).filter(|name| !name.is_empty()),
                artist_mbid: item.artist_mbid,
                album_title: Some(item.title),
                year: item.year,
            })
            .collect(),
        cutoff: view.cutoff,
        upgrade_allowed: view.upgrade_allowed,
    }))
}

fn upgrade_response(outcome: UpgradeOutcome) -> UpgradeRequestResponse {
    match outcome {
        UpgradeOutcome::Queued(task_id) => UpgradeRequestResponse {
            status: "queued".to_owned(),
            task_id: Some(task_id),
        },
        UpgradeOutcome::Satisfied => UpgradeRequestResponse {
            status: "satisfied".to_owned(),
            task_id: None,
        },
    }
}

fn parse_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, HttpError> {
    serde_json::from_slice(body).map_err(|cause| {
        HttpError(RequestsError::InvalidInput {
            message: format!("Invalid request body: {cause}"),
        })
    })
}

fn required(value: &str, name: &str) -> Result<String, HttpError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(HttpError(RequestsError::InvalidInput {
            message: format!("{name} is required"),
        }));
    }
    Ok(value.to_owned())
}

/// Fetch a better copy of an album below the cutoff. Admins and trusted
/// users. `POST /api/v3/downloads/upgrade/album`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/upgrade/album",
    request_body = UpgradeAlbumRequest,
    responses((status = 200, body = UpgradeRequestResponse))
)]
pub async fn upgrade_album_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    body: Bytes,
) -> Result<Json<UpgradeRequestResponse>, HttpError> {
    // The role check comes before the body parse, so a plain user always
    // hears 403 whatever they sent.
    principal.require_curator()?;
    let body: UpgradeAlbumRequest = parse_body(&body)?;
    let outcome = state
        .upgrades
        .request(UpgradeAsk {
            user_id: principal.user_id.clone(),
            kind: DispatchKind::Album,
            mbid: required(&body.release_group_mbid, "release_group_mbid")?,
            artist: body.artist_name,
            title: body.album_title,
        })
        .await?;
    Ok(Json(upgrade_response(outcome)))
}

/// Fetch a better copy of one track below the cutoff. Admins and trusted
/// users. `POST /api/v3/downloads/upgrade/track`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/upgrade/track",
    request_body = UpgradeTrackRequest,
    responses((status = 200, body = UpgradeRequestResponse))
)]
pub async fn upgrade_track_handler(
    State(state): State<ReviewState>,
    principal: Principal,
    body: Bytes,
) -> Result<Json<UpgradeRequestResponse>, HttpError> {
    // The role check comes before the body parse, so a plain user always
    // hears 403 whatever they sent.
    principal.require_curator()?;
    let body: UpgradeTrackRequest = parse_body(&body)?;
    let outcome = state
        .upgrades
        .request(UpgradeAsk {
            user_id: principal.user_id.clone(),
            kind: DispatchKind::Track,
            mbid: required(&body.recording_mbid, "recording_mbid")?,
            artist: body.artist_name,
            title: body.track_title,
        })
        .await?;
    Ok(Json(upgrade_response(outcome)))
}
