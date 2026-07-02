"""Album edition selection (CollectionManagement Feature E, D13-D17).

Pin storage + pin-aware owned edition + the editions enumerator + cache-busting,
and the 'acquire this edition' fill/upgrade fan-out with edition scoping.
"""

import threading
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ResourceNotFoundError, ValidationError
from infrastructure.persistence.album_release_pin_store import AlbumReleasePinStore
from infrastructure.persistence.library_db import LibraryDB
from models.album import Track
from services.album_service import AlbumService

RG = "11111111-1111-4111-8111-111111111111"
REL_STD = "22222222-2222-4222-8222-222222222222"
REL_DELUXE = "33333333-3333-4333-8333-333333333333"


def _rg_payload() -> dict:
    return {
        "id": RG,
        "title": "OK Computer",
        "releases": [
            {
                "id": REL_STD, "title": "OK Computer", "status": "Official",
                "date": "1997-06-16", "country": "GB", "packaging": "Jewel Case",
                "media": [{"track-count": 12}],
            },
            {
                "id": REL_DELUXE, "title": "OK Computer", "disambiguation": "deluxe",
                "status": "Official", "date": "2009-03-24", "country": "XW",
                "media": [{"track-count": 12}, {"track-count": 8}],
            },
        ],
    }


async def _seed_owned_file(library_db: LibraryDB) -> None:
    await library_db.upsert_library_file(
        {
            "release_group_mbid": RG, "release_mbid": REL_STD, "track_number": 1,
            "disc_number": 1, "track_title": "Airbag", "album_title": "OK Computer",
            "file_path": "/m/a/01.flac", "file_size_bytes": 1, "file_mtime": 0.0,
            "file_format": "flac", "source": "scan", "confidence": 1.0, "is_compilation": 0,
        }
    )


def _make_album_service(tmp_path: Path):
    lock = threading.Lock()
    library_db = LibraryDB(db_path=tmp_path / "library.db", write_lock=lock)
    pins = AlbumReleasePinStore(db_path=tmp_path / "library.db", write_lock=lock)
    mb_repo = MagicMock()
    mb_repo.get_release_group_by_id = AsyncMock(return_value=_rg_payload())
    memory_cache = AsyncMock()
    disk_cache = AsyncMock()
    service = AlbumService(
        MagicMock(), mb_repo, library_db, memory_cache, disk_cache,
        MagicMock(), None, None, release_pin_store=pins,
    )
    return service, library_db, pins, memory_cache, disk_cache


@pytest.mark.asyncio
async def test_pin_store_roundtrip(tmp_path: Path):
    pins = AlbumReleasePinStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    assert await pins.get(RG) is None
    await pins.set(RG, REL_DELUXE, "admin-1")
    assert await pins.get(RG) == REL_DELUXE
    assert await pins.get(RG.upper()) == REL_DELUXE  # case-insensitive key
    await pins.set(RG, REL_STD, "admin-1")  # re-pin overwrites
    assert await pins.get(RG) == REL_STD
    assert await pins.clear(RG) is True
    assert await pins.get(RG) is None
    assert await pins.clear(RG) is False


@pytest.mark.asyncio
async def test_editions_enumerator_flags_owned_and_pinned(tmp_path: Path):
    service, library_db, pins, *_ = _make_album_service(tmp_path)
    # the library's files belong to the standard edition
    await _seed_owned_file(library_db)
    await pins.set(RG, REL_DELUXE)

    data = await service.list_editions(RG)

    by_id = {item["release_mbid"]: item for item in data["items"]}
    assert by_id[REL_STD]["is_owned"] is True
    assert by_id[REL_STD]["track_count"] == 12
    assert by_id[REL_DELUXE]["is_pinned"] is True
    assert by_id[REL_DELUXE]["track_count"] == 20  # multi-disc media summed
    assert by_id[REL_DELUXE]["disambiguation"] == "deluxe"
    assert data["pinned_release_mbid"] == REL_DELUXE
    assert data["owned_release_mbid"] == REL_STD


@pytest.mark.asyncio
async def test_pin_overrides_owned_and_clearing_reverts(tmp_path: Path):
    service, library_db, pins, *_ = _make_album_service(tmp_path)
    await _seed_owned_file(library_db)
    assert await service._owned_release_id(RG) == REL_STD  # mode-over-files

    await pins.set(RG, REL_DELUXE)
    assert await service._owned_release_id(RG) == REL_DELUXE  # pin wins

    await pins.clear(RG)
    assert await service._owned_release_id(RG) == REL_STD  # reverts to auto


@pytest.mark.asyncio
async def test_set_pin_validates_edition_and_busts_caches(tmp_path: Path):
    service, _db, pins, memory_cache, disk_cache = _make_album_service(tmp_path)

    with pytest.raises(ResourceNotFoundError):
        await service.set_edition_pin(RG, "44444444-4444-4444-8444-444444444444", "admin-1")
    assert await pins.get(RG) is None

    await service.set_edition_pin(RG, REL_DELUXE, "admin-1")
    assert await pins.get(RG) == REL_DELUXE
    disk_cache.delete_album.assert_awaited_with(RG)
    assert memory_cache.delete.await_count >= 2  # album info + library details keys

    memory_cache.delete.reset_mock()
    disk_cache.delete_album.reset_mock()
    assert await service.clear_edition_pin(RG) is True
    disk_cache.delete_album.assert_awaited_with(RG)


# --- acquire_edition (D13, edition-scoped) --------------------------------------


def _mb_track(pos: int, rec: str | None, title: str, disc: int = 1) -> Track:
    return Track(position=pos, title=title, disc_number=disc, length=200_000, recording_id=rec)


def _make_download_service(*, rows, tracks, upgrade_allowed=True, cutoff="lossless"):
    from services.native.download_service import DownloadService

    library = AsyncMock()
    library.get_file_rows_for_album.return_value = rows
    album_service = AsyncMock()
    album_service.resolve_edition.return_value = REL_DELUXE
    mb = MagicMock()
    mb.get_release_by_id = AsyncMock(return_value={"id": REL_DELUXE})
    meta = MagicMock()
    meta.artist_name = "Radiohead"
    meta.artist_id = "art-1"
    meta.title = "OK Computer"
    mb.get_release_group = AsyncMock(return_value=meta)
    service = DownloadService(
        AsyncMock(), AsyncMock(), AsyncMock(), library, AsyncMock(), AsyncMock(),
        MagicMock(), musicbrainz=mb, album_service=album_service,
        upgrade_allowed=upgrade_allowed, quality_cutoff=cutoff,
    )
    service.request_track = AsyncMock(return_value="task-fill")
    service.request_upgrade_track = AsyncMock(return_value="task-upg")
    import services.album_utils as album_utils

    original = album_utils.extract_tracks
    album_utils.extract_tracks = lambda release: (tracks, 0)
    return service, original


@pytest.mark.asyncio
async def test_acquire_edition_fills_missing_and_upgrades_below_cutoff(tmp_path: Path):
    rows = [
        # rec-1 held at lossless (at cutoff): untouched
        {"recording_mbid": "rec-1", "disc_number": 1, "track_number": 1,
         "file_format": "flac", "bit_rate": 900},
        # rec-2 held at mp3_192 (below cutoff): upgraded
        {"recording_mbid": "rec-2", "disc_number": 1, "track_number": 2,
         "file_format": "mp3", "bit_rate": 192},
    ]
    tracks = [
        _mb_track(1, "rec-1", "Airbag"),
        _mb_track(2, "rec-2", "Paranoid Android"),
        _mb_track(3, "rec-3", "Subterranean Homesick Alien"),  # missing -> filled
    ]
    service, original = _make_download_service(rows=rows, tracks=tracks)
    try:
        result = await service.acquire_edition("admin-1", RG)
    finally:
        import services.album_utils as album_utils

        album_utils.extract_tracks = original

    assert result == {
        "release_mbid": REL_DELUXE, "total_tracks": 3,
        "requested": 1, "upgrades": 1, "skipped": 0,
    }
    fill = service.request_track.await_args.kwargs
    assert fill["recording_mbid"] == "rec-3"
    assert fill["release_mbid"] == REL_DELUXE  # the edition as a soft target (D14)
    upgrade = service.request_upgrade_track.await_args.kwargs
    assert upgrade["recording_mbid"] == "rec-2"


@pytest.mark.asyncio
async def test_acquire_edition_is_edition_scoped(tmp_path: Path):
    """A low-tier BONUS track outside the pinned edition's tracklist must not
    trigger anything (the peer-review scoping trap)."""
    rows = [
        {"recording_mbid": "rec-1", "disc_number": 1, "track_number": 1,
         "file_format": "flac", "bit_rate": 900},
        # a bonus track the edition doesn't contain, at a terrible tier
        {"recording_mbid": "rec-bonus", "disc_number": 1, "track_number": 99,
         "file_format": "mp3", "bit_rate": 96},
    ]
    tracks = [_mb_track(1, "rec-1", "Airbag")]  # the edition = this one track
    service, original = _make_download_service(rows=rows, tracks=tracks)
    try:
        result = await service.acquire_edition("admin-1", RG)
    finally:
        import services.album_utils as album_utils

        album_utils.extract_tracks = original

    assert result["requested"] == 0
    assert result["upgrades"] == 0  # the bonus track never triggered an upgrade
    service.request_track.assert_not_awaited()
    service.request_upgrade_track.assert_not_awaited()


@pytest.mark.asyncio
async def test_acquire_edition_requires_a_resolvable_edition(tmp_path: Path):
    service, original = _make_download_service(rows=[], tracks=[])
    import services.album_utils as album_utils

    album_utils.extract_tracks = original
    service._album_service.resolve_edition = AsyncMock(return_value=None)
    with pytest.raises(ValidationError):
        await service.acquire_edition("admin-1", RG)
