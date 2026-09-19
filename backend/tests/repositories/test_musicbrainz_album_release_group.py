"""Tests for MusicBrainzAlbumMixin.get_release_group - the protocol method that maps
a raw MusicBrainz release-group dict to an AlbumInfo (year/title/artist backfill)."""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from models.album import AlbumInfo
from repositories.musicbrainz_album import MusicBrainzAlbumMixin
from infrastructure.cache.memory_cache import InMemoryCache


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self) -> None:
        self._cache = InMemoryCache()


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
    repo._cache = InMemoryCache()

    api = AsyncMock(return_value={})
    monkeypatch.setattr(mod, "mb_api_get", api)
    assert (
        await repo.get_release_group_by_id(
            "40440440-4404-4404-8404-404404404404", ["artist-credits"]
        )
        is None
    )
    assert (
        await repo.get_release_group_by_id(
            "40440440-4404-4404-8404-404404404404", ["artist-credits"]
        )
        is None
    )
    assert api.await_count == 1

    monkeypatch.setattr(mod, "mb_api_get", AsyncMock(side_effect=RuntimeError("503")))
    assert (
        await repo.get_release_group_by_id(
            "50350350-3503-4503-8503-503503503503", ["artist-credits"]
        )
        is None
    )
    monkeypatch.setattr(
        mod,
        "mb_api_get",
        AsyncMock(
            return_value={"id": "50350350-3503-4503-8503-503503503503"}
        ),
    )
    assert await repo.get_release_group_by_id(
        "50350350-3503-4503-8503-503503503503", ["artist-credits"]
    ) == {"id": "50350350-3503-4503-8503-503503503503"}


@pytest.mark.asyncio
async def test_release_to_rg_resolution_threads_priority(monkeypatch):
    """#78: the album-page release->RG fallback passes the caller's priority, while
    background callers keep the BACKGROUND_SYNC default (honest-priority house rule)."""
    from types import SimpleNamespace

    import repositories.musicbrainz_album as mod
    from infrastructure.queue.priority_queue import RequestPriority

    repo = _Repo()
    repo._cache = InMemoryCache()

    api = AsyncMock(
        return_value=SimpleNamespace(release_group={"id": "rg-9"}, media=[])
    )
    monkeypatch.setattr(mod, "mb_api_get", api)

    resolved = await repo.get_release_group_id_from_release(
        "11111111-1111-4111-8111-111111111111",
        priority=RequestPriority.USER_INITIATED,
    )
    assert resolved == "rg-9"
    assert api.await_args.kwargs["priority"] is RequestPriority.USER_INITIATED

    api.reset_mock()
    assert (
        await repo.get_release_group_id_from_release(
            "22222222-2222-4222-8222-222222222222"
        )
        == "rg-9"
    )
    assert api.await_args.kwargs["priority"] is RequestPriority.BACKGROUND_SYNC


@pytest.mark.asyncio
async def test_release_group_ids_batch_fans_out_all_pending_ids():
    repo = _Repo()
    resolver = AsyncMock(side_effect=["rg-a", None])
    repo.get_release_group_id_from_release = resolver

    resolved = await repo.get_release_group_ids_batch(
        ["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"]
    )

    assert resolved == {
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa": "rg-a",
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb": None,
    }
    assert [call.args[0] for call in resolver.await_args_list] == [
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    ]
    assert all(
        call.kwargs["source_context"] is not None for call in resolver.await_args_list
    )


@pytest.mark.asyncio
async def test_release_group_ids_batch_rejects_source_switch_during_wire():
    import repositories.musicbrainz_base as mb_base
    from core.exceptions import ConfigurationError

    repo = _Repo()
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
            await repo.get_release_group_ids_batch(
                [
                    "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                ]
            )
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

    def capture_clear_token(self):
        return self, 0

    async def set_if_token(self, token, key, value, ttl_seconds=None, metadata=None):
        if token != self.capture_clear_token():
            return False
        await self.set(key, value, ttl_seconds=ttl_seconds)
        return True

    async def get(self, key):
        from repositories.musicbrainz_base import namespace_mb_cache_key

        return self.store.get(namespace_mb_cache_key(key))

    async def get_with_metadata(self, key):
        return await self.get(key), None

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
        first = await repo.get_release_group_id_from_release(
            "11111111-1111-4111-8111-111111111111"
        )
        second = await repo.get_release_group_id_from_release(
            "11111111-1111-4111-8111-111111111111"
        )
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
        first = await repo.resolve_recording_to_release_group(
            "c1c1c1c1-c1c1-4c1c-8c1c-c1c1c1c1c1c1"
        )
        second = await repo.resolve_recording_to_release_group(
            "c1c1c1c1-c1c1-4c1c-8c1c-c1c1c1c1c1c1"
        )
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

    first = await repo.get_release_group_id_from_release(
        "d0d0d0d0-d0d0-4d0d-8d0d-d0d0d0d0d0d0"
    )
    second = await repo.get_release_group_id_from_release(
        "d0d0d0d0-d0d0-4d0d-8d0d-d0d0d0d0d0d0"
    )

    assert first is None and second is None
    assert calls["n"] == 1
    assert cache.writes == [
        ("mb:release_to_rg:d0d0d0d0-d0d0-4d0d-8d0d-d0d0d0d0d0d0", "", 86400)
    ]


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
    raw_key = f"{MB_RELEASE_TO_RG_PREFIX}e0e0e0e0-e0e0-4e0e-8e0e-e0e0e0e0e0e0"
    cache.store[namespace_mb_cache_key(raw_key, source_context)] = "rg-positive"
    repo = _suffix_repo(cache)
    monkeypatch_target = mb_album
    saved = monkeypatch_target.mb_api_get
    try:
        monkeypatch_target.mb_api_get = must_not_be_called
        value = await repo.get_release_group_id_from_release(
            "e0e0e0e0-e0e0-4e0e-8e0e-e0e0e0e0e0e0"
        )
    finally:
        monkeypatch_target.mb_api_get = saved
    assert value == "rg-positive"
    # Served entirely from cache: no provider call, nothing rewritten.
    assert cache.writes == []


@pytest.mark.asyncio
async def test_mapping_fetch_does_not_request_recordings(monkeypatch):
    import msgspec
    import repositories.musicbrainz_album as mb_album
    from infrastructure.cache.memory_cache import InMemoryCache

    repo = _suffix_repo(InMemoryCache())

    async def provider(path, *, params, decode_type, **kwargs):
        assert path == "/release/ed1ed1ed-1ed1-4ed1-8ed1-ed1ed1ed1ed1"
        assert set(params["inc"].split("+")) == {"release-groups"}
        return msgspec.convert(
            {"release-group": {"id": "album-group"}}, type=decode_type
        )

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    assert (
        await repo.get_release_group_id_from_release(
            "ed1ed1ed-1ed1-4ed1-8ed1-ed1ed1ed1ed1"
        )
        == "album-group"
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("mapping_first", [False, True])
async def test_positions_load_exact_multidisc_tracklist_without_mapping_side_effect(
    monkeypatch, mapping_first
):
    import msgspec
    import repositories.musicbrainz_album as mb_album
    from infrastructure.cache.memory_cache import InMemoryCache

    repo = _suffix_repo(InMemoryCache())
    fetched_tracklists = []

    edition_a = "0a0a0a0a-0a0a-40a0-80a0-0a0a0a0a0a0a"
    edition_b = "0b0b0b0b-0b0b-40b0-80b0-0b0b0b0b0b0b"

    async def provider(path, *, params, decode_type=None, **kwargs):
        if params["inc"] == "release-groups":
            return msgspec.convert(
                {"release-group": {"id": "shared-group"}}, type=decode_type
            )
        assert params["inc"] == "recordings"
        fetched_tracklists.append(path)
        disc, position = (2, 7) if path == f"/release/{edition_a}" else (1, 3)
        return {
            "id": path.rsplit("/", 1)[1],
            "media": [
                {
                    "position": disc,
                    "tracks": [
                        {"position": position, "recording": {"id": "recording-one"}}
                    ],
                }
            ],
        }

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    if mapping_first:
        assert (
            await repo.get_release_group_id_from_release(edition_a) == "shared-group"
        )
    assert await repo.get_recording_position_on_release(
        edition_a, "recording-one"
    ) == (2, 7)
    assert await repo.get_recording_position_on_release(
        edition_b, "recording-one"
    ) == (1, 3)
    assert (
        await repo.get_recording_position_on_release(edition_a, "missing") is None
    )
    assert await repo.get_recording_position_on_release(
        edition_a, "recording-one"
    ) == (2, 7)
    assert fetched_tracklists == [f"/release/{edition_a}", f"/release/{edition_b}"]


_INVALID_MBIDS = ["0", "2047108-7", "5AbcDefGhIjKlMnOpQrStUv"]


async def _call_get_release_group_by_id(repo, mbid):
    return await repo.get_release_group_by_id(mbid)


async def _call_get_release_by_id(repo, mbid):
    return await repo.get_release_by_id(mbid)


async def _call_get_release_for_verification(repo, mbid):
    from infrastructure.queue.priority_queue import RequestPriority

    return await repo.get_release_for_verification(
        mbid, priority=RequestPriority.BACKGROUND_SYNC
    )


async def _call_get_release_group_id_from_release(repo, mbid):
    return await repo.get_release_group_id_from_release(mbid)


async def _call_fetch_release_group_id_from_release(repo, mbid):
    from repositories.musicbrainz_base import capture_mb_cache_token

    return await repo._fetch_release_group_id_from_release(
        mbid,
        f"mb:release_to_rg:{mbid}",
        cache_token=capture_mb_cache_token(repo._cache),
    )


async def _call_resolve_recording_to_release_group(repo, mbid):
    return await repo.resolve_recording_to_release_group(mbid)


async def _call_get_recording_by_id(repo, mbid):
    return await repo.get_recording_by_id(mbid)


async def _call_resolve_recording_mbid(repo, mbid):
    return await repo.resolve_recording_mbid(mbid)


async def _call_batch(repo, mbid):
    return await repo.get_release_group_ids_batch([mbid])


_GATE_CALLERS = {
    "get_release_group_by_id": _call_get_release_group_by_id,
    "get_release_by_id": _call_get_release_by_id,
    "get_release_for_verification": _call_get_release_for_verification,
    "get_release_group_id_from_release": _call_get_release_group_id_from_release,
    "_fetch_release_group_id_from_release": _call_fetch_release_group_id_from_release,
    "resolve_recording_to_release_group": _call_resolve_recording_to_release_group,
    "get_recording_by_id": _call_get_recording_by_id,
    "resolve_recording_mbid": _call_resolve_recording_mbid,
    "get_release_group_ids_batch": _call_batch,
}

_PRERESOLVE_KINDS = {
    "get_release_group_by_id": "release-group",
    "get_release_by_id": "release",
    "get_release_for_verification": "release",
    "get_release_group_id_from_release": "release",
    "_fetch_release_group_id_from_release": "release",
    "resolve_recording_to_release_group": "recording",
    "get_recording_by_id": "recording",
    "resolve_recording_mbid": "recording",
    "get_release_group_ids_batch": "release",
}

_WIRE_PATH_ENTITIES = {
    "get_release_group_by_id": "release-group",
    "get_release_by_id": "release",
    "get_release_for_verification": "release",
    "get_release_group_id_from_release": "release",
    "_fetch_release_group_id_from_release": "release",
    "resolve_recording_to_release_group": "recording",
    "get_recording_by_id": "recording",
    "resolve_recording_mbid": "recording",
    "get_release_group_ids_batch": "release",
}


def _uuid_pair(n: int) -> tuple[str, str]:
    return (
        f"{n:08x}-0000-4000-8000-000000000000",
        f"{n:08x}-1111-4000-8000-111111111111",
    )


def _wire_stub(name: str, wired_id: str):
    """Provider response shaped for each choke point's decoder path."""
    if name == "get_release_group_by_id":
        return {"id": wired_id, "title": "Target"}
    if name == "get_release_by_id":
        return {"id": wired_id, "media": []}
    if name == "get_recording_by_id":
        return {"id": wired_id, "title": "Target"}
    if name == "get_release_for_verification":
        # Mirrors MbContributionRelease defaults: _verified_release reads
        # every field below (musicbrainz_album.py:268-318).
        return SimpleNamespace(
            id=wired_id,
            release_group=SimpleNamespace(id="rg-target"),
            title="Target",
            date="",
            country="",
            status=None,
            packaging=None,
            barcode="",
            artist_credit=[],
            label_info=[],
            media=[],
        )
    if name in (
        "get_release_group_id_from_release",
        "_fetch_release_group_id_from_release",
        "get_release_group_ids_batch",
    ):
        return SimpleNamespace(release_group={"id": "rg-target"}, media=[])
    if name == "resolve_recording_to_release_group":
        return SimpleNamespace(
            releases=[
                {
                    "release-group": {"id": "rg-target", "title": "Target"},
                    "status": "Official",
                    "date": "2020-01-01",
                }
            ]
        )
    if name == "resolve_recording_mbid":
        return SimpleNamespace(id=wired_id)
    raise AssertionError(f"no stub for {name}")


def _assert_wired_result(name: str, result, lookup_id: str, wired_id: str) -> None:
    if name == "get_release_group_by_id":
        assert result == {"id": wired_id, "title": "Target"}
    elif name == "get_release_by_id":
        assert result == {"id": wired_id, "media": []}
    elif name == "get_recording_by_id":
        assert result == {"id": wired_id, "title": "Target"}
    elif name == "get_release_for_verification":
        assert result.release_mbid == wired_id
    elif name in (
        "get_release_group_id_from_release",
        "_fetch_release_group_id_from_release",
        "resolve_recording_to_release_group",
    ):
        assert result == "rg-target"
    elif name == "resolve_recording_mbid":
        assert result == wired_id
    elif name == "get_release_group_ids_batch":
        assert result == {lookup_id: "rg-target"}
    else:  # pragma: no cover - table typo guard
        raise AssertionError(f"no assertion for {name}")


async def _seed_redirect(repo, kind: str, old: str, new: str) -> None:
    from infrastructure.cache.cache_keys import mb_redirect_key
    from repositories.musicbrainz_base import (
        capture_mb_cache_token,
        capture_mb_source_context,
        mb_cache_set_if_current,
    )

    context = capture_mb_source_context()
    await mb_cache_set_if_current(
        repo._cache,
        mb_redirect_key(kind, old),
        new,
        ttl_seconds=86400,
        context=context,
        cache_token=capture_mb_cache_token(repo._cache),
    )


class _MappingStore:
    """Minimal canonical store stub: redirect mappings only."""

    def __init__(self, mapping: dict) -> None:
        self.mapping = mapping
        self.calls = 0
        self.keys: list[tuple] = []

    async def get_canonical_redirect(
        self, kind, from_mbids, *, source_context, trusted_identity_source_only=False
    ):
        self.calls += 1
        self.keys.append((kind, tuple(from_mbids)))
        return {
            m: self.mapping[(kind, m)] for m in from_mbids if (kind, m) in self.mapping
        }


@pytest.mark.asyncio
@pytest.mark.parametrize("bad_id", _INVALID_MBIDS)
@pytest.mark.parametrize("name", list(_GATE_CALLERS))
async def test_gated_lookup_rejects_invalid_mbid_without_wire(
    monkeypatch, name, bad_id
):
    """05-1: invalid MBIDs return absence, never reach the wire, and record
    degradation when a request scope exists."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.degradation import (
        clear_degradation_context,
        init_degradation_context,
    )

    repo = _Repo()
    provider = AsyncMock(side_effect=AssertionError("gated MBID reached the wire"))
    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    ctx = init_degradation_context()
    try:
        result = await _GATE_CALLERS[name](repo, bad_id)
    finally:
        clear_degradation_context()
    if name == "get_release_group_ids_batch":
        assert result == {bad_id: None}
    else:
        assert result is None
    assert provider.await_count == 0
    assert ctx.has_degradation()


@pytest.mark.asyncio
@pytest.mark.parametrize("name", list(_GATE_CALLERS))
async def test_gated_lookup_records_deterministic_failure(monkeypatch, name):
    """T7: an invalid-MBID gate records a deterministic failure so
    identification jobs classify garbage input as
    UNMAPPABLE_PROVIDER_PAYLOAD (F-IDENT-02), not a transient outage."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.degradation import (
        clear_degradation_context,
        init_degradation_context,
    )

    repo = _Repo()
    provider = AsyncMock(side_effect=AssertionError("gated MBID reached the wire"))
    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    ctx = init_degradation_context()
    try:
        await _GATE_CALLERS[name](repo, "0")
    finally:
        clear_degradation_context()
    assert provider.await_count == 0
    assert ctx.summary() == {"musicbrainz": "error"}
    assert ctx.deterministic_sources() == {"musicbrainz"}


@pytest.mark.asyncio
async def test_transient_fetch_failure_records_non_deterministic(monkeypatch):
    """T7: a transient provider failure records degradation WITHOUT the
    deterministic flag, so identification jobs keep deferring as outage."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.degradation import (
        clear_degradation_context,
        init_degradation_context,
    )

    repo = _Repo()
    valid = "12345678-1234-5678-9abc-123456789abc"
    monkeypatch.setattr(
        mb_album, "mb_api_get", AsyncMock(side_effect=TimeoutError("transient"))
    )
    ctx = init_degradation_context()
    try:
        assert await repo.get_release_group_by_id(valid) is None
    finally:
        clear_degradation_context()
    assert ctx.summary() == {"musicbrainz": "error"}
    assert ctx.deterministic_sources() == set()


@pytest.mark.asyncio
@pytest.mark.parametrize("name", list(_GATE_CALLERS))
async def test_gated_lookup_silent_without_degradation_scope(monkeypatch, name):
    """05-1: background jobs have no DegradationContext - the gate still
    returns absence without raising."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.degradation import clear_degradation_context

    clear_degradation_context()
    repo = _Repo()
    provider = AsyncMock(side_effect=AssertionError("gated MBID reached the wire"))
    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    try:
        result = await _GATE_CALLERS[name](repo, "0")
    finally:
        clear_degradation_context()
    if name == "get_release_group_ids_batch":
        assert result == {"0": None}
    else:
        assert result is None
    assert provider.await_count == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("name", list(_GATE_CALLERS))
async def test_gated_lookup_passes_valid_mbid_to_wire(monkeypatch, name):
    """05-1 positive control: a lowercase-valid MBID passes the gate and the
    provider response flows through each choke point's decoder path."""
    import repositories.musicbrainz_album as mb_album

    repo = _Repo()
    valid = "12345678-1234-5678-9abc-123456789abc"
    paths: list[str] = []

    async def provider(path, **kwargs):
        paths.append(path)
        return _wire_stub(name, valid)

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    result = await _GATE_CALLERS[name](repo, valid)
    assert paths == [f"/{_WIRE_PATH_ENTITIES[name]}/{valid}"]
    _assert_wired_result(name, result, valid, valid)


@pytest.mark.asyncio
@pytest.mark.parametrize("name", list(_GATE_CALLERS))
async def test_preresolve_memory_hit_wires_target_per_choke_point(monkeypatch, name):
    """03-8: a memory-seeded redirect mapping rewrites the wired MBID at every
    album choke point (batch inherits it via its delegate)."""
    import repositories.musicbrainz_album as mb_album

    repo = _Repo()
    old, new = _uuid_pair(1 + list(_GATE_CALLERS).index(name))
    await _seed_redirect(repo, _PRERESOLVE_KINDS[name], old, new)
    paths: list[str] = []

    async def provider(path, **kwargs):
        paths.append(path)
        return _wire_stub(name, new)

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    result = await _GATE_CALLERS[name](repo, old)
    assert paths == [f"/{_WIRE_PATH_ENTITIES[name]}/{new}"]
    _assert_wired_result(name, result, old, new)


@pytest.mark.asyncio
async def test_preresolve_durable_hit_backfills_memory(monkeypatch):
    """03-8: a durable-only mapping backfills memory; the second lookup is
    fully wire-free.

    The hop budget re-checks the terminal hop durably on every lookup, and
    durable misses are positives-only by design (never backfilled), so the
    total durable count is not pinned. Backfill evidence is per-key: OLD is
    read durably exactly once (both lookups terminate on the same target)."""
    import repositories.musicbrainz_album as mb_album

    repo = _Repo()
    old, new = _uuid_pair(21)
    store = _MappingStore({("release", old): new})
    repo._mb_canonical_store = store
    paths: list[str] = []

    async def provider(path, **kwargs):
        paths.append(path)
        return {"id": new, "media": []}

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    first = await repo.get_release_by_id(old)
    second = await repo.get_release_by_id(old)
    assert first == second == {"id": new, "media": []}
    assert paths == [f"/release/{new}"]
    assert store.keys.count(("release", (old,))) == 1


@pytest.mark.asyncio
async def test_preresolve_redirect_cycle_terminates(monkeypatch):
    """03-8: an A->B->A mapping cycle terminates via the hop guard instead of
    looping."""
    import repositories.musicbrainz_album as mb_album

    repo = _Repo()
    old, new = _uuid_pair(22)
    repo._mb_canonical_store = _MappingStore(
        {("recording", old): new, ("recording", new): old}
    )
    paths: list[str] = []

    async def provider(path, **kwargs):
        paths.append(path)
        return {"id": new, "title": "Target"}

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    result = await repo.get_recording_by_id(old)
    assert result == {"id": new, "title": "Target"}
    assert paths == [f"/recording/{new}"]


@pytest.mark.asyncio
async def test_preresolve_to_missing_target_wires_once_then_neg_cached(monkeypatch):
    """03-8 + 05-4: a mapping whose target 404s wires once; the terminal
    negative cache serves repeat lookups without further wires."""
    import repositories.musicbrainz_album as mb_album

    repo = _Repo()
    old, target = _uuid_pair(23)
    await _seed_redirect(repo, "release", old, target)
    provider = AsyncMock(return_value={})
    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    assert await repo.get_release_by_id(old) is None
    assert await repo.get_release_by_id(old) is None
    assert provider.await_count == 1
    assert provider.await_args.args[0] == f"/release/{target}"


@pytest.mark.asyncio
async def test_preresolve_garbage_durable_target_wires_original_mbid(monkeypatch):
    """Security: a non-UUID durable redirect target is treated as unknown -
    the lookup proceeds with the original MBID and no malformed path
    reaches the wire."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.validators import is_valid_mbid

    repo = _Repo()
    old, _ = _uuid_pair(24)
    repo._mb_canonical_store = _MappingStore(
        {("release", old): "not-a-uuid/../../evil"}
    )
    paths: list[str] = []

    async def provider(path, **kwargs):
        paths.append(path)
        return {"id": old, "media": []}

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    result = await repo.get_release_by_id(old)
    assert result == {"id": old, "media": []}
    assert paths == [f"/release/{old}"]
    for path in paths:
        assert is_valid_mbid(path.rsplit("/", 1)[-1])


@pytest.mark.asyncio
async def test_preresolve_garbage_memory_target_wires_original_mbid(monkeypatch):
    """Security: same unknown-target treatment for a garbage memory-seeded
    redirect - the lookup wires the original MBID, never the garbage."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.validators import is_valid_mbid

    repo = _Repo()
    old, _ = _uuid_pair(25)
    await _seed_redirect(repo, "release", old, "not-a-uuid/../../evil")
    paths: list[str] = []

    async def provider(path, **kwargs):
        paths.append(path)
        return {"id": old, "media": []}

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    result = await repo.get_release_by_id(old)
    assert result == {"id": old, "media": []}
    assert paths == [f"/release/{old}"]
    for path in paths:
        assert is_valid_mbid(path.rsplit("/", 1)[-1])


@pytest.mark.asyncio
async def test_release_detail_404_is_negative_cached_600s(monkeypatch) -> None:
    """05-4: the release-detail 404 gap now banks the RG-style 600 s miss
    entry - a repeat lookup wires nothing."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.cache.cache_keys import mb_release_key

    cache = _RealDictCache()
    repo = _suffix_repo(cache)
    valid = "12345678-1234-5678-9abc-123456789abc"
    provider = AsyncMock(return_value={})
    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    assert await repo.get_release_by_id(valid) is None
    assert await repo.get_release_by_id(valid) is None
    assert provider.await_count == 1
    assert cache.writes == [
        (mb_release_key(valid, ["recordings", "labels"]), {}, 600)
    ]


@pytest.mark.asyncio
async def test_recording_detail_404_is_negative_cached_600s(monkeypatch) -> None:
    """05-4: same gap closure for the recording-detail 404 path."""
    import repositories.musicbrainz_album as mb_album
    from infrastructure.cache.cache_keys import MB_RECORDING_PREFIX

    cache = _RealDictCache()
    repo = _suffix_repo(cache)
    valid = "12345678-1234-5678-9abc-123456789abc"
    provider = AsyncMock(return_value={})
    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    assert await repo.get_recording_by_id(valid) is None
    assert await repo.get_recording_by_id(valid) is None
    assert provider.await_count == 1
    assert cache.writes == [(f"{MB_RECORDING_PREFIX}{valid}:url-rels", {}, 600)]
