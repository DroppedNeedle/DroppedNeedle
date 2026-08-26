"""SoulseekStrategy single-track (1-track album) routing + enqueue verification.

A single requested as an album must score per-file via the TrackMatcher (canonical
duration + artist-evidence auto gate) instead of the folder scorer, and its manifest
must arm the canonical-duration import gate (``is_track``) and carry the expected
track (AcoustID title check / tag verification). 2026-07-05 wrong-single incident:
.dev-notes/Bugs/2026-07-05-wrong-single-remediation-plan.md, P1.
"""

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from models.download import DownloadTask, ScoredCandidate
from models.download_manifest import ManifestCodec
from repositories.protocols.download_client import DownloadSearchResult, TaskHandle
from services.native.acquisition.strategy import SoulseekStrategy, _clean_candidates

_CANONICAL = 155.556  # "the arrival" (recording 180ceef5...), seconds


def _search_result(username="peer", filename="peer/01.flac", duration=155.0):
    return DownloadSearchResult(
        username=username,
        filename=filename,
        parent_directory="peer",
        size=100,
        extension="flac",
        duration=duration,
    )


def _single_task(**overrides) -> DownloadTask:
    kwargs = dict(
        id="t1",
        user_id="u1",
        download_type="album",
        release_group_mbid="rg-1",
        release_mbid="release-1",
        release_track_mbid="release-track-1",
        artist_name="Yan Qing",
        album_title="the arrival",
        year=2026,
        track_count=1,
        track_title="the arrival",
        recording_mbid="rec-180ceef5",
        track_number=1,
        disc_number=1,
        track_duration_seconds=_CANONICAL,
        origin="user",
    )
    kwargs.update(overrides)
    return DownloadTask(**kwargs)


def test_clean_candidate_filter_rejects_explicit_markers_only_for_clean_tasks():
    explicit = ScoredCandidate(
        username="peer",
        parent_directory="Artist - Album [Explicit]",
        files=[_search_result(filename="Artist - Album [Explicit]/01.flac")],
        tier="auto",
    )
    unmarked = ScoredCandidate(
        username="peer",
        parent_directory="Artist - Album",
        files=[_search_result(filename="Artist - Album/01.flac")],
        tier="auto",
    )

    assert _clean_candidates(
        _single_task(content_variant="clean"), [explicit, unmarked]
    ) == [unmarked]
    assert _clean_candidates(_single_task(), [explicit, unmarked]) == [explicit, unmarked]


def _strategy(tmp_path: Path):
    indexer = MagicMock()
    indexer.search_album = AsyncMock(
        return_value=[SimpleNamespace(soulseek=_search_result())]
    )
    indexer.search_track = AsyncMock(return_value=[])
    scorer = MagicMock()
    scorer.rank = AsyncMock(return_value=[])
    track_matcher = MagicMock()
    track_matcher.rank = AsyncMock(return_value=[])
    client = AsyncMock()
    client.enqueue.return_value = TaskHandle(
        source="soulseek",
        username="Fabrizio83a",
        filenames=["peer/02. Arrival in Ashford.flac"],
    )
    store = AsyncMock()
    store.create_download_attempt.return_value = SimpleNamespace(id="attempt-1")
    album_service = MagicMock()
    album_service.get_exact_edition_tracks_info = AsyncMock(
        return_value=SimpleNamespace(
            tracks=[
                SimpleNamespace(
                    position=index,
                    disc_number=1,
                    title=f"Track {index}",
                    recording_id=f"recording-{index}",
                    release_track_id=f"release-track-{index}",
                    length=180_000,
                )
                for index in range(1, 13)
            ]
        )
    )
    strategy = SoulseekStrategy(
        indexer=indexer,
        scorer=scorer,
        track_matcher=track_matcher,
        client=client,
        store=store,
        file_processor=MagicMock(),
        staging=tmp_path,
        manifest_codec=ManifestCodec(),
        naming_template="{albumartist}/{album}/{title}.{ext}",
        album_service=album_service,
    )
    return strategy, indexer, scorer, track_matcher


# --- search routing --------------------------------------------------------------


@pytest.mark.asyncio
async def test_single_album_task_scores_via_track_matcher(tmp_path: Path):
    strategy, indexer, scorer, track_matcher = _strategy(tmp_path)

    await strategy.search_and_score(_single_task(), timeout=30, auto=0.7, manual=0.5)

    # searched with the ALBUM query ladder, scored with the per-file track matcher
    indexer.search_album.assert_awaited_once()
    track_matcher.rank.assert_awaited_once()
    scorer.rank.assert_not_awaited()
    target = track_matcher.rank.await_args.args[0]
    assert target.track_title == "the arrival"
    assert target.duration_seconds == _CANONICAL
    assert target.recording_mbid == "rec-180ceef5"


@pytest.mark.asyncio
async def test_single_without_threaded_identity_falls_back_to_folder_scorer(
    tmp_path: Path,
):
    # MusicBrainz was down at request time -> no track_title -> the album path
    # behaves exactly as before (degraded, still covered by the later gates).
    strategy, _indexer, scorer, track_matcher = _strategy(tmp_path)

    await strategy.search_and_score(
        _single_task(
            track_title=None, recording_mbid=None, track_duration_seconds=None
        ),
        timeout=30,
        auto=0.7,
        manual=0.5,
    )

    scorer.rank.assert_awaited_once()
    track_matcher.rank.assert_not_awaited()


@pytest.mark.asyncio
async def test_multi_track_album_still_uses_folder_scorer(tmp_path: Path):
    strategy, _indexer, scorer, track_matcher = _strategy(tmp_path)

    await strategy.search_and_score(
        _single_task(track_count=12, track_title=None, track_duration_seconds=None),
        timeout=30,
        auto=0.7,
        manual=0.5,
    )

    scorer.rank.assert_awaited_once()
    track_matcher.rank.assert_not_awaited()


# --- enqueue: canonical gate + expected track ------------------------------------


def _candidate(duration=137.0):
    f = _search_result(
        username="Fabrizio83a",
        filename="peer/02. Arrival in Ashford.flac",
        duration=duration,
    )
    return ScoredCandidate(
        username="Fabrizio83a",
        parent_directory="peer",
        files=[f],
        coherence=0.6,
        file_confidence=0.6,
        final_score=0.6,
        tier="manual",
    )


async def _enqueue(tmp_path: Path, task: DownloadTask, *, strict=True):
    strategy, *_ = _strategy(tmp_path)
    await strategy.enqueue(task, _candidate(), strict_track_duration=strict)
    raw = (tmp_path / task.id / "manifest.json").read_bytes()
    return ManifestCodec().decode(raw)


@pytest.mark.asyncio
async def test_single_enqueue_arms_canonical_duration_gate(tmp_path: Path):
    manifest = await _enqueue(tmp_path, _single_task())

    # is_track -> the import duration gate compares against the CANONICAL length and
    # a mismatch reads WRONG_TRACK (failover), not duration_mismatch (quarantine).
    assert manifest.is_track is True
    assert manifest.target_files[0].duration == _CANONICAL
    # the expected track rides the manifest: arms the AcoustID title check + P2
    assert len(manifest.expected_tracks) == 1
    expected = manifest.expected_tracks[0]
    assert expected.title == "the arrival"
    assert expected.recording_mbid == "rec-180ceef5"
    assert expected.release_track_mbid == "release-track-1"
    assert expected.duration_seconds == _CANONICAL
    assert manifest.release_mbid == "release-1"


@pytest.mark.asyncio
async def test_single_enqueue_strict_off_keeps_peer_duration(tmp_path: Path):
    # The last-resort semantics are preserved: with strict off the peer-advertised
    # length is the expectation (never strand the user on a bad MB duration).
    manifest = await _enqueue(tmp_path, _single_task(), strict=False)

    assert manifest.is_track is False
    assert manifest.target_files[0].duration == 137.0


@pytest.mark.asyncio
async def test_multi_track_album_enqueue_carries_complete_exact_track_map(
    tmp_path: Path,
):
    manifest = await _enqueue(
        tmp_path,
        _single_task(
            track_count=12,
            track_title=None,
            recording_mbid=None,
            track_duration_seconds=None,
        ),
    )

    assert manifest.is_track is False
    assert len(manifest.expected_tracks) == 12
    assert manifest.expected_tracks[0].release_track_mbid == "release-track-1"
    assert manifest.expected_tracks[-1].recording_mbid == "recording-12"
    assert (
        len(
            {
                (track.disc_number, track.track_number)
                for track in manifest.expected_tracks
            }
        )
        == 12
    )
    assert manifest.target_files[0].duration == 137.0
