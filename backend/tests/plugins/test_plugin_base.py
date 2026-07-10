"""Plugin adapter conformance: the generic ``PluginIndexerAdapter`` /
``PluginDownloadClientAdapter`` satisfy ``IndexerProtocol`` /
``DownloadClientProtocol`` at name, signature AND async-ness level (the same
contract the slskd/SABnzbd implementations are held to), so a third-party
plugin's search/acquire sides plug into the orchestrator with zero
``services/native`` changes."""

import inspect
from pathlib import Path

import pytest

from plugins.base import (
    AcquisitionPlugin,
    Candidate,
    DownloadTaskStatus,
    EnqueueRequest,
    PluginDownloadClientAdapter,
    PluginIndexerAdapter,
    SearchRequest,
    TaskHandle,
    UsenetRelease,
)
from plugins.base import TestResult as PluginTestResult  # aliased: pytest must not collect it
from repositories.protocols.download_client import DownloadClientProtocol
from repositories.protocols.indexer import IndexerProtocol

_CLIENT_METHODS = (
    "is_configured",
    "health_check",
    "enqueue",
    "get_status",
    "cancel",
    "list_completed_files",
    "get_file_path",
    "diagnose_downloads_mount",
)

_INDEXER_METHODS = (
    "is_configured",
    "health_check",
    "search_album",
    "search_track",
)


class _MinimalPlugin(AcquisitionPlugin):
    id = "minimal"
    name = "Minimal"
    version = "0.1.0"

    def settings_schema(self):
        return []

    def configure(self, settings):
        return None

    async def test_connection(self, settings=None):
        return PluginTestResult(ok=True, message="ok", version="0.1.0")

    async def search(self, request: SearchRequest) -> list[Candidate]:
        return [
            Candidate(
                source=self.id,
                usenet=UsenetRelease(
                    indexer_id=self.id,
                    indexer_name=self.name,
                    guid="g",
                    title=f"{request.artist_name} - {request.album_title or request.track_title}",
                    nzb_url="mock://g",
                ),
            )
        ]

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        return TaskHandle(source=self.id, job_name=request.job_name or request.task_id)

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        return DownloadTaskStatus(task_id="", status="completed", progress_percent=100.0)

    async def cancel(self, handle: TaskHandle) -> bool:
        return True

    async def completed_path(self, handle: TaskHandle) -> list[Path]:
        return [Path("/mock") / handle.job_name]


def test_download_client_adapter_conforms():
    adapter = PluginDownloadClientAdapter(_MinimalPlugin())

    assert isinstance(adapter, DownloadClientProtocol)
    assert adapter.client_name == "minimal"

    for name in _CLIENT_METHODS:
        proto_fn = getattr(DownloadClientProtocol, name)
        impl_fn = getattr(type(adapter), name)
        assert inspect.signature(impl_fn) == inspect.signature(proto_fn), name
        assert inspect.iscoroutinefunction(impl_fn) == inspect.iscoroutinefunction(
            proto_fn
        ), name


def test_indexer_adapter_conforms():
    adapter = PluginIndexerAdapter(_MinimalPlugin())

    assert isinstance(adapter, IndexerProtocol)
    assert adapter.indexer_name == "minimal"

    for name in _INDEXER_METHODS:
        proto_fn = getattr(IndexerProtocol, name)
        impl_fn = getattr(type(adapter), name)
        assert inspect.signature(impl_fn) == inspect.signature(proto_fn), name
        assert inspect.iscoroutinefunction(impl_fn) == inspect.iscoroutinefunction(
            proto_fn
        ), name


@pytest.mark.asyncio
async def test_adapters_delegate_round_trip():
    plugin = _MinimalPlugin()
    indexer = plugin.get_indexer()
    client = plugin.get_download_client()

    results = await indexer.search_album("Artist", "Album", 2020, 10)
    assert len(results) == 1
    assert results[0].source == "minimal"
    assert results[0].usenet.title == "Artist - Album"

    results = await indexer.search_track("Artist", "Track")
    assert results[0].usenet.title == "Artist - Track"

    handle = await client.enqueue(
        EnqueueRequest(task_id="t1", source="minimal", job_name="job-1")
    )
    assert handle.job_name == "job-1"
    status = await client.get_status(handle)
    assert status.status == "completed"
    files = await client.list_completed_files(handle)
    assert files == [Path("/mock/job-1")]
    assert await client.get_file_path(handle, "x") is None
    assert (await client.diagnose_downloads_mount()).supported is False
    health = await client.health_check()
    assert health.status == "ok"
    assert await client.cancel(handle) is True
