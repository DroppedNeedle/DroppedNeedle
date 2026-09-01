import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ExternalServiceError, ResourceNotFoundError
from infrastructure.queue.priority_queue import RequestPriority
from models.album import AlbumInfo
from services.album_service import AlbumService


def _make_service() -> tuple[AlbumService, MagicMock, MagicMock]:
    library_repo = MagicMock()
    mb_repo = MagicMock()
    library_db = MagicMock()
    memory_cache = MagicMock()
    disk_cache = MagicMock()
    preferences_service = MagicMock()
    preferences_service.get_advanced_settings.return_value = SimpleNamespace(
        cache_ttl_album_library=3600,
        cache_ttl_album_non_library=600,
    )
    audiodb_image_service = MagicMock()
    memory_cache.get = AsyncMock(return_value=None)
    memory_cache.set = AsyncMock()
    memory_cache.delete = AsyncMock()
    disk_cache.get_album = AsyncMock(return_value=None)
    disk_cache.delete_album = AsyncMock()
    library_db.get_library_files_for_album = AsyncMock(return_value=[])

    service = AlbumService(
        library_repo=library_repo,
        mb_repo=mb_repo,
        library_db=library_db,
        memory_cache=memory_cache,
        disk_cache=disk_cache,
        preferences_service=preferences_service,
        audiodb_image_service=audiodb_image_service,
    )
    return service, library_repo, library_db


@pytest.mark.asyncio
async def test_legacy_library_album_cache_is_rejected_and_new_selection_is_retained():
    service, _library_repo, _library_db = _make_service()
    legacy = {
        "title": "Album",
        "musicbrainz_id": _MBID,
        "artist_name": "Artist",
        "artist_id": "artist-1",
        "in_library": True,
        "tracks": [],
        "total_tracks": 0,
    }
    service._disk_cache.get_album.return_value = legacy

    assert await service._get_cached_album_info(_MBID, f"album:{_MBID}") is None
    service._disk_cache.delete_album.assert_awaited_once_with(_MBID)

    service._disk_cache.get_album.return_value = {
        **legacy,
        "selected_release_mbid": "release-20",
    }
    rebuilt = await service._get_cached_album_info(_MBID, f"album:{_MBID}")
    assert rebuilt is not None
    assert rebuilt.selected_release_mbid == "release-20"


def _mb_release_group() -> dict:
    return {
        "title": "Album",
        "first-release-date": "2024-01-01",
        "primary-type": "Album",
        "disambiguation": "",
        "artist-credit": [],
    }


@pytest.mark.asyncio
async def test_get_album_basic_info_in_library_from_local_files_not_ledger():
    # in_library follows non-deleted local files, not the library_albums ledger row.
    # Files present with no ledger row must still report in_library.
    service, library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_mb_release_group())
    library_repo.get_requested_mbids = AsyncMock(return_value=set())
    library_repo.get_album_details = AsyncMock(return_value=None)
    library_db.has_album_files = AsyncMock(return_value=True)
    library_db.get_album_by_mbid = AsyncMock(return_value=None)

    result = await service.get_album_basic_info("8e1e9e51-38dc-4df3-8027-a0ada37d4674")

    assert result.in_library is True
    library_db.has_album_files.assert_awaited()
    library_db.get_album_by_mbid.assert_not_awaited()


@pytest.mark.asyncio
async def test_get_album_basic_info_not_in_library_when_files_gone_despite_ledger_row():
    # Files soft-deleted but a stale ledger row lingers: in_library follows the files.
    service, library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_mb_release_group())
    library_repo.get_requested_mbids = AsyncMock(return_value=set())
    library_repo.get_album_details = AsyncMock(return_value=None)
    library_db.has_album_files = AsyncMock(return_value=False)
    library_db.get_album_by_mbid = AsyncMock(return_value={"mbid": "stale"})

    result = await service.get_album_basic_info("8e1e9e51-38dc-4df3-8027-a0ada37d4674")

    assert result.in_library is False


@pytest.mark.asyncio
async def test_cached_album_membership_is_overlaid_from_active_files():
    service, library_repo, library_db = _make_service()
    cached = AlbumInfo(
        title="Album",
        musicbrainz_id="8e1e9e51-38dc-4df3-8027-a0ada37d4674",
        artist_name="Artist",
        artist_id="artist-1",
        in_library=True,
        album_thumb_url="cached-thumb",
    )
    service._get_cached_album_info = AsyncMock(return_value=cached)
    service._apply_audiodb_album_images = AsyncMock(
        side_effect=lambda info, *a, **k: info
    )
    library_repo.is_configured.return_value = False
    library_db.resolve_library_album_identifier = AsyncMock(return_value=None)

    basic = await service.get_album_basic_info(cached.musicbrainz_id)
    full = await service.get_album_info(cached.musicbrainz_id)

    assert basic.in_library is False
    assert full.in_library is False


_MBID = "8e1e9e51-38dc-4df3-8027-a0ada37d4674"


@pytest.mark.asyncio
async def test_warm_full_album_cache_uses_cached_album_and_fetches_on_miss():
    service, _library_repo, _library_db = _make_service()
    cached = AlbumInfo(
        title="Cached",
        musicbrainz_id=_MBID,
        artist_name="Artist",
        artist_id="artist-1",
    )
    service._get_cached_album_info = AsyncMock(return_value=cached)
    service.get_album_info = AsyncMock()

    await service.warm_full_album_cache(_MBID)

    service._get_cached_album_info.assert_awaited_once()
    service.get_album_info.assert_not_awaited()

    service._get_cached_album_info.reset_mock()
    service._get_cached_album_info.return_value = None
    await service.warm_full_album_cache(_MBID)

    service.get_album_info.assert_awaited_once_with(
        _MBID, priority=RequestPriority.BACKGROUND_SYNC
    )


def _rg_with_ranked_release() -> dict:
    # find_primary_release picks the XW "worldwide" release regardless of size.
    return {
        "title": "Album",
        "first-release-date": "2024-01-01",
        "primary-type": "Album",
        "disambiguation": "",
        "artist-credit": [],
        "releases": [
            {"id": "deluxe-rel", "status": "Official", "country": "XW"},
            {"id": "owned-rel", "status": "Official", "country": "GB"},
        ],
    }


def _release_with_tracks(n: int) -> dict:
    return {
        "id": "rel",
        "media": [
            {
                "position": 1,
                "tracks": [
                    {
                        "position": i,
                        "recording": {
                            "title": f"T{i}",
                            "id": f"rec{i}",
                            "length": 200000,
                        },
                    }
                    for i in range(1, n + 1)
                ],
            }
        ],
    }


def _release_by_owned_or_deluxe(owned_id: str, owned_n: int, deluxe_n: int):
    async def _get(release_id: str, includes=None, priority=None) -> dict:
        return _release_with_tracks(owned_n if release_id == owned_id else deluxe_n)

    return _get


@pytest.mark.asyncio
async def test_get_album_info_uses_owned_release_edition_not_the_larger_ranked_release():
    # An owned album shows the edition on disc (12 tracks), not the deluxe ranked release (28).
    service, _library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._apply_audiodb_album_images = AsyncMock(
        side_effect=lambda info, *a, **k: info
    )
    service._save_album_to_cache = AsyncMock()
    library_db.has_album_files = AsyncMock(return_value=True)
    library_db.get_library_files_for_album = AsyncMock(
        return_value=[
            {"release_group_mbid": _MBID, "release_mbid": "owned-rel"}
            for _ in range(12)
        ]
    )
    service._mb_repo.get_release_by_id = _release_by_owned_or_deluxe(
        "owned-rel", owned_n=12, deluxe_n=28
    )

    result = await service.get_album_info(_MBID)

    assert result.total_tracks == 12
    assert result.selected_release_mbid == "owned-rel"
    library_db.get_library_files_for_album.assert_awaited_once()


@pytest.mark.asyncio
async def test_get_album_info_falls_back_to_ranked_release_when_not_owned():
    # No local files: keep the existing ranked-release behaviour, no owned lookup.
    service, _library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._apply_audiodb_album_images = AsyncMock(
        side_effect=lambda info, *a, **k: info
    )
    service._save_album_to_cache = AsyncMock()
    library_db.has_album_files = AsyncMock(return_value=False)
    service._check_in_library = AsyncMock(return_value=False)
    library_db.get_album_release_mbid = AsyncMock(return_value="owned-rel")
    service._mb_repo.get_release_by_id = _release_by_owned_or_deluxe(
        "owned-rel", owned_n=12, deluxe_n=28
    )

    result = await service.get_album_info(_MBID)

    assert result.total_tracks == 28
    assert result.selected_release_mbid == "deluxe-rel"
    library_db.get_album_release_mbid.assert_not_awaited()


@pytest.mark.asyncio
async def test_get_album_tracks_info_prefers_owned_release_edition():
    service, _library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    library_db.get_library_files_for_album = AsyncMock(
        return_value=[
            {"release_group_mbid": _MBID, "release_mbid": "owned-rel"}
            for _ in range(12)
        ]
    )
    service._mb_repo.get_release_by_id = _release_by_owned_or_deluxe(
        "owned-rel", owned_n=12, deluxe_n=28
    )

    result = await service.get_album_tracks_info(_MBID)

    assert result.total_tracks == 12
    assert len(result.tracks) == 12
    assert result.selected_release_mbid == "owned-rel"


def _native_tracks(*, release_id: str | None = "owned-rel") -> list[dict]:
    return [
        {
            "local_album_id": "local-album-1",
            "provider_release_mbid": release_id,
            "track_number": 1,
            "disc_number": 1,
            "track_title": "First",
            "duration_seconds": 61.25,
            "recording_mbid": "recording-1",
            "release_track_mbid": "release-track-1",
        },
        {
            "local_album_id": "local-album-1",
            "provider_release_mbid": release_id,
            "track_number": 2,
            "disc_number": 2,
            "track_title": "Second",
            "duration_seconds": 122.5,
            "recording_mbid": "recording-2",
            "release_track_mbid": "release-track-2",
        },
    ]


def _attach_native_store(service: AlbumService, rows: list[dict]) -> MagicMock:
    store = MagicMock()
    store.target_album_ownership_rows = AsyncMock(
        return_value=[{"local_album_id": "local-album-1"}]
    )
    store.get_target_album_tracks = AsyncMock(return_value=rows)
    service._native_library_store = store
    return store


@pytest.mark.asyncio
async def test_owned_native_tracks_return_without_musicbrainz_lookup():
    service, _library_repo, _library_db = _make_service()
    store = _attach_native_store(service, _native_tracks())
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock()

    result = await service.get_album_tracks_info(_MBID)

    assert [(track.disc_number, track.position) for track in result.tracks] == [
        (1, 1),
        (2, 2),
    ]
    assert result.total_length == 183750
    assert result.selected_release_mbid == "owned-rel"
    assert result.tracks[0].recording_id == "recording-1"
    assert result.tracks[0].release_track_id == "release-track-1"
    service._fetch_release_group.assert_not_awaited()
    service._mb_repo.get_release_by_id.assert_not_called()
    store.get_target_album_tracks.assert_awaited_once_with("local-album-1")


@pytest.mark.asyncio
async def test_unidentified_owned_tracks_return_locally_without_claiming_an_edition():
    service, _library_repo, _library_db = _make_service()
    _attach_native_store(service, _native_tracks(release_id=None))
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock()

    result = await service.get_album_tracks_info(_MBID)

    assert result.total_tracks == 2
    assert result.selected_release_mbid is None
    service._fetch_release_group.assert_not_awaited()


@pytest.mark.asyncio
async def test_matching_edition_pin_keeps_native_fast_path():
    service, _library_repo, _library_db = _make_service()
    _attach_native_store(service, _native_tracks())
    service._release_pins = SimpleNamespace(get=AsyncMock(return_value="owned-rel"))
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock()

    result = await service.get_album_tracks_info(_MBID)

    assert result.selected_release_mbid == "owned-rel"
    service._fetch_release_group.assert_not_awaited()


@pytest.mark.asyncio
async def test_different_edition_pin_fetches_only_that_selected_release():
    service, _library_repo, _library_db = _make_service()
    _attach_native_store(service, _native_tracks())
    service._release_pins = SimpleNamespace(get=AsyncMock(return_value="pinned-rel"))
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(
        return_value={
            **_rg_with_ranked_release(),
            "releases": [
                {"id": "pinned-rel", "status": "Official", "country": "US"},
                *_rg_with_ranked_release()["releases"],
            ],
        }
    )
    service._mb_repo.get_release_by_id = AsyncMock(return_value=_release_with_tracks(3))

    result = await service.get_album_tracks_info(_MBID)

    assert result.selected_release_mbid == "pinned-rel"
    service._mb_repo.get_release_by_id.assert_awaited_once_with(
        "pinned-rel",
        includes=["recordings", "labels"],
        priority=RequestPriority.USER_INITIATED,
    )


@pytest.mark.asyncio
async def test_ambiguous_owned_copies_do_not_merge_native_tracks():
    service, _library_repo, _library_db = _make_service()
    store = _attach_native_store(service, _native_tracks())
    store.target_album_ownership_rows.return_value = [
        {"local_album_id": "local-album-1"},
        {"local_album_id": "local-album-2"},
    ]
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._mb_repo.get_release_by_id = AsyncMock(return_value=_release_with_tracks(4))

    result = await service.get_album_tracks_info(_MBID)

    assert result.total_tracks == 4
    store.get_target_album_tracks.assert_not_awaited()
    service._mb_repo.get_release_by_id.assert_awaited_once()


@pytest.mark.asyncio
async def test_remote_tracks_fetch_selected_release_only_on_success():
    service, _library_repo, _library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._mb_repo.get_release_by_id = AsyncMock(return_value=_release_with_tracks(4))

    result = await service.get_album_tracks_info(_MBID)

    assert result.selected_release_mbid == "deluxe-rel"
    assert result.total_tracks == 4
    service._mb_repo.get_release_by_id.assert_awaited_once()
    assert service._mb_repo.get_release_by_id.await_args.args[0] == "deluxe-rel"


@pytest.mark.asyncio
@pytest.mark.parametrize("selected_result", [None, {"id": "deluxe-rel", "media": []}])
async def test_remote_tracks_fall_back_only_after_selected_miss_or_empty(
    selected_result: dict | None,
):
    service, _library_repo, _library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._mb_repo.get_release_by_id = AsyncMock(
        side_effect=[selected_result, _release_with_tracks(2)]
    )

    result = await service.get_album_tracks_info(_MBID)

    assert result.selected_release_mbid == "owned-rel"
    assert result.total_tracks == 2
    assert [
        item.args[0] for item in service._mb_repo.get_release_by_id.await_args_list
    ] == [
        "deluxe-rel",
        "owned-rel",
    ]


@pytest.mark.asyncio
async def test_remote_selected_provider_failure_never_drifts_to_fallback():
    service, _library_repo, _library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._mb_repo.get_release_by_id = AsyncMock(
        side_effect=ExternalServiceError("provider unavailable")
    )

    with pytest.raises(ExternalServiceError, match="provider unavailable"):
        await service.get_album_tracks_info(_MBID)

    service._mb_repo.get_release_by_id.assert_awaited_once()


@pytest.mark.asyncio
async def test_remote_track_leader_failure_does_not_leak_future_exception():
    service, _library_repo, _library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._mb_repo.get_release_by_id = AsyncMock(
        side_effect=ExternalServiceError("provider unavailable")
    )
    loop = asyncio.get_running_loop()
    reported: list[dict[str, object]] = []
    previous_handler = loop.get_exception_handler()
    loop.set_exception_handler(lambda _loop, context: reported.append(context))
    try:
        with pytest.raises(ExternalServiceError, match="provider unavailable"):
            await service.get_album_tracks_info(_MBID)
        await asyncio.sleep(0)
    finally:
        loop.set_exception_handler(previous_handler)

    assert reported == []
    assert service._tracks_in_flight == {}


@pytest.mark.asyncio
async def test_cancelled_remote_track_leader_cancels_waiters_and_clears_inflight():
    service, _library_repo, _library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    lookup_started = asyncio.Event()

    async def blocked_release(*_args, **_kwargs) -> dict:
        lookup_started.set()
        await asyncio.Event().wait()
        return _release_with_tracks(2)

    service._mb_repo.get_release_by_id = AsyncMock(side_effect=blocked_release)
    leader = asyncio.create_task(service.get_album_tracks_info(_MBID))
    await lookup_started.wait()
    waiter = asyncio.create_task(service.get_album_tracks_info(_MBID))
    await asyncio.sleep(0)
    leader.cancel()

    with pytest.raises(asyncio.CancelledError):
        await leader
    with pytest.raises(asyncio.CancelledError):
        await waiter
    assert service._tracks_in_flight == {}


@pytest.mark.asyncio
async def test_composed_remote_tracks_are_cached_for_fresh_session_repeat():
    service, _library_repo, _library_db = _make_service()
    values: dict[str, object] = {}
    service._cache.get.side_effect = lambda key: values.get(key)

    async def save(key: str, value: object, ttl_seconds: int = 60) -> None:
        values[key] = value

    service._cache.set.side_effect = save
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._mb_repo.get_release_by_id = AsyncMock(return_value=_release_with_tracks(3))

    first = await service.get_album_tracks_info(_MBID)
    second = await service.get_album_tracks_info(_MBID)

    assert second is first
    service._fetch_release_group.assert_awaited_once()
    service._cache.set.assert_awaited_once()


@pytest.mark.asyncio
async def test_concurrent_remote_track_requests_share_one_selected_lookup():
    service, _library_repo, _library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    lookup_started = asyncio.Event()
    release_lookup = asyncio.Event()

    async def delayed_release(*_args, **_kwargs) -> dict:
        lookup_started.set()
        await release_lookup.wait()
        return _release_with_tracks(2)

    service._mb_repo.get_release_by_id = AsyncMock(side_effect=delayed_release)

    first = asyncio.create_task(service.get_album_tracks_info(_MBID))
    await lookup_started.wait()
    second = asyncio.create_task(service.get_album_tracks_info(_MBID))
    await asyncio.sleep(0)
    release_lookup.set()
    first_result, second_result = await asyncio.gather(first, second)

    assert second_result is first_result
    service._mb_repo.get_release_by_id.assert_awaited_once()


@pytest.mark.asyncio
async def test_source_switch_separates_track_leaders_and_followers():
    import repositories.musicbrainz_base as mb_base

    service, _library_repo, _library_db = _make_service()
    old_gate = asyncio.Event()
    new_gate = asyncio.Event()
    old_started = asyncio.Event()
    new_started = asyncio.Event()
    calls: list[int] = []
    original_source = mb_base.capture_mb_source_context()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_source = "https://old.example/ws/2"
    mb_base.set_mb_api_base(
        old_source,
        source_mode="mirror",
        source_id="album-tracks-old",
        generation=original_source.generation + 1,
    )
    old_generation = mb_base.get_mb_source_generation()

    async def build(*_args, **_kwargs):
        generation = mb_base.get_mb_source_generation()
        calls.append(generation)
        if generation == old_generation:
            old_started.set()
            await old_gate.wait()
            tracks = ["old"]
        else:
            new_started.set()
            await new_gate.wait()
            tracks = ["new"]
        return SimpleNamespace(tracks=tracks, total_tracks=1), False

    service._build_album_tracks_info = build
    try:
        old_leader = asyncio.create_task(service.get_album_tracks_info(_MBID))
        await old_started.wait()
        old_follower = asyncio.create_task(service.get_album_tracks_info(_MBID))
        await asyncio.sleep(0)

        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="album-tracks-new",
            generation=old_generation + 1,
        )
        new_generation = mb_base.get_mb_source_generation()
        new_leader = asyncio.create_task(service.get_album_tracks_info(_MBID))
        await new_started.wait()
        new_follower = asyncio.create_task(service.get_album_tracks_info(_MBID))
        await asyncio.sleep(0)

        assert len(service._tracks_in_flight) == 2
        assert (_MBID.casefold(), old_generation) in service._tracks_in_flight
        assert (_MBID.casefold(), new_generation) in service._tracks_in_flight

        new_gate.set()
        new_leader_result, new_follower_result = await asyncio.gather(
            new_leader, new_follower
        )
        old_gate.set()
        old_leader_result, old_follower_result = await asyncio.gather(
            old_leader, old_follower
        )
    finally:
        old_gate.set()
        new_gate.set()
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source.source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )

    assert calls == [old_generation, new_generation]
    assert old_leader_result.tracks == ["old"]
    assert old_follower_result.tracks == ["old"]
    assert new_leader_result.tracks == ["new"]
    assert new_follower_result.tracks == ["new"]
    assert service._tracks_in_flight == {}


# P5: annotate_album_coverage (shared matcher on the album page)


def _lib_track(id, *, disc=1, track=0, title="", recording=None, duration=None):
    from services.native.library_manager import LibraryTrack

    return LibraryTrack(
        id=id,
        recording_mbid=recording,
        disc_number=disc,
        track_number=track,
        track_title=title,
        duration_seconds=duration,
    )


def _status(tracks):
    from services.native.library_manager import LibraryAlbumStatus

    return LibraryAlbumStatus(
        in_library=bool(tracks), track_count=len(tracks), tracks=tracks
    )


def _svc_self(tracks_info):
    """Bind the real method to a minimal fake self (AlbumService's ctor is heavy)."""
    from types import SimpleNamespace
    from unittest.mock import AsyncMock

    return SimpleNamespace(get_album_tracks_info=AsyncMock(return_value=tracks_info))


def _mb_tracks(*specs):
    from types import SimpleNamespace

    return SimpleNamespace(
        tracks=[
            SimpleNamespace(
                position=p, disc_number=d, title=t, recording_id=r, length=ln
            )
            for (p, d, t, r, ln) in specs
        ],
        total_tracks=len(specs),
    )


@pytest.mark.asyncio
async def test_annotate_coverage_incident_shape_all_orphans():
    """The incident's exact library state: one wrong file (position 2, 137 s, no
    recording) against a 1-track release - covered 0/1, the file is an orphan."""
    from services.album_service import AlbumService

    status = _status(
        [_lib_track("f-wrong", track=2, title="Arrival in Ashford", duration=137.24)]
    )
    info = _mb_tracks((1, 1, "the arrival", "rec-180ceef5", 155556))

    out = await AlbumService.annotate_album_coverage(_svc_self(info), "rg-1", status)

    assert out.expected_tracks == 1
    assert out.covered_tracks == 0
    assert out.matched_file_ids == []
    assert [o.id for o in out.orphans] == ["f-wrong"]


@pytest.mark.asyncio
async def test_annotate_coverage_full_album_no_orphans():
    from services.album_service import AlbumService

    status = _status(
        [
            _lib_track("f1", track=1, title="A", recording="rec-1", duration=200.0),
            _lib_track("f2", track=2, title="B", duration=210.0),
        ]
    )
    info = _mb_tracks((1, 1, "A", "rec-1", 200000), (2, 1, "B", None, 210000))

    out = await AlbumService.annotate_album_coverage(_svc_self(info), "rg-1", status)

    assert (out.covered_tracks, out.expected_tracks) == (2, 2)
    assert sorted(out.matched_file_ids) == ["f1", "f2"]
    assert out.orphans == []


@pytest.mark.asyncio
async def test_annotate_coverage_fails_open_on_mb_error():
    from types import SimpleNamespace
    from unittest.mock import AsyncMock

    from services.album_service import AlbumService

    status = _status([_lib_track("f1", track=1, title="A")])
    fake = SimpleNamespace(
        get_album_tracks_info=AsyncMock(side_effect=RuntimeError("down"))
    )

    out = await AlbumService.annotate_album_coverage(fake, "rg-1", status)

    assert out.expected_tracks == 0  # zeroed -> page falls back to presence-only
    assert out.orphans == []


# #78: release-MBID aliases must resolve to their release group

_RELEASE_ALIAS = "04a68af0-b66b-4578-9844-12615de7183a"
_RESOLVED_RG = "f722b0db-3035-3b4b-b499-57e42d4219c7"


@pytest.mark.asyncio
async def test_fetch_release_group_resolves_release_mbid_alias():
    # A release MBID 404s as a release group; the fallback resolves release -> RG
    # and retries with the real id, at the caller's priority.
    service, _, _ = _make_service()
    rg = {**_mb_release_group(), "id": _RESOLVED_RG}
    service._mb_repo.get_release_group_by_id = AsyncMock(side_effect=[None, rg])
    service._mb_repo.get_release_group_id_from_release = AsyncMock(
        return_value=_RESOLVED_RG
    )

    result = await service._fetch_release_group(_RELEASE_ALIAS)

    assert result is rg
    service._mb_repo.get_release_group_id_from_release.assert_awaited_once_with(
        _RELEASE_ALIAS, priority=RequestPriority.USER_INITIATED
    )
    lookups = service._mb_repo.get_release_group_by_id.await_args_list
    assert lookups[0].args[0] == _RELEASE_ALIAS
    assert lookups[1].args[0] == _RESOLVED_RG


@pytest.mark.asyncio
async def test_fetch_release_group_raises_when_alias_unresolvable():
    service, _, _ = _make_service()
    service._mb_repo.get_release_group_by_id = AsyncMock(return_value=None)
    service._mb_repo.get_release_group_id_from_release = AsyncMock(return_value=None)

    with pytest.raises(ResourceNotFoundError):
        await service._fetch_release_group(_RELEASE_ALIAS)

    service._mb_repo.get_release_group_by_id.assert_awaited_once()


@pytest.mark.asyncio
async def test_fetch_release_group_raises_when_resolved_rg_also_missing():
    service, _, _ = _make_service()
    service._mb_repo.get_release_group_by_id = AsyncMock(return_value=None)
    service._mb_repo.get_release_group_id_from_release = AsyncMock(
        return_value=_RESOLVED_RG
    )

    with pytest.raises(ResourceNotFoundError):
        await service._fetch_release_group(_RELEASE_ALIAS)


@pytest.mark.asyncio
async def test_get_album_basic_info_via_release_mbid_alias():
    # End-to-end: basic info succeeds for a release-MBID link, and the in-library
    # check keys on the canonical RG id, not the alias.
    service, library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    rg = {**_mb_release_group(), "id": _RESOLVED_RG}
    service._mb_repo.get_release_group_by_id = AsyncMock(side_effect=[None, rg])
    service._mb_repo.get_release_group_id_from_release = AsyncMock(
        return_value=_RESOLVED_RG
    )
    library_repo.get_requested_mbids = AsyncMock(return_value=set())
    library_db.has_album_files = AsyncMock(return_value=True)

    result = await service.get_album_basic_info(_RELEASE_ALIAS)

    assert result.title == "Album"
    assert result.in_library is True
    assert result.musicbrainz_id == _RESOLVED_RG
    library_db.has_album_files.assert_awaited_once_with(_RESOLVED_RG)


@pytest.mark.asyncio
async def test_resolve_album_identity_preserves_release_alias_as_edition():
    service, _, _ = _make_service()
    rg = {**_mb_release_group(), "id": _RESOLVED_RG}
    service._mb_repo.get_release_group_by_id = AsyncMock(side_effect=[None, rg])
    service._mb_repo.get_release_group_id_from_release = AsyncMock(
        return_value=_RESOLVED_RG
    )

    release_group_mbid, release_mbid = await service.resolve_album_identity(
        _RELEASE_ALIAS
    )

    assert release_group_mbid == _RESOLVED_RG
    assert release_mbid == _RELEASE_ALIAS
