"""Integration tests for LibraryScanner.

Scans a temp directory of real fixture audio files with a real ``AudioTagger``
and a real ``LibraryManager``/``LibraryDB`` (on a tmp sqlite db), while mocking
the two network boundaries - ``MusicBrainzMatcher`` (Tier 2/3) and
``AudioFingerprinter`` (Tier 3) - to drive each tier deterministically.
"""

import os
import shutil
import threading
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.persistence.scan_state_store import ScanStateStore
from infrastructure.sse_publisher import SSEPublisher
from core.exceptions import ResourceNotFoundError, ValidationError
from models.audio import AudioTag, FingerprintResult
from services.native.library_manager import LibraryManager
from services.native.library_scanner import LibraryScanner, _LEDGER_BATCH_SIZE
from services.native.musicbrainz_matcher import MatchResult

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "library"

_RG_AIRBAG = "b1392450-e666-3926-a536-22c65f834433"  # flac_full_01/02 release group
_RG_MEZZANINE = "rg-mezzanine-0001"  # m4a_full_01 release group


@pytest.fixture(autouse=True)
def _fast_artist_backoff(monkeypatch):
    monkeypatch.setattr("services.native.library_scanner._ARTIST_BREAKER_WAIT_S", 0.0)


def _build(tmp_path, *, text_match=None, fingerprint=None, resolve=None, album_match=None):
    lock = threading.Lock()
    db_path = tmp_path / "library.db"
    db = LibraryDB(db_path=db_path, write_lock=lock)
    manager = LibraryManager(db)
    state = ScanStateStore(db_path=db_path, write_lock=lock)  # AUD-5: same db + lock
    mb = AsyncMock()
    mb.text_match = AsyncMock(return_value=text_match or MatchResult(confidence=0.0))
    mb.resolve_recording_to_release_group = AsyncMock(return_value=resolve)
    fp = AsyncMock()
    fp.fingerprint = AsyncMock(return_value=fingerprint or FingerprintResult(status="disabled"))
    album = AsyncMock()
    album.identify = AsyncMock(return_value=album_match)
    album.resolve_release_group_artist = AsyncMock(return_value=(None, None))
    scanner = LibraryScanner(AudioTagger(), fp, mb, album, manager, state, SSEPublisher())
    return scanner, manager, state, db


def _music_dir(tmp_path, *names):
    music = tmp_path / "music"
    music.mkdir(exist_ok=True)
    paths = []
    for i, name in enumerate(names):
        dest = music / f"{i:02d}_{name}"
        shutil.copy(FIXTURES / name, dest)
        paths.append(dest)
    return music, paths


# -- tier transitions --


@pytest.mark.asyncio
async def test_tier1_mbids_in_tags(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac")
    await scanner.scan([music])
    rows = await db.get_library_files_for_album(_RG_AIRBAG)
    assert len(rows) == 1
    assert rows[0]["confidence"] == 1.0
    assert rows[0]["recording_mbid"] == "rec-airbag-0001"
    assert (await state.get_state())["status"] == "idle"


@pytest.mark.asyncio
async def test_tier2_text_match(tmp_path):
    scanner, manager, state, db = _build(
        tmp_path, text_match=MatchResult(confidence=0.92, release_group_mbid="rg-text")
    )
    music, _ = _music_dir(tmp_path, "flac_only_release_mbid.flac")  # has artist+album, no RG tag
    await scanner.scan([music])
    rows = await db.get_library_files_for_album("rg-text")
    assert len(rows) == 1
    assert rows[0]["confidence"] == 0.92
    assert rows[0]["source"] == "scan"


@pytest.mark.asyncio
async def test_tier3_fingerprint_resolution(tmp_path):
    scanner, manager, state, db = _build(
        tmp_path,
        fingerprint=FingerprintResult(status="pass", score=0.9, recording_id="rec-fp"),
        resolve="rg-fp",
    )
    music, _ = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    await scanner.scan([music])
    rows = await db.get_library_files_for_album("rg-fp")
    assert len(rows) == 1
    assert rows[0]["recording_mbid"] == "rec-fp"
    assert rows[0]["confidence"] == 0.9


@pytest.mark.asyncio
async def test_tier4_manual_review_text_source(tmp_path):
    scanner, manager, state, db = _build(tmp_path)  # no text match, fingerprint disabled
    music, paths = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    await scanner.scan([music])
    unmatched = await manager.get_unmatched()
    assert len(unmatched) == 1
    assert unmatched[0].file_path == str(paths[0])
    assert unmatched[0].source == "text_match"  # fingerprint disabled -> not attempted
    assert not await db.has_any_files()


@pytest.mark.asyncio
async def test_tier4_source_acoustid_when_fingerprint_attempted(tmp_path):
    scanner, manager, state, db = _build(
        tmp_path, fingerprint=FingerprintResult(status="skip", score=0.4)
    )
    music, _ = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    await scanner.scan([music])
    unmatched = await manager.get_unmatched()
    assert unmatched[0].source == "acoustid"
    assert unmatched[0].fingerprint_score == 0.4


@pytest.mark.asyncio
async def test_tier4_fingerprint_fail_status_is_acoustid_source(tmp_path):
    # A 'fail' verdict (confident audio, no recording id) still counts as an
    # AcoustID attempt for manual-review triage.
    scanner, manager, state, db = _build(
        tmp_path, fingerprint=FingerprintResult(status="fail", score=0.92)
    )
    music, _ = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    await scanner.scan([music])
    unmatched = await manager.get_unmatched()
    assert unmatched[0].source == "acoustid"
    assert unmatched[0].fingerprint_score == 0.92


@pytest.mark.asyncio
async def test_tier4_error_status_is_text_match_source(tmp_path):
    # An fpcalc/HTTP 'error' must NOT masquerade as an AcoustID no-match.
    scanner, manager, state, db = _build(
        tmp_path, fingerprint=FingerprintResult(status="error", error="fpcalc gone")
    )
    music, _ = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    await scanner.scan([music])
    unmatched = await manager.get_unmatched()
    assert unmatched[0].source == "text_match"


@pytest.mark.asyncio
async def test_manual_review_candidate_mbids_round_trip(tmp_path):
    # Confident fingerprint but unresolvable release group -> Tier 4 with the
    # recording id as a candidate; the API exposes a decoded list, not a blob.
    scanner, manager, state, db = _build(
        tmp_path,
        fingerprint=FingerprintResult(status="pass", score=0.95, recording_id="rec-xyz"),
        resolve=None,
    )
    music, _ = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    await scanner.scan([music])
    unmatched = await manager.get_unmatched()
    assert len(unmatched) == 1
    assert unmatched[0].candidate_mbids == ["rec-xyz"]
    assert not hasattr(unmatched[0], "candidate_mbids_encoded")


# -- full scan, SSE, lifecycle --


@pytest.mark.asyncio
async def test_full_scan_row_count_and_sse_events(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(
        tmp_path, "flac_full_01.flac", "flac_full_02.flac", "m4a_full_01.m4a"
    )
    await scanner.scan([music])
    stats = await manager.get_stats()
    assert stats.total_tracks == 3
    assert stats.total_albums == 2  # flac_full_01/02 share a release group
    latest = scanner._events._latest["library:scan"]
    assert {"started", "progress", "complete"} <= set(latest)
    assert latest["started"]["total"] == 3
    assert (await state.get_state())["status"] == "idle"


@pytest.mark.asyncio
async def test_scan_failure_sets_error_status_and_emits_failed(tmp_path):
    # A failure inside the scan (here: reconcile blows up) must fail closed -
    # status -> 'error' and a terminal 'failed' SSE event - never crash the loop.
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac")
    scanner._library.reconcile_with_filesystem = AsyncMock(side_effect=RuntimeError("boom"))
    await scanner.scan([music])
    assert (await state.get_state())["status"] == "error"
    assert "failed" in scanner._events._latest["library:scan"]


@pytest.mark.asyncio
async def test_cancel_stops_scan(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(
        tmp_path,
        "flac_only_release_mbid.flac",
        "flac_only_release_mbid.flac",
        "flac_only_release_mbid.flac",
    )
    calls = {"n": 0}

    async def cancel_after_first(_target):
        calls["n"] += 1
        if calls["n"] == 1:
            scanner.request_cancel()
        return MatchResult(confidence=0.92, release_group_mbid="rg-x")

    scanner._mb_matcher.text_match = AsyncMock(side_effect=cancel_after_first)
    await scanner.scan([music])
    assert (await state.get_state())["status"] == "cancelled"
    rows = await db.get_library_files_for_album("rg-x")
    assert len(rows) == 1  # only the first file processed before cancellation


@pytest.mark.asyncio
async def test_resume_skips_ledgered_paths(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, paths = _music_dir(tmp_path, "flac_full_01.flac", "m4a_full_01.m4a")
    # Seed an interrupted scan: flac_full_01 already processed (ledgered).
    await state.start(total_files=2)
    await state.advance([str(paths[0])], processed=1, matched=1, failed=0)
    await scanner.scan([music], resume=True)
    assert not await db.has_album_files(_RG_AIRBAG)  # skipped - never processed
    assert await db.has_album_files(_RG_MEZZANINE)  # the remainder
    assert (await state.get_state())["status"] == "idle"


@pytest.mark.asyncio
async def test_startup_check_resumes_when_scanning(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac")
    await state.start(total_files=1)  # status = scanning, empty ledger
    await scanner.startup_check([music])
    assert await db.has_album_files(_RG_AIRBAG)
    assert (await state.get_state())["status"] == "idle"


@pytest.mark.asyncio
async def test_startup_check_noop_when_idle(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac")
    await scanner.startup_check([music])  # status idle -> no scan kicked off
    assert not await db.has_any_files()


@pytest.mark.asyncio
async def test_incremental_second_scan_skips_unchanged(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac", "m4a_full_01.m4a")
    await scanner.scan([music])

    calls = {"n": 0}
    original = manager.upsert_file

    async def spy(*args, **kwargs):
        calls["n"] += 1
        return await original(*args, **kwargs)

    manager.upsert_file = spy
    await scanner.scan([music])
    assert calls["n"] == 0  # nothing changed -> nothing re-upserted


@pytest.mark.asyncio
async def test_force_rescan_reidentifies_unchanged(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac", "m4a_full_01.m4a")
    await scanner.scan([music])

    calls = {"n": 0}
    original = manager.upsert_file

    async def spy(*args, **kwargs):
        calls["n"] += 1
        return await original(*args, **kwargs)

    manager.upsert_file = spy
    await scanner.scan([music], force=True)
    assert calls["n"] > 0  # force bypasses the unchanged-file skip -> files re-upserted


@pytest.mark.asyncio
async def test_scan_enriches_untagged_file_from_path(tmp_path):
    scanner, manager, _state, db = _build(
        tmp_path, text_match=MatchResult(confidence=0.95, release_group_mbid="rg-fn")
    )
    album_dir = tmp_path / "music" / "Trapeze" / "Trapeze - Hot Wire"
    album_dir.mkdir(parents=True)
    shutil.copy(
        FIXTURES / "flac_no_tags.flac", album_dir / "Trapeze - Hot Wire - 05 - Turn It On.flac"
    )

    await scanner.scan([tmp_path / "music"])

    target = scanner._mb_matcher.text_match.call_args.args[0]
    assert target.artist == "Trapeze"
    assert target.album == "Hot Wire"
    assert await db.has_album_files("rg-fn")


def _album_dir(tmp_path, folder, count, fixture="flac_only_release_mbid.flac"):
    album_dir = tmp_path / "music" / folder
    album_dir.mkdir(parents=True)
    paths = []
    for i in range(count):
        dest = album_dir / f"{i + 1:02d} track.flac"
        shutil.copy(FIXTURES / fixture, dest)
        paths.append(dest)
    return paths


@pytest.mark.asyncio
async def test_album_match_imports_whole_folder(tmp_path):
    from services.native.album_matcher import AlbumMatch

    scanner, manager, state, db = _build(tmp_path)
    _album_dir(tmp_path, "Santana/Santana (1969)", 2)

    async def identify(locals_, *, seed_release_groups=None):
        return AlbumMatch(
            accepted=True, distance=0.05,
            release_group_mbid="rg-album", release_mbid="rel-album",
            assignments={lt.path: f"rec-{i}" for i, lt in enumerate(locals_)},
        )

    scanner._album_identifier.identify = AsyncMock(side_effect=identify)
    await scanner.scan([tmp_path / "music"])

    rows = await db.get_library_files_for_album("rg-album")
    assert len(rows) == 2
    assert {r["recording_mbid"] for r in rows} == {"rec-0", "rec-1"}
    scanner._mb_matcher.text_match.assert_not_awaited()


@pytest.mark.asyncio
async def test_album_match_claims_unmapped_files_under_the_album(tmp_path):
    # Phase 2 (all-or-nothing): a file the matched release didn't map to a track is kept
    # under the ONE album (blank recording), NOT scattered to a per-file guess.
    from services.native.album_matcher import AlbumMatch

    scanner, manager, state, db = _build(
        tmp_path, text_match=MatchResult(confidence=0.95, release_group_mbid="rg-bonus"),
    )
    _album_dir(tmp_path, "Artist/Album", 2)

    async def identify(locals_, *, seed_release_groups=None):
        return AlbumMatch(
            accepted=True, distance=0.05,
            release_group_mbid="rg-album", release_mbid="rel-album",
            assignments={locals_[0].path: "rec-0"},  # only 1 of 2 mapped
        )

    scanner._album_identifier.identify = AsyncMock(side_effect=identify)
    await scanner.scan([tmp_path / "music"])

    rows = await db.get_library_files_for_album("rg-album")
    assert len(rows) == 2  # BOTH files under the one album
    assert "rec-0" in {r["recording_mbid"] for r in rows}  # the mapped one keeps its track
    assert await db.get_library_files_for_album("rg-bonus") == []  # never scattered
    scanner._mb_matcher.text_match.assert_not_awaited()  # no per-file fallback for an album


@pytest.mark.asyncio
async def test_album_match_skipped_for_single_file_folder(tmp_path):
    scanner, manager, state, db = _build(
        tmp_path, text_match=MatchResult(confidence=0.95, release_group_mbid="rg-solo"),
    )
    _album_dir(tmp_path, "Artist/Single", 1)
    await scanner.scan([tmp_path / "music"])

    scanner._album_identifier.identify.assert_not_awaited()
    assert await db.has_album_files("rg-solo")


@pytest.mark.asyncio
async def test_reconcile_album_artists_collapses_name_variants(tmp_path):
    from models.audio import AudioInfo
    from unittest.mock import AsyncMock as _AM

    scanner, manager, state, db = _build(tmp_path)
    info = AudioInfo(
        duration_seconds=200.0, bitrate=900, sample_rate=44100, channels=2,
        file_format="flac", file_size_bytes=1000,
    )
    for i, name in enumerate(["BOL4", "Bolbbalgan4"]):
        tag = AudioTag(title=f"t{i}", artist=name, album="Galaxy", track_number=i + 1, album_artist=name)
        await manager.upsert_file(
            tmp_path / f"{i}.flac", tag, info,
            release_group_mbid="rg-bol4", recording_mbid=f"rec{i}", confidence=1.0, source="scan",
        )
    assert (await db.get_library_stats())["total_artists"] == 2

    scanner._album_identifier.resolve_release_group_artist = _AM(return_value=("mbid-bol4", "볼빨간사춘기"))
    await scanner._reconcile_album_artists()

    assert (await db.get_library_stats())["total_artists"] == 1
    artists, _total = await db.get_artists_aggregated()
    assert [a["artist_name"] for a in artists] == ["볼빨간사춘기"]


@pytest.mark.asyncio
async def test_get_artists_aggregated_sort_and_search(tmp_path):
    from models.audio import AudioInfo

    scanner, manager, state, db = _build(tmp_path)
    info = AudioInfo(
        duration_seconds=200.0, bitrate=900, sample_rate=44100, channels=2,
        file_format="flac", file_size_bytes=1000,
    )
    seed = [("Alpha", "rg-a1"), ("Alpha", "rg-a2"), ("Beta", "rg-b1"), ("Gamma", "rg-g1")]
    for i, (name, rg) in enumerate(seed):
        tag = AudioTag(title=f"t{i}", artist=name, album=rg, track_number=1, album_artist=name)
        await manager.upsert_file(
            tmp_path / f"{i}.flac", tag, info,
            release_group_mbid=rg, recording_mbid=f"rec{i}", confidence=1.0, source="scan",
        )

    by_name, total = await db.get_artists_aggregated(sort_by="name", sort_order="asc")
    assert total == 3
    assert [a["artist_name"] for a in by_name] == ["Alpha", "Beta", "Gamma"]
    desc, _ = await db.get_artists_aggregated(sort_by="name", sort_order="desc")
    assert [a["artist_name"] for a in desc] == ["Gamma", "Beta", "Alpha"]

    by_albums, _ = await db.get_artists_aggregated(sort_by="album_count", sort_order="desc")
    assert by_albums[0]["artist_name"] == "Alpha"
    assert by_albums[0]["album_count"] == 2

    found, found_total = await db.get_artists_aggregated(q="Bet")
    assert found_total == 1
    assert [a["artist_name"] for a in found] == ["Beta"]
    assert found[0]["date_added"] is not None

    page2, _ = await db.get_artists_aggregated(limit=2, offset=2, sort_by="name")
    assert [a["artist_name"] for a in page2] == ["Gamma"]


async def _seed_unresolved(manager, tmp_path, name="BOL4", rg="rg-x", fname="x.flac"):
    from models.audio import AudioInfo

    info = AudioInfo(
        duration_seconds=200.0, bitrate=900, sample_rate=44100, channels=2,
        file_format="flac", file_size_bytes=1000,
    )
    tag = AudioTag(title="t", artist=name, album="A", track_number=1, album_artist=name)
    await manager.upsert_file(
        tmp_path / fname, tag, info, release_group_mbid=rg, recording_mbid="r",
        confidence=1.0, source="scan",
    )


@pytest.mark.asyncio
async def test_reconcile_emits_finalizing_progress(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    await _seed_unresolved(manager, tmp_path, name="A", rg="rg-a", fname="a.flac")
    await _seed_unresolved(manager, tmp_path, name="B", rg="rg-b", fname="b.flac")
    events: list[tuple[str, dict]] = []
    original = scanner._events.publish

    async def spy(channel, event, data):
        events.append((event, data))
        return await original(channel, event, data)

    scanner._events.publish = spy
    scanner._album_identifier.resolve_release_group_artist = AsyncMock(
        return_value=("mbid-x", "Artist X")
    )
    await scanner._reconcile_album_artists()

    finals = [d for (e, d) in events if e == "finalizing"]
    assert finals, "expected a finalizing event"
    assert finals[0]["total"] == 2 and finals[0]["remaining"] == 2
    assert any(d["remaining"] == 0 for d in finals)


@pytest.mark.asyncio
async def test_reconcile_retries_until_resolved(tmp_path, monkeypatch):
    monkeypatch.setattr("services.native.library_scanner._ARTIST_BREAKER_WAIT_S", 0.0)
    scanner, manager, state, db = _build(tmp_path)
    await _seed_unresolved(manager, tmp_path)
    calls = {"n": 0}

    async def flaky(_rg):
        calls["n"] += 1
        return (None, None) if calls["n"] == 1 else ("mbid-x", "볼빨간사춘기")

    scanner._album_identifier.resolve_release_group_artist = AsyncMock(side_effect=flaky)
    assert await scanner._reconcile_album_artists() is True
    assert calls["n"] >= 2
    assert (await db.get_library_stats())["total_artists"] == 1


@pytest.mark.asyncio
async def test_reconcile_reports_musicbrainz_down(tmp_path, monkeypatch):
    monkeypatch.setattr("services.native.library_scanner._ARTIST_BREAKER_WAIT_S", 0.0)
    scanner, manager, state, db = _build(tmp_path)
    await _seed_unresolved(manager, tmp_path)
    scanner._album_identifier.resolve_release_group_artist = AsyncMock(return_value=(None, None))
    assert await scanner._reconcile_album_artists() is False


@pytest.mark.asyncio
async def test_reconcile_skips_compilations(tmp_path):
    from models.audio import AudioInfo

    scanner, manager, state, db = _build(tmp_path)
    info = AudioInfo(
        duration_seconds=200.0, bitrate=900, sample_rate=44100, channels=2,
        file_format="flac", file_size_bytes=1000,
    )
    tag = AudioTag(title="x", artist="Someone", album="Comp", track_number=1, compilation=True)
    await manager.upsert_file(
        tmp_path / "c.flac", tag, info, release_group_mbid="rg-va", recording_mbid="r", confidence=1.0, source="scan",
    )
    assert await scanner._library.get_release_groups_needing_artist() == []


@pytest.mark.asyncio
async def test_scan_prunes_review_rows_for_now_matched_files(tmp_path):
    scanner, manager, _state, db = _build(
        tmp_path, text_match=MatchResult(confidence=0.95, release_group_mbid="rg-late")
    )
    music, paths = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    tag, info = AudioTagger().read_tags(paths[0])
    await manager.queue_for_manual_review(paths[0], tag, info, source="text_match")
    assert len(await manager.get_unmatched()) == 1

    await scanner.scan([music])

    assert await db.has_album_files("rg-late")
    assert await manager.get_unmatched() == []


@pytest.mark.asyncio
async def test_rescan_counts_skipped_files_as_matched(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac", "flac_full_02.flac")
    await scanner.scan([music])
    assert (await state.get_state())["matched_files"] == 2

    await scanner.scan([music])
    assert (await state.get_state())["matched_files"] == 2


@pytest.mark.asyncio
async def test_incremental_reimports_changed_file(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, paths = _music_dir(tmp_path, "flac_full_01.flac")
    await scanner.scan([music])

    # Bump mtime so the file looks modified (content stays valid for the tagger).
    st = paths[0].stat()
    os.utime(paths[0], (st.st_atime, st.st_mtime + 10))

    calls = {"n": 0}
    original = manager.upsert_file

    async def spy(*args, **kwargs):
        calls["n"] += 1
        return await original(*args, **kwargs)

    manager.upsert_file = spy
    await scanner.scan([music])
    assert calls["n"] == 1  # changed file is re-imported


@pytest.mark.asyncio
async def test_soft_delete_removed_file(tmp_path):
    scanner, manager, state, db = _build(tmp_path)
    music, paths = _music_dir(tmp_path, "flac_full_01.flac", "m4a_full_01.m4a")
    await scanner.scan([music])
    assert await db.has_album_files(_RG_MEZZANINE)

    paths[1].unlink()  # remove the m4a from disk
    await scanner.scan([music])
    assert not await db.has_album_files(_RG_MEZZANINE)  # reconciled away
    assert await db.has_album_files(_RG_AIRBAG)  # survivor untouched


@pytest.mark.asyncio
async def test_progress_writes_are_batched_not_per_file(tmp_path):
    # AUD-4: a large scan batches ledger writes and throttles SSE - it must NOT
    # write scan_state once per file.
    scanner, manager, state, db = _build(tmp_path)
    music = tmp_path / "music"
    music.mkdir()
    n = _LEDGER_BATCH_SIZE + 50  # cross exactly one batch boundary
    for i in range(n):
        shutil.copy(FIXTURES / "flac_full_01.flac", music / f"{i:04d}.flac")

    advance_calls = 0
    real_advance = state.advance

    async def counting_advance(*args, **kwargs):
        nonlocal advance_calls
        advance_calls += 1
        return await real_advance(*args, **kwargs)

    state.advance = counting_advance

    progress_events = 0
    real_publish = scanner._events.publish

    async def counting_publish(channel, event, data):
        nonlocal progress_events
        if event == "progress":
            progress_events += 1
        return await real_publish(channel, event, data)

    scanner._events.publish = counting_publish

    await scanner.scan([music])

    assert await db.has_any_files()
    # 150 files -> ledger flushed at the 100 boundary + once at the end = 2, never 150.
    assert advance_calls <= 3
    assert advance_calls < n
    assert progress_events < n  # throttled (~2s window), not one per file


# -- Phase 5 admin file operations (real AudioTagger round-trips) --


@pytest.mark.asyncio
async def test_update_track_tags_writes_to_disk_and_refreshes_row(tmp_path):
    scanner, manager, _state, db = _build(tmp_path)
    music, paths = _music_dir(tmp_path, "flac_full_01.flac")
    await scanner.scan([music])
    file_id = (await db.get_library_files_for_album(_RG_AIRBAG))[0]["id"]

    new_tag = AudioTag(
        title="Renamed Track",
        artist="Radiohead",
        album="OK Computer",
        album_artist="Radiohead",
        track_number=1,
        year=1997,
        musicbrainz_release_group_id=_RG_AIRBAG,
        musicbrainz_recording_id="rec-airbag-0001",
    )
    updated = await scanner.update_track_tags(file_id, new_tag)

    assert updated.track_title == "Renamed Track"
    # The tags were actually written to the file on disk.
    tag_on_disk, _info = AudioTagger().read_tags(paths[0])
    assert tag_on_disk.title == "Renamed Track"
    # The DB row was refreshed in place, still in the same album.
    tracks = await manager.get_tracks(_RG_AIRBAG)
    assert tracks[0].track_title == "Renamed Track"


@pytest.mark.asyncio
async def test_update_track_tags_unknown_id_raises_not_found(tmp_path):
    scanner, _manager, _state, _db = _build(tmp_path)
    tag = AudioTag(
        title="x", artist="x", album="x", track_number=1,
        musicbrainz_release_group_id="rg-x",
    )
    with pytest.raises(ResourceNotFoundError):
        await scanner.update_track_tags("does-not-exist", tag)


@pytest.mark.asyncio
async def test_update_track_tags_requires_release_group(tmp_path):
    scanner, _manager, _state, db = _build(tmp_path)
    music, _paths = _music_dir(tmp_path, "flac_full_01.flac")
    await scanner.scan([music])
    file_id = (await db.get_library_files_for_album(_RG_AIRBAG))[0]["id"]
    tag = AudioTag(title="x", artist="x", album="x", track_number=1)  # no RG
    with pytest.raises(ValidationError):
        await scanner.update_track_tags(file_id, tag)


@pytest.mark.asyncio
async def test_resolve_unmatched_accept_imports_file(tmp_path):
    scanner, manager, _state, _db = _build(tmp_path, resolve="rg-resolved")
    _music, paths = _music_dir(tmp_path, "flac_no_tags.flac")
    tag, info = AudioTagger().read_tags(paths[0])
    await manager.queue_for_manual_review(
        paths[0], tag, info, source="acoustid", candidates=["rec-candidate"]
    )
    review_id = (await manager.get_unmatched())[0].id

    await scanner.resolve_unmatched(review_id, "accept")

    assert await manager.has_album("rg-resolved") is True
    assert await manager.get_unmatched() == []  # dropped from the review list


@pytest.mark.asyncio
async def test_resolve_unmatched_batch_attributes_files_to_one_album(tmp_path):
    scanner, manager, _state, _db = _build(tmp_path)
    _music, paths = _music_dir(tmp_path, "flac_no_tags.flac", "flac_no_tags.flac")
    for p in paths:
        tag, info = AudioTagger().read_tags(p)
        await manager.queue_for_manual_review(p, tag, info, source="text_match")
    ids = sorted(u.id for u in await manager.get_unmatched())

    result = await scanner.resolve_unmatched_batch(
        "rg-batch", [(ids[0], "rec-a"), (ids[1], "rec-b")]
    )

    assert result == {"resolved": 2, "failed": []}
    assert await manager.has_album("rg-batch") is True
    assert await manager.get_unmatched() == []


@pytest.mark.asyncio
async def test_resolve_unmatched_batch_continues_past_bad_rows(tmp_path):
    scanner, manager, _state, _db = _build(tmp_path)
    _music, paths = _music_dir(tmp_path, "flac_no_tags.flac")
    tag, info = AudioTagger().read_tags(paths[0])
    await manager.queue_for_manual_review(paths[0], tag, info, source="text_match")
    good_id = (await manager.get_unmatched())[0].id

    result = await scanner.resolve_unmatched_batch(
        "rg-x", [(good_id, "rec-a"), (999999, "rec-b")]
    )

    assert result["resolved"] == 1
    assert [f["review_id"] for f in result["failed"]] == [999999]
    assert await manager.has_album("rg-x") is True


@pytest.mark.asyncio
async def test_unmatched_row_carries_track_and_disc_number(tmp_path):
    import msgspec

    scanner, manager, _state, _db = _build(tmp_path)
    _music, paths = _music_dir(tmp_path, "flac_no_tags.flac")
    tag, info = AudioTagger().read_tags(paths[0])
    tag = msgspec.structs.replace(tag, track_number=7, disc_number=2)
    await manager.queue_for_manual_review(paths[0], tag, info, source="text_match")

    entry = (await manager.get_unmatched())[0]
    assert entry.track_number == 7
    assert entry.disc_number == 2


@pytest.mark.asyncio
async def test_stats_counts_distinct_artists(tmp_path):
    scanner, manager, _state, _db = _build(tmp_path)
    music, _ = _music_dir(
        tmp_path, "flac_full_01.flac", "flac_full_02.flac", "m4a_full_01.m4a"
    )
    await scanner.scan([music])
    stats = await manager.get_stats()
    assert stats.total_albums == 2
    assert stats.total_artists == 2
    assert stats.last_scan_at is not None


@pytest.mark.asyncio
async def test_resolve_unmatched_reject_marks_resolved_without_import(tmp_path):
    scanner, manager, _state, db = _build(tmp_path)
    _music, paths = _music_dir(tmp_path, "flac_no_tags.flac")
    tag, info = AudioTagger().read_tags(paths[0])
    await manager.queue_for_manual_review(
        paths[0], tag, info, source="text_match", candidates=[]
    )
    review_id = (await manager.get_unmatched())[0].id

    await scanner.resolve_unmatched(review_id, "reject")

    assert await manager.get_unmatched() == []
    assert await db.has_any_files() is False  # nothing imported


@pytest.mark.asyncio
async def test_resolve_unmatched_manual_id_requires_mbid(tmp_path):
    scanner, manager, _state, _db = _build(tmp_path)
    _music, paths = _music_dir(tmp_path, "flac_no_tags.flac")
    tag, info = AudioTagger().read_tags(paths[0])
    await manager.queue_for_manual_review(paths[0], tag, info, source="text_match")
    review_id = (await manager.get_unmatched())[0].id
    with pytest.raises(ValidationError):
        await scanner.resolve_unmatched(review_id, "manual_id", mbid=None)


@pytest.mark.asyncio
async def test_resolve_unmatched_unknown_id_raises_not_found(tmp_path):
    scanner, _manager, _state, _db = _build(tmp_path)
    with pytest.raises(ResourceNotFoundError):
        await scanner.resolve_unmatched(999999, "reject")


@pytest.mark.asyncio
async def test_rescan_album_refreshes_present_and_soft_deletes_missing(tmp_path):
    scanner, manager, _state, _db = _build(tmp_path)
    _music, paths = _music_dir(tmp_path, "flac_full_01.flac", "flac_full_02.flac")
    await scanner.scan([_music])
    assert len(await manager.get_tracks(_RG_AIRBAG)) == 2

    paths[0].unlink()  # one file vanishes from disk
    refreshed = await scanner.rescan_album(_RG_AIRBAG)

    assert refreshed == 1  # only the surviving file was re-read
    tracks_after = await manager.get_tracks(_RG_AIRBAG)
    assert len(tracks_after) == 1  # the missing file was soft-deleted


@pytest.mark.asyncio
async def test_read_track_tags_returns_current_tags(tmp_path):
    scanner, _manager, _state, db = _build(tmp_path)
    music, _paths = _music_dir(tmp_path, "flac_full_01.flac")
    await scanner.scan([music])
    file_id = (await db.get_library_files_for_album(_RG_AIRBAG))[0]["id"]
    tag = await scanner.read_track_tags(file_id)
    assert tag.musicbrainz_release_group_id == _RG_AIRBAG


@pytest.mark.asyncio
async def test_read_track_tags_unknown_id_raises_not_found(tmp_path):
    scanner, _manager, _state, _db = _build(tmp_path)
    with pytest.raises(ResourceNotFoundError):
        await scanner.read_track_tags("does-not-exist")


# -- folder-level fingerprint consensus (files never scatter across release groups) --


@pytest.mark.asyncio
async def test_folder_fingerprint_match_prevents_scatter(tmp_path):
    """Junk tags -> the tag-based album match returns nothing; fingerprinting the folder
    resolves ONE release group and both files land under it, instead of scattering across
    two (the Led Zeppelin bug)."""
    from services.native.album_matcher import AlbumMatch

    scanner, _manager, _state, db = _build(
        tmp_path,
        fingerprint=FingerprintResult(status="pass", score=0.9, recording_id="rec-debut"),
        resolve="rg-debut",
    )
    music, paths = _music_dir(
        tmp_path, "flac_only_release_mbid.flac", "flac_only_release_mbid.flac"
    )
    accepted = AlbumMatch(
        accepted=True, distance=0.05,
        release_group_mbid="rg-debut", release_mbid="rel-debut",
        assignments={str(paths[0]): "rec-1", str(paths[1]): "rec-2"},
        artist_mbid="a1", artist_name="Led Zeppelin",
    )
    # First (tag) attempt finds nothing; the fingerprint-seeded second attempt succeeds.
    scanner._album_identifier.identify = AsyncMock(side_effect=[None, accepted])

    await scanner.scan([music])

    rows = await db.get_library_files_for_album("rg-debut")
    assert len(rows) == 2  # both files, ONE release group - no scatter
    # the second attempt was seeded with the fingerprint-resolved release group
    second_call = scanner._album_identifier.identify.await_args_list[1]
    assert second_call.kwargs["seed_release_groups"] == ["rg-debut"]


@pytest.mark.asyncio
async def test_no_fingerprint_seed_skips_second_attempt(tmp_path):
    """AcoustID disabled -> no seed -> the fingerprint attempt is skipped, files fall
    through to per-file review, and each file is fingerprinted at most once (reused)."""
    scanner, manager, _state, db = _build(tmp_path)  # fingerprint disabled, identify -> None
    scanner._album_identifier.identify = AsyncMock(return_value=None)
    music, _ = _music_dir(
        tmp_path, "flac_only_release_mbid.flac", "flac_only_release_mbid.flac"
    )

    await scanner.scan([music])

    assert not await db.has_any_files()
    assert len(await manager.get_unmatched()) == 2
    assert scanner._album_identifier.identify.await_count == 1  # no seeds -> no 2nd attempt
    assert scanner._fingerprinter.fingerprint.await_count == 2  # once per file, reused


# -- Phase 1: anchor + stickiness + INV-1 (ScannerAlbumIdentity) --


@pytest.mark.asyncio
async def test_should_keep_prior_blocks_album_to_compilation(tmp_path):
    # INV-1: never demote a studio album to a compilation/live release group.
    scanner, *_ = _build(tmp_path)
    scanner._album_identifier.release_group_type = AsyncMock(
        side_effect=lambda rg: {
            "rg-album": ("album", frozenset()),
            "rg-comp": ("album", frozenset({"compilation"})),
            "rg-live": ("album", frozenset({"live"})),
        }.get(rg, (None, frozenset()))
    )
    prior = {"release_group_mbid": "rg-album", "confidence": 1.0, "source": "download"}
    assert await scanner._should_keep_prior(prior, "rg-comp", 0.99) is True
    assert await scanner._should_keep_prior(prior, "rg-live", 0.99) is True


@pytest.mark.asyncio
async def test_should_keep_prior_stickiness_and_override(tmp_path):
    scanner, *_ = _build(tmp_path)
    scanner._album_identifier.release_group_type = AsyncMock(
        return_value=("album", frozenset())
    )
    # a confident download resists a non-improving change to another studio album
    dl = {"release_group_mbid": "rg-a", "confidence": 1.0, "source": "download"}
    assert await scanner._should_keep_prior(dl, "rg-b", 0.95) is True
    # a high-confidence scan prior yields only to a clearly-better match (+margin)
    scan_prior = {"release_group_mbid": "rg-a", "confidence": 0.85, "source": "scan"}
    assert await scanner._should_keep_prior(scan_prior, "rg-b", 0.95) is False
    assert await scanner._should_keep_prior(scan_prior, "rg-b", 0.88) is True


@pytest.mark.asyncio
async def test_should_keep_prior_noop_cases(tmp_path):
    scanner, *_ = _build(tmp_path)
    scanner._album_identifier.release_group_type = AsyncMock(
        return_value=("album", frozenset())
    )
    same = {"release_group_mbid": "rg-a", "confidence": 1.0, "source": "download"}
    assert await scanner._should_keep_prior(same, "rg-a", 0.1) is False  # no change
    assert await scanner._should_keep_prior({"release_group_mbid": None}, "rg-b", 0.1) is False
    low = {"release_group_mbid": "rg-a", "confidence": 0.5, "source": "scan"}
    assert await scanner._should_keep_prior(low, "rg-b", 0.6) is False  # not anchored -> allow


def test_incumbent_rg_picks_dominant_confident(tmp_path):
    scanner, *_ = _build(tmp_path)
    existing = {
        "/a": {"release_group_mbid": "rg-x", "source": "download", "confidence": 1.0},
        "/b": {"release_group_mbid": "rg-x", "source": "scan", "confidence": 0.9},
        "/c": {"release_group_mbid": "rg-y", "source": "scan", "confidence": 0.4},  # ignored
    }
    assert scanner._incumbent_rg(existing) == "rg-x"
    assert scanner._incumbent_rg({}) is None


@pytest.mark.asyncio
async def test_get_attributions_for_paths_returns_active_rows(tmp_path):
    scanner, manager, _state, _db = _build(tmp_path)
    _music, paths = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    p = paths[0]
    tag, info = AudioTagger().read_tags(p)
    await manager.upsert_file(
        p, tag, info, release_group_mbid="rg-x", source="download", confidence=1.0
    )
    got = await manager.get_attributions_for_paths([str(p), "/nope.flac"])
    assert set(got) == {str(p)}
    assert got[str(p)]["release_group_mbid"] == "rg-x"
    assert got[str(p)]["source"] == "download"
    assert await manager.get_attributions_for_paths([]) == {}


@pytest.mark.asyncio
async def test_rescan_never_demotes_a_downloaded_album_to_compilation(tmp_path):
    # End-to-end (per-file path): a downloaded studio-album track whose fingerprint now
    # resolves to a compilation recording must NOT be re-attributed to the compilation.
    scanner, manager, _state, db = _build(
        tmp_path,
        fingerprint=FingerprintResult(status="pass", score=0.95, recording_id="rec-comp"),
        resolve="rg-comp",
    )
    scanner._album_identifier.release_group_type = AsyncMock(
        side_effect=lambda rg: {
            "rg-studio": ("album", frozenset()),
            "rg-comp": ("album", frozenset({"compilation"})),
        }.get(rg, (None, frozenset()))
    )
    music, paths = _music_dir(tmp_path, "flac_only_release_mbid.flac")
    p = paths[0]
    tag, info = AudioTagger().read_tags(p)
    # the download already placed it on the studio album; the stale file_mtime makes the
    # scan re-process it (rather than incremental-skip), so the guard is exercised
    await manager.upsert_file(
        p, tag, info, release_group_mbid="rg-studio", source="download",
        confidence=1.0, file_mtime=1.0,
    )

    await scanner.scan([music])

    assert len(await db.get_library_files_for_album("rg-studio")) == 1  # kept
    assert await db.get_library_files_for_album("rg-comp") == []  # never demoted


@pytest.mark.asyncio
async def test_reidentify_album_forces_correction_past_stickiness(tmp_path):
    # Phase 4 / R-CORRECT: a forced re-identify overrides even a confident (sticky)
    # download attribution - the deliberate way to fix an album stuck on the wrong RG.
    from services.native.album_matcher import AlbumMatch

    scanner, manager, _state, db = _build(tmp_path)
    paths = _album_dir(tmp_path, "Artist/Album", 2)
    for p in paths:  # a download stuck them on the WRONG album (confident -> normally sticky)
        tag, info = AudioTagger().read_tags(p)
        await manager.upsert_file(
            p, tag, info, release_group_mbid="rg-wrong", source="download", confidence=1.0
        )

    async def identify(locals_, *, seed_release_groups=None):
        return AlbumMatch(
            accepted=True, distance=0.02,
            release_group_mbid="rg-right", release_mbid="rel-right",
            assignments={lt.path: f"rec-{i}" for i, lt in enumerate(locals_)},
        )

    scanner._album_identifier.identify = AsyncMock(side_effect=identify)

    moved = await scanner.reidentify_album("rg-wrong")

    assert moved == 2
    assert len(await db.get_library_files_for_album("rg-right")) == 2  # corrected
    assert await db.get_library_files_for_album("rg-wrong") == []  # left the wrong album


@pytest.mark.asyncio
async def test_reidentify_invalidates_old_and_new_album_caches(tmp_path):
    # The correction must bust the cached pages of both the group it left and the group it
    # moved to, so neither shows stale numbers without a manual refresh.
    from services.native.album_matcher import AlbumMatch

    scanner, manager, _state, _db = _build(tmp_path)
    paths = _album_dir(tmp_path, "Artist/Album", 2)
    for p in paths:
        tag, info = AudioTagger().read_tags(p)
        await manager.upsert_file(
            p, tag, info, release_group_mbid="rg-wrong", source="download", confidence=1.0
        )

    async def identify(locals_, *, seed_release_groups=None):
        return AlbumMatch(
            accepted=True, distance=0.02,
            release_group_mbid="rg-right", release_mbid="rel-right",
            assignments={lt.path: f"rec-{i}" for i, lt in enumerate(locals_)},
        )

    scanner._album_identifier.identify = AsyncMock(side_effect=identify)
    busted: list[set[str]] = []

    async def capture(changed):
        busted.append(set(changed))

    scanner._invalidate_albums = capture

    await scanner.reidentify_album("rg-wrong")

    assert busted, "invalidator should run"
    assert {"rg-wrong", "rg-right"} <= busted[0]


@pytest.mark.asyncio
async def test_scan_invalidates_changed_albums_but_not_an_unchanged_rescan(tmp_path):
    # A first attribution busts the album it lands on; an incremental re-scan that changes
    # nothing must not re-invalidate (which would needlessly re-fetch from MusicBrainz).
    scanner, _manager, _state, _db = _build(tmp_path)
    music, _ = _music_dir(tmp_path, "flac_full_01.flac")  # tier1 tags -> _RG_AIRBAG
    calls: list[set[str]] = []

    async def capture(changed):
        calls.append(set(changed))

    scanner._invalidate_albums = capture

    await scanner.scan([music])
    assert calls and _RG_AIRBAG in calls[0]

    calls.clear()
    await scanner.scan([music])  # unchanged: incremental skip -> no write -> no invalidation
    assert calls == []


@pytest.mark.asyncio
async def test_weakly_verified_download_rows_are_reidentifiable(tmp_path):
    """P7 (D7): a download import stamped BELOW-sticky confidence stays anchored
    (source='download' resists non-improving churn) but yields to a clearly-better
    scan identification - unlike the pre-P7 hardcoded 1.0, which froze a wrong
    import at fake-perfect confidence forever."""
    scanner, *_ = _build(tmp_path)
    scanner._album_identifier.release_group_type = AsyncMock(
        return_value=("album", frozenset())
    )
    weak_dl = {"release_group_mbid": "rg-a", "confidence": 0.6, "source": "download"}
    # anchored: a merely-comparable match never churns it
    assert await scanner._should_keep_prior(weak_dl, "rg-b", 0.55) is True
    # re-identifiable: a clearly-better identification (+0.05 margin) wins
    assert await scanner._should_keep_prior(weak_dl, "rg-b", 0.70) is False
