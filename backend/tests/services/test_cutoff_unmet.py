"""Cutoff-unmet detection (CollectionManagement Feature B §4).

The one real correctness trap: the SQL ``_TIER_RANK_CASE`` must rank every
(format, bitrate) band exactly like ``quality_tiers.tier_for`` - a drifted band
would silently list satisfied albums (wasted grabs) or hide upgradable ones.
"""

import threading
from pathlib import Path

import pytest

from infrastructure.persistence.library_db import LibraryDB
from models.audio import AudioInfo, AudioTag
from services.native.library_manager import LibraryManager
from services.native.quality_tiers import tier_for

_FORMATS = ["flac", "alac", "wav", "ape", "wv", "mp3", "m4a", "ogg", "opus", "FLAC", ""]
_BITRATES = [0, 64, 191, 192, 255, 256, 319, 320, 321, 1411]


@pytest.fixture
def manager(tmp_path: Path) -> LibraryManager:
    db = LibraryDB(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    return LibraryManager(db)


def _tag(rg: str, track: int = 1) -> AudioTag:
    return AudioTag(
        title=f"Track {track}",
        artist="Artist",
        album=f"Album {rg}",
        album_artist="Artist",
        track_number=track,
        disc_number=1,
        musicbrainz_release_group_id=rg,
    )


def _info(file_format: str, bitrate: int) -> AudioInfo:
    return AudioInfo(
        duration_seconds=200.0,
        bitrate=bitrate,
        sample_rate=44100,
        channels=2,
        file_format=file_format,
        file_size_bytes=1000,
    )


async def _add_file(manager, path: str, rg: str, fmt: str, bitrate: int, track: int = 1):
    await manager.upsert_file(
        Path(path), _tag(rg, track), _info(fmt, bitrate),
        release_group_mbid=rg, recording_mbid=f"rec-{rg}-{track}",
    )


@pytest.mark.asyncio
async def test_sql_case_matches_tier_for_on_every_band(manager: LibraryManager):
    """THE correctness-trap test: for the full (format, bitrate) grid, the SQL rank
    must agree with tier_for - an album is listed iff tier_for says it's below the
    cutoff, and its reported current_tier must be tier_for's answer."""
    expected: dict[str, str] = {}
    for i, fmt in enumerate(_FORMATS):
        for j, bitrate in enumerate(_BITRATES):
            rg = f"rg-{i}-{j}"
            await _add_file(manager, f"/music/{rg}/01.x", rg, fmt, bitrate)
            expected[rg] = tier_for(fmt, bitrate)

    listed = {row["release_group_mbid"]: row["current_tier"]
              for row in await manager.list_cutoff_unmet("lossless")}

    for rg, tier in expected.items():
        if tier == "lossless":
            assert rg not in listed, f"{rg} ({tier}) wrongly listed as cutoff-unmet"
        else:
            assert listed.get(rg) == tier, (
                f"{rg}: SQL said {listed.get(rg)!r}, tier_for said {tier!r}"
            )


@pytest.mark.asyncio
async def test_album_is_rated_by_its_worst_track(manager: LibraryManager):
    await _add_file(manager, "/music/a/01.flac", "rg-worst", "flac", 900, track=1)
    await _add_file(manager, "/music/a/02.mp3", "rg-worst", "mp3", 192, track=2)

    rows = await manager.list_cutoff_unmet("lossless")

    assert len(rows) == 1
    row = rows[0]
    assert row["release_group_mbid"] == "rg-worst"
    assert row["current_tier"] == "mp3_192"
    assert row["track_count"] == 2


@pytest.mark.asyncio
async def test_cutoff_boundary_at_tier_is_satisfied(manager: LibraryManager):
    await _add_file(manager, "/music/b/01.mp3", "rg-at", "mp3", 320)
    await _add_file(manager, "/music/c/01.mp3", "rg-below", "mp3", 256)

    listed = {r["release_group_mbid"] for r in await manager.list_cutoff_unmet("mp3_320")}

    assert "rg-at" not in listed  # at the cutoff = satisfied, never upgraded past it
    assert "rg-below" in listed


@pytest.mark.asyncio
async def test_soft_deleted_files_do_not_count(manager: LibraryManager):
    path = "/music/d/01.mp3"
    await _add_file(manager, path, "rg-del", "mp3", 128)
    # soft-delete must match the stored path exactly (str(Path(...)) uses "\" on Windows)
    await manager._db.soft_delete_library_file(str(Path(path)))

    assert await manager.list_cutoff_unmet("lossless") == []
