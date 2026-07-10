"""Post-import lyrics hook: when a downloaded track lands in the library,
``LocalLyricsService.fetch_lyrics_for_new_import`` fetches from LRCLIB once and
writes the ``.lrc`` sidecar - but only when the admin setting is on and the
file has no lyrics yet. Best-effort by contract: every failure is swallowed so
an import can never be failed or slowed by lyrics. ``FileProcessor`` schedules
it fire-and-forget after a successful import."""

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.settings import LibrarySettings
from models.audio import AudioInfo, AudioTag
from services.local_lyrics_service import LocalLyricsService
from services.native.file_processor import FileProcessor

_SYNCED = "[00:14.20]In the next world war\n[00:20.00]In a jackknifed juggernaut"


@pytest.fixture
def audio_path(tmp_path):
    # a lyric-less file: no tags mutagen can read, no sidecar next to it
    path = tmp_path / "01 Airbag.mp3"
    path.write_bytes(b"not really audio")
    return path


def _make_service(*, enabled, fetch_result=None):
    prefs = MagicMock()
    prefs.get_library_settings.return_value = LibrarySettings(
        lyrics_fetch_enabled=enabled
    )
    lrclib = MagicMock()
    lrclib.fetch_lyrics = AsyncMock(return_value=fetch_result)
    service = LocalLyricsService(
        MagicMock(),
        library_repo=MagicMock(),
        preferences_service=prefs,
        lrclib_repository=lrclib,
    )
    return service, lrclib


@pytest.mark.asyncio
async def test_fetches_and_writes_sidecar_when_lyrics_missing(audio_path):
    service, lrclib = _make_service(enabled=True, fetch_result=(_SYNCED, True))

    await service.fetch_lyrics_for_new_import(
        audio_path,
        artist_name="Radiohead",
        track_title="Airbag",
        album_title="OK Computer",
        duration_seconds=284.7,
    )

    assert audio_path.with_suffix(".lrc").read_text(encoding="utf-8") == _SYNCED
    lrclib.fetch_lyrics.assert_awaited_once_with(
        artist_name="Radiohead",
        track_name="Airbag",
        album_name="OK Computer",
        duration_seconds=284.7,
    )


@pytest.mark.asyncio
async def test_skips_when_lyrics_already_exist(audio_path):
    audio_path.with_suffix(".lrc").write_text("[00:01.00]Local line", encoding="utf-8")
    service, lrclib = _make_service(enabled=True, fetch_result=(_SYNCED, True))

    await service.fetch_lyrics_for_new_import(
        audio_path, artist_name="Radiohead", track_title="Airbag"
    )

    lrclib.fetch_lyrics.assert_not_awaited()
    # the existing sidecar was not overwritten
    assert (
        audio_path.with_suffix(".lrc").read_text(encoding="utf-8")
        == "[00:01.00]Local line"
    )


@pytest.mark.asyncio
async def test_setting_off_makes_no_calls(audio_path):
    service, lrclib = _make_service(enabled=False, fetch_result=(_SYNCED, True))

    await service.fetch_lyrics_for_new_import(
        audio_path, artist_name="Radiohead", track_title="Airbag"
    )

    lrclib.fetch_lyrics.assert_not_awaited()
    assert not audio_path.with_suffix(".lrc").exists()


@pytest.mark.asyncio
async def test_miss_is_negative_cached_across_hook_calls(audio_path):
    service, lrclib = _make_service(enabled=True, fetch_result=None)

    await service.fetch_lyrics_for_new_import(
        audio_path, artist_name="Radiohead", track_title="Airbag"
    )
    await service.fetch_lyrics_for_new_import(
        audio_path, artist_name="Radiohead", track_title="Airbag"
    )

    lrclib.fetch_lyrics.assert_awaited_once()  # ~24h negative cache
    assert not audio_path.with_suffix(".lrc").exists()


@pytest.mark.asyncio
async def test_any_failure_is_swallowed(audio_path):
    service, lrclib = _make_service(enabled=True)
    lrclib.fetch_lyrics.side_effect = RuntimeError("LRCLIB exploded")

    # must not raise - the hook's contract is that lyrics never break an import
    await service.fetch_lyrics_for_new_import(
        audio_path, artist_name="Radiohead", track_title="Airbag"
    )

    assert not audio_path.with_suffix(".lrc").exists()


# --- FileProcessor scheduling ------------------------------------------------


def _tag_and_info():
    tag = AudioTag(title="Airbag", artist="Radiohead", album="OK Computer", track_number=1)
    info = AudioInfo(
        duration_seconds=284.7,
        bitrate=320,
        sample_rate=44100,
        channels=2,
        file_format="mp3",
        file_size_bytes=1024,
    )
    return tag, info


@pytest.mark.asyncio
async def test_file_processor_schedules_fire_and_forget_fetch(audio_path):
    lyrics = MagicMock()
    lyrics.fetch_lyrics_for_new_import = AsyncMock()
    processor = FileProcessor(MagicMock(), lyrics_service=lyrics)
    tag, info = _tag_and_info()

    processor._schedule_lyrics_fetch(audio_path, tag, info)
    await asyncio.gather(*processor._lyrics_tasks)

    lyrics.fetch_lyrics_for_new_import.assert_awaited_once_with(
        audio_path,
        artist_name="Radiohead",
        track_title="Airbag",
        album_title="OK Computer",
        duration_seconds=284.7,
    )


@pytest.mark.asyncio
async def test_file_processor_lyrics_failure_never_raises(audio_path):
    lyrics = MagicMock()
    lyrics.fetch_lyrics_for_new_import = MagicMock(side_effect=RuntimeError("boom"))
    processor = FileProcessor(MagicMock(), lyrics_service=lyrics)
    tag, info = _tag_and_info()

    # scheduling must never propagate a lyrics failure into the import
    processor._schedule_lyrics_fetch(audio_path, tag, info)
    await asyncio.sleep(0)  # let any created task settle


def test_file_processor_without_lyrics_service_is_noop(audio_path):
    processor = FileProcessor(MagicMock())
    tag, info = _tag_and_info()

    processor._schedule_lyrics_fetch(audio_path, tag, info)  # must not raise

    assert processor._lyrics_tasks == set()
