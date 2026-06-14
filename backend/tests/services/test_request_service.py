from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ExternalServiceError
from services.request_service import RequestService


def _make_service(queue_add_result: dict | None = None) -> tuple[RequestService, MagicMock, MagicMock]:
    lidarr_repo = MagicMock()
    request_queue = MagicMock()
    request_history = MagicMock()

    request_queue.enqueue = AsyncMock(return_value=True)
    request_queue.get_status = MagicMock(return_value={"queue_size": 0, "processing": False})
    request_history.async_record_request = AsyncMock()
    request_history.async_get_record = AsyncMock(return_value=None)

    service = RequestService(lidarr_repo, request_queue, request_history)
    return service, request_queue, request_history


@pytest.mark.asyncio
async def test_request_album_records_history_and_returns_response():
    service, request_queue, request_history = _make_service()

    response = await service.request_album(
        "rg-123", artist="Fallback Artist", album="Fallback Album", year=2024, user_role="admin"
    )

    assert response.success is True
    assert response.message == "Request accepted"
    assert response.musicbrainz_id == "rg-123"
    assert response.status == "pending"

    request_queue.enqueue.assert_awaited_once_with("rg-123")
    request_history.async_record_request.assert_awaited_once()
    kwargs = request_history.async_record_request.await_args.kwargs
    assert kwargs["artist_name"] == "Fallback Artist"
    assert kwargs["album_title"] == "Fallback Album"


@pytest.mark.asyncio
async def test_request_album_wraps_errors():
    service, request_queue, _ = _make_service()
    request_queue.enqueue = AsyncMock(side_effect=RuntimeError("boom"))

    with pytest.raises(ExternalServiceError):
        await service.request_album("rg-123", user_role="admin")


def test_get_queue_status_returns_schema():
    service, request_queue, _ = _make_service()
    request_queue.get_status.return_value = {"queue_size": 3, "processing": True}

    status = service.get_queue_status()

    assert status.queue_size == 3
    assert status.processing is True


@pytest.mark.asyncio
async def test_cancel_batch_admin_cancels_all_without_ownership_lookup():
    service, request_queue, request_history = _make_service()
    request_queue.cancel = AsyncMock(return_value=True)
    request_history.async_update_status = AsyncMock()

    response = await service.cancel_batch(["rg-1", "rg-2"], user_id=None)

    assert response.cancelled == 2
    assert response.failed == 0
    assert response.success is True
    # Admins bypass user scoping, so ownership is never looked up.
    request_history.async_get_record.assert_not_awaited()
    assert request_queue.cancel.await_count == 2


@pytest.mark.asyncio
async def test_cancel_batch_user_only_cancels_owned_requests():
    service, request_queue, request_history = _make_service()
    request_queue.cancel = AsyncMock(return_value=True)
    request_history.async_update_status = AsyncMock()
    records = {
        "rg-mine": SimpleNamespace(user_id="alice"),
        "rg-theirs": SimpleNamespace(user_id="bob"),
    }
    request_history.async_get_record = AsyncMock(side_effect=lambda mbid: records.get(mbid))

    response = await service.cancel_batch(["rg-mine", "rg-theirs"], user_id="alice")

    assert response.cancelled == 1
    assert response.failed == 1
    # Only the request owned by alice is actually cancelled.
    request_queue.cancel.assert_awaited_once_with("rg-mine")


@pytest.mark.asyncio
async def test_cancel_batch_user_missing_record_counts_as_failed():
    service, request_queue, request_history = _make_service()
    request_queue.cancel = AsyncMock(return_value=True)
    request_history.async_update_status = AsyncMock()
    request_history.async_get_record = AsyncMock(return_value=None)

    response = await service.cancel_batch(["rg-unknown"], user_id="alice")

    assert response.cancelled == 0
    assert response.failed == 1
    assert response.success is False
    request_queue.cancel.assert_not_awaited()
