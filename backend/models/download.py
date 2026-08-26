"""Download domain models (Phase 6).

All structs are msgspec ``AppStruct`` subclasses. ``DownloadSearchResult`` is
defined on the protocol (the boundary type); the models here are the
service/persistence-layer domain types.
"""

from infrastructure.msgspec_fastapi import AppStruct
from repositories.protocols.download_client import DownloadSearchResult
from repositories.protocols.indexer import UsenetRelease


class ScoredCandidate(AppStruct):
    """A scored acquisition candidate - the convergence point for both sources
    (D7). ``source`` discriminates: a soulseek candidate is a group of ``files``
    for one ``(username, parent_directory)``; a usenet candidate carries one
    ``usenet_release``.

    ``tier`` is one of ``auto`` (final >= auto threshold), ``manual``
    (manual threshold <= final < auto), or ``rejected`` (below manual; kept in
    the ranked list for "Show all results anyway" but never auto-picked).

    Fields are defaulted (``AppStruct`` is ``kw_only``) so old persisted candidate
    blobs decode with ``source="soulseek"``/``usenet_release=None``, and existing
    readers of ``.username``/``.files`` stay correct for Soulseek and safe-but-empty
    for Usenet.
    """

    source: str = "soulseek"
    username: str = ""
    parent_directory: str = ""
    files: list[DownloadSearchResult] = []
    usenet_release: UsenetRelease | None = None
    coherence: float = 0.0
    file_confidence: float = 0.0
    final_score: float = 0.0
    tier: str = "rejected"
    # Response-only pointer into the persisted candidate list. It lets a current-policy
    # review projection reorder/filter older blobs without changing what a Pick indexes.
    candidate_index: int | None = None


class DownloadsMountStatus(AppStruct):
    """Result of the slskd-downloads bind-mount health check (C7).

    ``ok`` means the path is usable; ``move_supported`` reports whether it shares a
    rename boundary with a configured library root.

    ``reason`` is one of: ``ok``, ``not_set``, ``missing``, ``not_writable``,
    ``different_mount``, ``different_filesystem``, or ``stat_error``.
    """

    ok: bool
    move_supported: bool
    reason: str
    path: str


class TargetAlbum(AppStruct):
    """The album the user wants - the scorer's match target."""

    artist_name: str
    album_title: str
    year: int | None = None
    track_count: int | None = None
    duration_seconds: float | None = None
    release_group_mbid: str | None = None


class TargetTrack(AppStruct):
    """A single track the user wants - the TrackMatcher's match target."""

    artist_name: str
    track_title: str
    album_title: str | None = None
    duration_seconds: float | None = None
    recording_mbid: str | None = None
    release_group_mbid: str | None = None


class SearchJob(AppStruct):
    """A search job row (``search_jobs``). Candidates are stored separately in
    the ``candidates_blob`` column and exposed via the store's
    ``get_search_job_candidates``."""

    id: str
    user_id: str
    artist_name: str
    album_title: str
    year: int | None = None
    track_count: int | None = None
    release_group_mbid: str | None = None
    artist_mbid: str | None = None
    search_query: str = ""
    status: str = "searching"
    error_message: str | None = None
    created_at: float = 0.0
    completed_at: float | None = None
    updated_at: float = 0.0


class DownloadActivitySummary(AppStruct):
    """Small user-scoped projection for global activity indicators.

    ``revision`` changes only when a task or held-review row is inserted, removed,
    changes owner, or changes durable state. Progress-only writes deliberately do
    not advance it because task SSE carries progress without a full-list refetch.
    """

    revision: int
    active_count: int
    held_count: int
    failed_count: int
    landed_release_group_mbids: list[str] = []


class DownloadTask(AppStruct):
    """A download task row (``download_tasks``)."""

    id: str
    user_id: str
    download_type: str = "album"
    release_group_mbid: str = ""
    release_mbid: str | None = None
    release_track_mbid: str | None = None
    recording_mbid: str | None = None
    artist_mbid: str | None = None
    artist_name: str = ""
    album_title: str = ""
    track_title: str | None = None
    track_number: int | None = None
    disc_number: int | None = None
    year: int | None = None
    track_count: int | None = None
    track_duration_seconds: float | None = None
    download_client: str = "slskd"
    # Acquisition source ("soulseek" | "usenet"); the fixed v1 map is
    # soulseek→slskd, usenet→sabnzbd. Drives the "via album NZB" UI label and
    # source-scoped failover. Defaulted so old rows decode as soulseek.
    source: str = "soulseek"
    # Why the task exists: "user" (a request), "retry" (re-dispatch of a failed task)
    # or "upgrade" (a curator-triggered quality upgrade). Orthogonal to ``source``;
    # defaulted so old rows decode as user requests.
    origin: str = "user"
    # Requested lyrical-content edition. ``clean`` is a safety contract, not a
    # filename hint: import must fingerprint to the exact requested recording.
    content_variant: str = "original"
    source_username: str | None = None
    source_directory: str | None = None
    search_query: str | None = None
    search_job_id: str | None = None
    candidate_index: int | None = None
    status: str = "queued"
    preflight_score: float | None = None
    progress_percent: int = 0
    total_size_bytes: int | None = None
    downloaded_bytes: int = 0
    files_total: int = 0
    files_completed: int = 0
    files_failed: int = 0
    quality_format: str | None = None
    quality_bitrate: int | None = None
    quality_sample_rate: int | None = None
    quality_bit_depth: int | None = None
    # Durable source-selection state. Search-time queue depth is an advertised peer
    # signal; queue_position_* is refreshed from live slskd transfers. The preferred
    # quality deadline is an epoch timestamp so the shared resolution-pool budget
    # survives restarts.
    advertised_queue_depth: int | None = None
    queue_position_start: int | None = None
    queue_position_end: int | None = None
    remote_queued: bool = False
    preferred_quality_fallback_at: float | None = None
    quality_pool_key: str | None = None
    attempt_number: int = 0
    attempt_total: int = 0
    has_next_source: bool = False
    staging_path: str | None = None
    final_path: str | None = None
    error_message: str | None = None
    retry_count: int = 0
    last_polled_at: float | None = None
    created_at: float = 0.0
    started_at: float | None = None
    completed_at: float | None = None
    cancelled_at: float | None = None
    updated_at: float = 0.0
