//! Identity repair, identity preparation and scan diagnostics routes, at
//! v2's paths under `/api/v3/library`. Every route is admin-only, as in
//! v2. Store work runs on a blocking thread.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::blocking;
use super::operations::{OperationControlBody, OperationView, ReasonView, detail_view, job_view};
use crate::operations::models::{Control, OperationJob};
use crate::repairs::models::{
    FindingRow, FindingState, MANAGEMENT_MAPPING_VERSION, Purpose, REPAIR_TARGET_VERSION,
};
use crate::repairs::reasons;
use crate::repairs::service::{RepairDetail, Repairs};
use crate::wiring::LibraryServices;
use droppedneedle_auth::Admin;

pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .admin(routes!(list_repairs, create_repair))
        .admin(routes!(estimate_repair))
        .admin(routes!(get_repair))
        .admin(routes!(repair_findings))
        .admin(routes!(apply_repair))
        .admin(routes!(pause_repair))
        .admin(routes!(resume_repair))
        .admin(routes!(stop_repair))
        .admin(routes!(list_preparations, create_preparation))
        .admin(routes!(estimate_preparation))
        .admin(routes!(get_preparation))
        .admin(routes!(preparation_findings))
        .admin(routes!(apply_preparation))
        .admin(routes!(discard_preparation))
        .admin(routes!(scan_diagnostics))
}

// ---------------------------------------------------------------------------
// DTOs.
// ---------------------------------------------------------------------------

/// One page of repair or preparation jobs, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperationListView {
    pub items: Vec<OperationView>,
    pub next_cursor: Option<String>,
}

/// Start an identity repair.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RepairCreateBody {
    /// Makes a retried request return the same job.
    pub idempotency_key: String,
    /// Library roots to check; none means every root.
    #[serde(default)]
    pub root_ids: Vec<String>,
    /// Only recheck identities this matcher version wrote.
    pub source_matcher_version: Option<String>,
    #[serde(default = "default_target")]
    pub target_matcher_version: String,
}

fn default_target() -> String {
    REPAIR_TARGET_VERSION.to_owned()
}

/// Start an identity preparation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PreparationCreateBody {
    /// Makes a retried request return the same job.
    pub idempotency_key: String,
    /// Library roots to prepare; none means every root.
    #[serde(default)]
    pub root_ids: Vec<String>,
}

/// Apply a ready report.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RepairApplyBody {
    /// The job's `row_revision` as last read.
    pub expected_row_revision: i64,
    /// Must be true: Apply changes album matches.
    pub confirmation: bool,
}

/// How many albums an identity repair would check.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RepairEstimateView {
    pub identity_count: i64,
    pub selected_root_count: usize,
    pub queued_repair_count: i64,
}

/// Where the albums Library Management may touch stand today.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PreparationEstimateView {
    pub album_count: i64,
    pub ready_album_count: i64,
    pub mapping_required_count: i64,
    pub exact_release_required_count: i64,
    pub selected_root_count: usize,
    pub queued_preparation_count: i64,
}

/// A page of jobs.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

/// A page of findings.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FindingsQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    /// Repairs: `valid`, `safe_detach`, `needs_review`, `unverifiable`,
    /// `manual_identity`. Preparations: `ready`, `mapping_ready`,
    /// `exact_release_required`, `exact_release_auto_accepted`,
    /// `needs_review`, `unverifiable`.
    pub finding_category: Option<String>,
}

/// The edition a preparation suggests for an album.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SuggestedEditionView {
    pub release_mbid: String,
    pub release_group_mbid: String,
    pub title: String,
    pub track_count: u32,
    /// How many editions fit the files.
    pub competing_count: u32,
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    /// With automatic edition acceptance on: `AUTO_ACCEPT`, or why the
    /// suggestion waits for a person.
    pub auto_gate: Option<String>,
}

/// The revisions to echo when undoing the album's automatic edition.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutomaticEditionUndoInfoView {
    pub expected_album_revision: i64,
    pub expected_identity_revision: i64,
}

/// One album's finding.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RepairFindingView {
    #[serde(flatten)]
    pub finding: FindingRow,
    /// What the finding means and what to do about it.
    pub reason: ReasonView,
    pub suggested_edition: Option<SuggestedEditionView>,
    pub automatic_undo: Option<AutomaticEditionUndoInfoView>,
}

/// One page of findings.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RepairFindingListView {
    pub items: Vec<RepairFindingView>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    /// Findings per code that still describe their album.
    pub current_counts_by_finding: BTreeMap<String, i64>,
    /// The report used older rules; run a fresh check before applying.
    pub refresh_required: bool,
}

fn finding_view(row: FindingRow) -> RepairFindingView {
    let reason = if row.state == FindingState::Stale {
        reasons::finding("STALE_SUBJECT")
    } else {
        reasons::finding(&row.reason_code)
    };
    let suggested_edition = row
        .suggested_release_mbid
        .clone()
        .zip(row.suggested_release_group_mbid.clone())
        .map(|(release_mbid, release_group_mbid)| {
            let summary = row.payload.summary.clone().unwrap_or_default();
            SuggestedEditionView {
                release_mbid,
                release_group_mbid,
                title: summary.title,
                track_count: summary.track_count,
                competing_count: summary.competing_count.max(1),
                date: summary.date,
                country: summary.country,
                status: summary.status,
                auto_gate: summary.auto_gate,
            }
        });
    let automatic_undo = row
        .automatic_undo
        .map(|(album, identity)| AutomaticEditionUndoInfoView {
            expected_album_revision: album,
            expected_identity_revision: identity,
        });
    RepairFindingView {
        reason: reason.into(),
        suggested_edition,
        automatic_undo,
        finding: row,
    }
}

fn repair_view(detail: &RepairDetail) -> OperationView {
    OperationView {
        repair_summary: detail.summary.clone(),
        ..detail_view(&detail.detail)
    }
}

fn root_ids(pairs: Vec<(String, String)>) -> Vec<String> {
    pairs
        .into_iter()
        .filter(|(key, _)| key == "root_id")
        .map(|(_, value)| value)
        .collect()
}

// ---------------------------------------------------------------------------
// Shared bodies of the repair and preparation handlers.
// ---------------------------------------------------------------------------

async fn get_job(
    repairs: Repairs,
    job_id: String,
    purpose: Purpose,
) -> Result<Json<OperationView>, ApiError> {
    let detail = blocking(move || repairs.get(&job_id, purpose)).await?;
    Ok(Json(repair_view(&detail)))
}

async fn list_jobs(
    repairs: Repairs,
    query: HistoryQuery,
    purpose: Purpose,
    default_limit: usize,
) -> Result<Json<OperationListView>, ApiError> {
    let (items, next_cursor) = blocking(move || {
        repairs.history(
            purpose,
            query.limit.unwrap_or(default_limit),
            query.cursor.as_deref(),
        )
    })
    .await?;
    Ok(Json(OperationListView {
        items: items.iter().map(repair_view).collect(),
        next_cursor,
    }))
}

async fn findings(
    repairs: Repairs,
    job_id: String,
    query: FindingsQuery,
    purpose: Purpose,
) -> Result<Json<RepairFindingListView>, ApiError> {
    let page = blocking(move || {
        repairs.findings(
            &job_id,
            purpose,
            query.limit.unwrap_or(100),
            query.cursor.as_deref(),
            query.finding_category.as_deref(),
        )
    })
    .await?;
    Ok(Json(RepairFindingListView {
        items: page.items.into_iter().map(finding_view).collect(),
        next_cursor: page.next_cursor,
        has_more: page.has_more,
        current_counts_by_finding: page.current_counts_by_finding,
        refresh_required: page.refresh_required,
    }))
}

async fn apply(
    repairs: Repairs,
    job_id: String,
    body: RepairApplyBody,
    purpose: Purpose,
) -> Result<Json<OperationView>, ApiError> {
    let job = blocking(move || {
        repairs.begin_apply(
            &job_id,
            purpose,
            body.expected_row_revision,
            body.confirmation,
        )
    })
    .await?;
    Ok(Json(job_view(&job)))
}

async fn control(
    repairs: Repairs,
    job_id: String,
    control: Control,
    body: OperationControlBody,
) -> Result<Json<OperationView>, ApiError> {
    let job = blocking(move || {
        repairs.control(
            &job_id,
            Purpose::ExistingMatches,
            control,
            body.expected_row_revision,
            body.idempotency_key.as_deref(),
        )
    })
    .await?;
    Ok(Json(job_view(&job)))
}

fn created(job: &OperationJob) -> Json<OperationView> {
    Json(job_view(job))
}

// ---------------------------------------------------------------------------
// Identity repairs.
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/identity-repairs",
    request_body = RepairCreateBody,
    responses(
        (status = 200, description = "The repair job (new, or the one this key started)", body = OperationView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "The idempotency key started another kind of job"),
    )
)]
pub async fn create_repair(
    State(repairs): State<Repairs>,
    caller: Admin,
    ValidJson(body): ValidJson<RepairCreateBody>,
) -> Result<Json<OperationView>, ApiError> {
    let user = caller.user_id().to_owned();
    let job = blocking(move || {
        repairs.create(
            Purpose::ExistingMatches,
            &user,
            &body.idempotency_key,
            body.root_ids,
            body.source_matcher_version,
            body.target_matcher_version,
        )
    })
    .await?;
    Ok(created(&job))
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/identity-repairs",
    params(HistoryQuery),
    responses(
        (status = 200, description = "Repair jobs, newest first", body = OperationListView),
        (status = 400, description = "Bad page size or cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_repairs(
    State(repairs): State<Repairs>,
    _caller: Admin,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<Json<OperationListView>, ApiError> {
    list_jobs(repairs, query, Purpose::ExistingMatches, 50).await
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/identity-repairs/estimate",
    params(("root_id" = Option<Vec<String>>, Query, explode, description = "Library roots; none means every root")),
    responses(
        (status = 200, description = "Estimate", body = RepairEstimateView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn estimate_repair(
    State(repairs): State<Repairs>,
    _caller: Admin,
    ValidQuery(pairs): ValidQuery<Vec<(String, String)>>,
) -> Result<Json<RepairEstimateView>, ApiError> {
    let roots = root_ids(pairs);
    let selected_root_count = roots.len();
    let estimate = blocking(move || repairs.estimate_repair(roots)).await?;
    Ok(Json(RepairEstimateView {
        identity_count: estimate.identity_count,
        selected_root_count,
        queued_repair_count: estimate.queued_repair_count,
    }))
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/identity-repairs/{job_id}",
    params(("job_id" = String, Path, description = "Repair job id")),
    responses(
        (status = 200, description = "Repair job", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown repair job"),
    )
)]
pub async fn get_repair(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
) -> Result<Json<OperationView>, ApiError> {
    get_job(repairs, job_id, Purpose::ExistingMatches).await
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/identity-repairs/{job_id}/findings",
    params(("job_id" = String, Path, description = "Repair job id"), FindingsQuery),
    responses(
        (status = 200, description = "Findings, newest change first", body = RepairFindingListView),
        (status = 400, description = "Bad page size, cursor or category"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown repair job"),
    )
)]
pub async fn repair_findings(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidQuery(query): ValidQuery<FindingsQuery>,
) -> Result<Json<RepairFindingListView>, ApiError> {
    findings(repairs, job_id, query, Purpose::ExistingMatches).await
}

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/identity-repairs/{job_id}/apply",
    params(("job_id" = String, Path, description = "Repair job id")),
    request_body = RepairApplyBody,
    responses(
        (status = 200, description = "Applying in the background", body = OperationView),
        (status = 400, description = "Confirmation missing"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown repair job"),
        (status = 409, description = "The report changed since it was read"),
    )
)]
pub async fn apply_repair(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<RepairApplyBody>,
) -> Result<Json<OperationView>, ApiError> {
    apply(repairs, job_id, body, Purpose::ExistingMatches).await
}

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/identity-repairs/{job_id}/pause",
    params(("job_id" = String, Path, description = "Repair job id")),
    request_body = OperationControlBody,
    responses(
        (status = 200, description = "Repair job", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown repair job"),
        (status = 409, description = "Stale revision or reused idempotency key"),
    )
)]
pub async fn pause_repair(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<OperationControlBody>,
) -> Result<Json<OperationView>, ApiError> {
    control(repairs, job_id, Control::Pause, body).await
}

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/identity-repairs/{job_id}/resume",
    params(("job_id" = String, Path, description = "Repair job id")),
    request_body = OperationControlBody,
    responses(
        (status = 200, description = "Repair job", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown repair job"),
        (status = 409, description = "Stale revision or reused idempotency key"),
    )
)]
pub async fn resume_repair(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<OperationControlBody>,
) -> Result<Json<OperationView>, ApiError> {
    control(repairs, job_id, Control::Resume, body).await
}

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/identity-repairs/{job_id}/stop",
    params(("job_id" = String, Path, description = "Repair job id")),
    request_body = OperationControlBody,
    responses(
        (status = 200, description = "Repair job", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown repair job"),
        (status = 409, description = "Stale revision or reused idempotency key"),
    )
)]
pub async fn stop_repair(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<OperationControlBody>,
) -> Result<Json<OperationView>, ApiError> {
    control(repairs, job_id, Control::Stop, body).await
}

// ---------------------------------------------------------------------------
// Identity preparations.
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/identity-preparations",
    request_body = PreparationCreateBody,
    responses(
        (status = 200, description = "The preparation job (new, or the one this key started)", body = OperationView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "The idempotency key started another kind of job"),
    )
)]
pub async fn create_preparation(
    State(repairs): State<Repairs>,
    caller: Admin,
    ValidJson(body): ValidJson<PreparationCreateBody>,
) -> Result<Json<OperationView>, ApiError> {
    let user = caller.user_id().to_owned();
    let job = blocking(move || {
        repairs.create(
            Purpose::ManagementReadiness,
            &user,
            &body.idempotency_key,
            body.root_ids,
            None,
            MANAGEMENT_MAPPING_VERSION.to_owned(),
        )
    })
    .await?;
    Ok(created(&job))
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/identity-preparations",
    params(HistoryQuery),
    responses(
        (status = 200, description = "Preparation jobs, newest first", body = OperationListView),
        (status = 400, description = "Bad page size or cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_preparations(
    State(repairs): State<Repairs>,
    _caller: Admin,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<Json<OperationListView>, ApiError> {
    list_jobs(repairs, query, Purpose::ManagementReadiness, 20).await
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/identity-preparations/estimate",
    params(("root_id" = Option<Vec<String>>, Query, explode, description = "Library roots; none means every root")),
    responses(
        (status = 200, description = "Estimate", body = PreparationEstimateView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn estimate_preparation(
    State(repairs): State<Repairs>,
    _caller: Admin,
    ValidQuery(pairs): ValidQuery<Vec<(String, String)>>,
) -> Result<Json<PreparationEstimateView>, ApiError> {
    let roots = root_ids(pairs);
    let selected_root_count = roots.len();
    let estimate = blocking(move || repairs.estimate_preparation(roots)).await?;
    Ok(Json(PreparationEstimateView {
        album_count: estimate.album_count,
        ready_album_count: estimate.ready_album_count,
        mapping_required_count: estimate.mapping_required_count,
        exact_release_required_count: estimate.exact_release_required_count,
        selected_root_count,
        queued_preparation_count: estimate.queued_preparation_count,
    }))
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/identity-preparations/{job_id}",
    params(("job_id" = String, Path, description = "Preparation job id")),
    responses(
        (status = 200, description = "Preparation job", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown preparation job"),
    )
)]
pub async fn get_preparation(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
) -> Result<Json<OperationView>, ApiError> {
    get_job(repairs, job_id, Purpose::ManagementReadiness).await
}

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/identity-preparations/{job_id}/findings",
    params(("job_id" = String, Path, description = "Preparation job id"), FindingsQuery),
    responses(
        (status = 200, description = "Findings that still describe their album", body = RepairFindingListView),
        (status = 400, description = "Bad page size, cursor or category"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown preparation job"),
    )
)]
pub async fn preparation_findings(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidQuery(query): ValidQuery<FindingsQuery>,
) -> Result<Json<RepairFindingListView>, ApiError> {
    findings(repairs, job_id, query, Purpose::ManagementReadiness).await
}

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/identity-preparations/{job_id}/apply",
    params(("job_id" = String, Path, description = "Preparation job id")),
    request_body = RepairApplyBody,
    responses(
        (status = 200, description = "Applying in the background", body = OperationView),
        (status = 400, description = "Confirmation missing, or the report used older rules"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown preparation job"),
        (status = 409, description = "The report changed since it was read"),
    )
)]
pub async fn apply_preparation(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<RepairApplyBody>,
) -> Result<Json<OperationView>, ApiError> {
    apply(repairs, job_id, body, Purpose::ManagementReadiness).await
}

#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/identity-preparations/{job_id}/discard",
    params(("job_id" = String, Path, description = "Preparation job id")),
    request_body = OperationControlBody,
    responses(
        (status = 200, description = "Dismissed", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown preparation job"),
        (status = 409, description = "The report changed since it was read"),
    )
)]
pub async fn discard_preparation(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<OperationControlBody>,
) -> Result<Json<OperationView>, ApiError> {
    let job = blocking(move || repairs.discard(&job_id, body.expected_row_revision)).await?;
    Ok(Json(job_view(&job)))
}

// ---------------------------------------------------------------------------
// Scan diagnostics.
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/scan/runs/{id}/diagnostics",
    params(("id" = String, Path, description = "Scan run id")),
    responses(
        (status = 200, description = "Redacted diagnostic report, as a JSON download", content_type = "application/json", body = Object),
        (status = 400, description = "Bad run id, or the report is over the 2 MB limit"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown scan run"),
    )
)]
pub async fn scan_diagnostics(
    State(repairs): State<Repairs>,
    _caller: Admin,
    Path(run_id): Path<String>,
) -> Result<Response, ApiError> {
    // One pooled read; a database too busy to hand out a connection
    // answers the server-busy 503.
    let (filename, bytes) = blocking(move || match repairs.scan_diagnostics(&run_id) {
        Ok(report) => report.map_err(ApiError::from),
        Err(error @ (DbError::Busy { .. } | DbError::LaneClosed)) => {
            Err(ApiError::unavailable(Unavailable::ServerBusy, error))
        }
        Err(error) => Err(ApiError::internal(error)),
    })
    .await?;
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        bytes,
    )
        .into_response())
}
