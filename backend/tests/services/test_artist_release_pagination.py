"""Service-level tests for filter-aware artist release pagination."""

import asyncio
import os
import tempfile
from types import SimpleNamespace
from typing import Any

os.environ.setdefault("ROOT_APP_DIR", tempfile.mkdtemp())

import pytest
from unittest.mock import AsyncMock, MagicMock

from core.exceptions import ClientDisconnectedError
from infrastructure.cache.cache_keys import mb_artist_release_groups_key
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_artist import MusicBrainzArtistMixin
from repositories.musicbrainz_base import capture_mb_source_context
from services.artist_service import ArtistService


ARTIST_MBID = "f4a31f0a-51dd-4fa7-986d-3095c40c5ed9"


def _make_release_group(
    rg_id: str, title: str, primary_type: str, date: str = "2020-01-01"
) -> dict:
    return {
        "id": rg_id,
        "title": title,
        "primary-type": primary_type,
        "secondary-types": [],
        "first-release-date": date,
    }


def _make_prefs(
    primary_types: list[str] | None = None, secondary_types: list[str] | None = None
) -> MagicMock:
    p = MagicMock()
    p.get_preferences.return_value = MagicMock(
        primary_types=primary_types
        if primary_types is not None
        else ["Album", "Single", "EP"],
        secondary_types=secondary_types
        if secondary_types is not None
        else ["Studio", "Live", "Compilation"],
    )
    p.get_advanced_settings.return_value = MagicMock(
        cache_ttl_artist_library=21600,
        cache_ttl_artist_non_library=3600,
    )
    return p


def _make_dict_cache() -> tuple[AsyncMock, dict[str, Any]]:
    """AsyncMock cache backed by a real dict, for multi-request tests."""
    store: dict[str, Any] = {}
    cache = AsyncMock()
    cache.get = AsyncMock(side_effect=store.get)
    cache.set = AsyncMock(
        side_effect=lambda key, value, ttl_seconds: store.__setitem__(key, value)
    )
    return cache, store


def _make_service(
    *,
    mb_release_pages: list[tuple[list[dict], int]] | None = None,
    prefs: MagicMock | None = None,
    memory_cache: AsyncMock | None = None,
) -> ArtistService:
    mb_repo = AsyncMock()
    if mb_release_pages is not None:
        mb_repo.get_artist_release_groups = AsyncMock(side_effect=mb_release_pages)
    else:
        mb_repo.get_artist_release_groups = AsyncMock(return_value=([], 0))

    async def get_artist_release_groups_with_context(*args, **kwargs):
        kwargs.pop("preserve_fetch_width", None)
        groups, total = await mb_repo.get_artist_release_groups(*args, **kwargs)
        return groups, total, capture_mb_source_context()

    mb_repo.get_artist_release_groups_with_context = AsyncMock(
        side_effect=get_artist_release_groups_with_context
    )
    library_repo = MagicMock()
    library_repo.is_configured.return_value = False
    library_repo.get_library_mbids = AsyncMock(return_value=set())
    library_repo.get_requested_mbids = AsyncMock(return_value=set())
    library_repo.get_artist_mbids = AsyncMock(return_value=set())

    wikidata_repo = AsyncMock()

    if memory_cache is None:
        memory_cache = AsyncMock()
        memory_cache.get = AsyncMock(return_value=None)
        memory_cache.set = AsyncMock()

    disk_cache = AsyncMock()
    disk_cache.get_artist = AsyncMock(return_value=None)
    disk_cache.set_artist = AsyncMock()

    service = ArtistService(
        mb_repo=mb_repo,
        library_repo=library_repo,
        wikidata_repo=wikidata_repo,
        preferences_service=prefs or _make_prefs(),
        memory_cache=memory_cache,
        disk_cache=disk_cache,
    )
    service.test_mb_repo = mb_repo
    return service


def _collected_list(collected):
    return list(collected.values())


async def _cancel_artist_warm_tasks() -> None:
    from core.task_registry import TaskRegistry

    registry = TaskRegistry.get_instance()
    names = [
        name
        for name in registry.get_all()
        if name.startswith(f"mb-rg-warm-{ARTIST_MBID.casefold()}:")
    ]
    for name in names:
        await registry.cancel(name)


@pytest.mark.asyncio
async def test_warm_seed_reuses_page_zero_and_clears_after_cancellation():
    await _cancel_artist_warm_tasks()
    page = [
        _make_release_group(f"rg-{i}", f"Album {i}", "Album")
        for i in range(100)
    ]
    cache, store = _make_dict_cache()
    svc = _make_service(memory_cache=cache)
    started = asyncio.Event()
    release = asyncio.Event()
    calls: list[int] = []

    async def fetch(_artist, offset, _limit, **_kwargs):
        calls.append(offset)
        if offset == 0:
            return page, 200
        started.set()
        await release.wait()
        return page, 200

    svc.test_mb_repo.get_artist_release_groups = AsyncMock(side_effect=fetch)
    try:
        first = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert first.warming is True
        await started.wait()

        second = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert second.warming is True
        assert calls == [0, 100]

        await _cancel_artist_warm_tasks()
        await asyncio.sleep(0)
        assert not svc._release_group_warm_seeds
        assert mb_artist_release_groups_key(ARTIST_MBID) not in store
    finally:
        release.set()
        await _cancel_artist_warm_tasks()


@pytest.mark.asyncio
async def test_warm_seed_clears_after_failure_and_retries_page_zero():
    await _cancel_artist_warm_tasks()
    page = [
        _make_release_group(f"rg-{i}", f"Album {i}", "Album")
        for i in range(100)
    ]
    cache, _store = _make_dict_cache()
    svc = _make_service(memory_cache=cache)
    calls: list[int] = []

    async def fail_warm(_artist, offset, _limit, **_kwargs):
        calls.append(offset)
        if offset == 0:
            return page, 200
        raise RuntimeError("warm failed")

    svc.test_mb_repo.get_artist_release_groups = AsyncMock(side_effect=fail_warm)
    try:
        first = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert first.warming is True
        await asyncio.sleep(0)
        await asyncio.sleep(0)
        assert not svc._release_group_warm_seeds

        await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert calls.count(0) == 2
    finally:
        await _cancel_artist_warm_tasks()


@pytest.mark.asyncio
async def test_warm_seed_source_change_rejects_late_write(monkeypatch):
    await _cancel_artist_warm_tasks()
    from repositories import musicbrainz_base as mb_base

    page = [
        _make_release_group(f"rg-{i}", f"Album {i}", "Album")
        for i in range(100)
    ]
    cache, store = _make_dict_cache()
    svc = _make_service(memory_cache=cache)
    started = asyncio.Event()
    release = asyncio.Event()

    async def fetch(_artist, offset, _limit, **_kwargs):
        if offset == 0:
            return page, 200
        started.set()
        await release.wait()
        return page, 200

    svc.test_mb_repo.get_artist_release_groups = AsyncMock(side_effect=fetch)
    old_generation = mb_base.get_mb_source_generation()
    try:
        first = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert first.warming is True
        await started.wait()

        monkeypatch.setattr(mb_base, "_mb_api_base", "https://new.example/ws/2")
        monkeypatch.setattr(mb_base, "_mb_source_generation", old_generation + 1)
        release.set()
        await asyncio.sleep(0)
        await asyncio.sleep(0)

        assert not svc._release_group_warm_seeds
        assert mb_artist_release_groups_key(ARTIST_MBID) not in store
    finally:
        release.set()
        await _cancel_artist_warm_tasks()



@pytest.mark.asyncio
async def test_delayed_release_page_drops_old_source_groups(monkeypatch):
    await _cancel_artist_warm_tasks()
    from repositories import musicbrainz_base as mb_base

    cache, store = _make_dict_cache()
    svc = _make_service(memory_cache=cache)
    started = asyncio.Event()
    release = asyncio.Event()
    old_group = _make_release_group("rg-old", "Old Source Album", "Album")

    async def fetch(_artist, _offset, _limit, **_kwargs):
        started.set()
        await release.wait()
        return [old_group], 1

    svc.test_mb_repo.get_artist_release_groups = AsyncMock(side_effect=fetch)
    old_generation = mb_base.get_mb_source_generation()
    try:
        task = asyncio.create_task(
            svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        )
        await started.wait()
        monkeypatch.setattr(mb_base, "_mb_api_base", "https://new.example/ws/2")
        monkeypatch.setattr(mb_base, "_mb_source_generation", old_generation + 1)
        release.set()
        result = await task

        assert result.albums == []
        assert result.singles == []
        assert result.eps == []
        assert "Old Source Album" not in str(result)
        assert mb_artist_release_groups_key(ARTIST_MBID) not in store
        assert not svc._release_group_warm_seeds
    finally:
        release.set()
        await _cancel_artist_warm_tasks()


@pytest.mark.asyncio
async def test_full_artist_profile_seeds_warm_from_fetched_width(monkeypatch):
    await _cancel_artist_warm_tasks()
    import repositories.musicbrainz_artist as artist_module

    repository = MusicBrainzArtistMixin.__new__(MusicBrainzArtistMixin)
    repository._cache = InMemoryCache(max_entries=100)
    repository._preferences_service = _make_prefs()
    repository._warm_release_group_cache = AsyncMock()
    library_repo = MagicMock()
    library_repo.get_artist_mbids = AsyncMock(return_value=set())
    library_repo.get_library_mbids = AsyncMock(return_value=set())
    library_repo.get_requested_mbids = AsyncMock(return_value=set())
    disk_cache = AsyncMock()
    disk_cache.get_artist = AsyncMock(return_value=None)
    disk_cache.set_artist = AsyncMock()
    service = ArtistService(
        mb_repo=repository,
        library_repo=library_repo,
        wikidata_repo=AsyncMock(),
        preferences_service=_make_prefs(),
        memory_cache=repository._cache,
        disk_cache=disk_cache,
    )
    source_context = capture_mb_source_context()
    initial_groups = [
        _make_release_group(f"rg-{index}", f"Album {index}", "Album")
        for index in range(100)
    ]
    continuation_groups = [
        _make_release_group(f"rg-{index}", f"Album {index}", "Album")
        for index in range(100, 200)
    ]
    offsets: list[int] = []
    continuation_done = asyncio.Event()

    async def provider(path, params=None, **_kwargs):
        if path == "/artist/artist-id":
            return {
                "id": "artist-id",
                "name": "Test Artist",
                "release-group-count": 200,
            }
        offsets.append(int((params or {}).get("offset", 0)))
        if offsets[-1] == 100:
            continuation_done.set()
            return SimpleNamespace(
                release_groups=continuation_groups,
                release_group_count=200,
            )
        return SimpleNamespace(
            release_groups=initial_groups,
            release_group_count=200,
        )

    monkeypatch.setattr(artist_module, "mb_api_get", provider)
    monkeypatch.setattr(
        artist_module,
        "get_mb_response_context",
        lambda: source_context,
    )

    result, _library, _albums, _requested = await service._fetch_artist_data(
        "artist-id",
        include_releases=True,
        source_context=source_context,
    )
    await continuation_done.wait()
    await _cancel_artist_warm_tasks()

    assert len(result["release-group-list"]) == 50
    assert offsets == [0, 100]


class TestFilterAwarePagination:
    @pytest.mark.asyncio
    async def test_single_page_fits_filter(self):
        rg1 = _make_release_group("rg-1", "Album A", "Album")
        rg2 = _make_release_group("rg-2", "Single B", "Single")
        svc = _make_service(mb_release_pages=[([rg1, rg2], 2)])

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        assert len(result.albums) == 1
        assert result.albums[0].title == "Album A"
        assert len(result.singles) == 1
        assert result.singles[0].title == "Single B"
        assert result.has_more is False
        assert result.next_offset is None
        assert result.returned_count == 2
        assert result.source_total_count == 2

    @pytest.mark.asyncio
    async def test_sparse_filter_scans_multiple_batches(self):
        batch1 = [
            _make_release_group(f"rg-{i}", f"Broadcast {i}", "Broadcast")
            for i in range(5)
        ]
        _rg_album = _make_release_group("rg-album", "Real Album", "Album")
        batch2 = [_rg_album]
        cache, store = _make_dict_cache()
        svc = _make_service(
            mb_release_pages=[
                (batch1, 6),
                (batch2, 6),
            ],
            memory_cache=cache,
        )

        # ST4/A3: page 1 is all-Broadcast -> incomplete slice, warming=true,
        # null total. The background walker finishes batch 2; the follow-up
        # read then serves the real album with exact totals.
        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert result.albums == []
        assert result.warming is True
        assert result.source_total_count is None

        collected = {f"rg-{i}": g for i, g in enumerate(batch1)}
        collected["rg-album"] = _rg_album
        await svc._warm_release_group_pages(
            ARTIST_MBID,
            collected,
            total=6,
            raw_offset=6,
        )

        warmed = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert [a.title for a in warmed.albums] == ["Real Album"]
        assert warmed.warming is False
        assert warmed.source_total_count == 1
        assert warmed.returned_count == 1

    @pytest.mark.asyncio
    async def test_empty_result_set(self):
        svc = _make_service(mb_release_pages=[([], 0)])

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        assert result.returned_count == 0
        assert result.has_more is False
        assert result.next_offset is None
        # A3: a definitive "no release groups at all" answer is complete,
        # not warming.
        assert result.source_total_count == 0
        assert result.warming is False

    @pytest.mark.asyncio
    async def test_offset_reflects_client_param(self):
        rg = _make_release_group("rg-1", "Album 1", "Album")
        svc = _make_service(mb_release_pages=[([rg], 1)])

        result = await svc.get_artist_releases(ARTIST_MBID, offset=200, limit=50)

        assert result.offset == 200

    @pytest.mark.asyncio
    async def test_returned_count_across_categories(self):
        rgs = [
            _make_release_group("rg-a1", "Album 1", "Album"),
            _make_release_group("rg-a2", "Album 2", "Album"),
            _make_release_group("rg-s1", "Single 1", "Single"),
            _make_release_group("rg-e1", "EP 1", "EP"),
        ]
        svc = _make_service(mb_release_pages=[(rgs, 4)])

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        assert result.returned_count == 4
        assert len(result.albums) == 2
        assert len(result.singles) == 1
        assert len(result.eps) == 1

    @pytest.mark.asyncio
    async def test_limit_controls_returned_items(self):
        rgs = [
            _make_release_group("rg-a1", "Album 1", "Album"),
            _make_release_group("rg-a2", "Album 2", "Album"),
            _make_release_group("rg-a3", "Album 3", "Album"),
            _make_release_group("rg-s1", "Single 1", "Single"),
            _make_release_group("rg-e1", "EP 1", "EP"),
        ]
        svc = _make_service(mb_release_pages=[(rgs, 5)])

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=3)

        assert result.returned_count == 3
        assert len(result.albums) == 3
        assert len(result.singles) == 0
        assert len(result.eps) == 0
        assert result.has_more is True
        assert result.next_offset == 3

    @pytest.mark.asyncio
    async def test_exception_returns_empty_page(self):
        svc = _make_service()
        svc._library_repo.get_library_mbids = AsyncMock(
            side_effect=RuntimeError("boom")
        )

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        assert result.returned_count == 0
        assert result.has_more is False
        assert result.next_offset is None

    @pytest.mark.asyncio
    async def test_empty_filter_types_returns_immediately(self):
        svc = _make_service(
            prefs=_make_prefs(primary_types=[], secondary_types=[]),
        )

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        assert result.returned_count == 0
        assert result.has_more is False
        assert result.next_offset is None
        svc._mb_repo.get_artist_release_groups.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_all_types_filtered_out_except_one(self):
        batch = [
            _make_release_group(f"rg-b{i}", f"Broadcast {i}", "Broadcast")
            for i in range(5)
        ] + [_make_release_group("rg-album", "Found Album", "Album")]
        svc = _make_service(
            mb_release_pages=[
                (batch, 6),
            ]
        )

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        assert result.returned_count == 1
        assert result.albums[0].title == "Found Album"
        assert result.source_total_count == 1
        assert result.has_more is False

    @pytest.mark.asyncio
    async def test_global_sort_across_batches(self):
        batch1 = [_make_release_group("rg-a", "Old Album", "Album", "2010-01-01")]
        batch2 = [_make_release_group("rg-b", "New Album", "Album", "2020-01-01")]
        cache, store = _make_dict_cache()
        svc = _make_service(
            mb_release_pages=[
                (batch1, 2),
                (batch2, 2),
            ],
            memory_cache=cache,
        )

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert result.warming is True

        collected = {
            "rg-a": batch1[0],
            "rg-b": batch2[0],
        }
        await svc._warm_release_group_pages(
            ARTIST_MBID, collected, total=2, raw_offset=2
        )

        warmed = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert len(warmed.albums) == 2
        assert warmed.albums[0].title == "New Album"
        assert warmed.albums[1].title == "Old Album"

    @pytest.mark.asyncio
    async def test_next_offset_is_arithmetic(self):
        rgs = [
            _make_release_group(f"rg-{i}", f"Album {i}", "Album") for i in range(100)
        ]
        cache, _ = _make_dict_cache()
        svc = _make_service(mb_release_pages=[(rgs, 100)], memory_cache=cache)

        page1 = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=10)
        page2 = await svc.get_artist_releases(ARTIST_MBID, offset=10, limit=10)

        assert page1.has_more is True
        assert page1.returned_count == 10
        assert page1.next_offset == 10
        assert page2.returned_count == 10
        assert page2.next_offset == 20

    @pytest.mark.asyncio
    async def test_no_drops_across_sequential_pages(self):
        rgs = [_make_release_group(f"rg-{i}", f"Album {i}", "Album") for i in range(10)]
        cache, _ = _make_dict_cache()
        svc = _make_service(mb_release_pages=[(rgs, 10)], memory_cache=cache)

        page1 = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=3)
        page2 = await svc.get_artist_releases(ARTIST_MBID, offset=3, limit=3)
        page3 = await svc.get_artist_releases(ARTIST_MBID, offset=6, limit=3)
        page4 = await svc.get_artist_releases(ARTIST_MBID, offset=9, limit=3)

        pages = [page1, page2, page3, page4]
        assert [page.returned_count for page in pages] == [3, 3, 3, 1]
        assert [page.next_offset for page in pages] == [3, 6, 9, None]
        ids = [item.id for page in pages for item in page.albums]
        assert ids == [item["id"] for item in rgs]
        assert len(ids) == len(set(ids))

    @pytest.mark.asyncio
    async def test_disconnect_during_multi_page_fetch_raises(self):
        batch1 = [
            _make_release_group(f"rg-{i}", f"Album {i}", "Album") for i in range(5)
        ]
        batch2 = [
            _make_release_group(f"rg-{i + 5}", f"Album {i + 5}", "Album")
            for i in range(5)
        ]
        svc = _make_service(mb_release_pages=[(batch1, 200), (batch2, 200)])
        # A3 contract: disconnects are checked before the page-1 fetch; once
        # page 1 succeeds the request returns (warming) and never checks again.
        is_disconnected = AsyncMock(return_value=True)

        with pytest.raises(ClientDisconnectedError):
            await svc.get_artist_releases(
                ARTIST_MBID,
                offset=0,
                limit=50,
                is_disconnected=is_disconnected,
            )

        svc._cache.set.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_complete_fetch_cached(self):
        batch1 = [
            _make_release_group(f"rg-{i}", f"Album {i}", "Album") for i in range(100)
        ]
        batch2 = [
            _make_release_group(f"rg-{100 + i}", f"Album {100 + i}", "Album")
            for i in range(9)
        ]
        cache, store = _make_dict_cache()
        svc = _make_service(
            mb_release_pages=[(batch1, 109), (batch2, 109)],
            memory_cache=cache,
        )

        first = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        # A3: first view serves page 1 with warming=true / null total...
        assert first.warming is True
        assert first.source_total_count is None

        # ...the background walker completes the remaining pages...
        collected = {
            str(g["id"]).casefold(): g for page in (batch1, batch2) for g in page
        }
        # The spawned walker (registry name mb-rg-warm-*) is still pending;
        # cancel it so only our explicit completion write lands.

        await _cancel_artist_warm_tasks()

        # ...then complete the walk deterministically ourselves.
        await svc._warm_release_group_pages(
            ARTIST_MBID, collected, total=109, raw_offset=109
        )

        # ...and the second view is served from the shared cache with exact
        # totals, byte-for-byte the old full-walk contract.
        second = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert second.warming is False
        assert second.returned_count == 50
        assert second.source_total_count == 109
        assert store[mb_artist_release_groups_key(ARTIST_MBID)] == _collected_list(
            collected
        )

    @pytest.mark.asyncio
    async def test_partial_fetch_not_cached(self):
        batch1 = [
            _make_release_group(f"rg-{i}", f"Album {i}", "Album") for i in range(100)
        ]
        svc = _make_service(mb_release_pages=[(batch1, 200), ([], 200)])

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)

        # A3: partial slice served with warming=true / null total; the shared
        # key stays unwritten (outage-safety rule).
        assert result.warming is True
        assert result.source_total_count is None
        store_writes = [
            call.args[0]
            for call in svc._cache.set.await_args_list
            if call.args and call.args[0].startswith("mb:artist_rgs:")
        ]
        assert store_writes == []

    @pytest.mark.asyncio
    async def test_gid_sorted_pages_no_drop_regression(self):
        # MB's browse endpoint pages by one order but its JSON serializer
        # re-sorts each page by GID: the target RG (Negative Spaces) can land
        # anywhere in a page, and scan-position pagination dropped it.
        negative_spaces_id = "fe83cc29-01a9-4650-95ca-d3e135c07278"
        page1 = [
            _make_release_group(
                f"aaaaaaaa-0000-4000-8000-{i:012d}", f"Album {i}", "Album"
            )
            for i in range(99)
        ] + [
            _make_release_group(
                negative_spaces_id, "Negative Spaces", "Album", "2024-11-15"
            )
        ]
        page2 = [
            _make_release_group(
                f"bbbbbbbb-0000-4000-8000-{i:012d}", f"Album B{i}", "Album"
            )
            for i in range(9)
        ]
        cache, _ = _make_dict_cache()
        svc = _make_service(
            mb_release_pages=[(page1, 109), (page2, 109)],
            memory_cache=cache,
        )

        page_a = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        page_b = await svc.get_artist_releases(ARTIST_MBID, offset=50, limit=50)
        page_c = await svc.get_artist_releases(ARTIST_MBID, offset=100, limit=50)

        ids = [item.id for page in (page_a, page_b, page_c) for item in page.albums]
        assert len(ids) == 109
        assert len(ids) == len(set(ids))
        assert ids.count(negative_spaces_id) == 1
        assert page_c.has_more is False

    @pytest.mark.asyncio
    async def test_overlapping_pages_deduped(self):
        rgs = [_make_release_group(f"rg-{i}", f"Album {i}", "Album") for i in range(15)]
        page1 = rgs[:10]
        page2 = rgs[5:]  # overlaps page1 by 50%
        cache, store = _make_dict_cache()
        svc = _make_service(
            mb_release_pages=[(page1, 15), (page2, 15)], memory_cache=cache
        )

        result = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        assert result.warming is True

        collected = {}
        raw = 0
        for page in (page1, page2):
            for g in page:
                collected.setdefault(str(g["id"]).casefold(), g)
            raw += len(page)
        await svc._warm_release_group_pages(
            ARTIST_MBID, collected, total=15, raw_offset=raw
        )

        warmed = await svc.get_artist_releases(ARTIST_MBID, offset=0, limit=50)
        ids = [item.id for item in warmed.albums]
        assert len(ids) == 15
        assert len(ids) == len(set(ids))
