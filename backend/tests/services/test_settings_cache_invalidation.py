"""Integration tests - SettingsService invalidation methods clear the right cache keys."""

from types import SimpleNamespace

from unittest.mock import AsyncMock, MagicMock

import pytest

from infrastructure.cache.cache_keys import (
    ALBUM_INFO_PREFIX,
    ARTIST_DISCOVERY_PREFIX,
    ARTIST_DISCOVERY_TOP_ALBUMS_PREFIX,
    ARTIST_DISCOVERY_TOP_SONGS_PREFIX,
    ARTIST_INFO_PREFIX,
    DISCOVER_QUEUE_ENRICH_PREFIX,
    DISCOVER_RESPONSE_PREFIX,
    GENRE_ARTIST_PREFIX,
    HOME_RESPONSE_PREFIX,
    JELLYFIN_PREFIX,
    LB_PREFIX,
    LFM_PREFIX,
    LIBRARY_ARTIST_ALBUMS_PREFIX,
    LOCAL_FILES_PREFIX,
    SOURCE_RESOLUTION_PREFIX,
    musicbrainz_prefixes,
)
from infrastructure.cache.memory_cache import InMemoryCache
from services.settings_service import SettingsService


async def _build_service(disk_cache=None) -> tuple[SettingsService, InMemoryCache]:
    cache = InMemoryCache(max_entries=500)
    service = SettingsService(
        preferences_service=None,
        cache=cache,
        disk_cache=disk_cache,
    )
    return service, cache


async def _populate(cache: InMemoryCache, keys: list[str]) -> None:
    for key in keys:
        await cache.set(key, "v", ttl_seconds=300)


def _prefs(primary: list[str], secondary: list[str]):
    from api.v1.schemas.settings import UserPreferences

    return UserPreferences(primary_types=primary, secondary_types=secondary)


@pytest.mark.asyncio(loop_scope="function")
async def test_identical_preference_change_is_a_no_op():
    """ST1: an unchanged PUT /settings/preferences payload must not clear any
    cache entries (previously it wiped artist_info/album_info/artist_albums +
    every musicbrainz prefix)."""
    service, cache = await _build_service()

    keys = [
        ARTIST_INFO_PREFIX + "art1",
        ALBUM_INFO_PREFIX + "alb1",
        *[f"{p}mb{i}" for i, p in enumerate(musicbrainz_prefixes())],
        "unrelated:key",
    ]
    await _populate(cache, keys)

    same = _prefs(["album", "ep", "single"], ["studio"])
    cleared = await service.apply_preference_change(same, same)

    assert cleared == 0
    for key in keys:
        assert await cache.get(key) == "v"


@pytest.mark.asyncio(loop_scope="function")
async def test_changed_preference_types_flush_search_cache_only(monkeypatch):
    """ST1: changed type filters require ZERO prefix sweeps - raw MB caches
    filter per request and search results embed sorted types in their key.
    The in-process search cache is still flushed as transition insurance."""
    from services.search_service import SearchService

    service, cache = await _build_service()
    mb_keys = [f"{p}keep" for p in musicbrainz_prefixes()]
    await _populate(cache, mb_keys)

    flushed = {"n": 0}
    original = SearchService.clear_cached_results.__func__

    def spy_clear(cls):
        flushed["n"] += 1
        return original(cls)

    monkeypatch.setattr(SearchService, "clear_cached_results", classmethod(spy_clear))

    previous = _prefs(["album", "ep", "single"], ["studio"])
    changed = _prefs(["album"], ["live", "studio"])
    cleared = await service.apply_preference_change(previous, changed)

    assert cleared == 0
    assert flushed["n"] == 1
    for key in mb_keys:
        # Zero prefix sweeps: MB-derived caches stay warm across pref changes.
        assert await cache.get(key) == "v"


@pytest.mark.asyncio(loop_scope="function")
async def test_unknown_previous_treated_as_changed():
    service, _cache = await _build_service()

    flushed = {"n": 0}
    monkeypatch = pytest.MonkeyPatch()
    try:
        from services.search_service import SearchService

        original = SearchService.clear_cached_results.__func__

        def spy_clear(cls):
            flushed["n"] += 1
            return original(cls)

        monkeypatch.setattr(
            SearchService, "clear_cached_results", classmethod(spy_clear)
        )
        cleared = await service.apply_preference_change(None, _prefs(["album"], []))
    finally:
        monkeypatch.undo()

    assert cleared == 0
    assert flushed["n"] == 1


@pytest.mark.asyncio(loop_scope="function")
async def test_clear_home_cache():
    service, cache = await _build_service()

    home_keys = [
        f"{HOME_RESPONSE_PREFIX}page1",
        f"{DISCOVER_RESPONSE_PREFIX}rock",
        f"{GENRE_ARTIST_PREFIX}pop",
        f"{JELLYFIN_PREFIX}lib",
        f"{LB_PREFIX}stats",
        f"{LFM_PREFIX}chart",
    ]
    unrelated = ["unrelated:key"]
    await _populate(cache, home_keys + unrelated)

    cleared = await service.clear_home_cache()

    assert cleared == len(home_keys)
    for key in home_keys:
        assert await cache.get(key) is None
    assert await cache.get("unrelated:key") == "v"


@pytest.mark.asyncio(loop_scope="function")
async def test_clear_source_resolution_cache():
    """SOURCE_RESOLUTION_PREFIX = 'source_resolution' (no trailing colon)
    must match both 'source_resolution:x' and 'source_resolution_tracks:y'."""
    service, cache = await _build_service()

    sr_keys = [
        f"{SOURCE_RESOLUTION_PREFIX}:track1",
        f"{SOURCE_RESOLUTION_PREFIX}_tracks:track2",
    ]
    unrelated = ["unrelated:key"]
    await _populate(cache, sr_keys + unrelated)

    cleared = await service.clear_source_resolution_cache()

    assert cleared == len(sr_keys)
    for key in sr_keys:
        assert await cache.get(key) is None
    assert await cache.get("unrelated:key") == "v"


@pytest.mark.asyncio(loop_scope="function")
async def test_clear_local_files_cache():
    service, cache = await _build_service()

    lf_keys = [f"{LOCAL_FILES_PREFIX}scan1", f"{LOCAL_FILES_PREFIX}scan2"]
    unrelated = ["unrelated:key"]
    await _populate(cache, lf_keys + unrelated)

    cleared = await service.clear_local_files_cache()

    assert cleared == len(lf_keys)
    for key in lf_keys:
        assert await cache.get(key) is None
    assert await cache.get("unrelated:key") == "v"


@pytest.mark.asyncio(loop_scope="function")
async def test_unrelated_keys_survive():
    """Clearing one domain must not affect keys from other domains."""
    service, cache = await _build_service()

    survivor_keys = [
        f"{LOCAL_FILES_PREFIX}scan",
        f"{SOURCE_RESOLUTION_PREFIX}:t1",
        f"{LIBRARY_ARTIST_ALBUMS_PREFIX}mbid1",
    ]
    target_keys = [f"{p}x" for p in musicbrainz_prefixes()]
    await _populate(cache, survivor_keys + target_keys)

    await service.clear_home_cache()

    for key in survivor_keys:
        assert await cache.get(key) == "v", f"Key {key!r} was incorrectly cleared"


@pytest.mark.asyncio(loop_scope="function")
async def test_jellyfin_settings_change_clears_user_import_service():
    """Phase 6: configuring Jellyfin must rebuild the import service singleton so a
    newly-configured server is enumerable without an app restart."""
    from unittest.mock import patch, MagicMock, AsyncMock

    service, _cache = await _build_service()
    mbid = MagicMock()
    mbid.clear_jellyfin_mbid_index = AsyncMock()
    import_fn = MagicMock()
    auth_fn = MagicMock()

    with (
        patch("core.dependencies.get_jellyfin_repository", MagicMock()),
        patch("core.dependencies.get_jellyfin_playback_service", MagicMock()),
        patch("core.dependencies.get_jellyfin_library_service", MagicMock()),
        patch("core.dependencies.get_home_service", MagicMock()),
        patch("core.dependencies.get_home_charts_service", MagicMock()),
        patch("core.dependencies.get_mbid_store", MagicMock(return_value=mbid)),
        patch("core.dependencies.auth_providers.get_user_import_service", import_fn),
        patch(
            "core.dependencies.auth_providers.get_jellyfin_user_auth_service", auth_fn
        ),
    ):
        await service.on_jellyfin_settings_changed()

    import_fn.cache_clear.assert_called_once()
    auth_fn.cache_clear.assert_called_once()


@pytest.mark.asyncio(loop_scope="function")
async def test_jellyfin_and_cover_changes_rebuild_every_target_cover_consumer():
    from unittest.mock import AsyncMock, MagicMock, patch

    service, _cache = await _build_service()
    target_names = (
        "get_target_coverart_repository",
        "get_target_consumer_composition",
        "get_target_compat_services",
        "get_target_search_service",
        "get_target_genre_cover_prewarm_service",
        "get_target_home_service",
        "get_target_home_charts_service",
        "get_target_wrapped_service",
        "get_target_discover_service",
        "get_target_discover_queue_manager",
    )
    target_providers = {name: MagicMock() for name in target_names}
    mbid = MagicMock()
    mbid.clear_jellyfin_mbid_index = AsyncMock()

    with (
        patch("core.dependencies.get_jellyfin_repository", MagicMock()),
        patch("core.dependencies.get_jellyfin_playback_service", MagicMock()),
        patch("core.dependencies.get_jellyfin_library_service", MagicMock()),
        patch("core.dependencies.get_home_service", MagicMock()),
        patch("core.dependencies.get_home_charts_service", MagicMock()),
        patch("core.dependencies.get_coverart_repository", MagicMock()),
        patch("core.dependencies.get_mbid_store", MagicMock(return_value=mbid)),
        patch("core.dependencies.auth_providers.get_user_import_service", MagicMock()),
        patch(
            "core.dependencies.auth_providers.get_jellyfin_user_auth_service",
            MagicMock(),
        ),
        patch.multiple("core.dependencies", **target_providers),
    ):
        await service.on_jellyfin_settings_changed()
        await service.on_coverart_settings_changed()

    for name, provider in target_providers.items():
        minimum = 2 if name != "get_target_home_service" else 1
        assert provider.cache_clear.call_count >= minimum, name


@pytest.mark.asyncio(loop_scope="function")
async def test_navidrome_settings_change_rebuilds_every_captured_repository():
    from unittest.mock import AsyncMock, MagicMock, patch

    service, _cache = await _build_service()
    mbid = MagicMock()
    mbid.clear_navidrome_mbid_indexes = AsyncMock()
    new_repo = MagicMock()
    new_repo.clear_cache = AsyncMock()
    providers = {
        name: MagicMock()
        for name in (
            "get_navidrome_library_service",
            "get_target_navidrome_library_service",
            "get_navidrome_folder_scope_service",
            "get_navidrome_playback_service",
            "get_library_service",
            "get_home_service",
            "get_home_charts_service",
        )
    }
    repository = MagicMock(return_value=new_repo)

    with (
        patch("core.dependencies.get_navidrome_repository", repository),
        patch("core.dependencies.get_mbid_store", MagicMock(return_value=mbid)),
        patch.multiple("core.dependencies", **providers),
    ):
        await service.on_navidrome_settings_changed(enabled=False)

    repository.cache_clear.assert_called_once()
    for provider in providers.values():
        provider.cache_clear.assert_called_once()
    new_repo.clear_cache.assert_awaited_once()


@pytest.mark.asyncio(loop_scope="function")
async def test_plex_settings_change_clears_user_import_service():
    """Phase 6: configuring Plex must rebuild the import service singleton too."""
    from unittest.mock import patch, MagicMock, AsyncMock

    service, _cache = await _build_service()
    mbid = MagicMock()
    mbid.clear_plex_mbid_indexes = AsyncMock()
    plex_repo = MagicMock()
    plex_repo.clear_cache = AsyncMock()
    import_fn = MagicMock()
    auth_fn = MagicMock()
    target_plex = MagicMock()

    with (
        patch(
            "core.dependencies.get_plex_repository", MagicMock(return_value=plex_repo)
        ),
        patch("core.dependencies.get_plex_library_service", MagicMock()),
        patch("core.dependencies.get_target_plex_library_service", target_plex),
        patch("core.dependencies.get_plex_playback_service", MagicMock()),
        patch("core.dependencies.get_home_service", MagicMock()),
        patch("core.dependencies.get_home_charts_service", MagicMock()),
        patch("core.dependencies.get_mbid_store", MagicMock(return_value=mbid)),
        patch("core.dependencies.auth_providers.get_user_import_service", import_fn),
        patch("core.dependencies.auth_providers.get_plex_user_auth_service", auth_fn),
    ):
        await service.on_plex_settings_changed(enabled=False)

    import_fn.cache_clear.assert_called_once()
    auth_fn.cache_clear.assert_called_once()
    target_plex.cache_clear.assert_called_once()


@pytest.mark.asyncio(loop_scope="function")
async def test_youtube_settings_change_clears_home_cache():
    """on_youtube_settings_changed should reset singleton AND clear home caches."""
    from unittest.mock import patch, MagicMock

    service, cache = await _build_service()

    home_keys = [
        f"{HOME_RESPONSE_PREFIX}page1",
        f"{DISCOVER_RESPONSE_PREFIX}rock",
    ]
    await _populate(cache, home_keys)

    mock_repo_fn = MagicMock()
    with patch("core.dependencies.get_youtube_repo", mock_repo_fn):
        await service.on_youtube_settings_changed()

    mock_repo_fn.cache_clear.assert_called_once()
    for key in home_keys:
        assert await cache.get(key) is None


@pytest.fixture
def mb_live_state():
    """Snapshot and restore the live MusicBrainz module state around a test."""
    from repositories.musicbrainz_base import (
        brainzmash_runtime_enabled,
        capture_mb_source_context,
        get_mb_source_id,
        set_mb_api_base,
        mb_rate_limiter,
    )

    source = capture_mb_source_context()
    source_id = get_mb_source_id()
    runtime = brainzmash_runtime_enabled()
    rate = mb_rate_limiter.rate
    capacity = mb_rate_limiter.capacity
    yield
    set_mb_api_base(
        source.source_url,
        source_mode=source.source_mode,
        source_id=source_id,
        generation=source.generation,
        brainzmash_binding_valid=runtime,
    )
    mb_rate_limiter.update_rate(rate)
    mb_rate_limiter.update_capacity(capacity)


def _mb_settings(api_url: str, rate_limit: float, concurrent_searches: int):
    from api.v1.schemas.settings import MusicBrainzConnectionSettings

    return MusicBrainzConnectionSettings(
        source_mode="mirror",
        api_url=api_url,
        rate_limit=rate_limit,
        concurrent_searches=concurrent_searches,
    )


def _seed_mb_live_state(settings) -> None:
    from repositories.musicbrainz_base import set_mb_api_base, mb_rate_limiter

    set_mb_api_base(
        settings.api_url,
        source_mode=settings.source_mode,
        source_id=settings.source_id,
        generation=settings.generation,
    )
    mb_rate_limiter.update_rate(settings.rate_limit)
    mb_rate_limiter.update_capacity(settings.concurrent_searches)


def _spy_mb_side_effects(monkeypatch):
    from unittest.mock import MagicMock

    from repositories.musicbrainz_base import mb_circuit_breaker, mb_deduplicator

    reset = MagicMock()
    clear = MagicMock()
    monkeypatch.setattr(mb_circuit_breaker, "reset", reset)
    monkeypatch.setattr(mb_deduplicator, "clear", clear)
    return reset, clear


@pytest.mark.asyncio(loop_scope="function")
async def test_musicbrainz_settings_unchanged_skips_reset_and_cache_clear(
    mb_live_state, monkeypatch
):
    """Saving identical MusicBrainz settings is a no-op: no breaker reset, no
    deduplicator clear, caches survive - endpoint behavior did not change."""
    service, cache = await _build_service()
    settings = _mb_settings("https://mb.mirror.example/ws/2", 2.0, 4)
    _seed_mb_live_state(settings)
    reset, clear = _spy_mb_side_effects(monkeypatch)

    mb_keys = [f"{p}noop" for p in musicbrainz_prefixes()]
    await _populate(cache, mb_keys)

    await service.on_musicbrainz_settings_changed(settings)

    reset.assert_not_called()
    clear.assert_not_called()
    for key in mb_keys:
        assert await cache.get(key) == "v"


@pytest.mark.asyncio(loop_scope="function")
async def test_musicbrainz_settings_endpoint_change_resets_and_clears(
    mb_live_state, monkeypatch
):
    """An endpoint change resets the breaker, clears the deduplicator, and
    clears the MusicBrainz cache prefixes."""
    disk_cache = SimpleNamespace(clear_musicbrainz=AsyncMock())
    service, cache = await _build_service(disk_cache)
    _seed_mb_live_state(_mb_settings("https://mb-a.example/ws/2", 2.0, 4))
    reset, clear = _spy_mb_side_effects(monkeypatch)
    from services.search_service import SearchService

    search_clear = MagicMock()
    monkeypatch.setattr(SearchService, "clear_cached_results", search_clear)
    mb_keys = [f"{p}endpoint" for p in musicbrainz_prefixes()]
    assert len(mb_keys) == 27
    # Source switches clear only MusicBrainz-backed discovery categories.
    assert ARTIST_DISCOVERY_TOP_SONGS_PREFIX in musicbrainz_prefixes()
    assert ARTIST_DISCOVERY_TOP_ALBUMS_PREFIX in musicbrainz_prefixes()
    assert ARTIST_DISCOVERY_PREFIX not in musicbrainz_prefixes()
    assert ALBUM_INFO_PREFIX in musicbrainz_prefixes()
    assert DISCOVER_QUEUE_ENRICH_PREFIX in musicbrainz_prefixes()
    album_info_key = f"{ALBUM_INFO_PREFIX}rg-endpoint"
    similar_key = f"{ARTIST_DISCOVERY_PREFIX}similar:artist:10:listenbrainz"
    await _populate(cache, mb_keys + [album_info_key, similar_key])

    await service.on_musicbrainz_settings_changed(
        _mb_settings("https://mb-b.example/ws/2", 2.0, 4)
    )
    assert await cache.get(album_info_key) is None
    assert await cache.get(similar_key) == "v"

    from repositories.musicbrainz_base import get_mb_api_base

    reset.assert_called_once()
    clear.assert_called_once()
    search_clear.assert_called_once()
    assert get_mb_api_base() == "https://mb-b.example/ws/2"
    for key in mb_keys:
        assert await cache.get(key) is None
    disk_cache.clear_musicbrainz.assert_awaited_once()


@pytest.mark.asyncio(loop_scope="function")
async def test_musicbrainz_source_change_clears_durable_discover_snapshots(
    tmp_path, mb_live_state, monkeypatch
):
    import threading

    from infrastructure.persistence.discovery_snapshot_store import (
        DiscoverySnapshotStore,
    )
    from repositories import musicbrainz_base as mb_base

    snapshot_store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    await snapshot_store.save("discover_response:u1", "u1", b"old-response", 123.0)
    await snapshot_store.save("discover_queue:u1", "u1", b"old-queue", 124.0)
    await snapshot_store.save("discover:u1", "u1", b"unrelated", 125.0)
    disk_cache = SimpleNamespace(clear_musicbrainz=AsyncMock())
    service, _cache = await _build_service(disk_cache)
    service._discovery_snapshot_store = snapshot_store

    old_settings = _mb_settings("https://mb-old.example/ws/2", 2.0, 4)
    new_settings = _mb_settings("https://mb-new.example/ws/2", 2.0, 4)
    _seed_mb_live_state(old_settings)
    old_context = mb_base.capture_mb_source_context()
    real_delete = snapshot_store.delete_source_dependent_snapshots
    delete_calls = 0

    async def fail_once() -> int:
        nonlocal delete_calls
        delete_calls += 1
        if delete_calls == 1:
            raise RuntimeError("synthetic snapshot clear failure")
        return await real_delete()

    monkeypatch.setattr(snapshot_store, "delete_source_dependent_snapshots", fail_once)

    with pytest.raises(RuntimeError, match="synthetic snapshot clear failure"):
        await service.on_musicbrainz_settings_changed(new_settings)
    assert mb_base.get_mb_api_base() == old_settings.api_url
    assert await snapshot_store.get("discover_response:u1") == b"old-response"
    assert await snapshot_store.get("discover_queue:u1") == b"old-queue"

    await service.on_musicbrainz_settings_changed(new_settings)

    assert delete_calls == 2
    assert disk_cache.clear_musicbrainz.await_count == 2
    assert mb_base.get_mb_api_base() == new_settings.api_url
    assert await snapshot_store.get("discover_response:u1") is None
    assert await snapshot_store.get("discover_queue:u1") is None
    assert await snapshot_store.get("discover:u1") == b"unrelated"

    republished = await mb_base.mb_publish_if_current(
        old_context,
        lambda: snapshot_store.save(
            "discover_response:u1", "u1", b"stale-republish", 126.0
        ),
    )
    assert republished is False
    assert await snapshot_store.get("discover_response:u1") is None


@pytest.mark.asyncio(loop_scope="function")
async def test_musicbrainz_settings_rate_only_change_resets_without_cache_clear(
    mb_live_state, monkeypatch
):
    """A control-only change re-arms transport behavior but retains provider
    results because the endpoint identity did not change."""
    service, cache = await _build_service()
    snapshot_store = SimpleNamespace(delete_source_dependent_snapshots=AsyncMock())
    service._discovery_snapshot_store = snapshot_store
    _seed_mb_live_state(_mb_settings("https://mb-a.example/ws/2", 2.0, 4))
    reset, clear = _spy_mb_side_effects(monkeypatch)

    mb_keys = [f"{p}rate" for p in musicbrainz_prefixes()]
    await _populate(cache, mb_keys)

    await service.on_musicbrainz_settings_changed(
        _mb_settings("https://mb-a.example/ws/2", 3.0, 4)
    )

    from repositories.musicbrainz_base import mb_rate_limiter

    reset.assert_called_once()

    clear.assert_called_once()
    snapshot_store.delete_source_dependent_snapshots.assert_not_awaited()
    assert mb_rate_limiter.rate == 3.0
    for key in mb_keys:
        assert await cache.get(key) == "v"


@pytest.mark.asyncio(loop_scope="function")
async def test_musicbrainz_settings_concurrency_only_skips_durable_snapshot_clear(
    mb_live_state, monkeypatch
):
    service, _cache = await _build_service()
    snapshot_store = SimpleNamespace(delete_source_dependent_snapshots=AsyncMock())
    service._discovery_snapshot_store = snapshot_store
    _seed_mb_live_state(_mb_settings("https://mb-a.example/ws/2", 2.0, 4))
    _spy_mb_side_effects(monkeypatch)

    await service.on_musicbrainz_settings_changed(
        _mb_settings("https://mb-a.example/ws/2", 2.0, 8)
    )

    snapshot_store.delete_source_dependent_snapshots.assert_not_awaited()


@pytest.mark.asyncio(loop_scope="function")
async def test_musicbrainz_settings_clear_failure_leaves_live_state_for_retry(
    mb_live_state, monkeypatch
):
    service, cache = await _build_service()
    from repositories.musicbrainz_base import (
        get_mb_api_base,
        mb_circuit_breaker,
        mb_rate_limiter,
        mb_rate_limiter_bypassed,
        set_mb_rate_limiter_bypass,
    )

    old_settings = _mb_settings("https://mb-old.example/ws/2", 2.0, 4)
    new_settings = _mb_settings("https://mb-new.example/ws/2", 3.0, 8)
    _seed_mb_live_state(old_settings)
    set_mb_rate_limiter_bypass(False)
    mb_circuit_breaker.reset()
    before_breaker = mb_circuit_breaker.get_state()
    calls = 0
    original_clear = cache.clear_prefix

    async def fail_once(prefix: str) -> int:
        nonlocal calls
        calls += 1
        if calls == 1:
            raise RuntimeError("synthetic cache clear failure")
        return await original_clear(prefix)

    monkeypatch.setattr(cache, "clear_prefix", fail_once)

    with pytest.raises(RuntimeError, match="synthetic cache clear failure"):
        await service.on_musicbrainz_settings_changed(new_settings)

    assert get_mb_api_base() == old_settings.api_url
    assert mb_rate_limiter.rate == old_settings.rate_limit
    assert mb_rate_limiter.capacity == old_settings.concurrent_searches
    assert mb_rate_limiter_bypassed() is False
    assert mb_circuit_breaker.get_state() == before_breaker

    await service.on_musicbrainz_settings_changed(new_settings)

    assert calls == 28
    assert get_mb_api_base() == new_settings.api_url
    assert mb_rate_limiter.rate == new_settings.rate_limit
    assert mb_rate_limiter.capacity == new_settings.concurrent_searches
