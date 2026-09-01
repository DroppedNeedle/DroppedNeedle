"""Tests for MusicBrainzAlbumMixin.get_release_group - the protocol method that maps
a raw MusicBrainz release-group dict to an AlbumInfo (year/title/artist backfill)."""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from models.album import AlbumInfo
from repositories.musicbrainz_album import MusicBrainzAlbumMixin


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self) -> None:
        self._cache = AsyncMock()  # unused: get_release_group_by_id is stubbed per-test


_RG = {
    "id": "rg-1",
    "title": "OK Computer",
    "first-release-date": "1997-05-21",
    "primary-type": "Album",
    "artist-credit": [
        {"name": "Radiohead", "artist": {"id": "art-1", "name": "Radiohead"}}
    ],
}


@pytest.mark.asyncio
async def test_get_release_group_maps_dict_to_album_info():
    repo = _Repo()
    repo.get_release_group_by_id = AsyncMock(return_value=_RG)

    info = await repo.get_release_group("rg-1")

    assert isinstance(info, AlbumInfo)
    assert info.year == 1997
    assert info.title == "OK Computer"
    assert info.artist_name == "Radiohead"
    assert info.artist_id == "art-1"  # the MBID radio_service reads as artist_mbid
    assert info.musicbrainz_id == "rg-1"


@pytest.mark.asyncio
async def test_get_release_group_returns_none_when_missing():
    repo = _Repo()
    repo.get_release_group_by_id = AsyncMock(return_value=None)
    assert await repo.get_release_group("rg-x") is None


@pytest.mark.asyncio
async def test_get_release_group_tolerates_sparse_dict():
    """No date and no artist-credit must still map without raising (year falls to None)."""
    repo = _Repo()
    repo.get_release_group_by_id = AsyncMock(
        return_value={"id": "rg-2", "title": "Untitled"}
    )

    info = await repo.get_release_group("rg-2")

    assert info.year is None
    assert info.artist_name == "Unknown Artist"
    assert info.artist_id == ""


@pytest.mark.asyncio
async def test_fetch_rg_negative_caches_404_but_not_transient(monkeypatch):
    """A definitive 404 (mb_api_get -> {}) is negative-cached briefly so a merged/garbage
    mbid isn't re-fetched every discover build; a transient error stays uncached to retry."""
    import repositories.musicbrainz_album as mod

    repo = _Repo()
    repo._cache = AsyncMock()

    monkeypatch.setattr(mod, "mb_api_get", AsyncMock(return_value={}))
    assert (
        await repo._fetch_release_group_by_id("rg-404", ["artist-credits"], "ck-404")
        is None
    )
    repo._cache.set.assert_awaited_once_with("ck-404", {}, ttl_seconds=600)

    repo._cache.set.reset_mock()
    monkeypatch.setattr(mod, "mb_api_get", AsyncMock(side_effect=RuntimeError("503")))
    assert (
        await repo._fetch_release_group_by_id("rg-503", ["artist-credits"], "ck-503")
        is None
    )
    repo._cache.set.assert_not_called()


@pytest.mark.asyncio
async def test_release_to_rg_resolution_threads_priority(monkeypatch):
    """#78: the album-page release->RG fallback passes the caller's priority, while
    background callers keep the BACKGROUND_SYNC default (honest-priority house rule)."""
    from types import SimpleNamespace

    import repositories.musicbrainz_album as mod
    from infrastructure.queue.priority_queue import RequestPriority

    repo = _Repo()
    repo._cache = AsyncMock()
    repo._cache.get = AsyncMock(return_value=None)

    api = AsyncMock(
        return_value=SimpleNamespace(release_group={"id": "rg-9"}, media=[])
    )
    monkeypatch.setattr(mod, "mb_api_get", api)

    resolved = await repo.get_release_group_id_from_release(
        "rel-1", priority=RequestPriority.USER_INITIATED
    )
    assert resolved == "rg-9"
    assert api.await_args.kwargs["priority"] is RequestPriority.USER_INITIATED

    api.reset_mock()
    assert await repo.get_release_group_id_from_release("rel-2") == "rg-9"
    assert api.await_args.kwargs["priority"] is RequestPriority.BACKGROUND_SYNC


@pytest.mark.asyncio
async def test_release_group_ids_batch_fans_out_all_pending_ids():
    repo = _Repo()
    repo._cache.get = AsyncMock(return_value=None)
    resolver = AsyncMock(side_effect=["rg-a", None])
    repo.get_release_group_id_from_release = resolver

    resolved = await repo.get_release_group_ids_batch(["rel-a", "rel-b"])

    assert resolved == {"rel-a": "rg-a", "rel-b": None}
    assert [call.args[0] for call in resolver.await_args_list] == ["rel-a", "rel-b"]
    assert all(
        call.kwargs["source_context"] is not None for call in resolver.await_args_list
    )


@pytest.mark.asyncio
async def test_release_group_ids_batch_rejects_source_switch_during_wire():
    import repositories.musicbrainz_base as mb_base
    from core.exceptions import ConfigurationError

    repo = _Repo()
    repo._cache.get = AsyncMock(return_value=None)
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="old-batch",
        generation=old_generation,
    )

    async def resolve(_release_id, *, source_context=None):
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="new-batch",
            generation=old_generation + 1,
        )
        return "rg-old"

    repo.get_release_group_id_from_release = resolve
    try:
        with pytest.raises(ConfigurationError, match="batch resolution"):
            await repo.get_release_group_ids_batch(["rel-a", "rel-b"])
    finally:
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )


class _RealDictCache:
    """Functioning in-memory cache so negative-cache writes are observable."""

    def __init__(self) -> None:
        self.store: dict = {}
        self.writes: list[tuple] = []

    async def get(self, key):
        from repositories.musicbrainz_base import namespace_mb_cache_key

        return self.store.get(namespace_mb_cache_key(key))

    async def set(self, key, value, ttl_seconds=None):
        from repositories.musicbrainz_base import namespace_mb_cache_key

        self.writes.append((key, value, ttl_seconds))
        self.store[namespace_mb_cache_key(key)] = value


def _suffix_repo(cache: _RealDictCache) -> MusicBrainzAlbumMixin:
    from types import SimpleNamespace

    repo = MusicBrainzAlbumMixin.__new__(MusicBrainzAlbumMixin)
    repo._cache = cache

    class _Prefs:
        def get_advanced_settings(self):
            return SimpleNamespace(cache_ttl_search=60)

    repo._preferences_service = _Prefs()
    return repo


@pytest.mark.asyncio
async def test_transient_release_to_rg_failure_is_not_negative_cached(
    monkeypatch,
) -> None:
    """F-MATCH-05: a transient release-to-group failure records degradation
    without writing the definitive empty sentinel; an immediate healthy retry
    reaches the provider and returns the real group."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.degradation import (
        clear_degradation_context,
        init_degradation_context,
    )

    cache = _RealDictCache()
    repo = _suffix_repo(cache)
    calls = {"n": 0}

    async def flaky_get(
        url, params=None, priority=None, decode_type=None, source_context=None
    ):
        calls["n"] += 1
        if calls["n"] == 1:
            raise TimeoutError("transient provider failure")
        return SimpleNamespace(release_group={"id": "rg-real"}, media=[])

    monkeypatch.setattr(mb_album, "mb_api_get", flaky_get)

    ctx = init_degradation_context()
    try:
        first = await repo.get_release_group_id_from_release("rel-1")
        second = await repo.get_release_group_id_from_release("rel-1")
    finally:
        clear_degradation_context()

    assert first is None
    assert second == "rg-real"
    assert calls["n"] == 2  # the transient failure was not cached
    assert not any(value == "" for _, value, _ in cache.writes)
    assert "musicbrainz" in ctx.deterministic_sources() or ctx.has_degradation()


@pytest.mark.asyncio
async def test_transient_recording_to_rg_failure_is_not_negative_cached(
    monkeypatch,
) -> None:
    """Same policy for the recording-to-group helper."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.degradation import clear_degradation_context

    cache = _RealDictCache()
    repo = _suffix_repo(cache)
    calls = {"n": 0}

    async def flaky_get(url, params=None, priority=None, decode_type=None, **kwargs):
        calls["n"] += 1
        if calls["n"] == 1:
            raise TimeoutError("transient provider failure")
        return SimpleNamespace(
            releases=[
                {
                    "release-group": {"id": "rg-from-recording"},
                    "status": "Official",
                    "date": "1997-05-21",
                }
            ]
        )

    monkeypatch.setattr(mb_album, "mb_api_get", flaky_get)
    try:
        first = await repo.resolve_recording_to_release_group("rec-1")
        second = await repo.resolve_recording_to_release_group("rec-1")
    finally:
        clear_degradation_context()

    assert first is None
    assert second == "rg-from-recording"
    assert calls["n"] == 2
    assert not any(value == "" for _, value, _ in cache.writes)


@pytest.mark.asyncio
async def test_provider_confirmed_no_group_stays_a_cached_negative(
    monkeypatch,
) -> None:
    """A decoded response proving 'no release group' is the legitimate long
    negative case: cached for 86400 s and served without another call."""
    import repositories.musicbrainz_album as mb_album

    cache = _RealDictCache()
    repo = _suffix_repo(cache)
    calls = {"n": 0}

    async def no_group_get(url, params=None, priority=None, decode_type=None, **kwargs):
        calls["n"] += 1
        # A decoded response with no "release-group" key models as {}.
        return SimpleNamespace(release_group={}, media=[])

    monkeypatch.setattr(mb_album, "mb_api_get", no_group_get)

    first = await repo.get_release_group_id_from_release("rel-none")
    second = await repo.get_release_group_id_from_release("rel-none")

    assert first is None and second is None
    assert calls["n"] == 1
    assert cache.writes == [("mb:release_to_rg:rel-none", "", 86400)]


@pytest.mark.asyncio
async def test_positive_release_to_rg_result_keeps_existing_ttl_and_value() -> None:
    from infrastructure.cache.cache_keys import MB_RELEASE_TO_RG_PREFIX
    from repositories.musicbrainz_base import (
        capture_mb_source_context,
        namespace_mb_cache_key,
    )
    import repositories.musicbrainz_album as mb_album

    async def must_not_be_called(url, params=None, priority=None, decode_type=None):
        raise AssertionError("provider boundary reached on a cached positive")

    cache = _RealDictCache()
    source_context = capture_mb_source_context()
    raw_key = f"{MB_RELEASE_TO_RG_PREFIX}rel-pos"
    cache.store[namespace_mb_cache_key(raw_key, source_context)] = "rg-positive"
    repo = _suffix_repo(cache)
    monkeypatch_target = mb_album
    saved = monkeypatch_target.mb_api_get
    try:
        monkeypatch_target.mb_api_get = must_not_be_called
        value = await repo.get_release_group_id_from_release("rel-pos")
    finally:
        monkeypatch_target.mb_api_get = saved
    assert value == "rg-positive"
    # Served entirely from cache: no provider call, nothing rewritten.
    assert cache.writes == []
