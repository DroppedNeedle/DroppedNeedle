"""Durable library-management persistence contracts."""

from __future__ import annotations

from typing import Literal

from infrastructure.msgspec_fastapi import AppStruct
from models.audio import AudioInfo, AudioTag
from models.audio_metadata import AudioFieldValue
from models.audio_metadata import DesiredAudioDocument
from models.library_management_planning import PinnedLibraryManagementProfile

ManagementBlobKind = Literal[
    "tag_snapshot", "image", "sidecar_manifest", "metadata_document"
]

LibraryManagementImportOrigin = Literal[
    "acquisition", "drop_import", "edition_conversion"
]
LibraryManagementAutomaticTrigger = Literal[
    "acquisition", "drop_import", "scan_discovered"
]
LibraryManagementImportJournalState = Literal[
    "planned",
    "staged",
    "validated",
    "replacement_backed_up",
    "published",
    "catalog_committed",
    "cleanup_pending",
    "completed",
    "rollback_pending",
    "rolled_back",
    "needs_attention",
]


class LibraryManagementImportArtifact(AppStruct):
    kind: Literal["external_art", "sidecar"]
    destination_root_id: str
    destination_relative_path: str
    source_path: str | None = None
    content: bytes | None = None
    source_fingerprint: str | None = None


class LibraryManagementImportFile(AppStruct):
    ordinal: int
    input_path: str
    destination_root_id: str
    destination_relative_path: str
    tag: AudioTag
    info: AudioInfo
    release_group_mbid: str | None
    release_mbid: str | None
    recording_mbid: str | None
    confidence: float
    source: str
    source_path: str | None = None
    download_task_id: str | None = None
    file_mtime: float | None = None
    replacement_local_track_id: str | None = None
    replacement_root_id: str | None = None
    replacement_relative_path: str | None = None
    recycle_bin_path: str | None = None
    authoritative_mapping: bool = False
    reviewed_recording_identity: bool = False
    release_track_mbid: str | None = None
    medium_position: int | None = None
    release_track_position: int | None = None
    baseline_relative_path: str | None = None
    desired_document: DesiredAudioDocument | None = None
    pinned_profile: PinnedLibraryManagementProfile | None = None
    metadata_snapshot_id: str | None = None
    projection_hash: str | None = None
    settings_revision: str | None = None
    naming_policy_revision: str | None = None
    undo_retention_days: int | None = None
    management_warnings: tuple[str, ...] = ()
    artifacts: tuple[LibraryManagementImportArtifact, ...] = ()
    conversion_recycle_only: bool = False


class LibraryManagementImportBundle(AppStruct):
    idempotency_key: str
    origin: LibraryManagementImportOrigin
    policy_revision: str
    files: tuple[LibraryManagementImportFile, ...]
    conversion_job_id: str | None = None
    conversion_expected_row_revision: int | None = None
    conversion_local_album_id: str | None = None
    conversion_preview_job_id: str | None = None
    conversion_recycle_bin_path: str | None = None


class LibraryManagementPublishedImportFile(AppStruct):
    request: LibraryManagementImportFile
    destination_path: str
    staged_fingerprint: str
    tag: AudioTag
    info: AudioInfo


class LibraryManagementImportResult(AppStruct):
    bundle_id: str
    paths: tuple[str, ...]
    local_track_ids: tuple[str, ...]
    repeated: bool = False


class LibraryManagementImportBundleRecord(AppStruct):
    id: str
    idempotency_key: str
    origin: LibraryManagementImportOrigin
    policy_revision: str
    request_json: str
    request_hash: str
    state: Literal[
        "preparing",
        "publishing",
        "catalog_committed",
        "cleanup_pending",
        "completed",
        "rolled_back",
        "needs_attention",
    ]
    result_json: str = "{}"
    created_at: float = 0.0
    updated_at: float = 0.0
    row_revision: int = 1


class LibraryManagementImportJournal(AppStruct):
    bundle_id: str
    ordinal: int
    state: LibraryManagementImportJournalState
    source_fingerprint: str
    source_size: int
    source_mtime_ns: int
    temporary_relative_path: str
    destination_root_id: str
    destination_relative_path: str
    staged_fingerprint: str | None = None
    replacement_fingerprint: str | None = None
    replacement_backup_relative_path: str | None = None
    baseline_blob_sha256: str | None = None
    baseline_format: str | None = None
    baseline_adapter_version: str | None = None
    baseline_stat_revision: str | None = None
    baseline_tag_revision: str | None = None
    baseline_image_snapshot_json: str = "[]"
    baseline_ancillary_snapshot_json: str = "[]"
    baseline_file_mtime_ns: int | None = None
    baseline_file_mode: int | None = None
    failure_code: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    row_revision: int = 1


ManagementReferenceKind = Literal[
    "baseline", "operation_snapshot", "artwork", "sidecar", "metadata_snapshot"
]
ManagementMode = Literal[
    "preview",
    "apply",
    "automatic_apply",
    "undo",
    "baseline_restore",
    "duplicate_resolution",
]
ManagementOrigin = Literal["manual", "acquisition", "drop_import", "scan_discovered"]
ManagementPhase = Literal[
    "planning", "ready", "applying", "undoing", "restoring", "complete"
]
ManagementEligibility = Literal["eligible", "warning", "blocked", "stale"]
MutationSubjectKind = Literal["audio", "sidecar", "external_art"]
MutationJournalState = Literal[
    "planned",
    "snapshot_saved",
    "staged",
    "validated",
    "source_backed_up",
    "published",
    "catalog_committed",
    "cleanup_pending",
    "completed",
    "rollback_pending",
    "rolled_back",
    "needs_attention",
]
CollisionClassification = Literal[
    "same_catalog_track_same_content",
    "same_path_same_content",
    "same_path_different_content",
    "same_release_position_different_content",
    "normalized_path_collision",
    "sidecar_collision",
    "destination_created_after_preview",
]

MANAGEMENT_DISABLED = "MANAGEMENT_DISABLED"
IDENTITY_NOT_ACCEPTED = "IDENTITY_NOT_ACCEPTED"
RELEASE_NOT_SELECTED = "RELEASE_NOT_SELECTED"
TRACK_NOT_MAPPED = "TRACK_NOT_MAPPED"
METADATA_UNAVAILABLE = "METADATA_UNAVAILABLE"
OPTIONAL_ENRICHMENT_DEFERRED = "OPTIONAL_ENRICHMENT_DEFERRED"
FORMAT_UNSUPPORTED = "FORMAT_UNSUPPORTED"
FIELD_UNSUPPORTED_BY_FORMAT = "FIELD_UNSUPPORTED_BY_FORMAT"
FILE_UNREADABLE = "FILE_UNREADABLE"
FILE_CHANGED = "FILE_CHANGED"
PROFILE_CHANGED = "PROFILE_CHANGED"
POLICY_CHANGED = "POLICY_CHANGED"
OVERRIDE_CHANGED = "OVERRIDE_CHANGED"
ROOT_UNAVAILABLE = "ROOT_UNAVAILABLE"
ROOT_READ_ONLY = "ROOT_READ_ONLY"
OUT_OF_ROOT = "OUT_OF_ROOT"
SYMLINK_UNSUPPORTED = "SYMLINK_UNSUPPORTED"
PATH_COLLISION_IDENTICAL = "PATH_COLLISION_IDENTICAL"
PATH_COLLISION_DIFFERENT = "PATH_COLLISION_DIFFERENT"
POSITION_COLLISION = "POSITION_COLLISION"
SIDECAR_COLLISION = "SIDECAR_COLLISION"
PATH_TOO_LONG = "PATH_TOO_LONG"
SCRIPT_VALIDATION_FAILED = "SCRIPT_VALIDATION_FAILED"
INSUFFICIENT_SPACE = "INSUFFICIENT_SPACE"
BASELINE_UNAVAILABLE = "BASELINE_UNAVAILABLE"
BASELINE_SNAPSHOT_MISSING = "BASELINE_SNAPSHOT_MISSING"
BASELINE_SNAPSHOT_CORRUPT = "BASELINE_SNAPSHOT_CORRUPT"
UNDO_EXPIRED = "UNDO_EXPIRED"
RECOVERY_NEEDS_ATTENTION = "RECOVERY_NEEDS_ATTENTION"
BUNDLE_BLOCKED = "BUNDLE_BLOCKED"
BUNDLE_TOO_LARGE = "BUNDLE_TOO_LARGE"
RECYCLE_UNAVAILABLE = "RECYCLE_UNAVAILABLE"
DUPLICATE_CHANGED = "DUPLICATE_CHANGED"
EXTERNAL_REFRESH_PROTOCOL_UNAVAILABLE = "EXTERNAL_REFRESH_PROTOCOL_UNAVAILABLE"
EXTERNAL_REFRESH_NOT_CONFIGURED = "EXTERNAL_REFRESH_NOT_CONFIGURED"
EXTERNAL_REFRESH_AUTH_FAILED = "EXTERNAL_REFRESH_AUTH_FAILED"
EXTERNAL_REFRESH_FAILED = "EXTERNAL_REFRESH_FAILED"

AUTOMATIC_MANAGEMENT_RETRY_CODES = frozenset(
    {METADATA_UNAVAILABLE, ROOT_UNAVAILABLE, ROOT_READ_ONLY}
)
MANAGEMENT_RETRY_DELAYS_SECONDS = (300, 900, 1800, 3600, 7200, 14400, 28800, 86400)
EXTERNAL_REFRESH_INTERRUPTED = "EXTERNAL_REFRESH_INTERRUPTED"

MANAGEMENT_RECYCLE_ROOT_ID = "__library_management_recycle__"


class LibraryManagementBlob(AppStruct):
    sha256: str
    kind: ManagementBlobKind
    byte_length: int
    relative_path: str
    media_metadata_json: str = "{}"
    created_at: float = 0.0
    row_revision: int = 1


class LibraryManagementBlobReference(AppStruct):
    blob_sha256: str
    reference_kind: ManagementReferenceKind
    reference_id: str
    created_at: float = 0.0


class LibraryManagementBlobCleanupResult(AppStruct):
    temporary_files_removed: int = 0
    unreferenced_blobs_removed: int = 0
    unledgered_files_removed: int = 0


class LibraryManagementBaseline(AppStruct):
    id: str
    local_track_id: str
    original_root_id: str
    original_relative_path: str
    format: str
    adapter_version: str
    semantic_snapshot_blob_sha256: str
    stat_revision: str
    tag_revision: str
    image_snapshot_json: str = "[]"
    ancillary_snapshot_json: str = "[]"
    file_mtime_ns: int | None = None
    file_mode: int | None = None
    identity_revision: int | None = None
    created_at: float = 0.0
    restore_status: Literal["available", "restoring", "restored", "stale", "purged"] = (
        "available"
    )
    last_verified_at: float | None = None
    row_revision: int = 1
    catalog_document_json: str | None = None
    catalog_document_hash: str | None = None


class LibraryTrackManagementState(AppStruct):
    local_track_id: str
    baseline_id: str | None = None
    applied_profile_id: str | None = None
    applied_profile_revision: str | None = None
    applied_projection_hash: str | None = None
    applied_naming_script_revision: str | None = None
    applied_override_revision: str | None = None
    last_operation_job_id: str | None = None
    managed_root_id: str | None = None
    managed_path_revision: str | None = None
    last_managed_at: float | None = None
    last_outcome: str | None = None
    last_reason_code: str | None = None
    row_revision: int = 1


class LibraryManagementOverride(AppStruct):
    id: str
    subject_kind: Literal["album", "track"]
    field_name: str
    value_json: str
    mode: Literal["replace", "preserve", "clear"]
    local_album_id: str | None = None
    local_track_id: str | None = None
    actor_user_id: str | None = None
    reason: str | None = None
    subject_revision: int = 1
    created_at: float = 0.0
    updated_at: float = 0.0
    row_revision: int = 1


class LibraryManagementTagEditFieldIntent(AppStruct):
    field_name: str
    subject_kind: Literal["album", "track"]
    value: AudioFieldValue | None = None
    override_id: str | None = None
    expected_override_row_revision: int | None = None


class LibraryManagementTagEditIntent(AppStruct):
    local_track_id: str
    local_album_id: str
    mode: Literal["save_override", "write_once", "reset_canonical"]
    fields: list[LibraryManagementTagEditFieldIntent]


class LibraryManagementMetadataSnapshot(AppStruct):
    id: str
    provider: str
    entity_kind: str
    entity_id: str
    input_hash: str
    canonical_payload_json: str
    payload_sha256: str
    fetched_at: float
    expires_at: float | None = None
    provider_version_notes: str | None = None


class LibraryManagementJobSnapshot(AppStruct):
    job_id: str
    mode: ManagementMode
    origin: ManagementOrigin
    phase: ManagementPhase
    selection_json: str
    profile_revision: str
    settings_revision: str
    naming_revision: str
    policy_revision: str
    catalog_revision: int
    profile_snapshot_json: str
    proposed_settings_revision: str | None = None
    preview_token_hash: str | None = None
    preview_created_at: float | None = None
    preview_expires_at: float | None = None
    apply_idempotency_key: str | None = None
    target_root_id: str | None = None
    linked_operation_job_id: str | None = None
    intent_json: str = "{}"
    summary_json: str = "{}"
    warnings_json: str = "[]"
    staging_cursor: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    row_revision: int = 1


ExternalRefreshTarget = Literal["plex", "jellyfin", "navidrome"]
ExternalRefreshState = Literal[
    "pending",
    "delivering",
    "retry_wait",
    "succeeded",
    "failed",
    "unavailable",
]


class LibraryManagementExternalRefreshDelivery(AppStruct):
    id: str
    operation_job_id: str
    target: ExternalRefreshTarget
    state: ExternalRefreshState = "pending"
    attempts: int = 0
    max_attempts: int = 1
    retry_delay_seconds: int = 30
    not_before: float = 0.0
    lease_owner: str | None = None
    lease_expires_at: float | None = None
    failure_code: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    completed_at: float | None = None
    row_revision: int = 1


class LibraryManagementPlanItem(AppStruct):
    job_id: str
    ordinal: int
    bundle_ordinal: int
    expected_catalog_revision: int
    expected_policy_revision: str
    expected_profile_revision: str
    expected_root_id: str
    expected_relative_path: str
    expected_stat_revision: str
    expected_tag_revision: str
    expected_file_fingerprint: str
    source_path_identity: str
    desired_document_json: str
    desired_document_hash: str
    eligibility: ManagementEligibility
    created_at: float
    local_album_id: str | None = None
    local_track_id: str | None = None
    expected_album_revision: int | None = None
    expected_track_revision: int | None = None
    expected_identity_revision: int | None = None
    expected_album_identity_revision: int | None = None
    expected_override_revision: str | None = None
    destination_root_id: str | None = None
    destination_relative_path: str | None = None
    destination_collision_key: str | None = None
    artwork_choices_json: str = "[]"
    diff_json: str = "{}"
    capability_json: str = "{}"
    collision_json: str = "[]"
    reason_code: str | None = None
    estimated_temporary_bytes: int = 0
    catalog_document_json: str | None = None
    catalog_document_hash: str | None = None


class LibraryManagementOperationSnapshot(AppStruct):
    id: str
    job_id: str
    work_ordinal: int
    local_track_id: str
    before_root_id: str
    before_relative_path: str
    format: str
    adapter_version: str
    semantic_snapshot_blob_sha256: str
    source_fingerprint: str
    created_at: float
    expires_at: float
    after_root_id: str | None = None
    after_relative_path: str | None = None
    image_snapshot_json: str = "[]"
    ancillary_snapshot_json: str = "[]"
    before_management_state_json: str = "{}"
    file_mtime_ns: int | None = None
    file_mode: int | None = None
    row_revision: int = 1


class LibraryFileMutationJournal(AppStruct):
    id: str
    job_id: str
    plan_item_ordinal: int
    subject_kind: MutationSubjectKind
    subject_key: str
    state: MutationJournalState
    created_at: float
    updated_at: float
    local_track_id: str | None = None
    source_root_id: str | None = None
    source_relative_path: str | None = None
    temporary_root_id: str | None = None
    temporary_relative_path: str | None = None
    backup_root_id: str | None = None
    backup_relative_path: str | None = None
    destination_root_id: str | None = None
    destination_relative_path: str | None = None
    source_fingerprint: str | None = None
    staged_fingerprint: str | None = None
    baseline_id: str | None = None
    operation_snapshot_id: str | None = None
    attempts: int = 0
    failure_code: str | None = None
    recovery_evidence_json: str = "{}"
    row_revision: int = 1


class LibraryManagementCollisionEvidence(AppStruct):
    id: str
    job_id: str
    plan_item_ordinal: int
    classification: CollisionClassification
    destination_root_id: str
    destination_relative_path: str
    evidence_json: str
    created_at: float
    existing_local_track_id: str | None = None


class LibraryManagementCatalogMutation(AppStruct):
    journal_id: str
    plan_item_ordinal: int
    local_track_id: str
    local_album_id: str
    expected_album_revision: int
    expected_track_revision: int
    expected_root_id: str
    expected_relative_path: str
    expected_stat_revision: str
    expected_tag_revision: str
    expected_identity_revision: int | None
    expected_album_identity_revision: int
    expected_override_revision: str
    expected_identity_kind: Literal["exact_release", "custom_edition"]
    expected_release_group_mbid: str
    expected_release_mbid: str | None
    expected_recording_mbid: str | None
    expected_release_track_mbid: str | None
    expected_custom_manifest_id: str | None
    destination_root_id: str
    destination_relative_path: str
    destination_file_path: str
    destination_path_hash: str
    file_size_bytes: int
    file_mtime_ns: int
    stat_revision: str
    tag_revision: str
    file_fingerprint: str
    tag: AudioTag
    catalog_tag: AudioTag
    info: AudioInfo
    baseline_id: str
    operation_snapshot_id: str
    applied_profile_id: str
    applied_profile_revision: str
    applied_projection_hash: str
    applied_naming_script_revision: str
    applied_override_revision: str | None = None
    restored_management_state_json: str | None = None
    recycle_only: bool = False
    allow_unmapped_conversion_restore: bool = False


class LibraryManagementBundleCommitResult(AppStruct):
    catalog_revision: int
    snapshot_revision: int
    committed_journal_ids: tuple[str, ...]
    committed_journal_revisions: dict[str, int]
