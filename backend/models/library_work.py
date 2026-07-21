"""Durable scan, identification, operation, review, and migration models."""

from __future__ import annotations

from typing import Literal

import msgspec

from infrastructure.msgspec_fastapi import AppStruct
from models.local_catalog import LocalAlbum, LocalArtist, LocalArtistCredit, LocalTrack


class IdentificationJob(AppStruct):
    id: str
    dedupe_key: str
    local_album_id: str | None = None
    local_track_id: str | None = None
    kind: Literal["automatic", "review_retry", "post_processing"] = "automatic"
    state: str = "queued"
    priority: int = 100
    enqueue_sequence: int = 0
    input_revision: str = ""
    requested_by_user_id: str | None = None
    not_before: float = 0.0
    created_at: float = 0.0


class ReviewDecision(AppStruct):
    id: str
    local_album_id: str | None = None
    local_track_id: str | None = None
    state: Literal["needs_review", "keep_tagged", "excluded", "resolved"] = (
        "needs_review"
    )
    reason_code: str = ""
    attempt_id: str | None = None
    input_revision: str = ""
    decision_revision: int = 1
    decided_by_user_id: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    decided_at: float | None = None
    row_revision: int = 1


ScanKind = Literal["incremental", "rescan_files", "policy_reconcile"]
ScanTrigger = Literal[
    "manual", "automatic", "subsonic", "startup_resume", "policy_apply"
]
ScanPhase = Literal["queued", "discovering", "indexing", "reconciling"]
ScanState = Literal[
    "queued",
    "discovering",
    "indexing",
    "reconciling",
    "pausing",
    "paused",
    "stopping",
    "completed",
    "cancelled",
    "superseded_policy_changed",
    "failed",
]


class ScanScope(AppStruct):
    root_id: str
    scope_id: str | None = None
    relative_path: str = "."
    root_path: str | None = None
    effective_policy: Literal["local_metadata", "automatic", "excluded"] = "automatic"
    policy_revision: str = ""
    estimated_count: int | None = None


class ScanRun(AppStruct):
    id: str
    kind: ScanKind
    trigger: ScanTrigger
    state: ScanState = "queued"
    phase: ScanPhase = "queued"
    requested_by_user_id: str | None = None
    aggregate_scope: str = "all"
    queued_at: float = 0.0
    started_at: float | None = None
    updated_at: float = 0.0
    terminal_at: float | None = None
    resume_phase: ScanPhase | None = None
    requested_control: Literal["none", "pause", "stop"] = "none"
    terminal_code: str | None = None
    coalesced_request_count: int = 0
    row_revision: int = 1
    event_revision: int = 0
    counters: dict[str, int] = msgspec.field(default_factory=dict)
    phase_timings: dict[str, float] = msgspec.field(default_factory=dict)


class ScanRequest(AppStruct):
    kind: ScanKind
    trigger: ScanTrigger
    scopes: list[ScanScope]
    requested_by_user_id: str | None = None
    policy_revision: str = ""


class ScanRequestResult(AppStruct):
    run_id: str
    disposition: Literal["started", "queued", "coalesced", "expanded", "conflict"]
    state: ScanState
    row_revision: int
    queued_reason: str | None = None
    conflicting_kind: ScanKind | None = None


class ScanRunSnapshot(AppStruct):
    run: ScanRun
    scopes: list[ScanScope] = msgspec.field(default_factory=list)
    counters: dict[str, int] = msgspec.field(default_factory=dict)


class ScanControlResult(AppStruct):
    run_id: str
    state: ScanState
    row_revision: int
    event_revision: int
    stream_revision: int


class OperationJob(AppStruct):
    id: str
    kind: Literal["bulk_review_apply", "repair", "explicit_reidentification"]
    requested_by_user_id: str | None = None
    state: str = "queued"
    input_catalog_revision: int | None = None
    expected_work_count: int = 0
    idempotency_key: str | None = None
    created_at: float = 0.0
    row_revision: int = 1
    event_revision: int = 0


class OperationWorkItem(AppStruct):
    ordinal: int
    expected_subject_revision: int
    expected_input_revision: str
    action: str
    idempotency_key: str
    local_album_id: str | None = None
    local_track_id: str | None = None


class RepairFinding(AppStruct):
    id: str
    local_album_id: str
    expected_album_revision: int
    finding_code: str
    confidence: str
    evidence_id: str | None = None
    expected_identity_revision: int | None = None
    reason_code: str = ""
    apply_eligible: bool = False


class ScanInventoryItem(AppStruct):
    root_id: str
    relative_path: str
    absolute_path: str
    file_size_bytes: int
    file_mtime_ns: int
    stat_revision: str
    effective_policy: Literal["local_metadata", "automatic", "excluded"]
    comparison_result: Literal[
        "new", "changed", "unchanged", "excluded", "candidate_missing"
    ]
    policy_revision: str = ""
    local_track_id: str | None = None
    scope_relative_path: str = "."


class ScannedTrackWrite(msgspec.Struct):
    artist: LocalArtist
    album: LocalAlbum
    track: LocalTrack
    credit: LocalArtistCredit
    root_id: str
    relative_path: str
    comparison_result: Literal["new", "changed"]
    grouping_context: str


class MigrationProvenance(AppStruct):
    source_kind: str
    source_key: str
    target_kind: str
    target_id: str
    source_revision: str
    imported_at: float
