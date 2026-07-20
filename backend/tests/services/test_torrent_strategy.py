"""TorrentStrategy: the seeding-safety invariant (import hands the processor COPIES
under staging - qBittorrent's payload files are never moved), torrent-arm search
filtering, and blocklist-by-identity on failure."""

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from models.download import ScoredCandidate
from models.download_identity import usenet_identity
from models.download_manifest import DownloadManifest
from repositories.protocols.download_client import TaskHandle
from repositories.protocols.indexer import IndexerResult, TorrentRelease
from services.native.acquisition.strategy import TorrentStrategy
from services.native.file_processor import ProcessResult


def _release(**kw) -> TorrentRelease:
    base = dict(
        indexer_id="1", indexer_name="RED", guid="g", title="Artist - Album FLAC",
        magnet_url="magnet:?xt=x", size_bytes=500_000_000, seeders=10,
    )
    base.update(kw)
    return TorrentRelease(**base)


def _task(**kw):
    task = MagicMock()
    task.id = kw.get("id", "t1")
    task.search_job_id = kw.get("search_job_id", "job1")
    task.candidate_index = kw.get("candidate_index", 0)
    task.release_group_mbid = "rg-1"
    task.origin = "request"
    return task


def _strategy(tmp_path, *, indexer=None, client=None, store=None, file_processor=None):
    return TorrentStrategy(
        indexer=indexer or AsyncMock(),
        scorer=AsyncMock(),
        client=client or AsyncMock(),
        store=store or AsyncMock(),
        file_processor=file_processor or AsyncMock(),
        import_settle_seconds=0.0,
        staging=tmp_path / "staging",
        manifest_codec=MagicMock(),
        naming_template="{artist}/{album}",
        album_service=None,
    )


@pytest.mark.asyncio
async def test_import_hands_processor_copies_and_leaves_sources_seeding(tmp_path):
    # The downloaded (seeding) payload lives in the client's dir.
    payload = tmp_path / "downloads" / "Album"
    payload.mkdir(parents=True)
    src_a = payload / "01 - One.flac"
    src_b = payload / "cd2" / "01 - One.flac"
    src_b.parent.mkdir()
    src_a.write_bytes(b"a" * 64)
    src_b.write_bytes(b"b" * 64)

    client = AsyncMock()
    client.list_completed_files.return_value = [src_a, src_b]
    processor = AsyncMock()
    processor.process_downloaded_folder.return_value = ProcessResult(
        succeeded=[], failed=[]
    )
    strategy = _strategy(tmp_path, client=client, file_processor=processor)
    manifest = DownloadManifest(
        task_id="t1", release_group_mbid="rg-1", artist_name="Artist",
        album_title="Album", naming_template="{artist}/{album}", target_files=[],
        handle=TaskHandle(source="torrent"),
    )

    _result, enumerated = await strategy.import_files(_task(), manifest, completed=True)

    assert enumerated == 2
    handed = processor.process_downloaded_folder.await_args.args[1]
    staging_root = (tmp_path / "staging").resolve()
    assert len(handed) == 2
    for path in handed:
        assert Path(path).resolve().is_relative_to(staging_root)  # copies, not payload
    # Multi-disc structure survives the copy (identical basenames must not collide).
    assert {p.parent.name for p in handed} == {"Album", "cd2"} or len(
        {str(p) for p in handed}
    ) == 2
    # The seeding payload is untouched.
    assert src_a.read_bytes() == b"a" * 64
    assert src_b.read_bytes() == b"b" * 64


@pytest.mark.asyncio
async def test_search_and_score_filters_torrent_arm(tmp_path):
    indexer = AsyncMock()
    torrent = _release()
    indexer.search_album.return_value = [
        IndexerResult(source="torrent", torrent=torrent),
        IndexerResult(source="usenet", usenet=None),
    ]
    strategy = _strategy(tmp_path, indexer=indexer)
    task = _task()
    task.download_type = "album"
    task.artist_name, task.album_title = "Artist", "Album"
    task.year, task.track_count = 2007, 10

    await strategy.search_and_score(task, timeout=5.0, auto=0.7, manual=0.5)

    releases = strategy._scorer.rank.await_args.args[1]
    assert releases == [torrent]


@pytest.mark.asyncio
async def test_blocklist_on_failure_uses_torrent_namespace(tmp_path):
    release = _release()
    store = AsyncMock()
    store.get_search_job_candidates.return_value = [
        ScoredCandidate(source="torrent", torrent_release=release)
    ]
    strategy = _strategy(tmp_path, store=store)

    await strategy.maybe_blocklist_on_failure(
        _task(), None, completed=True, enumerated_any=True
    )

    kwargs = store.record_quarantine.await_args.kwargs
    assert kwargs["source"] == "torrent"
    assert kwargs["identity"] == usenet_identity(release.title, release.size_bytes)
    assert kwargs["reason"] == "verify_failed"


@pytest.mark.asyncio
async def test_candidate_identity_reads_torrent_release(tmp_path):
    release = _release()
    strategy = _strategy(tmp_path)
    cand = ScoredCandidate(source="torrent", torrent_release=release)
    assert strategy.candidate_identity(cand) == usenet_identity(
        release.title, release.size_bytes
    )
