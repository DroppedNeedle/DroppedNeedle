"""B1: /extended coalescing + relations-only slimming.

K=5 concurrent cold renders produce exactly one leader chain (followers share
the IDENTICAL object the leader built); the MB leg goes through
get_artist_relations (url-rels only, IMAGE_FETCH lane); failures keep the
historical null-object contract for leader AND followers.
"""

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from models.artist import ArtistInfo
from services.artist_service import ArtistService

ARTIST_MBID = "f4a31f0a-51dd-4fa7-986d-3095c40c5ed9"


class InMemoryCacheStub:
    """Minimal real cache: None-miss semantics like production get()."""

    def __init__(self):
        self._store: dict[str, object] = {}

    async def get(self, key):
        return self._store.get(key)

    async def set(self, key, value, ttl_seconds=60):
        self._store[key] = value

    def capture_clear_token(self):
        return (self, 0)

    async def get_with_metadata(self, key):
        return self._store.get(key), None

    async def set_if_token(self, token, key, value, ttl_seconds=60, metadata=None):
        del token, ttl_seconds, metadata
        self._store[key] = value
        return True


def _relations_payload() -> dict:
    return {
        "id": ARTIST_MBID,
        "name": "Test Artist",
        "relations": [
            {
                "type": "wikipedia",
                "url": {"resource": "https://en.wikipedia.org/wiki/Test_Artist"},
            },
            {
                "type": "wikidata",
                "url": {"resource": "https://www.wikidata.org/wiki/Q123"},
            },
        ],
    }


def _make_service(*, relations_slow: float = 0.0) -> tuple[ArtistService, dict]:
    mb_repo = AsyncMock()
    calls = {"relations": 0, "detail": 0}

    async def fake_relations(mbid):
        calls["relations"] += 1
        if relations_slow:
            await asyncio.sleep(relations_slow)
        return _relations_payload()

    async def fail_detail(mbid):
        calls["detail"] += 1
        raise AssertionError("full detail must not be fetched for /extended")

    mb_repo.get_artist_by_id = AsyncMock(side_effect=fail_detail)
    mb_repo.get_artist_relations = AsyncMock(side_effect=fake_relations)

    library_repo = MagicMock()
    library_repo.is_configured.return_value = False

    wikidata_repo = AsyncMock()
    wiki_calls = {"extract": 0, "image": 0}

    async def fake_extract(url, lang="en"):
        wiki_calls["extract"] += 1
        if relations_slow:
            await asyncio.sleep(relations_slow)
        return "A description."

    async def fake_image(wikidata_id):
        wiki_calls["image"] += 1
        if relations_slow:
            await asyncio.sleep(relations_slow)
        return "https://example.com/img.jpg"

    wikidata_repo.get_wikipedia_extract = AsyncMock(side_effect=fake_extract)
    wikidata_repo.get_artist_image_from_wikidata = AsyncMock(side_effect=fake_image)
    wikidata_repo.get_wikidata_id_from_url = MagicMock(return_value="Q123")

    prefs = MagicMock()
    prefs.get_advanced_settings.return_value = MagicMock(
        cache_ttl_artist_library=21600,
        cache_ttl_artist_non_library=3600,
    )

    memory_cache = InMemoryCacheStub()
    disk_cache = AsyncMock()
    # capture_clear_token is sync on real caches; an AsyncMock child leaves
    # a dangling coroutine (RuntimeWarning) on every _begin_projection.
    disk_cache.capture_clear_token = MagicMock(return_value=("test-disk", 0))
    disk_cache.get_artist = AsyncMock(return_value=None)
    disk_cache.set_artist = AsyncMock()

    svc = ArtistService(
        mb_repo=mb_repo,
        library_repo=library_repo,
        wikidata_repo=wikidata_repo,
        preferences_service=prefs,
        memory_cache=memory_cache,
        disk_cache=disk_cache,
    )
    return svc, {
        "mb_calls": calls,
        "wiki_calls": wiki_calls,
        "wikidata": wikidata_repo,
        "memory_cache": memory_cache,
    }


class TestExtendedCoalescing:
    @pytest.mark.asyncio
    async def test_k_concurrent_cold_renders_share_one_leader_chain(self):
        svc, probes = _make_service(relations_slow=0.05)

        results = await asyncio.gather(
            *(svc.get_artist_extended_info(ARTIST_MBID) for _ in range(5))
        )

        # Followers receive the IDENTICAL object the leader built.
        assert all(r is results[0] for r in results)
        assert results[0].description == "A description."
        assert results[0].image == "https://example.com/img.jpg"

        assert probes["mb_calls"]["relations"] == 1  # one leader, one browse
        assert probes["wiki_calls"]["extract"] == 1  # dedup collapses followers
        assert probes["wiki_calls"]["image"] == 1
        # All five callers reached the repo (coalescing happens at dedupe level,
        # not by dropping requests); only one load executed per hop.
        # The service-level future map collapses followers BEFORE the repo:
        # exactly one caller touches each wiki hop (the leader).
        assert probes["wikidata"].get_wikipedia_extract.await_count == 1
        assert probes["wikidata"].get_artist_image_from_wikidata.await_count == 1
        # Full-detail fetch is never used on this path (B1 slimming).
        assert probes["mb_calls"]["detail"] == 0

    @pytest.mark.asyncio
    async def test_failure_keeps_null_object_contract_for_all_callers(self):
        svc, _probes = _make_service()

        async def failing_relations(mbid):
            raise RuntimeError("mb down")

        svc._mb_repo.get_artist_relations = AsyncMock(side_effect=failing_relations)

        results = await asyncio.gather(
            *(svc.get_artist_extended_info(ARTIST_MBID) for _ in range(3))
        )

        assert all(r.description is None and r.image is None for r in results)

    @pytest.mark.asyncio
    async def test_warm_described_entry_short_circuits(self):
        from repositories.musicbrainz_base import (
            capture_mb_source_context,
            namespace_mb_cache_key,
        )

        svc, probes = _make_service()
        # Production reads the source-namespaced key (the real cache also
        # namespaces on write, so this round-trips there); the stub does no
        # namespacing, so the seed must carry it explicitly.
        await probes["memory_cache"].set(
            namespace_mb_cache_key(
                f"artist_info:{ARTIST_MBID}", capture_mb_source_context()
            ),
            ArtistInfo(
                name="Test Artist",
                musicbrainz_id=ARTIST_MBID,
                description="Already have it",
                image="https://example.com/x.jpg",
            ),
        )

        result = await svc.get_artist_extended_info(ARTIST_MBID)

        assert result.description == "Already have it"
        assert probes["mb_calls"]["relations"] == 0
