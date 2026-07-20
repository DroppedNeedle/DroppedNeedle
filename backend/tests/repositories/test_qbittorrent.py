"""QbittorrentDownloadClient: state mapping, enqueue correlation-by-tag, the
private-tracker cancel rule (never delete a completed torrent), and path remap."""

from pathlib import Path
import httpx
from unittest.mock import AsyncMock

import pytest

from repositories.protocols.download_client import EnqueueRequest, TaskHandle
from repositories.qbittorrent.qbittorrent_client import QbittorrentApiError, QbittorrentClient
from repositories.qbittorrent.qbittorrent_download_client import (
    QbittorrentDownloadClient,
    _map_status,
)
from repositories.qbittorrent.qbittorrent_models import QbtTorrentInfo


def _info(**kw) -> QbtTorrentInfo:
    base = dict(
        hash="abc123", name="droppedneedle-t1-0", state="downloading", progress=0.4,
        size=1000, downloaded=400, category="droppedneedle", tags="droppedneedle-t1-0",
        content_path="/data/torrents/droppedneedle/Album", save_path="/data/torrents/droppedneedle",
    )
    base.update(kw)
    return QbtTorrentInfo(**base)


def _client(infos=None):
    api = AsyncMock()
    api.torrents_info.return_value = infos if infos is not None else []
    api.delete_torrents.return_value = True
    return QbittorrentDownloadClient(
        api, "http://qbt:8080", "api-key", Path("/qbittorrent-downloads")
    ), api


# --- state mapping ----------------------------------------------------------------

@pytest.mark.parametrize(
    ("state", "progress", "expected", "active"),
    [
        ("downloading", 0.4, "downloading", True),
        ("forcedDL", 0.4, "downloading", True),
        ("stalledDL", 0.4, "queued", False),
        ("queuedDL", 0.0, "queued", False),
        ("pausedDL", 0.4, "queued", False),
        ("metaDL", 0.0, "queued", False),
        ("checkingDL", 0.9, "processing", False),
        ("moving", 0.9, "processing", False),
        ("error", 0.4, "failed", False),
        ("missingFiles", 0.4, "failed", False),
        ("uploading", 1.0, "completed", False),
        ("stalledUP", 1.0, "completed", False),
        ("pausedUP", 1.0, "completed", False),
    ],
)
def test_map_status(state, progress, expected, active):
    status = _map_status(_info(state=state, progress=progress))
    assert status.status == expected
    assert status.has_active_transfer is active
    assert status.matched_transfers == 1


def test_completed_reports_full_bytes_while_seeding():
    status = _map_status(_info(state="uploading", progress=1.0))
    assert status.progress_percent == 100.0
    assert status.bytes_downloaded == status.bytes_total == 1000


# --- enqueue / correlation ---------------------------------------------------------

@pytest.mark.asyncio
async def test_enqueue_correlates_by_tag_and_returns_hash():
    client, api = _client()
    api.torrents_info.side_effect = [[], [_info()]]
    handle = await client.enqueue(
        EnqueueRequest(
            task_id="t1", source="torrent", magnet_uri="magnet:?xt=x",
            job_name="droppedneedle-t1-0", category="droppedneedle",
        )
    )
    assert handle.source == "torrent"
    assert handle.external_id == "abc123"
    assert handle.correlation_id == "droppedneedle-t1-0"
    assert handle.job_name == "droppedneedle-t1-0"
    add_kwargs = api.add_torrent.await_args.kwargs
    assert add_kwargs["tag"] == "droppedneedle-t1-0"
    assert add_kwargs["category"] == "droppedneedle"


@pytest.mark.asyncio
async def test_enqueue_without_link_raises():
    client, _ = _client()
    with pytest.raises(QbittorrentApiError):
        await client.enqueue(EnqueueRequest(task_id="t1", source="torrent"))


# --- cancel: the private-tracker rule ----------------------------------------------

@pytest.mark.asyncio
async def test_cancel_completed_torrent_never_deletes():
    client, api = _client([_info(state="uploading", progress=1.0)])
    assert await client.cancel(TaskHandle(source="torrent", torrent_hash="abc123")) is True
    api.delete_torrents.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_incomplete_torrent_deletes_with_files():
    client, api = _client([_info(state="downloading", progress=0.4)])
    assert await client.cancel(TaskHandle(source="torrent", torrent_hash="abc123")) is True
    api.delete_torrents.assert_awaited_once_with("abc123", delete_files=True)


# --- path remap --------------------------------------------------------------------

def test_local_path_strips_save_path_prefix():
    client, _ = _client()
    local = client._local_path(_info())
    assert local == Path("/qbittorrent-downloads/Album")


def test_local_path_falls_back_to_basename_on_prefix_mismatch():
    client, _ = _client()
    local = client._local_path(_info(save_path="/somewhere/else"))
    assert local == Path("/qbittorrent-downloads/Album")


@pytest.mark.asyncio
async def test_get_status_unmatched_reports_zero_transfers():
    client, _ = _client([])
    status = await client.get_status(TaskHandle(source="torrent", job_name="nope"))
    assert status.status == "queued"
    assert status.matched_transfers == 0


@pytest.mark.asyncio
async def test_raw_client_sends_bearer_header_on_every_request():
    seen = []

    def handler(request):
        seen.append(request.headers.get("Authorization"))
        return httpx.Response(200, text="v5.2.1")

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        client = QbittorrentClient(http, "http://qbt", "secret")
        assert await client.version() == "v5.2.1"
    assert seen == ["Bearer secret"]


@pytest.mark.asyncio
async def test_raw_client_rejects_http_200_fails_add_response():
    def handler(request):
        return httpx.Response(200, text="Fails.")

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        client = QbittorrentClient(http, "http://qbt", "secret")
        with pytest.raises(QbittorrentApiError):
            await client.add_torrent(urls="magnet:?xt=x")


@pytest.mark.asyncio
async def test_health_rejects_qbittorrent_before_5_2():
    client, api = _client()
    api.version.return_value = "v5.1.2"
    status = await client.health_check()
    assert status.status == "error"
    assert "5.2" in status.message


@pytest.mark.asyncio
async def test_raw_client_marks_invalid_api_key_as_auth_error():
    def handler(request):
        return httpx.Response(401, text="Unauthorized")

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        client = QbittorrentClient(http, "http://qbt", "bad")
        with pytest.raises(QbittorrentApiError) as exc:
            await client.version()
    assert exc.value.auth is True
