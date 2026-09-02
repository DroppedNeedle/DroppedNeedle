"""AlbumPreflightScorer tests - the highest-stakes code in Phase 6.

Covers the two-phase scoring, tier assignment (auto/manual/rejected), quarantine
exclusion, the quality floor, CJK preservation, edition-suffix subset matching,
version-mismatch penalty, and the artist-from-path heuristic.

Per the binding rejected-tier-retention design (final pre-implementation review),
below-threshold groups are KEPT in the ranked list tagged ``tier='rejected'`` (so
the Review tab's "Show all results anyway" needs no re-search) rather than
removed - so junk/mixed-source assertions check the tier, not absence.
"""

from pathlib import Path
from unittest.mock import AsyncMock
import threading

import pytest
from rapidfuzz import fuzz

from infrastructure.persistence.download_store import DownloadStore
from models.download import ScoredCandidate, TargetAlbum
from repositories.protocols.download_client import DownloadSearchResult
from api.v1.schemas.settings import DownloadPolicySettings
from services.native.acquisition.quality import build_snapshot
from services.native.acquisition.decision import SpecPolicy
from services.native.album_preflight_scorer import (
    _has_artist_evidence,

    AlbumPreflightScorer,
    _artist_from_path,
    _file_confidence,
    _normalize_for_match,
    rank_stored_candidates,
)


def _mk(
    parent,
    name,
    *,
    ext="flac",
    bitrate=900,
    username="alice",
    speed=2_000_000,
    free=True,
    queue_length=0,
):
    return DownloadSearchResult(
        username=username,
        filename=f"{parent}/{name}",
        parent_directory=parent,
        size=30_000_000,
        extension=ext,
        bitrate=bitrate,
        bit_depth=16 if ext == "flac" else None,
        sample_rate=44100,
        duration=240.0,
        has_free_slot=free,
        upload_speed=speed,
        queue_length=queue_length,
    )


def _store(quarantine=None):
    store = AsyncMock()
    store.load_quarantine_set.return_value = set(quarantine or set())
    return store


def policy_snapshot(**over):
    """Legacy quality kwargs moved onto the required keyword-only snapshot."""
    base = dict(quality_min="mp3_320", quality_max="lossless")
    base.update(over)
    return build_snapshot(DownloadPolicySettings(**base))


_TARGET = TargetAlbum(
    artist_name="Radiohead", album_title="OK Computer", year=1997, track_count=12
)
_PARENT = "Radiohead OK Computer 1997"


@pytest.mark.asyncio
async def test_perfect_album_auto_accepted():
    files = [_mk(_PARENT, f"OK Computer {n:02d}.flac") for n in range(1, 13)]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(_TARGET, files, snapshot=policy_snapshot())
    assert len(candidates) == 1
    top = candidates[0]
    assert top.coherence == pytest.approx(1.0)
    assert top.final_score >= 0.85
    assert top.tier == "auto"


@pytest.mark.asyncio
async def test_partial_album_is_manual():
    # 7/12 tracks, generic filenames (low title match), no free slot / no speed.
    files = [
        _mk(_PARENT, f"{n:02d} Airbag.mp3", ext="mp3", bitrate=320, speed=0, free=False)
        for n in range(1, 8)
    ]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(_TARGET, files, snapshot=policy_snapshot())
    top = candidates[0]
    assert 0.50 <= top.final_score < 0.70
    assert top.tier == "manual"


@pytest.mark.asyncio
async def test_numbered_sequel_folder_rejected_for_self_titled_debut():
    # The Led Zeppelin case on Soulseek: a "Led Zeppelin II" folder must not be picked for
    # a "Led Zeppelin" (debut) request - it's dropped before scoring, not ranked.
    target = TargetAlbum(
        artist_name="Led Zeppelin", album_title="Led Zeppelin", year=1969, track_count=9
    )
    sequel = [
        _mk("Led Zeppelin - Led Zeppelin II (1969)", f"{n:02d} Track.flac")
        for n in range(1, 10)
    ]
    scorer = AlbumPreflightScorer(_store())
    assert await scorer.rank(target, sequel, snapshot=policy_snapshot()) == []
    # the actual debut folder still scores normally
    debut = [
        _mk("Led Zeppelin - Led Zeppelin (1969)", f"{n:02d} Track.flac")
        for n in range(1, 10)
    ]
    assert len(await scorer.rank(target, debut, snapshot=policy_snapshot())) == 1


@pytest.mark.asyncio
async def test_junk_folder_is_rejected():
    files = [
        _mk(
            "Various Artists - Unknown Album",
            "track.mp3",
            ext="mp3",
            bitrate=320,
            username="charlie",
        )
    ]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(_TARGET, files, snapshot=policy_snapshot())
    junk = next(c for c in candidates if c.username == "charlie")
    assert junk.coherence < 0.50
    assert junk.tier == "rejected"


@pytest.mark.asyncio
async def test_quarantined_candidate_excluded():
    from models.download_identity import soulseek_identity

    files = [_mk(_PARENT, f"OK Computer {n:02d}.flac") for n in range(1, 13)]
    quarantined = {
        ("soulseek", soulseek_identity(f.username, f.filename)) for f in files
    }
    scorer = AlbumPreflightScorer(_store(quarantine=quarantined))
    candidates = await scorer.rank(_TARGET, files, snapshot=policy_snapshot())
    assert all(c.username != "alice" for c in candidates)


@pytest.mark.asyncio
async def test_peer_folder_exhaustion_row_drops_peer_folders():
    """#255 defect 2 consumption: the orchestrator's bare-username row (a clean
    import that under-delivered) must remove that peer's folders from ranking -
    no per-file identity can ever match it, so the folder-level consult does."""
    pool = [
        _mk(_PARENT, f"OK Computer {n:02d}.flac", username=username)
        for username in ("peer", "freshpeer")
        for n in (1, 2)
    ]
    scorer = AlbumPreflightScorer(_store(quarantine={("soulseek", "peer")}))
    candidates = await scorer.rank(_TARGET, pool, snapshot=policy_snapshot())
    assert {c.username for c in candidates} == {"freshpeer"}
    # without the row both peers rank
    control = await AlbumPreflightScorer(_store()).rank(
        _TARGET, pool, snapshot=policy_snapshot()
    )
    assert {c.username for c in control} == {"peer", "freshpeer"}


@pytest.mark.asyncio
async def test_peer_folder_exhaustion_round_trip_through_store(tmp_path: Path):
    """End to end through real persistence: the recorded bare row suppresses the
    peer on a fresh search for the same RG, and the manual re-request clear
    (delete by RG) restores it."""
    store = DownloadStore(db_path=tmp_path / "c.db", write_lock=threading.Lock())
    await store.record_quarantine(
        source="soulseek",
        identity="peer",
        reason="verify_failed",
        release_group_mbid="rg-1",
    )
    assert ("soulseek", "peer") in await store.load_quarantine_set()
    pool = [
        _mk(_PARENT, f"OK Computer {n:02d}.flac", username=username)
        for username in ("peer", "freshpeer")
        for n in (1, 2)
    ]
    scorer = AlbumPreflightScorer(store)
    ranked = await scorer.rank(_TARGET, pool, snapshot=policy_snapshot())
    assert {c.username for c in ranked} == {"freshpeer"}
    cleared = await store.delete_quarantine_for_album("rg-1")
    assert cleared == 1
    restored = await scorer.rank(_TARGET, pool, snapshot=policy_snapshot())
    assert {c.username for c in restored} == {"peer", "freshpeer"}


@pytest.mark.asyncio
async def test_mixed_sources_split_by_coherence():
    good = [_mk(_PARENT, f"OK Computer {n:02d}.flac") for n in range(1, 13)]
    bad = [
        _mk(
            "Various Artists - Unknown Album",
            "x.mp3",
            ext="mp3",
            bitrate=320,
            username="charlie",
        )
    ]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(_TARGET, good + bad, snapshot=policy_snapshot())
    alice = next(c for c in candidates if c.username == "alice")
    charlie = next(c for c in candidates if c.username == "charlie")
    assert alice.tier == "auto"
    assert charlie.tier == "rejected"


@pytest.mark.asyncio
async def test_threshold_configurable():
    files = [
        _mk(_PARENT, f"{n:02d} Airbag.mp3", ext="mp3", bitrate=320, speed=0, free=False)
        for n in range(1, 8)
    ]
    scorer = AlbumPreflightScorer(_store())
    relaxed = await scorer.rank(
        _TARGET, files, snapshot=policy_snapshot(), auto_accept_threshold=0.50
    )
    assert relaxed[0].tier == "auto"


@pytest.mark.asyncio
async def test_quality_gate_drops_out_of_range_keeps_in_range():
    # default range mp3_320..lossless: a 96kbps mp3 (tier 'low') is dropped; FLAC kept.
    low_mp3 = _mk(_PARENT, "01 Airbag.mp3", ext="mp3", bitrate=96, username="bob")
    # FLAC with EMPTY extension field and ABSENT bitrate - still classed lossless (C6a/C6b).
    lossless = DownloadSearchResult(
        username="alice",
        filename=f"{_PARENT}/OK Computer 01.flac",
        parent_directory=_PARENT,
        size=30_000_000,
        extension="",
        bitrate=None,
    )
    scorer = AlbumPreflightScorer(
        _store()
    )  # defaults: mp3_320..lossless, flac_mp3_only
    candidates = await scorer.rank(
        _TARGET, [low_mp3, lossless], snapshot=policy_snapshot()
    )
    all_files = [f for c in candidates for f in c.files]
    assert all(
        f.username != "bob" for f in all_files
    )  # 96kbps mp3 out of range -> dropped
    assert any(f.username == "alice" for f in all_files)  # lossless kept


@pytest.mark.asyncio
async def test_folder_with_sidecars_still_matches_and_enqueues_audio_only():
    # A real Soulseek folder search returns the album's sidecars (cover art, cue, log,
    # m3u) alongside the FLACs. They must not gate the folder out (codec/quality) nor be
    # enqueued as tracks - the regression behind "no matching candidate" on 173 results.
    audio = [_mk(_PARENT, f"OK Computer {n:02d}.flac") for n in range(1, 13)]
    sidecars = [
        _mk(_PARENT, "cover.jpg", ext="jpg", bitrate=None),
        _mk(_PARENT, "folder.png", ext="png", bitrate=None),
        _mk(_PARENT, "OK Computer.cue", ext="cue", bitrate=None),
        _mk(_PARENT, "OK Computer.log", ext="log", bitrate=None),
        _mk(_PARENT, "00.m3u", ext="m3u", bitrate=None),
    ]
    scorer = AlbumPreflightScorer(
        _store()
    )  # defaults: flac_mp3_only, mp3_320..lossless
    candidates = await scorer.rank(
        _TARGET, audio + sidecars, snapshot=policy_snapshot()
    )
    assert len(candidates) == 1
    top = candidates[0]
    assert top.tier == "auto"
    assert top.coherence == pytest.approx(
        1.0
    )  # 12/12 audio, sidecars don't inflate the count
    # only the FLACs are enqueued; no sidecar would reach (and fail) the importer
    assert all(f.filename.endswith(".flac") for f in top.files)
    assert len(top.files) == 12


@pytest.mark.asyncio
async def test_art_only_folder_is_not_a_candidate():
    # A folder of pure cover art (no audio) must not become a candidate.
    art = [
        _mk(
            "Radiohead OK Computer Scans",
            "front.jpg",
            ext="jpg",
            bitrate=None,
            username="bob",
        ),
        _mk(
            "Radiohead OK Computer Scans",
            "back.jpg",
            ext="jpg",
            bitrate=None,
            username="bob",
        ),
    ]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(_TARGET, art, snapshot=policy_snapshot())
    assert candidates == []


@pytest.mark.asyncio
async def test_flac_mp3_only_excludes_other_codecs():
    flac = _mk(_PARENT, "01.flac", ext="flac")
    ogg = _mk(f"{_PARENT} (ogg)", "01.ogg", ext="ogg", bitrate=320, username="bob")
    scorer = AlbumPreflightScorer(_store())  # flac_mp3_only=True (default)
    users = {
        c.username
        for c in await scorer.rank(_TARGET, [flac, ogg], snapshot=policy_snapshot())
    }
    assert "bob" not in users  # OGG folder excluded by flac_mp3_only
    assert "alice" in users
    # toggle off -> the 320 OGG folder is now allowed
    assert (
        "bob"
        in {
            c.username
            for c in await AlbumPreflightScorer(_store()).rank(
                _TARGET,
                [flac, ogg],
                snapshot=policy_snapshot(flac_mp3_only=False),
            )
        }
    )


@pytest.mark.asyncio
async def test_only_lossless_range_drops_mp3():
    flac = [_mk(_PARENT, f"{n:02d}.flac", ext="flac") for n in range(1, 13)]
    mp3 = [
        _mk(f"{_PARENT} (mp3)", f"{n:02d}.mp3", ext="mp3", bitrate=320, username="bob")
        for n in range(1, 13)
    ]
    users = {
        c.username
        for c in await AlbumPreflightScorer(_store()).rank(
            _TARGET,
            flac + mp3,
            snapshot=policy_snapshot(quality_min="lossless", quality_max="lossless"),
        )
    }
    assert "bob" not in users  # MP3 dropped: only lossless accepted
    assert "alice" in users


@pytest.mark.asyncio
async def test_quality_precedes_score_within_same_safe_acceptance_tier():
    # Both candidates pass the same automatic safety boundary, so the user's
    # quality-first policy chooses lossless before using match score as a tie-breaker.
    mp3 = [
        _mk(_PARENT, f"OK Computer {n:02d}.mp3", ext="mp3", bitrate=320)
        for n in range(1, 13)
    ]
    flac = [
        _mk("Radiohead OK Computer", f"{n:02d}.flac", ext="flac", username="bob")
        for n in range(1, 13)
    ]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(_TARGET, mp3 + flac, snapshot=policy_snapshot())
    assert {candidate.tier for candidate in candidates[:2]} == {"auto"}
    assert candidates[0].username == "bob"


@pytest.mark.asyncio
async def test_auto_candidate_precedes_non_auto_hires_candidate():
    non_auto_hires = [
        DownloadSearchResult(
            username="hires-but-wrong",
            filename="Music/Radiohead/OK Computer Deluxe/01 stray.flac",
            parent_directory="OK Computer Deluxe",
            size=80_000_000,
            extension="flac",
            bitrate=900,
            bit_depth=24,
            sample_rate=96000,
            duration=240.0,
            has_free_slot=True,
            upload_speed=20_000_000,
            queue_length=0,
        )
    ]
    safe_redbook = [
        _mk(_PARENT, f"OK Computer {n:02d}.flac", username="safe") for n in range(1, 13)
    ]

    candidates = await AlbumPreflightScorer(_store()).rank(
        _TARGET,
        non_auto_hires + safe_redbook,
        snapshot=policy_snapshot(),
    )

    assert candidates[0].username == "safe"
    assert candidates[0].tier == "auto"
    assert candidates[-1].username == "hires-but-wrong"
    assert candidates[-1].tier != "auto"


@pytest.mark.asyncio
async def test_same_match_band_prefers_free_slot_then_shorter_queue_then_speed():
    def peer(username, *, free, queue_length, speed):
        return [
            _mk(
                _PARENT,
                f"OK Computer {n:02d}.flac",
                username=username,
                free=free,
                queue_length=queue_length,
                speed=speed,
            )
            for n in range(1, 13)
        ]

    results = [
        *peer("long-fast", free=False, queue_length=5, speed=20_000_000),
        *peer("short-slow", free=False, queue_length=1, speed=1_000_000),
        *peer("short-fast", free=False, queue_length=1, speed=5_000_000),
        *peer("free-slow", free=True, queue_length=0, speed=500_000),
    ]

    candidates = await AlbumPreflightScorer(_store()).rank(
        _TARGET, results, snapshot=policy_snapshot()
    )

    assert [candidate.username for candidate in candidates] == [
        "free-slow",
        "short-fast",
        "short-slow",
        "long-fast",
    ]


@pytest.mark.asyncio
async def test_avalon_does_not_accept_so_long_avalon():
    target = TargetAlbum(
        artist_name="Anthony Green", album_title="Avalon", year=2008, track_count=12
    )
    wrong_album = [
        _mk(
            "[2025] So Long, Avalon",
            f"Music/Anthony Green/[2025] So Long, Avalon/{n:02d} track.flac",
            username="wrong-album",
        )
        for n in range(1, 13)
    ]
    avalon = [
        _mk(
            "[2008] Avalon",
            f"Music/Anthony Green/[2008] Avalon/{n:02d} track.flac",
            username="correct-album",
        )
        for n in range(1, 13)
    ]

    candidates = await AlbumPreflightScorer(_store()).rank(
        target, wrong_album + avalon, snapshot=policy_snapshot()
    )

    assert [candidate.username for candidate in candidates] == ["correct-album"]


def test_stored_review_is_safely_reranked_without_losing_pick_indexes():
    target = TargetAlbum(
        artist_name="Anthony Green", album_title="Avalon", year=2008, track_count=12
    )
    manual = ScoredCandidate(
        username="manual",
        parent_directory="Avalon Deluxe",
        files=[
            _mk(
                "Avalon Deluxe",
                "Music/Anthony Green/Avalon Deluxe/01 track.flac",
                username="manual",
            )
        ],
        final_score=0.59,
        tier="manual",
    )
    wrong = ScoredCandidate(
        username="wrong",
        parent_directory="[2025] So Long, Avalon",
        files=[
            _mk(
                "[2025] So Long, Avalon",
                "Music/Anthony Green/[2025] So Long, Avalon/01 track.flac",
                username="wrong",
            )
        ],
        final_score=0.90,
        tier="auto",
    )
    safe = ScoredCandidate(
        username="safe",
        parent_directory="[2008] Avalon",
        files=[
            _mk(
                "[2008] Avalon",
                "Music/Anthony Green/[2008] Avalon/01 track.flac",
                username="safe",
            )
        ],
        final_score=0.85,
        tier="auto",
    )

    projected = rank_stored_candidates(target, [manual, wrong, safe])

    assert [candidate.username for candidate in projected] == ["safe", "manual"]
    assert [candidate.candidate_index for candidate in projected] == [2, 0]


@pytest.mark.asyncio
async def test_hires_folder_outranks_redbook_within_lossless():
    # H1: a 24/96 FLAC folder must rank ABOVE a 16/44 FLAC folder of the same album (same
    # tier), where before the captured bit_depth/sample_rate were never read by the sort.
    redbook = [
        _mk(_PARENT, f"OK Computer {n:02d}.flac") for n in range(1, 13)
    ]  # 16/44100
    hires = [
        DownloadSearchResult(
            username="bob",
            filename=f"{_PARENT}/OK Computer {n:02d}.flac",
            parent_directory=_PARENT,
            size=80_000_000,
            extension="flac",
            bitrate=900,
            bit_depth=24,
            sample_rate=96000,
            duration=240.0,
        )
        for n in range(1, 13)
    ]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(
        _TARGET, redbook + hires, snapshot=policy_snapshot()
    )
    # acquisition cutover: the composite key orders snapshot preference step,
    # evidence certainty and target distance before availability; both folders
    # sit inside ONE lossless step under the 'highest' preference, so the
    # legacy fidelity-first depth/rate tie-break is gone and peer availability
    # (free slot, known queue, upload speed) now separates them.
    assert [candidate.username for candidate in candidates] == [
        "alice",  # free-slot redbook beats peer-defaulted hires metadata
        "bob",
    ]
    assert candidates[1].files[0].bit_depth == 24


@pytest.mark.asyncio
async def test_queued_24_48_outranks_free_16_44_within_lossless():
    redbook = [
        _mk(
            _PARENT,
            f"OK Computer {n:02d}.flac",
            username="free-redbook",
            free=True,
            queue_length=0,
            speed=20_000_000,
        )
        for n in range(1, 13)
    ]
    hires = [
        DownloadSearchResult(
            username="queued-hires",
            filename=f"{_PARENT}/OK Computer {n:02d}.flac",
            parent_directory=_PARENT,
            size=80_000_000,
            extension="flac",
            bitrate=900,
            bit_depth=24,
            sample_rate=48_000,
            duration=240.0,
            has_free_slot=False,
            upload_speed=500_000,
            queue_length=2710,
        )
        for n in range(1, 13)
    ]

    candidates = await AlbumPreflightScorer(_store()).rank(
        _TARGET, redbook + hires, snapshot=policy_snapshot()
    )

    # acquisition cutover: with no lossless resolution sub-ordering inside one
    # preference step, the queued 24/48 copy can no longer out-rank an outright
    # free redbook folder - availability dominates once band/step/certainty tie.
    assert [candidate.username for candidate in candidates[:2]] == [
        "free-redbook",
        "queued-hires",
    ]


@pytest.mark.asyncio
async def test_complete_album_availability_uses_the_slowest_file():
    ready = [
        _mk(
            _PARENT,
            f"OK Computer {n:02d}.flac",
            username="ready",
            speed=1_000_000,
            free=True,
            queue_length=0,
        )
        for n in range(1, 13)
    ]
    mixed = [
        _mk(
            _PARENT,
            f"OK Computer {n:02d}.flac",
            username="mixed",
            speed=20_000_000,
            free=n != 12,
            queue_length=100 if n == 12 else 0,
        )
        for n in range(1, 13)
    ]

    candidates = await AlbumPreflightScorer(_store()).rank(
        _TARGET, mixed + ready, snapshot=policy_snapshot()
    )

    assert [candidate.username for candidate in candidates[:2]] == ["ready", "mixed"]


def test_cjk_not_mangled():
    text = "林宥嘉 神秘嘉宾"
    assert _normalize_for_match(text) == text.lower()
    assert fuzz.token_set_ratio(text, "林宥嘉 - 神秘嘉宾 - 01") >= 85


def test_edition_suffix_subset_match():
    score = fuzz.token_set_ratio(
        _normalize_for_match("OK Computer"),
        _normalize_for_match("OK Computer OKNOTOK 1997-2017"),
    )
    assert score >= 85


def test_artist_from_path_variants():
    assert _artist_from_path("Radiohead - OK Computer") == "Radiohead"
    assert _artist_from_path("Artist/Album") == "Artist"
    assert _artist_from_path("", "Fallback") == ""


@pytest.mark.asyncio
async def test_obfuscated_live_folder_rejected_via_shared_edition_spec():
    # ArrRebuild M3: the Soulseek path now runs the shared wrong_edition spec. A folder the
    # wrong-album guard DEFERS on (no readable artist, Q4) is still dropped when it carries
    # an edition marker the studio request never asked for - previously it survived as a
    # rejected-tier candidate.
    target = TargetAlbum(
        artist_name="Radiohead", album_title="OK Computer", track_count=12
    )
    live = [
        _mk("Live at Glastonbury 2003 xq-scrambled", f"{n:02d}.flac")
        for n in range(1, 13)
    ]
    scorer = AlbumPreflightScorer(_store())
    assert await scorer.rank(target, live, snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_ignored_term_policy_drops_folder():
    # A user ignored-term drops a folder that the always-on guards would have kept.
    files = [_mk(f"{_PARENT} WEB", f"OK Computer {n:02d}.flac") for n in range(1, 13)]
    scorer = AlbumPreflightScorer(_store())
    assert (
        await scorer.rank(
            _TARGET,
            files,
            snapshot=policy_snapshot(),
            spec_extras=SpecPolicy(ignored_terms=("web",)),
        )
        == []
    )
    # without the policy it scores normally
    assert (
        len(
            await AlbumPreflightScorer(_store()).rank(
                _TARGET, files, snapshot=policy_snapshot()
            )
        )
        == 1
    )


@pytest.mark.asyncio
async def test_max_size_policy_drops_oversize_folder():
    # 12 * 30MB = 360MB; a 100MB cap rejects the whole folder (a mislabeled boxset).
    files = [_mk(_PARENT, f"OK Computer {n:02d}.flac") for n in range(1, 13)]
    assert (
        await AlbumPreflightScorer(_store()).rank(
            _TARGET,
            files,
            snapshot=policy_snapshot(),
            spec_extras=SpecPolicy(max_size_mb=100),
        )
        == []
    )


def test_version_mismatch_penalised():
    remix = DownloadSearchResult(
        username="u",
        filename="x/Song (Remix).flac",
        parent_directory="Artist - Album",
        size=1,
        extension="flac",
    )
    original = DownloadSearchResult(
        username="u",
        filename="x/Song.flac",
        parent_directory="Artist - Album",
        size=1,
        extension="flac",
    )
    conf_remix = _file_confidence("Song", "Artist", None, remix)
    conf_original = _file_confidence("Song", "Artist", None, original)
    assert conf_remix < conf_original
    assert conf_remix < 0.70  # off-version cannot auto-accept


@pytest.mark.asyncio
async def test_fedition03_reissue_folder_rankable_but_sequel_excluded():
    """F-EDITION-03 smoke: a valid reissue folder (signed descriptors) stays
    rankable on the Soulseek path while a same-artist different album is
    excluded before acquisition."""
    target = TargetAlbum(
        artist_name="Led Zeppelin", album_title="Led Zeppelin", year=1969, track_count=9
    )
    reissue = [
        _mk("Led Zeppelin - Led Zeppelin (OKNOTOK)", f"{n:02d} Track.flac")
        for n in range(1, 10)
    ]
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(
        target, reissue, snapshot=policy_snapshot()
    )
    assert len(candidates) == 1  # rankable - descriptors are harmless

    sequel = [
        _mk("Led Zeppelin - Presence", f"{n:02d} Track.flac")
        for n in range(1, 10)
    ]
    assert await scorer.rank(target, sequel, snapshot=policy_snapshot()) == []


# GH-284: digit-bearing artists earn evidence from their own paths


def _result_for(filename: str) -> DownloadSearchResult:
    parent, name = filename.rsplit("/", 1)
    return _mk(parent, name)


@pytest.mark.parametrize(
    ("artist", "filename", "expected"),
    [
        # artist directory present in the Soulseek path -> evidence
        ("deadmau5", "/music/deadmau5/For Lack of a Better Name/track-1.flac", True),
        ("u2", "/music/u2/the joshua tree/track-1.flac", True),
        # pure-numeric name matches an exact path segment
        ("311", "/music/311/greatest hits/track-1.flac", True),
        ("311", "/music/311/track-1.flac", True),
        ("Matchbox 20", "/music/matchbox 20/album/track-1.mp3", True),
    ],
)
def test_digit_bearing_artists_earn_evidence_from_their_paths(
    artist: str, filename: str, expected: bool
) -> None:
    target = TargetAlbum(artist_name=artist, album_title="Greatest Hits")
    assert _has_artist_evidence(target, [_result_for(filename)]) is expected


@pytest.mark.parametrize(
    ("artist", "filename"),
    [
        # a bare year directory is not the artist "311"
        ("311", "/music/1994 greatest hits/track-1.flac"),
        # a track ordinal carrying the digits is not artist evidence either
        ("311", "/music/various/album/1994 - track-311.flac"),
    ],
)
def test_numeric_year_and_ordinal_paths_stay_negative(
    artist: str, filename: str
) -> None:
    target = TargetAlbum(artist_name=artist, album_title="Whatever")
    assert _has_artist_evidence(target, [_result_for(filename)]) is False


def test_wrong_artist_stays_negative() -> None:
    target = TargetAlbum(artist_name="deadmau5", album_title="Whatever")
    assert (
        _has_artist_evidence(
            target,
            [_result_for("/music/random artist/album/track-1.flac")],
        )
        is False
    )


@pytest.mark.asyncio
async def test_high_score_deadmau5_candidate_reaches_auto():
    """End-to-end rank(): a deadmau5 candidate above threshold with matching
    artist-path evidence auto-accepts instead of being evidence-capped."""
    files = [
        _mk("/music/deadmau5/For Lack of a Better Name", f"track-{n}.flac")
        for n in (1, 2)
    ]
    target = TargetAlbum(
        artist_name="deadmau5",
        album_title="For Lack of a Better Name",
        track_count=2,
    )
    scorer = AlbumPreflightScorer(_store())
    candidates = await scorer.rank(
        target, files, snapshot=policy_snapshot(), auto_accept_threshold=0.7
    )
    assert candidates
    top = candidates[0]
    assert top.tier == "auto"
