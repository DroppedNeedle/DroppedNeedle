"""Unit tests for the local-library lyrics service: LRC parsing + extraction/cache."""

import os
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest
from mutagen.flac import FLAC
from mutagen.id3 import ID3, SYLT, USLT

from services.local_lyrics_service import (
    LocalLyricsService,
    _extract_embedded,
    _read_sidecar,
    parse_lrc,
)

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "library"


# ---------------------------------------------------------------- parse_lrc

def test_parse_basic_timestamps():
    is_synced, lines = parse_lrc("[00:12.50]First line\n[00:15.00]Second line\n")
    assert is_synced is True
    assert lines == [("First line", 12.5), ("Second line", 15.0)]


def test_parse_fractional_precision_variants():
    # 2-digit centiseconds, 3-digit milliseconds, colon separator, no fraction
    is_synced, lines = parse_lrc(
        "[00:01.25]a\n[00:02.125]b\n[00:03:50]c\n[00:04]d\n"
    )
    assert is_synced is True
    assert lines == [("a", 1.25), ("b", 2.125), ("c", 3.5), ("d", 4.0)]


def test_parse_multiple_timestamps_per_line():
    is_synced, lines = parse_lrc("[00:10.00][01:10.00]Chorus\n[00:30.00]Verse\n")
    assert is_synced is True
    assert lines == [("Chorus", 10.0), ("Verse", 30.0), ("Chorus", 70.0)]


def test_parse_lines_sorted_by_start():
    _, lines = parse_lrc("[01:00.00]Later\n[00:05.00]Earlier\n")
    assert [text for text, _ in lines] == ["Earlier", "Later"]


def test_parse_strips_metadata_tags():
    text = "[ar:Radiohead]\n[ti:Airbag]\n[al:OK Computer]\n[by:someone]\n[00:01.00]Hi\n"
    is_synced, lines = parse_lrc(text)
    assert is_synced is True
    assert lines == [("Hi", 1.0)]


def test_parse_applies_positive_offset_shifting_earlier():
    # LRC convention: positive offset (ms) shifts lyric display earlier
    _, lines = parse_lrc("[offset:+500]\n[00:10.00]Line\n")
    assert lines == [("Line", 9.5)]


def test_parse_applies_negative_offset_shifting_later():
    _, lines = parse_lrc("[offset:-1500]\n[00:10.00]Line\n")
    assert lines == [("Line", 11.5)]


def test_parse_offset_never_goes_below_zero():
    _, lines = parse_lrc("[offset:+5000]\n[00:01.00]Line\n")
    assert lines == [("Line", 0.0)]


def test_parse_ignores_malformed_lines_when_synced():
    text = "[00:01.00]Good\n[xx:yy.zz]Broken stamp\nrandom untimed text\n[00:02.00]Also good\n"
    is_synced, lines = parse_lrc(text)
    assert is_synced is True
    assert lines == [("Good", 1.0), ("Also good", 2.0)]


def test_parse_bare_timestamp_lines_skipped():
    _, lines = parse_lrc("[00:01.00]Text\n[00:05.00]\n")
    assert lines == [("Text", 1.0)]


def test_parse_plain_text_is_unsynced():
    is_synced, lines = parse_lrc("Line one\nLine two\n\nLine three\n")
    assert is_synced is False
    assert lines == [
        ("Line one", None), ("Line two", None), ("", None), ("Line three", None),
    ]


def test_parse_empty_text_yields_no_lines():
    assert parse_lrc("") == (False, [])
    assert parse_lrc("\n\n  \n") == (False, [])


# ---------------------------------------------------------- sidecar reading

def _copy_fixture(tmp_path, name):
    dest = tmp_path / name
    dest.write_bytes((FIXTURES / name).read_bytes())
    return dest


def test_sidecar_lrc_found_next_to_audio(tmp_path):
    audio = _copy_fixture(tmp_path, "flac_full_01.flac")
    (tmp_path / "flac_full_01.lrc").write_text("[00:01.00]Hello", encoding="utf-8")
    assert _read_sidecar(audio) == "[00:01.00]Hello"


def test_sidecar_txt_requires_lrc_content(tmp_path):
    audio = _copy_fixture(tmp_path, "flac_full_01.flac")
    (tmp_path / "flac_full_01.txt").write_text("just some notes", encoding="utf-8")
    assert _read_sidecar(audio) is None
    (tmp_path / "flac_full_01.txt").write_text("[00:01.00]Timed", encoding="utf-8")
    assert _read_sidecar(audio) == "[00:01.00]Timed"


def test_sidecar_missing_returns_none(tmp_path):
    audio = _copy_fixture(tmp_path, "flac_full_01.flac")
    assert _read_sidecar(audio) is None


# ------------------------------------------------------- embedded extraction

def test_embedded_flac_lyrics_tag(tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_01.flac")
    audio = FLAC(audio_path)
    audio["LYRICS"] = "Plain embedded lyrics\nSecond line"
    audio.save()

    result = _extract_embedded(audio_path)
    assert result is not None
    assert result.is_synced is False
    assert [l.text for l in result.lines] == ["Plain embedded lyrics", "Second line"]


def test_embedded_flac_lrc_content_parsed_as_synced(tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_02.flac")
    audio = FLAC(audio_path)
    audio["LYRICS"] = "[00:05.00]Synced from tag\n[00:09.30]More"
    audio.save()

    result = _extract_embedded(audio_path)
    assert result.is_synced is True
    assert result.lines[0].text == "Synced from tag"
    assert result.lines[0].start_seconds == 5.0
    assert result.lines[1].start_seconds == pytest.approx(9.3)


def test_embedded_mp3_uslt(tmp_path):
    audio_path = _copy_fixture(tmp_path, "mp3_full_01.mp3")
    tags = ID3(audio_path)
    tags.add(USLT(encoding=3, lang="eng", desc="", text="Uslt line one\nUslt line two"))
    tags.save(audio_path)

    result = _extract_embedded(audio_path)
    assert result.is_synced is False
    assert [l.text for l in result.lines] == ["Uslt line one", "Uslt line two"]


def test_embedded_mp3_sylt_converted_to_synced(tmp_path):
    audio_path = _copy_fixture(tmp_path, "mp3_full_01.mp3")
    tags = ID3(audio_path)
    tags.add(SYLT(
        encoding=3, lang="eng", format=2, type=1,
        text=[("Second", 12000), ("First", 3000)],
    ))
    tags.save(audio_path)

    result = _extract_embedded(audio_path)
    assert result.is_synced is True
    assert [(l.text, l.start_seconds) for l in result.lines] == [
        ("First", 3.0), ("Second", 12.0),
    ]


def test_embedded_none_when_no_lyric_tags(tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_01.flac")
    assert _extract_embedded(audio_path) is None


# ------------------------------------------------------------ service + cache

def _service_for(path):
    local_files = MagicMock()
    local_files.resolve_validated_path = AsyncMock(return_value=path)
    return LocalLyricsService(local_files)


@pytest.mark.asyncio
async def test_service_prefers_sidecar_over_embedded(tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_01.flac")
    audio = FLAC(audio_path)
    audio["LYRICS"] = "embedded text"
    audio.save()
    (tmp_path / "flac_full_01.lrc").write_text("[00:01.00]From sidecar", encoding="utf-8")

    result = await _service_for(audio_path).get_lyrics("file-1")
    assert result.is_synced is True
    assert result.lines[0].text == "From sidecar"


@pytest.mark.asyncio
async def test_service_returns_none_when_nothing_found(tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_01.flac")
    assert await _service_for(audio_path).get_lyrics("file-1") is None


@pytest.mark.asyncio
async def test_service_caches_by_path_and_mtime(tmp_path, monkeypatch):
    audio_path = _copy_fixture(tmp_path, "flac_full_01.flac")
    (tmp_path / "flac_full_01.lrc").write_text("[00:01.00]Cached", encoding="utf-8")
    service = _service_for(audio_path)

    calls = {"n": 0}
    real_extract = LocalLyricsService._extract

    def counting_extract(path):
        calls["n"] += 1
        return real_extract(path)

    monkeypatch.setattr(LocalLyricsService, "_extract", staticmethod(counting_extract))

    first = await service.get_lyrics("file-1")
    second = await service.get_lyrics("file-1")
    assert calls["n"] == 1  # second hit served from cache, no re-read
    assert first == second

    # touching the sidecar invalidates the (path, mtime) key
    stat = (tmp_path / "flac_full_01.lrc").stat()
    os.utime(tmp_path / "flac_full_01.lrc", ns=(stat.st_atime_ns, stat.st_mtime_ns + 10_000_000_000))
    await service.get_lyrics("file-1")
    assert calls["n"] == 2
