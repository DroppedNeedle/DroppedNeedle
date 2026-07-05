"""Native album removal (TODO 5): soft-delete by default, optional on-disk delete,
and the artist auto-removal cascade derived from remaining aggregated albums.

Drives a REAL LibraryDB + LibraryManager with on-disk temp files so the disk
unlink and the soft-delete are genuinely exercised, not mocked.
"""

import threading
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from infrastructure.persistence.library_db import LibraryDB
from models.audio import AudioInfo, AudioTag
from services.library_service import LibraryService
from services.native.library_manager import LibraryManager


def _info() -> AudioInfo:
    return AudioInfo(
        duration_seconds=100.0,
        bitrate=900,
        sample_rate=44100,
        channels=2,
        file_format="flac",
        file_size_bytes=10,
        bit_depth=16,
    )


async def _seed(
    manager: LibraryManager,
    path: Path,
    *,
    rg: str,
    artist_mbid: str,
    artist: str = "Artist",
    album: str = "Album",
    track: int = 1,
) -> None:
    path.write_bytes(b"x")
    tag = AudioTag(
        title=f"track {track}",
        artist=artist,
        album=album,
        album_artist=artist,
        track_number=track,
        disc_number=1,
        year=2000,
        musicbrainz_album_artist_id=artist_mbid,
    )
    await manager.upsert_file(
        path, tag, _info(), release_group_mbid=rg, recording_mbid=f"{rg}-rec{track}"
    )


def _service(db: LibraryDB) -> LibraryService:
    # cover_repo/memory_cache/disk_cache unused on this path; preferences mocked.
    return LibraryService(
        library_repo=LibraryManager(db),
        library_db=db,
        cover_repo=None,
        preferences_service=MagicMock(),
    )


@pytest.fixture
def db(tmp_path: Path) -> LibraryDB:
    return LibraryDB(db_path=tmp_path / "library.db", write_lock=threading.Lock())


@pytest.mark.asyncio
async def test_remove_album_soft_deletes_rows_but_keeps_files_on_disk(db, tmp_path):
    manager = LibraryManager(db)
    f1, f2 = tmp_path / "a1.flac", tmp_path / "a2.flac"
    await _seed(manager, f1, rg="rg-1", artist_mbid="art-1", track=1)
    await _seed(manager, f2, rg="rg-1", artist_mbid="art-1", track=2)

    resp = await _service(db).remove_album("rg-1", delete_files=False)

    assert resp.success is True
    assert resp.artist_removed is True  # sole album of the artist
    assert resp.artist_name == "Artist"
    assert await manager.has_album("rg-1") is False  # rows soft-deleted
    assert f1.exists() and f2.exists()  # recoverable: files untouched


@pytest.mark.asyncio
async def test_remove_album_delete_files_unlinks_from_disk(db, tmp_path):
    manager = LibraryManager(db)
    f1, f2 = tmp_path / "a1.flac", tmp_path / "a2.flac"
    await _seed(manager, f1, rg="rg-1", artist_mbid="art-1", track=1)
    await _seed(manager, f2, rg="rg-1", artist_mbid="art-1", track=2)

    resp = await _service(db).remove_album("rg-1", delete_files=True)

    assert resp.success is True
    assert not f1.exists() and not f2.exists()
    assert await manager.has_album("rg-1") is False


@pytest.mark.asyncio
async def test_remove_album_keeps_artist_with_other_albums(db, tmp_path):
    manager = LibraryManager(db)
    await _seed(manager, tmp_path / "a.flac", rg="rg-1", artist_mbid="art-1")
    await _seed(manager, tmp_path / "b.flac", rg="rg-2", artist_mbid="art-1")

    resp = await _service(db).remove_album("rg-1")

    assert resp.artist_removed is False
    assert resp.artist_name is None
    assert await manager.has_album("rg-2") is True


@pytest.mark.asyncio
async def test_remove_unknown_album_is_idempotent(db):
    # No files and no materialised row: a no-op success, not an error -
    # consistent with the removal preview, which also returns success for
    # unknown albums so users can clear ghost entries without failing on a
    # missing-files edge case.
    resp = await _service(db).remove_album("does-not-exist")
    assert resp.success is True


@pytest.mark.asyncio
async def test_removal_preview_flags_sole_album_artist(db, tmp_path):
    manager = LibraryManager(db)
    await _seed(manager, tmp_path / "a.flac", rg="rg-1", artist_mbid="art-1")

    preview = await _service(db).get_album_removal_preview("rg-1")

    assert preview.artist_will_be_removed is True
    assert preview.artist_name == "Artist"


@pytest.mark.asyncio
async def test_removal_preview_other_albums_keep_artist(db, tmp_path):
    manager = LibraryManager(db)
    await _seed(manager, tmp_path / "a.flac", rg="rg-1", artist_mbid="art-1")
    await _seed(manager, tmp_path / "b.flac", rg="rg-2", artist_mbid="art-1")

    preview = await _service(db).get_album_removal_preview("rg-1")

    assert preview.artist_will_be_removed is False
    assert preview.artist_name is None


@pytest.mark.asyncio
async def test_removal_preview_unknown_album_is_noop(db):
    preview = await _service(db).get_album_removal_preview("does-not-exist")

    assert preview.success is True
    assert preview.artist_will_be_removed is False


@pytest.mark.asyncio
async def test_count_artist_albums_matches_by_name_when_no_mbid(db, tmp_path):
    manager = LibraryManager(db)
    await _seed(manager, tmp_path / "a.flac", rg="rg-1", artist_mbid="", artist="NoMBID")

    by_name = await db.count_artist_albums(artist_mbid=None, artist_name="NoMBID")
    excluded = await db.count_artist_albums(
        artist_mbid=None, artist_name="NoMBID", exclude_release_group_mbid="rg-1"
    )

    assert by_name == 1
    assert excluded == 0


@pytest.mark.asyncio
async def test_delete_album_by_mbid_removes_materialised_row(db):
    # library_albums is the source /basic derives in_library from; it has no
    # soft-delete, so removal must hard-delete the row. mbid match is case-folded.
    await db.upsert_album({"mbid": "RG-1", "title": "Album", "artist_mbid": "art-1"})
    assert await db.get_album_by_mbid("rg-1") is not None

    await db.delete_album_by_mbid("rg-1")

    assert await db.get_album_by_mbid("RG-1") is None


@pytest.mark.asyncio
async def test_remove_album_clears_in_library_materialised_source(db, tmp_path):
    # Reproduce the stale "In Library" bug: an imported album leaves a row in BOTH
    # library_files (soft-deletable) and library_albums (the in_library source).
    # Removal must clear the materialised row too, with no re-scan/sync needed.
    manager = LibraryManager(db)
    await _seed(manager, tmp_path / "a.flac", rg="rg-1", artist_mbid="art-1")
    await db.upsert_album({"mbid": "rg-1", "title": "Album", "artist_mbid": "art-1"})
    assert await db.get_album_by_mbid("rg-1") is not None

    await _service(db).remove_album("rg-1")

    assert await db.get_album_by_mbid("rg-1") is None  # in_library now resolves false
    assert await manager.has_album("rg-1") is False


@pytest.mark.asyncio
async def test_soft_delete_album_files_returns_affected_paths(db, tmp_path):
    manager = LibraryManager(db)
    f1, f2 = tmp_path / "a1.flac", tmp_path / "a2.flac"
    await _seed(manager, f1, rg="rg-1", artist_mbid="art-1", track=1)
    await _seed(manager, f2, rg="rg-1", artist_mbid="art-1", track=2)

    paths = await db.soft_delete_album_files("rg-1")

    assert set(paths) == {str(f1), str(f2)}
    # idempotent: nothing left to soft-delete
    assert await db.soft_delete_album_files("rg-1") == []


# -- P5: single-file removal (the album page's orphan-review action) --


@pytest.mark.asyncio
async def test_remove_file_soft_deletes_row_and_unlinks(db, tmp_path):
    from core.exceptions import ResourceNotFoundError

    manager = LibraryManager(db)
    keep = tmp_path / "keep.flac"
    orphan = tmp_path / "orphan.flac"
    await _seed(manager, keep, rg="rg-1", artist_mbid="am-1", track=1)
    await _seed(manager, orphan, rg="rg-1", artist_mbid="am-1", track=2)
    rows = await db.get_library_files_for_album("rg-1")
    orphan_id = next(r["id"] for r in rows if r["file_path"] == str(orphan))
    service = _service(db)

    result = await service.remove_file(orphan_id)

    assert result.status == "ok"
    assert not orphan.exists()                       # audio unlinked
    assert keep.exists()                             # sibling untouched
    remaining = await db.get_library_files_for_album("rg-1")
    assert [r["file_path"] for r in remaining] == [str(keep)]
    # soft-deleted, not hard-deleted (recoverable via re-import)
    raw = await db.get_library_file_by_id(orphan_id)
    assert raw is not None and raw["deleted_at"] is not None
    # a second removal of the same id is a 404, not a crash
    with pytest.raises(ResourceNotFoundError):
        await service.remove_file(orphan_id)


@pytest.mark.asyncio
async def test_remove_file_last_file_drops_ghost_album_row(db, tmp_path):
    manager = LibraryManager(db)
    only = tmp_path / "only.flac"
    await _seed(manager, only, rg="rg-solo", artist_mbid="am-2")
    await db.upsert_album(
        {"mbid": "rg-solo", "artist_mbid": "am-2", "artist_name": "Artist", "title": "Album"}
    )
    rows = await db.get_library_files_for_album("rg-solo")
    service = _service(db)

    await service.remove_file(rows[0]["id"])

    # the materialised ledger row is gone too, so /basic stops saying In Library
    assert await db.get_album_by_mbid("rg-solo") is None


@pytest.mark.asyncio
async def test_remove_file_unknown_id_raises_not_found(db):
    from core.exceptions import ResourceNotFoundError

    service = _service(db)
    with pytest.raises(ResourceNotFoundError):
        await service.remove_file("no-such-file")
