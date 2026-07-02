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
from models.download_manifest import DownloadManifest, ExpectedFile
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


def _manifest(*files: ExpectedFile, task_id="t1", rg="rg-1", is_track=False) -> DownloadManifest:
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
