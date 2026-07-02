"""Replace-on-import (CollectionManagement D4/D10/D18/D19).

The safety invariants, exercised through the real import pipeline with temp dirs:
only ``origin='upgrade'`` replaces; only strictly-better replaces; the same-path
case recycles the old bytes BEFORE the in-place publish; the different-path case
soft-deletes the old row and recycles the old file (no duplicate active rows);
``place_held_file`` performs the D10 confirm-replace; no recycle bin = no replace.
"""

import shutil
import sqlite3
import threading
from pathlib import Path

import pytest

from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.library_db import LibraryDB
from models.audio import AudioInfo, AudioTag
from models.download_manifest import DownloadManifest, ExpectedFile, ExpectedTrack
from models.held_import import HeldImport
from services.native.file_processor import FileProcessor
from services.native.library_manager import LibraryManager
from services.native.naming import NamingTemplateEngine

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "library"
_FLAC = FIXTURES / "flac_full_01.flac"  # tags: Airbag / Radiohead / OK Computer, disc 1 track 1
_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"
# where the naming template places the imported flac fixture
_NEW_REL = "Radiohead/OK Computer (1997)/0101 Airbag.flac"


class _StubClient:
    def __init__(self, downloads_root: Path) -> None:
        self._root = downloads_root

    async def get_file_path(self, handle, remote_filename: str, size: int | None = None):
        return self._root / remote_filename.replace("\\", "/").lstrip("/")


def _make(tmp_path: Path, *, with_bin: bool = True, store: DownloadStore | None = None):
    downloads = tmp_path / "downloads"
    downloads.mkdir(parents=True, exist_ok=True)
    library = tmp_path / "library"
    library.mkdir(parents=True, exist_ok=True)
    bin_path = tmp_path / ".recycle"
    manager = LibraryManager(
        LibraryDB(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    )
    fp = FileProcessor(
        AudioTagger(),
        naming_engine=NamingTemplateEngine(),
        library_manager=manager,
        library_paths=[library],
        client=_StubClient(downloads),
        slskd_downloads_path=downloads,
        verify_downloads=False,
        download_store=store,
        recycle_bin=bin_path if with_bin else None,
    )
    return fp, manager, library, downloads, bin_path


def _manifest(*files: ExpectedFile, origin="user", rg="rg-1", tracks=None) -> DownloadManifest:
    return DownloadManifest(
        task_id="t1",
        source_username="peer",
        release_group_mbid=rg,
        artist_name="Radiohead",
        album_title="OK Computer",
        naming_template=_TEMPLATE,
        target_files=list(files),
        expected_tracks=tracks or [],
        year=1997,
        origin=origin,
    )


async def _seed_existing(
    manager: LibraryManager, path: Path, *, rg="rg-1", track=1,
    file_format="mp3", bit_rate=192, content=b"OLD-BYTES",
) -> Path:
    """A held library file at a known tier: physical bytes on disk + a row whose
    file_format/bit_rate drive the tier judgment."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    tag = AudioTag(
        title="Airbag", artist="Radiohead", album="OK Computer", album_artist="Radiohead",
        track_number=track, disc_number=1, year=1997, musicbrainz_release_group_id=rg,
    )
    info = AudioInfo(
        duration_seconds=200.0, bitrate=bit_rate, sample_rate=44100, channels=2,
        file_format=file_format, file_size_bytes=len(content),
    )
    await manager.upsert_file(
        path, tag, info, release_group_mbid=rg, recording_mbid=f"rec-{track}", source="scan"
    )
    return path


def _bin_files(bin_path: Path) -> list[Path]:
    return [p for p in bin_path.rglob("*") if p.is_file()]


@pytest.mark.asyncio
async def test_upgrade_replaces_worse_file_at_different_path(tmp_path: Path):
    fp, manager, library, downloads, bin_path = _make(tmp_path)
    old = await _seed_existing(manager, library / "Radiohead/OK Computer (1997)/old-copy.mp3")
    shutil.copy(_FLAC, downloads / "01 Airbag.flac")

    result = await fp.process_downloaded(
        _manifest(ExpectedFile(filename="01 Airbag.flac", size=1), origin="upgrade")
    )

    assert result.failed == []
    new_path = library / _NEW_REL
    assert result.succeeded == [str(new_path)]
    assert new_path.exists()
    assert not old.exists()  # recycled, never left in place
    recycled = _bin_files(bin_path)
    assert len(recycled) == 1 and recycled[0].read_bytes() == b"OLD-BYTES"  # moved, not deleted
    # exactly one ACTIVE row at the slot, pointing at the new file
    present = await manager.get_file_at_position("rg-1", 1, 1)
    assert present is not None and present["file_path"] == str(new_path)


@pytest.mark.asyncio
async def test_upgrade_never_replaces_equal_or_better(tmp_path: Path):
    fp, manager, library, downloads, bin_path = _make(tmp_path)
    old = await _seed_existing(
        manager, library / "Radiohead/OK Computer (1997)/old-copy.flac",
        file_format="flac", bit_rate=900,  # lossless: the incoming flac is only EQUAL
    )
    shutil.copy(_FLAC, downloads / "01 Airbag.flac")

    result = await fp.process_downloaded(
        _manifest(ExpectedFile(filename="01 Airbag.flac", size=1), origin="upgrade")
    )

    assert result.succeeded == [str(old)]  # dedup kept the existing copy
    assert old.exists()
    assert not (library / _NEW_REL).exists()
    assert _bin_files(bin_path) == []


@pytest.mark.asyncio
async def test_user_origin_import_never_replaces(tmp_path: Path):
    fp, manager, library, downloads, bin_path = _make(tmp_path)
    old = await _seed_existing(manager, library / "Radiohead/OK Computer (1997)/old-copy.mp3")
    shutil.copy(_FLAC, downloads / "01 Airbag.flac")

    result = await fp.process_downloaded(
        _manifest(ExpectedFile(filename="01 Airbag.flac", size=1), origin="user")
    )

    assert result.succeeded == [str(old)]
    assert old.exists()
    assert _bin_files(bin_path) == []


@pytest.mark.asyncio
async def test_same_path_upgrade_recycles_before_publish(tmp_path: Path):
    fp, manager, library, downloads, bin_path = _make(tmp_path)
    # the existing file sits at EXACTLY the path the new import resolves to
    old = await _seed_existing(manager, library / _NEW_REL)
    shutil.copy(_FLAC, downloads / "01 Airbag.flac")

    result = await fp.process_downloaded(
        _manifest(ExpectedFile(filename="01 Airbag.flac", size=1), origin="upgrade")
    )

    assert result.succeeded == [str(old)]
    recycled = _bin_files(bin_path)
    assert len(recycled) == 1 and recycled[0].read_bytes() == b"OLD-BYTES"  # old bytes preserved
    tag, info = AudioTagger().read_tags(old)  # the path now holds the NEW audio
    assert info.file_format == "flac"
    assert tag.title == "Airbag"
    present = await manager.get_file_at_position("rg-1", 1, 1)
    assert present is not None and present["file_path"] == str(old)


@pytest.mark.asyncio
async def test_no_recycle_bin_disables_replacement(tmp_path: Path):
    # An upgrade must never destroy the only copy of the old bytes: with no bin
    # configured, the import keeps the existing file (add-only behaviour).
    fp, manager, library, downloads, _bin = _make(tmp_path, with_bin=False)
    old = await _seed_existing(manager, library / "Radiohead/OK Computer (1997)/old-copy.mp3")
    shutil.copy(_FLAC, downloads / "01 Airbag.flac")

    result = await fp.process_downloaded(
        _manifest(ExpectedFile(filename="01 Airbag.flac", size=1), origin="upgrade")
    )

    assert result.succeeded == [str(old)]
    assert old.exists()


@pytest.mark.asyncio
async def test_folder_import_upgrade_replaces_at_position(tmp_path: Path):
    # The Usenet folder path shares the same seams: a strictly-better matched file
    # retires the old copy at the (disc, track) slot.
    fp, manager, library, _downloads, bin_path = _make(tmp_path)
    old = await _seed_existing(manager, library / "Radiohead/OK Computer (1997)/old-copy.mp3")
    job_dir = tmp_path / "job"
    job_dir.mkdir()
    shutil.copy(_FLAC, job_dir / "airbag.flac")
    manifest = _manifest(
        origin="upgrade",
        tracks=[ExpectedTrack(track_number=1, disc_number=1, duration_seconds=0.3, title="Airbag")],
    )

    result = await fp.process_downloaded_folder(manifest, [job_dir / "airbag.flac"])

    assert result.failed == []
    assert not old.exists()
    assert len(_bin_files(bin_path)) == 1
    present = await manager.get_file_at_position("rg-1", 1, 1)
    assert present is not None and present["file_path"] == str(library / _NEW_REL)


def _seed_auth_and_store(tmp_path: Path) -> DownloadStore:
    db_path = tmp_path / "downloads.db"
    conn = sqlite3.connect(db_path)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
    )
    conn.execute(
        "INSERT OR IGNORE INTO auth_users (id, username, role) VALUES ('user-a', 'a', 'admin')"
    )
    conn.commit()
    conn.close()
    return DownloadStore(db_path=db_path, write_lock=threading.Lock())


def _held(held_path: Path, *, task_id: str | None) -> HeldImport:
    return HeldImport(
        id=1, user_id="user-a", held_path=str(held_path), reason="fingerprint_mismatch",
        source="soulseek", status="held", created_at=0.0, release_group_mbid="rg-1",
        recording_mbid="rec-1", track_number=1, disc_number=1, track_title="Airbag",
        artist_name="Radiohead", album_title="OK Computer", year=1997,
        naming_template=_TEMPLATE, source_task_id=task_id,
    )


@pytest.mark.asyncio
async def test_place_held_file_performs_d10_confirm_replace(tmp_path: Path):
    # An upgrade whose AcoustID disagreed was held (never auto-swapped); the human's
    # "import anyway" performs the strictly-better replace.
    store = _seed_auth_and_store(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="Radiohead",
        album_title="OK Computer", origin="upgrade",
    )
    fp, manager, library, _downloads, bin_path = _make(tmp_path, store=store)
    old = await _seed_existing(manager, library / "Radiohead/OK Computer (1997)/old-copy.mp3")
    held_file = tmp_path / "held" / "src.flac"
    held_file.parent.mkdir()
    shutil.copy(_FLAC, held_file)

    target = await fp.place_held_file(_held(held_file, task_id=task.id))

    assert Path(target).exists()
    assert not old.exists()
    assert len(_bin_files(bin_path)) == 1  # old copy recycled, not deleted
    present = await manager.get_file_at_position("rg-1", 1, 1)
    assert present is not None and present["file_path"] == str(target)


@pytest.mark.asyncio
async def test_place_held_file_worse_upgrade_keeps_existing(tmp_path: Path):
    store = _seed_auth_and_store(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="Radiohead",
        album_title="OK Computer", origin="upgrade",
    )
    fp, manager, library, _downloads, bin_path = _make(tmp_path, store=store)
    old = await _seed_existing(
        manager, library / "Radiohead/OK Computer (1997)/old-copy.flac",
        file_format="flac", bit_rate=900,  # already lossless: the held flac is only equal
    )
    held_file = tmp_path / "held" / "src.flac"
    held_file.parent.mkdir()
    shutil.copy(_FLAC, held_file)

    target = await fp.place_held_file(_held(held_file, task_id=task.id))

    assert target == old
    assert old.exists() and old.read_bytes() == b"OLD-BYTES"
    assert _bin_files(bin_path) == []
    assert not held_file.exists()  # the rejected held copy is dropped, not imported


@pytest.mark.asyncio
async def test_place_held_file_uses_persisted_origin_when_task_is_gone(tmp_path: Path):
    """'Clear finished' can delete the upgrade task before the human
    reviews the held file - the origin persisted on the held row must keep the
    D10 confirm-replace semantics alive."""
    fp, manager, library, _downloads, bin_path = _make(tmp_path)  # no download store at all
    old = await _seed_existing(manager, library / "Radiohead/OK Computer (1997)/old-copy.mp3")
    held_file = tmp_path / "held" / "src.flac"
    held_file.parent.mkdir()
    shutil.copy(_FLAC, held_file)
    held = _held(held_file, task_id=None)
    held = type(held)(**{**{f: getattr(held, f) for f in held.__struct_fields__}, "origin": "upgrade"})

    target = await fp.place_held_file(held)

    assert Path(target).exists()
    assert not old.exists()
    assert len(_bin_files(bin_path)) == 1


@pytest.mark.asyncio
async def test_upgrade_rerun_after_crash_still_retires_the_old_file(tmp_path: Path):
    """Publish succeeded but the process died before retiring the old
    file - the re-run finds the target already present and must STILL replace."""
    fp, manager, library, downloads, bin_path = _make(tmp_path)
    old = await _seed_existing(manager, library / "Radiohead/OK Computer (1997)/old-copy.mp3")
    # the new file is already at its target from the crashed first run
    new_path = library / _NEW_REL
    new_path.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(_FLAC, new_path)
    shutil.copy(_FLAC, downloads / "01 Airbag.flac")

    result = await fp.process_downloaded(
        _manifest(ExpectedFile(filename="01 Airbag.flac", size=1), origin="upgrade")
    )

    assert result.failed == []
    assert not old.exists()  # retired despite the target-already-present branch
    assert len(_bin_files(bin_path)) == 1
    present = await manager.get_file_at_position("rg-1", 1, 1)
    assert present is not None and present["file_path"] == str(new_path)
