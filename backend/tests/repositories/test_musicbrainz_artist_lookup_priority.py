import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

import repositories.musicbrainz_artist as artist_module
import repositories.musicbrainz_base as mb_base
from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_artist import MusicBrainzArtistMixin
from repositories.musicbrainz_base import MbSourceContext


@pytest.mark.asyncio
async def test_artist_lookup_threads_priority_to_both_musicbrainz_calls(
    monkeypatch,
) -> None:
    artist_payload = {"id": "artist-id", "name": "Test Artist"}
    browse_payload = SimpleNamespace(release_groups=[], release_group_count=0)
    mb_get = AsyncMock(side_effect=[artist_payload, browse_payload])
    monkeypatch.setattr(artist_module, "mb_api_get", mb_get)

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = SimpleNamespace(
        get=AsyncMock(return_value=None),
        set=AsyncMock(),
    )

    result = await repository.get_artist_by_id(
        "artist-id",
        priority=RequestPriority.BACKGROUND_SYNC,
    )

    assert result == {
        "id": "artist-id",
        "name": "Test Artist",
        "release-group-count": 0,
    }
    assert mb_get.await_count == 2
    assert all(
        call.kwargs["priority"] == RequestPriority.BACKGROUND_SYNC
        for call in mb_get.await_args_list
    )


@pytest.mark.asyncio
async def test_artist_aggregate_drops_mixed_generation_browse_data(monkeypatch):
    previous_source = mb_base.get_mb_api_base()
    previous_generation = mb_base.get_mb_source_generation()
    monkeypatch.setattr(mb_base, "_mb_api_base", "https://new.example/ws/2")
    monkeypatch.setattr(mb_base, "_mb_source_generation", previous_generation + 1)
    current_generation = mb_base.get_mb_source_generation()
    old_context = MbSourceContext(
        source_url="https://old.example/ws/2",
        generation=current_generation - 1,
    )
    new_context = MbSourceContext(
        source_url=mb_base.get_mb_api_base(),
        generation=current_generation,
    )

    async def provider(path, **_kwargs):
        if path.startswith("/artist/"):
            mb_base._mb_response_context.set(new_context)
            return {"id": "artist-id", "name": "New Artist"}
        mb_base._mb_response_context.set(old_context)
        return SimpleNamespace(
            release_groups=[{"id": "old-group"}],
            release_group_count=1,
        )

    cache_set = AsyncMock()
    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = SimpleNamespace(
        get=AsyncMock(return_value=None),
        set=cache_set,
    )
    monkeypatch.setattr(artist_module, "mb_api_get", provider)

    try:
        result = await repository.get_artist_by_id("artist-id")
    finally:
        monkeypatch.setattr(mb_base, "_mb_api_base", previous_source)
        monkeypatch.setattr(mb_base, "_mb_source_generation", previous_generation)

    assert result == {
        "id": "artist-id",
        "name": "New Artist",
        "release-group-count": 0,
    }
    cache_set.assert_not_awaited()


@pytest.mark.asyncio
async def test_artist_aggregate_drops_stale_detail_when_browse_is_current(monkeypatch):
    previous_source = mb_base.get_mb_api_base()
    previous_generation = mb_base.get_mb_source_generation()
    monkeypatch.setattr(mb_base, "_mb_api_base", "https://new.example/ws/2")
    monkeypatch.setattr(mb_base, "_mb_source_generation", previous_generation + 1)
    current_generation = mb_base.get_mb_source_generation()
    old_context = MbSourceContext(
        source_url="https://old.example/ws/2",
        generation=current_generation - 1,
    )
    new_context = MbSourceContext(
        source_url=mb_base.get_mb_api_base(),
        generation=current_generation,
    )

    async def provider(path, **_kwargs):
        if path.startswith("/artist/"):
            mb_base._mb_response_context.set(old_context)
            return {"id": "artist-id", "name": "Old Artist"}
        mb_base._mb_response_context.set(new_context)
        return SimpleNamespace(
            release_groups=[{"id": "new-group"}],
            release_group_count=1,
        )

    cache_set = AsyncMock()
    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = SimpleNamespace(
        get=AsyncMock(return_value=None),
        set=cache_set,
    )
    monkeypatch.setattr(artist_module, "mb_api_get", provider)

    try:
        result = await repository.get_artist_by_id("artist-id")
    finally:
        monkeypatch.setattr(mb_base, "_mb_api_base", previous_source)
        monkeypatch.setattr(mb_base, "_mb_source_generation", previous_generation)

    assert result is None
    cache_set.assert_not_awaited()


@pytest.mark.asyncio
async def test_case_variants_share_artist_detail_and_browse_wires(monkeypatch):
    started = asyncio.Event()
    release = asyncio.Event()
    calls: list[str] = []

    async def provider(path, **_kwargs):
        calls.append(path)
        if path == "/artist/artist-id":
            started.set()
            await release.wait()
            return {"id": "artist-id", "name": "Test Artist"}
        if path == "/release-group":
            return SimpleNamespace(release_groups=[], release_group_count=0)
        raise AssertionError(f"unexpected MusicBrainz path: {path}")

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = SimpleNamespace(
        get=AsyncMock(return_value=None),
        set=AsyncMock(),
    )
    monkeypatch.setattr(artist_module, "mb_api_get", provider)

    first = asyncio.create_task(repository.get_artist_by_id("ARTIST-ID"))
    await started.wait()
    second = asyncio.create_task(repository.get_artist_by_id("artist-id"))
    await asyncio.sleep(0)
    release.set()
    result_one, result_two = await asyncio.gather(first, second)

    assert result_one == result_two
    assert calls == ["/artist/artist-id", "/release-group"]
