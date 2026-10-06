//! Scan control and library activity routes.
//!
//! Requests by kind and scope with the policy guard, the current runs,
//! history pages, estimates, failed paths, pause, resume and stop, the
//! identification pause switch, and the activity feed. Everything here is
//! for administrators except the activity feed, which every signed-in
//! user reads (administrators see a little more). Ported from v2's
//! `library_scan_target` routes.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::auth::{Principal, RequireAdmin};
use super::error::{LibraryError, ValidJson, ValidQuery};
use super::handlers::run_view;
use super::models::{ScanRunView, snake};
use crate::library::activity::feed::LibraryActivity;
use crate::library::scan::models::{Disposition, ScanKind};
use crate::library::scans::{MAX_FAILURE_PAGE, MAX_PAGE, RunControl};
use crate::library::wiring::LibrarySetup;

/// Scan control and activity routes, merged into the library router.
pub fn routes() -> Router<LibrarySetup> {
    Router::new()
        // GET on this path (the run list) lives with the other handlers;
        // the merge joins the two methods.
        .route("/library/scan/runs", post(request_scan_run))
        .route("/library/scan/runs/current", get(current_scan_runs))
        .route("/library/scan/runs/history", get(scan_run_history))
        .route("/library/scan/runs/estimate", get(estimate_scan_run))
        .route("/library/scan/runs/{id}/failures", get(scan_run_failures))
        .route("/library/scan/runs/{id}/pause", post(pause_scan_run))
        .route("/library/scan/runs/{id}/resume", post(resume_scan_run))
        .route("/library/scan/runs/{id}/stop", post(stop_scan_run))
        .route("/library/activity", get(library_activity))
        .route("/library/identification/pause", post(pause_identification))
        .route(
            "/library/identification/resume",
            post(resume_identification),
        )
}

/// Run store-backed work on a blocking thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, LibraryError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause))
}

// ---------------------------------------------------------------------------
// DTOs.
// ---------------------------------------------------------------------------

/// Scan kind a caller can ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanKindBody {
    /// Read new and changed files.
    #[default]
    Incremental,
    /// Read every file again, changed or not.
    RescanFiles,
    /// Walk again under the current policy without reading tags.
    PolicyReconcile,
}

impl From<ScanKindBody> for ScanKind {
    fn from(kind: ScanKindBody) -> Self {
        match kind {
            ScanKindBody::Incremental => Self::Incremental,
            ScanKindBody::RescanFiles => Self::RescanFiles,
            ScanKindBody::PolicyReconcile => Self::PolicyReconcile,
        }
    }
}

/// A scan request.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ScanRunRequestBody {
    /// What kind of scan to run.
    #[serde(default)]
    pub kind: ScanKindBody,
    /// Root and path-rule ids to scan; empty means every root.
    #[serde(default)]
    pub scope_ids: Vec<String>,
    /// Policy revision from the library settings the caller last read.
    /// A request against older settings is refused with 409.
    #[serde(default)]
    pub expected_policy_revision: String,
}

/// How the request was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanDisposition {
    /// A new run started.
    Started,
    /// A new run waits behind the active one.
    Queued,
    /// An existing run already covers it.
    Coalesced,
    /// A queued run grew to cover it.
    Expanded,
    /// A run of another kind is in the way.
    Conflict,
}

impl From<Disposition> for ScanDisposition {
    fn from(disposition: Disposition) -> Self {
        match disposition {
            Disposition::Started => Self::Started,
            Disposition::Queued => Self::Queued,
            Disposition::Coalesced => Self::Coalesced,
            Disposition::Expanded => Self::Expanded,
            Disposition::Conflict => Self::Conflict,
        }
    }
}

/// Answer to a scan request.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanRunRequestedResponse {
    /// The run that will do the work.
    pub run_id: String,
    pub disposition: ScanDisposition,
    /// That run's state.
    pub state: String,
    /// That run's row revision.
    pub row_revision: u64,
    /// Why the request waits, when it does.
    #[schema(required = true)]
    pub queued_reason: Option<String>,
    /// Kind of the run in the way, on a conflict.
    #[schema(required = true)]
    pub conflicting_kind: Option<String>,
}

/// The run holding the worker and the next queued one.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanRunCurrentResponse {
    #[schema(required = true)]
    pub active: Option<ScanRunView>,
    #[schema(required = true)]
    pub queued: Option<ScanRunView>,
}

/// History page query.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct HistoryQuery {
    /// Runs per page, 1 to 50 (default 50).
    pub limit: Option<usize>,
    /// Cursor from the previous page.
    pub cursor: Option<String>,
}

/// One page of finished runs, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanRunHistoryResponse {
    pub items: Vec<ScanRunView>,
    /// Pass back as `cursor` for the next page; null on the last.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// Approximate size of a scan.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanEstimateResponse {
    /// Always true: the count is catalog rows, not a fresh walk.
    pub approximate: bool,
    #[schema(required = true)]
    pub estimated_file_count: Option<u64>,
    /// When the count was taken (unix seconds).
    #[schema(required = true)]
    pub estimated_at: Option<f64>,
}

/// Failed paths query.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct FailuresQuery {
    /// Rows per page, 1 to 200 (default 50).
    pub limit: Option<usize>,
    /// Cursor from the previous page.
    pub cursor: Option<i64>,
}

/// One path a scan could not read or index.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanRunFailureItem {
    pub root_id: String,
    pub relative_path: String,
    /// Machine code, for example `TAG_READ_FAILED`.
    pub failure_code: String,
    pub failure_detail: String,
    /// `discovering`, `indexing` or `reconciling`.
    pub phase: String,
    /// Unix seconds.
    pub recorded_at: f64,
}

/// One page of a run's failed paths, oldest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanRunFailuresResponse {
    pub items: Vec<ScanRunFailureItem>,
    /// Pass back as `cursor` for the next page; null on the last.
    #[schema(required = true)]
    pub next_cursor: Option<i64>,
}

/// A pause, resume or stop request.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ScanControlBody {
    /// The run's row revision as the caller last saw it.
    pub expected_revision: u64,
}

/// A run after pause, resume or stop.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanControlResponse {
    pub run_id: String,
    pub state: String,
    pub row_revision: u64,
    pub event_revision: u64,
    pub stream_revision: u64,
}

/// A pause or resume of identification.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct IdentificationControlBody {
    /// The switch revision from the activity feed's
    /// `control_revision`.
    pub expected_revision: u64,
}

/// Identification queue state after a pause or resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationState {
    Running,
    /// Paused, with a job still finishing.
    Pausing,
    Paused,
}

/// Answer to a pause or resume of identification.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IdentificationControlResponse {
    pub state: IdentificationState,
    pub row_revision: u64,
}

// ---------------------------------------------------------------------------
// Scan runs.
// ---------------------------------------------------------------------------

/// Request a scan by kind over chosen roots and path rules.
#[utoipa::path(
    post,
    path = "/api/v3/library/scan/runs",
    request_body = ScanRunRequestBody,
    responses(
        (status = 202, description = "Scan taken", body = ScanRunRequestedResponse),
        (status = 400, description = "Bad body or unknown scope id"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "Library disabled or library policy changed"),
    )
)]
pub async fn request_scan_run(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<ScanRunRequestBody>,
) -> Result<(StatusCode, Json<ScanRunRequestedResponse>), LibraryError> {
    let user_id = caller.0.user_id;
    let result = blocking(move || {
        state.request_scan_run(
            body.kind.into(),
            &body.scope_ids,
            &body.expected_policy_revision,
            &user_id,
        )
    })
    .await??;
    Ok((
        StatusCode::ACCEPTED,
        Json(ScanRunRequestedResponse {
            run_id: result.run_id,
            disposition: result.disposition.into(),
            state: snake(&result.state),
            row_revision: result.row_revision,
            queued_reason: result.queued_reason,
            conflicting_kind: result.conflicting_kind.as_ref().map(snake),
        }),
    ))
}

/// The run holding the scan worker and the next queued one.
#[utoipa::path(
    get,
    path = "/api/v3/library/scan/runs/current",
    responses(
        (status = 200, description = "Current runs", body = ScanRunCurrentResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn current_scan_runs(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
) -> Result<Json<ScanRunCurrentResponse>, LibraryError> {
    let (active, queued) = blocking(move || state.current_scan_runs()).await?;
    Ok(Json(ScanRunCurrentResponse {
        active: active.as_ref().map(run_view),
        queued: queued.as_ref().map(run_view),
    }))
}

/// Finished runs, newest first, a page at a time.
#[utoipa::path(
    get,
    path = "/api/v3/library/scan/runs/history",
    params(
        ("limit" = Option<usize>, Query, description = "Runs per page, 1 to 50"),
        ("cursor" = Option<String>, Query, description = "Cursor from the previous page"),
    ),
    responses(
        (status = 200, description = "History page", body = ScanRunHistoryResponse),
        (status = 400, description = "Bad cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn scan_run_history(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<Json<ScanRunHistoryResponse>, LibraryError> {
    let limit = query.limit.unwrap_or(MAX_PAGE);
    let (runs, next_cursor) =
        blocking(move || state.scan_history_page(limit, query.cursor.as_deref())).await??;
    Ok(Json(ScanRunHistoryResponse {
        items: runs.iter().map(run_view).collect(),
        next_cursor,
    }))
}

/// Roughly how many files a scan over the chosen scopes would read.
#[utoipa::path(
    get,
    path = "/api/v3/library/scan/runs/estimate",
    params(
        ("scope_ids" = Option<Vec<String>>, Query, explode, description = "Root and path-rule ids; none means every root"),
    ),
    responses(
        (status = 200, description = "Estimate", body = ScanEstimateResponse),
        (status = 400, description = "Unknown scope id"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn estimate_scan_run(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    ValidQuery(pairs): ValidQuery<Vec<(String, String)>>,
) -> Result<Json<ScanEstimateResponse>, LibraryError> {
    let scope_ids: Vec<String> = pairs
        .into_iter()
        .filter(|(key, _)| key == "scope_ids")
        .map(|(_, value)| value)
        .collect();
    let (count, at) = blocking(move || state.estimate_scan(&scope_ids)).await??;
    Ok(Json(ScanEstimateResponse {
        approximate: true,
        estimated_file_count: Some(count),
        estimated_at: Some(at),
    }))
}

/// Paths one run could not read or index, oldest first.
#[utoipa::path(
    get,
    path = "/api/v3/library/scan/runs/{id}/failures",
    params(
        ("id" = String, Path, description = "Scan run id"),
        ("limit" = Option<usize>, Query, description = "Rows per page, 1 to 200"),
        ("cursor" = Option<i64>, Query, description = "Cursor from the previous page"),
    ),
    responses(
        (status = 200, description = "Failed paths", body = ScanRunFailuresResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown run id"),
    )
)]
pub async fn scan_run_failures(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<FailuresQuery>,
) -> Result<Json<ScanRunFailuresResponse>, LibraryError> {
    let limit = query.limit.unwrap_or(50).min(MAX_FAILURE_PAGE);
    let (failures, next_cursor) =
        blocking(move || state.scan_run_failures(&id, limit, query.cursor)).await??;
    Ok(Json(ScanRunFailuresResponse {
        items: failures
            .into_iter()
            .map(|failure| ScanRunFailureItem {
                phase: snake(&failure.phase),
                root_id: failure.root_id,
                relative_path: failure.relative_path,
                failure_code: failure.failure_code,
                failure_detail: failure.failure_detail,
                recorded_at: failure.recorded_at,
            })
            .collect(),
        next_cursor,
    }))
}

async fn control(
    state: LibrarySetup,
    id: String,
    control: RunControl,
    body: ScanControlBody,
) -> Result<Json<ScanControlResponse>, LibraryError> {
    let controlled =
        blocking(move || state.control_scan_run(&id, control, body.expected_revision)).await??;
    let run = controlled.run;
    Ok(Json(ScanControlResponse {
        state: snake(&run.state),
        run_id: run.id,
        row_revision: run.row_revision,
        event_revision: run.event_revision,
        stream_revision: controlled.stream_revision,
    }))
}

/// Pause a running scan at its next checkpoint.
#[utoipa::path(
    post,
    path = "/api/v3/library/scan/runs/{id}/pause",
    params(("id" = String, Path, description = "Scan run id")),
    request_body = ScanControlBody,
    responses(
        (status = 200, description = "Run after the request", body = ScanControlResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown run id"),
        (status = 409, description = "Run changed, or cannot pause in its state"),
    )
)]
pub async fn pause_scan_run(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ScanControlBody>,
) -> Result<Json<ScanControlResponse>, LibraryError> {
    control(state, id, RunControl::Pause, body).await
}

/// Resume a paused scan where it stopped.
#[utoipa::path(
    post,
    path = "/api/v3/library/scan/runs/{id}/resume",
    params(("id" = String, Path, description = "Scan run id")),
    request_body = ScanControlBody,
    responses(
        (status = 200, description = "Run after the request", body = ScanControlResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown run id"),
        (status = 409, description = "Run changed, or is not paused"),
    )
)]
pub async fn resume_scan_run(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ScanControlBody>,
) -> Result<Json<ScanControlResponse>, LibraryError> {
    control(state, id, RunControl::Resume, body).await
}

/// Stop a scan. What it already indexed stays.
#[utoipa::path(
    post,
    path = "/api/v3/library/scan/runs/{id}/stop",
    params(("id" = String, Path, description = "Scan run id")),
    request_body = ScanControlBody,
    responses(
        (status = 200, description = "Run after the request", body = ScanControlResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown run id"),
        (status = 409, description = "Run changed, or already finished"),
    )
)]
pub async fn stop_scan_run(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ScanControlBody>,
) -> Result<Json<ScanControlResponse>, LibraryError> {
    control(state, id, RunControl::Stop, body).await
}

// ---------------------------------------------------------------------------
// Activity and identification.
// ---------------------------------------------------------------------------

/// What the library is doing now: scans, identification, and the work
/// stack.
#[utoipa::path(
    get,
    path = "/api/v3/library/activity",
    responses(
        (status = 200, description = "Activity feed", body = LibraryActivity),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn library_activity(
    State(state): State<LibrarySetup>,
    caller: Principal,
) -> Result<Json<LibraryActivity>, LibraryError> {
    let admin = caller.role.is_admin();
    Ok(Json(
        blocking(move || state.library_activity(admin)).await??,
    ))
}

async fn identification_control(
    state: LibrarySetup,
    paused: bool,
    user_id: String,
    body: IdentificationControlBody,
) -> Result<Json<IdentificationControlResponse>, LibraryError> {
    let answer = blocking(move || {
        state.set_identification_paused(paused, &user_id, Some(body.expected_revision))
    })
    .await??;
    Ok(Json(IdentificationControlResponse {
        state: match answer.state {
            "pausing" => IdentificationState::Pausing,
            "paused" => IdentificationState::Paused,
            _ => IdentificationState::Running,
        },
        row_revision: answer.row_revision,
    }))
}

/// Pause identification. A job already running finishes first.
#[utoipa::path(
    post,
    path = "/api/v3/library/identification/pause",
    request_body = IdentificationControlBody,
    responses(
        (status = 200, description = "Queue state", body = IdentificationControlResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "The switch changed since it was read"),
    )
)]
pub async fn pause_identification(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<IdentificationControlBody>,
) -> Result<Json<IdentificationControlResponse>, LibraryError> {
    identification_control(state, true, caller.0.user_id, body).await
}

/// Resume identification.
#[utoipa::path(
    post,
    path = "/api/v3/library/identification/resume",
    request_body = IdentificationControlBody,
    responses(
        (status = 200, description = "Queue state", body = IdentificationControlResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "The switch changed since it was read"),
    )
)]
pub async fn resume_identification(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<IdentificationControlBody>,
) -> Result<Json<IdentificationControlResponse>, LibraryError> {
    identification_control(state, false, caller.0.user_id, body).await
}
