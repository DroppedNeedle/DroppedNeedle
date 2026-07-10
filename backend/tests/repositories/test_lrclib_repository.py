"""LrcLibRepository tests - /api/get param shape, User-Agent, synced-over-plain
preference, and the soft failure modes (404/instrumental/errors all -> None).
No live calls: the httpx client is a stub."""

import httpx
import pytest

from repositories.lrclib_repository import (
    LRCLIB_API_URL,
    LRCLIB_USER_AGENT,
    LrcLibRepository,
)

_HIT_JSON = b"""
{"id": 3396226, "trackName": "Airbag", "artistName": "Radiohead",
"albumName": "OK Computer", "duration": 284.0, "instrumental": false,
"plainLyrics": "In the next world war\\nIn a jackknifed juggernaut",
"syncedLyrics": "[00:14.20]In the next world war\\n[00:20.00]In a jackknifed juggernaut"}
"""

_PLAIN_ONLY_JSON = b"""
{"instrumental": false, "syncedLyrics": null,
"plainLyrics": "In the next world war\\nIn a jackknifed juggernaut"}
"""

_INSTRUMENTAL_JSON = b'{"instrumental": true, "plainLyrics": "", "syncedLyrics": ""}'


class _StubClient:
    def __init__(self, responses: list[httpx.Response] | None = None, exc: Exception | None = None):
        self._responses = list(responses or [])
        self._exc = exc
        self.calls: list[dict] = []

    async def get(self, url: str, params=None, headers=None, timeout=None) -> httpx.Response:
        self.calls.append(
            {"url": url, "params": dict(params or {}), "headers": dict(headers or {})}
        )
        if self._exc is not None:
            raise self._exc
        return self._responses.pop(0)


@pytest.mark.asyncio
async def test_fetch_prefers_synced_lyrics_and_sends_signature():
    client = _StubClient([httpx.Response(200, content=_HIT_JSON)])
    repo = LrcLibRepository(client)

    result = await repo.fetch_lyrics(
        artist_name="Radiohead",
        track_name="Airbag",
        album_name="OK Computer",
        duration_seconds=284.7,
    )

    assert result == (
        "[00:14.20]In the next world war\n[00:20.00]In a jackknifed juggernaut",
        True,
    )
    call = client.calls[0]
    assert call["url"] == LRCLIB_API_URL
    assert call["params"] == {
        "artist_name": "Radiohead",
        "track_name": "Airbag",
        "album_name": "OK Computer",
        "duration": "285",  # rounded whole seconds
    }
    assert call["headers"]["User-Agent"] == LRCLIB_USER_AGENT


@pytest.mark.asyncio
async def test_fetch_falls_back_to_plain_lyrics():
    client = _StubClient([httpx.Response(200, content=_PLAIN_ONLY_JSON)])
    result = await LrcLibRepository(client).fetch_lyrics(
        artist_name="Radiohead", track_name="Airbag"
    )
    assert result == ("In the next world war\nIn a jackknifed juggernaut", False)


@pytest.mark.asyncio
async def test_fetch_404_means_no_lyrics():
    client = _StubClient([httpx.Response(404, content=b'{"message": "not found"}')])
    assert (
        await LrcLibRepository(client).fetch_lyrics(artist_name="A", track_name="T")
        is None
    )


@pytest.mark.asyncio
async def test_fetch_instrumental_means_no_lyrics():
    client = _StubClient([httpx.Response(200, content=_INSTRUMENTAL_JSON)])
    assert (
        await LrcLibRepository(client).fetch_lyrics(artist_name="A", track_name="T")
        is None
    )


@pytest.mark.asyncio
async def test_fetch_network_error_is_soft_none():
    client = _StubClient(exc=httpx.ConnectTimeout("boom"))
    assert (
        await LrcLibRepository(client).fetch_lyrics(artist_name="A", track_name="T")
        is None
    )


@pytest.mark.asyncio
async def test_fetch_skips_request_without_artist_or_title():
    client = _StubClient()
    repo = LrcLibRepository(client)
    assert await repo.fetch_lyrics(artist_name=None, track_name="T") is None
    assert await repo.fetch_lyrics(artist_name="A", track_name="") is None
    assert client.calls == []  # LRCLIB never contacted
