"""ProwlarrIndexer: protocol surface, ladder, backoff-skip, member-never-fails."""

import pytest

from models.common import ServiceStatus
from repositories.prowlarr.prowlarr_client import ProwlarrClient
from repositories.prowlarr.prowlarr_indexer import ProwlarrIndexer
from tests.mocks.prowlarr_mock import client_for


def _indexer(host: str, **kwargs) -> ProwlarrIndexer:
    client = ProwlarrClient(client_for(), f"http://{host}", "secret")
    return ProwlarrIndexer(client, categories=[3000], **kwargs)


def test_protocol_surface_and_unconfigured():
    indexer = _indexer("prowlarr")
    assert indexer.indexer_name == "usenet"
    assert indexer.is_configured() is True
    assert ProwlarrIndexer(None).is_configured() is False
    assert _indexer("prowlarr", enabled=False).is_configured() is False


def test_no_future_annotations_import():
    import pathlib
    import re

    for name in ("prowlarr_client.py", "prowlarr_indexer.py", "prowlarr_models.py"):
        text = pathlib.Path(f"repositories/prowlarr/{name}").read_text()
        assert re.search(r"^from __future__ import annotations", text, re.M) is None


@pytest.mark.asyncio
async def test_health_ok_reports_version_and_count():
    status = await _indexer("prowlarr").health_check()
    assert status.status == "ok"
    assert status.version == "1.32.2.4987"
    # 2 enabled of 3 rows: the disabled tracker is not counted.
    assert "2 enabled indexer(s)" in status.message


@pytest.mark.asyncio
async def test_health_degrades_when_status_404s():
    status = await _indexer("prowlarr-nostatus").health_check()
    assert status.status == "ok"  # indexer list answered; version just missing
    assert status.version is None


@pytest.mark.asyncio
async def test_health_unconfigured_and_unreachable():
    status = await ProwlarrIndexer(None).health_check()
    assert isinstance(status, ServiceStatus) and status.status == "error"
    status = await _indexer("prowlarr-auth").health_check()
    assert status.status == "error"  # never raises


@pytest.mark.asyncio
async def test_search_album_wraps_usenet_results():
    results = await _indexer("prowlarr").search_album("Radiohead", "In Rainbows")
    assert len(results) == 1
    assert results[0].source == "usenet"
    assert results[0].usenet is not None
    assert "In Rainbows" in results[0].usenet.title


@pytest.mark.asyncio
async def test_search_track_and_empty_query():
    results = await _indexer("prowlarr").search_track("Radiohead", "Weird Fishes")
    assert len(results) == 1  # free-text mock feed
    assert await _indexer("prowlarr").search_album("", "") == []


@pytest.mark.asyncio
async def test_member_errors_map_to_empty_never_raise():
    for host in ("prowlarr-auth", "prowlarr-error", "prowlarr-html"):
        assert await _indexer(host).search_album("Radiohead", "In Rainbows") == []


@pytest.mark.asyncio
async def test_rate_limit_backs_off_member():
    indexer = _indexer("prowlarr-ratelimit", rate_limit_backoff=60.0)
    assert await indexer.search_album("Radiohead", "In Rainbows") == []
    # Second search skips without HTTP (backoff); still empty, still silent.
    assert await indexer.search_album("Radiohead", "In Rainbows") == []
    assert indexer._backoff_until > 0


@pytest.mark.asyncio
async def test_ladder_retries_with_ascii_hyphen_when_the_title_has_a_unicode_hyphen():
    """#522: Prowlarr/indexers return 0 for MusicBrainz's U+2010 "D-DAY" but find
    the release for the ASCII hyphen. A genuine clean empty on the canonical query
    must fall through to the normalized rung, which folds the dash."""
    queries: list[str] = []
    indexer = _indexer("prowlarr")

    async def search(query, categories, **kwargs):
        queries.append(query)
        if "‐" in query:
            return []
        return await ProwlarrClient.search(indexer._client, query, categories, **kwargs)

    indexer._client.search = search  # type: ignore[method-assign]
    results = await indexer.search_album("Agust D", "D‐DAY")

    assert queries == ["Agust D D‐DAY", "Agust D D-DAY"]
    assert len(results) == 1


@pytest.mark.asyncio
async def test_search_cache_serves_repeat_query():
    calls = {"n": 0}
    inner = _indexer("prowlarr")

    async def counting_search(query, categories, **kwargs):
        calls["n"] += 1
        return await ProwlarrClient.search(inner._client, query, categories, **kwargs)

    inner._client.search = counting_search  # type: ignore[method-assign]
    first = await inner.search_album("Radiohead", "In Rainbows")
    second = await inner.search_album("Radiohead", "In Rainbows")
    assert calls["n"] == 1
    assert [r.usenet.guid for r in first] == [r.usenet.guid for r in second]
