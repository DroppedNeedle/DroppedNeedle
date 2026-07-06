from datetime import datetime
from infrastructure.msgspec_fastapi import AppStruct


class StatusMessage(AppStruct):
    title: str | None = None
    messages: list[str] = []


class ActiveRequestItem(AppStruct):
    musicbrainz_id: str
    artist_name: str
    album_title: str
    requested_at: datetime
    status: str
    artist_mbid: str | None = None
    year: int | None = None
    cover_url: str | None = None
    progress: float | None = None
    eta: datetime | None = None
    size: float | None = None
    size_remaining: float | None = None
    download_status: str | None = None
    download_state: str | None = None
    status_messages: list[StatusMessage] | None = None
    error_message: str | None = None
    library_queue_id: int | None = None
    quality: str | None = None
    protocol: str | None = None
    download_client: str | None = None
    user_id: str | None = None
    requested_by_name: str | None = None


class RequestHistoryItem(AppStruct):
    musicbrainz_id: str
    artist_name: str
    album_title: str
    requested_at: datetime
    status: str
    artist_mbid: str | None = None
    year: int | None = None
    cover_url: str | None = None
    completed_at: datetime | None = None
    in_library: bool = False
    user_id: str | None = None
    requested_by_name: str | None = None
    reviewed_by_name: str | None = None
    reviewed_at: datetime | None = None
    download_task_id: str | None = None
    can_reimport: bool = False


class ActiveRequestsResponse(AppStruct):
    items: list[ActiveRequestItem]
    count: int


class RequestHistoryResponse(AppStruct):
    items: list[RequestHistoryItem]
    total: int
    page: int
    page_size: int
    total_pages: int


class CancelRequestResponse(AppStruct):
    success: bool
    message: str


class RetryRequestResponse(AppStruct):
    success: bool
    message: str


class ClearHistoryResponse(AppStruct):
    success: bool


class ActiveCountResponse(AppStruct):
    count: int


class ApprovalActionResponse(AppStruct):
    success: bool
    message: str


class AutoDownloadApprovalItem(AppStruct):
    user_id: str
    artist_mbid: str
    artist_name: str
    requested_at: float  # epoch seconds
    user_name: str | None = None


class AutoDownloadApprovalsResponse(AppStruct):
    items: list[AutoDownloadApprovalItem]
    count: int


class PersonalMixApprovalItem(AppStruct):
    user_id: str
    requested_at: float  # epoch seconds
    user_name: str | None = None


class PersonalMixApprovalsResponse(AppStruct):
    items: list[PersonalMixApprovalItem]
    count: int


class WantedWatchItem(AppStruct):
    """One album-level availability watch (Wanted plan §6 Phase 2).

    ``kind``: 'missing' | 'partial'. ``state``: 'watching' | 'dormant' |
    'stopped' | 'fulfilled'. ``last_outcome``: no_results | seen_only |
    new_manual | auto_dispatched | satisfied | error | None (never checked).
    Timestamps are epoch seconds."""

    release_group_mbid: str
    artist_name: str
    album_title: str
    kind: str
    state: str
    check_count: int
    next_check_at: float
    new_candidate_count: int
    created_at: float
    artist_mbid: str | None = None
    year: int | None = None
    cover_url: str | None = None
    first_release_date: str | None = None
    last_checked_at: float | None = None
    last_outcome: str | None = None
    user_id: str | None = None
    # display name of the requester the watch acts for; resolved only for admin
    # callers (the "watched by" chip - non-admins only ever see their own watches)
    user_name: str | None = None


class WantedRetryingItem(AppStruct):
    """A request still in its auto-retry ladder, shown read-only on the Wanted
    tab before the watcher takes over. ``retry_count`` is retries already spent;
    the upcoming attempt is ``retry_count + 1`` of ``max_attempts``."""

    release_group_mbid: str
    artist_name: str
    album_title: str
    retry_count: int
    max_attempts: int
    next_retry_at: float  # epoch seconds
    artist_mbid: str | None = None
    year: int | None = None
    cover_url: str | None = None
    user_id: str | None = None
    user_name: str | None = None


class WantedWatchesResponse(AppStruct):
    items: list[WantedWatchItem]
    count: int
    # albums still auto-retrying (read-only rows; they graduate into items
    # when the ladder exhausts and the watcher enrols them)
    retrying: list[WantedRetryingItem] = []


class WantedActionResponse(AppStruct):
    success: bool
    state: str  # the watch's state after the action
