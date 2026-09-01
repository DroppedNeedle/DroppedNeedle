import asyncio
from types import SimpleNamespace

import pytest

import repositories.musicbrainz_artist as artist_module
import repositories.musicbrainz_base as mb_base
from repositories.musicbrainz_base import (
    MbSourceContext,
    mb_cache_get_if_current,
    namespace_mb_cache_key,
)
from repositories.musicbrainz_artist import MusicBrainzArtistMixin
from infrastructure.cache.cache_keys import mb_artist_search_key


class _RawCache:
    def __init__(self) -> None:
        self.values: dict[str, object] = {}
        self.read_keys: list[str] = []

    async def get(self, key: str):
        self.read_keys.append(key)
        return self.values.get(key)

    async def set(self, key: str, value, *, ttl_seconds: int):
        self.values[key] = value


class _BlockingCache(_RawCache):
    def __init__(self) -> None:
        super().__init__()
        self.started = asyncio.Event()
        self.release = asyncio.Event()

    async def get(self, key: str):
        self.read_keys.append(key)
        self.started.set()
        await self.release.wait()
        return self.values.get(key)


@pytest.fixture(autouse=True)
def restore_musicbrainz_source():
    before = mb_base.capture_mb_source_context()
    before_runtime = mb_base.brainzmash_runtime_enabled()
    yield
    mb_base.set_mb_api_base(
        before.source_url,
        source_mode=before.source_mode,
        source_id=before.source_id,
        generation=before.generation,
        brainzmash_binding_valid=before_runtime,
    )


@pytest.mark.asyncio
async def test_cache_fence_never_reads_legacy_unscoped_entry():
    mb_base.set_mb_api_base(
        mb_base.OFFICIAL_MB_API_BASE,
        source_mode="official",
        source_id="current-source",
        generation=4,
    )
    context = mb_base.capture_mb_source_context()
    key = mb_artist_search_key("legacy", 10, 0)
    cache = _RawCache()
    cache.values[key] = ["legacy"]

    assert await mb_cache_get_if_current(cache, key, context) is None
    assert cache.read_keys == [namespace_mb_cache_key(key, context)]


@pytest.mark.asyncio
async def test_cache_fence_does_not_read_previous_generation_namespace():
    old = MbSourceContext(
        source_url=mb_base.OFFICIAL_MB_API_BASE,
        source_mode="official",
        source_id="old-source",
        generation=3,
    )
    old_key = namespace_mb_cache_key(mb_artist_search_key("album", 10, 0), old)
    cache = _RawCache()
    cache.values[old_key] = ["old"]

    mb_base.set_mb_api_base(
        mb_base.OFFICIAL_MB_API_BASE,
        source_mode="official",
        source_id="new-source",
        generation=4,
    )
    current = mb_base.capture_mb_source_context()

    assert (
        await mb_cache_get_if_current(
            cache, mb_artist_search_key("album", 10, 0), current
        )
        is None
    )
    assert cache.read_keys == [
        namespace_mb_cache_key(mb_artist_search_key("album", 10, 0), current)
    ]


@pytest.mark.asyncio
async def test_cache_fence_discards_value_when_source_changes_during_read():
    mb_base.set_mb_api_base(
        mb_base.OFFICIAL_MB_API_BASE,
        source_mode="official",
        source_id="old-source",
        generation=3,
    )
    old = mb_base.capture_mb_source_context()
    cache = _BlockingCache()
    task = asyncio.create_task(
        mb_cache_get_if_current(cache, mb_artist_search_key("album", 10, 0), old)
    )
    await cache.started.wait()

    mb_base.set_mb_api_base(
        "https://mirror.example/ws/2",
        source_mode="mirror",
        source_id="new-source",
        generation=4,
    )
    cache.values[cache.read_keys[0]] = ["old"]
    cache.release.set()

    assert await task is None


class _ArtistRepo(MusicBrainzArtistMixin):
    def __init__(self, cache: _RawCache) -> None:
        self._cache = cache
        self._preferences_service = SimpleNamespace(
            get_advanced_settings=lambda: SimpleNamespace(cache_ttl_search=60)
        )


@pytest.mark.asyncio
async def test_artist_search_uses_scoped_key_instead_of_legacy_cache(monkeypatch):
    mb_base.set_mb_api_base(
        mb_base.OFFICIAL_MB_API_BASE,
        source_mode="official",
        source_id="search-source",
        generation=7,
    )
    cache = _RawCache()
    key = mb_artist_search_key("current", 10, 0)
    cache.values[key] = []

    async def fake_get(*_args, **_kwargs):
        return SimpleNamespace(artists=[{"id": "artist-1", "name": "Current Artist"}])

    monkeypatch.setattr(artist_module, "mb_api_get", fake_get)
    artist_module.mb_deduplicator.clear()
    try:
        results = await _ArtistRepo(cache).search_artists("current")
    finally:
        artist_module.mb_deduplicator.clear()

    assert [result.title for result in results] == ["Current Artist"]
    assert cache.read_keys == [
        namespace_mb_cache_key(key, mb_base.capture_mb_source_context())
    ]
