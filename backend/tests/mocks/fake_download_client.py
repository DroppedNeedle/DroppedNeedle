"""``FakeDownloadClient`` - an in-memory second ``DownloadClientProtocol`` impl,
and ``FakeIndexer`` - an in-memory second ``IndexerProtocol`` impl.

Their purpose is the conformance contract tests: a second concrete implementation
of each protocol proves "a hypothetical second client/indexer needs zero changes
to ``services/native``", and that the boundaries are not slskd-shaped. They use no
httpx and no FastAPI.

Method signatures MUST stay byte-identical to the protocols (the contract tests
compare ``inspect.signature``), so this module deliberately does NOT use
``from __future__ import annotations``.
"""

from pathlib import Path

from models.common import ServiceStatus
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
)
from repositories.protocols.indexer import IndexerResult, UsenetRelease


class FakeDownloadClient:
    def __init__(self) -> None:
        self._enqueued: dict[str, list[str]] = {}

    @property
    def client_name(self) -> str:
        return "fake"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok", version="fake-1.0", message="fake")

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        username = request.files[0].username if request.files else "fake-peer"
        filenames = [f.filename for f in request.files]
        self._enqueued[username] = filenames
        return TaskHandle(source=request.source, username=username, filenames=filenames)

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        total = len(handle.filenames)
        return DownloadTaskStatus(
            task_id="",
            status="completed",
            files_total=total,
            files_completed=total,
            progress_percent=100.0,
        )

    async def abort(self, handle: TaskHandle) -> bool:
        self._enqueued.pop(handle.username, None)
        return True

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        return DownloadMaterialization(
            state="completed",
            mount_root="/fake/downloads",
            file_paths=[str(value) for value in await self.list_completed_files(handle)],
            mount_healthy=True,
        )

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        self._enqueued.pop(handle.username, None)
        return True

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        return [
            Path("/fake/downloads") / f.replace("\\", "/").lstrip("/")
            for f in handle.filenames
        ]

    async def get_file_path(self, handle: TaskHandle, remote_filename: str, size: int | None = None) -> Path | None:
        return Path("/fake/downloads") / remote_filename.replace("\\", "/").lstrip("/")

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return MountDiagnosis(supported=False)


class FakeIndexer:
    @property
    def indexer_name(self) -> str:
        return "fake-indexer"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok", version="fake-1.0", message="fake")

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        return [
            IndexerResult(
                source="usenet",
                usenet=UsenetRelease(
                    indexer_id="fake",
                    indexer_name="fake-indexer",
                    guid="g1",
                    title=f"{artist_name} - {album_title}",
                    nzb_url="https://idx.example/nzb/g1",
                    size_bytes=100_000_000,
                    category_ids=[3040],
                ),
            )
        ]

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        return [
            IndexerResult(
                source="usenet",
                usenet=UsenetRelease(
                    indexer_id="fake",
                    indexer_name="fake-indexer",
                    guid="g2",
                    title=f"{artist_name} - {track_title}",
                    nzb_url="https://idx.example/nzb/g2",
                    size_bytes=10_000_000,
                    category_ids=[3010],
                ),
            )
        ]
