"""Admin HTTP contracts for inert Library Management settings and previews."""

from __future__ import annotations

from typing import Literal

import msgspec

from api.v1.schemas.library_management import (
    LibraryManagementProfile,
    LibraryManagementRootOverrides,
    LibraryManagementSettings,
)
from api.v1.schemas.library_operations import OperationResponse
from infrastructure.msgspec_fastapi import AppStruct

ManagementSelectionKind = Literal["roots", "artists", "albums", "tracks", "filter"]
DuplicateResolutionAction = Literal[
    "keep_existing",
    "keep_incoming_alternate",
    "recycle_existing_keep_incoming",
    "recycle_incoming_keep_existing",
]
DuplicateCollisionKind = Literal[
    "same_path_same_content",
    "same_path_different_content",
    "same_release_position_different_content",
    "normalized_path_collision",
    "sidecar_collision",
    "destination_created_after_preview",
]
LibraryManagementTagEditMode = Literal["save_override", "write_once", "reset_canonical"]
LibraryManagementTagEditValue = str | int | bool | list[str] | None


class LibraryManagementCatalogFilterRequest(AppStruct):
    search: str | None = None
    genre: str | None = None
    from_year: int | None = None
    to_year: int | None = None
    artist_ids: list[str] = msgspec.field(default_factory=list)
    album_artist_only: bool = False


class LibraryManagementSelectionRequest(AppStruct):
    kind: ManagementSelectionKind
    ids: list[str] = msgspec.field(default_factory=list)
    catalog_filter: LibraryManagementCatalogFilterRequest | None = None


class LibraryManagementPreviewCreateRequest(AppStruct):
    selection: LibraryManagementSelectionRequest
    profile_id: str
    expected_settings_revision: str
    expected_policy_revision: str
    idempotency_key: str | None = None
    target_root_id: str | None = None
    overrides: LibraryManagementRootOverrides | None = None


class LibraryManagementPreviewCreatedResponse(AppStruct):
    job_id: str
    preview_token: str
    created_at: float
    expires_at: float
    existing: bool = False


class LibraryManagementTagEditFieldRequest(AppStruct):
    field_name: str
    value: LibraryManagementTagEditValue = None


class LibraryManagementTagEditPreviewRequest(AppStruct):
    local_track_id: str
    mode: LibraryManagementTagEditMode
    expected_settings_revision: str
    expected_policy_revision: str
    fields: list[LibraryManagementTagEditFieldRequest] = msgspec.field(
        default_factory=list
    )
    idempotency_key: str | None = None


class LibraryManagementTagEditorFieldResponse(AppStruct):
    field_name: str
    scope: Literal["album", "track"]
    cardinality: Literal["string", "integer", "boolean", "ordered_strings"]
    current_value: LibraryManagementTagEditValue = None
    override_id: str | None = None
    override_mode: Literal["replace", "preserve", "clear"] | None = None
    override_row_revision: int | None = None


class LibraryManagementTagEditorContextResponse(AppStruct):
    local_track_id: str
    local_album_id: str
    root_id: str
    profile_id: str
    profile_name: str
    settings_revision: str
    policy_revision: str
    track_revision: int
    album_revision: int
    accepted_identity: bool
    identity_reason: str | None = None
    fields: list[LibraryManagementTagEditorFieldResponse] = msgspec.field(
        default_factory=list
    )


class LibraryManagementApplyRequest(AppStruct):
    preview_token: str
    expected_operation_row_revision: int
    idempotency_key: str
    confirmation: bool = False


class LibraryManagementDiscardRequest(AppStruct):
    expected_operation_row_revision: int


class LibraryManagementUndoPreviewRequest(AppStruct):
    expected_operation_row_revision: int
    idempotency_key: str


class LibraryManagementBaselineRestorePreviewRequest(AppStruct):
    selection: LibraryManagementSelectionRequest
    expected_settings_revision: str
    expected_policy_revision: str
    idempotency_key: str


class LibraryManagementDuplicateResolutionPreviewRequest(AppStruct):
    source_job_id: str
    source_plan_item_ordinal: int
    expected_source_operation_row_revision: int
    collision_kind: DuplicateCollisionKind
    existing_root_id: str
    existing_relative_path: str
    action: DuplicateResolutionAction
    expected_settings_revision: str
    expected_policy_revision: str
    idempotency_key: str
    existing_local_track_id: str | None = None
    alternate_relative_path: str | None = None


class LibraryManagementBaselinePurgeImpactResponse(AppStruct):
    baseline_count: int
    referenced_blob_count: int
    referenced_blob_bytes: int
    blocked_journal_count: int
    active_restore_count: int
    catalog_revision: int
    impact_token: str


class LibraryManagementBaselinePurgeRequest(AppStruct):
    impact_token: str
    expected_catalog_revision: int
    typed_confirmation: str
    idempotency_key: str


class LibraryManagementBaselinePurgeResponse(AppStruct):
    purged_baseline_count: int
    detached_reference_count: int
    cleaned_blob_count: int
    existing: bool = False


class LibraryManagementPreviewSummaryResponse(AppStruct):
    item_count: int = 0
    bundle_count: int = 0
    eligible_count: int = 0
    warning_count: int = 0
    blocked_count: int = 0
    stale_count: int = 0
    no_change_count: int = 0
    tag_change_count: int = 0
    artwork_change_count: int = 0
    path_change_count: int = 0
    sidecar_change_count: int = 0
    estimated_temporary_bytes: int = 0
    expanded_track_count: int = 0
    reasons: dict[str, int] = msgspec.field(default_factory=dict)
    roots: dict[str, int] = msgspec.field(default_factory=dict)
    formats: dict[str, int] = msgspec.field(default_factory=dict)
    metadata_snapshot_ids: list[str] = msgspec.field(default_factory=list)


class LibraryManagementExternalRefreshResponse(AppStruct):
    target: Literal["plex", "jellyfin", "navidrome"]
    state: Literal[
        "pending",
        "delivering",
        "retry_wait",
        "succeeded",
        "failed",
        "unavailable",
    ]
    attempts: int = 0
    max_attempts: int = 1
    failure_code: str | None = None
    updated_at: float = 0.0
    completed_at: float | None = None


class LibraryManagementPreviewDetailResponse(AppStruct):
    job_id: str
    state: str
    phase: str
    mode: str
    origin: str
    profile_id: str
    profile_name: str
    profile_revision: str
    settings_revision: str
    policy_revision: str
    catalog_revision: int
    proposed_settings_revision: str | None = None
    target_root_id: str | None = None
    selection: dict = msgspec.field(default_factory=dict)
    summary: LibraryManagementPreviewSummaryResponse = msgspec.field(
        default_factory=LibraryManagementPreviewSummaryResponse
    )
    created_at: float = 0.0
    updated_at: float = 0.0
    expires_at: float | None = None
    expired: bool = False
    stale: bool = False
    stale_reasons: list[str] = msgspec.field(default_factory=list)
    ready_for_confirmation: bool = False
    operation_row_revision: int = 1
    operation_event_revision: int = 0
    terminal_code: str | None = None
    expected_work_count: int = 0
    completed_count: int = 0
    succeeded_count: int = 0
    failed_count: int = 0
    skipped_count: int = 0
    control_request: str = "none"
    undo_available_count: int = 0
    undo_expired_count: int = 0
    undo_expires_at: float | None = None
    baseline_available_count: int = 0
    external_refreshes: list[LibraryManagementExternalRefreshResponse] = msgspec.field(
        default_factory=list
    )


class LibraryManagementPlanItemResponse(AppStruct):
    ordinal: int
    bundle_ordinal: int
    local_album_id: str | None = None
    local_track_id: str | None = None
    source_root_id: str | None = None
    source_relative_path: str | None = None
    destination_root_id: str | None = None
    destination_relative_path: str | None = None
    eligibility: str = "blocked"
    reason_code: str | None = None
    estimated_temporary_bytes: int = 0
    desired_document: dict = msgspec.field(default_factory=dict)
    artwork_choices: list[dict] = msgspec.field(default_factory=list)
    diff: dict = msgspec.field(default_factory=dict)
    capability: dict = msgspec.field(default_factory=dict)
    collisions: list[dict] = msgspec.field(default_factory=list)


class LibraryManagementPlanItemPageResponse(AppStruct):
    items: list[LibraryManagementPlanItemResponse]
    next_after_ordinal: int | None = None
    has_more: bool = False


class LibraryManagementResultItemResponse(AppStruct):
    plan: LibraryManagementPlanItemResponse
    work_state: str
    failure_code: str | None = None
    result: dict = msgspec.field(default_factory=dict)
    journal_states: list[str] = msgspec.field(default_factory=list)


class LibraryManagementResultPageResponse(AppStruct):
    items: list[LibraryManagementResultItemResponse]
    next_after_ordinal: int | None = None
    has_more: bool = False


class LibraryManagementOperationHistoryItemResponse(AppStruct):
    operation: OperationResponse
    mode: str
    origin: str
    phase: str
    profile_id: str
    profile_name: str
    profile_revision: str
    target_root_id: str | None = None
    selection: dict = msgspec.field(default_factory=dict)


class LibraryManagementOperationHistoryResponse(AppStruct):
    items: list[LibraryManagementOperationHistoryItemResponse]
    next_cursor: str | None = None


class LibraryManagementRecoveryDiagnosticsResponse(AppStruct):
    recoverable_bundle_count: int
    nonterminal_journal_count: int
    needs_attention_count: int
    cleanup_pending_count: int
    oldest_updated_at: float | None = None
    state_counts: dict[str, int] = msgspec.field(default_factory=dict)


class LibraryManagementProfileCreateRequest(AppStruct):
    name: str
    expected_settings_revision: str
    description: str = ""


class LibraryManagementProfileCopyRequest(AppStruct):
    name: str
    expected_settings_revision: str


class LibraryManagementProfileUpdateRequest(AppStruct):
    profile: LibraryManagementProfile
    expected_settings_revision: str


class LibraryManagementProfileDeleteRequest(AppStruct):
    expected_settings_revision: str


class LibraryManagementProfileMutationResponse(AppStruct):
    profile: LibraryManagementProfile
    settings_revision: str


class LibraryManagementSettingsImpactRequest(AppStruct):
    settings: LibraryManagementSettings
    expected_settings_revision: str | None = None


class LibraryManagementActivationPreviewRequest(AppStruct):
    root_id: str
    settings: LibraryManagementSettings
    expected_settings_revision: str
    expected_policy_revision: str
    idempotency_key: str | None = None


class LibraryManagementActivationProof(AppStruct):
    root_id: str
    job_id: str
    preview_token: str


class LibraryManagementActivationConfirmRequest(AppStruct):
    settings: LibraryManagementSettings
    proofs: list[LibraryManagementActivationProof]
    expected_settings_revision: str
    confirmation: bool = False
