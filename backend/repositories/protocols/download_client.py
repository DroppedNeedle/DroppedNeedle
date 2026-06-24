"""Pluggable download-client contract.

Stable boundary between ``services/native`` and any concrete download client.
The orchestrator, matcher, and file processor import only this module's
protocol types, never ``repositories/slskd``.

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


class DownloadFileRef(AppStruct):
    username: str
    filename: str
    size: int


class TaskRef(AppStruct):
    """slskd has no batch id, so a task is correlated to its transfers by the
    peer username plus the exact set of filenames it enqueued (C2)."""

    username: str
    filenames: list[str]


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
    # Filenames whose transfer has succeeded so far. Lets the orchestrator import
    # the already-finished subset of a stalled task without touching files that
    # never arrived (which would otherwise be quarantined as verify failures).
    succeeded_filenames: list[str] = []
    # True when at least one non-terminal transfer is actively connecting or
    # moving bytes (vs. sitting in the peer's remote upload queue). The stall
    # watchdog uses this to pick the active-stall timeout vs the queued timeout.
    has_active_transfer: bool = False


@runtime_checkable
class DownloadClientProtocol(Protocol):
    """Pluggable download client contract. One active client at a time.

    No ``delete_transfer`` method (DEC-1): post-import removal of completed
    transfer records is done via ``cancel(task_ref)``.
    """

    @property
    def client_name(self) -> str: ...

    def is_configured(self) -> bool: ...

    async def health_check(self) -> ServiceStatus: ...

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[DownloadSearchResult]: ...

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[DownloadSearchResult]: ...

    async def enqueue(self, files: list[DownloadFileRef]) -> TaskRef: ...

    async def get_status(self, task_ref: TaskRef) -> DownloadTaskStatus: ...

    async def cancel(self, task_ref: TaskRef) -> bool:
        """Cancel an in-flight task AND/OR remove its completed transfer
        records (DELETE ``?remove=true``). Serves both user cancellation and
        post-import cleanup (DEC-1)."""
        ...

    async def get_file_path(
        self,
        username: str,
        remote_filename: str,
    ) -> Path | None:
        """Local on-disk path of a completed download (the import source).

        A client whose layout is unpredictable may return ``None``; the
        orchestrator then falls back to ``get_status``.
        """
        ...
