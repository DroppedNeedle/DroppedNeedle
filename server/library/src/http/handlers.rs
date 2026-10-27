//! Library handlers: thin Axum handlers over the library services.
//!
//! Everything that reaches the library stores (synchronous SQLite) or the
//! publisher runs on a blocking thread; only the in-memory root registry
//! answers inline.

use crate::manage::Publishing;
use crate::management::service::Management;
use crate::scans::Scans;
use crate::service::Identification;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

use super::blocking;
use super::error::LibraryError;
use super::models::{
    AddRootBody, AggregateScope, BaselineRestoreBody, BaselineRestoreResponse, IdentifyBody,
    IdentifyResponse, ManageAppliedFile, ManageApplyBody, ManageApplyResponse, ManageFileKind,
    ManageFileView, ManageItemBody, ManageKind, ManagePreviewBody, ManagePreviewResponse,
    ManageTagChange, ManageUndoBody, ManageUndoResponse, PluginFileState, PluginFileView,
    PluginFilesPage, PluginFilesQuery, RootView, RootsResponse, RunDetailResponse, ScanBody,
    ScanFileView, ScanResponse, ScanRunView, ScanRunsResponse, ScanScopeView,
};
use crate::identify::models::IdentifyKind;
use crate::manage::{PreviewItemInput, plain_retag};
use crate::publish::planner::PlanKind;
use crate::scan::models::{EffectivePolicy, ScanRun, ScanScope};
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

fn check_managed_names(items: &[ManageItemBody]) -> Result<(), LibraryError> {
    for item in items {
        for (name, values) in &item.managed_updates {
            if crate::tags::TagField::from_name(name).is_none() {
                return Err(LibraryError::InvalidInput {
                    message: format!("Unknown managed field '{name}'"),
                });
            }
            if values.is_empty() || values.iter().any(|value| value.is_empty()) {
                return Err(LibraryError::InvalidInput {
                    message: format!("Managed field '{name}' needs non-empty values"),
                });
            }
        }
    }
    Ok(())
}

/// Only an admin may move a file or choose tag values. Anyone else gets
/// the plain retag: the album's edition tags, written in place.
fn check_plain_retag(caller: &Principal, plain: bool) -> Result<(), LibraryError> {
    if plain || Admin::try_from(caller.clone()).is_ok() {
        Ok(())
    } else {
        Err(LibraryError::Forbidden {
            message: "Admin role required to move files or set tag values".to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// Roots and scan.
// ---------------------------------------------------------------------------

/// List library roots.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/roots",
    responses(
        (status = 200, description = "Root registry", body = RootsResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_roots(
    State(state): State<Publishing>,
    _caller: User,
) -> Result<Json<RootsResponse>, ApiError> {
    let held = {
        let state = state.clone();
        blocking(move || state.held_publish_bundles()).await?
    };
    let policy_revision = {
        let state = state.clone();
        blocking(move || state.core.policy_revision()).await?
    };
    let registry = state.core.live_registry();
    Ok(Json(RootsResponse {
        roots: registry
            .roots()
            .iter()
            .map(|root| RootView {
                id: root.id.clone(),
                path: root.path.to_string_lossy().into_owned(),
                policy: root.policy,
            })
            .collect(),
        enabled: registry.enabled(),
        policy_revision,
        held_publish_bundles: held,
    }))
}

/// Add a library root. Adding the first root enables the library.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/roots",
    request_body = AddRootBody,
    responses(
        (status = 201, description = "Added root", body = RootView),
        (status = 400, description = "Bad root path or policy"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "Root id exists or recovery blocked"),
    )
)]
pub async fn add_root(
    State(state): State<Scans>,
    _caller: Admin,
    ValidJson(body): ValidJson<AddRootBody>,
) -> Result<(StatusCode, Json<RootView>), ApiError> {
    let policy = body.policy.unwrap_or(EffectivePolicy::Automatic);
    if body.path.is_empty() {
        return Err(ApiError::invalid_input("Root path must not be empty"));
    }
    // Blocking stat plus publish-cell refresh: off the runtime.
    let (root, _) = blocking(move || state.add_root(body.id, body.path, policy)).await?;
    Ok((
        StatusCode::CREATED,
        Json(RootView {
            id: root.id,
            path: root.path.to_string_lossy().into_owned(),
            policy: root.policy,
        }),
    ))
}

/// Trigger a manual scan over one root or every scheduled root.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/scan",
    request_body = ScanBody,
    responses(
        (status = 200, description = "Scan request answer", body = ScanResponse),
        (status = 400, description = "No scopes to scan"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown root id"),
        (status = 409, description = "Library disabled or policy moved"),
    )
)]
pub async fn trigger_scan(
    State(state): State<Scans>,
    caller: Admin,
    ValidJson(body): ValidJson<ScanBody>,
) -> Result<Json<ScanResponse>, ApiError> {
    let user_id = caller.user_id().to_owned();
    let result = blocking(move || state.request_scan(body.root_id.as_deref(), &user_id)).await?;
    Ok(Json(ScanResponse {
        run_id: result.run_id,
        disposition: result.disposition,
        state: result.state,
    }))
}

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
// Identify.
// ---------------------------------------------------------------------------

/// Enqueue one album for identification.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/identify",
    request_body = IdentifyBody,
    responses(
        (status = 200, description = "Enqueue answer", body = IdentifyResponse),
        (status = 400, description = "Bad album id or kind"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn enqueue_identify(
    State(state): State<Identification>,
    caller: Curator,
    ValidJson(body): ValidJson<IdentifyBody>,
) -> Result<Json<IdentifyResponse>, ApiError> {
    if body.album_id.is_empty() {
        return Err(ApiError::invalid_input("Album id must not be empty"));
    }
    let kind = body.kind.unwrap_or(IdentifyKind::Manual);
    let user_id = caller.user_id().to_owned();
    let job = blocking(move || state.enqueue_identify(&body.album_id, kind, &user_id)).await?;
    Ok(Json(IdentifyResponse {
        job_id: job.id,
        album_id: job.local_album_id,
        state: job.state,
    }))
}

// ---------------------------------------------------------------------------
// Management: preview, apply, undo, baseline restore.
// ---------------------------------------------------------------------------

/// Build a sealed management preview (`retag` or `organize`).
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/manage/preview",
    request_body = ManagePreviewBody,
    responses(
        (status = 200, description = "Sealed preview", body = ManagePreviewResponse),
        (status = 400, description = "Bad kind, fields, or items"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required; admin role for anything but a plain retag"),
        (status = 409, description = "Missing identity, collision, no space, or the album's chosen edition is still being placed"),
    )
)]
pub async fn manage_preview(
    State(state): State<Publishing>,
    caller: Curator,
    ValidJson(body): ValidJson<ManagePreviewBody>,
) -> Result<Json<ManagePreviewResponse>, ApiError> {
    let kind = match body.kind {
        ManageKind::Retag => PlanKind::SamePath,
        ManageKind::Organize => PlanKind::Move,
    };
    if body.album_id.is_empty() {
        return Err(ApiError::invalid_input("Album id must not be empty"));
    }
    if body.items.is_empty() {
        return Err(ApiError::invalid_input("Preview needs at least one file"));
    }
    check_managed_names(&body.items)?;
    let inputs: Vec<PreviewItemInput> = body
        .items
        .into_iter()
        .map(|item| PreviewItemInput {
            root_id: item.root_id,
            rel_path: item.rel_path,
            dest_rel: item.dest_rel,
            managed_updates: item.managed_updates.into_iter().collect(),
        })
        .collect();
    check_plain_retag(&caller, plain_retag(kind, &inputs))?;
    let sealed = blocking(move || state.plan_preview(kind, &body.album_id, inputs)).await?;
    Ok(Json(ManagePreviewResponse {
        preview_token: sealed.token,
        expires_day: sealed.expires_day,
        bundle_id: sealed.bundle_id,
        files: sealed
            .files
            .into_iter()
            .map(|file| ManageFileView {
                track_id: file.track_id,
                source: file.source,
                dest: file.dest,
                kind: match file.kind {
                    PlanKind::SamePath => ManageFileKind::SamePath,
                    PlanKind::Move => ManageFileKind::Move,
                },
                changes: file
                    .changes
                    .into_iter()
                    .map(|change| ManageTagChange {
                        field: change.field,
                        from: change.from,
                        to: change.to,
                    })
                    .collect(),
            })
            .collect(),
    }))
}

/// Apply a sealed preview exactly once.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/manage/apply",
    request_body = ManageApplyBody,
    responses(
        (status = 200, description = "Apply answer", body = ManageApplyResponse),
        (status = 400, description = "Missing preview token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required; admin role for anything but a plain retag"),
        (status = 404, description = "Unknown preview token"),
        (status = 409, description = "Stale preview or blocked bundle"),
    )
)]
pub async fn manage_apply(
    State(state): State<Publishing>,
    caller: Curator,
    ValidJson(body): ValidJson<ManageApplyBody>,
) -> Result<Json<ManageApplyResponse>, ApiError> {
    if body.preview_token.is_empty() {
        return Err(ApiError::invalid_input("Preview token must not be empty"));
    }
    // The role is checked before the token is spent, so a refused caller
    // cannot use up someone else's preview.
    let applied = blocking(move || {
        if let Some(plain) = state.preview_is_plain_retag(&body.preview_token) {
            check_plain_retag(&caller, plain)?;
        }
        Ok::<_, LibraryError>(state.apply_preview(&body.preview_token, Some(caller.user_id()))?)
    })
    .await?;
    Ok(Json(ManageApplyResponse {
        bundle_id: applied.bundle_id,
        outcome: applied.outcome,
        files: applied
            .files
            .into_iter()
            .map(|file| ManageAppliedFile {
                track_id: file.track_id,
                root_id: file.root_id,
                rel_path: file.rel_path,
            })
            .collect(),
    }))
}

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
        (status = 200, description = "A page of the plugin's files", body = PluginFilesPage),
        (status = 400, description = "Bad cursor or limit"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_plugin_files(
    State(state): State<Publishing>,
    _caller: Admin,
    ValidQuery(query): ValidQuery<PluginFilesQuery>,
) -> Result<Json<PluginFilesPage>, ApiError> {
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::invalid_input("Limit must be 1 to 100"));
    }
    let before = match query.cursor.as_deref().filter(|raw| !raw.is_empty()) {
        None => None,
        Some(raw) => Some(
            raw.split_once('|')
                .and_then(|(at, id)| Some((at.parse::<i64>().ok()?, id.to_owned())))
                .ok_or_else(|| {
                    ApiError::invalid_input("That cursor is not valid; start from the first page.")
                })?,
        ),
    };
    let mut files = blocking(move || state.plugin_files(&query.plugin, before, limit + 1)).await?;
    let next_cursor = if files.len() > limit as usize {
        files.truncate(limit as usize);
        files
            .last()
            .map(|last| format!("{}|{}", last.added_at, last.bundle_id))
    } else {
        None
    };
    Ok(Json(PluginFilesPage {
        items: files
            .into_iter()
            .map(|added| PluginFileView {
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
            })
            .collect(),
        next_cursor,
    }))
}

/// Restore tracks to their first-management baselines.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/manage/baseline/restore",
    request_body = BaselineRestoreBody,
    responses(
        (status = 200, description = "Restore answer", body = BaselineRestoreResponse),
        (status = 400, description = "No tracks listed"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "Restore blocked for a file"),
    )
)]
pub async fn baseline_restore(
    State(state): State<Publishing>,
    _caller: Admin,
    ValidJson(body): ValidJson<BaselineRestoreBody>,
) -> Result<Json<BaselineRestoreResponse>, ApiError> {
    if body.track_ids.is_empty() {
        return Err(ApiError::invalid_input("Restore needs at least one track"));
    }
    let applied = blocking(move || state.baseline_restore(&body.track_ids)).await?;
    Ok(Json(BaselineRestoreResponse {
        bundle_id: applied.bundle_id,
        outcome: applied.outcome,
        restored: applied
            .files
            .into_iter()
            .map(|file| file.track_id)
            .collect(),
    }))
}
