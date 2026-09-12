"""Album edition selection (CollectionManagement Feature E, D13-D17).

Pin storage + pin-aware owned edition + the editions enumerator + cache-busting,
and the 'acquire this edition' fill/upgrade fan-out with edition scoping.
"""

import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import (
    ConflictError,
    ExternalServiceError,
    ResourceNotFoundError,
    ValidationError,
)
from infrastructure.cache.cache_keys import (
    ALBUM_INFO_PREFIX,
    ALBUM_TRACKS_INFO_PREFIX,
    LIBRARY_ALBUM_DETAILS_PREFIX,
)
from infrastructure.persistence.album_release_pin_store import AlbumReleasePinStore
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.queue.priority_queue import RequestPriority
from models.album import Track
from services.album_service import AlbumService, _edition_pick_metrics

RG = "11111111-1111-4111-8111-111111111111"
REL_STD = "22222222-2222-4222-8222-222222222222"
REL_DELUXE = "33333333-3333-4333-8333-333333333333"


def _rg_payload() -> dict:
    return {
        "id": RG,
        "title": "OK Computer",
        "releases": [
            {
                "id": REL_STD,
                "title": "OK Computer",
                "status": "Official",
                "date": "1997-06-16",
                "country": "GB",
                "packaging": "Jewel Case",
                "media": [{"track-count": 12}],
            },
            {
                "id": REL_DELUXE,
                "title": "OK Computer",
                "disambiguation": "deluxe",
                "status": "Official",
                "date": "2009-03-24",
                "country": "XW",
                "media": [{"track-count": 12}, {"track-count": 8}],
            },
        ],
    }


async def _seed_owned_file(library_db: LibraryDB) -> None:
    await library_db.upsert_library_file(
        {
            "release_group_mbid": RG,
            "release_mbid": REL_STD,
            "track_number": 1,
            "disc_number": 1,
            "track_title": "Airbag",
            "album_title": "OK Computer",
            "file_path": "/m/a/01.flac",
            "file_size_bytes": 1,
            "file_mtime": 0.0,
            "file_format": "flac",
            "source": "scan",
            "confidence": 1.0,
            "is_compilation": 0,
        }
    )


def _make_album_service(tmp_path: Path):
    lock = threading.Lock()
    library_db = LibraryDB(db_path=tmp_path / "library.db", write_lock=lock)
    pins = AlbumReleasePinStore(db_path=tmp_path / "library.db", write_lock=lock)
    mb_repo = MagicMock()
    mb_repo.get_release_group_by_id = AsyncMock(return_value=_rg_payload())
    memory_cache = AsyncMock()
    memory_cache.get.return_value = None
    disk_cache = AsyncMock()
    disk_cache.get_album.return_value = None
    service = AlbumService(
        MagicMock(),
        mb_repo,
        library_db,
        memory_cache,
        disk_cache,
        MagicMock(),
        None,
        None,
        release_pin_store=pins,
    )
    return service, library_db, pins, memory_cache, disk_cache


@pytest.mark.asyncio
async def test_pin_store_roundtrip(tmp_path: Path):
    pins = AlbumReleasePinStore(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
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
    assert data["selected_release_mbid"] == REL_DELUXE


@pytest.mark.asyncio
async def test_pin_overrides_owned_and_clearing_reverts(tmp_path: Path):
    service, library_db, pins, *_ = _make_album_service(tmp_path)
    await _seed_owned_file(library_db)
    assert (await service._effective_release_id(RG, _rg_payload()))[0] == REL_STD

    await pins.set(RG, REL_DELUXE)
    assert (await service._effective_release_id(RG, _rg_payload()))[0] == REL_DELUXE

    await pins.clear(RG)
    assert (await service._effective_release_id(RG, _rg_payload()))[0] == REL_STD


@pytest.mark.asyncio
async def test_exact_edition_tracklist_returns_only_requested_release(tmp_path: Path):
    service, *_ = _make_album_service(tmp_path)
    service._mb_repo.get_release_by_id = AsyncMock(
        return_value={
            "id": REL_DELUXE,
            "release-group": {"id": RG},
            "media": [
                {
                    "position": 1,
                    "tracks": [
                        {
                            "id": "66666666-6666-4666-8666-666666666666",
                            "position": 1,
                            "title": "Airbag",
                            "length": 276_000,
                            "recording": {"id": "77777777-7777-4777-8777-777777777777"},
                        }
                    ],
                }
            ],
        }
    )

    info = await service.get_exact_edition_tracks_info(RG, REL_DELUXE)

    assert info.selected_release_mbid == REL_DELUXE
    assert info.tracks[0].release_track_id == "66666666-6666-4666-8666-666666666666"
    service._mb_repo.get_release_by_id.assert_awaited_once_with(
        REL_DELUXE,
        includes=["recordings", "labels"],
        priority=RequestPriority.USER_INITIATED,
    )


@pytest.mark.asyncio
async def test_exact_edition_lookup_never_falls_back(tmp_path: Path):
    service, *_ = _make_album_service(tmp_path)
    service._mb_repo.get_release_by_id = AsyncMock(return_value=None)

    with pytest.raises(ResourceNotFoundError, match="exact edition"):
        await service.get_exact_edition_tracks_info(RG, REL_DELUXE)

    service._mb_repo.get_release_group_by_id.assert_not_awaited()


@pytest.mark.asyncio
async def test_exact_edition_lookup_preserves_provider_failure(tmp_path: Path):
    service, *_ = _make_album_service(tmp_path)
    service._mb_repo.get_release_by_id = AsyncMock(
        side_effect=ExternalServiceError("MusicBrainz unavailable")
    )

    with pytest.raises(ExternalServiceError, match="MusicBrainz unavailable"):
        await service.get_exact_edition_tracks_info(RG, REL_DELUXE)

    service._mb_repo.get_release_group_by_id.assert_not_awaited()


REL_AUTO_11 = "44444444-4444-4444-8444-444444444444"
REL_AUTO_20 = "55555555-5555-4555-8555-555555555555"
REL_AUTO_OTHER_11 = "66666666-6666-4666-8666-666666666666"


def _avalon_payload() -> dict:
    return {
        "id": RG,
        "title": "Avalon",
        "first-release-date": "2008-08-04",
        "artist-credit": [],
        "releases": [
            {
                "id": REL_AUTO_11,
                "status": "Official",
                "country": "XW",
                "date": "2008-08-04",
                "media": [{"track-count": 11}],
            },
            {
                "id": REL_AUTO_20,
                "status": "Official",
                "country": "US",
                "date": "2008-08-05",
                "media": [{"track-count": 20}],
            },
            {
                "id": REL_AUTO_OTHER_11,
                "status": "Official",
                "country": "US",
                "date": "2008-08-05",
                "media": [{"track-count": 11}],
            },
        ],
    }


def _release_payload(release_id: str, track_count: int) -> dict:
    return {
        "id": release_id,
        "media": [
            {
                "position": 1,
                "tracks": [
                    {
                        "position": position,
                        "recording": {
                            "id": f"recording-{position}",
                            "title": f"Track {position}",
                        },
                    }
                    for position in range(1, track_count + 1)
                ],
            }
        ],
    }


async def _seed_unidentified_files(library_db: LibraryDB, count: int) -> None:
    for position in range(1, count + 1):
        await library_db.upsert_library_file(
            {
                "release_group_mbid": RG,
                "release_mbid": None,
                "track_number": position,
                "disc_number": 1,
                "track_title": f"Track {position}",
                "album_title": "Avalon",
                "file_path": f"/m/avalon/{position:02d}.flac",
                "file_size_bytes": 1,
                "file_mtime": 0.0,
                "file_format": "flac",
                "source": "scan",
                "confidence": 1.0,
                "is_compilation": 0,
            }
        )


@pytest.mark.asyncio
async def test_twenty_unidentified_files_use_the_authoritative_edition(
    tmp_path: Path,
):
    service, library_db, _pins, *_ = _make_album_service(tmp_path)
    await _seed_unidentified_files(library_db, 20)
    service._mb_repo.get_release_group_by_id.return_value = _avalon_payload()

    async def release_by_id(release_id: str, **_kwargs) -> dict:
        count = 20 if release_id == REL_AUTO_20 else 11
        return _release_payload(release_id, count)

    service._mb_repo.get_release_by_id = AsyncMock(side_effect=release_by_id)
    service._apply_audiodb_album_images = AsyncMock(
        side_effect=lambda info, *_args, **_kwargs: info
    )
    service._save_album_to_cache = AsyncMock()

    full = await service.get_album_info(RG)
    tracks = await service.get_album_tracks_info(RG)
    editions = await service.list_editions(RG)
    acquisition_target = await service.resolve_edition(RG)

    assert full.selected_release_mbid == REL_AUTO_20
    assert full.total_tracks == 20
    # The tracks endpoint reports the authoritative selected edition; the
    # edition query and acquisition resolver agree on the same 20-track target.
    assert tracks.selected_release_mbid == REL_AUTO_20
    assert tracks.total_tracks == 20
    assert editions["selected_release_mbid"] == REL_AUTO_20
    assert editions["owned_release_mbid"] is None
    assert acquisition_target == REL_AUTO_20


@pytest.mark.asyncio
async def test_effective_selection_precedence_and_pin_clear(tmp_path: Path):
    service, library_db, pins, *_ = _make_album_service(tmp_path)
    await _seed_unidentified_files(library_db, 20)
    payload = _avalon_payload()

    await pins.set(RG, REL_AUTO_OTHER_11)
    assert (await service._effective_release_id(RG, payload))[0] == REL_AUTO_OTHER_11

    await pins.clear(RG)
    assert (await service._effective_release_id(RG, payload))[0] == REL_AUTO_20

    rows = await library_db.get_library_files_for_album(RG)
    for row in rows:
        row["release_mbid"] = REL_AUTO_11
    library_db.get_library_files_for_album = AsyncMock(return_value=rows)
    selected, owned, _pinned, _basis = await service._effective_release_id(RG, payload)
    assert selected == REL_AUTO_11
    assert owned == REL_AUTO_11


@pytest.mark.asyncio
async def test_effective_selection_falls_back_for_missing_evidence(
    tmp_path: Path,
):
    # No library rows plus a pin for an edition outside the group: nothing
    # to infer from, so the pick is the ranked first release. (Multi-album
    # ambiguity is a target-wiring case - see
    # test_target_two_albums_sharing_a_group_yield_no_library_evidence.)
    service, _library_db, pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(return_value=[])
    await pins.set(RG, "77777777-7777-4777-8777-777777777777")

    selected, owned, pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )

    assert selected == REL_AUTO_11
    assert owned is None
    assert pinned == "77777777-7777-4777-8777-777777777777"
    assert basis == "ranked"


@pytest.mark.asyncio
async def test_identically_tagged_duplicates_never_combine(tmp_path: Path):
    """Two active albums with the same unanimous embedded MBID (preserved
    Beets duplicates) still yield no embedded inference - unanimity is
    per-album, and cross-album rows never combine. Rows mirror the live
    target wiring (per-album ids throughout)."""
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=[
            {
                "local_album_id": album,
                "release_group_mbid": album,
                "embedded_release_mbid": REL_AUTO_11,
                "release_mbid": None,
                "provider_release_mbid": None,
            }
            for album in ("local-a", "local-b")
            for _ in range(10)
        ]
    )

    selected, owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )

    assert selected == REL_AUTO_11
    assert owned is None
    assert basis == "ranked"


@pytest.mark.asyncio
async def test_effective_selection_propagates_pin_and_library_lookup_failures(
    tmp_path: Path,
):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    service._release_pins = SimpleNamespace(
        get=AsyncMock(side_effect=ConflictError("multiple active albums"))
    )

    # Ambiguous pin reads degrade to unpinned so shared-RG pages still resolve;
    # only real lookup failures propagate.
    selected, _owned, pinned, _basis = await service._effective_release_id(
        RG, _avalon_payload()
    )
    assert pinned is None
    assert selected == REL_AUTO_11

    service._release_pins = None
    service._library_db.get_library_files_for_album = AsyncMock(
        side_effect=RuntimeError("library read failed")
    )
    with pytest.raises(RuntimeError, match="library read failed"):
        await service._effective_release_id(RG, _avalon_payload())


def test_closest_release_preserves_ranking_on_ties_and_ignores_unknown_counts():
    ranked = [
        {"id": "ranked-first", "media": [{"track-count": 11}]},
        {"id": "ranked-second", "media": [{"track-count": 21}]},
    ]
    assert AlbumService._closest_release_id(ranked, 16) == "ranked-first"
    assert (
        AlbumService._closest_release_id(
            [{"id": "missing"}, {"id": "invalid", "media": [{"track-count": "?"}]}],
            16,
        )
        is None
    )


@pytest.mark.asyncio
async def test_partial_collection_and_no_library_use_expected_fallbacks(tmp_path: Path):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=[
            {"local_album_id": "local-a", "release_group_mbid": "local-a"}
            for _ in range(18)
        ]
    )
    assert (await service._effective_release_id(RG, _avalon_payload()))[
        0
    ] == REL_AUTO_20

    service._library_db.get_library_files_for_album.return_value = []
    assert (await service._effective_release_id(RG, _avalon_payload()))[
        0
    ] == REL_AUTO_11


def _embedded_rows(
    embedded: str | None, count: int = 20, release_mbid: str | None = None
) -> list[dict]:
    # Target-wiring row shape (the live path): per-album ids throughout.
    return [
        {
            "local_album_id": "local-a",
            "release_group_mbid": "local-a",
            "embedded_release_mbid": embedded,
            "release_mbid": release_mbid,
            "provider_release_mbid": None,
        }
        for _ in range(count)
    ]


@pytest.mark.asyncio
async def test_embedded_tags_tier_beats_file_count_inference(tmp_path: Path):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_AUTO_11)
    )

    selected, owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )

    # 20 files would infer the 20-track edition; unanimous tags win instead,
    # and owned stays None (display inference is not identity evidence).
    assert selected == REL_AUTO_11
    assert owned is None
    assert basis == "embedded_tags"


@pytest.mark.asyncio
async def test_acquisition_resolver_follows_display_tags_win(tmp_path: Path):
    """Acquisition shares the display precedence: with unanimous embedded tags
    for the 11-track edition, resolve_edition picks it (not the 20-track
    file-count inference the ranked lane would prefer)."""
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_AUTO_11)
    )
    service._mb_repo.get_release_group_by_id.return_value = _avalon_payload()

    display_selected = (
        await service._effective_release_id(RG, _avalon_payload())
    )[0]

    assert await service.resolve_edition(RG) == REL_AUTO_11
    assert display_selected == REL_AUTO_11


@pytest.mark.asyncio
async def test_embedded_tags_fire_from_real_library_rows(tmp_path: Path):
    """The unanimous-embedded tier must work through the real LibraryDB,
    not just mocked row dicts. Seeds 20 scanned rows whose tags name the
    11-track edition and reads them back unmocked - file-count alone would
    infer the 20-track edition."""
    service, library_db, _pins, *_ = _make_album_service(tmp_path)
    for index in range(1, 21):
        await library_db.upsert_library_file(
            {
                "release_group_mbid": RG,
                "release_mbid": None,
                "embedded_release_mbid": REL_AUTO_11,
                "track_number": index,
                "disc_number": 1,
                "track_title": f"Track {index}",
                "album_title": "Avalon",
                "file_path": f"/m/avalon/{index:02d}.flac",
                "file_size_bytes": 1,
                "file_mtime": 0.0,
                "file_format": "flac",
                "source": "scan",
                "confidence": 1.0,
                "is_compilation": 0,
            }
        )

    selected, owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )

    assert selected == REL_AUTO_11
    assert owned is None
    assert basis == "embedded_tags"


@pytest.mark.asyncio
async def test_pin_and_owned_beat_embedded_tags(tmp_path: Path):
    service, _library_db, pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_AUTO_11)
    )

    await pins.set(RG, REL_AUTO_OTHER_11)
    selected, _owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )
    assert selected == REL_AUTO_OTHER_11
    assert basis == "pin"

    await pins.clear(RG)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_AUTO_11, release_mbid=REL_AUTO_20)
    )
    selected, owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )
    assert selected == REL_AUTO_20
    assert owned == REL_AUTO_20
    assert basis == "owned"


@pytest.mark.asyncio
async def test_partial_or_conflicting_embedded_tags_fall_through(tmp_path: Path):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)

    rows = _embedded_rows(REL_AUTO_11)
    rows[0]["embedded_release_mbid"] = None
    service._library_db.get_library_files_for_album = AsyncMock(return_value=rows)
    selected, _owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )
    assert selected == REL_AUTO_20
    assert basis == "file_count"

    rows = _embedded_rows(REL_AUTO_11)
    rows[0]["embedded_release_mbid"] = REL_AUTO_20
    service._library_db.get_library_files_for_album = AsyncMock(return_value=rows)
    selected, _owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )
    assert selected == REL_AUTO_20
    assert basis == "file_count"


@pytest.mark.asyncio
async def test_invalid_or_stale_embedded_tags_fall_through(tmp_path: Path):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)

    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows("not-a-mbid")
    )
    selected, _owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )
    assert selected == REL_AUTO_20
    assert basis == "file_count"

    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows("88888888-8888-4888-8888-888888888888")
    )
    selected, _owned, _pinned, basis = await service._effective_release_id(
        RG, _avalon_payload()
    )
    assert selected == REL_AUTO_20
    assert basis == "file_count"


REL_HEX_11 = "abcdefab-cdef-abcd-efab-cdefabcdefab"
REL_HEX_20 = "12345678-9abc-defa-bcde-fabcdefabcdef"


def _hex_payload() -> dict:
    return {
        "id": RG,
        "title": "Hex",
        "releases": [
            {
                "id": REL_HEX_11,
                "title": "Hex",
                "status": "Official",
                "date": "2008-08-04",
                "country": "XW",
                "media": [{"track-count": 11}],
            },
            {
                "id": REL_HEX_20,
                "title": "Hex",
                "status": "Official",
                "date": "2009-01-01",
                "country": "US",
                "media": [{"track-count": 20}],
            },
        ],
    }


@pytest.mark.asyncio
async def test_pick_membership_is_case_insensitive(tmp_path: Path):
    """Stored MBIDs keep their verbatim case; every membership check folds.
    Uses hex-letter MBIDs (upper() must actually change something)."""
    service, _library_db, pins, *_ = _make_album_service(tmp_path)

    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_HEX_11.upper())
    )
    selected, _owned, _pinned, basis = await service._effective_release_id(
        RG, _hex_payload()
    )
    assert selected == REL_HEX_11
    assert basis == "embedded_tags"

    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(None, release_mbid=REL_HEX_11.upper())
    )
    selected, owned, _pinned, basis = await service._effective_release_id(
        RG, _hex_payload()
    )
    assert selected == REL_HEX_11
    assert owned == REL_HEX_11.upper()
    assert basis == "owned"

    await pins.set(RG, REL_HEX_11.upper())
    service._library_db.get_library_files_for_album = AsyncMock(return_value=[])
    selected, _owned, pinned, basis = await service._effective_release_id(
        RG, _hex_payload()
    )
    assert selected == REL_HEX_11
    assert pinned == REL_HEX_11.upper()
    assert basis == "pin"


@pytest.mark.asyncio
async def test_list_editions_reports_selected_basis(tmp_path: Path):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    # list_editions resolves the RG payload (standard 12-track + deluxe 20-track);
    # 20 files would infer deluxe, but unanimous standard tags win.
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_STD)
    )

    editions = await service.list_editions(RG)

    assert editions["selected_release_mbid"] == REL_STD
    assert editions["selected_basis"] == "embedded_tags"


@pytest.mark.asyncio
async def test_effective_selection_counts_pick_basis(tmp_path: Path):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_AUTO_11)
    )
    before = dict(_edition_pick_metrics.snapshot().counters)

    await service._effective_release_id(RG, _avalon_payload())

    after = _edition_pick_metrics.snapshot().counters
    assert after.get("edition_pick:embedded_tags", 0) == (
        before.get("edition_pick:embedded_tags", 0) + 1
    )


@pytest.mark.asyncio
async def test_no_releases_yields_no_selection_or_basis(tmp_path: Path):
    service, _library_db, _pins, *_ = _make_album_service(tmp_path)
    service._library_db.get_library_files_for_album = AsyncMock(
        return_value=_embedded_rows(REL_AUTO_11)
    )

    selected, owned, pinned, basis = await service._effective_release_id(RG, {"id": RG})

    assert selected is None
    assert owned is None
    assert pinned is None
    assert basis is None


@pytest.mark.asyncio
async def test_set_pin_validates_edition_and_busts_caches(tmp_path: Path):
    service, _db, pins, memory_cache, disk_cache = _make_album_service(tmp_path)

    with pytest.raises(ResourceNotFoundError):
        await service.set_edition_pin(
            RG, "44444444-4444-4444-8444-444444444444", "admin-1"
        )
    assert await pins.get(RG) is None

    await service.set_edition_pin(RG, REL_DELUXE, "admin-1")
    assert await pins.get(RG) == REL_DELUXE
    disk_cache.delete_album.assert_awaited_with(RG)
    assert {item.args[0] for item in memory_cache.delete.await_args_list} == {
        f"{ALBUM_INFO_PREFIX}{RG}",
        f"{ALBUM_TRACKS_INFO_PREFIX}{RG}",
        f"{LIBRARY_ALBUM_DETAILS_PREFIX}{RG}",
    }

    memory_cache.delete.reset_mock()
    disk_cache.delete_album.reset_mock()
    assert await service.clear_edition_pin(RG) is True
    disk_cache.delete_album.assert_awaited_with(RG)


# --- acquire_edition (D13, edition-scoped) --------------------------------------


def _mb_track(pos: int, rec: str | None, title: str, disc: int = 1) -> Track:
    return Track(
        position=pos, title=title, disc_number=disc, length=200_000, recording_id=rec
    )


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
        AsyncMock(),
        AsyncMock(),
        AsyncMock(),
        library,
        AsyncMock(),
        AsyncMock(),
        MagicMock(),
        musicbrainz=mb,
        album_service=album_service,
        upgrade_allowed=upgrade_allowed,
        quality_cutoff=cutoff,
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
        {
            "recording_mbid": "rec-1",
            "disc_number": 1,
            "track_number": 1,
            "file_format": "flac",
            "bit_rate": 900,
        },
        # rec-2 held at mp3_192 (below cutoff): upgraded
        {
            "recording_mbid": "rec-2",
            "disc_number": 1,
            "track_number": 2,
            "file_format": "mp3",
            "bit_rate": 192,
        },
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
        "release_mbid": REL_DELUXE,
        "total_tracks": 3,
        "requested": 1,
        "upgrades": 1,
        "skipped": 0,
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
        {
            "recording_mbid": "rec-1",
            "disc_number": 1,
            "track_number": 1,
            "file_format": "flac",
            "bit_rate": 900,
        },
        # a bonus track the edition doesn't contain, at a terrible tier
        {
            "recording_mbid": "rec-bonus",
            "disc_number": 1,
            "track_number": 99,
            "file_format": "mp3",
            "bit_rate": 96,
        },
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


# ---------------------------------------------------------------------------
# Phase-0 shared RG fixture (LibraryFindings-All X-04 step 0.3, E-01/E-04
# pre-work): 10-track-US vs 12-track-XW, mirrored in
# tests/repositories/test_edition_policy.py (recall order) and
# tests/services/native/test_album_evidence_engine.py (decide order).
# ---------------------------------------------------------------------------

_SHARED_RG_DIVERGENCE = "rg-shared-edition-divergence"
_SHARED_REL_US_10 = {
    "id": "rel-ffff-us-10",
    "status": "Official",
    "date": "2024-01-31",
    "country": "US",
    "media": [{"track-count": 10}],
}
_SHARED_REL_XW_12 = {
    "id": "rel-0000-xw-12",
    "status": "Official",
    "date": "2024",
    "country": "XW",
    "media": [{"track-count": 12}],
}
_SHARED_TARGET_TRACKS = 10


@pytest.mark.skip(reason="pending D2")
def test_shared_rg_display_order_documents_divergence_from_recall():
    """0.3 display lane, as-documented-divergent (owner ruled D2 = document
    divergence): the display ranking has no count/date/Official signals and
    sorts XW-first, while recall ranks the 10-track US edition first by
    proximity. This skip is PERMANENT by owner ruling - do not flip it in
    2.4 (recall-vs-decide agreement is asserted in the engine suite)."""
    from repositories.edition_policy import recall_key
    from services.album_utils import get_ranked_releases

    payload = {
        "id": _SHARED_RG_DIVERGENCE,
        "title": "Album",
        "releases": [_SHARED_REL_US_10, _SHARED_REL_XW_12],
    }
    assert [release["id"] for release in get_ranked_releases(payload)] == [
        _SHARED_REL_XW_12["id"],
        _SHARED_REL_US_10["id"],
    ]
    assert sorted(
        [_SHARED_REL_US_10, _SHARED_REL_XW_12],
        key=lambda release: recall_key(release, _SHARED_TARGET_TRACKS),
    ) == [_SHARED_REL_US_10, _SHARED_REL_XW_12]


# ---------------------------------------------------------------------------
# Target pin wiring (LibraryFindings-All E-03 step 4.8, test a)
# ---------------------------------------------------------------------------


def _seed_target_copy(db_path: Path, album_id: str, track_id: str, rg: str) -> None:
    """One indexed local album carrying ``rg`` via raw sqlite (house pattern:
    seed prerequisite tables directly, never through another store)."""
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY)"
        )
        connection.execute(
            "INSERT OR IGNORE INTO local_artists "
            "(id, display_name, folded_name, kind, created_at, updated_at) "
            "VALUES ('artist-1', 'Artist', 'artist', 'person', 1.0, 1.0)"
        )
        connection.execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, "
            "title_folded, album_artist_id, grouping_source, created_at, "
            "updated_at) VALUES (?, 'root-a', 'k', 'Album', 'album', "
            "'artist-1', 'manual', 1.0, 1.0)",
            (album_id,),
        )
        connection.execute(
            "INSERT INTO local_album_external_identities (local_album_id, "
            "provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 3)",
            (album_id, rg),
        )
        connection.execute(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, "
            "relative_path, path_hash, file_size_bytes, file_mtime_ns, "
            "stat_revision, title, title_folded, album_title, "
            "album_title_folded, file_format, ingest_source, imported_at, "
            "membership_source) "
            "VALUES (?, ?, 'root-a', ?, ?, 'h', 1, 0, 's', 'T', 't', 'Album', "
            "'album', 'flac', 'scan', 1.0, 'manual')",
            (track_id, album_id, f"/m/{track_id}.flac", f"{track_id}.flac"),
        )
        connection.commit()


@pytest.mark.asyncio
async def test_target_wiring_pinned_release_id_resolves_single_and_degrades_on_shared(
    tmp_path: Path,
):
    """E-03: on target wiring ``_pinned_release_id`` reads the RG through the
    ``TargetAlbumReleasePinStore`` adapter. A pin on the single copy carrying
    the RG resolves; once a second indexed copy shares the RG the adapter
    raises ``ConflictError`` and the read degrades to None (the degrade
    comment's behavior), while the per-copy reads stay isolated."""
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from services.native.target_reference_adapters import TargetAlbumReleasePinStore

    db_path = tmp_path / "target.db"
    store = NativeLibraryStore(db_path=db_path, write_lock=threading.Lock())
    service = object.__new__(AlbumService)
    service._release_pins = TargetAlbumReleasePinStore(store)
    service._native_library_store = store

    _seed_target_copy(db_path, "album-a", "track-a", RG)
    await service._release_pins.set("album-a", REL_DELUXE, "admin-1")
    assert await service._pinned_release_id(RG) == REL_DELUXE

    _seed_target_copy(db_path, "album-b", "track-b", RG)
    with pytest.raises(ConflictError):
        await service._release_pins.get(RG)
    assert await service._pinned_release_id(RG) is None
    # No-cross-read rule: the pin on copy A never leaks into copy B's read.
    assert await service.get_edition_pin_for_local_album("album-a") == REL_DELUXE
    assert await service.get_edition_pin_for_local_album("album-b") is None
