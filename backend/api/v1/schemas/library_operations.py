"""Target review, catalog-correction, operation, and repair contracts."""

from __future__ import annotations

from typing import Literal

import msgspec

from infrastructure.msgspec_fastapi import AppStruct
from models.identification import CandidateEvidence, TrackEvidence

ReviewState = Literal["needs_review", "keep_tagged", "excluded", "resolved"]
OperationState = Literal[
    "queued",
    "running",
    "paused",
    "ready",
    "succeeded",
    "failed",
    "cancelled",
    "stopped",
]


class ReviewListItem(AppStruct):
    id: str
    state: ReviewState
    reason_code: str
    local_album_id: str | None = None
    local_track_id: str | None = None
    album_title: str = ""
    album_artist_name: str = ""
    year: int | None = None
    track_count: int = 0
    metadata_incomplete_count: int = 0
    root_id: str = ""
    relative_path: str = ""
    effective_policy: str = "automatic"
    exclusion_source: str | None = None
    release_group_mbid: str | None = None
    identity_source: str | None = None
    candidate_count: int = 0
    evidence_summary: dict[str, int] = msgspec.field(default_factory=dict)
    active_job_state: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    row_revision: int = 1


class ReviewListResponse(AppStruct):
    items: list[ReviewListItem]
    next_cursor: str | None = None
    has_more: bool = False
    filtered_total: int = 0
    counts_by_state: dict[str, int] = msgspec.field(default_factory=dict)
    counts_by_reason: dict[str, int] = msgspec.field(default_factory=dict)
    catalog_revision: int = 0


class ReviewTrackDetail(AppStruct):
    id: str
    title: str
    artist_name: str = ""
    local_artist_id: str | None = None
    relative_path: str = ""
    disc_number: int = 1
    track_number: int = 0
    availability: str = "indexed"
    membership_locked: bool = False
    recording_mbid: str | None = None


class ReviewHistoryItem(AppStruct):
    id: str
    kind: Literal["attempt", "decision", "action"]
    state: str
    reason_code: str = ""
    created_at: float = 0.0
    actor_user_id: str | None = None


class ReviewCandidateDetail(AppStruct):
    candidate_key: str
    evidence_revision: str
    evidence: CandidateEvidence
    automatic_safe: bool = False


class ReviewDetailResponse(AppStruct):
    review: ReviewListItem
    tracks: list[ReviewTrackDetail]
    current_evidence: CandidateEvidence | None = None
    candidates: list[ReviewCandidateDetail] = msgspec.field(default_factory=list)
    supported: list[TrackEvidence] = msgspec.field(default_factory=list)
    unknown: list[TrackEvidence] = msgspec.field(default_factory=list)
    contradictory: list[TrackEvidence] = msgspec.field(default_factory=list)
    history: list[ReviewHistoryItem] = msgspec.field(default_factory=list)
    available_actions: list[str] = msgspec.field(default_factory=list)
    catalog_revision: int = 0
    album_revision: int | None = None
    identity_revision: int | None = None
    input_revision: str = ""
    evidence_revision: str = ""
    job_revision: int | None = None


class ReviewActionRequest(AppStruct):
    expected_review_revision: int
    expected_catalog_revision: int
    expected_identity_revision: int | None = None
    expected_evidence_revision: str | None = None
    idempotency_key: str | None = None
    confirmation: bool = False


class CandidateAcceptanceRequest(ReviewActionRequest):
    candidate_key: str = ""
    manual_override: bool = False


class ReviewActionResponse(AppStruct):
    review_id: str
    state: ReviewState
    row_revision: int
    catalog_revision: int
    action_id: str
    operation_job_id: str | None = None
    remaining_exclusion_source: str | None = None


class BulkReviewSelection(AppStruct):
    review_ids: list[str] = msgspec.field(default_factory=list)
    expected_revisions: dict[str, int] = msgspec.field(default_factory=dict)
    normalized_filter: dict[str, str] = msgspec.field(default_factory=dict)
    catalog_revision: int | None = None


class BulkReviewPreviewRequest(AppStruct):
    action: Literal["keep_tagged", "retry", "exclude", "accept_candidate"]
    selection: BulkReviewSelection
    candidate_key: str | None = None


class BulkReviewPreviewResponse(AppStruct):
    preview_token: str
    action: str
    eligible_count: int
    ineligible_count: int
    stale_count: int
    reasons: dict[str, int] = msgspec.field(default_factory=dict)
    album_count: int = 0
    track_count: int = 0
    root_count: int = 0
    crosses_policy_boundaries: bool = False
    estimated_job_count: int = 0
    playlist_reference_count: int = 0
    history_reference_count: int = 0
    requires_local_metadata_confirmation: bool = False
    common_candidate_keys: list[str] = msgspec.field(default_factory=list)


class OperationWorkResult(AppStruct):
    ordinal: int
    action: str
    state: str
    local_album_id: str | None = None
    local_track_id: str | None = None
    failure_code: str | None = None
    result: dict = msgspec.field(default_factory=dict)


class BulkReviewApplyRequest(AppStruct):
    preview_token: str
    idempotency_key: str
    action: Literal["keep_tagged", "retry", "exclude", "accept_candidate"]
    selection: BulkReviewSelection
    candidate_key: str | None = None
    confirm_local_metadata: bool = False


class RepairReportSummary(AppStruct):
    total_identities: int
    remaining_identities: int
    input_track_count: int
    playable_after_detach_track_count: int
    estimated_apply_changes: int
    catalog_snapshot_revision: int
    target_matcher_version: str
    counts_by_finding: dict[str, int] = msgspec.field(default_factory=dict)
    counts_by_reason: dict[str, int] = msgspec.field(default_factory=dict)
    album_counts_by_root: dict[str, int] = msgspec.field(default_factory=dict)
    provider_deferred_count: int = 0
    failed_evidence_count: int = 0
    purpose: str = "existing_matches"
    ready_album_count: int = 0
    mapping_candidate_count: int = 0
    exact_release_required_count: int = 0
    needs_review_count: int = 0


class OperationResponse(AppStruct):
    id: str
    kind: str
    state: OperationState
    expected_work_count: int = 0
    completed_count: int = 0
    succeeded_count: int = 0
    failed_count: int = 0
    skipped_count: int = 0
    control_request: str = "none"
    terminal_code: str | None = None
    row_revision: int = 1
    event_revision: int = 0
    created_at: float = 0.0
    updated_at: float = 0.0
    results: list[OperationWorkResult] = msgspec.field(default_factory=list)
    results_truncated: bool = False
    repair_summary: RepairReportSummary | None = None
    reidentification_candidates: list[ReviewCandidateDetail] = msgspec.field(
        default_factory=list
    )


class OperationListResponse(AppStruct):
    items: list[OperationResponse]
    next_cursor: str | None = None


class OperationControlRequest(AppStruct):
    expected_row_revision: int
    idempotency_key: str | None = None


class ReidentificationRequest(AppStruct):
    expected_album_revision: int
    expected_input_revision: str
    idempotency_key: str
    one_off_local_metadata: bool = False


class ReidentificationCandidateRequest(AppStruct):
    expected_row_revision: int
    candidate_key: str
    confirmation: bool = False


class MembershipPreviewRequest(AppStruct):
    track_ids: list[str]
    expected_album_revisions: dict[str, int]
    target_album_id: str | None = None
    title: str | None = None
    album_artist_name: str | None = None


class AutomaticGroupingPreview(AppStruct):
    local_album_id: str
    title: str
    album_artist_name: str
    track_ids: list[str]
    reason_code: str


class MembershipPreviewResponse(AppStruct):
    preview_token: str
    source_album_ids: list[str]
    target_album_id: str | None = None
    track_ids: list[str] = msgspec.field(default_factory=list)
    identity_conflicts: list[str] = msgspec.field(default_factory=list)
    aliases: list[str] = msgspec.field(default_factory=list)
    automatic_groups: list[AutomaticGroupingPreview] = msgspec.field(
        default_factory=list
    )
    reference_counts: dict[str, int] = msgspec.field(default_factory=dict)


class MembershipApplyRequest(MembershipPreviewRequest):
    preview_token: str = ""
    idempotency_key: str = ""
    identity_choice: Literal["detach", "retain_manual"] = "detach"


class CatalogCorrectionResponse(AppStruct):
    kind: str
    track_ids: list[str] = msgspec.field(default_factory=list)
    source_album_ids: list[str] = msgspec.field(default_factory=list)
    target_album_id: str | None = None
    surviving_artist_id: str | None = None
    retired_artist_ids: list[str] = msgspec.field(default_factory=list)
    catalog_revision: int = 0


class ArtistMergePreviewRequest(AppStruct):
    source_artist_ids: list[str]
    surviving_artist_id: str
    expected_revisions: dict[str, int]


class ArtistMergeApplyRequest(ArtistMergePreviewRequest):
    preview_token: str
    idempotency_key: str
    provider_choice: Literal["detach", "retain_survivor"] = "detach"


class RepairCreateRequest(AppStruct):
    idempotency_key: str
    root_ids: list[str] = msgspec.field(default_factory=list)
    source_matcher_version: str | None = None
    target_matcher_version: str = "feedback-fixes-v1"


class RepairEstimateResponse(AppStruct):
    identity_count: int
    selected_root_count: int
    queued_repair_count: int


class IdentityPreparationCreateRequest(AppStruct):
    idempotency_key: str
    root_ids: list[str] = msgspec.field(default_factory=list)


class IdentityPreparationEstimateResponse(AppStruct):
    album_count: int
    ready_album_count: int
    mapping_required_count: int
    exact_release_required_count: int
    selected_root_count: int
    queued_preparation_count: int


class RepairApplyRequest(AppStruct):
    expected_row_revision: int
    confirmation: bool


class RepairFindingResponse(AppStruct):
    id: str
    local_album_id: str
    evidence_id: str | None
    review_id: str | None
    finding_code: str
    reason_code: str
    confidence: str
    apply_eligible: bool
    state: str
    apply_result: str | None = None
    updated_at: float = 0.0
    row_revision: int = 1


class RepairFindingListResponse(AppStruct):
    items: list[RepairFindingResponse]
    next_cursor: str | None = None
    has_more: bool = False
