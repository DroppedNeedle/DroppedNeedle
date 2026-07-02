"""TrackMatcher tests - per-track scoring, no group phase, quarantine + floor."""

from unittest.mock import AsyncMock

import pytest

from models.download import TargetTrack
from repositories.protocols.download_client import DownloadSearchResult
from services.native.track_matcher import TrackMatcher


def _store(quarantine=None):
    store = AsyncMock()
    store.load_quarantine_set.return_value = set(quarantine or set())
    return store


def _file(filename, parent, *, ext="flac", bitrate=900, duration=284.0, username="alice"):
    return DownloadSearchResult(
        username=username,
        filename=filename,
        parent_directory=parent,
        size=20_000_000,
        extension=ext,
        bitrate=bitrate,
        duration=duration,
    )


_TARGET = TargetTrack(artist_name="Radiohead", track_title="Airbag", duration_seconds=284.0)


@pytest.mark.asyncio
async def test_match_returns_single_file_candidate():
    results = [_file("Radiohead - OK Computer/Airbag.flac", "Radiohead - OK Computer")]
    matcher = TrackMatcher(_store())
    candidate = await matcher.match(_TARGET, results)
    assert candidate is not None
    assert len(candidate.files) == 1
    assert candidate.tier == "auto"
    assert candidate.final_score >= 0.70


@pytest.mark.asyncio
async def test_match_no_results_returns_none():
    matcher = TrackMatcher(_store())
    assert await matcher.match(_TARGET, []) is None


@pytest.mark.asyncio
async def test_match_excludes_quarantined():
    from models.download_identity import soulseek_identity

    results = [_file("Radiohead - OK Computer/Airbag.flac", "Radiohead - OK Computer")]
    quarantined = {("soulseek", soulseek_identity(results[0].username, results[0].filename))}
    matcher = TrackMatcher(_store(quarantine=quarantined))
    assert await matcher.match(_TARGET, results) is None


@pytest.mark.asyncio
async def test_match_picks_highest_confidence():
    good = _file("Radiohead - OK Computer/Airbag.flac", "Radiohead - OK Computer")
    wrong = _file("Misc/Some Other Track.flac", "Misc", duration=120.0)
    matcher = TrackMatcher(_store())
    candidate = await matcher.match(_TARGET, [wrong, good])
    assert candidate.files[0].filename == good.filename


@pytest.mark.asyncio
async def test_match_flac_mp3_only_excludes_other_codecs():
    ogg = _file("Artist/Airbag.ogg", "Artist", ext="ogg", bitrate=320)
    assert await TrackMatcher(_store()).match(_TARGET, [ogg]) is None  # default: flac_mp3_only
    assert await TrackMatcher(_store(), flac_mp3_only=False).match(_TARGET, [ogg]) is not None


@pytest.mark.asyncio
async def test_match_only_lossless_drops_mp3():
    mp3 = _file("Radiohead - OK Computer/Airbag.mp3", "Radiohead - OK Computer", ext="mp3", bitrate=320)
    matcher = TrackMatcher(_store(), quality_min="lossless", quality_max="lossless")
    assert await matcher.match(_TARGET, [mp3]) is None


@pytest.mark.asyncio
async def test_match_prefers_flac_over_better_matched_mp3():
    mp3 = _file("Radiohead - OK Computer/Airbag.mp3", "Radiohead - OK Computer", ext="mp3", bitrate=320)
    flac = _file("OKC/Airbag.flac", "OKC", ext="flac", username="bob")
    candidate = await TrackMatcher(_store()).match(_TARGET, [mp3, flac])
    assert candidate is not None
    assert candidate.files[0].username == "bob"  # FLAC tier wins absolutely


@pytest.mark.asyncio
async def test_rank_held_tier_floor_keeps_only_strictly_better():
    """Per-track upgrade floor (D12): with held_tier set, only files STRICTLY above
    the recording's held tier survive - an equal-tier copy is never a wasted grab."""
    results = [
        _file("A/Airbag.flac", "A", ext="flac", bitrate=900, username="flac-peer"),
        _file("B/Airbag.mp3", "B", ext="mp3", bitrate=320, username="mp3320-peer"),
        _file("C/Airbag.mp3", "C", ext="mp3", bitrate=192, username="mp3192-peer"),
    ]
    matcher = TrackMatcher(_store(), quality_min="low")

    ranked = await matcher.rank(_TARGET, results, held_tier="mp3_320")

    assert [c.username for c in ranked] == ["flac-peer"]  # only lossless beats mp3_320

    # no floor (not an upgrade run): everything in range still ranks
    ranked = await matcher.rank(_TARGET, results)
    assert {c.username for c in ranked} == {"flac-peer", "mp3320-peer", "mp3192-peer"}
