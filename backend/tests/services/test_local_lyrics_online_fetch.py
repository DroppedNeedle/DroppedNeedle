"""LocalLyricsService online-fetch behaviour (optional LRCLIB integration):
the admin gate short-circuits when off, hits write a .lrc sidecar beside the
audio file, read-only mounts fall back to the in-memory cache, and misses are
negative-cached so repeated requests never re-hit LRCLIB."""

from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.settings import LibrarySettings
from services.local_lyrics_service import LocalLyricsService

_SYNCED = "[00:14.20]In the next world war\n[00:20.00]In a jackknifed juggernaut"
_ROW = {
    "artist_name": "Radiohead",
    "album_artist_name": "Radiohead",
    "track_title": "Airbag",
    "album_title": "OK Computer",
    "duration_seconds": 284.7,
}


@pytest.fixture
def audio_path(tmp_path):
    # a lyric-less file: no tags mutagen can read, no sidecar next to it
    path = tmp_path / "01 Airbag.mp3"
    path.write_bytes(b"not really audio")
    return path


def _make_service(audio_path, *, enabled, fetch_result=None):
    local_files = MagicMock()
    local_files.resolve_validated_path = AsyncMock(return_value=audio_path)
    prefs = MagicMock()
    prefs.get_library_settings.return_value = LibrarySettings(
        lyrics_fetch_enabled=enabled
    )
    repo = MagicMock()
    repo.get_file_row_by_id = AsyncMock(return_value=dict(_ROW))
    lrclib = MagicMock()
    lrclib.fetch_lyrics = AsyncMock(return_value=fetch_result)
    service = LocalLyricsService(
        local_files,
        library_repo=repo,
        preferences_service=prefs,
        lrclib_repository=lrclib,
    )
    return service, lrclib, repo


@pytest.mark.asyncio
async def test_setting_off_never_touches_lrclib(audio_path):
    service, lrclib, repo = _make_service(
        audio_path, enabled=False, fetch_result=(_SYNCED, True)
    )

    assert await service.get_lyrics("f1") is None

    lrclib.fetch_lyrics.assert_not_awaited()
    repo.get_file_row_by_id.assert_not_awaited()
    assert not audio_path.with_suffix(".lrc").exists()  # byte-identical to today


@pytest.mark.asyncio
async def test_hit_writes_lrc_sidecar_and_serves_synced_lyrics(audio_path):
    service, lrclib, _repo = _make_service(
        audio_path, enabled=True, fetch_result=(_SYNCED, True)
    )

    response = await service.get_lyrics("f1")

    assert response is not None and response.is_synced is True
    assert response.lines[0].text == "In the next world war"
    assert response.lines[0].start_seconds == 14.2
    # sidecar persisted next to the audio file, synced content as-is
    assert audio_path.with_suffix(".lrc").read_text(encoding="utf-8") == _SYNCED
    lrclib.fetch_lyrics.assert_awaited_once_with(
        artist_name="Radiohead",
        track_name="Airbag",
        album_name="OK Computer",
        duration_seconds=284.7,
    )

    # next request is served by the sidecar path - no second LRCLIB call
    again = await service.get_lyrics("f1")
    assert again is not None and again.is_synced is True
    assert lrclib.fetch_lyrics.await_count == 1


@pytest.mark.asyncio
async def test_plain_lyrics_written_as_untimestamped_lines(audio_path):
    plain = "In the next world war\nIn a jackknifed juggernaut"
    service, _lrclib, _repo = _make_service(
        audio_path, enabled=True, fetch_result=(plain, False)
    )

    response = await service.get_lyrics("f1")

    assert response is not None and response.is_synced is False
    assert [line.start_seconds for line in response.lines] == [None, None]
    assert audio_path.with_suffix(".lrc").read_text(encoding="utf-8") == plain


@pytest.mark.asyncio
async def test_failed_sidecar_write_still_serves_from_memory(audio_path):
    service, lrclib, _repo = _make_service(
        audio_path, enabled=True, fetch_result=(_SYNCED, True)
    )
    # a directory squatting on the sidecar path makes write_text raise OSError
    # (stands in for a read-only library mount)
    audio_path.with_suffix(".lrc").mkdir()

    response = await service.get_lyrics("f1")
    assert response is not None and response.is_synced is True

    # served again from the in-memory cache, still only one LRCLIB call
    again = await service.get_lyrics("f1")
    assert again is response
    assert lrclib.fetch_lyrics.await_count == 1


@pytest.mark.asyncio
async def test_miss_is_negative_cached(audio_path):
    service, lrclib, _repo = _make_service(audio_path, enabled=True, fetch_result=None)

    assert await service.get_lyrics("f1") is None
    assert await service.get_lyrics("f1") is None

    lrclib.fetch_lyrics.assert_awaited_once()  # 24h negative cache
    assert not audio_path.with_suffix(".lrc").exists()


@pytest.mark.asyncio
async def test_negative_cache_expires(audio_path, monkeypatch):
    service, lrclib, _repo = _make_service(audio_path, enabled=True, fetch_result=None)

    assert await service.get_lyrics("f1") is None
    # jump past the 24h TTL: the next request may ask LRCLIB again
    key = str(audio_path)
    service._negative[key] -= LocalLyricsService._NEGATIVE_TTL_SECONDS + 1
    assert await service.get_lyrics("f1") is None
    assert lrclib.fetch_lyrics.await_count == 2


@pytest.mark.asyncio
async def test_local_lyrics_win_without_fetch(audio_path):
    audio_path.with_suffix(".lrc").write_text(
        "[00:01.00]Local line", encoding="utf-8"
    )
    service, lrclib, _repo = _make_service(
        audio_path, enabled=True, fetch_result=(_SYNCED, True)
    )

    response = await service.get_lyrics("f1")

    assert response is not None
    assert response.lines[0].text == "Local line"
    lrclib.fetch_lyrics.assert_not_awaited()
