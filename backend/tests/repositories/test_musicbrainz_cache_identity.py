import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock
import pytest

import repositories.musicbrainz_album as mb_album
from infrastructure.cache.cache_keys import mb_release_group_key
from repositories.musicbrainz_album import MusicBrainzAlbumMixin


class _Cache:
    def __init__(self) -> None:
        self.values = {}
        self.writes = []

    async def get(self, key):
        from repositories.musicbrainz_base import namespace_mb_cache_key

        return self.values.get(namespace_mb_cache_key(key))

    async def set(self, key, value, ttl_seconds=None):
        from repositories.musicbrainz_base import namespace_mb_cache_key

        self.values[namespace_mb_cache_key(key)] = value
        self.writes.append((key, value, ttl_seconds))


class _Prefs:
    def get_advanced_settings(self):
        return SimpleNamespace(cache_ttl_search=60)


def _repo() -> MusicBrainzAlbumMixin:
    repo = MusicBrainzAlbumMixin.__new__(MusicBrainzAlbumMixin)
    repo._cache = _Cache()
    repo._preferences_service = _Prefs()
    return repo


def _payload():
    return SimpleNamespace(
        release_groups=[
            {
                "id": "rg-album",
                "title": "Album",
                "primary-type": "Album",
                "secondary-types": ["album"],
                "artist-credit": [],
            },
            {
                "id": "rg-single",
                "title": "Single",
                "primary-type": "Single",
                "secondary-types": ["single"],
                "artist-credit": [],
            },
        ]
    )


@pytest.mark.asyncio
async def test_tag_secondary_type_policy_is_part_of_cache_identity(monkeypatch):
    repo = _repo()
    calls = 0

    async def provider(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        return _payload()

    monkeypatch.setattr(mb_album, "mb_api_get", provider)

    albums = await repo.search_release_groups_by_tag(
        "measured-tag", included_secondary_types={"album"}
    )
    singles = await repo.search_release_groups_by_tag(
        "measured-tag", included_secondary_types={"single"}
    )

    assert [item.musicbrainz_id for item in albums] == ["rg-album"]
    assert [item.musicbrainz_id for item in singles] == ["rg-single"]
    assert calls == 2
    keys = [key for key, _value, _ttl in repo._cache.writes]
    assert len(set(keys)) == 2
    assert all("secondary=" in key for key in keys)


@pytest.mark.asyncio
async def test_concurrent_tag_callers_share_one_provider_request(monkeypatch):
    repo = _repo()
    started = asyncio.Event()
    release = asyncio.Event()
    calls = 0

    async def provider(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        started.set()
        await release.wait()
        return _payload()

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    first = asyncio.create_task(
        repo.search_release_groups_by_tag("dedupe-tag", included_secondary_types=None)
    )
    await started.wait()
    second = asyncio.create_task(
        repo.search_release_groups_by_tag("dedupe-tag", included_secondary_types=None)
    )
    await asyncio.sleep(0)
    release.set()

    result_one, result_two = await asyncio.gather(first, second)
    assert result_one == result_two
    assert calls == 1


@pytest.mark.asyncio
async def test_cached_release_group_miss_returns_none_without_wire(monkeypatch):
    repo = _repo()
    includes = ["artist-credits", "releases"]
    from repositories.musicbrainz_base import (
        capture_mb_source_context,
        namespace_mb_cache_key,
    )

    source_context = capture_mb_source_context()
    raw_key = mb_release_group_key("missing", includes)
    repo._cache.values[namespace_mb_cache_key(raw_key, source_context)] = {}
    provider = AsyncMock()
    monkeypatch.setattr(mb_album, "mb_api_get", provider)

    assert await repo.get_release_group_by_id("missing") is None
    assert provider.await_count == 0


@pytest.mark.asyncio
async def test_case_variants_share_detail_wire_and_release_to_rg_batch_identity(
    monkeypatch,
):
    repo = _repo()
    calls: list[str] = []
    started = asyncio.Event()
    release = asyncio.Event()

    async def provider(path, *_args, **_kwargs):
        calls.append(path)
        if path == "/release-group/artist-rg":
            started.set()
            await release.wait()
            return {"id": "artist-rg", "title": "RG"}
        if path == "/release/release-a":
            return SimpleNamespace(
                release_group={"id": "group-a"},
                media=[],
            )
        raise AssertionError(f"unexpected MusicBrainz path: {path}")

    monkeypatch.setattr(mb_album, "mb_api_get", provider)
    first = asyncio.create_task(repo.get_release_group_by_id("ARTIST-RG"))
    await started.wait()
    second = asyncio.create_task(repo.get_release_group_by_id("artist-rg"))
    await asyncio.sleep(0)
    release.set()
    detail_one, detail_two = await asyncio.gather(first, second)

    batch = await repo.get_release_group_ids_batch(["RELEASE-A", "release-a"])

    assert detail_one == detail_two == {"id": "artist-rg", "title": "RG"}
    assert batch == {"RELEASE-A": "group-a", "release-a": "group-a"}
    assert calls == ["/release-group/artist-rg", "/release/release-a"]
