"""Request/response DTOs for the download-client + search + quarantine routes (Phase 6)."""

import msgspec

from infrastructure.msgspec_fastapi import AppStruct
from models.common import ServiceStatus
from models.download import DownloadsMountStatus, ScoredCandidate


class TestConnectionResponse(AppStruct):
    valid: bool
    version: str | None = None
    message: str = ""


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
    # Auto-retry hints for the queue UI: when the next attempt is due (None if it won't
    # auto-retry), and the configured attempt cap (0 when auto-retry is off).
    next_retry_at: float | None = None
    retry_max: int = 0


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


class TrackRequestBody(AppStruct):
    artist_name: str
    track_title: str
    album_title: str | None = None
    duration_seconds: int | None = None
    release_group_mbid: str | None = None


class TrackRequestResponse(AppStruct):
    status: str  # "queued" | "already_in_library"
    task_id: str | None = None
