"""ITunesRepository: result filtering, error handling, degradation recording.

The live shape is recorded in repositories/ITUNES_API_NOTES.md; these fixtures
echo it (including the tribute-album-ranks-first trap the probe surfaced).
"""

import json
from unittest.mock import AsyncMock

import httpx
import pytest

from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from repositories.itunes_repository import ITunesRepository


def _response(status_code: int, payload: dict | None = None) -> httpx.Response:
    return httpx.Response(
        status_code=status_code,
        content=json.dumps(payload or {}).encode(),
        request=httpx.Request("GET", "https://itunes.apple.com/search"),
    )


def _result(artist: str, name: str = "Nevermind", url: str = "https://music.apple.com/gb/album/x") -> dict:
    return {
        "wrapperType": "collection",
        "collectionType": "Album",
        "artistName": artist,
        "collectionName": name,
        "collectionViewUrl": url,
    }


@pytest.fixture(autouse=True)
def _reset_breaker():
    ITunesRepository.reset_circuit_breaker()
    yield
    ITunesRepository.reset_circuit_breaker()


@pytest.mark.asyncio
async def test_find_album_skips_tribute_albums_ranked_first():
    client = AsyncMock()
    client.get = AsyncMock(
        return_value=_response(
            200,
            {
                "resultCount": 2,
                "results": [
                    _result("Piano Tribute Players", "Piano Tribute to Nirvana"),
                    _result("Nirvana", "Nevermind", "https://music.apple.com/gb/album/real"),
                ],
            },
        )
    )
    repo = ITunesRepository(client)

    found = await repo.find_album("Nirvana", "Nevermind", country="GB")

    assert found is not None
    assert found.artist_name == "Nirvana"
    assert found.url.endswith("/real")


@pytest.mark.asyncio
async def test_find_album_returns_none_when_nothing_matches_artist():
    client = AsyncMock()
    client.get = AsyncMock(
        return_value=_response(
            200,
            {"resultCount": 1, "results": [_result("Someone Else Entirely")]},
        )
    )
    repo = ITunesRepository(client)

    assert await repo.find_album("Nirvana", "Nevermind") is None


@pytest.mark.asyncio
async def test_http_error_degrades_and_returns_none():
    client = AsyncMock()
    client.get = AsyncMock(return_value=_response(500))
    repo = ITunesRepository(client)

    ctx = init_degradation_context()
    try:
        assert await repo.find_album("Nirvana", "Nevermind") is None
    finally:
        clear_degradation_context()
    assert ctx.has_degradation()


@pytest.mark.asyncio
async def test_rate_limit_degrades_and_returns_none():
    client = AsyncMock()
    client.get = AsyncMock(return_value=_response(429))
    repo = ITunesRepository(client)

    ctx = init_degradation_context()
    try:
        assert await repo.find_album("Nirvana", "Nevermind") is None
    finally:
        clear_degradation_context()
    assert ctx.has_degradation()


@pytest.mark.asyncio
async def test_empty_term_short_circuits_without_a_call():
    client = AsyncMock()
    repo = ITunesRepository(client)

    assert await repo.find_album("", "") is None
    client.get.assert_not_awaited()
