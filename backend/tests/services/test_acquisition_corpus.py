"""Replay regression corpus (P6, 2026-07-05 wrong-single incident).

Fixtures under ``tests/fixtures/acquisition_corpus/`` are REAL (anonymised) search
results extracted from this instance's ``search_jobs`` blobs: both incident search
jobs (every candidate a wrong-artist "arrival" token match), the legitimate
soulseek album picks that completed correctly (the lowest-scoring of which - Boards
of Canada "Inferno" at 0.801 - is the calibration floor for any scorer change),
a pre-P1 single, and a 2-track EP case.

The invariant this file pins: **no scorer change may auto-accept an incident
candidate, and every legitimate prior must keep auto-accepting.** Run via
``make test-acquisition-corpus``. When a scorer change fails a legit case here,
the change is wrong - not the corpus.
"""

import json
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from models.download import TargetAlbum, TargetTrack
from repositories.protocols.download_client import DownloadSearchResult
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.track_matcher import TrackMatcher

_CORPUS_DIR = Path(__file__).resolve().parent.parent / "fixtures" / "acquisition_corpus"
_CASES = sorted(_CORPUS_DIR.glob("*.json"))


def _load(path: Path) -> dict:
    return json.loads(path.read_text())


def _results(case: dict) -> list[DownloadSearchResult]:
    return [
        DownloadSearchResult(
            username=f["username"],
            filename=f["filename"],
            parent_directory=f["parent_directory"],
            size=f["size"],
            extension=f["extension"] or "",
            bitrate=f["bitrate"],
            bit_depth=f.get("bit_depth"),
            sample_rate=f.get("sample_rate"),
            duration=f.get("duration"),
            has_free_slot=f["has_free_slot"],
            upload_speed=f["upload_speed"],
        )
        for group in case["groups"]
        for f in group["files"]
    ]


def _store():
    store = AsyncMock()
    store.load_quarantine_set.return_value = set()
    return store


async def _rank_album(case: dict):
    target = case["target"]
    scorer = AlbumPreflightScorer(_store())
    return await scorer.rank(
        TargetAlbum(
            artist_name=target["artist_name"],
            album_title=target["album_title"],
            year=target["year"],
            track_count=target["track_count"],
        ),
        _results(case),
    )


async def _rank_single(case: dict):
    target = case["target"]
    matcher = TrackMatcher(_store())
    return await matcher.rank(
        TargetTrack(
            artist_name=target["artist_name"],
            track_title=target.get("track_title") or target["album_title"],
            album_title=target["album_title"],
            duration_seconds=target.get("duration_seconds"),
            recording_mbid=target.get("recording_mbid"),
        ),
        _results(case),
    )


def _by_parent(ranked, parent):
    matches = [c for c in ranked if c.parent_directory == parent]
    assert matches, f"picked candidate {parent!r} missing from ranked output"
    return matches[0]


@pytest.mark.asyncio
@pytest.mark.parametrize("path", _CASES, ids=lambda p: p.stem)
async def test_corpus_case(path: Path):
    case = _load(path)
    expect = case["expect"]

    ranked = await (_rank_single(case) if case["kind"] == "single" else _rank_album(case))
    assert ranked, f"{case['name']}: scorer returned no candidates"

    if expect.get("no_auto"):
        autos = [c for c in ranked if c.tier == "auto"]
        assert autos == [], (
            f"{case['name']}: wrong-content candidates reached tier=auto: "
            f"{[(c.parent_directory, round(c.final_score, 3)) for c in autos]}"
        )
    if expect.get("top_auto"):
        assert ranked[0].tier == "auto", (
            f"{case['name']}: top candidate no longer auto "
            f"(tier={ranked[0].tier}, score={ranked[0].final_score:.3f}) - "
            "the autopick path would park a previously-automatic download"
        )
    if expect.get("picked_tier"):
        picked = _by_parent(ranked, expect["picked_parent"])
        assert picked.tier == expect["picked_tier"], (
            f"{case['name']}: the historically-picked candidate is now "
            f"tier={picked.tier} (score={picked.final_score:.3f})"
        )
    if expect.get("picked_not_rejected"):
        picked = _by_parent(ranked, expect["picked_parent"])
        assert picked.tier != "rejected", (
            f"{case['name']}: the historically-picked candidate is now rejected"
        )

    # The MB-degraded fallback: a single whose identity threading failed scores
    # through the ALBUM scorer - the incident class must not auto there either.
    if expect.get("album_no_auto"):
        album_ranked = await _rank_album(case)
        autos = [c for c in album_ranked if c.tier == "auto"]
        assert autos == [], (
            f"{case['name']}: album-scorer fallback autos wrong content: "
            f"{[(c.parent_directory, round(c.final_score, 3)) for c in autos]}"
        )
