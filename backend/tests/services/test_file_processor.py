"""Tests for FileProcessor: verify_downloaded_file (Phase 3) + process_downloaded
(Phase 7 import pipeline)."""

import shutil
import threading
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.library_db import LibraryDB
from models.audio import AudioInfo, AudioTag, FingerprintResult
from models.download_manifest import DownloadManifest, ExpectedFile, ExpectedTrack
from services.native.file_processor import (
    DOWNLOADS_MOUNT_UNAVAILABLE,
    QUARANTINE_REASONS,
    WRONG_TRACK,
    FileProcessor,
    VerifyStatus,
    _FolderCandidate,
    _folder_names_wrong_album,
)
from services.native.library_manager import LibraryManager
from services.native.naming import NamingTemplateEngine

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "library"
# flac_full_01.flac was generated with title="Airbag", artist="Radiohead".
_FLAC = FIXTURES / "flac_full_01.flac"
_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"


@pytest.fixture
def processor() -> FileProcessor:
    return FileProcessor(AudioTagger())


def test_valid_file_matching_expectations_passes(processor):
    result = processor.verify_downloaded_file(_FLAC, title="Airbag", artist="Radiohead")
    assert result.status == VerifyStatus.PASS
    assert result.passed is True


def test_no_expectations_verifies_readability_only(processor):
    assert processor.verify_downloaded_file(_FLAC).passed is True


def test_missing_file_fails(processor, tmp_path):
    result = processor.verify_downloaded_file(tmp_path / "nope.flac")
    assert result.status == VerifyStatus.FAIL
    assert result.reason == "file not found"


def test_unreadable_file_fails(processor, tmp_path):
    junk = tmp_path / "junk.flac"
    junk.write_bytes(b"not audio")
    result = processor.verify_downloaded_file(junk)
    assert result.status == VerifyStatus.FAIL
    assert result.reason == "tags unreadable"


def test_title_mismatch_fails(processor):
    result = processor.verify_downloaded_file(_FLAC, title="Wrong Title")
    assert result.status == VerifyStatus.FAIL
    assert result.reason == "title mismatch"


def test_match_is_case_insensitive(processor):
    assert processor.verify_downloaded_file(_FLAC, artist="radiohead").passed is True


def test_artist_mismatch_fails(processor):
    result = processor.verify_downloaded_file(_FLAC, artist="Wrong Artist")
    assert result.status == VerifyStatus.FAIL
    assert result.reason == "artist mismatch"


def test_album_mismatch_fails(processor):
    result = processor.verify_downloaded_file(_FLAC, album="Wrong Album")
    assert result.status == VerifyStatus.FAIL
    assert result.reason == "album mismatch"


def test_track_number_mismatch_fails(processor):
    result = processor.verify_downloaded_file(_FLAC, track_number=999)
    assert result.status == VerifyStatus.FAIL
    assert result.reason == "track number mismatch"


class _StubClient:
    def __init__(self, downloads_root: Path) -> None:
        self._root = downloads_root
        self.cancel = MagicMock()  # asserted NEVER called by FileProcessor (DEC-1)

    async def get_file_path(self, handle, remote_filename: str, size: int | None = None):
        return self._root / remote_filename.replace("\\", "/").lstrip("/")


def _place(downloads: Path, rel: str) -> None:
    dest = downloads / rel
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(_FLAC, dest)


def _manifest(
    *files: ExpectedFile, task_id="t1", rg="rg-1", is_track=False, expected_tracks=()
) -> DownloadManifest:
    return DownloadManifest(
        task_id=task_id,
        source_username="peer",
        release_group_mbid=rg,
        artist_name="Radiohead",
        artist_mbid="a74b1b7f-71a5-4011-9441-d0b5e4122711",
        album_title="OK Computer",
        naming_template=_TEMPLATE,
        target_files=list(files),
        year=1997,
        is_track=is_track,
        expected_tracks=list(expected_tracks),
    )


def _make_processor(tmp_path: Path, *, downloads=None, fingerprinter=None, verify=True):
    downloads = downloads or (tmp_path / "downloads")
    downloads.mkdir(parents=True, exist_ok=True)
    library = tmp_path / "library"
    library.mkdir(parents=True, exist_ok=True)
    manager = LibraryManager(
        LibraryDB(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    )
    client = _StubClient(downloads)
    fp = FileProcessor(
        AudioTagger(),
        naming_engine=NamingTemplateEngine(),
        library_manager=manager,
        library_paths=[library],
        client=client,
        slskd_downloads_path=downloads,
        fingerprinter=fingerprinter,
        verify_downloads=verify,
    )
    return fp, manager, client, library, downloads


@pytest.mark.asyncio
async def test_process_downloaded_imports_and_moves(tmp_path: Path):
    fp, manager, client, library, downloads = _make_processor(tmp_path)
    _place(downloads, "Radiohead - OK Computer/01 Airbag.flac")
    manifest = _manifest(ExpectedFile(filename="Radiohead - OK Computer/01 Airbag.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert result.failed == []
    moved = Path(result.succeeded[0])
    assert moved.exists()
    # source moved out of the download dir (no leftover)
    assert not (downloads / "Radiohead - OK Computer/01 Airbag.flac").exists()
    # tags written from the manifest
    tag, _ = AudioTagger().read_tags(moved)
    assert tag.album == "OK Computer"
    assert tag.musicbrainz_release_group_id == "rg-1"
    assert await manager.has_album("rg-1") is True
    # DEC-1: FileProcessor never touches slskd transfers
    client.cancel.assert_not_called()


@pytest.mark.asyncio
async def test_process_downloaded_duration_mismatch_quarantines(tmp_path: Path):
    fp, _manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "A/track.flac")
    # expected 500s but the fixture is a few seconds -> mismatch
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1, duration=500.0))

    result = await fp.process_downloaded(manifest)

    assert result.succeeded == []
    assert len(result.failed) == 1
    assert result.failed[0].reason == "duration_mismatch"


@pytest.mark.asyncio
async def test_process_downloaded_track_duration_mismatch_is_wrong_track_not_quarantined(
    tmp_path: Path,
):
    """For a per-track download (is_track), a duration mismatch means the WRONG
    recording was picked - it must be WRONG_TRACK (fail over), not a quarantinable
    duration_mismatch (which would globally blacklist an otherwise-good file)."""
    fp, _manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "A/track.flac")
    manifest = _manifest(
        ExpectedFile(filename="A/track.flac", size=1, duration=500.0), is_track=True
    )

    result = await fp.process_downloaded(manifest)

    assert result.succeeded == []
    assert result.failed[0].reason == WRONG_TRACK
    assert WRONG_TRACK not in QUARANTINE_REASONS


@pytest.mark.asyncio
async def test_process_downloaded_only_filenames_imports_subset(tmp_path: Path):
    """only_filenames restricts the import to the given subset - the file left out is
    not processed (so a never-arrived file can't be recorded as a failure)."""
    fp, _manager, _client, _library, downloads = _make_processor(tmp_path)
    _place(downloads, "A/one.flac")
    _place(downloads, "A/two.flac")
    manifest = _manifest(
        ExpectedFile(filename="A/one.flac", size=1),
        ExpectedFile(filename="A/two.flac", size=1),
    )

    result = await fp.process_downloaded(manifest, only_filenames={"A/one.flac"})

    assert len(result.succeeded) == 1
    assert result.failed == []
    assert (downloads / "A/two.flac").exists()   # the unlisted file was left untouched


@pytest.mark.asyncio
async def test_process_downloaded_continue_on_failure(tmp_path: Path):
    fp, _manager, _client, library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "A/good.flac")
    manifest = _manifest(
        ExpectedFile(filename="A/good.flac", size=1),
        ExpectedFile(filename="A/missing.flac", size=1),  # never placed
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1  # the good file imported despite the bad one
    assert len(result.failed) == 1
    assert result.failed[0].filename == "A/missing.flac"


@pytest.mark.asyncio
async def test_process_downloaded_import_error_is_per_file(tmp_path: Path):
    """A move/tag/DB error during the mutating phase fails THAT file (reason
    import-failed, not a quarantine reason) and the batch continues - it must not
    abort and orphan files already imported earlier in the loop."""
    import msgspec

    fp, manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "A/good.flac")
    _place(downloads, "A/bad.flac")
    # Distinct titles AND track numbers -> two distinct tracks with distinct library
    # targets (no same-target collision, no position-dedup) and a stable signal for the
    # flaky tagger now that tags are written on the staged copy.
    tagger = AudioTagger()
    for rel, title, track in (("A/good.flac", "Good Track", 1), ("A/bad.flac", "Bad Track", 2)):
        existing, _ = tagger.read_tags(downloads / rel)
        tagger.write_mb_tags(
            downloads / rel, msgspec.structs.replace(existing, title=title, track_number=track)
        )

    real_write = AudioTagger().write_album_identity
    def flaky_write(path, tag):
        if tag.title == "Bad Track":
            raise OSError("disk full")
        return real_write(path, tag)
    fp._tagger.write_album_identity = flaky_write

    manifest = _manifest(
        ExpectedFile(filename="A/good.flac", size=1),
        ExpectedFile(filename="A/bad.flac", size=1),
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1   # good file imported despite the bad one
    assert len(result.failed) == 1
    assert result.failed[0].filename == "A/bad.flac"
    from services.native.file_processor import IMPORT_FAILED, QUARANTINE_REASONS
    assert result.failed[0].reason == IMPORT_FAILED
    assert IMPORT_FAILED not in QUARANTINE_REASONS   # a local I/O error must not blacklist the peer
    assert await manager.has_album("rg-1") is True


@pytest.mark.asyncio
async def test_process_downloaded_cross_mount_copies(tmp_path: Path, monkeypatch):
    """slskd's dir and the library are often SEPARATE mounts (Docker volumes) where
    os.replace raises EXDEV. The importer must fall back to copy, still import the
    file, and remove the slskd source (no leftover / doubled storage)."""
    import errno
    import os as _os

    fp, manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "A/track.flac")
    src = downloads / "A/track.flac"

    real_replace = _os.replace
    def fake_replace(a, b):
        # only the source -> staging move (out of the downloads dir) is "cross-mount"
        if str(a).startswith(str(downloads)):
            raise OSError(errno.EXDEV, "Invalid cross-device link")
        return real_replace(a, b)
    monkeypatch.setattr(_os, "replace", fake_replace)

    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))
    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert Path(result.succeeded[0]).exists()
    assert not src.exists()              # slskd source removed after the cross-mount copy
    assert not (downloads / "A").exists()  # and the now-empty leftover folder pruned
    assert await manager.has_album("rg-1") is True


@pytest.mark.asyncio
async def test_process_downloaded_cross_mount_survives_metadata_rejection(
    tmp_path: Path, monkeypatch
):
    """Cross-mount import on a filesystem that rejects metadata copies (TrueNAS NFSv4
    ACLs refuse chmod/utime even for the owner, so copystat raises). The bytes still
    copy and the file imports - the import must not fail the whole download over the
    cosmetic metadata copy2 used to insist on."""
    import errno
    import os as _os

    fp, manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "A/track.flac")
    src = downloads / "A/track.flac"

    real_replace = _os.replace
    def fake_replace(a, b):
        if str(a).startswith(str(downloads)):  # only the cross-mount source->staging move
            raise OSError(errno.EXDEV, "Invalid cross-device link")
        return real_replace(a, b)
    monkeypatch.setattr(_os, "replace", fake_replace)

    def reject_metadata(*_a, **_k):
        raise PermissionError(errno.EPERM, "Operation not permitted")
    monkeypatch.setattr(shutil, "copystat", reject_metadata)

    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))
    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert Path(result.succeeded[0]).exists()
    assert not src.exists()
    assert await manager.has_album("rg-1") is True


@pytest.mark.asyncio
async def test_process_downloaded_prunes_empty_leftover_dirs(tmp_path: Path):
    """After the file is moved out, the now-empty folders slskd created are pruned -
    walking up nested dirs - but never the downloads mount root itself."""
    fp, _manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "Artist - Album/CD1/track.flac")
    manifest = _manifest(ExpectedFile(filename="Artist - Album/CD1/track.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert not (downloads / "Artist - Album" / "CD1").exists()
    assert not (downloads / "Artist - Album").exists()
    assert downloads.exists()  # the mount root must survive


@pytest.mark.asyncio
async def test_process_downloaded_keeps_dir_with_remaining_sibling(tmp_path: Path):
    """A half-imported album - a sibling file still in the same folder - must keep its
    folder: rmdir only removes empty dirs, so pending tracks/cover art are never lost."""
    fp, _manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    _place(downloads, "A/good.flac")
    (downloads / "A" / "cover.jpg").write_bytes(b"jpg")  # keeps the dir non-empty
    manifest = _manifest(ExpectedFile(filename="A/good.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert (downloads / "A").exists()                 # not pruned: sibling remains
    assert (downloads / "A" / "cover.jpg").exists()


@pytest.mark.asyncio
async def test_process_downloaded_mount_unavailable(tmp_path: Path):
    missing_mount = tmp_path / "nope"  # never created
    fp, _manager, _client, _library, _downloads = _make_processor(
        tmp_path, downloads=tmp_path / "real", verify=False
    )
    fp._slskd_downloads_path = missing_mount
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert result.succeeded == []
    assert result.failed[0].reason == DOWNLOADS_MOUNT_UNAVAILABLE


@pytest.mark.asyncio
async def test_process_downloaded_fingerprint_mismatch_on_wrong_artist(tmp_path: Path):
    # Rejected only when AcoustID confidently names a DIFFERENT artist (the manifest artist
    # is "Radiohead").
    fingerprinter = MagicMock()
    fingerprinter.fingerprint = AsyncMock(
        return_value=FingerprintResult(status="pass", score=0.95, artist="Coldplay", title="Yellow")
    )
    fp, _manager, _client, _library, downloads = _make_processor(
        tmp_path, fingerprinter=fingerprinter, verify=True
    )
    _place(downloads, "A/track.flac")
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1), rg="rg-1")

    result = await fp.process_downloaded(manifest)

    assert result.succeeded == []
    assert result.failed[0].reason == "fingerprint_mismatch"


@pytest.mark.asyncio
async def test_process_downloaded_fingerprint_title_check_armed_for_single(tmp_path: Path):
    """When the manifest carries the ONE expected track (a track download or a
    1-track single), a confident AcoustID hit naming a clearly different SONG is
    rejected even when the artist matches - the per-file path used to run
    artist-only (2026-07-05 wrong-single incident, P1.4)."""
    fingerprinter = MagicMock()
    fingerprinter.fingerprint = AsyncMock(
        return_value=FingerprintResult(
            status="pass", score=0.95, artist="Radiohead", title="Completely Other Song"
        )
    )
    fp, _manager, _client, _library, downloads = _make_processor(
        tmp_path, fingerprinter=fingerprinter, verify=True
    )
    _place(downloads, "A/track.flac")
    manifest = _manifest(
        ExpectedFile(filename="A/track.flac", size=1),
        expected_tracks=[ExpectedTrack(track_number=1, title="Airbag")],
    )

    result = await fp.process_downloaded(manifest)

    assert result.succeeded == []
    assert result.failed[0].reason == "fingerprint_mismatch"


@pytest.mark.asyncio
async def test_process_downloaded_fingerprint_title_check_skipped_without_expected_track(
    tmp_path: Path,
):
    # No expected track on the manifest (a multi-file album import) -> artist-only,
    # exactly the pre-existing behaviour.
    fingerprinter = MagicMock()
    fingerprinter.fingerprint = AsyncMock(
        return_value=FingerprintResult(
            status="pass", score=0.95, artist="Radiohead", title="Completely Other Song"
        )
    )
    fp, manager, _client, _library, downloads = _make_processor(
        tmp_path, fingerprinter=fingerprinter, verify=True
    )
    _place(downloads, "A/track.flac")
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1  # artist agrees -> imported
    assert await manager.has_album("rg-1") is True


@pytest.mark.asyncio
async def test_process_downloaded_fingerprint_accepts_right_artist_other_release_group(tmp_path: Path):
    # The Thin Lizzy bug: a VALID track whose AcoustID release-group list doesn't include
    # the requested one (incomplete RG coverage / reissue) must NOT be rejected - only a
    # wrong artist/song is.
    fingerprinter = MagicMock()
    fingerprinter.fingerprint = AsyncMock(
        return_value=FingerprintResult(
            status="pass", score=0.95, artist="Radiohead", release_group_ids=["some-other-rg"]
        )
    )
    fp, manager, _client, _library, downloads = _make_processor(
        tmp_path, fingerprinter=fingerprinter, verify=True
    )
    _place(downloads, "A/track.flac")
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1), rg="rg-1")

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1   # right artist, different RG -> imported, not rejected
    assert await manager.has_album("rg-1") is True


@pytest.mark.asyncio
async def test_process_downloaded_fingerprint_failopen_when_disabled(tmp_path: Path):
    fingerprinter = MagicMock()
    fingerprinter.fingerprint = AsyncMock(return_value=FingerprintResult(status="disabled"))
    fp, manager, _client, _library, downloads = _make_processor(
        tmp_path, fingerprinter=fingerprinter, verify=True
    )
    _place(downloads, "A/track.flac")
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1   # disabled fingerprint never blocks the import
    assert await manager.has_album("rg-1") is True


@pytest.mark.asyncio
async def test_process_downloaded_missing_file_is_not_quarantined(tmp_path: Path):
    """slskd 'delivered' a file we can't find on a healthy mount (folder-name
    sanitisation / nesting / mount-path mismatch). That's a local/config fault, NOT a
    bad peer: it must fail with SOURCE_FILE_MISSING, which is not a quarantine reason,
    so the peer is never blacklisted (AUD: 'watched it finish in slskd, then failed')."""
    from services.native.file_processor import SOURCE_FILE_MISSING

    fp, _manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    (downloads / "A").mkdir(parents=True, exist_ok=True)  # folder present, file absent
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert result.succeeded == []
    assert result.failed[0].reason == SOURCE_FILE_MISSING
    assert SOURCE_FILE_MISSING not in QUARANTINE_REASONS


@pytest.mark.asyncio
async def test_process_downloaded_skips_duplicate_track_position(tmp_path: Path):
    """A second source file for a track the album already holds (a re-pull or a
    different-format copy: same disc+track, different path) is skipped, not written as
    a duplicate, and its leftover slskd source is removed. The completeness count
    already deduped by position - this stops the *files* doubling on disk."""
    import msgspec

    fp, manager, _client, _library, downloads = _make_processor(tmp_path, verify=False)
    tagger = AudioTagger()
    # two different on-disk sources, both album track 1 (the fixture is track 1, disc 1);
    # only the title differs, so they render to different library paths
    for rel, title in (("A/first.flac", "Take One"), ("B/second.flac", "Take Two")):
        _place(downloads, rel)
        existing, _ = tagger.read_tags(downloads / rel)
        tagger.write_mb_tags(downloads / rel, msgspec.structs.replace(existing, title=title))

    r1 = await fp.process_downloaded(_manifest(ExpectedFile(filename="A/first.flac", size=1)))
    assert len(r1.succeeded) == 1

    r2 = await fp.process_downloaded(
        _manifest(ExpectedFile(filename="B/second.flac", size=1), task_id="t2")
    )

    # the duplicate position counts a success (already present) but isn't re-written
    assert len(r2.succeeded) == 1
    assert r2.failed == []
    assert Path(r2.succeeded[0]) == Path(r1.succeeded[0])   # points at the kept copy
    rows = await manager.get_file_rows_for_album("rg-1")
    assert len(rows) == 1                                   # NO duplicate row
    assert not (downloads / "B" / "second.flac").exists()  # leftover source removed


@pytest.mark.asyncio
async def test_process_downloaded_crash_idempotency(tmp_path: Path):
    fp, manager, _client, library, downloads = _make_processor(tmp_path, verify=False)
    # Simulate a prior run: a library row already exists for this task + remote file,
    # but the source has been moved out of the download dir (gone).
    tag, info = AudioTagger().read_tags(_FLAC)
    existing_path = library / "Radiohead" / "OK Computer (1997)" / "already.flac"
    existing_path.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(_FLAC, existing_path)
    await manager.upsert_file(
        existing_path, tag, info,
        release_group_mbid="rg-1", source="download",
        download_task_id="t1", source_path=str(downloads / "A/track.flac"),
    )
    # the album folder remains after the prior run moved the file out; the file is gone
    (downloads / "A").mkdir(parents=True, exist_ok=True)
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))  # source not placed

    result = await fp.process_downloaded(manifest)

    # reconciled as already-imported: counted success, no failure, no duplicate
    assert result.failed == []
    assert len(result.succeeded) == 1
    assert Path(result.succeeded[0]) == existing_path
    rows = await manager.get_file_rows_for_album("rg-1")
    assert len(rows) == 1


# --- folder-import matcher: title corroboration (Lidarr metadata-first) -----------

def _cand(tmp_path, *, filename, title, dur, mbid=None, track_no=1):
    from models.audio import AudioInfo, AudioTag
    from services.native.file_processor import _FolderCandidate

    return _FolderCandidate(
        path=tmp_path / filename,
        tag=AudioTag(title=title, artist="Led Zeppelin", album="Led Zeppelin",
                     track_number=track_no, musicbrainz_recording_id=mbid),
        info=AudioInfo(duration_seconds=dur, bitrate=900, sample_rate=44100,
                       channels=2, file_format="flac", file_size_bytes=1),
    )


def test_title_tag_naming_a_different_track_vetoes_a_duration_coincidence(tmp_path):
    # A live "Kashmir" (~8:30) whose length coincides with the studio "How Many More
    # Times" (8:28) must NOT pair - the title tag names a different track.
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _pair_score

    track = ExpectedTrack(track_number=9, duration_seconds=508.0, title="How Many More Times")
    live = _cand(tmp_path, filename="04 Kashmir.flac",
                 title="Kashmir (Live From Knebworth, 1979)", dur=510.0)
    assert _pair_score(live, track) is None


def test_matching_title_tag_pairs(tmp_path):
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _pair_score

    track = ExpectedTrack(track_number=9, duration_seconds=508.0, title="How Many More Times")
    right = _cand(tmp_path, filename="09 How Many More Times.flac",
                  title="How Many More Times", dur=510.0)
    assert _pair_score(right, track) is not None


def test_untagged_file_still_pairs_on_duration(tmp_path):
    # The D18 case: an obfuscated rip with NO title tag must still match on duration.
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _pair_score

    track = ExpectedTrack(track_number=9, duration_seconds=508.0, title="How Many More Times")
    untagged = _cand(tmp_path, filename="aHR0cHM6.part01.flac", title="", dur=510.0)
    assert _pair_score(untagged, track) is not None


def test_recording_mbid_overrides_a_title_conflict(tmp_path):
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _pair_score

    track = ExpectedTrack(track_number=9, duration_seconds=508.0, title="How Many More Times",
                          recording_mbid="rec-hmmt")
    # Odd title tag, but the recording MBID confirms identity -> pair.
    weird = _cand(tmp_path, filename="09.flac", title="Untitled", dur=510.0, mbid="rec-hmmt")
    assert _pair_score(weird, track) is not None


# --- fingerprint recording-identity check (release-group is NOT gated) ------------

def _fp(status="pass", title=None, artist=None, rgs=None):
    from models.audio import FingerprintResult
    return FingerprintResult(status=status, score=0.95, title=title, artist=artist,
                             release_group_ids=rgs or [])


def test_fingerprint_disagrees_on_wrong_song():
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _fingerprint_disagrees
    track = ExpectedTrack(track_number=9, title="How Many More Times")
    assert _fingerprint_disagrees(_fp(title="Kashmir", artist="Led Zeppelin"), track, "Led Zeppelin")


def test_fingerprint_allows_right_song_wrong_release_group():
    # The Thin Lizzy bug: right song+artist, RG list excludes the requested one -> allow.
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _fingerprint_disagrees
    track = ExpectedTrack(track_number=1, title="Soldier of Fortune")
    fp = _fp(title="Soldier of Fortune", artist="Thin Lizzy", rgs=["a-different-rg"])
    assert _fingerprint_disagrees(fp, track, "Thin Lizzy") is False


def test_fingerprint_allows_reissue_bonus_title_variant():
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _fingerprint_disagrees
    track = ExpectedTrack(track_number=10, title="Killer Without a Cause (BBC Session 01-08-77)")
    fp = _fp(title="Killer Without a Cause", artist="Thin Lizzy")
    assert _fingerprint_disagrees(fp, track, "Thin Lizzy") is False


def test_fingerprint_fails_open_on_non_pass():
    from models.download_manifest import ExpectedTrack
    from services.native.file_processor import _fingerprint_disagrees
    track = ExpectedTrack(track_number=1, title="Anything")
    assert _fingerprint_disagrees(_fp(status="error"), track, "Artist") is False
    assert _fingerprint_disagrees(_fp(status="disabled"), track, "Artist") is False


def test_fingerprint_skips_artist_check_for_various_artists():
    # A V/A compilation track's performing artist legitimately differs from the album
    # artist - don't reject on that.
    from services.native.file_processor import _fingerprint_disagrees
    fp = _fp(title="Some Song", artist="Specific Band")
    assert _fingerprint_disagrees(fp, None, "Various Artists") is False


# --- import-time wrong-album guard (#1, tagless-safe) -------------------------------------

def _fc(album: str, *, artist: str = "Led Zeppelin", album_artist: str | None = None) -> _FolderCandidate:
    tag = AudioTag(
        title="t", artist=artist, album=album, album_artist=album_artist,
        track_number=1, disc_number=1,
    )
    info = AudioInfo(
        duration_seconds=200.0, bitrate=900, sample_rate=44100, channels=2,
        file_format="flac", file_size_bytes=1, bit_depth=16,
    )
    return _FolderCandidate(path=Path("x.flac"), tag=tag, info=info)


def test_import_guard_rejects_a_different_album_by_tags():
    # An obfuscated "Led Zeppelin" release that unpacks to Led Zeppelin II-tagged files.
    cands = [_fc("Led Zeppelin II") for _ in range(5)]
    assert _folder_names_wrong_album(cands, "Led Zeppelin", "Led Zeppelin") is True


def test_import_guard_accepts_the_correct_album():
    cands = [_fc("Led Zeppelin") for _ in range(5)]
    assert _folder_names_wrong_album(cands, "Led Zeppelin", "Led Zeppelin") is False


def test_import_guard_is_tagless_safe():
    # No album tag on any file -> can't judge -> never rejects (falls through to duration match).
    cands = [_fc("") for _ in range(5)]
    assert _folder_names_wrong_album(cands, "Led Zeppelin", "Led Zeppelin") is False


def test_import_guard_is_edition_tolerant():
    # A deluxe/edition suffix is NOT a different album.
    cands = [_fc("OK Computer Deluxe Edition", artist="Radiohead") for _ in range(5)]
    assert _folder_names_wrong_album(cands, "Radiohead", "OK Computer") is False


def test_import_guard_keeps_album_when_only_a_minority_mis_tagged():
    # One oddly-tagged file (a mislabeled bonus track) must not reject the whole album (owner Q1).
    cands = [_fc("Led Zeppelin"), _fc("Led Zeppelin"), _fc("Led Zeppelin II")]
    assert _folder_names_wrong_album(cands, "Led Zeppelin", "Led Zeppelin") is False


# -- held imports (capture on verify-fail + force-import) --


@pytest.mark.asyncio
async def test_fingerprint_mismatch_holds_file_for_review(tmp_path: Path):
    import sqlite3 as _sqlite

    from infrastructure.persistence.download_store import DownloadStore

    lock = threading.Lock()
    db_path = tmp_path / "library.db"
    conn = _sqlite.connect(db_path)
    conn.execute("CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)")
    conn.execute("INSERT OR IGNORE INTO auth_users VALUES ('user-a','alice','user')")
    conn.commit()
    conn.close()
    store = DownloadStore(db_path=db_path, write_lock=lock)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="Radiohead",
        album_title="OK Computer", source="usenet",
    )
    held_dir = tmp_path / "held"
    library = tmp_path / "library"
    library.mkdir()
    downloads = tmp_path / "downloads"
    downloads.mkdir()
    manager = LibraryManager(LibraryDB(db_path=db_path, write_lock=lock))
    fingerprinter = MagicMock()
    fingerprinter.fingerprint = AsyncMock(
        return_value=FingerprintResult(status="pass", score=0.99, artist="Coldplay", title="Yellow")
    )
    fp = FileProcessor(
        AudioTagger(), naming_engine=NamingTemplateEngine(), library_manager=manager,
        library_paths=[library], client=_StubClient(downloads), slskd_downloads_path=downloads,
        fingerprinter=fingerprinter, verify_downloads=True, download_store=store, held_dir=held_dir,
    )
    _place(downloads, "A/track.flac")
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1), task_id=task.id, rg="rg-1")

    result = await fp.process_downloaded(manifest)

    assert result.failed and result.failed[0].reason == "fingerprint_mismatch"
    held = await store.list_held_imports("user-a", "user")
    assert len(held) == 1
    assert held[0].release_group_mbid == "rg-1"
    assert held[0].source_task_id == task.id
    assert held[0].artist_mbid == "a74b1b7f-71a5-4011-9441-d0b5e4122711"  # kept for "import anyway"
    assert held[0].evidence_artist == "Coldplay"  # what AcoustID thought it was
    assert held[0].held_path.startswith(str(held_dir))
    assert Path(held[0].held_path).exists()  # copied into the held area (survives cleanup)


@pytest.mark.asyncio
async def test_place_held_file_imports_bypassing_verify(tmp_path: Path):
    from models.held_import import HeldImport

    fp, manager, _client, _library, _downloads = _make_processor(tmp_path)
    held_dir = tmp_path / "held"
    held_dir.mkdir()
    held_file = held_dir / "src.flac"
    shutil.copy(_FLAC, held_file)
    held = HeldImport(
        id=1, user_id="user-a", held_path=str(held_file), reason="fingerprint_mismatch",
        source="usenet", status="held", created_at=0.0, release_group_mbid="rg-9",
        release_mbid="rel-9", recording_mbid="rec-3", track_number=3, disc_number=1,
        track_title="You Shook Me", artist_name="Led Zeppelin", album_title="Led Zeppelin I",
        year=1969, naming_template=_TEMPLATE,
        artist_mbid="678d88b2-87b0-403b-b63d-5da7465aecc3",
    )

    target = await fp.place_held_file(held)

    assert Path(target).exists()  # no fingerprinter wired -> verify is skipped, it just imports
    assert "0103" in Path(target).name  # placed at disc 1 / track 3 per the naming template
    tag, _ = AudioTagger().read_tags(Path(target))
    assert tag.musicbrainz_release_group_id == "rg-9"  # album MBID stamped -> rescan-safe
    # album-artist MBID stamped too, so the library never invents a synthetic
    # artist id that would split this artist into two entries
    assert tag.musicbrainz_album_artist_id == "678d88b2-87b0-403b-b63d-5da7465aecc3"
    assert await manager.has_album("rg-9") is True


# -- P2: the file's OWN tags vs the requested identity (2026-07-05 wrong-single) --


def _retag(downloads: Path, rel: str, **tags) -> None:
    """Overwrite tags on a placed fixture with raw mutagen (house rule: never
    validate the tagger against its own round-trip)."""
    from mutagen.flac import FLAC

    audio = FLAC(downloads / rel)
    for key, value in tags.items():
        if value is None:
            audio.pop(key, None)
        else:
            audio[key] = value
    audio.save()


def _held_wired_processor(tmp_path: Path, *, verify=True, fingerprinter=None):
    """A processor with download_store + held_dir armed (the tag/duration holds need
    them), plus a real task row so _hold_for_review can resolve the owner."""
    import sqlite3 as _sqlite

    from infrastructure.persistence.download_store import DownloadStore

    lock = threading.Lock()
    db_path = tmp_path / "library.db"
    conn = _sqlite.connect(db_path)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
    )
    conn.execute("INSERT OR IGNORE INTO auth_users VALUES ('user-a','alice','user')")
    conn.commit()
    conn.close()
    store = DownloadStore(db_path=db_path, write_lock=lock)
    held_dir = tmp_path / "held"
    library = tmp_path / "library"
    library.mkdir()
    downloads = tmp_path / "downloads"
    downloads.mkdir()
    manager = LibraryManager(LibraryDB(db_path=db_path, write_lock=lock))
    fp = FileProcessor(
        AudioTagger(), naming_engine=NamingTemplateEngine(), library_manager=manager,
        library_paths=[library], client=_StubClient(downloads), slskd_downloads_path=downloads,
        fingerprinter=fingerprinter, verify_downloads=verify,
        download_store=store, held_dir=held_dir,
    )
    return fp, store, manager, downloads


def _single_manifest(task_id, *, expected_duration=None, hold_on_wrong_track=False,
                     canonical=None, expected_title="the arrival"):
    """A 1-track single manifest as the P1 enqueue writes it (Yan Qing / the arrival)."""
    return DownloadManifest(
        task_id=task_id,
        source_username="Fabrizio83a",
        release_group_mbid="rg-single",
        artist_name="Yan Qing",
        album_title="the arrival",
        naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1, duration=expected_duration)],
        year=2026,
        is_track=expected_duration is not None,
        hold_on_wrong_track=hold_on_wrong_track,
        expected_tracks=[
            ExpectedTrack(
                track_number=1, title=expected_title,
                recording_mbid="rec-180ceef5", duration_seconds=canonical,
            )
        ],
    )


@pytest.mark.asyncio
async def test_tagged_wrong_file_held_without_acoustid(tmp_path: Path):
    """The incident file (tagged Dan Romer / Arrival in Ashford) on a NULL-length
    single: no AcoustID coverage, duration gate unarmed - the file's own tags are the
    only signal, and they must hold it. This is the exact fail-open hole P2 closes."""
    fp, store, manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Yan Qing",
        album_title="the arrival",
    )
    _place(downloads, "A/track.flac")
    _retag(downloads, "A/track.flac", title="Arrival in Ashford", artist="Dan Romer",
           album="A Knight of the Seven Kingdoms (Season 1)")

    result = await fp.process_downloaded(_single_manifest(task.id))

    assert result.succeeded == []
    assert result.failed[0].reason == "tag_mismatch"
    assert await manager.has_album("rg-single") is False
    held = await store.list_held_imports("user-a", "user")
    assert len(held) == 1
    assert held[0].reason == "tag_mismatch"
    assert held[0].evidence_artist == "Dan Romer"        # what the tags said
    assert held[0].evidence_title == "Arrival in Ashford"
    assert held[0].track_title == "the arrival"           # what was requested


@pytest.mark.asyncio
async def test_feat_credit_artist_tag_imports(tmp_path: Path):
    # token_set is subset-tolerant: a featuring credit is the same artist, not a
    # conflict (all three known-legit cases in the library share this shape).
    fp, store, manager, downloads = _held_wired_processor(tmp_path)
    await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="Radiohead",
        album_title="OK Computer",
    )
    _place(downloads, "A/track.flac")
    _retag(downloads, "A/track.flac", artist="Radiohead feat. Someone Else")
    manifest = _manifest(ExpectedFile(filename="A/track.flac", size=1))

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1


@pytest.mark.asyncio
async def test_classical_composer_tag_imports(tmp_path: Path):
    """Classical rips tag the COMPOSER as artist while the album artist is the
    performer - an artist conflict alone must never hold; the strong title +
    agreeable duration carve-out is load-bearing."""
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-sym", artist_name="Berliner Philharmoniker",
        album_title="Beethoven: Symphony No. 9",
    )
    _place(downloads, "A/track.flac")
    _retag(downloads, "A/track.flac", artist="Ludwig van Beethoven",
           title="Symphony No. 9 in D minor, Op. 125 'Choral'")
    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-sym",
        artist_name="Berliner Philharmoniker", album_title="Beethoven: Symphony No. 9",
        naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1)],
        expected_tracks=[
            ExpectedTrack(track_number=1, title="Symphony No. 9 in D minor, Op. 125")
        ],
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert await store.list_held_imports("user-a", "user") == []


@pytest.mark.asyncio
async def test_untagged_file_never_tag_held(tmp_path: Path):
    # Fully untagged rips fall through to duration/filename matching (D18) -
    # absence of tags is unknown, not a conflict.
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Yan Qing",
        album_title="the arrival",
    )
    _place(downloads, "A/track.flac")
    from mutagen.flac import FLAC

    audio = FLAC(downloads / "A/track.flac")
    audio.delete()
    audio.save()

    result = await fp.process_downloaded(_single_manifest(task.id))

    assert len(result.succeeded) == 1


@pytest.mark.asyncio
async def test_degraded_manifest_album_tag_conflict_held(tmp_path: Path):
    """Identity threading failed (MB down at request time): no expected track, but a
    wrong-artist file whose ALBUM tag names other content still holds - the degraded
    task must not silently lose all protection (review should-fix #8)."""
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Yan Qing",
        album_title="the arrival",
    )
    _place(downloads, "A/track.flac")
    _retag(downloads, "A/track.flac", title="Arrival in Ashford", artist="Dan Romer",
           album="A Knight of the Seven Kingdoms (Season 1)")
    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-single",
        artist_name="Yan Qing", album_title="the arrival", naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1)],
    )

    result = await fp.process_downloaded(manifest)

    assert result.failed and result.failed[0].reason == "tag_mismatch"
    held = await store.list_held_imports("user-a", "user")
    assert len(held) == 1


@pytest.mark.asyncio
async def test_verify_off_skips_tag_check(tmp_path: Path):
    fp, store, _manager, downloads = _held_wired_processor(tmp_path, verify=False)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Yan Qing",
        album_title="the arrival",
    )
    _place(downloads, "A/track.flac")
    _retag(downloads, "A/track.flac", title="Arrival in Ashford", artist="Dan Romer")

    result = await fp.process_downloaded(_single_manifest(task.id))

    assert len(result.succeeded) == 1  # verification is the owner's existing toggle


@pytest.mark.asyncio
async def test_repull_duration_mismatch_holds_for_review(tmp_path: Path):
    """D9: the last-resort re-pull keeps the gate ON and captures the closest match
    in held-imports ('import anyway' is the human path), instead of importing an
    unverified file silently."""
    fp, store, manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Yan Qing",
        album_title="the arrival",
    )
    _place(downloads, "A/track.flac")  # fixture is 0.3s vs expected 155.556s

    result = await fp.process_downloaded(
        _single_manifest(
            task.id, expected_duration=155.556, canonical=155.556, hold_on_wrong_track=True
        )
    )

    assert result.failed and result.failed[0].reason == WRONG_TRACK
    assert await manager.has_album("rg-single") is False
    held = await store.list_held_imports("user-a", "user")
    assert len(held) == 1
    assert held[0].reason == WRONG_TRACK
    assert held[0].track_title == "the arrival"
    assert held[0].recording_mbid == "rec-180ceef5"


@pytest.mark.asyncio
async def test_ordinary_wrong_track_failover_does_not_hold(tmp_path: Path):
    # Without the re-pull flag a WRONG_TRACK failure just fails over - holding a copy
    # of every failover candidate would spam the held area.
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Yan Qing",
        album_title="the arrival",
    )
    _place(downloads, "A/track.flac")

    result = await fp.process_downloaded(
        _single_manifest(task.id, expected_duration=155.556, canonical=155.556)
    )

    assert result.failed and result.failed[0].reason == WRONG_TRACK
    assert await store.list_held_imports("user-a", "user") == []


# -- P2.5: usenet NULL-length singles - a tagged title must NAME the expected track --


def _pair(tag_title, *, track_number=1):
    from models.audio import AudioInfo, AudioTag
    from services.native.file_processor import _FolderCandidate

    tag = AudioTag(title=tag_title, artist="X", album="Y", track_number=track_number)
    info = AudioInfo(
        duration_seconds=0.0, bitrate=900, sample_rate=44100, channels=2,
        file_format="flac", file_size_bytes=1,
    )
    return _FolderCandidate(Path(f"{track_number:02d} - file.flac"), tag, info)


def test_pair_score_sole_track_null_length_requires_containment_title():
    from services.native.file_processor import _pair_score

    track = ExpectedTrack(track_number=1, title="the arrival")  # no MB length

    # positional coincidence, tag names other content -> vetoed
    assert _pair_score(_pair("Arrival in Ashford"), track, sole_track=True) is None
    # correct tag -> matches
    assert _pair_score(_pair("the arrival"), track, sole_track=True) is not None
    # untagged falls through to the position match (D18)
    assert _pair_score(_pair(""), track, sole_track=True) is not None
    # multi-track releases keep today's looser semantics (the <50 veto only)
    assert _pair_score(_pair("Arrival in Ashford"), track, sole_track=False) is not None


# -- P4: position-dedup livelock fix (incident review R3) + row_covers_track --


def _seed_library_row(db_path: Path, *, track_number, track_title, duration,
                      recording_mbid=None, file_path="/lib/squatter.flac", rg="rg-single"):
    import sqlite3 as _sqlite
    import time as _time
    import uuid as _uuid

    conn = _sqlite.connect(db_path)
    conn.execute(
        """INSERT INTO library_files
           (id, release_group_mbid, recording_mbid, disc_number, track_number,
            track_title, album_title, file_path, file_size_bytes, file_mtime,
            duration_seconds, file_format, source, imported_at)
           VALUES (?, ?, ?, 1, ?, ?, 'the arrival', ?, 1, 0, ?, 'flac', 'download', ?)""",
        (_uuid.uuid4().hex, rg, recording_mbid, track_number, track_title,
         file_path, duration, _time.time()),
    )
    conn.commit()
    conn.close()


def test_row_covers_track_table():
    from services.native.file_processor import row_covers_track

    expected = {"recording_mbid": "rec-1", "title": "the arrival", "duration_seconds": 155.556}
    # recording identity decides when both sides know it
    assert row_covers_track({"recording_mbid": "REC-1"}, **expected)
    assert not row_covers_track({"recording_mbid": "rec-other"}, **expected)
    # duration decides next
    assert row_covers_track({"duration_seconds": 154.0}, **expected)
    assert not row_covers_track({"duration_seconds": 137.24}, **expected)
    # containment title decides next ("in ashford" is a different work)
    assert row_covers_track({"track_title": "01 - the arrival"}, **expected | {"duration_seconds": None})
    assert not row_covers_track(
        {"track_title": "Arrival in Ashford"}, **expected | {"duration_seconds": None}
    )
    # nothing measurable on either side -> unknown, counts as covering (D18)
    assert row_covers_track({}, recording_mbid=None, title=None, duration_seconds=None)


@pytest.mark.asyncio
async def test_wrong_squatter_does_not_swallow_correct_import(tmp_path: Path):
    """R3 livelock regression: a wrong file squatting on the expected track's
    position must NOT cause the verified new file to be unlinked as its
    'duplicate'. The new file imports alongside; the squatter stays (D5)."""
    fp, store, manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Radiohead",
        album_title="the arrival",
    )
    _seed_library_row(
        tmp_path / "library.db", track_number=1,
        track_title="Arrival in Ashford", duration=137.24,
    )
    _place(downloads, "A/track.flac")  # the fixture: Airbag by Radiohead, track 1

    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-single",
        artist_name="Radiohead", album_title="the arrival", naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1)],
        expected_tracks=[ExpectedTrack(track_number=1, title="Airbag")],
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    imported = Path(result.succeeded[0])
    assert imported.exists()                      # the correct file LANDED
    assert imported.name != "squatter.flac"       # alongside, not swallowed
    rows = await manager.get_file_rows_for_album("rg-single")
    paths = {r["file_path"] for r in rows}
    assert "/lib/squatter.flac" in paths          # squatter kept for review (D5)
    assert str(imported) in paths


@pytest.mark.asyncio
async def test_covering_occupant_still_dedupes(tmp_path: Path):
    # The pre-existing dedup semantics are preserved when the occupying row IS the
    # expected track (a flac-vs-mp3 / re-pull duplicate).
    fp, store, manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-single", artist_name="Radiohead",
        album_title="the arrival",
    )
    squatter = tmp_path / "library" / "existing.flac"
    squatter.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(_FLAC, squatter)
    _seed_library_row(
        tmp_path / "library.db", track_number=1, track_title="Airbag",
        duration=0.3, file_path=str(squatter),
    )
    _place(downloads, "A/track.flac")
    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-single",
        artist_name="Radiohead", album_title="the arrival", naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1)],
        expected_tracks=[ExpectedTrack(track_number=1, title="Airbag", duration_seconds=0.3)],
    )

    result = await fp.process_downloaded(manifest)

    assert result.succeeded == [str(squatter)]    # kept the existing copy
    assert not (downloads / "A/track.flac").exists()  # redundant source dropped


# -- P7 (D7): real attribution confidence at import, on the scanner's scale --


def _row_confidence(manager_db: Path, rg: str) -> float:
    import sqlite3 as _sqlite

    conn = _sqlite.connect(manager_db)
    row = conn.execute(
        "SELECT confidence FROM library_files WHERE release_group_mbid=? AND deleted_at IS NULL",
        (rg,),
    ).fetchone()
    conn.close()
    assert row is not None
    return row[0]


@pytest.mark.asyncio
async def test_confidence_1_0_when_recording_tag_confirms(tmp_path: Path):
    # the fixture carries musicbrainz_trackid rec-airbag-0001 - positive identity
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-c1", artist_name="Radiohead",
        album_title="OK Computer",
    )
    _place(downloads, "A/track.flac")
    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-c1",
        artist_name="Radiohead", album_title="OK Computer", naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1)],
        expected_tracks=[
            ExpectedTrack(track_number=1, title="Airbag", recording_mbid="rec-airbag-0001")
        ],
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert _row_confidence(tmp_path / "library.db", "rg-c1") == pytest.approx(1.0)


@pytest.mark.asyncio
async def test_confidence_0_9_when_canonical_duration_validated(tmp_path: Path):
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-c2", artist_name="Radiohead",
        album_title="the arrival",
    )
    _place(downloads, "A/track.flac")  # real duration ~0.3s
    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-c2",
        artist_name="Radiohead", album_title="the arrival", naming_template=_TEMPLATE,
        is_track=True,  # strict gate armed: duration IS the canonical length
        target_files=[ExpectedFile(filename="A/track.flac", size=1, duration=0.3)],
        expected_tracks=[
            # no title (unknown) and a NON-matching recording MBID, so neither the
            # 1.0 nor the 0.8 tier can fire - the canonical duration is the evidence
            ExpectedTrack(track_number=1, title=None, recording_mbid="rec-none",
                          duration_seconds=0.3)
        ],
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert _row_confidence(tmp_path / "library.db", "rg-c2") == pytest.approx(0.9)


@pytest.mark.asyncio
async def test_confidence_0_8_when_title_only_agrees(tmp_path: Path):
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-c3", artist_name="Radiohead",
        album_title="OK Computer",
    )
    _place(downloads, "A/track.flac")
    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-c3",
        artist_name="Radiohead", album_title="OK Computer", naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1)],  # no canonical duration
        expected_tracks=[
            ExpectedTrack(track_number=1, title="Airbag", recording_mbid="rec-a-different-one")
        ],
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert _row_confidence(tmp_path / "library.db", "rg-c3") == pytest.approx(0.8)


@pytest.mark.asyncio
async def test_confidence_0_6_when_uncorroborated(tmp_path: Path):
    # a plain multi-file album import: no expected track, no canonical duration -
    # merely "didn't conflict" is not identity evidence
    fp, store, _manager, downloads = _held_wired_processor(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-c4", artist_name="Radiohead",
        album_title="OK Computer",
    )
    _place(downloads, "A/track.flac")
    manifest = DownloadManifest(
        task_id=task.id, source_username="peer", release_group_mbid="rg-c4",
        artist_name="Radiohead", album_title="OK Computer", naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename="A/track.flac", size=1)],
    )

    result = await fp.process_downloaded(manifest)

    assert len(result.succeeded) == 1
    assert _row_confidence(tmp_path / "library.db", "rg-c4") == pytest.approx(0.6)


@pytest.mark.asyncio
async def test_import_anyway_stays_full_confidence(tmp_path: Path):
    # D8: the human's decision outranks the heuristics - place_held_file stamps 1.0
    from models.held_import import HeldImport

    fp, store, manager, downloads = _held_wired_processor(tmp_path)
    held_file = tmp_path / "held_src.flac"
    shutil.copy(_FLAC, held_file)
    held = HeldImport(
        id=1, user_id="user-a", held_path=str(held_file), reason="tag_mismatch",
        source="soulseek", status="held", created_at=0.0, release_group_mbid="rg-c5",
        release_mbid=None, recording_mbid="rec-x", track_number=1, disc_number=1,
        track_title="the arrival", artist_name="Yan Qing", album_title="the arrival",
        year=2026, naming_template=_TEMPLATE, artist_mbid=None,
    )

    await fp.place_held_file(held)

    assert _row_confidence(tmp_path / "library.db", "rg-c5") == pytest.approx(1.0)
