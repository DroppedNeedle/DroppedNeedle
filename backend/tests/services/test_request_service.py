from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ExternalServiceError
from services.request_service import RequestService
from tests.helpers import make_builtin_dispatcher


def _make_service() -> tuple[RequestService, MagicMock, MagicMock]:
    request_history = MagicMock()
    download_service = MagicMock()

    request_history.async_record_request = AsyncMock()
    request_history.async_get_record = AsyncMock(return_value=None)
    request_history.async_update_status = AsyncMock()
    request_history.async_update_download_task_id = AsyncMock()
    request_history.async_bulk_record_requests = AsyncMock()
    request_history.async_get_active_mbids = AsyncMock(return_value=set())
    request_history.async_get_requested_mbids = AsyncMock(return_value=set())
    download_service.request_album = AsyncMock(return_value="task-1")
    download_service.request_track = AsyncMock(return_value="track-task-1")
    download_service.cancel_task = AsyncMock()

    get_ds = lambda: download_service  # noqa: E731
    service = RequestService(
        request_history,
        get_download_service=get_ds,
        acquisition=make_builtin_dispatcher(get_ds),
    )
    return service, request_history, download_service


@pytest.mark.asyncio
async def test_request_album_dispatches_download_and_links_task():
    service, request_history, download_service = _make_service()

    response = await service.request_album(
        "rg-123",
        artist="Fallback Artist",
        album="Fallback Album",
        year=2024,
        user_role="admin",
    )

    assert response.success is True
    assert response.message == "Request accepted"
    assert response.musicbrainz_id == "rg-123"
    assert response.status == "pending"

    download_service.request_album.assert_awaited_once()
    request_history.async_update_download_task_id.assert_awaited_once_with(
        "rg-123", "task-1"
    )
    request_history.async_record_request.assert_awaited_once()
    kwargs = request_history.async_record_request.await_args.kwargs
    assert kwargs["artist_name"] == "Fallback Artist"
    assert kwargs["album_title"] == "Fallback Album"


@pytest.mark.asyncio
async def test_request_album_canonicalizes_release_alias_before_history_and_dispatch():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        return_value=("canonical-rg", "release-edition")
    )
    service._album_service = album_service

    response = await service.request_album(
        "release-alias", artist="Artist", album="Album", user_role="admin"
    )

    assert response.musicbrainz_id == "canonical-rg"
    request_history.async_record_request.assert_awaited_once()
    assert (
        request_history.async_record_request.await_args.kwargs["musicbrainz_id"]
        == "canonical-rg"
    )
    assert (
        request_history.async_record_request.await_args.kwargs["release_mbid"]
        == "release-edition"
    )
    download_service.request_album.assert_awaited_once_with(
        user_id="",
        release_group_mbid="canonical-rg",
        artist_name="Artist",
        album_title="Album",
        year=None,
        track_count=None,
        recording_mbid=None,
        track_title=None,
        track_duration_seconds=None,
        download_type="album",
        artist_mbid=None,
        origin="user",
        release_mbid="release-edition",
        release_track_mbid=None,
    )


@pytest.mark.asyncio
async def test_newly_learned_alias_is_migrated_before_request_deduplication():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        return_value=("canonical-rg", "release-edition")
    )
    mbid_store = MagicMock()
    mbid_store.save_mbid_resolution_map = AsyncMock()
    request_history.async_canonicalize_known_release_aliases = AsyncMock(return_value=1)
    request_history.async_get_record.return_value = SimpleNamespace(
        status="awaiting_approval", monitor_artist=False
    )
    service._album_service = album_service
    service._mbid_store = mbid_store

    response = await service.request_album("release-edition", user_role="admin")

    assert response.status == "awaiting_approval"
    mbid_store.save_mbid_resolution_map.assert_awaited_once_with(
        {"release-edition": "canonical-rg"}
    )
    request_history.async_canonicalize_known_release_aliases.assert_awaited_once_with(
        ["release-edition"]
    )
    request_history.async_record_request.assert_not_awaited()
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_album_user_role_awaits_approval_without_dispatch():
    service, request_history, download_service = _make_service()

    response = await service.request_album("rg-123", user_role="user")

    assert response.status == "awaiting_approval"
    download_service.request_album.assert_not_awaited()
    request_history.async_update_download_task_id.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_track_user_role_records_exact_metadata_and_awaits_approval():
    service, request_history, download_service = _make_service()

    response = await service.request_track(
        "recording-1",
        user_id="listener-1",
        user_role="user",
        requested_by_name="Listener",
        artist_name="Radiohead",
        track_title="Airbag",
        album_title="OK Computer",
        duration_seconds=287,
        release_group_mbid="release-group-1",
        artist_mbid="artist-1",
        release_mbid="release-1",
    )

    assert response.status == "awaiting_approval"
    download_service.request_track.assert_not_awaited()
    request_history.async_record_request.assert_awaited_once_with(
        musicbrainz_id="recording-1",
        artist_name="Radiohead",
        album_title="OK Computer",
        artist_mbid="artist-1",
        user_id="listener-1",
        requested_by_name="Listener",
        release_mbid="release-1",
        initial_status="awaiting_approval",
        request_kind="track",
        track_title="Airbag",
        duration_seconds=287,
        track_release_group_mbid="release-group-1",
    )


@pytest.mark.asyncio
async def test_request_track_trusted_dispatches_exact_recording_and_links_task():
    service, request_history, download_service = _make_service()

    response = await service.request_track(
        "recording-1",
        user_id="trusted-1",
        user_role="trusted",
        artist_name="Radiohead",
        track_title="Airbag",
        album_title="OK Computer",
        release_group_mbid="release-group-1",
    )

    assert response.status == "queued"
    assert response.task_id == "track-task-1"
    download_service.request_track.assert_awaited_once()
    request_history.async_update_download_task_id.assert_awaited_once_with(
        "recording-1", "track-task-1"
    )


@pytest.mark.asyncio
async def test_request_album_already_in_library_not_linked_as_task_id():
    service, request_history, download_service = _make_service()
    download_service.request_album = AsyncMock(return_value="already_in_library")

    response = await service.request_album("rg-123", user_role="admin")

    assert response.success is True
    assert response.message == "Album is already in the library"
    request_history.async_update_download_task_id.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_album_wraps_errors():
    service, _request_history, download_service = _make_service()
    download_service.request_album = AsyncMock(side_effect=RuntimeError("boom"))

    with pytest.raises(ExternalServiceError):
        await service.request_album("rg-123", user_role="admin")


@pytest.mark.asyncio
async def test_request_batch_dispatches_each_and_links():
    service, request_history, download_service = _make_service()
    items = [
        {
            "musicbrainz_id": "rg-1",
            "artist_name": "A",
            "album_title": "B",
            "year": 2020,
        },
        {
            "musicbrainz_id": "rg-2",
            "artist_name": "C",
            "album_title": "D",
            "year": 2021,
        },
    ]
    download_service.request_album = AsyncMock(side_effect=["task-1", "task-2"])

    resp = await service.request_batch(items, user_role="admin", user_id="u1")

    assert resp.requested == 2
    assert resp.overflow == 0
    assert download_service.request_album.await_count == 2
    request_history.async_bulk_record_requests.assert_awaited_once()
    linked = {
        c.args[0]: c.args[1]
        for c in request_history.async_update_download_task_id.await_args_list
    }
    assert linked == {"rg-1": "task-1", "rg-2": "task-2"}


@pytest.mark.asyncio
async def test_request_batch_canonicalizes_aliases_and_keeps_the_source_release():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        side_effect=[
            ("canonical-1", "release-1"),
            ("canonical-2", None),
        ]
    )
    service._album_service = album_service
    download_service.request_album = AsyncMock(side_effect=["task-1", "task-2"])

    response = await service.request_batch(
        [
            {"musicbrainz_id": "release-alias", "artist_name": "A"},
            {"musicbrainz_id": "canonical-2", "artist_name": "B"},
        ],
        user_role="admin",
        user_id="u1",
    )

    assert response.requested == 2
    recorded = request_history.async_bulk_record_requests.await_args.args[0]
    assert recorded[0]["musicbrainz_id"] == "canonical-1"
    assert recorded[0]["release_mbid"] == "release-1"
    assert recorded[1]["musicbrainz_id"] == "canonical-2"
    assert recorded[1]["release_mbid"] is None
    assert (
        download_service.request_album.await_args_list[0].kwargs["release_group_mbid"]
        == "canonical-1"
    )
    assert (
        download_service.request_album.await_args_list[0].kwargs["release_mbid"]
        == "release-1"
    )


@pytest.mark.asyncio
async def test_request_batch_deduplicates_aliases_for_the_same_release_group():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        side_effect=[
            ("canonical-rg", "release-1"),
            ("canonical-rg", "release-2"),
        ]
    )
    service._album_service = album_service

    response = await service.request_batch(
        [
            {"musicbrainz_id": "release-1", "artist_name": "A"},
            {"musicbrainz_id": "release-2", "artist_name": "A"},
        ],
        user_role="admin",
        user_id="u1",
    )

    assert response.requested == 1
    assert response.skipped == 1
    recorded = request_history.async_bulk_record_requests.await_args.args[0]
    assert [item["musicbrainz_id"] for item in recorded] == ["canonical-rg"]
    download_service.request_album.assert_awaited_once()


@pytest.mark.asyncio
async def test_request_batch_uses_one_durable_alias_lookup_for_the_maximum_batch():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock()
    mbid_store = MagicMock()
    mbid_store.get_mbid_resolution_map = AsyncMock(
        return_value={"release-0": "canonical-0"}
    )
    service._album_service = album_service
    service._mbid_store = mbid_store

    response = await service.request_batch(
        [{"musicbrainz_id": f"release-{index}"} for index in range(500)],
        user_role="user",
        user_id="u1",
    )

    assert response.requested == 500
    mbid_store.get_mbid_resolution_map.assert_awaited_once()
    assert len(mbid_store.get_mbid_resolution_map.await_args.args[0]) == 500
    album_service.resolve_album_identity.assert_not_awaited()
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_batch_does_not_overwrite_an_approval_pending_request():
    service, request_history, download_service = _make_service()
    request_history.async_get_requested_mbids.return_value = {"rg-1"}

    response = await service.request_batch(
        [{"musicbrainz_id": "rg-1"}],
        user_role="admin",
        user_id="admin",
    )

    assert response.requested == 0
    assert response.skipped == 1
    request_history.async_bulk_record_requests.assert_not_awaited()
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_batch_user_role_awaits_approval_without_dispatch():
    service, _request_history, download_service = _make_service()
    items = [
        {"musicbrainz_id": "rg-1", "artist_name": "A", "album_title": "B", "year": 2020}
    ]

    resp = await service.request_batch(items, user_role="user", user_id="u1")

    assert "approval" in resp.message.lower()
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_batch_admin_cancels_all():
    service, request_history, download_service = _make_service()
    request_history.async_update_status = AsyncMock()
    request_history.async_get_record = AsyncMock(
        return_value=SimpleNamespace(user_id="bob", download_task_id=None)
    )

    response = await service.cancel_batch(
        ["rg-1", "rg-2"], user_id=None, user_role="admin"
    )

    assert response.cancelled == 2
    assert response.failed == 0
    assert response.success is True
    download_service.cancel_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_batch_cancels_linked_native_task():
    service, request_history, download_service = _make_service()
    request_history.async_update_status = AsyncMock()
    request_history.async_get_record = AsyncMock(
        return_value=SimpleNamespace(user_id="alice", download_task_id="task-9")
    )

    response = await service.cancel_batch(["rg-mine"], user_id="alice")

    assert response.cancelled == 1
    download_service.cancel_task.assert_awaited_once_with("task-9", "alice", "user")


@pytest.mark.asyncio
async def test_cancel_batch_user_only_cancels_owned_requests():
    service, request_history, download_service = _make_service()
    request_history.async_update_status = AsyncMock()
    records = {
        "rg-mine": SimpleNamespace(user_id="alice", download_task_id=None),
        "rg-theirs": SimpleNamespace(user_id="bob", download_task_id=None),
    }
    request_history.async_get_record = AsyncMock(
        side_effect=lambda mbid: records.get(mbid)
    )

    response = await service.cancel_batch(["rg-mine", "rg-theirs"], user_id="alice")

    assert response.cancelled == 1
    assert response.failed == 1
    download_service.cancel_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_batch_user_missing_record_counts_as_failed():
    service, request_history, _download = _make_service()
    request_history.async_update_status = AsyncMock()
    request_history.async_get_record = AsyncMock(return_value=None)

    response = await service.cancel_batch(["rg-unknown"], user_id="alice")

    assert response.cancelled == 0
    assert response.failed == 1
    assert response.success is False


# --- Request-count quota at submit (CollectionManagement Feature C, D20) --------


def _make_service_with_quota() -> (
    tuple[RequestService, MagicMock, MagicMock, MagicMock]
):
    service, request_history, download_service = _make_service()
    quota = MagicMock()
    quota.check_request_quota = AsyncMock()
    quota.check_storage_admission = AsyncMock()
    service._quota = quota
    return service, request_history, download_service, quota


@pytest.mark.asyncio
async def test_request_album_over_quota_rejected_before_recording():
    from core.exceptions import ValidationError

    service, request_history, _dl, quota = _make_service_with_quota()
    quota.check_request_quota.side_effect = ValidationError(
        "Request limit reached (2 per 7 days)"
    )

    with pytest.raises(ValidationError):
        await service.request_album("rg-1", user_id="u1", user_role="user")

    request_history.async_record_request.assert_not_awaited()
    quota.check_request_quota.assert_awaited_once_with("u1", "user")


@pytest.mark.asyncio
async def test_request_batch_counts_n_and_rejects_whole_batch():
    from core.exceptions import ValidationError

    service, request_history, _dl, quota = _make_service_with_quota()
    quota.check_request_quota.side_effect = ValidationError("Request limit reached")
    items = [
        {"musicbrainz_id": "rg-1"},
        {"musicbrainz_id": "rg-2"},
        {"musicbrainz_id": "rg-3"},
    ]

    with pytest.raises(ValidationError):
        await service.request_batch(items, user_id="u1", user_role="user")

    request_history.async_bulk_record_requests.assert_not_awaited()
    assert quota.check_request_quota.await_args.args == ("u1", "user", 3)


@pytest.mark.asyncio
async def test_request_batch_quota_counts_only_new_items():
    service, request_history, _dl, quota = _make_service_with_quota()
    # rg-1 is already active -> only 1 NEW ask is counted against the quota
    request_history.async_get_requested_mbids = AsyncMock(return_value={"rg-1"})
    items = [{"musicbrainz_id": "RG-1"}, {"musicbrainz_id": "rg-2", "artist_name": "A"}]

    response = await service.request_batch(items, user_id="u1", user_role="user")

    assert response.success is True
    assert quota.check_request_quota.await_args.args == ("u1", "user", 1)


@pytest.mark.asyncio
async def test_request_album_over_storage_cap_rejected_at_submit():
    """The byte caps fail fast at submit so a
    user's ask never sits in the approval queue only to die at approve time."""
    from core.exceptions import ValidationError

    service, request_history, _dl, quota = _make_service_with_quota()
    quota.check_storage_admission = AsyncMock(
        side_effect=ValidationError("Library storage limit reached (12.0 / 10 GB)")
    )

    with pytest.raises(ValidationError):
        await service.request_album("rg-1", user_id="u1", user_role="user")

    request_history.async_record_request.assert_not_awaited()
    quota.check_storage_admission.assert_awaited_once_with("u1", "user")


@pytest.mark.asyncio
async def test_request_album_resolves_download_service_per_dispatch():
    """Regression (stale-scorer bug): a settings save rebuilds the DownloadService
    singleton, so the request path must resolve it fresh at each dispatch and never
    capture an instance - else a saved quality change is ignored until restart. Uses
    DISTINCT mbids so the second call isn't short-circuited by request dedup."""
    request_history = MagicMock()
    request_history.async_record_request = AsyncMock()
    request_history.async_get_record = AsyncMock(return_value=None)
    request_history.async_update_status = AsyncMock()
    request_history.async_update_download_task_id = AsyncMock()

    ds_a, ds_b = MagicMock(), MagicMock()
    ds_a.request_album = AsyncMock(return_value="task-a")
    ds_b.request_album = AsyncMock(return_value="task-b")
    current = {"ds": ds_a}
    get_ds = lambda: current["ds"]  # noqa: E731
    service = RequestService(
        request_history,
        get_download_service=get_ds,
        acquisition=make_builtin_dispatcher(get_ds),
    )

    await service.request_album("rg-A", user_role="admin")
    current["ds"] = ds_b  # a policy save rebuilt the DownloadService singleton
    await service.request_album("rg-B", user_role="admin")

    ds_a.request_album.assert_awaited_once()  # first dispatch used the original engine
    ds_b.request_album.assert_awaited_once()  # second used the NEW one (fails if captured)
