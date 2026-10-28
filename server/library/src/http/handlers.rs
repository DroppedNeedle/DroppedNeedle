//! Library handlers: thin Axum handlers over the library services.
//!
//! Everything that reaches the library stores (synchronous SQLite) or the
//! publisher runs on a blocking thread; only the in-memory root registry
//! answers inline.

use crate::manage::Publishing;
use crate::management::service::Management;
use crate::scans::Scans;
use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::page::{Page, PageRequest};

use super::blocking;
use super::models::{
    AggregateScope, ManageUndoBody, ManageUndoResponse, PluginFileState, PluginFileView,
    PluginFilesQuery, RunDetailResponse, ScanFileView, ScanRunView, ScanRunsResponse,
    ScanScopeView,
};
use crate::scan::models::{ScanRun, ScanScope};
use droppedneedle_auth::{Admin, Curator, Principal, User};

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

pub(super) fn run_view(run: &ScanRun) -> ScanRunView {
    let terminal = run.terminal_code.map(crate::scan::reasons::scan_reason);
    ScanRunView {
        id: run.id.clone(),
        kind: run.kind,
        trigger: run.trigger,
        state: run.state.scan_state(),
        phase: run.state.phase(),
        aggregate_scope: AggregateScope::from_stored(&run.aggregate_scope),
        counters: run.counters,
        queued_at: run.queued_at,
        started_at: run.started_at,
        updated_at: run.updated_at,
        terminal_at: run.terminal_at,
        terminal_code: run.terminal_code,
        terminal_message: terminal.as_ref().map(|reason| reason.message.to_owned()),
        terminal_action: terminal.as_ref().map(|reason| reason.action.to_owned()),
        requested_by_user_id: run.requested_by_user_id.clone(),
        resume_phase: run.state.resume_phase(),
        requested_control: run.state.requested_control(),
        coalesced_request_count: run.coalesced_request_count,
        row_revision: run.row_revision,
        event_revision: run.event_revision,
        phase_timings: run
            .phase_timings
            .iter()
            .map(|(phase, seconds)| (phase.scan_phase().as_str().to_owned(), *seconds))
            .collect(),
    }
}

/// Run view for `caller`: only administrators see who asked for a run.
fn caller_run_view(caller: &Principal, run: &ScanRun) -> ScanRunView {
    let mut view = run_view(run);
    if !caller.is_admin() {
        view.requested_by_user_id = None;
    }
    view
}

fn scope_view(scope: &ScanScope) -> ScanScopeView {
    ScanScopeView {
        root_id: scope.root_id.clone(),
        relative_path: scope.relative_path.clone(),
        effective_policy: scope.effective_policy,
    }
}

// ---------------------------------------------------------------------------
// Scan runs.
// ---------------------------------------------------------------------------

/// List current plus recent scan runs.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/scan/runs",
    responses(
        (status = 200, description = "Scan runs", body = ScanRunsResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_runs(
    State(state): State<Scans>,
    User(caller): User,
) -> Result<Json<ScanRunsResponse>, ApiError> {
    let (current, history) = blocking(move || state.recent_scan_runs(20)).await?;
    let view = |run: &ScanRun| caller_run_view(&caller, run);
    Ok(Json(ScanRunsResponse {
        current: current.iter().map(view).collect(),
        history: history.iter().map(view).collect(),
    }))
}

/// Get one scan run with its scopes and discovered files.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/scan/runs/{id}",
    params(("id" = String, Path, description = "Scan run id")),
    responses(
        (status = 200, description = "Run detail", body = RunDetailResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown run id"),
    )
)]
pub async fn get_run(
    State(state): State<Scans>,
    User(caller): User,
    Path(id): Path<String>,
) -> Result<Json<RunDetailResponse>, ApiError> {
    let (run, scopes, files) = blocking(move || state.run_detail(&id)).await?;
    Ok(Json(RunDetailResponse {
        run: caller_run_view(&caller, &run),
        scopes: scopes.iter().map(scope_view).collect(),
        files: files
            .iter()
            .map(|file| ScanFileView {
                root_id: file.root_id.clone(),
                relative_path: file.relative_path.clone(),
                track_id: file.local_track_id.clone(),
                verdict: file.comparison_result,
            })
            .collect(),
    }))
}

// ---------------------------------------------------------------------------
// Managed changes: undo and plugin files.
// ---------------------------------------------------------------------------

/// Undo one published bundle as a new operation.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/manage/undo",
    request_body = ManageUndoBody,
    responses(
        (status = 200, description = "Undo answer", body = ManageUndoResponse),
        (status = 400, description = "Missing bundle id"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown bundle id, or one a curator did not apply"),
        (status = 409, description = "Undo blocked for a file"),
    )
)]
pub async fn manage_undo(
    State(state): State<Management>,
    caller: Curator,
    ValidJson(body): ValidJson<ManageUndoBody>,
) -> Result<Json<ManageUndoResponse>, ApiError> {
    if body.bundle_id.is_empty() {
        return Err(ApiError::invalid_input("Bundle id must not be empty"));
    }
    // An admin undoes any bundle; a curator only one they applied.
    let only_by = (!caller.is_admin()).then_some(caller.user_id().to_owned());
    let applied = blocking(move || state.undo_bundle(&body.bundle_id, only_by.as_deref())).await?;
    Ok(Json(ManageUndoResponse {
        bundle_id: applied.bundle_id,
        outcome: applied.outcome,
        restored: applied
            .files
            .into_iter()
            .map(|file| file.track_id)
            .collect(),
    }))
}

/// The files one plugin added to the library, newest first.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/manage/plugin-files",
    params(PluginFilesQuery),
    responses(
        (status = 200, description = "A page of the plugin's files", body = Page<PluginFileView>),
        (status = 400, description = "Bad cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_plugin_files(
    State(state): State<Publishing>,
    _caller: Admin,
    ValidQuery(query): ValidQuery<PluginFilesQuery>,
) -> Result<Json<Page<PluginFileView>>, ApiError> {
    let paging = PageRequest::<(i64, String)>::read(query.limit, query.cursor.as_deref(), 50, 100)?;
    let limit = paging.limit;
    let files =
        blocking(move || state.plugin_files(&query.plugin, paging.after, limit as u32 + 1)).await?;
    let page = Page::cut(files, limit, |last| (last.added_at, last.bundle_id.clone()));
    Ok(Json(page.map(|added| PluginFileView {
        bundle_id: added.bundle_id,
        root_id: added.file.root_id,
        relative_path: added.file.relative_path,
        size: added.file.size,
        added_at: added.added_at,
        state: if added.taken_back {
            PluginFileState::TakenBack
        } else {
            PluginFileState::Added
        },
    })))
}
