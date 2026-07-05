from unittest.mock import AsyncMock, MagicMock

import pytest

from services.album_service import AlbumService


def _make_service() -> tuple[AlbumService, MagicMock, MagicMock]:
    library_repo = MagicMock()
    mb_repo = MagicMock()
    library_db = MagicMock()
    memory_cache = MagicMock()
    disk_cache = MagicMock()
    preferences_service = MagicMock()
    audiodb_image_service = MagicMock()

    service = AlbumService(
        library_repo=library_repo,
        mb_repo=mb_repo,
        library_db=library_db,
        memory_cache=memory_cache,
        disk_cache=disk_cache,
        preferences_service=preferences_service,
        audiodb_image_service=audiodb_image_service,
    )
    return service, library_repo, library_db


def _mb_release_group() -> dict:
    return {
        "title": "Album",
        "first-release-date": "2024-01-01",
        "primary-type": "Album",
        "disambiguation": "",
        "artist-credit": [],
    }


@pytest.mark.asyncio
async def test_get_album_basic_info_in_library_from_local_files_not_ledger():
    # in_library follows non-deleted local files, not the library_albums ledger row.
    # Files present with no ledger row must still report in_library.
    service, library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_mb_release_group())
    library_repo.get_requested_mbids = AsyncMock(return_value=set())
    library_repo.get_album_details = AsyncMock(return_value=None)
    library_db.has_album_files = AsyncMock(return_value=True)
    library_db.get_album_by_mbid = AsyncMock(return_value=None)

    result = await service.get_album_basic_info("8e1e9e51-38dc-4df3-8027-a0ada37d4674")

    assert result.in_library is True
    library_db.has_album_files.assert_awaited()
    library_db.get_album_by_mbid.assert_not_awaited()


@pytest.mark.asyncio
async def test_get_album_basic_info_not_in_library_when_files_gone_despite_ledger_row():
    # Files soft-deleted but a stale ledger row lingers: in_library follows the files.
    service, library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_mb_release_group())
    library_repo.get_requested_mbids = AsyncMock(return_value=set())
    library_repo.get_album_details = AsyncMock(return_value=None)
    library_db.has_album_files = AsyncMock(return_value=False)
    library_db.get_album_by_mbid = AsyncMock(return_value={"mbid": "stale"})

    result = await service.get_album_basic_info("8e1e9e51-38dc-4df3-8027-a0ada37d4674")

    assert result.in_library is False


_MBID = "8e1e9e51-38dc-4df3-8027-a0ada37d4674"


def _rg_with_ranked_release() -> dict:
    # find_primary_release picks the XW "worldwide" release regardless of size.
    return {
        "title": "Album",
        "first-release-date": "2024-01-01",
        "primary-type": "Album",
        "disambiguation": "",
        "artist-credit": [],
        "releases": [{"id": "deluxe-rel", "status": "Official", "country": "XW"}],
    }


def _release_with_tracks(n: int) -> dict:
    return {
        "id": "rel",
        "media": [
            {
                "position": 1,
                "tracks": [
                    {"position": i, "recording": {"title": f"T{i}", "id": f"rec{i}", "length": 200000}}
                    for i in range(1, n + 1)
                ],
            }
        ],
    }


def _release_by_owned_or_deluxe(owned_id: str, owned_n: int, deluxe_n: int):
    async def _get(release_id: str, includes=None, priority=None) -> dict:
        return _release_with_tracks(owned_n if release_id == owned_id else deluxe_n)

    return _get


@pytest.mark.asyncio
async def test_get_album_info_uses_owned_release_edition_not_the_larger_ranked_release():
    # An owned album shows the edition on disc (12 tracks), not the deluxe ranked release (28).
    service, _library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._apply_audiodb_album_images = AsyncMock(side_effect=lambda info, *a, **k: info)
    service._save_album_to_cache = AsyncMock()
    library_db.has_album_files = AsyncMock(return_value=True)
    library_db.get_album_release_mbid = AsyncMock(return_value="owned-rel")
    service._mb_repo.get_release_by_id = _release_by_owned_or_deluxe("owned-rel", owned_n=12, deluxe_n=28)

    result = await service.get_album_info(_MBID)

    assert result.total_tracks == 12
    library_db.get_album_release_mbid.assert_awaited_once()


@pytest.mark.asyncio
async def test_get_album_info_falls_back_to_ranked_release_when_not_owned():
    # No local files: keep the existing ranked-release behaviour, no owned lookup.
    service, _library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    service._apply_audiodb_album_images = AsyncMock(side_effect=lambda info, *a, **k: info)
    service._save_album_to_cache = AsyncMock()
    library_db.has_album_files = AsyncMock(return_value=False)
    service._check_in_library = AsyncMock(return_value=False)
    library_db.get_album_release_mbid = AsyncMock(return_value="owned-rel")
    service._mb_repo.get_release_by_id = _release_by_owned_or_deluxe("owned-rel", owned_n=12, deluxe_n=28)

    result = await service.get_album_info(_MBID)

    assert result.total_tracks == 28
    library_db.get_album_release_mbid.assert_not_awaited()


@pytest.mark.asyncio
async def test_get_album_tracks_info_prefers_owned_release_edition():
    service, _library_repo, library_db = _make_service()
    service._get_cached_album_info = AsyncMock(return_value=None)
    service._fetch_release_group = AsyncMock(return_value=_rg_with_ranked_release())
    library_db.get_album_release_mbid = AsyncMock(return_value="owned-rel")
    service._mb_repo.get_release_by_id = _release_by_owned_or_deluxe("owned-rel", owned_n=12, deluxe_n=28)

    result = await service.get_album_tracks_info(_MBID)

    assert result.total_tracks == 12
    assert len(result.tracks) == 12


# -- P5: annotate_album_coverage (shared matcher on the album page) --


def _lib_track(id, *, disc=1, track=0, title="", recording=None, duration=None):
    from services.native.library_manager import LibraryTrack

    return LibraryTrack(
        id=id, recording_mbid=recording, disc_number=disc, track_number=track,
        track_title=title, duration_seconds=duration,
    )


def _status(tracks):
    from services.native.library_manager import LibraryAlbumStatus

    return LibraryAlbumStatus(in_library=bool(tracks), track_count=len(tracks), tracks=tracks)


def _svc_self(tracks_info):
    """Bind the real method to a minimal fake self (AlbumService's ctor is heavy)."""
    from types import SimpleNamespace
    from unittest.mock import AsyncMock

    return SimpleNamespace(get_album_tracks_info=AsyncMock(return_value=tracks_info))


def _mb_tracks(*specs):
    from types import SimpleNamespace

    return SimpleNamespace(
        tracks=[
            SimpleNamespace(
                position=p, disc_number=d, title=t, recording_id=r, length=ln
            )
            for (p, d, t, r, ln) in specs
        ],
        total_tracks=len(specs),
    )


@pytest.mark.asyncio
async def test_annotate_coverage_incident_shape_all_orphans():
    """The incident's exact library state: one wrong file (position 2, 137 s, no
    recording) against a 1-track release - covered 0/1, the file is an orphan."""
    from services.album_service import AlbumService

    status = _status([
        _lib_track("f-wrong", track=2, title="Arrival in Ashford", duration=137.24)
    ])
    info = _mb_tracks((1, 1, "the arrival", "rec-180ceef5", 155556))

    out = await AlbumService.annotate_album_coverage(_svc_self(info), "rg-1", status)

    assert out.expected_tracks == 1
    assert out.covered_tracks == 0
    assert out.matched_file_ids == []
    assert [o.id for o in out.orphans] == ["f-wrong"]


@pytest.mark.asyncio
async def test_annotate_coverage_full_album_no_orphans():
    from services.album_service import AlbumService

    status = _status([
        _lib_track("f1", track=1, title="A", recording="rec-1", duration=200.0),
        _lib_track("f2", track=2, title="B", duration=210.0),
    ])
    info = _mb_tracks((1, 1, "A", "rec-1", 200000), (2, 1, "B", None, 210000))

    out = await AlbumService.annotate_album_coverage(_svc_self(info), "rg-1", status)

    assert (out.covered_tracks, out.expected_tracks) == (2, 2)
    assert sorted(out.matched_file_ids) == ["f1", "f2"]
    assert out.orphans == []


@pytest.mark.asyncio
async def test_annotate_coverage_fails_open_on_mb_error():
    from types import SimpleNamespace
    from unittest.mock import AsyncMock

    from services.album_service import AlbumService

    status = _status([_lib_track("f1", track=1, title="A")])
    fake = SimpleNamespace(get_album_tracks_info=AsyncMock(side_effect=RuntimeError("down")))

    out = await AlbumService.annotate_album_coverage(fake, "rg-1", status)

    assert out.expected_tracks == 0  # zeroed -> page falls back to presence-only
    assert out.orphans == []
