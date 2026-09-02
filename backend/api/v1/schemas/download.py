"""Request/response DTOs for the download-client + search + quarantine routes (Phase 6)."""

from typing import Literal

import msgspec

from infrastructure.msgspec_fastapi import AppStruct
from models.common import ServiceStatus
from models.download import DownloadsMountStatus, ScoredCandidate


class TestConnectionResponse(AppStruct):
    valid: bool
    version: str | None = None
    message: str = ""


class IndexerTestResponse(AppStruct):
    """Result of testing a Newznab indexer's caps. ``supports_audio_search`` tells the
    user whether structured music search will be used or the ``t=search`` fallback.
    ``suggested_url`` is set when the URL looks like the site homepage but ``/api``
    responds as a real newznab endpoint - a 'did you mean' the UI can one-click apply."""

    valid: bool
    version: str | None = None
    message: str = ""
    supports_audio_search: bool = False
    category_count: int = 0
    suggested_url: str | None = None


class IndexerSavedResponse(AppStruct):
    id: str


class SabnzbdTestResponse(AppStruct):
    """Result of testing SABnzbd: version + the category list (for the picker) + the
    SABnzbd-side completed dir (the mount hint) + the mount diagnosis (how many
    sampled SABnzbd downloads resolve under the submitted mount, with an
    actionable message when they don't)."""

    valid: bool
    version: str | None = None
    message: str = ""
    categories: list[str] = msgspec.field(default_factory=list)
    complete_dir: str | None = None
    mount_has_files: bool | None = None
    resolvable_downloads: int | None = None
    sampled_downloads: int | None = None
    mount_message: str | None = None


class IndexerReorderRequest(AppStruct):
    ordered_ids: list[str]


class SourcePriority(AppStruct):
    """The order acquisition sources are tried (D3) - e.g. ``["soulseek", "usenet"]``."""

    order: list[str]


class DownloadClientStatusResponse(AppStruct):
    configured: bool
    client: ServiceStatus
    mount: DownloadsMountStatus
    # Set when the mount looks healthy but slskd's finished downloads aren't visible on
    # it (the silent misconfig); None when there's nothing to flag.
    mount_advisory: str | None = None
    # slskd's own configured downloads dir (its in-container path), shown as a hint so the
    # user can match it to DroppedNeedle's mount. None when slskd didn't report it.
    slskd_downloads_dir: str | None = None


class SearchAlbumRequest(AppStruct):
    artist_name: str
    album_title: str
    year: int | None = None
    track_count: int | None = None
    release_group_mbid: str | None = None


class SearchAlbumResponse(AppStruct):
    status: str  # "searching" | "already_in_library"
    job_id: str | None = None


class QualityRejectionSummary(AppStruct):
    outside_policy: int = 0
    unknown_rejected: int = 0
    not_importable: int = 0
    needs_review: int = 0


class SearchJobResponse(AppStruct):
    job_id: str
    status: str
    artist_name: str
    album_title: str
    candidate_count: int
    top_score: float | None = None
    quality_snapshot_summary: str | None = None
    quality_rejections: QualityRejectionSummary = msgspec.field(
        default_factory=QualityRejectionSummary
    )
    candidates: list[ScoredCandidate] = msgspec.field(default_factory=list)


class PickRequest(AppStruct):
    candidate_index: int


class PickResponse(AppStruct):
    task_id: str


class DismissReviewResponse(AppStruct):
    """'None of these - keep watching': the review's candidates were all rejected
    and the album is now on the wanted watchlist. ``state`` is the watch's state."""

    success: bool
    state: str


class OperationResult(AppStruct):
    success: bool


class QuarantineEntry(AppStruct):
    id: int
    client_id: str
    username: str
    filename: str
    reason: str
    quarantined_at: float
    release_group_mbid: str | None = None


class QuarantineListResponse(AppStruct):
    items: list[QuarantineEntry]
    page: int


class DownloadTaskResponse(AppStruct):
    """One download task as the queue UI consumes it (Phase 7/8)."""

    id: str
    user_id: str
    download_type: str
    # "soulseek" | "usenet" - drives the source badge + the "via album NZB" label
    # (derived as source=="usenet" && download_type=="track").
    source: str
    release_group_mbid: str
    release_mbid: str | None
    release_track_mbid: str | None
    recording_mbid: str | None
    artist_name: str
    album_title: str
    track_title: str | None
    year: int | None
    status: str
    progress_percent: int
    total_size_bytes: int | None
    downloaded_bytes: int
    files_total: int
    files_completed: int
    files_failed: int
    source_username: str | None
    search_job_id: str | None
    candidate_index: int | None
    preflight_score: float | None
    final_path: str | None
    error_message: str | None
    retry_count: int
    created_at: float
    updated_at: float
    # The task's last-attempt timestamp (None until it first reaches a terminal state).
    completed_at: float | None = None
    artist_mbid: str | None = None
    # Auto-retry hints for the queue UI: when the next attempt is due (None if it won't
    # auto-retry), and the configured attempt cap (0 when auto-retry is off).
    next_retry_at: float | None = None
    retry_max: int = 0
    # The FULL auto-retry backoff schedule, in minutes, for the configured attempt cap
    # (e.g. [15, 30, 60, 120, 240, 480]). Empty when auto-retry is off / retry_max == 0.
    retry_ladder_minutes: list[int] = []
    acquisition_cleanup_state: str = "not_tracked"
    quality_format: str | None = None
    quality_bitrate: int | None = None
    quality_bit_depth: int | None = None
    # Acquisition-quality projection (Acquisition plan): the snapshot contract,
    # stable step, evidence labels and manual-override marker for queue/review UI.
    quality_snapshot_summary: str | None = None
    quality_snapshot_hash: str | None = None
    quality_preference_step: int | None = None
    quality_certainty: str | None = None
    quality_provenance: str | None = None
    manual_quality_override: bool = False
    quality_sample_rate: int | None = None
    advertised_queue_depth: int | None = None
    queue_position_start: int | None = None
    queue_position_end: int | None = None
    remote_queued: bool = False
    preferred_quality_fallback_at: float | None = None
    attempt_number: int = 0
    attempt_total: int = 0
    has_next_source: bool = False
    held_for_review: bool = False


class PolicySummaryResponse(AppStruct):
    """Safe, signed-in-user projection of the acquisition policy (spec):
    quality summary sentence + source-mode label only - no admin internals."""

    summary: str
    source_mode: str
    legacy_rollback_compatible: bool
    quality_recipe_status: Literal["v1", "v2", "non_convertible", "invalid"] = "v1"
    quality_recipe_error: str | None = None


class PolicyImpactResponse(AppStruct):
    """Admin preview of an UNSAVED policy against persisted state (spec).
    ``legacy_representable`` reports whether a down-level image would preserve
    acquisition behaviour."""

    manual_search_jobs: int = 0
    queued_without_attempts: int = 0
    awaiting_review: int = 0
    remote_queued_zero_byte: int = 0
    transferring_immutable: int = 0
    held_reviews: int = 0
    legacy_representable: bool = True


class RestartWithPolicyRequest(AppStruct):
    """Guard against acting on a stale view: the caller echoes the stored
    hash it saw; a mismatch aborts the atomic restart."""

    expected_snapshot_hash: str | None = None


class RestartWithPolicyResponse(AppStruct):
    accepted: bool
    snapshot_summary: str | None = None
    message: str | None = None


class HeldImportResponse(AppStruct):
    """A downloaded track held for an "import anyway" review: the audio matched the track by
    duration, but the AcoustID recording-identity check disagreed - usually because the
    recording's MusicBrainz metadata is wrong. ``evidence_*`` is what AcoustID thought it
    was, shown so the human can decide with the facts in front of them."""

    id: int
    release_group_mbid: str | None
    release_mbid: str | None
    release_track_mbid: str | None
    recording_mbid: str | None
    track_number: int | None
    disc_number: int | None
    track_title: str | None
    artist_name: str | None
    album_title: str | None
    year: int | None
    original_filename: str | None
    file_format: str | None
    duration_seconds: float | None
    reason: str
    reason_detail: str | None
    source: str
    source_task_id: str | None
    created_at: float
    evidence_title: str | None = None
    evidence_artist: str | None = None
    evidence_score: float | None = None
    management_retry_count: int = 0
    management_next_retry_at: float | None = None


class HeldListResponse(AppStruct):
    items: list[HeldImportResponse]


class HeldActionResponse(AppStruct):
    status: str
    final_path: str | None = None


class HeldManagementActionResponse(AppStruct):
    status: str
    files: int


class DownloadListResponse(AppStruct):
    items: list[DownloadTaskResponse]
    page: int
    page_size: int


class DownloadActivitySummaryResponse(AppStruct):
    revision: int
    active_count: int
    held_count: int
    failed_count: int
    landed_release_group_mbids: list[str] = []


class DownloadFileItem(AppStruct):
    filename: str
    size: int
    duration: float | None = None


class DownloadFilesResponse(AppStruct):
    """Per-task file list (from the linked candidate) + aggregate progress."""

    task_id: str
    status: str
    files_total: int
    files_completed: int
    files_failed: int
    progress_percent: int
    files: list[DownloadFileItem] = msgspec.field(default_factory=list)


class CancelDownloadResponse(AppStruct):
    success: bool
    status: str = "cancelled"


class NextSourceRequest(AppStruct):
    expected_candidate_index: int


class NextSourceResponse(AppStruct):
    success: bool
    status: str
    candidate_index: int


class RetryDownloadResponse(AppStruct):
    success: bool
    task_id: str


class ClearDownloadsResponse(AppStruct):
    """Result of the queue's "Clear" bulk action: how many terminal (completed +
    cancelled) tasks were hard-deleted."""

    cleared: int


class StopRetriesResponse(AppStruct):
    """Result of "Stop all retries": how many still-scheduled auto-retries were
    stopped (cancelled)."""

    stopped: int


class RetryAllResponse(AppStruct):
    """Result of "Retry all failed": how many exhausted/non-auto-retrying failures were
    re-dispatched."""

    retried: int


class ReimportDownloadResponse(AppStruct):
    success: bool
    status: str
    files_imported: int
    files_failed: int
    error_message: str | None = None


class TrackRequestBody(AppStruct):
    artist_name: str
    track_title: str
    album_title: str | None = None
    duration_seconds: int | None = None
    release_group_mbid: str | None = None
    artist_mbid: str | None = None
    # MB RELEASE mbid (an edition): a SOFT acquisition target (D14) threaded into
    # DownloadTask.release_mbid - same value, two names (release_id on the wire).
    release_id: str | None = None


class TrackRequestResponse(AppStruct):
    status: str  # "awaiting_approval" | "queued" | "already_in_library"
    task_id: str | None = None


class CutoffUnmetItem(AppStruct):
    """One upgrade-worklist row: an album whose worst held tier is below the cutoff."""

    release_group_mbid: str
    current_tier: str
    track_count: int
    artist_name: str | None = None
    artist_mbid: str | None = None
    album_title: str | None = None
    year: int | None = None


class CutoffUnmetResponse(AppStruct):
    items: list[CutoffUnmetItem]
    cutoff: str
    upgrade_allowed: bool


class UpgradeAlbumRequestBody(AppStruct):
    release_group_mbid: str
    artist_name: str
    album_title: str
    year: int | None = None
    artist_mbid: str | None = None


class UpgradeTrackRequestBody(AppStruct):
    recording_mbid: str
    artist_name: str
    track_title: str
    album_title: str | None = None
    duration_seconds: int | None = None
    release_group_mbid: str | None = None
    artist_mbid: str | None = None


class UpgradeRequestResponse(AppStruct):
    # "queued" = an upgrade task was created (or an active one already exists);
    # "satisfied" = nothing to upgrade (at/above cutoff, or upgrades are off).
    status: str
    task_id: str | None = None
