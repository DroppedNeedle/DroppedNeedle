from types import SimpleNamespace
from unittest.mock import MagicMock

import asyncio
import pytest

import repositories.musicbrainz_album as mb_album
import repositories.musicbrainz_base as mb_base
from api.v1.schemas.settings import MusicBrainzConnectionSettings
from infrastructure.cache.memory_cache import InMemoryCache
from repositories.musicbrainz_album import MusicBrainzAlbumMixin
from services.settings_service import SettingsService


class _BlockingCache(InMemoryCache):
    def __init__(self, started: asyncio.Event, release: asyncio.Event, events: list):
        super().__init__()
        self.started = started
        self.release = release
        self.events = events

    async def set(self, key, value, *, ttl_seconds):
        self.started.set()
        await self.release.wait()
        self.events.append(("cache", mb_base.get_mb_api_base()))
        await super().set(key, value, ttl_seconds=ttl_seconds)


class _BlockingStore:
    def __init__(self, started: asyncio.Event, release: asyncio.Event, events: list):
        self.started = started
        self.release = release
        self.events = events
        self.saved = []

    async def save_release_to_rg(
        self, mapping, source_host=None, *, source_context=None
    ):
        async def write():
            self.started.set()
            await self.release.wait()
            self.events.append(("store", mb_base.get_mb_api_base()))
            self.saved.append(
                (
                    dict(mapping),
                    source_host
                    or (
                        source_context.source_url.split("/ws/2", 1)[0]
                        if source_context is not None
                        else None
                    ),
                )
            )

        await mb_base.mb_publish_if_current(source_context, write)


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self, cache):
        self._cache = cache
        self._preferences_service = MagicMock()
        self._preferences_service.get_advanced_settings.return_value = SimpleNamespace(
            cache_ttl_search=60
        )


@pytest.fixture
def restore_transport_state():
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    original_rate = mb_base.mb_rate_limiter.rate
    original_capacity = mb_base.mb_rate_limiter.capacity
    original_bypass = mb_base.mb_rate_limiter_bypassed()
    mb_base.set_mb_rate_limiter_bypass(False)
    mb_base.mb_circuit_breaker.reset()
    yield
    mb_base.set_mb_api_base(
        original_source.source_url,
        source_mode=original_source.source_mode,
        source_id=original_source_id,
        generation=original_source.generation,
        brainzmash_binding_valid=original_runtime,
    )
    mb_base.mb_rate_limiter.update_rate(original_rate)
    mb_base.mb_rate_limiter.update_capacity(original_capacity)
    mb_base.set_mb_rate_limiter_bypass(original_bypass)
    mb_base.mb_circuit_breaker.reset()


def _settings(url: str) -> MusicBrainzConnectionSettings:
    return MusicBrainzConnectionSettings(
        source_mode="mirror",
        selected_source_mode="mirror",
        api_url=url,
        rate_limit=2.0,
        concurrent_searches=4,
    )


@pytest.mark.asyncio
async def test_cache_publication_commits_before_source_switch(
    restore_transport_state, monkeypatch
):
    old_source = "https://old.example/ws/2"
    new_source = "https://new.example/ws/2"
    mb_base.set_mb_api_base(
        old_source,
        source_mode="mirror",
        source_id="source-commit-cache-old",
        generation=mb_base.get_mb_source_generation() + 1,
    )
    old_context = mb_base.MbSourceContext(
        old_source,
        mb_base.get_mb_source_generation(),
        "mirror",
        "source-commit-cache-old",
    )
    started = asyncio.Event()
    release = asyncio.Event()
    events = []
    cache = _BlockingCache(started, release, events)
    service = SettingsService(preferences_service=None, cache=cache)
    source_events = []
    original_set = mb_base.set_mb_api_base

    def record_source(url, **kwargs):
        original_set(url, **kwargs)
        source_events.append(("source", mb_base.get_mb_api_base()))

    monkeypatch.setattr(mb_base, "set_mb_api_base", record_source)
    publication = asyncio.create_task(
        mb_base.mb_cache_set_if_current(
            cache,
            "mb:artist:search:fence",
            "old",
            ttl_seconds=60,
            context=old_context,
        )
    )
    await started.wait()
    switch = asyncio.create_task(
        service.on_musicbrainz_settings_changed(_settings(new_source))
    )
    await asyncio.sleep(0)
    assert not switch.done()

    release.set()
    assert await publication is True
    await switch

    assert events == [("cache", old_source)]
    assert source_events == [("source", new_source)]
    assert await cache.get("mb:artist:search:fence") is None


@pytest.mark.asyncio
async def test_durable_publication_commits_before_source_switch(
    restore_transport_state, monkeypatch
):
    old_source = "https://old.example/ws/2"
    new_source = "https://new.example/ws/2"
    mb_base.set_mb_api_base(
        old_source,
        source_mode="mirror",
        source_id="source-commit-store-old",
        generation=mb_base.get_mb_source_generation() + 1,
    )
    old_context = mb_base.MbSourceContext(
        old_source,
        mb_base.get_mb_source_generation(),
        "mirror",
        "source-commit-store-old",
    )
    started = asyncio.Event()
    release = asyncio.Event()
    events = []
    cache = InMemoryCache()
    repo = _Repo(cache)
    store = _BlockingStore(started, release, events)
    repo._mb_canonical_store = store
    service = SettingsService(preferences_service=None, cache=cache)
    source_events = []
    original_set = mb_base.set_mb_api_base

    def record_source(url, **kwargs):
        original_set(url, **kwargs)
        source_events.append(("source", mb_base.get_mb_api_base()))

    monkeypatch.setattr(mb_base, "set_mb_api_base", record_source)

    async def provider(*_args, **_kwargs):
        mb_base._mb_response_context.set(old_context)
        return SimpleNamespace(release_group={"id": "rg-old"}, media=[])

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    lookup = asyncio.create_task(
        repo._fetch_release_group_id_from_release(
            "rel-fence", "mb:release_to_rg:rel-fence"
        )
    )
    await started.wait()
    switch = asyncio.create_task(
        service.on_musicbrainz_settings_changed(_settings(new_source))
    )
    await asyncio.sleep(0)
    assert not switch.done()

    release.set()
    assert await lookup == "rg-old"
    await switch

    assert events == [("store", old_source)]
    assert source_events == [("source", new_source)]
    assert store.saved == [({"rel-fence": "rg-old"}, "https://old.example")]
    assert await cache.get("mb:release_to_rg:rel-fence") is None


@pytest.mark.asyncio
async def test_cancelled_durable_publication_settles_before_releasing_source_lock(
    restore_transport_state,
):
    old_source = "https://old.example/ws/2"
    new_source = "https://new.example/ws/2"
    mb_base.set_mb_api_base(
        old_source,
        source_mode="mirror",
        source_id="source-commit-cancel-old",
        generation=mb_base.get_mb_source_generation() + 1,
    )
    old_context = mb_base.MbSourceContext(
        old_source,
        mb_base.get_mb_source_generation(),
        "mirror",
        "source-commit-cancel-old",
    )
    started = asyncio.Event()
    release = asyncio.Event()
    events = []
    store = _BlockingStore(started, release, events)
    service = SettingsService(preferences_service=None, cache=InMemoryCache())

    publication = asyncio.create_task(
        store.save_release_to_rg(
            {"rel-cancel": "rg-old"},
            old_source,
            source_context=old_context,
        )
    )
    await started.wait()
    publication.cancel()
    await asyncio.sleep(0)
    assert not publication.done()
    assert mb_base.mb_source_commit_lock.locked()

    switch = asyncio.create_task(
        service.on_musicbrainz_settings_changed(_settings(new_source))
    )
    await asyncio.sleep(0)
    assert not switch.done()

    release.set()
    with pytest.raises(asyncio.CancelledError):
        await publication
    await switch
    assert events == [("store", old_source)]
    assert mb_base.get_mb_api_base() == new_source
