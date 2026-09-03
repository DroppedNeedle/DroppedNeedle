import asyncio
from types import SimpleNamespace
from unittest.mock import ANY, AsyncMock, MagicMock

import pytest

from core.exceptions import ExternalServiceError, ValidationError
from infrastructure.persistence.request_history import (
    RequestBeginResult,
    RequesterCancelDecision,
    RequestHistoryStore,
)
from services.request_service import RequestService
from tests.helpers import make_builtin_dispatcher


def _make_service() -> tuple[RequestService, MagicMock, MagicMock]:
    request_history = MagicMock()
    download_service = MagicMock()

    request_history.async_get_record = AsyncMock(return_value=None)
    request_history.async_record_request = AsyncMock(
        side_effect=lambda **kwargs: RequestBeginResult(
            musicbrainz_id=str(kwargs["musicbrainz_id"]),
            request_kind=str(kwargs.get("request_kind", "album")),
            generation=1,
        )
    )
    request_history.async_bulk_record_requests = AsyncMock(
        side_effect=lambda items, **_kwargs: [
            RequestBeginResult(
                musicbrainz_id=str(item["musicbrainz_id"]),
                request_kind=str(item.get("request_kind", "album")),
                generation=1,
            )
            for item in items
        ]
    )
    request_history.async_add_requester = AsyncMock(return_value=True)
    request_history.async_add_requesters = AsyncMock(return_value=1)
    request_history.async_is_requester = AsyncMock(return_value=True)
    request_history.async_requester_count = AsyncMock(return_value=1)
    request_history.async_remove_requester = AsyncMock(return_value=True)
    request_history.async_prepare_requester_cancel = AsyncMock(
        return_value=RequesterCancelDecision("cancelled", "awaiting_approval", 1)
    )
    request_history.async_get_active_mbids = AsyncMock(return_value=set())
    request_history.async_get_requested_mbids = AsyncMock(return_value=set())
    request_history.async_canonicalize_known_release_aliases = AsyncMock(return_value=1)
    request_history.async_update_monitoring_flags = AsyncMock(return_value=True)
    request_history.async_update_dispatch_authorized = AsyncMock(return_value=True)
    request_history.async_update_status = AsyncMock(return_value=True)
    request_history.async_update_download_task_id = AsyncMock(return_value=True)
    request_history.async_restore_request_status = AsyncMock(return_value=True)
    request_history.async_claim_approval = AsyncMock(
        return_value=RequestBeginResult("request", "album", 1)
    )
    request_history.async_claim_retry = AsyncMock(
        return_value=RequestBeginResult("request", "album", 1)
    )
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
        "rg-123", "task-1", request_kind="album", expected_generation=1
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

    response = await service.request_album(
        "release-edition", artist="Artist", album="Album", user_role="admin"
    )

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

    response = await service.request_album(
        "rg-123", artist="Artist", album="Album", user_role="user"
    )

    assert response.status == "awaiting_approval"
    download_service.request_album.assert_not_awaited()
    request_history.async_update_download_task_id.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_album_already_in_library_not_linked_as_task_id():
    service, request_history, download_service = _make_service()
    download_service.request_album = AsyncMock(return_value="already_in_library")

    response = await service.request_album(
        "rg-123", artist="Artist", album="Album", user_role="admin"
    )

    assert response.success is True
    assert response.message == "Album is already in the library"
    request_history.async_update_download_task_id.assert_not_awaited()
    status_call = request_history.async_update_status.await_args
    assert status_call.args[:2] == ("rg-123", "imported")
    assert status_call.kwargs["expected_generation"] == 1


@pytest.mark.asyncio
async def test_request_album_wraps_errors():
    service, request_history, download_service = _make_service()
    download_service.request_album = AsyncMock(side_effect=RuntimeError("boom"))

    with pytest.raises(ExternalServiceError):
        await service.request_album(
            "rg-123", artist="Artist", album="Album", user_role="admin"
        )

    status_call = request_history.async_update_status.await_args
    assert status_call.args[:2] == ("rg-123", "failed")
    assert status_call.kwargs["expected_generation"] == 1


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
    assert all(
        call.kwargs["expected_generation"] == 1
        for call in request_history.async_update_download_task_id.await_args_list
    )


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
            {"musicbrainz_id": "release-alias", "artist_name": "A", "album_title": "B"},
            {"musicbrainz_id": "canonical-2", "artist_name": "C", "album_title": "D"},
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
    assert all(
        call.kwargs["expected_generation"] == 1
        for call in request_history.async_update_download_task_id.await_args_list
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
            {"musicbrainz_id": "release-1", "artist_name": "A", "album_title": "B"},
            {"musicbrainz_id": "release-2", "artist_name": "A", "album_title": "B"},
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
        [
            {
                "musicbrainz_id": f"release-{index}",
                "artist_name": f"Artist {index}",
                "album_title": f"Album {index}",
            }
            for index in range(500)
        ],
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
    assert response.status == "already_requested"
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
    assert resp.status == "awaiting_approval"
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_batch_admin_cancels_all():
    service, request_history, download_service = _make_service()
    request_history.async_update_status = AsyncMock(return_value=True)
    request_history.async_get_record = AsyncMock(
        return_value=SimpleNamespace(
            status="pending",
            user_id="bob",
            download_task_id=None,
            generation=1,
            dispatch_authorized=True,
        )
    )

    response = await service.cancel_batch(
        ["rg-1", "rg-2"], user_id=None, user_role="admin"
    )

    assert response.cancelled == 2
    assert response.failed == 0
    assert response.success is True
    download_service.cancel_task.assert_not_awaited()
    request_history.async_update_status.assert_any_await(
        "rg-1",
        "cancelled",
        completed_at=ANY,
        request_kind="album",
        expected_generation=1,
    )
    request_history.async_update_status.assert_any_await(
        "rg-2",
        "cancelled",
        completed_at=ANY,
        request_kind="album",
        expected_generation=1,
    )
    assert request_history.async_update_status.await_count == 2


@pytest.mark.asyncio
async def test_cancel_batch_without_user_identity_cannot_use_admin_override():
    service, request_history, download_service = _make_service()
    request_history.async_get_record = AsyncMock(
        return_value=SimpleNamespace(
            status="pending",
            user_id="owner",
            download_task_id="task-owned",
            generation=1,
        )
    )
    request_history.async_prepare_requester_cancel.return_value = RequesterCancelDecision(
        "denied", "pending", 1
    )

    response = await service.cancel_batch(
        ["rg-owned"], user_id=None, user_role="user"
    )

    assert response.success is False
    assert response.cancelled == 0
    assert response.failed == 1
    request_history.async_update_status.assert_not_awaited()
    download_service.cancel_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_batch_cancels_linked_native_task():
    service, request_history, download_service = _make_service()
    request_history.async_update_status = AsyncMock(return_value=True)
    request_history.async_get_record = AsyncMock(
        return_value=SimpleNamespace(
            status="downloading",
            user_id="alice",
            download_task_id="task-9",
            generation=1,
        )
    )
    request_history.async_prepare_requester_cancel.return_value = RequesterCancelDecision(
        "cancel_task", "downloading", 1
    )

    response = await service.cancel_batch(["rg-mine"], user_id="alice")

    assert response.cancelled == 1
    download_service.cancel_task.assert_awaited_once_with("task-9", "alice", "user")
    status_call = request_history.async_update_status.await_args
    assert status_call.args[:2] == ("rg-mine", "cancelled")
    assert status_call.kwargs["expected_generation"] == 1


@pytest.mark.asyncio
async def test_cancel_batch_user_only_cancels_owned_requests():
    service, request_history, download_service = _make_service()
    request_history.async_update_status = AsyncMock(return_value=True)
    records = {
        "rg-mine": SimpleNamespace(
            status="pending", user_id="alice", download_task_id=None, generation=1
        ),
        "rg-theirs": SimpleNamespace(
            status="pending", user_id="bob", download_task_id=None, generation=1
        ),
    }
    request_history.async_get_record = AsyncMock(
        side_effect=lambda mbid, **_kwargs: records.get(mbid)
    )
    request_history.async_prepare_requester_cancel.side_effect = [
        RequesterCancelDecision("cancelled", "pending", 1),
        RequesterCancelDecision("denied", "pending", 1),
    ]

    response = await service.cancel_batch(["rg-mine", "rg-theirs"], user_id="alice")

    assert response.cancelled == 1
    assert response.failed == 1
    download_service.cancel_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_batch_user_missing_record_counts_as_failed():
    service, request_history, _download = _make_service()
    request_history.async_update_status = AsyncMock(return_value=True)
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
    quota.check_request_quota = AsyncMock(return_value=True)
    quota.check_storage_admission = AsyncMock(return_value=True)
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
        await service.request_album(
            "rg-1", artist="Artist", album="Album", user_id="u1", user_role="user"
        )

    request_history.async_record_request.assert_not_awaited()
    quota.check_request_quota.assert_awaited_once_with("u1", "user")


@pytest.mark.asyncio
async def test_request_batch_counts_n_and_rejects_whole_batch():
    from core.exceptions import ValidationError

    service, request_history, _dl, quota = _make_service_with_quota()
    quota.check_request_quota.side_effect = ValidationError("Request limit reached")
    items = [
        {"musicbrainz_id": "rg-1", "artist_name": "A", "album_title": "B"},
        {"musicbrainz_id": "rg-2", "artist_name": "C", "album_title": "D"},
        {"musicbrainz_id": "rg-3", "artist_name": "E", "album_title": "F"},
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
    items = [
        {"musicbrainz_id": "RG-1", "artist_name": "A", "album_title": "B"},
        {"musicbrainz_id": "rg-2", "artist_name": "A", "album_title": "B"},
    ]

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
        await service.request_album(
            "rg-1", artist="Artist", album="Album", user_id="u1", user_role="user"
        )

    request_history.async_record_request.assert_not_awaited()
    quota.check_storage_admission.assert_awaited_once_with("u1", "user")


@pytest.mark.asyncio
async def test_request_album_resolves_download_service_per_dispatch():
    """Regression (stale-scorer bug): a settings save rebuilds the DownloadService
    singleton, so the request path must resolve it fresh at each dispatch and never
    capture an instance - else a saved quality change is ignored until restart. Uses
    DISTINCT mbids so the second call isn't short-circuited by request dedup."""
    request_history = MagicMock()
    request_history.async_record_request = AsyncMock(
        side_effect=[
            RequestBeginResult("rg-A", "album", 1),
            RequestBeginResult("rg-B", "album", 1),
        ]
    )
    request_history.async_get_record = AsyncMock(return_value=None)
    request_history.async_update_status = AsyncMock(return_value=True)
    request_history.async_update_download_task_id = AsyncMock(return_value=True)

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

    await service.request_album("rg-A", artist="A", album="B", user_role="admin")
    current["ds"] = ds_b  # a policy save rebuilt the DownloadService singleton
    await service.request_album("rg-B", artist="C", album="D", user_role="admin")

    ds_a.request_album.assert_awaited_once()  # first dispatch used the original engine
    ds_b.request_album.assert_awaited_once()  # second used the NEW one (fails if captured)
    assert [
        call.kwargs["expected_generation"]
        for call in request_history.async_update_download_task_id.await_args_list
    ] == [1, 1]


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
    assert response.task_id is None
    download_service.request_track.assert_not_awaited()
    kwargs = request_history.async_record_request.await_args.kwargs
    assert kwargs["musicbrainz_id"] == "recording-1"
    assert kwargs["request_kind"] == "track"
    assert kwargs["track_title"] == "Airbag"
    assert kwargs["duration_seconds"] == 287
    assert kwargs["track_release_group_mbid"] == "release-group-1"
    assert kwargs["release_mbid"] == "release-1"
    assert kwargs["initial_status"] == "awaiting_approval"
    assert kwargs["dispatch_authorized"] is False


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("user_role", "expected_status", "authorized"),
    [
        ("trusted", "queued", True),
        ("admin", "queued", True),
        ("unknown-role", "awaiting_approval", False),
    ],
)
async def test_request_track_role_gate_is_fail_closed(
    user_role: str, expected_status: str, authorized: bool
):
    service, request_history, download_service = _make_service()

    response = await service.request_track(
        "recording-role",
        user_id="listener-role",
        user_role=user_role,
        artist_name="Radiohead",
        track_title="Airbag",
        album_title="OK Computer",
        release_group_mbid="release-group-1",
    )

    assert response.status == expected_status
    assert (
        response.task_id == "track-task-1"
        if expected_status == "queued"
        else response.task_id is None
    )
    kwargs = request_history.async_record_request.await_args.kwargs
    assert kwargs["request_kind"] == "track"
    assert kwargs["dispatch_authorized"] is authorized
    if authorized:
        download_service.request_track.assert_awaited_once()
        assert (
            request_history.async_update_download_task_id.await_args.kwargs[
                "expected_generation"
            ]
            == 1
        )
    else:
        download_service.request_track.assert_not_awaited()


@pytest.mark.asyncio
async def test_existing_exact_track_attaches_listener_without_redispatch():
    service, request_history, download_service = _make_service()
    request_history.async_get_record.return_value = SimpleNamespace(
        status="pending", download_task_id="existing-track-task", generation=1
    )

    response = await service.request_track(
        "recording-shared",
        user_id="listener-2",
        user_role="user",
        requested_by_name="Second listener",
        artist_name="Radiohead",
        track_title="Airbag",
    )

    assert response.status == "queued"
    assert response.task_id == "existing-track-task"
    request_history.async_add_requester.assert_awaited_once_with(
        "recording-shared",
        "listener-2",
        "Second listener",
        request_kind="track",
    )
    download_service.request_track.assert_not_awaited()


@pytest.mark.asyncio
async def test_existing_album_attaches_listener_without_redispatch():
    service, request_history, download_service = _make_service()
    request_history.async_get_record.return_value = SimpleNamespace(
        status="pending", monitor_artist=False, generation=1
    )

    response = await service.request_album(
        "release-group-shared",
        artist="Artist",
        album="Album",
        user_id="listener-2",
        user_role="user",
        requested_by_name="Second listener",
    )

    assert response.status == "pending"
    request_history.async_add_requester.assert_awaited_once_with(
        "release-group-shared", "listener-2", "Second listener", request_kind="album"
    )
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_batch_duplicate_attaches_listener_and_reports_authoritative_status():
    service, request_history, download_service = _make_service()
    request_history.async_get_requested_mbids.return_value = {"rg-existing"}

    response = await service.request_batch(
        [
            {"musicbrainz_id": "rg-existing", "artist_name": "A", "album_title": "B"},
            {"musicbrainz_id": "rg-new", "artist_name": "Artist", "album_title": "Album"},
        ],
        user_id="listener-2",
        user_role="user",
    )

    assert response.status == "awaiting_approval"
    assert response.requested == 1
    request_history.async_add_requesters.assert_awaited_once_with(
        ["rg-existing"], "listener-2", None, request_kind="album"
    )
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("user_role", "expected_status", "authorized"),
    [
        ("user", "awaiting_approval", False),
        ("trusted", "pending", True),
        ("admin", "pending", True),
    ],
)
async def test_new_album_generation_persists_dispatch_authorization(
    user_role: str, expected_status: str, authorized: bool
):
    service, request_history, download_service = _make_service()

    response = await service.request_album(
        f"release-group-{user_role}",
        artist="Artist",
        album="Album",
        user_id=f"user-{user_role}",
        user_role=user_role,
    )

    assert response.status == expected_status
    kwargs = request_history.async_record_request.await_args.kwargs
    assert kwargs["dispatch_authorized"] is authorized
    if authorized:
        download_service.request_album.assert_awaited_once()
        assert (
            request_history.async_update_download_task_id.await_args.kwargs[
                "expected_generation"
            ]
            == 1
        )
    else:
        download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_album_and_track_with_same_uuid_use_isolated_history_kinds(tmp_path):
    store = RequestHistoryStore(tmp_path / "library.db")
    download_service = MagicMock()
    download_service.request_album = AsyncMock(return_value="album-task")
    download_service.request_track = AsyncMock(return_value="track-task")

    get_ds = lambda: download_service  # noqa: E731
    service = RequestService(
        store,
        get_download_service=get_ds,
        acquisition=make_builtin_dispatcher(get_ds),
    )

    await service.request_album(
        "same-uuid",
        artist="Artist",
        album="Album",
        user_id="album-user",
        user_role="trusted",
    )
    await service.request_track(
        "same-uuid",
        user_id="track-user",
        user_role="trusted",
        artist_name="Artist",
        track_title="Track",
        album_title="Album",
        duration_seconds=200,
        release_group_mbid="release-group",
        release_mbid="release-edition",
    )

    album = await store.async_get_record("same-uuid")
    track = await store.async_get_record("same-uuid", request_kind="track")
    assert album is not None
    assert track is not None
    assert album.request_kind == "album"
    assert album.download_task_id == "album-task"
    assert track.request_kind == "track"
    assert track.musicbrainz_id == "same-uuid"
    assert track.download_task_id == "track-task"
    assert track.track_title == "Track"
    assert track.release_mbid == "release-edition"
    assert await store.async_requester_count("same-uuid") == 1
    assert await store.async_requester_count("same-uuid", request_kind="track") == 1


@pytest.mark.asyncio
async def test_cancel_batch_uses_atomic_last_listener_decision_and_primary_owner():
    service, history, download_service = _make_service()
    history.async_get_record.return_value = SimpleNamespace(
        status="downloading",
        user_id="primary-owner",
        download_task_id="task-shared",
        generation=1,
    )
    history.async_prepare_requester_cancel.return_value = RequesterCancelDecision(
        "cancel_task", "downloading", 1
    )

    response = await service.cancel_batch(
        ["rg-shared"], user_id="second-listener", user_role="user"
    )

    assert response.success is True
    assert response.cancelled == 1
    download_service.cancel_task.assert_awaited_once_with(
        "task-shared", "primary-owner", "user"
    )
    status_calls = history.async_update_status.await_args_list
    assert status_calls
    assert status_calls[-1].args[:2] == ("rg-shared", "cancelled")
    assert status_calls[-1].kwargs["expected_generation"] == 1


@pytest.mark.asyncio
async def test_cancel_batch_restores_status_and_reports_failure_when_task_cancel_fails():
    service, history, download_service = _make_service()
    history.async_get_record.return_value = SimpleNamespace(
        status="downloading",
        user_id="primary-owner",
        download_task_id="task-shared",
        generation=1,
    )
    history.async_prepare_requester_cancel.return_value = RequesterCancelDecision(
        "cancel_task", "downloading", 1
    )
    download_service.cancel_task.side_effect = RuntimeError("provider secret")

    response = await service.cancel_batch(
        ["rg-shared"], user_id="second-listener", user_role="user"
    )

    assert response.success is False
    assert response.cancelled == 0
    assert response.failed == 1
    history.async_update_status.assert_not_awaited()
    history.async_restore_request_status.assert_awaited_once_with(
        "rg-shared",
        "downloading",
        expected_status="cancelling",
        expected_generation=1,
        request_kind="album",
    )
    assert "provider secret" not in response.message


@pytest.mark.asyncio
async def test_terminal_reactivation_starts_private_generation_without_old_listeners(
    tmp_path,
):
    store = RequestHistoryStore(tmp_path / "library.db")
    download_service = MagicMock()
    download_service.request_album = AsyncMock(return_value="new-task")
    get_ds = lambda: download_service  # noqa: E731
    service = RequestService(
        store,
        get_download_service=get_ds,
        acquisition=make_builtin_dispatcher(get_ds),
    )

    await store.async_record_request(
        "release-group-private",
        "Artist",
        "Album",
        user_id="old-owner",
        initial_status="pending",
    )
    await store.async_add_requester("release-group-private", "old-listener")
    await store.async_update_status("release-group-private", "failed")

    response = await service.request_album(
        "release-group-private",
        artist="Artist",
        album="Album",
        user_id="new-owner",
        user_role="user",
        requested_by_name="New owner",
    )

    assert response.status == "awaiting_approval"
    assert await store.async_is_requester("old-owner", "release-group-private") is False
    assert await store.async_is_requester("old-listener", "release-group-private") is False
    assert await store.async_is_requester("new-owner", "release-group-private") is True
    record = await store.async_get_record("release-group-private")
    assert record is not None
    assert record.status == "awaiting_approval"
    assert record.user_id == "new-owner"
    assert record.dispatch_authorized is False
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_stale_dispatch_generation_cancels_orphan_after_reactivation():
    service, history, download_service = _make_service()
    dispatch_started = asyncio.Event()
    release_old_dispatch = asyncio.Event()

    history.async_get_record = AsyncMock(
        side_effect=[
            None,
            SimpleNamespace(
                status="pending",
                user_id="old-owner",
                download_task_id=None,
                generation=1,
            ),
            SimpleNamespace(status="failed", generation=1),
        ]
    )
    history.async_record_request = AsyncMock(
        side_effect=[
            RequestBeginResult("rg-race", "album", 1),
            RequestBeginResult("rg-race", "album", 2),
        ]
    )
    history.async_prepare_requester_cancel.return_value = RequesterCancelDecision(
        "cancelled", "pending", 1
    )

    async def dispatch(**kwargs):
        if kwargs["user_id"] == "old-owner":
            dispatch_started.set()
            await release_old_dispatch.wait()
            return "orphan-task"
        return "successor-task"

    download_service.request_album = AsyncMock(side_effect=dispatch)
    link_generations: list[int | None] = []

    async def link(_mbid, _task_id, **kwargs):
        generation = kwargs.get("expected_generation")
        link_generations.append(generation)
        return generation != 1

    history.async_update_download_task_id = AsyncMock(side_effect=link)

    old_request = asyncio.create_task(
        service.request_album(
            "rg-race",
            artist="Race Artist",
            album="Race Album",
            user_id="old-owner",
            user_role="trusted",
        )
    )
    await dispatch_started.wait()

    cancelled = await service.cancel_batch(
        ["rg-race"], user_id="old-owner", user_role="user"
    )
    assert cancelled.success is True

    successor = await service.request_album(
        "rg-race",
        artist="Race Artist",
        album="Race Album",
        user_id="new-owner",
        user_role="trusted",
    )
    assert successor.success is True

    release_old_dispatch.set()
    with pytest.raises(ExternalServiceError):
        await old_request

    assert link_generations == [2, 1]
    download_service.cancel_task.assert_awaited_once_with(
        "orphan-task", "old-owner", "user"
    )


@pytest.mark.asyncio
async def test_overlapping_batches_dispatch_only_their_exact_winners():
    service, history, download_service = _make_service()
    items = [
        {"musicbrainz_id": "rg-a", "artist_name": "A", "album_title": "B"},
        {"musicbrainz_id": "rg-b", "artist_name": "C", "album_title": "D"},
    ]
    history.async_get_requested_mbids = AsyncMock(
        side_effect=[set(), set()]
    )
    history.async_bulk_record_requests = AsyncMock(
        side_effect=[
            [
                RequestBeginResult("rg-a", "album", 1)
            ],
            [
                RequestBeginResult("rg-b", "album", 1)
            ],
        ]
    )
    download_service.request_album = AsyncMock(
        side_effect=lambda **kwargs: f"task-{kwargs['release_group_mbid']}"
    )

    responses = await asyncio.gather(
        service.request_batch(items, user_id="u1", user_role="trusted"),
        service.request_batch(items, user_id="u1", user_role="trusted"),
    )

    dispatched = [
        call.kwargs["release_group_mbid"]
        for call in download_service.request_album.await_args_list
    ]
    assert dispatched == ["rg-a", "rg-b"]
    assert [response.requested for response in responses] == [1, 1]
    assert [
        call.kwargs["expected_generation"]
        for call in history.async_update_download_task_id.await_args_list
    ] == [1, 1]

@pytest.mark.asyncio
async def test_request_album_resolves_missing_names_from_musicbrainz():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        return_value=("rg-mbid-only", None)
    )
    album_service.get_album_basic_info = AsyncMock(
        return_value=SimpleNamespace(artist_name="MB Artist", title="MB Album")
    )
    service._album_service = album_service

    response = await service.request_album("rg-mbid-only", user_role="admin")

    assert response.success is True
    history_kwargs = request_history.async_record_request.await_args.kwargs
    assert history_kwargs["artist_name"] == "MB Artist"
    assert history_kwargs["album_title"] == "MB Album"
    assert "Unknown" not in (history_kwargs["artist_name"], history_kwargs["album_title"])
    dispatch_kwargs = download_service.request_album.await_args.kwargs
    assert dispatch_kwargs["artist_name"] == "MB Artist"
    assert dispatch_kwargs["album_title"] == "MB Album"


@pytest.mark.asyncio
async def test_request_album_mbid_only_with_failed_lookup_raises_and_records_nothing():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        return_value=("rg-unresolvable", None)
    )
    album_service.get_album_basic_info = AsyncMock(side_effect=Exception("mb down"))
    service._album_service = album_service

    with pytest.raises(ValidationError) as exc_info:
        await service.request_album("rg-unresolvable", user_role="admin")

    assert "rg-unresolvable" in str(exc_info.value)
    request_history.async_record_request.assert_not_awaited()
    download_service.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_request_album_supplied_names_skip_musicbrainz_lookup():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        return_value=("rg-given", None)
    )
    album_service.get_album_basic_info = AsyncMock()
    service._album_service = album_service

    response = await service.request_album(
        "rg-given", artist="Given Artist", album="Given Album", user_role="admin"
    )

    assert response.success is True
    album_service.get_album_basic_info.assert_not_awaited()
    history_kwargs = request_history.async_record_request.await_args.kwargs
    assert history_kwargs["artist_name"] == "Given Artist"
    assert history_kwargs["album_title"] == "Given Album"
    dispatch_kwargs = download_service.request_album.await_args.kwargs
    assert dispatch_kwargs["artist_name"] == "Given Artist"
    assert dispatch_kwargs["album_title"] == "Given Album"


@pytest.mark.asyncio
async def test_request_batch_resolves_one_and_fails_one_individually():
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(
        side_effect=[("rg-good", None), ("rg-bad", None)]
    )

    async def _basic(mbid: str):
        if mbid == "rg-good":
            return SimpleNamespace(artist_name="Good Artist", title="Good Album")
        raise Exception("not found")

    album_service.get_album_basic_info = AsyncMock(side_effect=_basic)
    service._album_service = album_service
    download_service.request_album = AsyncMock(return_value="task-good")

    response = await service.request_batch(
        [{"musicbrainz_id": "rg-good"}, {"musicbrainz_id": "rg-bad"}],
        user_role="admin",
        user_id="u1",
    )

    assert response.requested == 1
    assert response.skipped == 1
    recorded = request_history.async_bulk_record_requests.await_args.args[0]
    assert [item["musicbrainz_id"] for item in recorded] == ["rg-good"]
    assert recorded[0]["artist_name"] == "Good Artist"
    assert recorded[0]["album_title"] == "Good Album"
    download_service.request_album.assert_awaited_once()
    dispatch_kwargs = download_service.request_album.await_args.kwargs
    assert dispatch_kwargs["release_group_mbid"] == "rg-good"
    assert dispatch_kwargs["artist_name"] == "Good Artist"
    assert dispatch_kwargs["album_title"] == "Good Album"


@pytest.mark.asyncio
async def test_request_batch_treats_literal_unknown_as_missing():
    # The playlist missing-tracks flow sends literal "Unknown" placeholders;
    # the central guard must resolve them, never store or dispatch them.
    service, request_history, download_service = _make_service()
    album_service = MagicMock()
    album_service.resolve_album_identity = AsyncMock(return_value=("rg-pl", None))
    album_service.get_album_basic_info = AsyncMock(
        return_value=SimpleNamespace(artist_name="Real Artist", title="Real Album")
    )
    service._album_service = album_service
    download_service.request_album = AsyncMock(return_value="task-pl")

    response = await service.request_batch(
        [
            {
                "musicbrainz_id": "rg-pl",
                "artist_name": "Unknown",
                "album_title": "  ",
            }
        ],
        user_role="admin",
        user_id="u1",
    )

    assert response.requested == 1
    recorded = request_history.async_bulk_record_requests.await_args.args[0]
    assert recorded[0]["artist_name"] == "Real Artist"
    assert recorded[0]["album_title"] == "Real Album"
    dispatch_kwargs = download_service.request_album.await_args.kwargs
    assert dispatch_kwargs["artist_name"] == "Real Artist"
    assert dispatch_kwargs["album_title"] == "Real Album"
