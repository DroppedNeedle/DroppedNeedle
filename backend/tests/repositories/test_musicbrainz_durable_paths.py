from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import repositories.musicbrainz_album as mb_album
import repositories.musicbrainz_base as mb_base
from infrastructure.cache.cache_keys import MB_RELEASE_TO_RG_PREFIX
from infrastructure.cache.memory_cache import InMemoryCache
from repositories.musicbrainz_album import MusicBrainzAlbumMixin


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self) -> None:
        self._cache = InMemoryCache()
        self._preferences_service = MagicMock()
        self._preferences_service.get_advanced_settings.return_value = SimpleNamespace(
            cache_ttl_search=60
        )


@pytest.mark.asyncio
async def test_release_to_group_reads_durable_hit_before_wire(monkeypatch):
    repo = _Repo()
    store = MagicMock()
    store.get_release_to_rg_batch = AsyncMock(return_value={"rel-1": "rg-1"})
    repo._mb_canonical_store = store
    provider = AsyncMock(side_effect=AssertionError("durable hit reached wire"))
    monkeypatch.setattr(mb_album, "mb_api_get", provider)

    result = await repo.get_release_group_id_from_release("REL-1")

    assert result == "rg-1"
    assert await repo._cache.get(f"{MB_RELEASE_TO_RG_PREFIX}rel-1") == "rg-1"


@pytest.mark.asyncio
async def test_release_to_group_wire_miss_writes_durable_mapping(monkeypatch):
    repo = _Repo()
    store = MagicMock()
    store.get_release_to_rg_batch = AsyncMock(return_value={})
    store.save_release_to_rg = AsyncMock()
    repo._mb_canonical_store = store
    monkeypatch.setattr(
        mb_album,
        "mb_api_get",
        AsyncMock(return_value=SimpleNamespace(release_group={"id": "rg-2"}, media=[])),
    )

    result = await repo.get_release_group_id_from_release("rel-2")

    assert result == "rg-2"
    store.save_release_to_rg.assert_awaited_once()
    mapping, source_host = store.save_release_to_rg.await_args.args
    assert mapping == {"rel-2": "rg-2"}
    assert source_host.startswith("https://")


@pytest.mark.asyncio
async def test_stale_release_mapping_does_not_write_after_source_switch(monkeypatch):
    repo = _Repo()
    store = MagicMock()
    store.save_release_to_rg = AsyncMock()
    repo._mb_canonical_store = store
    original_source = mb_base.get_mb_api_base()
    mb_base.set_mb_api_base("https://old.example/ws/2")
    token = mb_base._mb_response_context.set(
        mb_base.MbSourceContext(
            source_url=mb_base.get_mb_api_base(),
            generation=mb_base.get_mb_source_generation(),
        )
    )

    async def old_provider(*_args, **_kwargs):
        mb_base.set_mb_api_base("https://new.example/ws/2")
        return SimpleNamespace(release_group={"id": "rg-stale"}, media=[])

    monkeypatch.setattr(mb_album, "mb_api_get", old_provider)
    try:
        result = await repo._fetch_release_group_id_from_release(
            "rel-stale", "mb:release_to_rg:rel-stale"
        )
    finally:
        mb_base._mb_response_context.reset(token)
        mb_base.set_mb_api_base(original_source)

    assert result == "rg-stale"
    store.save_release_to_rg.assert_not_awaited()
    assert await repo._cache.get("mb:release_to_rg:rel-stale") is None
