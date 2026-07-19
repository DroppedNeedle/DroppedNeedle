"""ProwlarrIndexer: JSON-row → tagged IndexerResult mapping (both arms), per-protocol
dedup, unusable-row drops, search cache, and health mapping."""

from unittest.mock import AsyncMock

import pytest

from repositories.prowlarr.prowlarr_client import ProwlarrApiError
from repositories.prowlarr.prowlarr_indexer import ProwlarrIndexer
from repositories.prowlarr.prowlarr_models import (
    ProwlarrCategory,
    ProwlarrSearchResult,
    ProwlarrSystemStatus,
)


def _row(
    *, protocol, title="Radiohead - In Rainbows FLAC", size=600_000_000, guid="g",
    download="https://prowlarr/dl", magnet="", seeders=None, indexer="idx",
    publish="2026-07-01T12:00:00Z",
) -> ProwlarrSearchResult:
    return ProwlarrSearchResult(
        guid=guid, title=title, size=size, indexer_id=7, indexer=indexer,
        protocol=protocol, download_url=download, magnet_url=magnet,
        categories=[ProwlarrCategory(id=3000, name="Audio")],
        seeders=seeders, leechers=1, grabs=12, publish_date=publish,
    )


def _indexer(rows=None, *, enabled=True):
    client = AsyncMock()
    client.search.return_value = rows or []
    client.system_status.return_value = ProwlarrSystemStatus(version="2.1.0")
    client.absolute_url = lambda value: value
    return ProwlarrIndexer(client, categories=[3000], enabled=enabled), client


@pytest.mark.asyncio
async def test_search_maps_both_protocol_arms():
    ix, _ = _indexer([
        _row(protocol="usenet", guid="u1"),
        _row(protocol="torrent", guid="t1", magnet="magnet:?xt=x", seeders=40,
             title="Radiohead - In Rainbows FLAC [torrent]"),
    ])
    results = await ix.search_album("Radiohead", "In Rainbows")
    by_source = {r.source for r in results}
    assert by_source == {"usenet", "torrent"}
    usenet = next(r.usenet for r in results if r.usenet is not None)
    torrent = next(r.torrent for r in results if r.torrent is not None)
    assert usenet.nzb_url == "https://prowlarr/dl"
    assert usenet.grabs == 12
    assert usenet.usenet_date is not None
    assert torrent.magnet_url == "magnet:?xt=x"
    assert torrent.seeders == 40
    assert torrent.category_ids == [3000]


@pytest.mark.asyncio
async def test_dedup_is_per_protocol_by_title_and_size():
    # Same title+size on BOTH protocols must keep one of EACH; a duplicate within a
    # protocol is dropped.
    ix, _ = _indexer([
        _row(protocol="usenet", guid="u1"),
        _row(protocol="usenet", guid="u2", indexer="other"),
        _row(protocol="torrent", guid="t1", seeders=5),
    ])
    results = await ix.search_album("Radiohead", "In Rainbows")
    assert len(results) == 2


@pytest.mark.asyncio
async def test_unusable_rows_are_dropped():
    ix, _ = _indexer([
        _row(protocol="usenet", download=""),  # no NZB URL
        _row(protocol="torrent", download="", magnet="", seeders=5),  # no grab link
        _row(protocol="unknown", guid="x"),  # unrecognised protocol
    ])
    assert await ix.search_album("Radiohead", "In Rainbows") == []


@pytest.mark.asyncio
async def test_search_error_returns_empty_not_raises():
    ix, client = _indexer()
    client.search.side_effect = ProwlarrApiError("boom")
    assert await ix.search_album("Radiohead", "In Rainbows") == []


@pytest.mark.asyncio
async def test_search_cache_prevents_second_hit():
    ix, client = _indexer([_row(protocol="usenet")])
    await ix.search_album("Radiohead", "In Rainbows")
    await ix.search_album("Radiohead", "In Rainbows")
    assert client.search.await_count == 1


@pytest.mark.asyncio
async def test_health_check_ok_and_error():
    ix, client = _indexer()
    status = await ix.health_check()
    assert status.status == "ok"
    assert status.version == "2.1.0"
    client.system_status.side_effect = ProwlarrApiError("no auth")
    assert (await ix.health_check()).status == "error"


def test_is_configured_follows_enabled_flag():
    ix, _ = _indexer(enabled=False)
    assert ix.is_configured() is False
