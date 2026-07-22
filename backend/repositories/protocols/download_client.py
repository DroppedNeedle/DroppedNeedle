"""Pluggable download-client contract (the *acquire → track → locate* half of
the split, D2).

Stable boundary between ``services/native`` and any concrete download client.
The orchestrator, matcher, and file processor import only this module's
protocol types, never ``repositories/slskd``. Search now lives on
``IndexerProtocol`` (``indexer.py``); this module no longer carries
``search_*``.

Does NOT use ``from __future__ import annotations``: the conformance contract
test compares ``inspect.signature`` of the protocol methods against each
implementation, so annotations here and in every implementation must be real
objects (not strings) and identical.
"""

from pathlib import Path
from typing import Protocol, runtime_checkable

from infrastructure.msgspec_fastapi import AppStruct
from models.common import ServiceStatus


class DownloadSearchResult(AppStruct):
    username: str
    filename: str
    parent_directory: str
    size: int
    extension: str
    bitrate: int | None = None
    bit_depth: int | None = None
    sample_rate: int | None = None
    duration: float | None = None
    has_free_slot: bool = False
    upload_speed: int = 0
    # slskd's current queue depth for this peer. Optional so persisted candidate
    # blobs written before this signal was carried still decode safely.
    queue_length: int | None = None


class DownloadFileRef(AppStruct):
    username: str
    filename: str
    size: int


class EnqueueRequest(AppStruct):
    """Client-agnostic hand-off (D8). slskd reads ``source``/``files``; SABnzbd
    reads ``nzb_url``/``job_name``/``category``/``priority``/``post_processing``.
    ``job_name`` (``droppedneedle-{task_id}``) is the PRE-enqueue correlation key
    that survives a crash before ``nzo_id`` exists."""

    task_id: str
    source: str
    files: list[DownloadFileRef] = []
    nzb_url: str | None = None
    job_name: str | None = None
    category: str | None = None
    priority: int | None = None
    post_processing: int | None = None


class TaskHandle(AppStruct):
    """Client-agnostic task correlation (D8), replacing ``TaskRef``.

    slskd populates ``username`` + ``filenames`` (no batch id, C2). SABnzbd
    populates ``job_name`` BEFORE enqueue (recoverable on crash) and fills
    ``nzo_id`` after the add returns. The manifest persists the whole handle.
    """

    source: str
    username: str = ""
    filenames: list[str] = []
    job_name: str = ""
    nzo_id: str = ""


class DownloadTaskStatus(AppStruct):
    task_id: str
    status: str
    files_total: int = 0
    files_completed: int = 0
    files_failed: int = 0
    bytes_total: int = 0
    bytes_downloaded: int = 0
    progress_percent: float = 0.0
    error: str | None = None
    # Filenames whose LATEST transfer attempt succeeded (retries deduped). The
    # orchestrator imports ONLY these - a never-finished file must not reach the
    # verify/import phase, where it could only be quarantined or logged as missing.
    succeeded_filenames: list[str] = []
    # True when at least one non-terminal transfer is actively connecting or
    # moving bytes (vs. sitting in the peer's remote upload queue). The stall
    # watchdog uses this to pick the active-stall timeout vs the queued timeout.
    has_active_transfer: bool = False
    # Number of FILES with a matched transfer record (retries deduped, like every
    # tally here). 0 means the enqueue produced nothing (peer offline / silently
    # rejected) - distinct from a real transfer queued at the peer, which has a
    # record. Lets the orchestrator fail a no-show enqueue over fast instead of
    # waiting out the full queued timeout for a transfer that never existed.
    matched_transfers: int = 0


class MountDiagnosis(AppStruct):
    """Cross-check of the client's completed (not-yet-imported) downloads against
    the configured import mount, to catch a silently-misconfigured path - reachable
    but pointing elsewhere, or unreadable (PUID/GID) - before it fails downloads one
    by one. ``supported=False`` when the client can't introspect its downloads.

    ``resolvable_downloads`` / ``sampled_downloads`` are the honest signal: of a small
    sample of slskd's finished transfers, how many can actually be LOCATED under the
    mount. ``mount_has_files`` alone is fooled when the mount is a parent of the real
    downloads dir (e.g. the whole media library) - full of files, yet none of slskd's
    downloads are reachable within the file-finder's walk budget."""

    supported: bool = False
    completed_downloads: int = 0
    mount_has_files: bool = True
    resolvable_downloads: int = 0
    sampled_downloads: int = 0
    # The client's own configured downloads dir (slskd's directories.downloads), in the
    # client's namespace - shown to the user so they can match it to DroppedNeedle's mount.
    client_downloads_dir: str | None = None


@runtime_checkable
class DownloadClientProtocol(Protocol):
    """Pluggable download client contract (acquire/track/locate).

    Clients enable independently (D2/D3): the old "one active client at a time"
    assumption is gone. No ``delete_transfer`` method (DEC-1): post-import
    removal of completed transfer records is done via ``cancel(handle)``.
    """

    @property
    def client_name(self) -> str: ...

    def is_configured(self) -> bool: ...

    async def health_check(self) -> ServiceStatus: ...

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle: ...

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus: ...

    async def cancel(self, handle: TaskHandle) -> bool:
        """Cancel an in-flight task AND/OR remove its completed transfer
        records. Serves both user cancellation and post-import cleanup (DEC-1)."""
        ...

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        """Audio files of a finished job, on disk (the import source for the
        folder-based Usenet import, D18).

        slskd already knows its filenames from the search, so its implementation
        can resolve them via ``get_file_path``; SABnzbd's unpacked filenames are
        unknown until after unpack, so it enumerates the job's ``storage`` folder.
        """
        ...

    async def get_file_path(
        self,
        handle: TaskHandle,
        remote_filename: str,
        size: int | None = None,
    ) -> Path | None:
        """Local on-disk path of a completed download (the import source).

        ``size`` (the expected byte size) lets an implementation recover a file
        whose on-disk name the client sanitised. A client whose layout is
        unpredictable may return ``None``; the orchestrator then falls back to
        ``get_status``.
        """
        ...

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        """Cross-check completed downloads against the configured import mount so a
        silently-misconfigured mount (reachable but wrong, or unreadable) is caught
        proactively. Clients that can't introspect return ``supported=False``."""
        ...
