//! Library Management job routes: make a preview from a profile, page
//! through its files, apply or discard it, get a new token for it, read
//! the history and results of applied jobs, and preview an undo.
//!
//! Every route is admin-only, as in v2. Store and file work runs on
//! `spawn_blocking`.

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::auth::RequireAdmin;
use super::error::{LibraryError, ValidJson, ValidQuery};
use super::operations::{OperationView, ReasonView, job_view};
use crate::library::management::models::{
    CatalogFilter, Eligibility, HistoryFilter, HistoryItem, ItemFilter, JobDetail, ManagementError,
    PlannedItem, PreviewHandle, ResultItem, Selection, SelectionKind, Summary,
};
use crate::library::management::reasons::ItemReason;
use crate::library::management::service::{ApplyRequest, Management, PreviewRequest};
use crate::library::wiring::LibrarySetup;
use droppedneedle_kernel::runtime_config::sections::LibraryManagementRootOverrides;

/// Management routes, relative for nesting under `/api/v3`.
pub fn management_router() -> Router<LibrarySetup> {
    Router::new()
        .route(
            "/library/management/previews",
            post(create_management_preview),
        )
        .route(
            "/library/management/previews/{job_id}",
            get(get_management_preview),
        )
        .route(
            "/library/management/previews/{job_id}/items",
            get(list_management_preview_items),
        )
        .route(
            "/library/management/previews/{job_id}/items/{ordinal}/artwork/{sha256}",
            get(get_management_preview_artwork),
        )
        .route(
            "/library/management/previews/{job_id}/discard",
            post(discard_management_preview),
        )
        .route(
            "/library/management/previews/{job_id}/apply",
            post(apply_management_preview),
        )
        .route(
            "/library/management/previews/{job_id}/reissue",
            post(reissue_management_preview),
        )
        .route(
            "/library/management/operations",
            get(list_management_operations),
        )
        .route(
            "/library/management/operations/{job_id}",
            get(get_management_operation),
        )
        .route(
            "/library/management/operations/{job_id}/results",
            get(list_management_operation_results),
        )
        .route(
            "/library/management/operations/{job_id}/undo-preview",
            post(create_management_undo_preview),
        )
}

impl From<ManagementError> for LibraryError {
    fn from(error: ManagementError) -> Self {
        match error {
            ManagementError::NotFound(_) => Self::NotFound,
            ManagementError::Invalid(message) => Self::InvalidInput { message },
            ManagementError::Conflict(message) => Self::Conflict { message },
            ManagementError::Forbidden(message) => Self::Forbidden {
                message: message.to_owned(),
            },
            ManagementError::Store(cause) => Self::internal(&cause),
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ManagementError> + Send + 'static,
) -> Result<T, LibraryError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause))?
        .map_err(LibraryError::from)
}

// ---------------------------------------------------------------------------
// DTOs.
// ---------------------------------------------------------------------------

/// What a preview covers.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagementSelectionKind {
    Roots,
    Artists,
    Albums,
    Tracks,
    Filter,
}

/// Catalog filter for a `filter` selection.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ManagementCatalogFilterBody {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub from_year: Option<i64>,
    #[serde(default)]
    pub to_year: Option<i64>,
    #[serde(default)]
    pub artist_ids: Vec<String>,
    #[serde(default)]
    pub album_artist_only: bool,
}

/// A preview's scope: library folders, artists, albums, files, or a
/// catalog filter.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementSelectionBody {
    pub kind: ManagementSelectionKind,
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub catalog_filter: Option<ManagementCatalogFilterBody>,
}

impl From<ManagementSelectionBody> for Selection {
    fn from(body: ManagementSelectionBody) -> Self {
        Self {
            kind: match body.kind {
                ManagementSelectionKind::Roots => SelectionKind::Roots,
                ManagementSelectionKind::Artists => SelectionKind::Artists,
                ManagementSelectionKind::Albums => SelectionKind::Albums,
                ManagementSelectionKind::Tracks => SelectionKind::Tracks,
                ManagementSelectionKind::Filter => SelectionKind::Filter,
            },
            ids: body.ids,
            catalog_filter: body.catalog_filter.map(|filter| CatalogFilter {
                search: filter.search,
                genre: filter.genre,
                from_year: filter.from_year,
                to_year: filter.to_year,
                artist_ids: filter.artist_ids,
                album_artist_only: filter.album_artist_only,
            }),
        }
    }
}

/// Make a preview.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementPreviewCreateBody {
    pub selection: ManagementSelectionBody,
    pub profile_id: String,
    /// The settings revision the page loaded; refused when it moved.
    pub expected_settings_revision: String,
    /// The library policy revision the page loaded; refused when it moved.
    pub expected_policy_revision: String,
    /// Makes a retried request return the same preview.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Move the files into this library folder instead of their own.
    #[serde(default)]
    pub target_root_id: Option<String>,
    /// Switches for this run only, on top of the profile.
    #[serde(default)]
    pub overrides: Option<LibraryManagementRootOverrides>,
}

/// A new preview and the token that applies it. Keep the token: it is
/// shown once (ask for a new one with reissue).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPreviewCreatedResponse {
    pub job_id: String,
    pub preview_token: String,
    pub created_at: f64,
    pub expires_at: f64,
    /// True when the idempotency key named an existing preview.
    pub existing: bool,
}

impl From<PreviewHandle> for ManagementPreviewCreatedResponse {
    fn from(handle: PreviewHandle) -> Self {
        Self {
            job_id: handle.job_id,
            preview_token: handle.preview_token,
            created_at: handle.created_at,
            expires_at: handle.expires_at,
            existing: handle.existing,
        }
    }
}

/// A new token for a ready preview.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPreviewReissueResponse {
    pub job_id: String,
    pub preview_token: String,
    pub created_at: f64,
    pub expires_at: f64,
}

/// What a preview counted.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementSummaryView {
    pub selected_item_count: Option<i64>,
    pub item_count: i64,
    pub bundle_count: i64,
    pub eligible_count: i64,
    pub warning_count: i64,
    pub blocked_count: i64,
    pub stale_count: i64,
    pub no_change_count: i64,
    pub tag_change_count: i64,
    pub artwork_change_count: i64,
    pub path_change_count: i64,
    pub sidecar_change_count: i64,
    pub estimated_temporary_bytes: i64,
    pub expanded_track_count: i64,
    /// Files per reason code.
    pub reasons: BTreeMap<String, i64>,
    /// Files per library folder.
    pub roots: BTreeMap<String, i64>,
    /// Files per audio format.
    pub formats: BTreeMap<String, i64>,
    pub deferred_sources: BTreeMap<String, i64>,
    pub metadata_snapshot_ids: Vec<String>,
}

impl From<Summary> for ManagementSummaryView {
    fn from(summary: Summary) -> Self {
        Self {
            selected_item_count: summary.selected_item_count,
            item_count: summary.item_count,
            bundle_count: summary.bundle_count,
            eligible_count: summary.eligible_count,
            warning_count: summary.warning_count,
            blocked_count: summary.blocked_count,
            stale_count: summary.stale_count,
            no_change_count: summary.no_change_count,
            tag_change_count: summary.tag_change_count,
            artwork_change_count: summary.artwork_change_count,
            path_change_count: summary.path_change_count,
            sidecar_change_count: summary.sidecar_change_count,
            estimated_temporary_bytes: summary.estimated_temporary_bytes,
            expanded_track_count: summary.expanded_track_count,
            reasons: summary.reasons,
            roots: summary.roots,
            formats: summary.formats,
            deferred_sources: summary.deferred_sources,
            metadata_snapshot_ids: summary.metadata_snapshot_ids,
        }
    }
}

/// A management job: a preview, an apply in progress, or a finished one.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementJobView {
    pub job_id: String,
    /// The operation state: `queued`, `running`, `paused`, `ready`,
    /// `succeeded`, `failed`, `cancelled`, or `stopped`.
    pub state: String,
    /// `planning`, `ready`, `applying`, `undoing`, or `complete`.
    pub phase: String,
    /// `preview`, `apply`, or `undo` (v2 history may show others).
    pub mode: String,
    pub origin: String,
    pub profile_id: String,
    pub profile_name: String,
    pub profile_revision: String,
    pub settings_revision: String,
    pub policy_revision: String,
    pub catalog_revision: i64,
    pub proposed_settings_revision: Option<String>,
    pub target_root_id: Option<String>,
    /// For an undo: the operation it puts back.
    pub linked_job_id: Option<String>,
    #[schema(value_type = Object)]
    pub selection: serde_json::Value,
    pub summary: ManagementSummaryView,
    pub created_at: f64,
    pub updated_at: f64,
    pub expires_at: Option<f64>,
    pub expired: bool,
    pub stale: bool,
    /// Why the preview can no longer be applied, in words.
    pub stale_reasons: Vec<String>,
    pub stale_input_count: i64,
    pub stale_sample_relative_paths: Vec<String>,
    pub ready_for_confirmation: bool,
    /// Echo this on apply, discard and undo preview.
    pub operation_row_revision: i64,
    pub operation_event_revision: i64,
    pub terminal_code: Option<String>,
    pub terminal_reason: Option<ReasonView>,
    pub worker_heartbeat_at: Option<f64>,
    pub worker_lease_expires_at: Option<f64>,
    pub worker_stalled: bool,
    pub expected_work_count: i64,
    pub completed_count: i64,
    pub succeeded_count: i64,
    pub failed_count: i64,
    pub skipped_count: i64,
    pub control_request: String,
    /// Files whose change can still be undone.
    pub undo_available_count: i64,
    pub undo_expired_count: i64,
    pub undo_expires_at: Option<f64>,
    pub baseline_available_count: i64,
    #[schema(value_type = Vec<Object>)]
    pub external_refreshes: Vec<serde_json::Value>,
}

impl From<JobDetail> for ManagementJobView {
    fn from(detail: JobDetail) -> Self {
        let job = detail.job;
        Self {
            job_id: job.id.clone(),
            state: job.state.as_str().to_owned(),
            phase: detail.phase,
            mode: detail.mode,
            origin: detail.origin,
            profile_id: detail.profile_id,
            profile_name: detail.profile_name,
            profile_revision: detail.profile_revision,
            settings_revision: detail.settings_revision,
            policy_revision: detail.policy_revision,
            catalog_revision: detail.catalog_revision,
            proposed_settings_revision: detail.proposed_settings_revision,
            target_root_id: detail.target_root_id,
            linked_job_id: detail.linked_job_id,
            selection: detail.selection,
            summary: detail.summary.into(),
            created_at: job.created_at,
            updated_at: job.updated_at,
            expires_at: detail.expires_at,
            expired: detail.expired,
            stale: !detail.stale_reasons.is_empty(),
            stale_reasons: detail.stale_reasons,
            stale_input_count: 0,
            stale_sample_relative_paths: Vec::new(),
            ready_for_confirmation: detail.ready_for_confirmation,
            operation_row_revision: job.row_revision,
            operation_event_revision: job.event_revision,
            terminal_reason: job
                .terminal_code
                .as_deref()
                .map(|code| crate::library::operations::reasons::terminal(code).into()),
            terminal_code: job.terminal_code.clone(),
            worker_heartbeat_at: detail.heartbeat_at,
            worker_lease_expires_at: detail.lease_expires_at,
            worker_stalled: detail.worker_stalled,
            expected_work_count: job.expected_work_count,
            completed_count: job.completed_count,
            succeeded_count: job.succeeded_count,
            failed_count: job.failed_count,
            skipped_count: job.skipped_count,
            control_request: job.control_request.as_str().to_owned(),
            undo_available_count: detail.undo_available_count,
            undo_expired_count: detail.undo_expired_count,
            undo_expires_at: detail.undo_expires_at,
            baseline_available_count: detail.baseline_available_count,
            external_refreshes: detail.external_refreshes,
        }
    }
}

/// Why a file is left alone or failed: a stable code, a plain sentence,
/// and what to do about it. v2 history carries only the code.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementReasonView {
    pub code: String,
    pub message: String,
    pub action: String,
}

impl From<ItemReason> for ManagementReasonView {
    fn from(reason: ItemReason) -> Self {
        Self {
            code: reason.code,
            message: reason.message,
            action: reason.action,
        }
    }
}

/// One planned file.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPlanItemView {
    pub ordinal: i64,
    /// The album this file is planned with; albums apply as a whole.
    pub bundle_ordinal: i64,
    pub local_album_id: Option<String>,
    pub local_track_id: Option<String>,
    pub source_root_id: Option<String>,
    pub source_relative_path: Option<String>,
    pub destination_root_id: Option<String>,
    pub destination_relative_path: Option<String>,
    /// `eligible`, `warning`, `blocked`, or `stale`.
    pub eligibility: String,
    pub reason_code: Option<String>,
    /// The reason in words, with what to do about it.
    pub reason: Option<ManagementReasonView>,
    pub estimated_temporary_bytes: i64,
    /// `fields`: the tags the file ends up with, `[{name, value}]`.
    #[schema(value_type = Object)]
    pub desired_document: serde_json::Value,
    #[schema(value_type = Vec<Object>)]
    pub artwork_choices: Vec<serde_json::Value>,
    /// `field_mutations` (`{name, operation, before, after}`),
    /// `tags_changed`, `path_changed`, `sidecars`.
    #[schema(value_type = Object)]
    pub diff: serde_json::Value,
    /// `audio_format`, `warnings`, `blockers`, and catalog facts.
    #[schema(value_type = Object)]
    pub capability: serde_json::Value,
    #[schema(value_type = Vec<Object>)]
    pub collisions: Vec<serde_json::Value>,
}

impl From<PlannedItem> for ManagementPlanItemView {
    fn from(item: PlannedItem) -> Self {
        let reason = item.facts.reason.clone();
        let fields = serde_json::to_value(&item.desired.fields).unwrap_or_default();
        let artwork_choices = item
            .desired
            .artwork
            .iter()
            .filter_map(|choice| serde_json::to_value(choice).ok())
            .collect();
        Self {
            ordinal: item.ordinal,
            bundle_ordinal: item.bundle_ordinal,
            local_album_id: (!item.album_id.is_empty()).then_some(item.album_id),
            local_track_id: item.track_id,
            source_root_id: Some(item.source_root),
            source_relative_path: Some(item.source_rel),
            destination_root_id: item.dest_root,
            destination_relative_path: item.dest_rel,
            eligibility: item.eligibility.as_str().to_owned(),
            reason_code: reason.as_ref().map(|reason| reason.code.clone()),
            reason: reason
                .filter(|reason| !reason.message.is_empty())
                .map(Into::into),
            estimated_temporary_bytes: i64::try_from(item.estimated_bytes).unwrap_or(i64::MAX),
            desired_document: serde_json::json!({ "fields": fields }),
            artwork_choices,
            diff: serde_json::to_value(&item.diff).unwrap_or_default(),
            capability: serde_json::to_value(&item.facts).unwrap_or_default(),
            collisions: Vec::new(),
        }
    }
}

/// One page of planned files.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPlanItemPage {
    pub items: Vec<ManagementPlanItemView>,
    pub next_after_ordinal: Option<i64>,
    pub has_more: bool,
}

/// One file after apply.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementResultItemView {
    pub plan: ManagementPlanItemView,
    /// `pending`, `running`, `succeeded`, `failed`, or `not_scheduled`.
    pub work_state: String,
    pub failure_code: Option<String>,
    /// Why the file failed or was not written, with what to do about it.
    pub failure_reason: Option<ManagementReasonView>,
    #[schema(value_type = Object)]
    pub result: serde_json::Value,
    pub journal_states: Vec<String>,
}

impl From<ResultItem> for ManagementResultItemView {
    fn from(item: ResultItem) -> Self {
        Self {
            plan: item.plan.into(),
            work_state: item.work_state,
            failure_code: item.failure.as_ref().map(|reason| reason.code.clone()),
            failure_reason: item
                .failure
                .filter(|reason| !reason.message.is_empty())
                .map(Into::into),
            result: item.result,
            journal_states: Vec::new(),
        }
    }
}

/// One page of results.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementResultPage {
    pub items: Vec<ManagementResultItemView>,
    pub next_after_ordinal: Option<i64>,
    pub has_more: bool,
}

/// One row of management history.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementHistoryItemView {
    pub operation: OperationView,
    pub mode: String,
    pub origin: String,
    pub phase: String,
    pub profile_id: String,
    pub profile_name: String,
    pub profile_revision: String,
    pub target_root_id: Option<String>,
    pub activation_preview: bool,
    #[schema(value_type = Object)]
    pub selection: serde_json::Value,
    pub eligible_count: i64,
    pub warning_count: i64,
    pub blocked_count: i64,
    pub expires_at: Option<f64>,
}

impl From<HistoryItem> for ManagementHistoryItemView {
    fn from(item: HistoryItem) -> Self {
        Self {
            operation: job_view(&item.job),
            mode: item.mode,
            origin: item.origin,
            phase: item.phase,
            profile_id: item.profile_id,
            profile_name: item.profile_name,
            profile_revision: item.profile_revision,
            target_root_id: item.target_root_id,
            activation_preview: false,
            selection: item.selection,
            eligible_count: item.summary.eligible_count,
            warning_count: item.summary.warning_count,
            blocked_count: item.summary.blocked_count,
            expires_at: item.expires_at,
        }
    }
}

/// One page of history, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementHistoryPage {
    pub items: Vec<ManagementHistoryItemView>,
    /// Pass back as `cursor` for the next page; null on the last.
    pub next_cursor: Option<String>,
}

/// Paging through a job's files.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ManagementItemsQuery {
    /// Return files after this ordinal (-1 for the first page).
    pub after_ordinal: Option<i64>,
    /// Files per page, 1 to 200 (default 100).
    pub limit: Option<i64>,
    /// `eligible`, `warning`, `blocked`, or `stale`.
    pub eligibility: Option<String>,
    pub reason_code: Option<String>,
    pub root_id: Option<String>,
    pub album_id: Option<String>,
    pub audio_format: Option<String>,
    /// `tags`, `path`, `sidecars`, or `no_change`.
    pub change_kind: Option<String>,
}

/// Paging through a job's results.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ManagementResultsQuery {
    pub after_ordinal: Option<i64>,
    pub limit: Option<i64>,
}

/// History filters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ManagementHistoryQuery {
    /// Rows per page, 1 to 50 (default 50).
    pub limit: Option<i64>,
    /// Cursor from the previous page.
    pub cursor: Option<String>,
    pub origin: Option<String>,
    pub profile_id: Option<String>,
    pub root_id: Option<String>,
    pub state: Option<String>,
    pub mode: Option<String>,
    pub created_from: Option<f64>,
    pub created_to: Option<f64>,
}

/// Apply a ready preview.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementApplyBody {
    pub preview_token: String,
    pub expected_operation_row_revision: i64,
    pub idempotency_key: String,
    /// Must be true: the administrator confirmed the change.
    #[serde(default)]
    pub confirmation: bool,
}

/// Discard a preview.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementDiscardBody {
    pub expected_operation_row_revision: i64,
}

/// Preview undoing an applied job.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementUndoPreviewBody {
    pub expected_operation_row_revision: i64,
    pub idempotency_key: String,
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Make a management preview. Planning runs in the background; poll the
/// preview until it is ready.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/previews",
    request_body = ManagementPreviewCreateBody,
    responses(
        (status = 200, description = "Preview queued", body = ManagementPreviewCreatedResponse),
        (status = 400, description = "Bad selection, profile or destination"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "Settings or library policy changed since the page loaded"),
    )
)]
pub async fn create_management_preview(
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    ValidJson(body): ValidJson<ManagementPreviewCreateBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, LibraryError> {
    let management = Management::new(&state);
    let request = PreviewRequest {
        selection: body.selection.into(),
        profile_id: body.profile_id,
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        idempotency_key: body.idempotency_key.filter(|key| !key.trim().is_empty()),
        target_root_id: body.target_root_id.filter(|root| !root.is_empty()),
        overrides: body.overrides,
    };
    let handle = blocking(move || management.create_preview(request, &caller.user_id)).await?;
    Ok(Json(handle.into()))
}

/// One preview with its summary.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/previews/{job_id}",
    params(("job_id" = String, Path, description = "Management job id")),
    responses(
        (status = 200, description = "Preview", body = ManagementJobView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
    )
)]
pub async fn get_management_preview(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
) -> Result<Json<ManagementJobView>, LibraryError> {
    let management = Management::new(&state);
    let detail = blocking(move || management.detail(&job_id)).await?;
    Ok(Json(detail.into()))
}

fn item_filter(query: &ManagementItemsQuery) -> Result<ItemFilter, LibraryError> {
    let eligibility = match query.eligibility.as_deref() {
        None => None,
        Some("eligible") => Some(Eligibility::Eligible),
        Some("warning") => Some(Eligibility::Warning),
        Some("blocked") => Some(Eligibility::Blocked),
        Some("stale") => Some(Eligibility::Stale),
        Some(_) => {
            return Err(LibraryError::InvalidInput {
                message: "eligibility must be eligible, warning, blocked or stale".to_owned(),
            });
        }
    };
    if let Some(kind) = query.change_kind.as_deref()
        && !matches!(kind, "tags" | "path" | "sidecars" | "no_change" | "artwork")
    {
        return Err(LibraryError::InvalidInput {
            message: "change_kind must be tags, path, sidecars or no_change".to_owned(),
        });
    }
    Ok(ItemFilter {
        eligibility,
        reason_code: query.reason_code.clone(),
        root_id: query.root_id.clone(),
        album_id: query.album_id.clone(),
        audio_format: query.audio_format.clone(),
        change_kind: query.change_kind.clone(),
    })
}

/// A page of the files a preview planned.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/previews/{job_id}/items",
    params(("job_id" = String, Path, description = "Management job id"), ManagementItemsQuery),
    responses(
        (status = 200, description = "Planned files", body = ManagementPlanItemPage),
        (status = 400, description = "Bad filter"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
    )
)]
pub async fn list_management_preview_items(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidQuery(query): ValidQuery<ManagementItemsQuery>,
) -> Result<Json<ManagementPlanItemPage>, LibraryError> {
    let filter = item_filter(&query)?;
    let management = Management::new(&state);
    let after = query.after_ordinal.unwrap_or(-1);
    let limit = query.limit.unwrap_or(100);
    let page = blocking(move || management.items(&job_id, after, limit, &filter)).await?;
    Ok(Json(ManagementPlanItemPage {
        items: page.items.into_iter().map(Into::into).collect(),
        next_after_ordinal: page.next_after_ordinal,
        has_more: page.has_more,
    }))
}

/// The cover art one planned file embeds or writes, as the preview lists
/// it by hash.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/previews/{job_id}/items/{ordinal}/artwork/{sha256}",
    params(
        ("job_id" = String, Path, description = "Management job id"),
        ("ordinal" = i64, Path, description = "Planned file ordinal"),
        ("sha256" = String, Path, description = "Artwork hash from artwork_choices"),
    ),
    responses(
        (status = 200, description = "The planned image", content_type = "image/jpeg"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job, file or artwork"),
    )
)]
pub async fn get_management_preview_artwork(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path((job_id, ordinal, sha256)): Path<(String, i64, String)>,
) -> Result<axum::response::Response, LibraryError> {
    use axum::http::{HeaderValue, header};
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LibraryError::from(ManagementError::NotFound(
            "That artwork is not in the preview.",
        )));
    }
    let management = Management::new(&state);
    let (bytes, mime) =
        blocking(move || management.preview_artwork(&job_id, ordinal, &sha256)).await?;
    let mut response = axum::response::Response::new(axum::body::Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// Set a preview aside without changing any file.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/previews/{job_id}/discard",
    params(("job_id" = String, Path, description = "Management job id")),
    request_body = ManagementDiscardBody,
    responses(
        (status = 200, description = "Discarded preview", body = ManagementJobView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "Stale revision or already applying"),
    )
)]
pub async fn discard_management_preview(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<ManagementDiscardBody>,
) -> Result<Json<ManagementJobView>, LibraryError> {
    let management = Management::new(&state);
    let detail =
        blocking(move || management.discard(&job_id, body.expected_operation_row_revision)).await?;
    Ok(Json(detail.into()))
}

/// Apply a ready preview: its albums are written one at a time in the
/// background. Watch the operation for progress.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/previews/{job_id}/apply",
    params(("job_id" = String, Path, description = "Management job id")),
    request_body = ManagementApplyBody,
    responses(
        (status = 200, description = "Apply queued", body = OperationView),
        (status = 400, description = "Not confirmed, or wrong token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "Preview expired, stale, or nothing to write"),
    )
)]
pub async fn apply_management_preview(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<ManagementApplyBody>,
) -> Result<Json<OperationView>, LibraryError> {
    let management = Management::new(&state);
    let request = ApplyRequest {
        preview_token: body.preview_token,
        expected_row_revision: body.expected_operation_row_revision,
        idempotency_key: body.idempotency_key,
        confirmation: body.confirmation,
    };
    let job = blocking(move || management.apply(&job_id, request)).await?;
    Ok(Json(job_view(&job)))
}

/// A new confirmation token for your own ready preview (the old one
/// stops working).
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/previews/{job_id}/reissue",
    params(("job_id" = String, Path, description = "Management job id")),
    responses(
        (status = 200, description = "New token", body = ManagementPreviewReissueResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required, or another administrator's preview"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "Preview no longer ready"),
    )
)]
pub async fn reissue_management_preview(
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    Path(job_id): Path<String>,
) -> Result<Json<ManagementPreviewReissueResponse>, LibraryError> {
    let management = Management::new(&state);
    let handle = blocking(move || management.reissue(&job_id, &caller.user_id)).await?;
    Ok(Json(ManagementPreviewReissueResponse {
        job_id: handle.job_id,
        preview_token: handle.preview_token,
        created_at: handle.created_at,
        expires_at: handle.expires_at,
    }))
}

fn history_cursor(raw: Option<&str>) -> Result<Option<(f64, String)>, LibraryError> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let bad = || LibraryError::InvalidInput {
        message: "That history cursor is not valid; start from the first page.".to_owned(),
    };
    let (at, id) = raw.split_once('|').ok_or_else(bad)?;
    let at: f64 = at.parse().map_err(|_| bad())?;
    Ok(Some((at, id.to_owned())))
}

/// Management history, newest first.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/operations",
    params(ManagementHistoryQuery),
    responses(
        (status = 200, description = "History page", body = ManagementHistoryPage),
        (status = 400, description = "Bad cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_management_operations(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    ValidQuery(query): ValidQuery<ManagementHistoryQuery>,
) -> Result<Json<ManagementHistoryPage>, LibraryError> {
    let filter = HistoryFilter {
        limit: query.limit.unwrap_or(50),
        cursor: history_cursor(query.cursor.as_deref())?,
        origin: query.origin,
        profile_id: query.profile_id,
        root_id: query.root_id,
        state: query.state,
        mode: query.mode,
        created_from: query.created_from,
        created_to: query.created_to,
    };
    let management = Management::new(&state);
    let (items, next_cursor) = blocking(move || management.history(filter)).await?;
    Ok(Json(ManagementHistoryPage {
        items: items.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

/// One management operation.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/operations/{job_id}",
    params(("job_id" = String, Path, description = "Management job id")),
    responses(
        (status = 200, description = "Operation", body = ManagementJobView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
    )
)]
pub async fn get_management_operation(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(job_id): Path<String>,
) -> Result<Json<ManagementJobView>, LibraryError> {
    get_management_preview(State(state), caller, Path(job_id)).await
}

/// A page of an applied operation's files with their outcome.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/operations/{job_id}/results",
    params(("job_id" = String, Path, description = "Management job id"), ManagementResultsQuery),
    responses(
        (status = 200, description = "Results", body = ManagementResultPage),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
    )
)]
pub async fn list_management_operation_results(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidQuery(query): ValidQuery<ManagementResultsQuery>,
) -> Result<Json<ManagementResultPage>, LibraryError> {
    let management = Management::new(&state);
    let after = query.after_ordinal.unwrap_or(-1);
    let limit = query.limit.unwrap_or(100);
    let page = blocking(move || management.results(&job_id, after, limit)).await?;
    Ok(Json(ManagementResultPage {
        items: page.items.into_iter().map(Into::into).collect(),
        next_after_ordinal: page.next_after_ordinal,
        has_more: page.has_more,
    }))
}

/// Preview undoing an applied operation. Apply the undo preview like any
/// other.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/operations/{job_id}/undo-preview",
    params(("job_id" = String, Path, description = "Management job id")),
    request_body = ManagementUndoPreviewBody,
    responses(
        (status = 200, description = "Undo preview queued", body = ManagementPreviewCreatedResponse),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "Not a finished apply, or nothing to undo"),
    )
)]
pub async fn create_management_undo_preview(
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<ManagementUndoPreviewBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, LibraryError> {
    let management = Management::new(&state);
    let handle = blocking(move || {
        management.undo_preview(
            &job_id,
            body.expected_operation_row_revision,
            &body.idempotency_key,
            &caller.user_id,
        )
    })
    .await?;
    Ok(Json(handle.into()))
}
