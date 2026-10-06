//! Library handlers: thin Axum handlers over [`LibrarySetup`].
//!
//! Everything that reaches the library stores (synchronous SQLite) or the
//! publisher runs on `spawn_blocking`; only the in-memory root registry
//! answers inline.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use super::auth::{Principal, RequireAdmin, RequireCurator};
use super::error::{LibraryError, ValidJson, ValidQuery};
use super::models::{
    AddRootBody, ApproveBody, BaselineRestoreBody, BaselineRestoreResponse, CandidateView,
    IdentifyBody, IdentifyResponse, IdentityView, ManageAppliedFile, ManageApplyBody,
    ManageApplyResponse, ManageFileView, ManageItemBody, ManagePreviewBody, ManagePreviewResponse,
    ManageUndoBody, ManageUndoResponse, PenaltyView, ReviewResolveResponse, ReviewView,
    ReviewsResponse, RootView, RootsResponse, RunDetailResponse, ScanBody, ScanFileView,
    ScanResponse, ScanRunView, ScanRunsResponse, ScanScopeView, snake,
};
use crate::library::identify::models::{CandidateEvidence, IdentifyKind, JobState, ReviewItem};
use crate::library::manage::PreviewItemInput;
use crate::library::publish::planner::PlanKind;
use crate::library::scan::models::{EffectivePolicy, ScanRun, ScanScope};
use crate::library::wiring::LibrarySetup;

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

/// Run store-backed service work on a blocking thread: the library stores
/// are synchronous SQLite and must not stall the async workers.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, LibraryError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause))
}

fn policy_label(policy: EffectivePolicy) -> String {
    match policy {
        EffectivePolicy::Automatic => "automatic".to_owned(),
        EffectivePolicy::LocalMetadata => "local_metadata".to_owned(),
        EffectivePolicy::Excluded => "excluded".to_owned(),
    }
}

fn parse_policy(raw: Option<&str>) -> Result<EffectivePolicy, LibraryError> {
    match raw.unwrap_or("automatic") {
        "automatic" => Ok(EffectivePolicy::Automatic),
        "local_metadata" => Ok(EffectivePolicy::LocalMetadata),
        "excluded" => Ok(EffectivePolicy::Excluded),
        other => Err(LibraryError::InvalidInput {
            message: format!("Unknown policy '{other}'"),
        }),
    }
}

pub(super) fn run_view(run: &ScanRun) -> ScanRunView {
    let terminal = run
        .terminal_code
        .as_deref()
        .map(crate::library::scan::reasons::scan_reason);
    ScanRunView {
        id: run.id.clone(),
        kind: snake(&run.kind),
        trigger: snake(&run.trigger),
        state: snake(&run.state),
        phase: snake(&run.phase),
        aggregate_scope: run.aggregate_scope.clone(),
        counters: run.counters.clone(),
        queued_at: run.queued_at,
        started_at: run.started_at,
        updated_at: run.updated_at,
        terminal_at: run.terminal_at,
        terminal_code: run.terminal_code.clone(),
        terminal_message: terminal.as_ref().map(|reason| reason.message.to_owned()),
        terminal_action: terminal.as_ref().map(|reason| reason.action.to_owned()),
        requested_by_user_id: run.requested_by_user_id.clone(),
        resume_phase: run.resume_phase.as_ref().map(snake),
        requested_control: snake(&run.requested_control),
        coalesced_request_count: run.coalesced_request_count,
        row_revision: run.row_revision,
        event_revision: run.event_revision,
        phase_timings: run.phase_timings.clone(),
    }
}

/// Run view for `caller`: only administrators see who asked for a run.
fn caller_run_view(caller: &Principal, run: &ScanRun) -> ScanRunView {
    let mut view = run_view(run);
    if !caller.role.is_admin() {
        view.requested_by_user_id = None;
    }
    view
}

fn scope_view(scope: &ScanScope) -> ScanScopeView {
    ScanScopeView {
        root_id: scope.root_id.clone(),
        relative_path: scope.relative_path.clone(),
        effective_policy: policy_label(scope.effective_policy),
    }
}

fn candidate_view(candidate: &CandidateEvidence) -> CandidateView {
    CandidateView {
        candidate_key: candidate.candidate_key.clone(),
        release_group_mbid: candidate.release_group_mbid.clone(),
        release_mbid: candidate.release_mbid.clone(),
        album_title: candidate.album_title.clone(),
        album_artist_name: candidate.album_artist_name.clone(),
        score: candidate.score,
        distance: candidate.distance,
        penalties: candidate
            .penalties
            .iter()
            .map(|penalty| PenaltyView {
                name: penalty.name.clone(),
                share: penalty.share,
            })
            .collect(),
        reason_code: candidate.reason_code.clone(),
        supported_tracks: candidate.supported_count(),
        contradictory_tracks: candidate.contradictory_count(),
    }
}

fn review_view(review: &ReviewItem) -> ReviewView {
    ReviewView {
        id: review.id.clone(),
        album_id: review.local_album_id.clone(),
        reason_code: review.reason_code.clone(),
        state: snake(&review.state),
        resolved_by: review.resolved_by_user_id.clone(),
        selected_candidate_key: review.selected_candidate_key.clone(),
        candidates: review.candidates.iter().map(candidate_view).collect(),
    }
}

fn identity_view(identity: &crate::library::identify::models::AlbumIdentity) -> IdentityView {
    IdentityView {
        release_mbid: identity.release_mbid.clone(),
        release_group_mbid: identity.release_group_mbid.clone(),
        decision_source: snake(&identity.decision_source),
    }
}

fn parse_identify_kind(raw: Option<&str>) -> Result<IdentifyKind, LibraryError> {
    match raw.unwrap_or("manual") {
        "automatic" => Ok(IdentifyKind::Automatic),
        "manual" => Ok(IdentifyKind::Manual),
        "historical" => Ok(IdentifyKind::Historical),
        other => Err(LibraryError::InvalidInput {
            message: format!("Unknown identify kind '{other}'"),
        }),
    }
}

fn check_managed_names(items: &[ManageItemBody]) -> Result<(), LibraryError> {
    for item in items {
        for (name, values) in &item.managed_updates {
            if crate::library::tags::TagField::from_name(name).is_none() {
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

// ---------------------------------------------------------------------------
// Roots and scan.
// ---------------------------------------------------------------------------

/// List library roots.
#[utoipa::path(
    get,
    path = "/api/v3/library/roots",
    responses(
        (status = 200, description = "Root registry", body = RootsResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_roots(
    State(state): State<LibrarySetup>,
    _caller: Principal,
) -> Result<Json<RootsResponse>, LibraryError> {
    let held = {
        let state = state.clone();
        blocking(move || state.held_publish_bundles()).await??
    };
    let registry = state.live_registry();
    Ok(Json(RootsResponse {
        roots: registry
            .roots()
            .iter()
            .map(|root| RootView {
                id: root.id.clone(),
                path: root.path.to_string_lossy().into_owned(),
                policy: policy_label(root.policy),
            })
            .collect(),
        enabled: registry.enabled(),
        policy_revision: registry.policy_revision().to_owned(),
        held_publish_bundles: held,
    }))
}

/// Add a library root. Adding the first root enables the library.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    ValidJson(body): ValidJson<AddRootBody>,
) -> Result<(StatusCode, Json<RootView>), LibraryError> {
    let policy = parse_policy(body.policy.as_deref())?;
    if body.path.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Root path must not be empty".to_owned(),
        });
    }
    // Blocking stat plus publish-cell refresh: off the runtime.
    let (root, _) = tokio::task::spawn_blocking(move || state.add_root(body.id, body.path, policy))
        .await
        .map_err(|cause| LibraryError::internal(&cause))??;
    Ok((
        StatusCode::CREATED,
        Json(RootView {
            id: root.id,
            path: root.path.to_string_lossy().into_owned(),
            policy: policy_label(root.policy),
        }),
    ))
}

/// Trigger a manual scan over one root or every scheduled root.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<ScanBody>,
) -> Result<Json<ScanResponse>, LibraryError> {
    let user_id = caller.0.user_id;
    let result = blocking(move || state.request_scan(body.root_id.as_deref(), &user_id)).await??;
    Ok(Json(ScanResponse {
        run_id: result.run_id,
        disposition: snake(&result.disposition),
        state: snake(&result.state),
    }))
}

/// List current plus recent scan runs.
#[utoipa::path(
    get,
    path = "/api/v3/library/scan/runs",
    responses(
        (status = 200, description = "Scan runs", body = ScanRunsResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_runs(
    State(state): State<LibrarySetup>,
    caller: Principal,
) -> Result<Json<ScanRunsResponse>, LibraryError> {
    let (current, history) =
        blocking(move || (state.coordinator.current(), state.coordinator.history(20))).await?;
    let view = |run: &ScanRun| caller_run_view(&caller, run);
    Ok(Json(ScanRunsResponse {
        current: current.iter().map(view).collect(),
        history: history.iter().map(view).collect(),
    }))
}

/// Get one scan run with its scopes and discovered files.
#[utoipa::path(
    get,
    path = "/api/v3/library/scan/runs/{id}",
    params(("id" = String, Path, description = "Scan run id")),
    responses(
        (status = 200, description = "Run detail", body = RunDetailResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown run id"),
    )
)]
pub async fn get_run(
    State(state): State<LibrarySetup>,
    caller: Principal,
    Path(id): Path<String>,
) -> Result<Json<RunDetailResponse>, LibraryError> {
    let (run, scopes, files) = blocking(move || state.run_detail(&id)).await??;
    Ok(Json(RunDetailResponse {
        run: caller_run_view(&caller, &run),
        scopes: scopes.iter().map(scope_view).collect(),
        files: files
            .iter()
            .map(|file| ScanFileView {
                root_id: file.root_id.clone(),
                relative_path: file.relative_path.clone(),
                track_id: file.local_track_id.clone(),
                verdict: snake(&file.comparison_result),
            })
            .collect(),
    }))
}

// ---------------------------------------------------------------------------
// Identify and reviews.
// ---------------------------------------------------------------------------

/// Enqueue one album for identification.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    ValidJson(body): ValidJson<IdentifyBody>,
) -> Result<Json<IdentifyResponse>, LibraryError> {
    if body.album_id.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Album id must not be empty".to_owned(),
        });
    }
    let kind = parse_identify_kind(body.kind.as_deref())?;
    let user_id = caller.0.user_id;
    let job = blocking(move || state.enqueue_identify(&body.album_id, kind, &user_id)).await??;
    let state_label: String = match job.state {
        JobState::Queued => "queued".to_owned(),
        other => snake(&other),
    };
    Ok(Json(IdentifyResponse {
        job_id: job.id,
        album_id: job.local_album_id,
        state: state_label,
    }))
}

/// Pending reviews for one album.
#[utoipa::path(
    get,
    path = "/api/v3/library/reviews",
    params(("album_id" = String, Query, description = "Local album id")),
    responses(
        (status = 200, description = "Pending reviews", body = ReviewsResponse),
        (status = 400, description = "Missing album id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_reviews(
    State(state): State<LibrarySetup>,
    _caller: Principal,
    ValidQuery(query): ValidQuery<std::collections::HashMap<String, String>>,
) -> Result<Json<ReviewsResponse>, LibraryError> {
    let Some(album_id) = query.get("album_id").cloned() else {
        return Err(LibraryError::InvalidInput {
            message: "Query needs album_id".to_owned(),
        });
    };
    let reviews = blocking(move || state.pending_reviews(&album_id)).await?;
    Ok(Json(ReviewsResponse {
        reviews: reviews.iter().map(review_view).collect(),
    }))
}

/// Approve a review with the curator's chosen candidate.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/approve",
    params(("id" = String, Path, description = "Review id")),
    request_body = ApproveBody,
    responses(
        (status = 200, description = "Settled review", body = ReviewResolveResponse),
        (status = 400, description = "Missing candidate key"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review or candidate"),
        (status = 409, description = "Review already settled"),
    )
)]
pub async fn approve_review(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ApproveBody>,
) -> Result<Json<ReviewResolveResponse>, LibraryError> {
    if body.candidate_key.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Candidate key must not be empty".to_owned(),
        });
    }
    let user_id = caller.0.user_id;
    let (review, identity) =
        blocking(move || state.approve_review(&id, &user_id, &body.candidate_key)).await??;
    Ok(Json(ReviewResolveResponse {
        review: review_view(&review),
        identity: identity.as_ref().map(identity_view),
    }))
}

/// Reject a review: the album keeps its tags, nothing seals.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/reject",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "Settled review", body = ReviewResolveResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled"),
    )
)]
pub async fn reject_review(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(id): Path<String>,
) -> Result<Json<ReviewResolveResponse>, LibraryError> {
    let user_id = caller.0.user_id;
    let review = blocking(move || state.reject_review(&id, &user_id)).await??;
    Ok(Json(ReviewResolveResponse {
        review: review_view(&review),
        identity: None,
    }))
}

// ---------------------------------------------------------------------------
// Management: preview, apply, undo, baseline restore.
// ---------------------------------------------------------------------------

/// Build a sealed management preview (`retag` or `organize`).
#[utoipa::path(
    post,
    path = "/api/v3/library/manage/preview",
    request_body = ManagePreviewBody,
    responses(
        (status = 200, description = "Sealed preview", body = ManagePreviewResponse),
        (status = 400, description = "Bad kind, fields, or items"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 409, description = "Missing identity, collision, or no space"),
    )
)]
pub async fn manage_preview(
    State(state): State<LibrarySetup>,
    _caller: RequireCurator,
    ValidJson(body): ValidJson<ManagePreviewBody>,
) -> Result<Json<ManagePreviewResponse>, LibraryError> {
    let kind = match body.kind.as_str() {
        "retag" => PlanKind::SamePath,
        "organize" => PlanKind::Move,
        other => {
            return Err(LibraryError::InvalidInput {
                message: format!("Unknown management kind '{other}'"),
            });
        }
    };
    if body.album_id.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Album id must not be empty".to_owned(),
        });
    }
    if body.items.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Preview needs at least one file".to_owned(),
        });
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
    let sealed =
        tokio::task::spawn_blocking(move || state.plan_preview(kind, &body.album_id, inputs))
            .await
            .map_err(|cause| LibraryError::internal(&cause))??;
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
                kind: file.kind,
            })
            .collect(),
    }))
}

/// Apply a sealed preview exactly once.
#[utoipa::path(
    post,
    path = "/api/v3/library/manage/apply",
    request_body = ManageApplyBody,
    responses(
        (status = 200, description = "Apply answer", body = ManageApplyResponse),
        (status = 400, description = "Missing preview token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown preview token"),
        (status = 409, description = "Stale preview or blocked bundle"),
    )
)]
pub async fn manage_apply(
    State(state): State<LibrarySetup>,
    _caller: RequireCurator,
    ValidJson(body): ValidJson<ManageApplyBody>,
) -> Result<Json<ManageApplyResponse>, LibraryError> {
    if body.preview_token.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Preview token must not be empty".to_owned(),
        });
    }
    let applied = tokio::task::spawn_blocking(move || state.apply_preview(&body.preview_token))
        .await
        .map_err(|cause| LibraryError::internal(&cause))??;
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
    path = "/api/v3/library/manage/undo",
    request_body = ManageUndoBody,
    responses(
        (status = 200, description = "Undo answer", body = ManageUndoResponse),
        (status = 400, description = "Missing bundle id"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown bundle id"),
        (status = 409, description = "Undo blocked for a file"),
    )
)]
pub async fn manage_undo(
    State(state): State<LibrarySetup>,
    _caller: RequireCurator,
    ValidJson(body): ValidJson<ManageUndoBody>,
) -> Result<Json<ManageUndoResponse>, LibraryError> {
    if body.bundle_id.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Bundle id must not be empty".to_owned(),
        });
    }
    let applied = tokio::task::spawn_blocking(move || state.undo_bundle(&body.bundle_id))
        .await
        .map_err(|cause| LibraryError::internal(&cause))??;
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

/// Restore tracks to their first-management baselines.
#[utoipa::path(
    post,
    path = "/api/v3/library/manage/baseline/restore",
    request_body = BaselineRestoreBody,
    responses(
        (status = 200, description = "Restore answer", body = BaselineRestoreResponse),
        (status = 400, description = "No tracks listed"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 409, description = "Restore blocked for a file"),
    )
)]
pub async fn baseline_restore(
    State(state): State<LibrarySetup>,
    _caller: RequireCurator,
    ValidJson(body): ValidJson<BaselineRestoreBody>,
) -> Result<Json<BaselineRestoreResponse>, LibraryError> {
    if body.track_ids.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Restore needs at least one track".to_owned(),
        });
    }
    let applied = tokio::task::spawn_blocking(move || state.baseline_restore(&body.track_ids))
        .await
        .map_err(|cause| LibraryError::internal(&cause))??;
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
