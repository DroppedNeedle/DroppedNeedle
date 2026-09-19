from core.exceptions import InvalidExternalPayloadError
"""While the MusicBrainz circuit breaker is open, repository methods degrade
quietly: the DegradationContext record is the error signal (per AGENTS.md), not
a per-call error log - a backed-off queue must not spam thousands of lines."""

import logging
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

import repositories.musicbrainz_album as album_module
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from repositories.musicbrainz_album import MusicBrainzAlbumMixin
from infrastructure.cache.memory_cache import InMemoryCache


class _Repo(MusicBrainzAlbumMixin):
    def __init__(self) -> None:
        self._cache = InMemoryCache()
        self._preferences_service = SimpleNamespace(
            get_advanced_settings=lambda: SimpleNamespace(cache_ttl_search=3600)
        )


@pytest.fixture
def open_breaker(monkeypatch):
    monkeypatch.setattr(
        album_module,
        "mb_api_get",
        AsyncMock(side_effect=CircuitOpenError("open", breaker_name="musicbrainz")),
    )
    degradation = Mock()
    monkeypatch.setattr(album_module, "_record_mb_degradation", degradation)
    return degradation


@pytest.mark.asyncio
async def test_search_albums_degrades_quietly_when_breaker_open(
    open_breaker, caplog
) -> None:
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert await _Repo().search_albums("query") == []
    assert caplog.records == []
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_search_recordings_degrades_quietly_when_breaker_open(
    open_breaker, caplog
) -> None:
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert await _Repo().search_recordings("query", "title") == []
    assert caplog.records == []
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_get_release_group_by_id_degrades_quietly_when_breaker_open(
    open_breaker, caplog
) -> None:
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert (
            await _Repo().get_release_group_by_id(
                "11111111-1111-4111-8111-111111111111"
            )
            is None
        )
    assert caplog.records == []
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_get_release_by_id_degrades_quietly_when_breaker_open(
    open_breaker, caplog
) -> None:
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert (
            await _Repo().get_release_by_id(
                "22222222-2222-4222-8222-222222222222",
                priority=RequestPriority.USER_INITIATED,
            )
            is None
        )
    assert caplog.records == []
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_get_recording_by_id_degrades_quietly_when_breaker_open(
    open_breaker, caplog
) -> None:
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert (
            await _Repo().get_recording_by_id(
                "33333333-3333-4333-8333-333333333333"
            )
            is None
        )
    assert caplog.records == []
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_search_release_groups_by_tag_degrades_quietly_when_breaker_open(
    open_breaker, caplog
) -> None:
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert await _Repo().search_release_groups_by_tag("tag") == []
    assert caplog.records == []
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_resolve_recording_to_release_group_degrades_quietly_when_breaker_open(
    open_breaker, caplog
) -> None:
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert (
            await _Repo().resolve_recording_to_release_group(
                "33333333-3333-4333-8333-333333333333"
            )
            is None
        )
    assert caplog.records == []
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_real_errors_still_log(open_breaker, caplog, monkeypatch) -> None:
    monkeypatch.setattr(
        album_module,
        "mb_api_get",
        AsyncMock(side_effect=RuntimeError("boom")),
    )
    with caplog.at_level(logging.ERROR, logger="repositories.musicbrainz_album"):
        assert (
            await _Repo().get_recording_by_id(
                "33333333-3333-4333-8333-333333333333"
            )
            is None
        )
    assert len(caplog.records) == 1
    open_breaker.assert_called_once()


@pytest.mark.asyncio
async def test_malformed_json_raises_invalid_payload_without_retry(monkeypatch):
    """F-056 fuel removal: a 200 response with unparseable JSON raises the
    honest deterministic error exactly once - no retry, no breaker failure."""
    import httpx as _httpx

    calls = {"n": 0}

    class _Client:
        async def get(self, *_args, **_kwargs):
            calls["n"] += 1
            return _httpx.Response(
                200,
                content=b"{not-json",
                headers={"content-type": "application/json"},
                request=_httpx.Request("GET", "https://musicbrainz.org/ws/2/x"),
            )

    from repositories import musicbrainz_base

    monkeypatch.setattr(musicbrainz_base, "_http_client", _Client())
    monkeypatch.setattr(musicbrainz_base.mb_rate_limiter, "acquire", AsyncMock())

    with pytest.raises(InvalidExternalPayloadError):
        await musicbrainz_base.mb_api_get("/release/xxx")

    assert calls["n"] == 1
