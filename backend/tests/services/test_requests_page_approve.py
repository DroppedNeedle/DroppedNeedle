"""task-049: approve dispatches the native pipeline via DownloadService.request_album
and links download_task_id, replacing the retired request_queue hop."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.persistence.request_history import RequestHistoryRecord
from services.requests_page_service import RequestsPageService
from tests.helpers import make_builtin_dispatcher


def _make(
    record_status="awaiting_approval",
    *,
    request_album_result="task-9",
    request_track_result="track-task-9",
    download_task_id=None,
    request_kind="album",
):
    request_history = MagicMock()
    request_history.async_get_record = AsyncMock(
        return_value=SimpleNamespace(
            status=record_status,
            album_title="OK Computer",
            artist_name="Radiohead",
            artist_mbid="artist-mbid-1",
            year=1997,
            user_id="u1",
            download_task_id=download_task_id,
            release_mbid="release-edition",
            musicbrainz_id="mbid-1",
            request_kind=request_kind,
            track_title="Airbag" if request_kind == "track" else None,
            duration_seconds=287 if request_kind == "track" else None,
            track_release_group_mbid="release-group-1"
            if request_kind == "track"
            else None,
        )
    )
    request_history.async_record_review = AsyncMock()
    request_history.async_update_download_task_id = AsyncMock()
    request_history.async_update_status = AsyncMock()

    download_service = MagicMock()
    download_service.request_album = AsyncMock(return_value=request_album_result)
    download_service.request_track = AsyncMock(return_value=request_track_result)
    download_service.cancel_task = AsyncMock()

    async def _mbids() -> set[str]:
        return set()

    service = RequestsPageService(
        library_repo=MagicMock(),
        request_history=request_history,
        library_mbids_fn=_mbids,
        get_download_service=lambda: download_service,
        acquisition=make_builtin_dispatcher(lambda: download_service),
    )
    return service, request_history, download_service


@pytest.mark.asyncio
async def test_approve_dispatches_download_and_links_task():
    service, history, download_service = _make()
    dispatch = service._acquisition.request_album
    service._acquisition.request_album = AsyncMock(wraps=dispatch)

    resp = await service.approve_request("mbid-1", "admin-id", "Admin")

    assert resp.success is True
    download_service.request_album.assert_awaited_once()
    assert (
        download_service.request_album.await_args.kwargs["release_mbid"]
        == "release-edition"
    )
    assert (
        service._acquisition.request_album.await_args.kwargs["track_count_priority"]
        is RequestPriority.USER_INITIATED
    )
    history.async_update_download_task_id.assert_awaited_once_with("mbid-1", "task-9")


@pytest.mark.asyncio
async def test_approve_already_in_library_not_linked():
    service, history, download_service = _make(
        request_album_result="already_in_library"
    )

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
async def test_approve_exact_track_dispatches_track_without_widening_to_album():
    service, history, download_service = _make(request_kind="track")

    resp = await service.approve_request("mbid-1", "admin-id", "Admin")

    assert resp.success is True
    download_service.request_album.assert_not_awaited()
    download_service.request_track.assert_awaited_once()
    kwargs = download_service.request_track.await_args.kwargs
    assert kwargs["recording_mbid"] == "mbid-1"
    assert kwargs["track_title"] == "Airbag"
    assert kwargs["release_group_mbid"] == "release-group-1"
    history.async_update_download_task_id.assert_awaited_once_with(
        "mbid-1", "track-task-9"
    )


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
    dispatch = service._acquisition.request_album
    service._acquisition.request_album = AsyncMock(wraps=dispatch)

    resp = await service.retry_request("mbid-1", user_id="u1", user_role="user")

    assert resp.success is True
    download_service.request_album.assert_awaited_once()
    assert (
        download_service.request_album.await_args.kwargs["release_mbid"]
        == "release-edition"
    )
    assert (
        service._acquisition.request_album.await_args.kwargs["track_count_priority"]
        is RequestPriority.USER_INITIATED
    )
    history.async_update_download_task_id.assert_awaited_once_with("mbid-1", "task-9")


@pytest.mark.asyncio
async def test_retry_exact_track_preserves_exact_track_semantics():
    service, history, download_service = _make(
        record_status="failed",
        request_kind="track",
        download_task_id="old-track-task",
    )

    resp = await service.retry_request("mbid-1", user_id="u1", user_role="user")

    assert resp.success is True
    download_service.request_album.assert_not_awaited()
    download_service.request_track.assert_awaited_once()
    assert download_service.request_track.await_args.kwargs["recording_mbid"] == "mbid-1"
    history.async_update_download_task_id.assert_awaited_once_with(
        "mbid-1", "track-task-9"
    )


def test_pending_exact_track_response_exposes_kind_title_and_release_artwork():
    item = RequestsPageService._build_pending_item(
        RequestHistoryRecord(
            musicbrainz_id="recording-1",
            artist_name="Radiohead",
            album_title="OK Computer",
            requested_at="2026-08-24T12:00:00+00:00",
            status="awaiting_approval",
            request_kind="track",
            track_title="Airbag",
            duration_seconds=287,
            track_release_group_mbid="7b0032d0-09b3-4f21-a207-9eb26b746c4f",
        )
    )

    assert item.request_kind == "track"
    assert item.track_title == "Airbag"
    assert item.duration_seconds == 287
    assert item.track_release_group_mbid == "7b0032d0-09b3-4f21-a207-9eb26b746c4f"
    assert "7b0032d0-09b3-4f21-a207-9eb26b746c4f" in (item.cover_url or "")


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


@pytest.mark.asyncio
async def test_approve_over_cap_returns_to_approval_queue_with_reason():
    """Feature C: a cap/quota rejection at approve time must NOT swallow the request
    into 'failed' (it silently vanished from every view) - it goes back to
    awaiting_approval and the admin sees the actual reason."""
    from core.exceptions import ValidationError

    service, history, download_service = _make()
    download_service.request_album = AsyncMock(
        side_effect=ValidationError("Library storage limit reached (12.0 / 10 GB)")
    )

    resp = await service.approve_request("mbid-1", "admin-id", "Admin")

    assert resp.success is False
    assert "Library storage limit reached" in resp.message
    history.async_update_status.assert_awaited_once_with("mbid-1", "awaiting_approval")


@pytest.mark.asyncio
async def test_retry_over_cap_restores_prior_status_with_reason():
    from core.exceptions import ValidationError

    service, history, download_service = _make(record_status="failed")
    download_service.request_album = AsyncMock(
        side_effect=ValidationError("Your storage budget is full (5.0 / 5 GB)")
    )

    resp = await service.retry_request("mbid-1", user_id="u1", user_role="admin")

    assert resp.success is False
    assert "storage budget" in resp.message
    # flipped to 'pending' for the attempt, then restored to the pre-retry status
    assert history.async_update_status.await_args_list[-1].args == ("mbid-1", "failed")
