import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

import repositories.musicbrainz_artist as artist_module
import repositories.musicbrainz_base as mb_base
from infrastructure.cache.cache_keys import (
    mb_artist_detail_key,
    mb_artist_rgs_page_key,
)
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_artist import (
    MusicBrainzArtistMixin,
    _ArtistSearchPayload,
)
from repositories.musicbrainz_base import MbSourceContext

@pytest.mark.asyncio
async def test_artist_lookup_threads_priority_to_both_musicbrainz_calls(
    monkeypatch,
) -> None:
    artist_payload = {"id": "9fff2f8a-21e6-47de-83b0-29db67d7a5ae", "name": "Test Artist"}
    browse_payload = SimpleNamespace(release_groups=[], release_group_count=0)
    mb_get = AsyncMock(side_effect=[artist_payload, browse_payload])
    monkeypatch.setattr(artist_module, "mb_api_get", mb_get)

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = InMemoryCache()

    result = await repository.get_artist_by_id(
        "9fff2f8a-21e6-47de-83b0-29db67d7a5ae",
        priority=RequestPriority.BACKGROUND_SYNC,
    )

    assert result == {
        "id": "9fff2f8a-21e6-47de-83b0-29db67d7a5ae",
        "name": "Test Artist",
        "release-group-count": 0,
    }
    assert mb_get.await_count == 2
    assert all(
        call.kwargs["priority"] == RequestPriority.BACKGROUND_SYNC
        for call in mb_get.await_args_list
    )



@pytest.mark.asyncio
async def test_artist_basic_profile_uses_one_wire_and_distinct_cache_key(
    monkeypatch,
) -> None:
    artist_payload = {
        "id": "b7c5e1d4-3a2f-4e8b-9c0d-1f2a3b4c5d6e",
        "name": "Test Artist",
        "release-group-count": 7,
    }
    mb_get = AsyncMock(return_value=artist_payload)
    monkeypatch.setattr(artist_module, "mb_api_get", mb_get)

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = InMemoryCache()

    result = await repository.get_artist_by_id(
        "b7c5e1d4-3a2f-4e8b-9c0d-1f2a3b4c5d6e",
        include_releases=False,
    )

    assert result == artist_payload
    assert mb_get.await_count == 1
    assert mb_get.await_args.kwargs["params"]["inc"] == (
        "tags+aliases+url-rels+release-groups"
    )
    mb_get.reset_mock()
    assert await repository.get_artist_by_id("b7c5e1d4-3a2f-4e8b-9c0d-1f2a3b4c5d6e", include_releases=False) == artist_payload
    mb_get.assert_not_awaited()


@pytest.mark.asyncio
async def test_combined_and_dedicated_artist_consumers_share_limit24_cache(
    monkeypatch,
):
    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = InMemoryCache(max_entries=100)
    repository._preferences_service = SimpleNamespace(
        get_advanced_settings=lambda: SimpleNamespace(cache_ttl_search=3600)
    )
    payload = _ArtistSearchPayload(
        artists=[{"id": "artist-id", "name": "Test Artist", "score": 100}]
    )
    provider_calls = 0

    async def provider(*_args, **_kwargs):
        nonlocal provider_calls
        provider_calls += 1
        return payload

    monkeypatch.setattr(artist_module, "mb_api_get", provider)

    # Two application consumers issue the same backend bucket request: the
    # combined page and the dedicated artist page. The repository cache key
    # includes (query, limit, offset), so only the first reaches MusicBrainz.
    combined_call = await repository.search_artists("Test", limit=24, offset=0)
    dedicated_call = await repository.search_artists("Test", limit=24, offset=0)

    assert len([combined_call, dedicated_call]) == 2
    assert combined_call == dedicated_call
    assert provider_calls == 1

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
            return {"id": "d4e5f6a7-8b9c-4d1e-9f2a-3b4c5d6e7f8a", "name": "New Artist"}
        mb_base._mb_response_context.set(old_context)
        return SimpleNamespace(
            release_groups=[{"id": "old-group"}],
            release_group_count=1,
        )

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = InMemoryCache()
    monkeypatch.setattr(artist_module, "mb_api_get", provider)

    try:
        result = await repository.get_artist_by_id("d4e5f6a7-8b9c-4d1e-9f2a-3b4c5d6e7f8a")
    finally:
        monkeypatch.setattr(mb_base, "_mb_api_base", previous_source)
        monkeypatch.setattr(mb_base, "_mb_source_generation", previous_generation)

    assert result == {
        "id": "d4e5f6a7-8b9c-4d1e-9f2a-3b4c5d6e7f8a",
        "name": "New Artist",
        "release-group-count": 0,
    }
    assert repository._cache.size() == 0


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
            return {"id": "e5f6a7b8-9c0d-4e2f-8a3b-4c5d6e7f8a9b", "name": "Old Artist"}
        mb_base._mb_response_context.set(new_context)
        return SimpleNamespace(
            release_groups=[{"id": "new-group"}],
            release_group_count=1,
        )

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = InMemoryCache()
    monkeypatch.setattr(artist_module, "mb_api_get", provider)

    try:
        result = await repository.get_artist_by_id("e5f6a7b8-9c0d-4e2f-8a3b-4c5d6e7f8a9b")
    finally:
        monkeypatch.setattr(mb_base, "_mb_api_base", previous_source)
        monkeypatch.setattr(mb_base, "_mb_source_generation", previous_generation)

    assert result is None
    # The Step 04-1 page cache-aside legitimately caches the
    # current-generation browse page even though the mixed-generation
    # aggregate itself is dropped, never cached: the drop decision concerns
    # the aggregate, not the page cache. Pin the keys, not the whole size.
    assert (
        await repository._cache.get(
            mb_artist_detail_key("e5f6a7b8-9c0d-4e2f-8a3b-4c5d6e7f8a9b")
        )
        is None
    )
    assert (
        await repository._cache.get(
            mb_artist_rgs_page_key("e5f6a7b8-9c0d-4e2f-8a3b-4c5d6e7f8a9b", 50, 0)
        )
        is not None
    )


@pytest.mark.asyncio
async def test_case_variants_share_artist_detail_and_browse_wires(monkeypatch):
    started = asyncio.Event()
    release = asyncio.Event()
    calls: list[str] = []

    async def provider(path, **_kwargs):
        calls.append(path)
        if path == "/artist/c3e0a5d2-8b1f-4a6e-9d0c-5f7a1e2b3c4d":
            started.set()
            await release.wait()
            return {"id": "c3e0a5d2-8b1f-4a6e-9d0c-5f7a1e2b3c4d", "name": "Test Artist"}
        if path == "/release-group":
            return SimpleNamespace(release_groups=[], release_group_count=0)
        raise AssertionError(f"unexpected MusicBrainz path: {path}")

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = InMemoryCache()
    monkeypatch.setattr(artist_module, "mb_api_get", provider)

    first = asyncio.create_task(repository.get_artist_by_id("C3E0A5D2-8B1F-4A6E-9D0C-5F7A1E2B3C4D"))
    await started.wait()
    second = asyncio.create_task(repository.get_artist_by_id("c3e0a5d2-8b1f-4a6e-9d0c-5f7a1e2b3c4d"))
    await asyncio.sleep(0)
    release.set()
    result_one, result_two = await asyncio.gather(first, second)

    assert result_one == result_two
    assert calls == ["/artist/c3e0a5d2-8b1f-4a6e-9d0c-5f7a1e2b3c4d", "/release-group"]
