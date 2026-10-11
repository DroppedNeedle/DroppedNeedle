//! Library Management tools besides profile runs: the tag editor, back to
//! the originals (restore previews and the purge), settling a duplicate,
//! the publish journal's writes that need an administrator, and the dry
//! run that turns automatic management on. Every route is admin-only, as
//! in v2. Store and file work runs on `spawn_blocking`.

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::auth::RequireAdmin;
use super::error::{LibraryError, ValidJson};
use super::management::{
    ManagementJobView, ManagementPreviewCreatedResponse, ManagementSelectionBody,
};
use crate::library::management::activation::{
    ActivationConfirm, ActivationProof, ActivationRequest,
};
use crate::library::management::duplicates::DuplicateRequest;
use crate::library::management::models::{
    DuplicateAction, DuplicateIntent, ManagementError, TagEditMode,
};
use crate::library::management::recovery::{Diagnostics, Resolved};
use crate::library::management::restore::{
    PurgeImpact, PurgeOutcome, PurgeRequest, RestoreRequest,
};
use crate::library::management::service::Management;
use crate::library::management::tag_edit::{EditorContext, TagEditRequest};
use crate::library::wiring::LibrarySetup;
use crate::runtime_config::sections::LibraryManagement;
use crate::settings::models::LibraryManagementSettingsResponse;

/// The routes, relative for nesting under `/api/v3`.
pub fn router() -> Router<LibrarySetup> {
    Router::new()
        .route(
            "/library/management/tracks/{track_id}/tag-editor",
            get(get_management_tag_editor),
        )
        .route(
            "/library/management/tag-edit-previews",
            post(create_management_tag_edit_preview),
        )
        .route(
            "/library/management/baselines/restore-previews",
            post(create_management_restore_preview),
        )
        .route(
            "/library/management/baselines/purge-impact",
            post(get_management_purge_impact),
        )
        .route(
            "/library/management/baselines/purge",
            post(purge_management_baselines),
        )
        .route(
            "/library/management/duplicate-resolution-previews",
            post(create_management_duplicate_preview),
        )
        .route(
            "/library/management/recovery/diagnostics",
            get(get_management_recovery_diagnostics),
        )
        .route(
            "/library/management/recovery/import-bundles/{bundle_id}/resolve",
            post(resolve_management_bundle),
        )
        .route(
            "/settings/library-management/activation-previews",
            post(create_management_activation_preview),
        )
        .route(
            "/settings/library-management/activation-previews/{job_id}",
            get(get_management_activation_preview),
        )
        .route(
            "/settings/library-management/activation-confirmations",
            post(confirm_management_activation),
        )
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ManagementError> + Send + 'static,
) -> Result<T, LibraryError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause))?
        .map_err(LibraryError::from)
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
    #[schema(value_type = Object)]
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

/// How a tag edit is kept.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagementTagEditMode {
    /// Write the value and keep it, so later runs keep it too.
    SaveOverride,
    /// Write the value once.
    WriteOnce,
    /// Drop the kept value; the edition's value comes back.
    ResetCanonical,
}

/// One edited tag.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementTagEditFieldBody {
    pub field_name: String,
    /// Text, a number, true/false, a list, or null to clear. Ignored for
    /// a reset.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub value: serde_json::Value,
}

/// Preview a tag edit.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementTagEditPreviewBody {
    pub local_track_id: String,
    pub mode: ManagementTagEditMode,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub fields: Vec<ManagementTagEditFieldBody>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// The tag editor for one file.
#[utoipa::path(
    get,
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
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(track_id): Path<String>,
) -> Result<Json<ManagementTagEditorView>, LibraryError> {
    let management = Management::new(&state);
    let context = blocking(move || management.tag_editor(&track_id)).await?;
    Ok(Json(context.into()))
}

/// Preview a tag edit; apply it like any preview.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    ValidJson(body): ValidJson<ManagementTagEditPreviewBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, LibraryError> {
    let management = Management::new(&state);
    let request = TagEditRequest {
        local_track_id: body.local_track_id,
        mode: match body.mode {
            ManagementTagEditMode::SaveOverride => TagEditMode::SaveOverride,
            ManagementTagEditMode::WriteOnce => TagEditMode::WriteOnce,
            ManagementTagEditMode::ResetCanonical => TagEditMode::ResetCanonical,
        },
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        fields: body
            .fields
            .into_iter()
            .map(|field| (field.field_name, field.value))
            .collect(),
        idempotency_key: key(body.idempotency_key),
    };
    let handle = blocking(move || management.create_tag_edit(request, &caller.user_id)).await?;
    Ok(Json(handle.into()))
}

// ---------------------------------------------------------------------------
// Originals: restore previews and the purge.
// ---------------------------------------------------------------------------

/// Preview putting files back the way they were before management.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementRestorePreviewBody {
    pub selection: ManagementSelectionBody,
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
    pub catalog_revision: i64,
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
            catalog_revision: impact.catalog_revision,
            impact_token: impact.impact_token,
        }
    }
}

/// Delete every saved original for good.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementPurgeBody {
    pub impact_token: String,
    pub expected_catalog_revision: i64,
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
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    ValidJson(body): ValidJson<ManagementRestorePreviewBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, LibraryError> {
    let management = Management::new(&state);
    let request = RestoreRequest {
        selection: body.selection.into(),
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        idempotency_key: body.idempotency_key,
    };
    let handle =
        blocking(move || management.create_restore_preview(request, &caller.user_id)).await?;
    Ok(Json(handle.into()))
}

/// What a purge of the saved originals would remove.
#[utoipa::path(
    post,
    path = "/api/v3/library/management/baselines/purge-impact",
    responses(
        (status = 200, description = "Purge impact", body = ManagementPurgeImpactView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn get_management_purge_impact(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
) -> Result<Json<ManagementPurgeImpactView>, LibraryError> {
    let management = Management::new(&state);
    let impact = blocking(move || management.purge_impact()).await?;
    Ok(Json(impact.into()))
}

/// Delete every saved original for good.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    ValidJson(body): ValidJson<ManagementPurgeBody>,
) -> Result<Json<ManagementPurgeView>, LibraryError> {
    let management = Management::new(&state);
    let request = PurgeRequest {
        impact_token: body.impact_token,
        expected_catalog_revision: body.expected_catalog_revision,
        typed_confirmation: body.typed_confirmation,
        idempotency_key: body.idempotency_key,
    };
    let outcome = blocking(move || management.purge(request, &caller.user_id)).await?;
    Ok(Json(outcome.into()))
}

// ---------------------------------------------------------------------------
// Duplicates.
// ---------------------------------------------------------------------------

/// What to do with a file whose place is taken.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagementDuplicateAction {
    KeepExisting,
    KeepIncomingAlternate,
    RecycleExistingKeepIncoming,
    RecycleIncomingKeepExisting,
}

/// Settle one file a preview could not place.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagementDuplicateBody {
    pub source_job_id: String,
    pub source_plan_item_ordinal: i64,
    pub expected_source_operation_row_revision: i64,
    pub collision_kind: String,
    pub existing_root_id: String,
    pub existing_relative_path: String,
    pub action: ManagementDuplicateAction,
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
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    ValidJson(body): ValidJson<ManagementDuplicateBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, LibraryError> {
    let management = Management::new(&state);
    let request = DuplicateRequest {
        intent: DuplicateIntent {
            source_job_id: body.source_job_id,
            source_plan_item_ordinal: body.source_plan_item_ordinal,
            collision_kind: body.collision_kind,
            existing_root_id: body.existing_root_id,
            existing_relative_path: body.existing_relative_path,
            existing_local_track_id: body.existing_local_track_id,
            action: match body.action {
                ManagementDuplicateAction::KeepExisting => DuplicateAction::KeepExisting,
                ManagementDuplicateAction::KeepIncomingAlternate => {
                    DuplicateAction::KeepIncomingAlternate
                }
                ManagementDuplicateAction::RecycleExistingKeepIncoming => {
                    DuplicateAction::RecycleExistingKeepIncoming
                }
                ManagementDuplicateAction::RecycleIncomingKeepExisting => {
                    DuplicateAction::RecycleIncomingKeepExisting
                }
            },
            alternate_relative_path: body.alternate_relative_path,
        },
        expected_source_row_revision: body.expected_source_operation_row_revision,
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        idempotency_key: body.idempotency_key,
    };
    let handle =
        blocking(move || management.create_duplicate_preview(request, &caller.user_id)).await?;
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
                .map(|bundle| ManagementAttentionBundleView {
                    bundle_id: bundle.bundle_id,
                    file_count: bundle.file_count,
                    sample_paths: bundle.sample_paths,
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
}

impl From<Resolved> for ManagementResolveView {
    fn from(resolved: Resolved) -> Self {
        Self {
            bundle_id: resolved.bundle_id,
            state: resolved.state,
            verified_files: resolved.verified_files,
            total_files: resolved.total_files,
        }
    }
}

/// What the publish journal holds, and the writes that need you.
#[utoipa::path(
    get,
    path = "/api/v3/library/management/recovery/diagnostics",
    responses(
        (status = 200, description = "Journal diagnostics", body = ManagementRecoveryView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn get_management_recovery_diagnostics(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
) -> Result<Json<ManagementRecoveryView>, LibraryError> {
    let management = Management::new(&state);
    let diagnostics = blocking(move || management.recovery_diagnostics()).await?;
    Ok(Json(diagnostics.into()))
}

/// Close a write that needs attention once its files check out. Nothing
/// on disk changes.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    Path(bundle_id): Path<String>,
) -> Result<Json<ManagementResolveView>, LibraryError> {
    let management = Management::new(&state);
    let resolved = blocking(move || management.resolve_bundle(&bundle_id, &caller.user_id)).await?;
    Ok(Json(resolved.into()))
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
    State(state): State<LibrarySetup>,
    RequireAdmin(caller): RequireAdmin,
    ValidJson(body): ValidJson<ManagementActivationPreviewBody>,
) -> Result<Json<ManagementPreviewCreatedResponse>, LibraryError> {
    let management = Management::new(&state);
    let request = ActivationRequest {
        root_id: body.root_id,
        settings: body.settings,
        expected_settings_revision: body.expected_settings_revision,
        expected_policy_revision: body.expected_policy_revision,
        idempotency_key: key(body.idempotency_key),
    };
    let handle = blocking(move || management.create_activation(request, &caller.user_id)).await?;
    Ok(Json(handle.into()))
}

/// One dry run.
#[utoipa::path(
    get,
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
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
) -> Result<Json<ManagementJobView>, LibraryError> {
    let management = Management::new(&state);
    let detail = blocking(move || management.activation_detail(&job_id)).await?;
    Ok(Json(detail.into()))
}

/// Save settings that turn automatic management on, with their dry runs.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    ValidJson(body): ValidJson<ManagementActivationConfirmBody>,
) -> Result<Json<LibraryManagementSettingsResponse>, LibraryError> {
    let management = Management::new(&state);
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
    let saved = blocking(move || management.confirm_activation(request)).await?;
    Ok(Json(saved))
}
