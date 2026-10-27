//! Library Management tools besides profile runs: the tag editor, back to
//! the originals (restore previews and the purge), settling a duplicate,
//! the publish journal's writes that need an administrator, and the dry
//! run that turns automatic management on. Every route is admin-only, as
//! in v2. Store and file work runs on a blocking thread.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::blocking;
use super::management::{ManagementJobView, ManagementPreviewCreatedResponse};
use crate::management::activation::{ActivationConfirm, ActivationProof, ActivationRequest};
use crate::management::duplicates::DuplicateRequest;
use crate::management::models::{DuplicateAction, DuplicateIntent, Selection, TagEditMode};
use crate::management::recovery::{AttentionBundle, Diagnostics, Resolved};
use crate::management::restore::{PurgeImpact, PurgeOutcome, PurgeRequest, RestoreRequest};
use crate::management::service::Management;
use crate::management::tag_edit::{EditorContext, TagEditRequest};
use crate::management::takeover::Cause;
use crate::publish::PlacementMode;
use crate::settings_models::LibraryManagementSettingsResponse;
use crate::wiring::LibraryServices;
use droppedneedle_auth::Admin;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;

/// Library Management tool routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .admin(routes!(get_management_tag_editor))
        .admin(routes!(create_management_tag_edit_preview))
        .admin(routes!(create_management_restore_preview))
        .admin(routes!(get_management_purge_impact))
        .admin(routes!(purge_management_baselines))
        .admin(routes!(create_management_duplicate_preview))
        .admin(routes!(get_management_recovery_diagnostics))
        .admin(routes!(resolve_management_bundle))
        .admin(routes!(list_management_automatic_skipped))
        .admin(routes!(list_management_takeovers))
        .admin(routes!(get_management_placement))
        .admin(routes!(check_management_placement))
        .admin(routes!(create_management_activation_preview))
        .admin(routes!(get_management_activation_preview))
        .admin(routes!(confirm_management_activation))
}

fn key(raw: Option<String>) -> Option<String> {
    raw.filter(|key| !key.trim().is_empty())
}

// ---------------------------------------------------------------------------
// Tag editor.
// ---------------------------------------------------------------------------

/// One tag in the editor.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementTagEditorFieldView {
    pub field_name: String,
    /// `album` (applies to every file of the album) or `track`.
    pub scope: String,
    /// `string`, `integer`, `boolean`, or `ordered_strings`.
    pub cardinality: String,
    /// The file's value now: text, a number, true/false, a list, or null.
    pub current_value: serde_json::Value,
    pub override_id: Option<String>,
    /// `replace`, `preserve`, or `clear`.
    pub override_mode: Option<String>,
    pub override_row_revision: Option<i64>,
}

/// One file's tags, saved overrides and match.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementTagEditorView {
    pub local_track_id: String,
    pub local_album_id: String,
    pub root_id: String,
    pub profile_id: String,
    pub profile_name: String,
    /// Echo on the edit preview.
    pub settings_revision: String,
    /// Echo on the edit preview.
    pub policy_revision: String,
    pub track_revision: i64,
    pub album_revision: i64,
    pub accepted_identity: bool,
    /// Why the file has no accepted match, as a reason code.
    pub identity_reason: Option<String>,
    pub fields: Vec<ManagementTagEditorFieldView>,
}

impl From<EditorContext> for ManagementTagEditorView {
    fn from(context: EditorContext) -> Self {
        Self {
            local_track_id: context.local_track_id,
            local_album_id: context.local_album_id,
            root_id: context.root_id,
            profile_id: context.profile_id,
            profile_name: context.profile_name,
            settings_revision: context.settings_revision,
            policy_revision: context.policy_revision,
            track_revision: context.track_revision,
            album_revision: context.album_revision,
            accepted_identity: context.accepted_identity,
            identity_reason: context.identity_reason,
            fields: context
                .fields
                .into_iter()
                .map(|field| ManagementTagEditorFieldView {
                    field_name: field.field_name,
                    scope: field.scope.to_owned(),
                    cardinality: field.cardinality.to_owned(),
                    current_value: field.current_value,
                    override_id: field.override_id,
                    override_mode: field.override_mode,
                    override_row_revision: field.override_row_revision,
                })
                .collect(),
        }
    }
}

/// One edited tag.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementTagEditFieldBody {
    pub field_name: String,
    /// Text, a number, true/false, a list, or null to clear. Ignored for
    /// a reset.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// Preview a tag edit.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementTagEditPreviewBody {
    pub local_track_id: String,
    pub mode: TagEditMode,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub fields: Vec<ManagementTagEditFieldBody>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// The tag editor for one file.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/tracks/{track_id}/tag-editor",
    params(("track_id" = String, Path, description = "Library track id")),
    responses(
        (status = 200, description = "The file's tags", body = ManagementTagEditorView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown track"),
        (status = 409, description = "The file cannot be read now"),
    )
)]
pub async fn get_management_tag_editor(
    State(management): State<Management>,
    _caller: Admin,
    Path(track_id): Path<String>,
) -> Result<Json<ManagementTagEditorView>, ApiError> {
    let context = blocking(move || management.tag_editor(&track_id)).await?;
    Ok(Json(context.into()))
}

/// Preview a tag edit; apply it like any preview.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/tag-edit-previews",
    request_body = ManagementTagEditPreviewBody,
    responses(
        (status = 200, description = "Preview queued", body = ManagementPreviewCreatedResponse),
        (status = 400, description = "Invalid edit"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown track"),
        (status = 409, description = "Settings or library folders changed"),
    )
)]
pub async fn create_management_tag_edit_preview(
    State(management): State<Management>,
    caller: Admin,
    ValidJson(body): ValidJson<ManagementTagEditPreviewBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, ApiError> {
    let request = TagEditRequest {
        local_track_id: body.local_track_id,
        mode: body.mode,
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        fields: body
            .fields
            .into_iter()
            .map(|field| (field.field_name, field.value))
            .collect(),
        idempotency_key: key(body.idempotency_key),
    };
    let handle = blocking(move || management.create_tag_edit(request, caller.user_id())).await?;
    Ok(Json(handle.into()))
}

// ---------------------------------------------------------------------------
// Originals: restore previews and the purge.
// ---------------------------------------------------------------------------

/// Preview putting files back the way they were before management.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementRestorePreviewBody {
    pub selection: Selection,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub idempotency_key: String,
}

/// What a purge of the saved originals would remove.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPurgeImpactView {
    pub baseline_count: i64,
    pub referenced_blob_count: i64,
    pub referenced_blob_bytes: i64,
    pub blocked_journal_count: i64,
    pub active_restore_count: i64,
    /// Echo on the purge.
    pub impact_token: String,
}

impl From<PurgeImpact> for ManagementPurgeImpactView {
    fn from(impact: PurgeImpact) -> Self {
        Self {
            baseline_count: impact.baseline_count,
            referenced_blob_count: impact.referenced_blob_count,
            referenced_blob_bytes: impact.referenced_blob_bytes,
            blocked_journal_count: impact.blocked_journal_count,
            active_restore_count: impact.active_restore_count,
            impact_token: impact.impact_token,
        }
    }
}

/// Delete every saved original for good.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementPurgeBody {
    pub impact_token: String,
    /// Must be `CONFIRM`.
    pub typed_confirmation: String,
    pub idempotency_key: String,
}

/// What a purge removed.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPurgeView {
    pub purged_baseline_count: i64,
    pub detached_reference_count: i64,
    pub cleaned_blob_count: i64,
    /// True when the request key named an earlier purge.
    pub existing: bool,
}

impl From<PurgeOutcome> for ManagementPurgeView {
    fn from(outcome: PurgeOutcome) -> Self {
        Self {
            purged_baseline_count: outcome.purged_baseline_count,
            detached_reference_count: outcome.detached_reference_count,
            cleaned_blob_count: outcome.cleaned_blob_count,
            existing: outcome.existing,
        }
    }
}

/// Preview putting the selected albums back to their originals.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/baselines/restore-previews",
    request_body = ManagementRestorePreviewBody,
    responses(
        (status = 200, description = "Preview queued", body = ManagementPreviewCreatedResponse),
        (status = 400, description = "Invalid selection"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "Settings or library folders changed"),
    )
)]
pub async fn create_management_restore_preview(
    State(management): State<Management>,
    caller: Admin,
    ValidJson(body): ValidJson<ManagementRestorePreviewBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, ApiError> {
    let request = RestoreRequest {
        selection: body.selection,
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        idempotency_key: body.idempotency_key,
    };
    let handle =
        blocking(move || management.create_restore_preview(request, caller.user_id())).await?;
    Ok(Json(handle.into()))
}

/// What a purge of the saved originals would remove.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/baselines/purge-impact",
    responses(
        (status = 200, description = "Purge impact", body = ManagementPurgeImpactView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn get_management_purge_impact(
    State(management): State<Management>,
    _caller: Admin,
) -> Result<Json<ManagementPurgeImpactView>, ApiError> {
    let impact = blocking(move || management.purge_impact()).await?;
    Ok(Json(impact.into()))
}

/// Delete every saved original for good.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/baselines/purge",
    request_body = ManagementPurgeBody,
    responses(
        (status = 200, description = "Originals purged", body = ManagementPurgeView),
        (status = 400, description = "Confirmation missing"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "The impact changed, or a restore or write is running"),
    )
)]
pub async fn purge_management_baselines(
    State(management): State<Management>,
    caller: Admin,
    ValidJson(body): ValidJson<ManagementPurgeBody>,
) -> Result<Json<ManagementPurgeView>, ApiError> {
    let request = PurgeRequest {
        impact_token: body.impact_token,
        typed_confirmation: body.typed_confirmation,
        idempotency_key: body.idempotency_key,
    };
    let outcome = blocking(move || management.purge(request, caller.user_id())).await?;
    Ok(Json(outcome.into()))
}

// ---------------------------------------------------------------------------
// Duplicates.
// ---------------------------------------------------------------------------

/// Settle one file a preview could not place.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementDuplicateBody {
    pub source_job_id: String,
    pub source_plan_item_ordinal: i64,
    pub expected_source_operation_row_revision: i64,
    pub collision_kind: String,
    pub existing_root_id: String,
    pub existing_relative_path: String,
    pub action: DuplicateAction,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub existing_local_track_id: Option<String>,
    /// For `keep_incoming_alternate`: where the incoming file goes.
    #[serde(default)]
    pub alternate_relative_path: Option<String>,
}

/// Preview settling one file whose destination is taken.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/duplicate-resolution-previews",
    request_body = ManagementDuplicateBody,
    responses(
        (status = 200, description = "Preview queued", body = ManagementPreviewCreatedResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown preview or file"),
        (status = 409, description = "The preview or settings changed"),
    )
)]
pub async fn create_management_duplicate_preview(
    State(management): State<Management>,
    caller: Admin,
    ValidJson(body): ValidJson<ManagementDuplicateBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, ApiError> {
    let request = DuplicateRequest {
        intent: DuplicateIntent {
            source_job_id: body.source_job_id,
            source_plan_item_ordinal: body.source_plan_item_ordinal,
            collision_kind: body.collision_kind,
            existing_root_id: body.existing_root_id,
            existing_relative_path: body.existing_relative_path,
            existing_local_track_id: body.existing_local_track_id,
            action: body.action,
            alternate_relative_path: body.alternate_relative_path,
        },
        expected_source_row_revision: body.expected_source_operation_row_revision,
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        idempotency_key: body.idempotency_key,
    };
    let handle =
        blocking(move || management.create_duplicate_preview(request, caller.user_id())).await?;
    Ok(Json(handle.into()))
}

// ---------------------------------------------------------------------------
// Recovery.
// ---------------------------------------------------------------------------

/// A write that needs an administrator.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementAttentionBundleView {
    pub bundle_id: String,
    pub file_count: i64,
    pub sample_paths: Vec<String>,
}

/// The publish journal at a glance.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementRecoveryView {
    /// Writes recovery will still finish or unwind.
    pub recoverable_bundle_count: i64,
    pub nonterminal_journal_count: i64,
    /// Files in writes that need an administrator.
    pub needs_attention_count: i64,
    pub cleanup_pending_count: i64,
    pub oldest_updated_at: Option<f64>,
    pub state_counts: BTreeMap<String, i64>,
    pub needs_attention_bundles: Vec<ManagementAttentionBundleView>,
    /// Writes recovery has been retrying for more than a day and is still
    /// retrying.
    pub retrying_bundles: Vec<ManagementAttentionBundleView>,
    /// Hidden temporary files under the library folders that no write
    /// accounts for, as the check after startup found them. Nothing
    /// removes them.
    pub leftover_file_count: i64,
    /// The first hundred of them.
    pub leftover_files: Vec<ManagementLeftoverFileView>,
}

/// A hidden temporary file no write accounts for.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementLeftoverFileView {
    pub root_id: String,
    /// The library folder it is in.
    pub root_path: String,
    /// Path under the library folder.
    pub rel_path: String,
}

impl From<AttentionBundle> for ManagementAttentionBundleView {
    fn from(bundle: AttentionBundle) -> Self {
        Self {
            bundle_id: bundle.bundle_id,
            file_count: bundle.file_count,
            sample_paths: bundle.sample_paths,
        }
    }
}

impl From<Diagnostics> for ManagementRecoveryView {
    fn from(diagnostics: Diagnostics) -> Self {
        Self {
            recoverable_bundle_count: diagnostics.recoverable_bundle_count,
            nonterminal_journal_count: diagnostics.nonterminal_journal_count,
            needs_attention_count: diagnostics.needs_attention_count,
            cleanup_pending_count: diagnostics.cleanup_pending_count,
            oldest_updated_at: diagnostics.oldest_updated_at,
            state_counts: diagnostics.state_counts,
            needs_attention_bundles: diagnostics
                .needs_attention_bundles
                .into_iter()
                .map(Into::into)
                .collect(),
            retrying_bundles: diagnostics
                .retrying_bundles
                .into_iter()
                .map(Into::into)
                .collect(),
            leftover_file_count: diagnostics.leftover_file_count,
            leftover_files: diagnostics
                .leftover_files
                .into_iter()
                .map(|file| ManagementLeftoverFileView {
                    root_id: file.root_id,
                    root_path: file.root_path,
                    rel_path: file.rel_path,
                })
                .collect(),
        }
    }
}

/// A closed write.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementResolveView {
    pub bundle_id: String,
    pub state: String,
    pub verified_files: i64,
    pub total_files: i64,
    /// What closing did, and did not do, in words.
    pub note: String,
}

impl From<Resolved> for ManagementResolveView {
    fn from(resolved: Resolved) -> Self {
        Self {
            bundle_id: resolved.bundle_id,
            state: resolved.state,
            verified_files: resolved.verified_files,
            total_files: resolved.total_files,
            note: resolved.note,
        }
    }
}

/// What the publish journal holds, and the writes that need you.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/recovery/diagnostics",
    responses(
        (status = 200, description = "Journal diagnostics", body = ManagementRecoveryView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn get_management_recovery_diagnostics(
    State(management): State<Management>,
    _caller: Admin,
) -> Result<Json<ManagementRecoveryView>, ApiError> {
    let diagnostics = blocking(move || management.recovery_diagnostics()).await?;
    Ok(Json(diagnostics.into()))
}

/// Close a write that needs attention once its files check out. Nothing
/// on disk changes.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/management/recovery/import-bundles/{bundle_id}/resolve",
    params(("bundle_id" = String, Path, description = "Publish bundle id")),
    responses(
        (status = 200, description = "Write closed", body = ManagementResolveView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown write"),
        (status = 409, description = "A file is missing or different, or the write needs no attention"),
    )
)]
pub async fn resolve_management_bundle(
    State(management): State<Management>,
    caller: Admin,
    Path(bundle_id): Path<String>,
) -> Result<Json<ManagementResolveView>, ApiError> {
    let resolved =
        blocking(move || management.resolve_bundle(&bundle_id, caller.user_id())).await?;
    Ok(Json(resolved.into()))
}

// ---------------------------------------------------------------------------
// Placement.
// ---------------------------------------------------------------------------

/// How DroppedNeedle can add a file to a library folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagementPlacementMode {
    /// Not checked yet, or the check could not tell.
    Unknown,
    /// A file is added only when its name is free.
    Rename,
    /// The same, through a hard link.
    Link,
    /// The disk has neither: the name is checked, then the file is moved
    /// in, and a file another program creates in between is replaced.
    Check,
}

impl From<PlacementMode> for ManagementPlacementMode {
    fn from(mode: PlacementMode) -> Self {
        match mode {
            PlacementMode::Unknown => Self::Unknown,
            PlacementMode::Rename => Self::Rename,
            PlacementMode::Link => Self::Link,
            PlacementMode::Check => Self::Check,
        }
    }
}

/// One library folder and how files can be added to it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementRootPlacementView {
    pub root_id: String,
    pub mode: ManagementPlacementMode,
}

/// What checking one library folder found.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPlacementCheckView {
    pub root_id: String,
    /// The folder's mode now.
    pub mode: ManagementPlacementMode,
    /// False when the check could not tell, for example on a full or
    /// read-only disk. `mode` is then the last result, which is kept.
    pub answered: bool,
}

/// Every saved library folder DroppedNeedle can write to. Excluded
/// folders are not listed.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementPlacementView {
    pub roots: Vec<ManagementRootPlacementView>,
}

/// How safely each library folder can be written to, as last checked.
/// Reading this checks nothing.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/settings/library-management/placement",
    responses(
        (status = 200, description = "Placement mode of each library folder", body = ManagementPlacementView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn get_management_placement(
    State(management): State<Management>,
    _caller: Admin,
) -> Result<Json<ManagementPlacementView>, ApiError> {
    let roots = blocking(move || management.placement_modes())
        .await?
        .into_iter()
        .map(|(root_id, mode)| ManagementRootPlacementView {
            root_id,
            mode: mode.into(),
        })
        .collect();
    Ok(Json(ManagementPlacementView { roots }))
}

/// Check one library folder now. The check creates a folder named
/// `.droppedneedle-management-probe` at the top of the library folder
/// and removes it again.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/settings/library-management/placement/{root_id}/check",
    params(("root_id" = String, Path, description = "Library root id")),
    responses(
        (status = 200, description = "The folder's placement mode now", body = ManagementPlacementCheckView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown or excluded library folder"),
        (status = 409, description = "The library folders cannot be opened right now"),
    )
)]
pub async fn check_management_placement(
    State(management): State<Management>,
    _caller: Admin,
    Path(root_id): Path<String>,
) -> Result<Json<ManagementPlacementCheckView>, ApiError> {
    let checked = root_id.clone();
    let check = blocking(move || management.check_placement(&checked)).await?;
    Ok(Json(ManagementPlacementCheckView {
        root_id,
        mode: check.mode.into(),
        answered: check.answered,
    }))
}

// ---------------------------------------------------------------------------
// Automatic management.
// ---------------------------------------------------------------------------

/// An album automatic management skipped.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementSkippedAlbumView {
    pub album_id: String,
    pub album_title: String,
    pub artist_name: Option<String>,
    /// Why, in words, with what to do about it.
    pub reason: super::management::ManagementReasonView,
    pub attempts: i64,
    pub updated_at: f64,
}

/// Albums automatic management skipped.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementSkippedPage {
    pub items: Vec<ManagementSkippedAlbumView>,
}

/// Albums automatic management skipped, newest first, each with why.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/automatic/skipped",
    responses(
        (status = 200, description = "Skipped albums", body = ManagementSkippedPage),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_management_automatic_skipped(
    State(management): State<Management>,
    _caller: Admin,
) -> Result<Json<ManagementSkippedPage>, ApiError> {
    let skipped = blocking(move || management.automatic_skipped(100)).await?;
    Ok(Json(ManagementSkippedPage {
        items: skipped
            .into_iter()
            .map(|album| ManagementSkippedAlbumView {
                album_id: album.album_id,
                album_title: album.album_title,
                artist_name: album.artist_name,
                reason: album.reason.into(),
                attempts: album.attempts,
                updated_at: album.updated_at,
            })
            .collect(),
    }))
}

/// An album DroppedNeedle stepped back from.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementTakeoverView {
    pub album_id: String,
    pub album_title: String,
    pub artist_name: Option<String>,
    pub cause: Cause,
    /// When it stepped back, unix milliseconds.
    pub noticed_at: i64,
}

/// Albums DroppedNeedle stepped back from.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagementTakeoverPage {
    /// Newest first, 100 at most.
    pub items: Vec<ManagementTakeoverView>,
    /// How many there are in all.
    pub total: i64,
    /// Albums already handed back that wait for their turn.
    pub handing_back: i64,
}

/// Albums left unmanaged because their files were changed outside
/// DroppedNeedle and that nobody has handed back yet, newest first.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/management/takeovers",
    responses(
        (status = 200, description = "Albums left unmanaged", body = ManagementTakeoverPage),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_management_takeovers(
    State(management): State<Management>,
    _caller: Admin,
) -> Result<Json<ManagementTakeoverPage>, ApiError> {
    let waiting = blocking(move || management.takeovers(100)).await?;
    Ok(Json(ManagementTakeoverPage {
        items: waiting
            .albums
            .into_iter()
            .map(|album| ManagementTakeoverView {
                album_id: album.album_id,
                album_title: album.album_title,
                artist_name: album.artist_name,
                cause: album.cause,
                noticed_at: album.noticed_at,
            })
            .collect(),
        total: waiting.total,
        handing_back: waiting.handing_back,
    }))
}

// ---------------------------------------------------------------------------
// Activation.
// ---------------------------------------------------------------------------

/// Dry-run one library folder under proposed settings.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementActivationPreviewBody {
    pub root_id: String,
    pub settings: LibraryManagement,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// One dry run offered as proof.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementActivationProofBody {
    pub root_id: String,
    pub job_id: String,
    pub preview_token: String,
}

/// Save settings with the dry runs that confirm them.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementActivationConfirmBody {
    pub settings: LibraryManagement,
    pub proofs: Vec<ManagementActivationProofBody>,
    pub expected_settings_revision: String,
    #[serde(default)]
    pub confirmation: bool,
}

/// Dry-run a library folder under proposed settings. Nothing changes.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/settings/library-management/activation-previews",
    request_body = ManagementActivationPreviewBody,
    responses(
        (status = 200, description = "Dry run queued", body = ManagementPreviewCreatedResponse),
        (status = 400, description = "The folder has no automatic trigger, or invalid settings"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "Settings or library folders changed"),
    )
)]
pub async fn create_management_activation_preview(
    State(management): State<Management>,
    caller: Admin,
    ValidJson(body): ValidJson<ManagementActivationPreviewBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, ApiError> {
    let request = ActivationRequest {
        root_id: body.root_id,
        settings: body.settings,
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        idempotency_key: key(body.idempotency_key),
    };
    let handle = blocking(move || management.create_activation(request, caller.user_id())).await?;
    Ok(Json(handle.into()))
}

/// One dry run.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/settings/library-management/activation-previews/{job_id}",
    params(("job_id" = String, Path, description = "Dry run job id")),
    responses(
        (status = 200, description = "Dry run", body = ManagementJobView),
        (status = 400, description = "Not a dry run"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
    )
)]
pub async fn get_management_activation_preview(
    State(management): State<Management>,
    _caller: Admin,
    Path(job_id): Path<String>,
) -> Result<Json<ManagementJobView>, ApiError> {
    let detail = blocking(move || management.activation_detail(&job_id)).await?;
    Ok(Json(detail.into()))
}

/// Save settings that turn automatic management on, with their dry runs.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/settings/library-management/activation-confirmations",
    request_body = ManagementActivationConfirmBody,
    responses(
        (status = 200, description = "Settings saved", body = LibraryManagementSettingsResponse),
        (status = 400, description = "Missing confirmation or proof"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown dry run"),
        (status = 409, description = "A dry run is stale or settings changed"),
    )
)]
pub async fn confirm_management_activation(
    State(management): State<Management>,
    caller: Admin,
    ValidJson(body): ValidJson<ManagementActivationConfirmBody>,
) -> Result<Json<LibraryManagementSettingsResponse>, ApiError> {
    let request = ActivationConfirm {
        settings: body.settings,
        proofs: body
            .proofs
            .into_iter()
            .map(|proof| ActivationProof {
                root_id: proof.root_id,
                job_id: proof.job_id,
                preview_token: proof.preview_token,
            })
            .collect(),
        expected_settings_revision: body.expected_settings_revision,
        confirmation: body.confirmation,
    };
    let saved = blocking(move || management.confirm_activation(request, caller.user_id())).await?;
    Ok(Json(saved))
}
