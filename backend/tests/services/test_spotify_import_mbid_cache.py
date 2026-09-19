import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import services.spotify_import_service as spotify_module
import repositories.musicbrainz_base as mb_base
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.queue.priority_queue import RequestPriority
from services.spotify_import_service import SpotifyImportService


def _service(mb_repo, cache=None) -> SpotifyImportService:
    return SpotifyImportService(
        client_factory=AsyncMock(),
        playlist_repo=MagicMock(),
        mb_repo=mb_repo,
        playlist_service=MagicMock(),
        cache=cache or InMemoryCache(),
    )


def _repo(store=None, **overrides):
    """Stubbed _mb_repo with the members _resolve_mbid may touch."""
    repo = SimpleNamespace(
        mb_canonical_store=store,
        get_cached_recording_to_release_group=AsyncMock(return_value=None),
        resolve_recording_to_release_group=AsyncMock(return_value=None),
        search_release_groups=AsyncMock(return_value=[]),
    )
    for name, value in overrides.items():
        setattr(repo, name, value)
    return repo


def _store(recordings=None):
    store = MagicMock()
    store.get_recordings_by_isrc = AsyncMock(return_value=list(recordings or []))
    store.save_isrc_recordings = AsyncMock()
    return store


@pytest.mark.asyncio
async def test_isrc_durable_rows_use_cache_only_before_wire(monkeypatch):
    store = MagicMock()
    store.get_recordings_by_isrc = AsyncMock(return_value=["REC-B", "rec-A", "REC-B"])
    repo = SimpleNamespace(mb_canonical_store=store)
    repo.get_cached_recording_to_release_group = AsyncMock(
        side_effect=lambda recording: (
            f"rg-{recording}" if recording == "rec-a" else None
        )
    )
    repo.resolve_recording_to_release_group = AsyncMock(
        side_effect=lambda recording: f"rg-wire-{recording}"
    )
    service = _service(repo)
    provider = AsyncMock(side_effect=AssertionError("cache hit reached /isrc"))
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await service._resolve_mbid("US1234567890", "Artist", "Album")

    assert result == "rg-rec-a"
    repo.get_cached_recording_to_release_group.assert_awaited_once_with("rec-a")
    repo.resolve_recording_to_release_group.assert_not_awaited()
    provider.assert_not_awaited()


@pytest.mark.asyncio
async def test_isrc_durable_rows_are_discarded_after_source_switch(monkeypatch):
    store = MagicMock()
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="old-spotify-isrc",
        generation=old_generation,
    )

    async def switch_during_read(_isrc, *, source_context=None):
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="new-spotify-isrc",
            generation=old_generation + 1,
        )
        return ["rec-old"]

    store.get_recordings_by_isrc = AsyncMock(side_effect=switch_during_read)
    store.save_isrc_recordings = AsyncMock()
    repo = SimpleNamespace(
        mb_canonical_store=store,
        get_cached_recording_to_release_group=AsyncMock(
            side_effect=AssertionError("stale durable row reached cache")
        ),
        resolve_recording_to_release_group=AsyncMock(return_value=None),
    )
    service = _service(repo)
    provider = AsyncMock(return_value={"recordings": []})
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)
    try:
        result = await service._resolve_mbid("US7894567890", "Artist", "Album")
    finally:
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )

    assert result is None
    repo.get_cached_recording_to_release_group.assert_not_awaited()
    provider.assert_awaited_once()


@pytest.mark.asyncio
async def test_isrc_durable_misses_use_one_wire_then_resolve_returned_recordings(
    monkeypatch,
):
    store = MagicMock()
    store.get_recordings_by_isrc = AsyncMock(return_value=["REC-B", "rec-A"])
    store.save_isrc_recordings = AsyncMock()
    repo = SimpleNamespace(mb_canonical_store=store)
    repo.get_cached_recording_to_release_group = AsyncMock(return_value=None)
    repo.resolve_recording_to_release_group = AsyncMock(
        side_effect=[None, "rg-rec-wire-b"]
    )
    service = _service(repo)
    calls = []
    provider = AsyncMock(
        side_effect=lambda path, **_kwargs: (
            calls.append(path)
            or {
                "recordings": [
                    {"id": "rec-wire-a", "releases": []},
                    {"id": "rec-wire-b", "releases": []},
                ]
            }
        )
    )
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await service._resolve_mbid("US4564567890", "Artist", "Album")

    assert result == "rg-rec-wire-b"
    assert calls == ["/isrc/US4564567890"]
    assert [
        call.args[0]
        for call in repo.get_cached_recording_to_release_group.await_args_list
    ] == ["rec-a", "rec-b"]
    assert [
        call.args[0] for call in repo.resolve_recording_to_release_group.await_args_list
    ] == ["rec-wire-a", "rec-wire-b"]
    store.save_isrc_recordings.assert_awaited_once()
    assert store.save_isrc_recordings.await_args.args[0] == [
        ("US4564567890", "rec-wire-a"),
        ("US4564567890", "rec-wire-b"),
    ]
    assert store.save_isrc_recordings.await_args.kwargs["source_context"] is not None
    assert (
        provider.await_count + repo.resolve_recording_to_release_group.await_count == 3
    )


@pytest.mark.asyncio
async def test_isrc_404_twice_wires_once(monkeypatch):
    store = _store()
    repo = _repo(store)
    service = _service(repo, InMemoryCache())
    provider = AsyncMock(return_value={"recordings": []})
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    first = await service._resolve_mbid("usrc17607839", "Artist", "Album")
    second = await service._resolve_mbid("usrc17607839", "Artist", "Album")

    assert first is None
    assert second is None
    assert provider.await_count == 1
    assert provider.await_args.args[0] == "/isrc/USRC17607839"
    # The negative hit skips the durable read as well as the wire, but the
    # title-search fallback still runs on every call.
    assert store.get_recordings_by_isrc.await_count == 1
    assert repo.search_release_groups.await_count == 2


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "bad_isrc", ["not-an-isrc!", "0VjIjW4GlUZAMYd2vXMi3b", "USRC1760783"]
)
async def test_invalid_isrc_wires_zero_and_reaches_fallback(monkeypatch, bad_isrc):
    store = _store()
    repo = _repo(
        store,
        search_release_groups=AsyncMock(
            return_value=[
                SimpleNamespace(
                    musicbrainz_id="rg-fallback",
                    title="Album",
                    artist="Artist",
                )
            ]
        ),
    )
    cache = InMemoryCache()
    service = _service(repo, cache)
    provider = AsyncMock(side_effect=AssertionError("invalid ISRC reached /isrc"))
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await service._resolve_mbid(bad_isrc, "Artist", "Album")

    assert result == "rg-fallback"
    provider.assert_not_awaited()
    store.get_recordings_by_isrc.assert_not_awaited()
    assert cache.size() == 0


@pytest.mark.asyncio
async def test_concurrent_duplicate_isrcs_coalesce(monkeypatch):
    store = _store()
    repo = _repo(store)
    service = _service(repo, InMemoryCache())
    wire_calls = []
    both_arrived = asyncio.Event()
    dedupe_calls = 0
    real_dedupe = spotify_module.mb_deduplicator.dedupe

    async def provider(path, **kwargs):
        wire_calls.append(path)
        await asyncio.wait_for(both_arrived.wait(), timeout=5)
        return {"recordings": []}

    async def counting_dedupe(key, factory):
        nonlocal dedupe_calls
        dedupe_calls += 1
        if dedupe_calls == 2:
            both_arrived.set()
        return await real_dedupe(key, factory)

    monkeypatch.setattr(spotify_module, "mb_api_get", provider)
    monkeypatch.setattr(spotify_module.mb_deduplicator, "dedupe", counting_dedupe)

    first, second = await asyncio.gather(
        service._resolve_mbid("GBAYE9700123", "Artist", "Album"),
        service._resolve_mbid("GBAYE9700123", "Artist", "Album"),
    )

    assert first is None
    assert second is None
    assert wire_calls == ["/isrc/GBAYE9700123"]


@pytest.mark.asyncio
async def test_fallback_search_rides_background_sync():
    repo = _repo(None)
    service = _service(repo, InMemoryCache())

    result = await service._resolve_mbid(None, "Artist", "Album")

    assert result is None
    repo.search_release_groups.assert_awaited_once_with(
        "Artist",
        "Album",
        limit=3,
        include_all_types=False,
        priority=RequestPriority.BACKGROUND_SYNC,
    )
