"""task-049: approve dispatches the native pipeline via DownloadService.request_album
and links download_task_id, replacing the retired request_queue hop."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from services.requests_page_service import RequestsPageService


def _make(record_status="awaiting_approval", *, request_album_result="task-9", download_task_id=None):
    request_history = MagicMock()
    request_history.async_get_record = AsyncMock(
        return_value=SimpleNamespace(
            status=record_status,
            album_title="OK Computer",
            artist_name="Radiohead",
            year=1997,
            user_id="u1",
            download_task_id=download_task_id,
        )
    )
    request_history.async_record_review = AsyncMock()
    request_history.async_update_download_task_id = AsyncMock()
    request_history.async_update_status = AsyncMock()

    download_service = MagicMock()
    download_service.request_album = AsyncMock(return_value=request_album_result)
    download_service.cancel_task = AsyncMock()

    async def _mbids() -> set[str]:
        return set()

    service = RequestsPageService(
        library_repo=MagicMock(),
        request_history=request_history,
        library_mbids_fn=_mbids,
        download_service=download_service,
    )
    return service, request_history, download_service


@pytest.mark.asyncio
async def test_approve_dispatches_download_and_links_task():
    service, history, download_service = _make()

    resp = await service.approve_request("mbid-1", "admin-id", "Admin")

    assert resp.success is True
    download_service.request_album.assert_awaited_once()
    history.async_update_download_task_id.assert_awaited_once_with("mbid-1", "task-9")


@pytest.mark.asyncio
async def test_approve_already_in_library_not_linked():
    service, history, download_service = _make(request_album_result="already_in_library")

    resp = await service.approve_request("mbid-1", "admin-id", "Admin")

    assert resp.success is True
    download_service.request_album.assert_awaited_once()
    history.async_update_download_task_id.assert_not_awaited()


@pytest.mark.asyncio
async def test_approve_rejects_non_awaiting_record():
    service, _history, download_service = _make(record_status="pending")

    resp = await service.approve_request("mbid-1", "admin-id", "Admin")

    assert resp.success is False
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_request_cancels_linked_native_task():
    service, history, download_service = _make(
        record_status="downloading", download_task_id="task-9"
    )

    resp = await service.cancel_request("mbid-1", user_id="u1", user_role="user")

    assert resp.success is True
    download_service.cancel_task.assert_awaited_once_with("task-9", "u1", "user")
    history.async_update_status.assert_awaited()


@pytest.mark.asyncio
async def test_retry_request_redispatches_native_and_links():
    service, history, download_service = _make(
        record_status="failed", download_task_id="old-task"
    )

    resp = await service.retry_request("mbid-1", user_id="u1", user_role="user")

    assert resp.success is True
    download_service.request_album.assert_awaited_once()
    history.async_update_download_task_id.assert_awaited_once_with("mbid-1", "task-9")


@pytest.mark.asyncio
async def test_sync_reconciles_request_from_native_download_task():
    """The rewritten reconciler reads the native download task (not the dead Lidarr
    queue): a failed task flips its still-active request to 'failed'."""
    record = SimpleNamespace(
        musicbrainz_id="mbid-x", status="downloading", download_task_id="task-x"
    )
    history = MagicMock()
    history.async_get_active_requests = AsyncMock(return_value=[record])
    history.async_update_status = AsyncMock()

    download_store = MagicMock()
    download_store.get_task = AsyncMock(return_value=SimpleNamespace(status="failed"))

    async def _mbids() -> set[str]:
        return set()

    service = RequestsPageService(
        library_repo=MagicMock(),
        request_history=history,
        library_mbids_fn=_mbids,
        download_store=download_store,
    )

    await service.sync_request_statuses()

    history.async_update_status.assert_awaited_once()
    assert history.async_update_status.await_args.args[:2] == ("mbid-x", "failed")
