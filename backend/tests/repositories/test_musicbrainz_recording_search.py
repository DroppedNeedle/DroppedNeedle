"""Tests for MusicBrainzAlbumMixin.search_recordings."""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from infrastructure.queue.priority_queue import RequestPriority
import pytest

import repositories.musicbrainz_album as album_module
from repositories.musicbrainz_album import (
    MusicBrainzAlbumMixin,
    RecordingMatch,
    _RecordingSearchPayload,
    _pick_best_release_group,
)
from repositories.musicbrainz_base import MbSourceContext

class _Repo(MusicBrainzAlbumMixin):
    def __init__(self) -> None:
        self._cache = AsyncMock()
        self._cache.get = AsyncMock(return_value=None)
        self._cache.set = AsyncMock()
        self._preferences_service = SimpleNamespace(
            get_advanced_settings=lambda: SimpleNamespace(cache_ttl_search=3600)
        )


_PAYLOAD = _RecordingSearchPayload(
    recordings=[
        {
            "id": "rec-sad",
            "title": "SAD!",
            "score": 100,
            "artist-credit": [
                {"name": "XXXTENTACION", "artist": {"name": "XXXTENTACION"}}
            ],
            "releases": [
                {
                    "id": "rel-q",
                    "title": "?",
                    "release-group": {
                        "id": "rg-q",
                        "title": "?",
                        "primary-type": "Album",
                    },
                }
            ],
        },
        {
            "id": "rec-sad-2",
            "title": "SAD!",
            "score": 100,
            "artist-credit": [
                {"name": "XXXTENTACION", "artist": {"name": "XXXTENTACION"}}
            ],
            "releases": [
                {
                    "id": "rel-mega",
                    "title": "Mega Hits 2018",
                    "release-group": {
                        "id": "rg-mega",
                        "title": "Mega Hits 2018",
                        "primary-type": "Album",
                        "secondary-types": ["Compilation"],
                    },
                },
                {
                    "id": "rel-mega-2",
                    "title": "Mega Hits 2018 (dupe RG)",
                    "release-group": {
                        "id": "rg-mega",
                        "title": "Mega Hits 2018",
                        "primary-type": "Album",
                        "secondary-types": ["Compilation"],
                    },
                },
            ],
        },
    ]
)


@pytest.mark.asyncio
async def test_search_recordings_parses_and_dedupes_release_groups():
    with patch(
        "repositories.musicbrainz_album.mb_api_get", AsyncMock(return_value=_PAYLOAD)
    ) as mock_get:
        matches = await _Repo().search_recordings("XXXTENTACION", "SAD!")

    assert mock_get.await_args.kwargs["params"]["query"] == (
        'recording:"SAD\\!" AND artist:"XXXTENTACION"'
    )
    assert [m.recording_mbid for m in matches] == ["rec-sad", "rec-sad-2"]

    first = matches[0]
    assert isinstance(first, RecordingMatch)
    assert first.artist == "XXXTENTACION"
    assert first.score == 100
    assert [rg.release_group_mbid for rg in first.release_groups] == ["rg-q"]
    assert first.release_groups[0].release_mbid == "rel-q"
    assert first.release_groups[0].secondary_types == ()

    second = matches[1]
    assert [rg.release_group_mbid for rg in second.release_groups] == ["rg-mega"]
    assert second.release_groups[0].secondary_types == ("Compilation",)


@pytest.mark.asyncio
async def test_search_recordings_concurrent_misses_share_one_wire_call():
    started = asyncio.Event()
    release = asyncio.Event()

    async def slow_get(*_args, **_kwargs):
        started.set()
        await release.wait()
        return _PAYLOAD

    repo = _Repo()
    with patch(
        "repositories.musicbrainz_album.mb_api_get", side_effect=slow_get
    ) as mock_get:
        first = asyncio.create_task(repo.search_recordings("Artist", "Title"))
        await started.wait()
        second = asyncio.create_task(repo.search_recordings("Artist", "Title"))
        await asyncio.sleep(0)
        release.set()
        results = await asyncio.gather(first, second)

    assert results[0] == results[1]
    assert mock_get.await_count == 1




@pytest.mark.asyncio
async def test_search_recordings_coalesces_same_priority_but_separates_lanes():
    started: dict[RequestPriority, asyncio.Event] = {
        RequestPriority.USER_INITIATED: asyncio.Event(),
        RequestPriority.BACKGROUND_SYNC: asyncio.Event(),
    }
    releases: dict[RequestPriority, asyncio.Event] = {
        RequestPriority.USER_INITIATED: asyncio.Event(),
        RequestPriority.BACKGROUND_SYNC: asyncio.Event(),
    }
    priorities: list[RequestPriority] = []

    async def slow_get(*_args, priority, **_kwargs):
        priorities.append(priority)
        started[priority].set()
        await releases[priority].wait()
        return _PAYLOAD

    repo = _Repo()
    with patch(
        "repositories.musicbrainz_album.mb_api_get", side_effect=slow_get
    ) as mock_get:
        user_one = asyncio.create_task(
            repo.search_recordings(
                "Artist", "Title", priority=RequestPriority.USER_INITIATED
            )
        )
        await started[RequestPriority.USER_INITIATED].wait()
        user_two = asyncio.create_task(
            repo.search_recordings(
                "Artist", "Title", priority=RequestPriority.USER_INITIATED
            )
        )
        background = asyncio.create_task(
            repo.search_recordings(
                "Artist", "Title", priority=RequestPriority.BACKGROUND_SYNC
            )
        )
        await started[RequestPriority.BACKGROUND_SYNC].wait()
        releases[RequestPriority.USER_INITIATED].set()
        releases[RequestPriority.BACKGROUND_SYNC].set()
        results = await asyncio.gather(user_one, user_two, background)

    assert results[0] == results[1] == results[2]
    assert mock_get.await_count == 2
    assert sorted(priorities) == sorted(
        [RequestPriority.USER_INITIATED, RequestPriority.BACKGROUND_SYNC]
    )
@pytest.mark.asyncio
async def test_search_recordings_failure_is_not_cached_and_retries():
    calls = 0
    started = asyncio.Event()
    release = asyncio.Event()

    async def fail_then_succeed(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            started.set()
            await release.wait()
            raise RuntimeError("provider unavailable")
        return _PAYLOAD

    repo = _Repo()
    with patch(
        "repositories.musicbrainz_album.mb_api_get",
        side_effect=fail_then_succeed,
    ) as mock_get:
        first_task = asyncio.create_task(repo.search_recordings("Artist", "Title"))
        await started.wait()
        second_task = asyncio.create_task(repo.search_recordings("Artist", "Title"))
        await asyncio.sleep(0)
        release.set()
        first, second = await asyncio.gather(first_task, second_task)
        retry = await repo.search_recordings("Artist", "Title")

    assert first == second == []
    assert [match.recording_mbid for match in retry] == ["rec-sad", "rec-sad-2"]
    assert mock_get.await_count == 2
    repo._cache.set.assert_awaited_once()


@pytest.mark.asyncio
async def test_recording_detail_concurrent_misses_share_one_wire_call():
    started = asyncio.Event()
    release = asyncio.Event()
    payload = {"id": "recording-id", "title": "Title"}

    async def slow_get(*_args, **_kwargs):
        started.set()
        await release.wait()
        return payload

    repo = _Repo()
    with patch(
        "repositories.musicbrainz_album.mb_api_get", side_effect=slow_get
    ) as mock_get:
        first = asyncio.create_task(repo.get_recording_by_id("RECORDING-ID"))
        await started.wait()
        second = asyncio.create_task(repo.get_recording_by_id("recording-id"))
        await asyncio.sleep(0)
        release.set()
        results = await asyncio.gather(first, second)

    assert results == [payload, payload]
    assert mock_get.await_count == 1


@pytest.mark.asyncio
async def test_recording_search_generation_separates_inflight_leaders(monkeypatch):
    generation = {"value": 0}
    started = asyncio.Event()
    release = asyncio.Event()
    calls = 0

    monkeypatch.setattr(
        album_module,
        "capture_mb_source_context",
        lambda: MbSourceContext("https://mb.example/ws/2", generation["value"]),
    )

    async def slow_get(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            started.set()
            await release.wait()
        return _PAYLOAD

    repo = _Repo()
    with patch("repositories.musicbrainz_album.mb_api_get", side_effect=slow_get):
        old_task = asyncio.create_task(repo.search_recordings("Artist", "Title"))
        await started.wait()
        generation["value"] = 1
        new_task = asyncio.create_task(repo.search_recordings("Artist", "Title"))
        await asyncio.sleep(0)
        release.set()
        old_result, new_result = await asyncio.gather(old_task, new_task)

    assert calls == 2
    assert [match.recording_mbid for match in old_result] == [
        "rec-sad",
        "rec-sad-2",
    ]
    assert [match.recording_mbid for match in new_result] == [
        "rec-sad",
        "rec-sad-2",
    ]


@pytest.mark.asyncio
async def test_recording_detail_generation_separates_inflight_leaders(monkeypatch):
    generation = {"value": 0}
    started = asyncio.Event()
    release = asyncio.Event()
    calls = 0
    payload = {"id": "recording-id", "title": "Title"}

    monkeypatch.setattr(
        album_module,
        "capture_mb_source_context",
        lambda: MbSourceContext("https://mb.example/ws/2", generation["value"]),
    )

    async def slow_get(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            started.set()
            await release.wait()
        return payload

    repo = _Repo()
    with patch("repositories.musicbrainz_album.mb_api_get", side_effect=slow_get):
        old_task = asyncio.create_task(repo.get_recording_by_id("recording-id"))
        await started.wait()
        generation["value"] = 1
        new_task = asyncio.create_task(repo.get_recording_by_id("recording-id"))
        await asyncio.sleep(0)
        release.set()
        old_result, new_result = await asyncio.gather(old_task, new_task)

    assert calls == 2
    assert old_result == payload
    assert new_result == payload


@pytest.mark.asyncio
async def test_recording_detail_failure_is_not_cached_and_retries():
    started = asyncio.Event()
    release = asyncio.Event()
    calls = 0

    async def fail(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            started.set()
            await release.wait()
        raise RuntimeError("provider unavailable")

    repo = _Repo()
    with patch(
        "repositories.musicbrainz_album.mb_api_get", side_effect=fail
    ) as mock_get:
        first_task = asyncio.create_task(repo.get_recording_by_id("recording-id"))
        await started.wait()
        second_task = asyncio.create_task(repo.get_recording_by_id("recording-id"))
        await asyncio.sleep(0)
        release.set()
        first, second = await asyncio.gather(first_task, second_task)
        retry = await repo.get_recording_by_id("recording-id")

    assert first is None and second is None and retry is None
    assert mock_get.await_count == 2
    repo._cache.set.assert_not_awaited()


@pytest.mark.asyncio
async def test_search_recordings_blank_inputs_short_circuit():
    repo = _Repo()
    with patch("repositories.musicbrainz_album.mb_api_get", AsyncMock()) as mock_get:
        assert await repo.search_recordings("", "SAD!") == []
        assert await repo.search_recordings("Artist", "   ") == []
    mock_get.assert_not_awaited()


@pytest.mark.asyncio
async def test_search_recordings_uses_cache_when_present():
    repo = _Repo()
    cached = [
        RecordingMatch(
            recording_mbid="r", title="t", artist="a", score=1, release_groups=[]
        )
    ]
    repo._cache.get = AsyncMock(return_value=cached)
    with patch("repositories.musicbrainz_album.mb_api_get", AsyncMock()) as mock_get:
        assert await repo.search_recordings("Artist", "Title") is cached
    mock_get.assert_not_awaited()


def test_include_all_types_bypasses_secondary_type_filter():
    repo = _Repo()
    comp = {
        "id": "rg-comp",
        "title": "Sittin' by the Road",
        "primary-type": "Album",
        "secondary-types": ["Compilation"],
    }
    assert repo._map_release_group_to_result(comp) is None
    mapped = repo._map_release_group_to_result(comp, include_all_types=True)
    assert mapped is not None
    assert mapped.musicbrainz_id == "rg-comp"


@pytest.mark.parametrize("reverse", [False, True])
def test_recording_fallback_prefers_official_compilation_over_bootleg_live(
    reverse: bool,
) -> None:
    releases = [
        {
            "id": "release-live",
            "status": "Bootleg",
            "date": "2019",
            "release-group": {
                "id": "rg-live",
                "title": "Festival 2019",
                "primary-type": "Album",
                "secondary-types": ["Live"],
            },
        },
        {
            "id": "release-compilation",
            "status": "Official",
            "date": "2009-10-30",
            "release-group": {
                "id": "rg-compilation",
                "title": "Greatest Hits",
                "primary-type": "Album",
                "secondary-types": ["Compilation"],
            },
        },
    ]
    if reverse:
        releases.reverse()

    assert _pick_best_release_group(releases) == (
        "rg-compilation",
        "Greatest Hits",
    )


def test_recording_fallback_keeps_bootleg_live_when_it_is_the_only_choice() -> None:
    release = {
        "id": "release-live",
        "status": "Bootleg",
        "release-group": {
            "id": "rg-live",
            "title": "Festival 2019",
            "primary-type": "Album",
            "secondary-types": ["Live"],
        },
    }

    assert _pick_best_release_group([release]) == ("rg-live", "Festival 2019")
