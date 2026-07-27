"""TargetLibraryRepository must satisfy the artist-paging half of the library repository
protocol that core/tasks.py pages with.

Regression: the target lifecycle injects TargetLibraryRepository into
start_artist_discovery_cache_warming_task, but the class had no get_artist_mbid_page, so
warm_artist_discovery_cache_periodically raised AttributeError on every cycle and the
artist discovery cache was never warmed.
"""

from unittest.mock import AsyncMock, MagicMock

import pytest

from services.native.target_library_repository import TargetLibraryRepository

MBIDS = {
    "0c1f7f1e-2a3b-4c5d-8e9f-000000000003",
    "0A1F7F1E-2A3B-4C5D-8E9F-000000000001",
    "0b1f7f1e-2a3b-4c5d-8e9f-000000000002",
}


def _repo(mbids: set[str]) -> TargetLibraryRepository:
    store = MagicMock()
    store.target_provider_artist_ids = AsyncMock(return_value=mbids)
    return TargetLibraryRepository(store)


@pytest.mark.asyncio
async def test_repository_exposes_the_paging_method_the_warmer_calls() -> None:
    # The warmer calls this by name; absence is the regression itself.
    assert hasattr(TargetLibraryRepository, "get_artist_mbid_page")


@pytest.mark.asyncio
async def test_page_is_sorted_casefolded_and_starts_after_the_cursor() -> None:
    repo = _repo(MBIDS)

    first = await repo.get_artist_mbid_page(after_mbid="", limit=2)
    assert first == [
        "0a1f7f1e-2a3b-4c5d-8e9f-000000000001",
        "0b1f7f1e-2a3b-4c5d-8e9f-000000000002",
    ]

    second = await repo.get_artist_mbid_page(after_mbid=first[-1], limit=2)
    assert second == ["0c1f7f1e-2a3b-4c5d-8e9f-000000000003"]

    # Exhausted: an empty page is what terminates the warmer's paging loop.
    assert await repo.get_artist_mbid_page(after_mbid=second[-1], limit=2) == []


@pytest.mark.asyncio
async def test_empty_library_terminates_immediately() -> None:
    assert await _repo(set()).get_artist_mbid_page(after_mbid="", limit=500) == []


@pytest.mark.asyncio
async def test_blank_mbids_are_skipped_and_limit_is_floored() -> None:
    repo = _repo({"", "0a1f7f1e-2a3b-4c5d-8e9f-000000000001"})
    assert await repo.get_artist_mbid_page(after_mbid="", limit=500) == [
        "0a1f7f1e-2a3b-4c5d-8e9f-000000000001"
    ]
    # limit <= 0 must still yield progress rather than an empty page, which the warmer
    # would misread as "no more artists" and stop.
    assert len(await repo.get_artist_mbid_page(after_mbid="", limit=0)) == 1
