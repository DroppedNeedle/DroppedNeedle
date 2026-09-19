import asyncio
import threading
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
import pytest_asyncio

import repositories.musicbrainz_base as mb
from core.exceptions import ConfigurationError
from infrastructure.http import mb_singleflight
from infrastructure.observability.optional_work import (
    OptionalWorkBudget,
    OptionalWorkDeferred,
    optional_work_budget,
)
from infrastructure.persistence.mb_response_store import MAX_PAYLOAD_BYTES, MbResponseStore
from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_response_cache import MbCachePolicy, request_key


_PATH = "/artist/a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e"
_PARAMS = {"inc": "tags+aliases+url-rels"}
_PAYLOAD = {"id": "a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e", "name": "Fixture Artist"}


@pytest_asyncio.fixture
async def response_runtime(tmp_path, monkeypatch):
    original = mb.capture_mb_source_context()
    original_runtime = mb.brainzmash_runtime_enabled()
    lock = threading.Lock()
    store = MbResponseStore(tmp_path / "shared.sqlite", lock)
    state = SimpleNamespace(store=store, lock=lock, calls=[], handler=None)

    async def transport(request):
        state.calls.append(request)
        if state.handler is not None:
            return await state.handler(request)
        return httpx.Response(200, json=_PAYLOAD)

    async with httpx.AsyncClient(transport=httpx.MockTransport(transport)) as client:
        monkeypatch.setattr(mb, "_http_client", client)
        monkeypatch.setattr(mb, "_mb_response_store", store)
        monkeypatch.setattr(mb, "mb_rate_limiter", SimpleNamespace(acquire=AsyncMock()))
        monkeypatch.setattr(mb_singleflight, "_owners", {})
        monkeypatch.setattr(mb_singleflight, "_capacity", asyncio.Condition())
        mb.mb_circuit_breaker.reset()
        mb.set_mb_api_base(
            mb.OFFICIAL_MB_API_BASE,
            source_mode="official",
            source_id="response-fixture",
            generation=original.generation + 1,
        )
        try:
            yield state
        finally:
            tasks = [owner.task for owner in mb_singleflight._owners.values() if owner.task]
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            mb.clear_mb_response_context()
            mb.mb_circuit_breaker.reset()
            mb.set_mb_api_base(
                original.source_url,
                source_mode=original.source_mode,
                source_id=original.source_id,
                generation=original.generation,
                brainzmash_binding_valid=original_runtime,
            )


async def _display():
    return await mb.mb_api_get(_PATH, _PARAMS, cache_policy=MbCachePolicy.DISPLAY_FRESH)


@pytest.mark.asyncio
async def test_fresh_response_survives_store_recreation_but_strict_request_bypasses(
    response_runtime, monkeypatch
):
    state = response_runtime
    assert await _display() == _PAYLOAD
    first = mb.get_mb_response_metadata()
    recreated = MbResponseStore(state.store.db_path, state.lock)
    monkeypatch.setattr(mb, "_mb_response_store", recreated)
    assert await _display() == _PAYLOAD
    cached = mb.get_mb_response_metadata()
    assert len(state.calls) == 1
    assert cached.origin == "l2"
    assert cached.fetched_at == first.fetched_at
    assert cached.fresh_until == first.fresh_until
    assert mb.get_mb_response_context().source_id == cached.source_id
    assert await mb.mb_api_get(_PATH, _PARAMS) == _PAYLOAD
    assert len(state.calls) == 2
    assert mb.get_mb_response_metadata().origin == "provider"


@pytest.mark.asyncio
async def test_clear_while_admission_waits_rejects_old_epoch(response_runtime, monkeypatch):
    store = response_runtime.store
    entered = asyncio.Event()
    release = asyncio.Event()
    original_admit = store.admit

    async def paused_admit(*args, **kwargs):
        entered.set()
        await release.wait()
        return await original_admit(*args, **kwargs)

    monkeypatch.setattr(store, "admit", paused_admit)
    pending = asyncio.create_task(_display())
    try:
        await asyncio.wait_for(entered.wait(), timeout=2)
        await store.clear()
        release.set()
        assert await pending == _PAYLOAD
        assert store.stats()["response_entries"] == 0
        assert await _display() == _PAYLOAD
        assert store.stats()["response_entries"] == 1
        assert len(response_runtime.calls) == 2
    finally:
        release.set()
        await asyncio.gather(pending, return_exceptions=True)


@pytest.mark.asyncio
async def test_oversized_valid_response_returns_without_admission(response_runtime):
    body = b'{"id":"a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e","blob":"' + b"x" * MAX_PAYLOAD_BYTES + b'"}'

    async def oversized(_request):
        return httpx.Response(200, content=body)

    response_runtime.handler = oversized
    result = await _display()
    assert result["blob"] == "x" * MAX_PAYLOAD_BYTES
    assert response_runtime.store.stats()["response_entries"] == 0


@pytest.mark.asyncio
async def test_corrupt_cached_bytes_refetch_once_and_restore_valid_entry(response_runtime):
    state = response_runtime
    assert await _display() == _PAYLOAD
    await state.store._write(
        lambda conn: conn.execute("UPDATE mb_responses SET payload=?", (b"not-json",))
    )
    assert await _display() == _PAYLOAD
    assert await _display() == _PAYLOAD
    assert len(state.calls) == 2
    assert state.store.stats()["response_entries"] == 1
    assert mb.get_mb_response_metadata().origin == "l2"


@pytest.mark.asyncio
@pytest.mark.parametrize("during_cache_read", [False, True])
async def test_source_switch_rejects_old_response_and_read(
    response_runtime, monkeypatch, during_cache_read
):
    state = response_runtime
    entered = asyncio.Event()
    release = asyncio.Event()
    old = mb.capture_mb_source_context()
    if during_cache_read:
        await _display()
        original_get = state.store.get

        async def paused_get(*args, **kwargs):
            entered.set()
            await release.wait()
            return await original_get(*args, **kwargs)

        monkeypatch.setattr(state.store, "get", paused_get)
    else:
        async def paused_http(_request):
            entered.set()
            await release.wait()
            return httpx.Response(200, json=_PAYLOAD)

        state.handler = paused_http
    pending = asyncio.create_task(_display())
    try:
        await asyncio.wait_for(entered.wait(), timeout=2)
        mb.set_mb_api_base(
            mb.OFFICIAL_MB_API_BASE, source_mode="official",
            source_id="replacement-source", generation=old.generation + 1,
        )
        release.set()
        with pytest.raises(ConfigurationError):
            await pending
        assert await _display() == _PAYLOAD
        assert mb.get_mb_response_metadata().source_id == "replacement-source"
        assert len(state.calls) == 2
    finally:
        release.set()
        await asyncio.gather(pending, return_exceptions=True)


@pytest.mark.asyncio
async def test_each_on_wire_waiter_receives_provider_metadata(response_runtime):
    state = response_runtime
    entered = asyncio.Event()
    release = asyncio.Event()

    async def paused_http(_request):
        entered.set()
        await release.wait()
        return httpx.Response(200, json=_PAYLOAD)

    state.handler = paused_http

    async def consume():
        value = await _display()
        return value, mb.get_mb_response_metadata(), mb.get_mb_response_context()

    first = asyncio.create_task(consume())
    second = None
    try:
        await asyncio.wait_for(entered.wait(), timeout=2)
        second = asyncio.create_task(consume())
        key = request_key(_PATH, _PARAMS, None, mb.capture_mb_source_context())
        async with asyncio.timeout(2):
            while len(mb_singleflight._owners[key].waiters) < 2:
                await asyncio.sleep(0)
        release.set()
        results = await asyncio.gather(first, second)
        assert len(state.calls) == 1
        for value, metadata, context in results:
            assert value == _PAYLOAD
            assert metadata.origin == "provider"
            assert metadata.source_id == context.source_id == "response-fixture"
            assert metadata.fetched_at < metadata.fresh_until < metadata.retention_until
        assert results[0][1] == results[1][1]
    finally:
        release.set()
        await asyncio.gather(*[task for task in (first, second) if task], return_exceptions=True)


@pytest_asyncio.fixture
async def isolated_owners(monkeypatch):
    monkeypatch.setattr(mb_singleflight, "_owners", {})
    monkeypatch.setattr(mb_singleflight, "_capacity", asyncio.Condition())
    yield
    tasks = [owner.task for owner in mb_singleflight._owners.values() if owner.task]
    for task in tasks:
        task.cancel()
    await asyncio.gather(*tasks, return_exceptions=True)


@pytest.mark.asyncio
async def test_queued_background_promotes_without_waiting_for_background_gate(isolated_owners):
    entered = asyncio.Event()
    blocked = asyncio.Event()
    dispatched = []

    async def factory(priority):
        if priority == RequestPriority.BACKGROUND_SYNC:
            entered.set()
            await blocked.wait()
        mb_singleflight.current_owner.get().before_dispatch()
        dispatched.append(priority)
        return "result"

    background = asyncio.create_task(mb_singleflight.run("promote", RequestPriority.BACKGROUND_SYNC, factory))
    await asyncio.wait_for(entered.wait(), timeout=2)
    foreground = asyncio.create_task(mb_singleflight.run("promote", RequestPriority.USER_INITIATED, factory))
    async with asyncio.timeout(2):
        assert await asyncio.gather(background, foreground) == ["result", "result"]
    assert dispatched == [RequestPriority.USER_INITIATED]
    assert not blocked.is_set()


@pytest.mark.asyncio
async def test_on_wire_owner_survives_one_cancelled_waiter(isolated_owners):
    started = asyncio.Event()
    release = asyncio.Event()
    calls = 0

    async def factory(priority):
        nonlocal calls
        calls += 1
        mb_singleflight.current_owner.get().before_dispatch()
        started.set()
        await release.wait()
        return "shared"

    first = asyncio.create_task(mb_singleflight.run("wire", RequestPriority.BACKGROUND_SYNC, factory))
    await asyncio.wait_for(started.wait(), timeout=2)
    second = asyncio.create_task(mb_singleflight.run("wire", RequestPriority.USER_INITIATED, factory))
    await asyncio.sleep(0)
    first.cancel()
    with pytest.raises(asyncio.CancelledError):
        await first
    release.set()
    assert await second == "shared"
    assert calls == 1


@pytest.mark.asyncio
async def test_last_waiter_cancels_queued_work_and_refunds_once(isolated_owners):
    entered = asyncio.Event()
    cancelled = asyncio.Event()
    blocked = asyncio.Event()
    budget = OptionalWorkBudget()

    async def factory(priority):
        entered.set()
        try:
            await blocked.wait()
        finally:
            cancelled.set()

    with optional_work_budget(budget):
        waiter = asyncio.create_task(mb_singleflight.run("cancel-last", RequestPriority.BACKGROUND_SYNC, factory))
    await asyncio.wait_for(entered.wait(), timeout=2)
    assert budget.remaining == 9
    waiter.cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiter
    async with asyncio.timeout(2):
        await cancelled.wait()
        while budget.remaining != 10:
            await asyncio.sleep(0)
    assert not blocked.is_set()


@pytest.mark.asyncio
async def test_foreground_takes_over_disabled_optional_owner(isolated_owners):
    entered = asyncio.Event()
    blocked = asyncio.Event()
    enabled = True
    budget = OptionalWorkBudget(guard=lambda: enabled)
    dispatched = []

    async def factory(priority):
        if priority == RequestPriority.BACKGROUND_SYNC:
            entered.set()
            await blocked.wait()
        mb_singleflight.current_owner.get().before_dispatch()
        dispatched.append(priority)
        return "foreground-data"

    with optional_work_budget(budget):
        background = asyncio.create_task(mb_singleflight.run("takeover", RequestPriority.BACKGROUND_SYNC, factory))
    await asyncio.wait_for(entered.wait(), timeout=2)
    enabled = False
    foreground = asyncio.create_task(mb_singleflight.run("takeover", RequestPriority.USER_INITIATED, factory))
    async with asyncio.timeout(2):
        assert await asyncio.gather(background, foreground) == ["foreground-data", "foreground-data"]
    assert dispatched == [RequestPriority.USER_INITIATED]


@pytest.mark.asyncio
async def test_shared_optional_budget_admits_only_ten_physical_owners(isolated_owners):
    budget = OptionalWorkBudget()
    dispatched = []

    async def factory(priority):
        mb_singleflight.current_owner.get().before_dispatch()
        dispatched.append(priority)
        return "admitted"

    with optional_work_budget(budget):
        results = await asyncio.gather(
            *[
                mb_singleflight.run(f"budget-{index}", RequestPriority.BACKGROUND_SYNC, factory)
                for index in range(11)
            ],
            return_exceptions=True,
        )
    assert results.count("admitted") == 10
    assert sum(isinstance(result, OptionalWorkDeferred) for result in results) == 1
    assert len(dispatched) == 10
    assert budget.remaining == 0


@pytest.mark.asyncio
async def test_near_expiry_artist_projection_survives_gather_l1_and_disk_restart(
    response_runtime, tmp_path, monkeypatch
):
    import time
    from infrastructure.cache.memory_cache import InMemoryCache
    from infrastructure.cache.disk_cache import DiskMetadataCache
    from repositories.musicbrainz_artist import MusicBrainzArtistMixin
    from services.artist_service import ArtistService, _artist_source_context
    from models.artist import ArtistInfo

    async def transport(request):
        if request.url.path.endswith("/release-group"):
            return httpx.Response(200, json={"release-groups": [{"id": "rg", "title": "Album"}], "release-group-count": 1})
        return httpx.Response(200, json=_PAYLOAD)

    response_runtime.handler = transport
    repo = MusicBrainzArtistMixin()
    repo._cache = InMemoryCache()
    await repo.get_artist_by_id("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e")
    deadline = time.time() + 20
    await response_runtime.store._write(
        lambda conn: conn.execute(
            "UPDATE mb_responses SET fresh=?", (deadline,)
        )
    )
    await repo._cache.clear()
    await repo.get_artist_by_id("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e")
    assert len(response_runtime.calls) == 2
    assert mb.get_mb_response_metadata().fresh_until == deadline

    cache = InMemoryCache()
    disk = DiskMetadataCache(tmp_path / "artist-cache")
    library = SimpleNamespace(
        existing_artist_mbids=AsyncMock(return_value=set()),
        existing_album_mbids=AsyncMock(return_value=set()),
        get_requested_mbids=AsyncMock(return_value=set()),
    )
    prefs = SimpleNamespace(get_advanced_settings=lambda: SimpleNamespace(
        cache_ttl_artist_library=86400, cache_ttl_artist_non_library=3600,
    ))
    service = ArtistService(repo, library, None, prefs, cache, disk)
    source = mb.capture_mb_source_context()
    _artist_source_context.set(source)
    service._begin_projection()
    artist, *_ = await service._fetch_artist_data("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e", source_context=source)
    assert mb.get_mb_response_metadata().fresh_until == deadline
    info = ArtistInfo(name=artist["name"], musicbrainz_id="a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e", in_library=True)
    written = asyncio.Event()
    original_set = disk.set_artist

    async def record_write(*args, **kwargs):
        await original_set(*args, **kwargs)
        written.set()

    monkeypatch.setattr(disk, "set_artist", record_write)
    await service._save_artist_to_cache("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e", info)
    await asyncio.wait_for(written.wait(), timeout=2)
    assert await service._get_cached_artist("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e") == info
    assert mb.get_mb_response_metadata().fresh_until == deadline
    await cache.clear()
    service._disk_cache = DiskMetadataCache(tmp_path / "artist-cache")
    assert await service._get_cached_artist("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e") == info
    assert mb.get_mb_response_metadata().fresh_until == deadline
    monkeypatch.setattr(time, "time", lambda: deadline + 1)
    assert await service._get_cached_artist("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e") is None


@pytest.mark.asyncio
async def test_artist_projection_combines_unequal_payload_deadlines(response_runtime):
    import time
    from infrastructure.cache.memory_cache import InMemoryCache
    from repositories.musicbrainz_artist import MusicBrainzArtistMixin

    async def transport(request):
        if request.url.path.endswith("/release-group"):
            return httpx.Response(200, json={"release-groups": [], "release-group-count": 0})
        return httpx.Response(200, json=_PAYLOAD)

    response_runtime.handler = transport
    repo = MusicBrainzArtistMixin()
    repo._cache = InMemoryCache()
    await repo.get_artist_by_id("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e")
    source = mb.capture_mb_source_context()
    core_key = request_key(_PATH, _PARAMS, None, source)
    earliest = time.time() + 10
    await response_runtime.store._write(lambda conn: conn.execute(
        "UPDATE mb_responses SET fresh=CASE WHEN key=? THEN ? ELSE ? END",
        (core_key, earliest + 30, earliest),
    ))
    await repo._cache.clear()
    await repo.get_artist_by_id("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e")
    assert mb.get_mb_response_metadata().fresh_until == earliest
    await repo.get_artist_by_id("a3cb0e2e-6f3a-4b2c-9d1e-5f8a7b6c4d3e")
    assert mb.get_mb_response_metadata().fresh_until == earliest
    assert len(response_runtime.calls) == 2


@pytest.mark.asyncio
async def test_projection_publication_rejects_response_clear_epoch(response_runtime):
    from infrastructure.cache.memory_cache import InMemoryCache

    await _display()
    cache = InMemoryCache()
    token = cache.capture_clear_token()
    source = mb.capture_mb_source_context()
    await response_runtime.store.clear()
    assert not await mb.mb_cache_set_if_current(
        cache, "mb_artist:old", _PAYLOAD, ttl_seconds=1000,
        context=source, cache_token=token,
    )


@pytest.mark.asyncio
async def test_disk_artist_projection_rejects_prefetch_clear_token(tmp_path):
    from infrastructure.cache.disk_cache import DiskMetadataCache

    disk = DiskMetadataCache(tmp_path)
    token = disk.capture_clear_token()
    await disk.clear_musicbrainz()
    await disk.set_artist("artist", {"name": "old"}, cache_token=token)
    assert await disk.get_artist("artist") is None


@pytest.mark.asyncio
async def test_promoting_artist_disk_tier_preserves_source_expiry(
    response_runtime, tmp_path, monkeypatch
):
    import time
    from dataclasses import replace
    from infrastructure.cache.disk_cache import DiskMetadataCache

    await _display()
    deadline = time.time() + 10
    metadata = replace(mb.get_mb_response_metadata(), fresh_until=deadline)
    disk = DiskMetadataCache(tmp_path)
    await disk.set_artist("artist", _PAYLOAD, ttl_seconds=3600, metadata=metadata)
    assert await disk.promote_artist_to_persistent("artist")
    restarted = DiskMetadataCache(tmp_path)
    payload, persisted = await restarted.get_artist_with_metadata("artist")
    assert payload == _PAYLOAD
    assert persisted.fresh_until == deadline
    monkeypatch.setattr(time, "time", lambda: deadline + 1)
    assert await restarted.get_artist("artist") is None


@pytest.mark.asyncio
async def test_service_projection_clear_during_assembly_blocks_both_tiers(
    response_runtime, tmp_path
):
    from infrastructure.cache.memory_cache import InMemoryCache
    from infrastructure.cache.disk_cache import DiskMetadataCache
    from services.artist_service import ArtistService, _artist_source_context
    from models.artist import ArtistInfo

    cache = InMemoryCache()
    disk = DiskMetadataCache(tmp_path)
    prefs = SimpleNamespace(get_advanced_settings=lambda: SimpleNamespace(
        cache_ttl_artist_library=86400, cache_ttl_artist_non_library=3600,
    ))
    service = ArtistService(None, None, None, prefs, cache, disk)
    _artist_source_context.set(mb.capture_mb_source_context())
    service._begin_projection()
    await _display()
    await cache.clear()
    info = ArtistInfo(name="Old", musicbrainz_id="artist")
    await service._save_artist_to_cache("artist", info)
    assert await service._get_cached_artist("artist") is None
    assert await disk.get_artist("artist") is None
