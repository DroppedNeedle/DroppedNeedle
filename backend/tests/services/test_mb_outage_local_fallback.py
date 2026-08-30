"""Provider outages use local rows; true 404s stay not found.

Typed MusicBrainz failures degrade locally for known rows and propagate for
non-library IDs; provider 404s remain ResourceNotFoundError.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import ExternalServiceError, ResourceNotFoundError


from services.album_service import AlbumService
from services.artist_service import ArtistService

RG_ID = "e9d2f3f6-68d0-30c7-a0af-031f36b14b81"
LOCAL_ALBUM_ID = "local-album-1"
ARTIST_ID = "88d17133-abbc-42db-9526-4e2c1db60336"


def _ownership_rows():
    return [
        {
            "local_album_id": LOCAL_ALBUM_ID,
            "title": "Outage Album",
            "album_artist_name": "Local Artist",
            "year": 2026,
            "release_group_mbid": RG_ID,
        }
    ]


def _track_rows():
    return [
        {
            "track_number": 1,
            "disc_number": 1,
            "track_title": "Local Song",
            "duration_seconds": 180.0,
            "recording_mbid": "rec-1",
            "release_track_mbid": "rt-1",
            "provider_release_mbid": "release-1",
        }
    ]


def _album_service_with_store(ownership, tracks):
    store = MagicMock()
    store.target_album_ownership_rows = AsyncMock(return_value=ownership)
    store.get_target_album_tracks = AsyncMock(return_value=tracks)
    store.list_target_albums = AsyncMock(
        return_value=([{"provider_artist_mbid": "artist-mbid-1"}], 1)
    )
    mb_repo = MagicMock()
    mb_repo.get_release_group_by_id = AsyncMock(return_value=None)
    mb_repo.get_release_group_id_from_release = AsyncMock(return_value=None)
    memory_cache = MagicMock()
    memory_cache.get = AsyncMock(return_value=None)
    disk_cache = MagicMock()
    disk_cache.get_album = AsyncMock(return_value=None)
    service = AlbumService(
        library_repo=MagicMock(),
        mb_repo=mb_repo,
        library_db=MagicMock(),
        memory_cache=memory_cache,
        disk_cache=disk_cache,
        preferences_service=MagicMock(),
        native_library_store=store,
    )
    # _provider_album_id passes identifiers through untouched.
    service._ownership = None
    return service


def _artist_service_with_store(artist_rows, *, provider_error: Exception | None = None):
    store = MagicMock()
    store.list_target_artists = AsyncMock(return_value=(artist_rows, 1))
    mb_repo = MagicMock()
    if provider_error is None:
        mb_repo.get_artist_by_id = AsyncMock(return_value=None)
    else:
        mb_repo.get_artist_by_id = AsyncMock(side_effect=provider_error)
    memory_cache = MagicMock()
    memory_cache.get = AsyncMock(return_value=None)
    disk_cache = MagicMock()
    disk_cache.get_artist = AsyncMock(return_value=None)
    service = ArtistService(
        mb_repo,
        MagicMock(),
        MagicMock(),
        MagicMock(),
        memory_cache,
        disk_cache,
        None,
        None,
        None,
        None,
        native_library_store=store,
    )
    service._library_repo.is_configured = MagicMock(return_value=False)
    # Deterministic elif-branch in _fetch_artist_data during the outage.
    service._ownership = MagicMock()
    service._ownership.provider_artist_relationship = AsyncMock(
        return_value=(False, set())
    )
    service._get_library_cache_mbids = AsyncMock(return_value=set())
    return service


@pytest.mark.asyncio
async def test_album_basic_info_falls_back_to_local_on_mb_failure():
    service = _album_service_with_store(_ownership_rows(), _track_rows())
    basic = await service.get_album_basic_info(RG_ID)
    assert basic.title == "Outage Album"
    assert basic.in_library is True
    assert basic.service_status is None
    assert basic.musicbrainz_id == RG_ID


@pytest.mark.asyncio
async def test_album_info_falls_back_to_local_with_tracklist():
    service = _album_service_with_store(_ownership_rows(), _track_rows())
    info = await service.get_album_info(RG_ID)
    assert info.title == "Outage Album"
    assert info.total_tracks == 1
    assert info.tracks[0].title == "Local Song"
    assert info.tracks[0].length == 180000
    assert info.tracks[0].recording_id == "rec-1"
    assert info.service_status is None
    # The degraded build must never be cached under the MB-derived key.
    service._cache.set.assert_not_called()


@pytest.mark.asyncio
async def test_album_info_still_404s_when_not_locally_owned():
    service = _album_service_with_store([], [])
    with pytest.raises(Exception) as exc:
        await service.get_album_info("00000000-0000-4000-8000-000000000000")
    assert "Failed to get album info" in str(exc.value)


@pytest.mark.asyncio
async def test_artist_basic_info_falls_back_to_local_on_mb_failure():
    rows = [
        {
            "artist_mbid": "local-artist-1",
            "artist_name": "Local Artist",
            "provider_artist_mbid": ARTIST_ID,
            "album_count": 3,
        }
    ]
    service = _artist_service_with_store(
        rows, provider_error=ExternalServiceError("MusicBrainz unavailable")
    )
    info = await service.get_artist_info_basic(ARTIST_ID)
    assert info.name == "Local Artist"
    assert info.musicbrainz_id == ARTIST_ID
    assert info.in_library is True
    assert info.release_group_count == 3
    assert info.service_status is None
    service._cache.set.assert_not_called()


@pytest.mark.asyncio
async def test_artist_detail_falls_back_to_local_on_mb_failure():
    rows = [
        {
            "artist_mbid": "local-artist-1",
            "artist_name": "Local Artist",
            "provider_artist_mbid": ARTIST_ID,
            "album_count": 2,
        }
    ]
    service = _artist_service_with_store(
        rows, provider_error=ExternalServiceError("MusicBrainz unavailable")
    )
    info = await service.get_artist_info(ARTIST_ID)
    assert info.name == "Local Artist"
    assert info.musicbrainz_id == ARTIST_ID


@pytest.mark.asyncio
async def test_artist_not_found_remains_404_when_not_locally_known():
    service = _artist_service_with_store([])
    with pytest.raises(ResourceNotFoundError, match="^Artist not found$"):
        await service.get_artist_info_basic("00000000-0000-4000-8000-000000000001")


@pytest.mark.asyncio
async def test_artist_provider_outage_propagates_when_not_locally_known():
    service = _artist_service_with_store(
        [],
        provider_error=ExternalServiceError("provider detail must stay server-side"),
    )
    with pytest.raises(ExternalServiceError):
        await service.get_artist_info_basic(ARTIST_ID)


@pytest.mark.asyncio
async def test_artist_detail_provider_outage_propagates_when_not_locally_known():
    service = _artist_service_with_store(
        [],
        provider_error=ExternalServiceError("provider detail must stay server-side"),
    )
    with pytest.raises(ExternalServiceError):
        await service.get_artist_info(ARTIST_ID)


@pytest.mark.asyncio
async def test_artist_releases_fall_back_to_local_discography():
    rows = [
        {
            "artist_mbid": "local-artist-1",
            "artist_name": "Local Artist",
            "provider_artist_mbid": ARTIST_ID,
            "album_count": 2,
        }
    ]
    store = MagicMock()
    store.list_target_artists = AsyncMock(return_value=(rows, 1))
    store.list_target_albums = AsyncMock(
        return_value=(
            [
                {
                    "release_group_mbid": "local-rg-1",
                    "provider_release_group_mbid": "rg-mbid-1",
                    "album_title": "Local Album",
                    "year": 2024,
                    "original_release_date": "2024-01-12",
                    "track_count": 10,
                    "is_compilation": False,
                },
                {
                    "release_group_mbid": "local-rg-2",
                    "provider_release_group_mbid": None,
                    "album_title": "Local Single",
                    "year": 2025,
                    "original_release_date": None,
                    "track_count": 1,
                    "is_compilation": False,
                },
            ],
            2,
        )
    )
    mb_repo = MagicMock()
    mb_repo.get_artist_release_groups = AsyncMock(return_value=([], 0))
    memory_cache = MagicMock()
    memory_cache.get = AsyncMock(return_value=None)
    disk_cache = MagicMock()
    disk_cache.get_artist = AsyncMock(return_value=None)
    prefs = MagicMock()
    prefs.get_preferences.return_value = SimpleNamespace(
        primary_types=["Album", "Single", "EP"],
        secondary_types=["Studio"],
    )
    service = ArtistService(
        mb_repo,
        MagicMock(),
        MagicMock(),
        prefs,
        memory_cache,
        disk_cache,
        None,
        None,
        None,
        None,
        native_library_store=store,
    )
    service._ownership = MagicMock()
    service._ownership.provider_artist_relationship = AsyncMock(
        return_value=(False, set())
    )
    service._get_library_cache_mbids = AsyncMock(return_value=set())
    service._library_repo.is_configured = MagicMock(return_value=False)
    service._library_repo.get_requested_mbids = AsyncMock(return_value=set())

    releases = await service.get_artist_releases(ARTIST_ID)
    assert [r.title for r in releases.albums] == ["Local Album"]
    assert releases.albums[0].id == "rg-mbid-1"
    assert releases.albums[0].in_library is True
    assert releases.albums[0].year == 2024
    assert [r.title for r in releases.singles] == ["Local Single"]
    assert releases.source_total_count == 2


@pytest.mark.asyncio
async def test_artist_releases_stay_empty_for_non_library_artist():
    store = MagicMock()
    store.list_target_artists = AsyncMock(return_value=([], 0))
    mb_repo = MagicMock()
    mb_repo.get_artist_release_groups = AsyncMock(return_value=([], 0))
    memory_cache = MagicMock()
    memory_cache.get = AsyncMock(return_value=None)
    disk_cache = MagicMock()
    disk_cache.get_artist = AsyncMock(return_value=None)
    prefs = MagicMock()
    prefs.get_preferences.return_value = SimpleNamespace(
        primary_types=["Album", "Single", "EP"],
        secondary_types=["Studio"],
    )
    service = ArtistService(
        mb_repo,
        MagicMock(),
        MagicMock(),
        prefs,
        memory_cache,
        disk_cache,
        None,
        None,
        None,
        None,
        native_library_store=store,
    )
    service._ownership = MagicMock()
    service._ownership.provider_artist_relationship = AsyncMock(
        return_value=(False, set())
    )
    service._get_library_cache_mbids = AsyncMock(return_value=set())
    service._library_repo.is_configured = MagicMock(return_value=False)
    service._library_repo.get_requested_mbids = AsyncMock(return_value=set())

    releases = await service.get_artist_releases("00000000-0000-4000-8000-000000000001")
    assert releases.albums == []
    assert releases.singles == []
    assert releases.eps == []
    assert releases.source_total_count == 0
