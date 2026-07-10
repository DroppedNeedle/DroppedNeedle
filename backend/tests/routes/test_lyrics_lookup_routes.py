"""Route tests for GET /lyrics/lookup (metadata-only LRCLIB lookup, Plex playback).

Covers the admin gate (setting off -> 404, no LRCLIB call), the shared response
shape on a hit, 404 on a miss, and the ~24h in-process cache (hits AND misses)
that keeps seek-driven refetches from re-hitting LRCLIB.
"""

from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import FastAPI

from api.v1.routes.lyrics import router
from api.v1.schemas.settings import LibrarySettings
from core.dependencies import get_lyrics_lookup_service
from services.lyrics_lookup_service import LyricsLookupService
from tests.helpers import build_test_client, override_user_auth

_SYNCED = "[00:14.20]In the next world war\n[00:20.00]In a jackknifed juggernaut"


def _make_service(*, enabled, fetch_result=None):
    prefs = MagicMock()
    prefs.get_library_settings.return_value = LibrarySettings(
        lyrics_fetch_enabled=enabled
    )
    lrclib = MagicMock()
    lrclib.fetch_lyrics = AsyncMock(return_value=fetch_result)
    return LyricsLookupService(prefs, lrclib), lrclib


def _make_client(service):
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_lyrics_lookup_service] = lambda: service
    override_user_auth(app)
    return build_test_client(app)


def test_lookup_404_when_setting_off_and_never_touches_lrclib():
    service, lrclib = _make_service(enabled=False, fetch_result=(_SYNCED, True))
    client = _make_client(service)

    resp = client.get("/lyrics/lookup", params={"artist": "Radiohead", "track": "Airbag"})

    assert resp.status_code == 404
    lrclib.fetch_lyrics.assert_not_awaited()


def test_lookup_hit_returns_shared_lyrics_shape():
    service, lrclib = _make_service(enabled=True, fetch_result=(_SYNCED, True))
    client = _make_client(service)

    resp = client.get(
        "/lyrics/lookup",
        params={
            "artist": "Radiohead",
            "track": "Airbag",
            "album": "OK Computer",
            "duration": 284.7,
        },
    )

    assert resp.status_code == 200
    body = resp.json()
    assert set(body.keys()) == {"text", "is_synced", "lines"}
    assert body["is_synced"] is True
    assert body["lines"][0] == {"text": "In the next world war", "start_seconds": 14.2}
    assert body["lines"][1]["start_seconds"] == 20.0
    assert body["text"] == "In the next world war\nIn a jackknifed juggernaut"
    lrclib.fetch_lyrics.assert_awaited_once_with(
        artist_name="Radiohead",
        track_name="Airbag",
        album_name="OK Computer",
        duration_seconds=284.7,
    )


def test_lookup_plain_lyrics_unsynced():
    plain = "In the next world war\nIn a jackknifed juggernaut"
    service, _lrclib = _make_service(enabled=True, fetch_result=(plain, False))
    client = _make_client(service)

    resp = client.get("/lyrics/lookup", params={"artist": "Radiohead", "track": "Airbag"})

    assert resp.status_code == 200
    body = resp.json()
    assert body["is_synced"] is False
    assert [line["start_seconds"] for line in body["lines"]] == [None, None]


def test_lookup_404_on_lrclib_miss():
    service, lrclib = _make_service(enabled=True, fetch_result=None)
    client = _make_client(service)

    resp = client.get("/lyrics/lookup", params={"artist": "Nobody", "track": "Nothing"})

    assert resp.status_code == 404
    lrclib.fetch_lyrics.assert_awaited_once()


def test_lookup_hit_is_cached_so_seek_refetches_never_re_hit_lrclib():
    service, lrclib = _make_service(enabled=True, fetch_result=(_SYNCED, True))
    client = _make_client(service)

    first = client.get("/lyrics/lookup", params={"artist": "Radiohead", "track": "Airbag"})
    # normalized key: case/whitespace variants land on the same cache entry
    second = client.get(
        "/lyrics/lookup", params={"artist": "  radiohead ", "track": "AIRBAG"}
    )

    assert first.status_code == 200
    assert second.status_code == 200
    assert second.json() == first.json()
    lrclib.fetch_lyrics.assert_awaited_once()


def test_lookup_miss_is_cached_too():
    service, lrclib = _make_service(enabled=True, fetch_result=None)
    client = _make_client(service)

    assert client.get(
        "/lyrics/lookup", params={"artist": "Nobody", "track": "Nothing"}
    ).status_code == 404
    assert client.get(
        "/lyrics/lookup", params={"artist": "Nobody", "track": "Nothing"}
    ).status_code == 404

    lrclib.fetch_lyrics.assert_awaited_once()  # ~24h negative cache


@pytest.mark.asyncio
async def test_cache_expires_after_ttl():
    service, lrclib = _make_service(enabled=True, fetch_result=(_SYNCED, True))

    assert await service.lookup(artist="Radiohead", track="Airbag") is not None
    # jump past the TTL: the next request may ask LRCLIB again
    key = next(iter(service._cache))
    expires_at, response = service._cache[key]
    service._cache[key] = (expires_at - LyricsLookupService._TTL_SECONDS - 1, response)

    assert await service.lookup(artist="Radiohead", track="Airbag") is not None
    assert lrclib.fetch_lyrics.await_count == 2
