"""Request/response DTOs for the download-client + search + quarantine routes (Phase 6)."""

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
    user whether structured music search will be used or the ``t=search`` fallback."""

    valid: bool
    version: str | None = None
    message: str = ""
    supports_audio_search: bool = False
    category_count: int = 0


class IndexerSavedResponse(AppStruct):
    id: str


class SabnzbdTestResponse(AppStruct):
    """Result of testing SABnzbd: version + the category list (for the picker) + the
    SABnzbd-side completed dir (the mount hint)."""

    valid: bool
    version: str | None = None
    message: str = ""
    categories: list[str] = msgspec.field(default_factory=list)
    complete_dir: str | None = None


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


class SearchJobResponse(AppStruct):
    job_id: str
    status: str
    artist_name: str
    album_title: str
    candidate_count: int
    top_score: float | None = None
    candidates: list[ScoredCandidate] = msgspec.field(default_factory=list)


class PickRequest(AppStruct):
    candidate_index: int


class PickResponse(AppStruct):
    task_id: str


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


class HeldImportResponse(AppStruct):
    """A downloaded track held for an "import anyway" review: the audio matched the track by
    duration, but the AcoustID recording-identity check disagreed - usually because the
    recording's MusicBrainz metadata is wrong. ``evidence_*`` is what AcoustID thought it
    was, shown so the human can decide with the facts in front of them."""

    id: int
    release_group_mbid: str | None
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
    source: str
    source_task_id: str | None
    created_at: float
    evidence_title: str | None = None
    evidence_artist: str | None = None
    evidence_score: float | None = None


class HeldListResponse(AppStruct):
    items: list[HeldImportResponse]


class HeldActionResponse(AppStruct):
    status: str
    final_path: str | None = None


class DownloadListResponse(AppStruct):
    items: list[DownloadTaskResponse]
    page: int
    page_size: int


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
    status: str  # "queued" | "already_in_library"
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
