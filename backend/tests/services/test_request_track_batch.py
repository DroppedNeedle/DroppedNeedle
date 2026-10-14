"""RequestService.request_track_batch over a real RequestHistoryStore (#204)."""

from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ValidationError
from infrastructure.persistence.request_history import RequestHistoryStore
from services.native.download_service import ALREADY_IN_LIBRARY
from services.request_service import MAX_TRACK_BATCH, RequestService
from tests.helpers import make_builtin_dispatcher


def _service(tmp_path, *, quota=None):
    store = RequestHistoryStore(tmp_path / "library.db")
    downloads = MagicMock()
    task_ids = iter(f"task-{i}" for i in range(10_000))
    downloads.request_track = AsyncMock(side_effect=lambda **_: next(task_ids))
    downloads.request_album = AsyncMock(return_value="album-task")
    downloads.cancel_task = AsyncMock()
    get_ds = lambda: downloads  # noqa: E731
    service = RequestService(
        store, get_download_service=get_ds, acquisition=make_builtin_dispatcher(get_ds)
    )
    service._quota = quota
    return service, store, downloads


def _item(i, **extra):
    return {
        "recording_mbid": f"REC-{i:04d}",
        "artist_name": "Creedence Clearwater Revival",
        "track_title": f"Song {i}",
        "album_title": "Willy and the Poor Boys",
        "duration_seconds": 140 + i,
        "release_group_mbid": "rg-willy",
        **extra,
    }


@pytest.mark.asyncio
async def test_trusted_batch_records_and_dispatches_each_track(tmp_path):
    service, store, downloads = _service(tmp_path)

    response = await service.request_track_batch(
        [_item(1), _item(2, release_group_mbid="rg-bayou")],
        user_id="u1",
        user_role="trusted",
        requested_by_name="Pat",
    )

    assert (response.requested, response.skipped, response.status) == (2, 0, "pending")
    calls = {
        c.kwargs["recording_mbid"]: c.kwargs
        for c in downloads.request_track.await_args_list
    }
    assert calls["rec-0001"]["release_group_mbid"] == "rg-willy"
    assert calls["rec-0002"]["release_group_mbid"] == "rg-bayou"
    assert calls["rec-0001"]["track_title"] == "Song 1"
    record = await store.async_get_record("rec-0002", request_kind="track")
    assert record.request_kind == "track"
    assert record.track_release_group_mbid == "rg-bayou"
    assert record.duration_seconds == 142
    assert record.download_task_id is not None
    assert record.status == "pending"


@pytest.mark.asyncio
async def test_active_track_requests_are_skipped_and_listened_to(tmp_path):
    service, store, downloads = _service(tmp_path)
    await service.request_track_batch([_item(1)], user_id="u1", user_role="trusted")
    downloads.request_track.reset_mock()

    response = await service.request_track_batch(
        [_item(1), _item(2)], user_id="u2", user_role="trusted"
    )

    assert (response.requested, response.skipped) == (1, 1)
    assert [
        c.kwargs["recording_mbid"] for c in downloads.request_track.await_args_list
    ] == ["rec-0002"]
    assert await store.async_requester_count("rec-0001", request_kind="track") == 2


@pytest.mark.asyncio
async def test_album_request_with_same_mbid_does_not_mask_a_track(tmp_path):
    """Track keys live under ``track:``; a bare-MBID check would conflate them."""
    service, _store, downloads = _service(tmp_path)
    await service.request_album(
        "rec-0001", artist="A", album="B", user_id="u1", user_role="trusted"
    )

    response = await service.request_track_batch(
        [_item(1)], user_id="u1", user_role="trusted"
    )

    assert response.requested == 1
    downloads.request_track.assert_awaited_once()


@pytest.mark.asyncio
async def test_user_role_awaits_approval_without_dispatch(tmp_path):
    service, store, downloads = _service(tmp_path)

    response = await service.request_track_batch(
        [_item(1), _item(2)], user_id="u1", user_role="user"
    )

    assert (response.requested, response.status) == (2, "awaiting_approval")
    downloads.request_track.assert_not_awaited()
    record = await store.async_get_record("rec-0001", request_kind="track")
    assert record.status == "awaiting_approval"
    # The approval path dispatches from the stored row: the chosen album must be on it.
    assert record.track_release_group_mbid == "rg-willy"


@pytest.mark.asyncio
async def test_duplicates_and_blank_ids_are_skipped(tmp_path):
    service, _store, downloads = _service(tmp_path)
    response = await service.request_track_batch(
        [
            _item(1),
            {**_item(1), "recording_mbid": "rec-0001"},
            _item(2, recording_mbid=" "),
        ],
        user_id="u1",
        user_role="trusted",
    )
    assert (response.requested, response.skipped) == (1, 2)
    assert downloads.request_track.await_count == 1


@pytest.mark.asyncio
async def test_batch_is_capped_and_reports_overflow(tmp_path):
    service, store, _downloads = _service(tmp_path)
    response = await service.request_track_batch(
        [_item(i) for i in range(MAX_TRACK_BATCH + 2)], user_id="u1", user_role="user"
    )
    assert (response.requested, response.overflow) == (MAX_TRACK_BATCH, 2)
    assert await store.async_get_record("rec-0501", request_kind="track") is None


@pytest.mark.asyncio
async def test_quota_charged_once_for_new_items_and_rejection_records_nothing(tmp_path):
    quota = MagicMock()
    quota.check_request_quota = AsyncMock(side_effect=ValidationError("over quota"))
    quota.check_storage_admission = AsyncMock()
    service, store, downloads = _service(tmp_path, quota=quota)

    with pytest.raises(ValidationError):
        await service.request_track_batch(
            [_item(1), _item(2), _item(3)], user_id="u1", user_role="user"
        )

    quota.check_request_quota.assert_awaited_once_with("u1", "user", 3)
    assert await store.async_get_record("rec-0001", request_kind="track") is None
    downloads.request_track.assert_not_awaited()


@pytest.mark.asyncio
async def test_already_in_library_and_per_item_failures(tmp_path):
    service, store, downloads = _service(tmp_path)

    async def dispatch(**kwargs):
        recording = kwargs["recording_mbid"]
        if recording == "rec-0001":
            return ALREADY_IN_LIBRARY
        if recording == "rec-0002":
            raise ValidationError("recording not on the chosen edition")
        return "task-3"

    downloads.request_track = AsyncMock(side_effect=dispatch)

    response = await service.request_track_batch(
        [_item(1), _item(2), _item(3)], user_id="u1", user_role="trusted"
    )

    assert response.requested == 1
    assert "1 already in library" in response.message
    assert (
        await store.async_get_record("rec-0001", request_kind="track")
    ).status == "imported"
    assert (
        await store.async_get_record("rec-0002", request_kind="track")
    ).status == "failed"
    third = await store.async_get_record("rec-0003", request_kind="track")
    assert third.download_task_id == "task-3"


@pytest.mark.asyncio
async def test_repeat_batch_reports_already_requested(tmp_path):
    service, _store, downloads = _service(tmp_path)
    await service.request_track_batch(
        [_item(1), _item(2)], user_id="u1", user_role="trusted"
    )
    response = await service.request_track_batch(
        [_item(1), _item(2)], user_id="u1", user_role="trusted"
    )
    assert (response.requested, response.skipped, response.status) == (
        0,
        2,
        "already_requested",
    )
    assert downloads.request_track.await_count == 2


@pytest.mark.asyncio
async def test_edition_carrying_the_recording_is_requested_and_recorded(tmp_path):
    """Without an edition the downloader uses the album's default one, which
    may not carry the recording and makes it refuse the request (#204)."""
    service, store, downloads = _service(tmp_path)

    await service.request_track_batch(
        [_item(1, release_mbid="rel-1969"), _item(2)],
        user_id="u1",
        user_role="trusted",
    )

    calls = {
        c.kwargs["recording_mbid"]: c.kwargs
        for c in downloads.request_track.await_args_list
    }
    assert calls["rec-0001"]["release_mbid"] == "rel-1969"
    assert calls["rec-0002"]["release_mbid"] is None
    record = await store.async_get_record("rec-0001", request_kind="track")
    assert record.release_mbid == "rel-1969"
