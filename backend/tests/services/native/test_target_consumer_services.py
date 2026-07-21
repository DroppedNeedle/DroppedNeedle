import asyncio
import sqlite3
import shutil
import threading
import base64
import time
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, call

import pytest
import pytest_asyncio
import msgspec
from fastapi import FastAPI
from mutagen.flac import FLAC, Picture

from api.compat.jellyfin.builders import JellyfinBuilder
from api.compat.subsonic.models import to_album_id3, to_child
from api.v1.routes import library, library_target, local_library, requests
from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from api.v1.schemas.discover import RadioPlanRequest, RadioSeedItem
from api.v1.schemas.scrobble import ScrobbleRequest
from core.dependencies import (
    get_library_manager,
    get_local_files_service,
    get_preferences_service,
    get_request_history_store,
    get_request_service,
    get_target_native_library_service,
)
from core.exceptions import ProviderIdentityRequiredError, ResourceNotFoundError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.persistence.auth_store import AuthStore
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalAlbumAlias,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
from services.album_service import AlbumService
from services.compat.favorites_service import FavoritesService
from services.compat.id_map_service import CompatIdMapService
from services.compat.target_library_view_service import TargetLibraryViewService
from services.compat.target_cover_art_service import TargetCoverArtService
from services.home.cached_local_artwork_service import CachedLocalArtworkService
from services.local_files_service import LocalFilesService
from services.home_service import HomeService
from services.native.target_library_repository import TargetLibraryRepository
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.library_ownership_service import (
    AlbumOwnershipCandidate,
    LibraryOwnershipService,
)
from services.native.library_manager import LibraryManager
from services.native.target_native_library_service import TargetNativeLibraryService
from services.native.target_catalog_writer_service import TargetCatalogWriterService
from services.native.download_service import DownloadService
from infrastructure.audio.tagger import AudioTagger
from repositories.coverart_disk_cache import get_cache_filename
from models.audio import AudioTag
from services.native.target_reference_adapters import (
    TargetAlbumReleasePinStore,
    TargetCompatIdMapStore,
    TargetFavoritesStore,
    TargetPlayHistoryStore,
    TargetPlaylistRepository,
    TargetGenreIndex,
)
from services.discover.radio_plan_service import RadioPlanService
from services.playlist_service import PlaylistService
from services.personal_mix_service import PersonalMixService, _MixTrack
from services.request_service import RequestService
from services.spotify_import_service import SpotifyImportService
from tests.helpers import build_test_client, mock_user
from middleware import _get_current_user


IDENTIFIED_ALBUM_ID = "10000000-0000-4000-8000-000000000001"
LOCAL_ALBUM_ID = "10000000-0000-4000-8000-000000000002"
IDENTIFIED_TRACK_ID = "20000000-0000-4000-8000-000000000001"
LOCAL_TRACK_ID = "20000000-0000-4000-8000-000000000002"
IDENTIFIED_ARTIST_ID = "30000000-0000-4000-8000-000000000001"
LOCAL_ARTIST_ID = "30000000-0000-4000-8000-000000000002"
RELEASE_GROUP_MBID = "40000000-0000-4000-8000-000000000001"
RECORDING_MBID = "50000000-0000-4000-8000-000000000001"
ARTIST_MBID = "60000000-0000-4000-8000-000000000001"
COMPILATION_ALBUM_ID = "10000000-0000-4000-8000-000000000003"
COMPILATION_TRACK_ID = "20000000-0000-4000-8000-000000000003"
COMPILATION_ARTIST_ID = "30000000-0000-4000-8000-000000000003"
TRACK_ARTIST_ID = "30000000-0000-4000-8000-000000000004"
TRACK_ARTIST_MBID = "60000000-0000-4000-8000-000000000004"


def _membership(
    *, album_id: str, track_id: str, artist_id: str, root: Path, title: str
) -> CatalogMembership:
    artist = LocalArtist(
        id=artist_id,
        display_name=f"{title} Artist",
        folded_name=f"{title} artist".casefold(),
        kind="person",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=album_id,
        root_id="root-1",
        grouping_key=f"group:{album_id}",
        title=title,
        album_artist_id=artist_id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    path = root / f"{track_id}.flac"
    path.write_bytes(b"fLaC" + b"\0" * 64)
    track = LocalTrack(
        id=track_id,
        local_album_id=album_id,
        root_id="root-1",
        file_path=str(path),
        relative_path=path.name,
        path_hash=f"hash:{track_id}",
        file_size_bytes=path.stat().st_size,
        file_mtime_ns=path.stat().st_mtime_ns,
        stat_revision=f"stat:{track_id}",
        title=f"{title} Track",
        artist_name=artist.display_name,
        album_title=title,
        album_artist_name=artist.display_name,
        genre=f"{title} Genre",
        duration_seconds=180,
        file_format="flac",
        imported_at=2,
    )
    credit = LocalArtistCredit(local_artist_id=artist_id, position=0)
    return CatalogMembership(
        album=album,
        artists=[artist],
        tracks=[track],
        album_credits=[credit],
        track_credits={track_id: [credit]},
    )


@pytest_asyncio.fixture
async def target_services(tmp_path: Path):
    db_path = tmp_path / "target.db"
    lock = threading.Lock()
    auth = AuthStore(db_path, lock)
    await auth.create_user(
        id="user-1", display_name="Target User", role="admin", username="target"
    )
    root = tmp_path / "Music"
    root.mkdir()
    store = NativeLibraryStore(db_path, lock)
    await store.create_catalog_membership(
        _membership(
            album_id=IDENTIFIED_ALBUM_ID,
            track_id=IDENTIFIED_TRACK_ID,
            artist_id=IDENTIFIED_ARTIST_ID,
            root=root,
            title="Identified",
        )
    )
    await store.create_catalog_membership(
        _membership(
            album_id=LOCAL_ALBUM_ID,
            track_id=LOCAL_TRACK_ID,
            artist_id=LOCAL_ARTIST_ID,
            root=root,
            title="Local Only",
        )
    )
    local_path = root / f"{LOCAL_TRACK_ID}.flac"
    shutil.copy2(
        Path(__file__).parents[2] / "fixtures/library/flac_no_tags.flac",
        local_path,
    )
    audio = FLAC(local_path)
    picture = Picture()
    picture.type = 3
    picture.mime = "image/png"
    picture.desc = "Cover"
    picture.width = 1
    picture.height = 1
    picture.depth = 8
    picture.data = base64.b64decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
    )
    audio.add_picture(picture)
    audio.save()
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, "
            "selected_at) VALUES (?, 'musicbrainz', ?, 'manual', 3)",
            (IDENTIFIED_ALBUM_ID, RELEASE_GROUP_MBID),
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 3)",
            (IDENTIFIED_TRACK_ID, RECORDING_MBID),
        )
        connection.execute(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 3)",
            (IDENTIFIED_ARTIST_ID, ARTIST_MBID),
        )
        connection.execute(
            "INSERT INTO local_album_artwork "
            "(local_album_id, source, source_locator, updated_at) "
            "VALUES (?, 'embedded', ?, 3)",
            (LOCAL_ALBUM_ID, LOCAL_TRACK_ID),
        )
    favorites = FavoritesService(TargetFavoritesStore(store))
    history = TargetPlayHistoryStore(store)
    view = TargetLibraryViewService(store, favorites, history)
    return store, view, favorites, history, root


@pytest.mark.asyncio
async def test_target_view_browses_identified_and_local_only_without_provider_leaks(
    target_services,
) -> None:
    _store, view, _favorites, _history, _root = target_services

    albums, total = await view.get_albums(page_size=10)
    by_id = {album.rg_mbid: album for album in albums}

    assert total == 2
    assert by_id[IDENTIFIED_ALBUM_ID].musicbrainz_release_group_id == RELEASE_GROUP_MBID
    assert by_id[LOCAL_ALBUM_ID].musicbrainz_release_group_id is None
    assert to_album_id3(by_id[IDENTIFIED_ALBUM_ID]).musicBrainzId == RELEASE_GROUP_MBID
    assert to_album_id3(by_id[LOCAL_ALBUM_ID]).musicBrainzId is None

    local_tracks = await view.get_album_tracks(LOCAL_ALBUM_ID)
    assert [track.file_id for track in local_tracks] == [LOCAL_TRACK_ID]
    assert to_child(local_tracks[0]).musicBrainzId is None


@pytest.mark.asyncio
async def test_target_stats_count_local_only_albums(target_services) -> None:
    store, _view, _favorites, _history, _root = target_services
    stats = await TargetNativeLibraryService(store).stats()

    assert stats.total_albums == 2
    assert stats.local_only_count == 1


@pytest.mark.asyncio
async def test_identified_album_is_cover_available_without_stored_url(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    album = await TargetNativeLibraryService(store).album(IDENTIFIED_ALBUM_ID)

    assert album is not None
    assert album.cover_available is True


@pytest.mark.asyncio
async def test_provider_artwork_backfill_is_idempotent(target_services) -> None:
    store, _view, _favorites, _history, _root = target_services

    assert await store.get_local_artwork(IDENTIFIED_ALBUM_ID) is None
    assert await store.backfill_identified_provider_artwork(updated_at=10) == 1
    assert await store.backfill_identified_provider_artwork(updated_at=20) == 0

    artwork = await store.get_local_artwork(IDENTIFIED_ALBUM_ID)
    assert artwork is not None
    assert artwork["source"] == "provider"
    assert artwork["source_locator"] == RELEASE_GROUP_MBID


@pytest.mark.asyncio
async def test_target_media_matching_projection_uses_only_target_catalog(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    repository = TargetLibraryRepository(store)

    rows = await repository.get_all_albums_for_matching()

    assert rows == [
        ("Identified", "Identified Artist", RELEASE_GROUP_MBID, ARTIST_MBID)
    ]


@pytest.mark.asyncio
async def test_target_repository_resolves_active_provider_and_local_album_ids(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    repository = TargetLibraryRepository(store)

    assert (
        await repository.resolve_library_album_identifier(RELEASE_GROUP_MBID)
        == RELEASE_GROUP_MBID
    )
    assert (
        await repository.resolve_library_album_identifier(IDENTIFIED_ALBUM_ID)
        == RELEASE_GROUP_MBID
    )
    assert (
        await repository.resolve_library_album_identifier(LOCAL_ALBUM_ID)
        == LOCAL_ALBUM_ID
    )
    assert await repository.resolve_library_album_identifier("missing") is None


@pytest.mark.asyncio
async def test_album_service_selects_by_active_target_album_file_count(
    target_services,
) -> None:
    store, _view, _favorites, _history, root = target_services
    release_group_mbid = "70000000-0000-4000-8000-000000000001"
    release_11 = "71000000-0000-4000-8000-000000000001"
    release_20 = "72000000-0000-4000-8000-000000000001"
    active_album_id = "10000000-0000-4000-8000-000000000091"
    historical_album_id = "10000000-0000-4000-8000-000000000092"
    active = _membership(
        album_id=active_album_id,
        track_id="20000000-0000-4000-8000-000000000091",
        artist_id="30000000-0000-4000-8000-000000000091",
        root=root,
        title="Avalon Active",
    )
    active.tracks[0].track_number = 1
    template = active.tracks[0]
    for position in range(2, 21):
        track_id = f"29000000-0000-4000-8000-{position:012d}"
        path = root / f"{track_id}.flac"
        path.write_bytes(b"fLaC" + b"\0" * 64)
        track = msgspec.structs.replace(
            template,
            id=track_id,
            file_path=str(path),
            relative_path=path.name,
            path_hash=f"hash:{track_id}",
            stat_revision=f"stat:{track_id}",
            title=f"Avalon Track {position}",
            track_number=position,
        )
        active.tracks.append(track)
        active.track_credits[track_id] = list(active.track_credits[template.id])
    historical = _membership(
        album_id=historical_album_id,
        track_id="20000000-0000-4000-8000-000000000092",
        artist_id="30000000-0000-4000-8000-000000000092",
        root=root,
        title="Avalon History",
    )
    historical.tracks = []
    historical.track_credits = {}
    await store.create_catalog_membership(active)
    await store.create_catalog_membership(historical)
    with sqlite3.connect(store.db_path) as connection:
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 3)",
            [
                (active_album_id, release_group_mbid),
                (historical_album_id, release_group_mbid),
            ],
        )

    service = object.__new__(AlbumService)
    service._library_db = TargetLibraryRepository(store)
    service._release_pins = TargetAlbumReleasePinStore(store)
    payload = {
        "id": release_group_mbid,
        "releases": [
            {
                "id": release_11,
                "status": "Official",
                "country": "XW",
                "media": [{"track-count": 11}],
            },
            {
                "id": release_20,
                "status": "Official",
                "country": "US",
                "media": [{"track-count": 20}],
            },
        ],
    }

    selected, owned, pinned = await service._effective_release_id(
        release_group_mbid, payload
    )

    assert selected == release_20
    assert owned is None
    assert pinned is None


@pytest.mark.asyncio
async def test_target_repository_projects_durable_requested_state(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    requests = RequestHistoryStore(store.db_path, threading.Lock())
    await requests.async_record_request(
        "requested-release", "Requested Artist", "Requested Album"
    )
    await requests.async_update_status("requested-release", "queued")

    repository = TargetLibraryRepository(store, requests)

    assert await repository.get_requested_mbids() == {"requested-release"}


@pytest.mark.asyncio
async def test_shared_provider_identities_aggregate_active_local_copies(
    target_services,
) -> None:
    store, _view, _favorites, _history, root = target_services
    duplicate_album_id = "10000000-0000-4000-8000-000000000090"
    duplicate_track_id = "20000000-0000-4000-8000-000000000090"
    membership = _membership(
        album_id=duplicate_album_id,
        track_id=duplicate_track_id,
        artist_id=IDENTIFIED_ARTIST_ID,
        root=root,
        title="Identified Duplicate",
    )
    membership.artists = []
    membership.tracks[0].file_format = "mp3"
    membership.tracks[0].bit_rate = 128
    second_duplicate_track_id = "20000000-0000-4000-8000-000000000091"
    second_duplicate_path = root / f"{second_duplicate_track_id}.flac"
    second_duplicate_path.write_bytes(b"fLaC" + b"\0" * 64)
    second_duplicate_track = msgspec.structs.replace(
        membership.tracks[0],
        id=second_duplicate_track_id,
        file_path=str(second_duplicate_path),
        relative_path=second_duplicate_path.name,
        path_hash=f"hash:{second_duplicate_track_id}",
        stat_revision=f"stat:{second_duplicate_track_id}",
        title="Identified Duplicate Second Track",
        track_number=2,
    )
    membership.tracks.append(second_duplicate_track)
    membership.track_credits[second_duplicate_track_id] = [
        LocalArtistCredit(local_artist_id=IDENTIFIED_ARTIST_ID, position=0)
    ]
    await store.create_catalog_membership(membership)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET release_mbid = 'release-1' "
            "WHERE local_album_id = ?",
            (IDENTIFIED_ALBUM_ID,),
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'release-2', 'manual', 3)",
            (duplicate_album_id, RELEASE_GROUP_MBID),
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 3)",
            (duplicate_track_id, RECORDING_MBID),
        )
    repository = TargetLibraryRepository(store)

    assert {
        row["id"]
        for row in await repository.get_library_files_for_album(RELEASE_GROUP_MBID)
    } == {IDENTIFIED_TRACK_ID, duplicate_track_id, second_duplicate_track_id}
    assert [
        row["id"]
        for row in await repository.get_library_files_for_album(duplicate_album_id)
    ] == [duplicate_track_id, second_duplicate_track_id]
    assert {
        row["id"]
        for row in await repository.get_library_files_for_recording(RECORDING_MBID)
    } == {IDENTIFIED_TRACK_ID, duplicate_track_id}
    assert await repository.album_quality_tier(RELEASE_GROUP_MBID) == "low"
    assert await repository.recording_quality_tier(RECORDING_MBID) == "lossless"
    assert await repository.get_album_release_mbid(RELEASE_GROUP_MBID) == "release-2"
    assert await store.resolve_target_id("album", RELEASE_GROUP_MBID) is None
    assert await store.resolve_target_id("track", RECORDING_MBID) is None
    with pytest.raises(ResourceNotFoundError, match="not found"):
        await store.add_target_favorite("user-1", "album", RELEASE_GROUP_MBID, 3)
    assert (
        await store.target_favorite_map(
            "user-1", "album", [IDENTIFIED_ALBUM_ID, duplicate_album_id]
        )
        == {}
    )
    assert await repository.get_library_mbids(include_release_ids=False) == {
        RELEASE_GROUP_MBID
    }
    assert await repository.get_library_mbids(include_release_ids=True) == {
        RELEASE_GROUP_MBID,
        "release-1",
        "release-2",
    }

    await store.mark_target_tracks_missing(
        [IDENTIFIED_TRACK_ID],
        actor_user_id="user-1",
        reason_code="TEST_DUPLICATE_COPY",
        missing_at=4,
    )

    assert await repository.has_recording(IDENTIFIED_TRACK_ID) is False
    assert await repository.has_recording(RECORDING_MBID) is True
    assert [
        row["id"]
        for row in await repository.get_library_files_for_recording(RECORDING_MBID)
    ] == [duplicate_track_id]
    assert await repository.album_quality_tier(RELEASE_GROUP_MBID) == "low"
    assert await repository.recording_quality_tier(RECORDING_MBID) == "low"
    assert await repository.get_library_mbids(include_release_ids=True) == {
        RELEASE_GROUP_MBID,
        "release-2",
    }


@pytest.mark.asyncio
async def test_target_cover_prefers_managed_local_bytes_for_identified_album(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, _root = target_services
    cache_dir = tmp_path / "covers"
    cache_dir.mkdir()
    content = b"\xff\xd8\xffmanaged-local-cover"
    cache_path = cache_dir / (
        f"{get_cache_filename(f'rg_{RELEASE_GROUP_MBID}', '500')}.bin"
    )
    cache_path.write_bytes(content)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO local_album_artwork "
            "(local_album_id, source, source_locator, version, updated_at) "
            "VALUES (?, 'provider', ?, 1, 3)",
            (IDENTIFIED_ALBUM_ID, RELEASE_GROUP_MBID),
        )
    provider = AsyncMock()
    service = TargetCoverArtService(
        store,
        provider,
        CachedLocalArtworkService(store, cache_dir),
    )

    cover = await service.get_release_group_cover(IDENTIFIED_ALBUM_ID)
    etag = await service.get_release_group_cover_etag(IDENTIFIED_ALBUM_ID)

    assert cover == (content, "image/jpeg", "provider")
    assert etag is not None
    provider.get_release_group_cover.assert_not_awaited()
    provider.get_release_group_cover_etag.assert_not_awaited()


@pytest.mark.asyncio
async def test_target_cover_without_managed_art_uses_only_injected_provider(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, _root = target_services
    provider = AsyncMock()
    provider.get_release_group_cover.return_value = (
        b"\x89PNG\r\n\x1a\nprovider",
        "image/png",
        "coverartarchive",
    )
    service = TargetCoverArtService(
        store,
        provider,
        CachedLocalArtworkService(store, tmp_path / "covers"),
    )

    cover = await service.get_release_group_cover(IDENTIFIED_ALBUM_ID)

    assert cover == (
        b"\x89PNG\r\n\x1a\nprovider",
        "image/png",
        "coverartarchive",
    )
    provider.get_release_group_cover.assert_awaited_once_with(RELEASE_GROUP_MBID, "500")


@pytest.mark.asyncio
async def test_target_cover_distinguishes_external_and_known_local_only_ids(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, _root = target_services
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "DELETE FROM local_album_artwork WHERE local_album_id = ?",
            (LOCAL_ALBUM_ID,),
        )
    provider = AsyncMock()
    provider.get_release_group_cover.return_value = (
        b"external-album",
        "image/jpeg",
        "coverartarchive",
    )
    provider.get_artist_image.return_value = (
        b"external-artist",
        "image/jpeg",
        "provider",
    )
    service = TargetCoverArtService(
        store,
        provider,
        CachedLocalArtworkService(store, tmp_path / "covers"),
    )

    assert await service.get_release_group_cover("external-rg") == (
        b"external-album",
        "image/jpeg",
        "coverartarchive",
    )
    assert await service.get_artist_image("external-artist") == (
        b"external-artist",
        "image/jpeg",
        "provider",
    )
    assert await service.get_release_group_cover(LOCAL_ALBUM_ID) is None
    assert await service.get_artist_image(LOCAL_ARTIST_ID) is None
    assert await service.get_artist_image(IDENTIFIED_ARTIST_ID) == (
        b"external-artist",
        "image/jpeg",
        "provider",
    )

    assert provider.get_release_group_cover.await_args_list == [
        call("external-rg", "500")
    ]
    assert provider.get_artist_image.await_args_list == [
        call("external-artist", None),
        call(ARTIST_MBID, None),
    ]


@pytest.mark.asyncio
async def test_target_cover_projects_provider_warming_to_local_ids(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, _root = target_services
    provider = AsyncMock()
    provider.get_release_group_cover.return_value = None
    provider.get_artist_image.return_value = None
    provider.is_rg_cover_warming = (
        lambda identifier, size: identifier == RELEASE_GROUP_MBID and size == "500"
    )
    provider.is_artist_cover_warming = (
        lambda identifier, size: identifier == ARTIST_MBID and size == 500
    )
    service = TargetCoverArtService(
        store,
        provider,
        CachedLocalArtworkService(store, tmp_path / "covers"),
    )

    assert await service.get_release_group_cover(IDENTIFIED_ALBUM_ID) is None
    assert await service.get_artist_image(IDENTIFIED_ARTIST_ID, 500) is None
    assert service.is_rg_cover_warming(IDENTIFIED_ALBUM_ID, "500") is True
    assert service.is_artist_cover_warming(IDENTIFIED_ARTIST_ID, 500) is True


@pytest.mark.asyncio
async def test_target_cover_forwards_provider_only_cover_operations(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, _root = target_services
    provider = AsyncMock()
    provider.get_release_cover.return_value = (
        b"release-cover",
        "image/jpeg",
        "coverartarchive",
    )
    provider.get_release_cover_etag.return_value = "release-etag"
    provider.debug_artist_image.return_value = {"artist_found": True}
    service = TargetCoverArtService(
        store,
        provider,
        CachedLocalArtworkService(store, tmp_path / "covers"),
    )

    cover = await service.get_release_cover("release-id", "250", defer=True)
    etag = await service.get_release_cover_etag("release-id", "250")
    await service.batch_prefetch_covers(
        [IDENTIFIED_ALBUM_ID, RELEASE_GROUP_MBID], "250", 2
    )
    debug = await service.debug_artist_image(
        IDENTIFIED_ARTIST_ID, {"artist_found": False}
    )

    assert cover == (b"release-cover", "image/jpeg", "coverartarchive")
    assert etag == "release-etag"
    assert debug == {"artist_found": True}
    provider.get_release_cover.assert_awaited_once_with("release-id", "250", defer=True)
    provider.get_release_cover_etag.assert_awaited_once_with("release-id", "250")
    provider.batch_prefetch_covers.assert_awaited_once_with(
        [RELEASE_GROUP_MBID], "250", 2
    )
    provider.debug_artist_image.assert_awaited_once_with(
        ARTIST_MBID, {"artist_found": False}
    )


@pytest.mark.asyncio
async def test_target_genre_and_radio_pool_use_target_membership_only(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET genre = 'Ambient', genre_folded = 'ambient' "
            "WHERE id IN (?, ?)",
            (IDENTIFIED_TRACK_ID, LOCAL_TRACK_ID),
        )
        connection.execute(
            "INSERT INTO artist_genres VALUES (?, ?, ?)",
            (ARTIST_MBID.lower(), ARTIST_MBID, '["Latin"]'),
        )
        connection.execute(
            "INSERT INTO artist_genre_lookup VALUES (?, 'latin')",
            (ARTIST_MBID.lower(),),
        )
        connection.execute(
            "INSERT INTO artist_genres VALUES ('legacy-only', 'legacy-only', '[\"Ambient\"]')"
        )
        connection.execute(
            "INSERT INTO artist_genre_lookup VALUES ('legacy-only', 'ambient')"
        )

    genre_index = TargetGenreIndex(store)
    repository = TargetLibraryRepository(store)
    radio = RadioPlanService(
        AsyncMock(),
        AsyncMock(),
        SimpleNamespace(
            normalize_mbid=lambda value: value.casefold() if value else None
        ),
        library_db=repository,
        genre_index=genre_index,
    )

    assert await genre_index.get_artists_for_genres(["Ambient"]) == {
        "ambient": [ARTIST_MBID.lower()]
    }
    artists = await genre_index.get_artists_by_genre("Ambient")
    albums = await genre_index.get_albums_by_genre("Ambient")
    assert {(row["mbid"], row["local_id"]) for row in artists} == {
        (ARTIST_MBID, IDENTIFIED_ARTIST_ID),
        (None, LOCAL_ARTIST_ID),
    }
    assert {(row["mbid"], row["local_id"]) for row in albums} == {
        (RELEASE_GROUP_MBID, IDENTIFIED_ALBUM_ID),
        (None, LOCAL_ALBUM_ID),
    }
    assert await genre_index.get_top_genres() == [("ambient", 2)]
    assert await genre_index.get_genres_for_artists([ARTIST_MBID]) == {
        ARTIST_MBID.lower(): ["Latin"]
    }
    tracks = await radio._library_tracks([(ARTIST_MBID, "Identified Artist")], set())
    assert [track.local_file_id for track in tracks] == [IDENTIFIED_TRACK_ID]
    shelf = await radio.build_plan(
        "user-1",
        RadioPlanRequest(
            seed_type="items",
            mode="library",
            items=[
                RadioSeedItem(
                    artist_mbid=ARTIST_MBID,
                    artist_name="Identified Artist",
                    album_mbid=RELEASE_GROUP_MBID,
                    album_name="Identified",
                )
            ],
        ),
    )
    assert [track.local_file_id for track in shelf.tracks] == [IDENTIFIED_TRACK_ID]


@pytest.mark.asyncio
async def test_compilation_track_artist_is_browsable_across_target_projection(
    target_services,
) -> None:
    store, view, _favorites, _history, root = target_services
    album_artist = LocalArtist(
        id=COMPILATION_ARTIST_ID,
        display_name="Various Artists",
        folded_name="various artists",
        kind="various_artists",
        created_at=4,
        updated_at=4,
    )
    track_artist = LocalArtist(
        id=TRACK_ARTIST_ID,
        display_name="Compilation Guest",
        folded_name="compilation guest",
        kind="person",
        created_at=4,
        updated_at=4,
    )
    path = root / f"{COMPILATION_TRACK_ID}.flac"
    path.write_bytes(b"fLaC" + b"\0" * 64)
    album = LocalAlbum(
        id=COMPILATION_ALBUM_ID,
        root_id="root-1",
        grouping_key="compilation:guest",
        title="Compilation",
        album_artist_id=COMPILATION_ARTIST_ID,
        album_artist_name="Various Artists",
        is_compilation=True,
        created_at=4,
        updated_at=4,
    )
    track = LocalTrack(
        id=COMPILATION_TRACK_ID,
        local_album_id=COMPILATION_ALBUM_ID,
        root_id="root-1",
        file_path=str(path),
        relative_path=path.name,
        path_hash="hash:compilation-guest",
        file_size_bytes=path.stat().st_size,
        file_mtime_ns=path.stat().st_mtime_ns,
        stat_revision="stat:compilation-guest",
        title="Guest Track",
        artist_name="Compilation Guest",
        album_title="Compilation",
        album_artist_name="Various Artists",
        file_format="flac",
        imported_at=4,
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[album_artist, track_artist],
            tracks=[track],
            album_credits=[
                LocalArtistCredit(local_artist_id=COMPILATION_ARTIST_ID, position=0)
            ],
            track_credits={
                COMPILATION_TRACK_ID: [
                    LocalArtistCredit(local_artist_id=TRACK_ARTIST_ID, position=0)
                ]
            },
        )
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 4)",
            (TRACK_ARTIST_ID, TRACK_ARTIST_MBID),
        )

    artists, _ = await view.get_artists(limit=50)
    detail = await view.get_artist_with_albums(TRACK_ARTIST_ID)
    repository = TargetLibraryRepository(store)
    provider_artist_ids = await repository.get_artist_mbids()
    stats = await TargetNativeLibraryService(store).stats()

    assert TRACK_ARTIST_ID in {artist.artist_mbid for artist in artists}
    assert detail is not None
    artist, albums = detail
    assert artist.artist_mbid == TRACK_ARTIST_ID
    assert [item.rg_mbid for item in albums] == [COMPILATION_ALBUM_ID]
    assert provider_artist_ids == {ARTIST_MBID, TRACK_ARTIST_MBID}
    assert await repository.get_all_artist_mbids() == provider_artist_ids
    assert stats.total_artists == 4

    await store.mark_target_tracks_missing(
        [COMPILATION_TRACK_ID],
        actor_user_id=None,
        reason_code="test_missing",
        missing_at=5,
    )
    assert await repository.get_artist_mbids() == {ARTIST_MBID}


@pytest.mark.asyncio
async def test_target_jellyfin_provider_ids_are_optional_and_local_ids_stay_stable(
    target_services,
) -> None:
    store, view, _favorites, _history, _root = target_services
    ids = CompatIdMapService(TargetCompatIdMapStore(store))
    covers = SimpleNamespace(
        get_release_group_cover_etag=AsyncMock(return_value=None),
        get_artist_image_etag=AsyncMock(return_value=None),
    )
    builder = JellyfinBuilder(ids, covers, "server")
    identified = await view.get_album(IDENTIFIED_ALBUM_ID)
    local_only = await view.get_album(LOCAL_ALBUM_ID)

    identified_item = await builder.album(identified)
    local_item = await builder.album(local_only)

    assert identified_item.ProviderIds == {
        "MusicBrainzReleaseGroup": RELEASE_GROUP_MBID
    }
    assert local_item.ProviderIds is None
    assert await ids.from_jf(local_item.Id) == ("album", LOCAL_ALBUM_ID)


@pytest.mark.asyncio
async def test_target_references_use_local_ids_and_share_one_projection(
    target_services,
) -> None:
    store, view, favorites, history, _root = target_services
    await favorites.add("user-1", "album", LOCAL_ALBUM_ID)
    await history.insert(
        "user-1",
        track_name="Local Only Track",
        artist_name="Local Only Artist",
        album_name="Local Only",
        release_group_mbid=LOCAL_ALBUM_ID,
        played_at="2026-07-14T12:00:00+00:00",
    )
    user = SimpleNamespace(id="user-1")

    album = await view.get_album(LOCAL_ALBUM_ID, user=user)
    track = await view.get_track(LOCAL_TRACK_ID, user=user)

    assert album.starred_at is not None
    assert album.play_count == 1
    assert track.play_count == 1
    with sqlite3.connect(store.db_path) as connection:
        favorite = connection.execute(
            "SELECT item_id FROM library_user_favorites"
        ).fetchone()
        played = connection.execute(
            "SELECT local_track_id, local_album_id, local_artist_id "
            "FROM library_play_history"
        ).fetchone()
    assert favorite == (LOCAL_ALBUM_ID,)
    assert played == (LOCAL_TRACK_ID, LOCAL_ALBUM_ID, LOCAL_ARTIST_ID)


@pytest.mark.asyncio
async def test_target_local_file_stream_supports_ranges(target_services) -> None:
    store, _view, _favorites, _history, root = target_services
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    service = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )

    chunks, headers, status = await service.stream_track(LOCAL_TRACK_ID, "bytes=0-3")
    payload = b"".join([chunk async for chunk in chunks])

    assert status == 206
    assert headers["Content-Range"].startswith("bytes 0-3/")
    assert payload == b"fLaC"


@pytest.mark.asyncio
async def test_target_local_routes_cover_full_catalog_read_surface(
    target_services,
) -> None:
    store, _view, _favorites, _history, root = target_services
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        ),
        get_advanced_settings=lambda: SimpleNamespace(
            cache_ttl_local_files_recently_added=120
        ),
    )
    service = LocalFilesService(
        TargetLibraryRepository(store), preferences, InMemoryCache()
    )
    app = FastAPI()
    app.include_router(local_library.router)
    app.dependency_overrides[get_local_files_service] = lambda: service
    client = build_test_client(app)

    responses = {
        "albums": client.get("/local/albums?limit=10"),
        "match": client.get(f"/local/albums/match/{RELEASE_GROUP_MBID}"),
        "tracks": client.get(f"/local/albums/{LOCAL_ALBUM_ID}/tracks"),
        "search": client.get("/local/search?q=Local"),
        "recent": client.get("/local/recent?limit=10"),
        "stats": client.get("/local/stats"),
        "suggestions": client.get("/local/suggestions?limit=10"),
        "decades": client.get("/local/decades"),
    }

    assert {name: response.status_code for name, response in responses.items()} == {
        name: 200 for name in responses
    }
    assert {item["musicbrainz_id"] for item in responses["albums"].json()["items"]} == {
        IDENTIFIED_ALBUM_ID,
        LOCAL_ALBUM_ID,
    }
    assert responses["match"].json()["tracks"][0]["track_file_id"] == (
        IDENTIFIED_TRACK_ID
    )
    assert responses["tracks"].json()[0]["track_file_id"] == LOCAL_TRACK_ID
    assert responses["search"].json()["tracks"][0]["track_file_id"] == (LOCAL_TRACK_ID)
    assert len(responses["recent"].json()) == 2
    assert responses["stats"].json()["total_tracks"] == 2
    assert len(responses["suggestions"].json()["items"]) == 2


@pytest.mark.asyncio
async def test_target_home_projects_identified_and_local_only_catalog_rows(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    repository = TargetLibraryRepository(store)
    disabled = SimpleNamespace(
        enabled=False,
        username="",
        jellyfin_url="",
        api_key="",
        navidrome_url="",
        password="",
        plex_url="",
        plex_token="",
        music_library_ids=[],
        api_enabled=False,
    )
    preferences = SimpleNamespace(
        get_listenbrainz_connection=lambda: disabled,
        get_jellyfin_connection=lambda: disabled,
        get_youtube_connection=lambda: disabled,
        get_navidrome_connection=lambda: disabled,
        get_plex_connection=lambda: disabled,
        is_download_source_ready=lambda: False,
        is_lastfm_enabled=lambda: False,
    )
    listenbrainz = AsyncMock()
    listenbrainz.get_sitewide_top_artists.return_value = []
    listenbrainz.get_sitewide_top_release_groups.return_value = []
    service = HomeService(
        listenbrainz_repo=listenbrainz,
        jellyfin_repo=AsyncMock(),
        library_repo=repository,
        musicbrainz_repo=AsyncMock(),
        preferences_service=preferences,
        ownership_service=LibraryOwnershipService(store),
    )

    response = await service.get_home_data("user-1")

    albums = {item.local_id: item for item in response.library_albums.items}
    recent = {item.local_id: item for item in response.recently_added.items}
    artists = {item.local_id: item for item in response.library_artists.items}
    assert set(albums) == {IDENTIFIED_ALBUM_ID, LOCAL_ALBUM_ID}
    assert set(recent) == {IDENTIFIED_ALBUM_ID, LOCAL_ALBUM_ID}
    assert set(artists) == {IDENTIFIED_ARTIST_ID, LOCAL_ARTIST_ID}
    assert albums[IDENTIFIED_ALBUM_ID].mbid == RELEASE_GROUP_MBID
    assert albums[LOCAL_ALBUM_ID].mbid is None
    assert artists[IDENTIFIED_ARTIST_ID].mbid == ARTIST_MBID
    assert artists[LOCAL_ARTIST_ID].mbid is None


@pytest.mark.asyncio
async def test_target_personal_mix_resolves_provider_recording_to_local_file(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    service = object.__new__(PersonalMixService)
    service._library_repo = TargetLibraryRepository(store)

    matched = await service._match_library_files(
        [
            _MixTrack(
                track_name="Identified Track",
                artist_name="Identified Artist",
                album_name="Identified",
                release_group_mbid=RELEASE_GROUP_MBID,
                artist_mbid=ARTIST_MBID,
                recording_mbid=RECORDING_MBID,
                in_library=True,
            )
        ]
    )

    assert matched[0].library_file_id == IDENTIFIED_TRACK_ID
    assert matched[0].track_number == 0


@pytest.mark.asyncio
async def test_target_album_archive_uses_target_rows_for_local_only_album(
    target_services,
) -> None:
    store, _view, _favorites, _history, root = target_services
    service = LocalFilesService(
        TargetLibraryRepository(store),
        SimpleNamespace(
            get_typed_library_settings=lambda: SimpleNamespace(
                library_roots=[SimpleNamespace(path=str(root))]
            )
        ),
        InMemoryCache(),
    )

    archive, name = await service.create_album_zip_by_mbid(LOCAL_ALBUM_ID)
    try:
        with zipfile.ZipFile(archive) as contents:
            assert contents.namelist() == ["00 Local Only Track.flac"]
        assert name == "Local Only Artist - Local Only.zip"
    finally:
        archive.unlink(missing_ok=True)


@pytest.mark.asyncio
async def test_target_playlist_writes_and_legacy_track_ids_resolve_to_local_references(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, _root = target_services
    repository = TargetPlaylistRepository(store)
    service = PlaylistService(
        None,
        tmp_path,
        library_db=TargetLibraryRepository(store),
        async_repo=repository,
    )
    owner = SimpleNamespace(id="user-1", role="user")
    playlist = await service.create_playlist(
        "Local collection", source_ref="subsonic:old-7", user_id=owner.id
    )

    first = await service.add_tracks(
        playlist.id,
        owner,
        [
            {
                "track_name": "Identified Track",
                "artist_name": "Identified Artist",
                "album_name": "Identified",
                "track_source_id": RECORDING_MBID,
                "source_type": "droppedneedle-local",
                "available_sources": ["droppedneedle-local"],
                "duration": 180,
            }
        ],
    )
    second = await service.add_file_id_entry(
        playlist.id, LOCAL_TRACK_ID, requesting=owner
    )

    assert first[0].library_file_id == IDENTIFIED_TRACK_ID
    assert first[0].album_id == IDENTIFIED_ALBUM_ID
    assert first[0].artist_id == IDENTIFIED_ARTIST_ID
    assert second.library_file_id == LOCAL_TRACK_ID
    assert await service.get_imported_source_ids("subsonic:", owner.id) == {"old-7"}
    assert await service.get_streamable_counts() == {playlist.id: (2, 360)}

    await service.reorder_track(playlist.id, owner, second.id, 0)
    tracks = await service.get_tracks(playlist.id)
    assert [track.id for track in tracks] == [second.id, first[0].id]

    with sqlite3.connect(store.db_path) as connection:
        rows = connection.execute(
            "SELECT local_track_id, local_album_id, local_artist_id "
            "FROM library_playlist_tracks ORDER BY position"
        ).fetchall()
        assert connection.execute(
            "SELECT COUNT(*) FROM playlist_tracks"
        ).fetchone() == (0,)
    assert rows == [
        (LOCAL_TRACK_ID, LOCAL_ALBUM_ID, LOCAL_ARTIST_ID),
        (IDENTIFIED_TRACK_ID, IDENTIFIED_ALBUM_ID, IDENTIFIED_ARTIST_ID),
    ]


@pytest.mark.asyncio
async def test_spotify_and_personal_mix_write_only_target_playlists(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, _root = target_services
    repository = TargetPlaylistRepository(store)
    playlists = PlaylistService(
        None,
        tmp_path,
        library_db=TargetLibraryRepository(store),
        async_repo=repository,
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO playlists "
            "(id, name, created_at, updated_at, user_id) "
            "VALUES ('legacy-sentinel', 'Legacy', '1', '1', 'user-1')"
        )

    spotify_client = AsyncMock()
    spotify_client.get_playlist.return_value = {"id": "spotify-1", "name": "Spotify"}
    spotify_client.get_playlist_tracks.return_value = [
        {
            "name": "Imported Track",
            "artists": [{"name": "Imported Artist"}],
            "album": {"id": "", "name": "Imported Album", "images": []},
            "duration_ms": 180_000,
        }
    ]
    factory = AsyncMock()
    factory.resolve_spotify.return_value = spotify_client
    spotify = SpotifyImportService(
        client_factory=factory,
        playlist_repo=None,
        mb_repo=AsyncMock(),
        playlist_service=playlists,
        async_playlist_repo=repository,
    )
    spotify_playlist_id = await spotify.ensure_playlist_record(
        "user-1", "spotify-1", "Imported Spotify"
    )
    await spotify.populate_playlist("user-1", "spotify-1", spotify_playlist_id)

    personal_mix = object.__new__(PersonalMixService)
    personal_mix._playlists = playlists
    personal_mix_id = await personal_mix._upsert_playlist(
        None,
        "personal-mix:user-1",
        SimpleNamespace(id="user-1", role="user"),
        [
            _MixTrack(
                track_name="Mix Track",
                artist_name="Mix Artist",
                album_name="Local Only",
                release_group_mbid=LOCAL_ALBUM_ID,
                artist_mbid=None,
                recording_mbid=None,
                in_library=True,
                library_file_id=LOCAL_TRACK_ID,
            )
        ],
    )

    spotify_tracks = await repository.get_tracks(spotify_playlist_id)
    personal_tracks = await repository.get_tracks(personal_mix_id)
    assert [track.track_name for track in spotify_tracks] == ["Imported Track"]
    assert [track.library_file_id for track in personal_tracks] == [LOCAL_TRACK_ID]
    with sqlite3.connect(store.db_path) as connection:
        assert connection.execute("SELECT id, name FROM playlists").fetchall() == [
            ("legacy-sentinel", "Legacy")
        ]
        assert connection.execute(
            "SELECT source_ref FROM library_playlists ORDER BY source_ref"
        ).fetchall() == [
            ("personal-mix:user-1",),
            ("spotify:spotify-1",),
        ]


@pytest.mark.asyncio
async def test_target_tag_and_removal_writers_audit_without_deleting_stable_rows(
    target_services, tmp_path: Path
) -> None:
    store, _view, _favorites, _history, root = target_services
    track_id = "20000000-0000-4000-8000-000000000099"
    album_id = "10000000-0000-4000-8000-000000000099"
    artist_id = "30000000-0000-4000-8000-000000000099"
    path = root / "real-local.flac"
    shutil.copy2(Path(__file__).parents[2] / "fixtures/library/flac_no_tags.flac", path)
    stat = path.stat()
    _tag, info = AudioTagger().read_tags(path)
    membership = _membership(
        album_id=album_id,
        track_id=track_id,
        artist_id=artist_id,
        root=root,
        title="Editable",
    )
    membership.tracks[0].file_path = str(path)
    membership.tracks[0].relative_path = path.name
    membership.tracks[0].file_size_bytes = stat.st_size
    membership.tracks[0].file_mtime_ns = stat.st_mtime_ns
    membership.tracks[0].stat_revision = f"{stat.st_size}:{stat.st_mtime_ns}"
    membership.tracks[0].file_format = info.file_format
    membership.tracks[0].duration_seconds = info.duration_seconds
    await store.create_catalog_membership(membership)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET stat_revision_kind = 'legacy_float' WHERE id = ?",
            (track_id,),
        )
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    local_files = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )
    library_service = TargetNativeLibraryService(store)
    writer = TargetCatalogWriterService(store, local_files, library_service)
    before_revision = await store.get_catalog_revision()

    updated = await writer.update_tags(
        track_id,
        AudioTag(
            title="Edited locally",
            artist="Local artist",
            album="Local album",
            album_artist="Local artist",
            track_number=4,
            genre="Ambient",
        ),
        actor_user_id="user-1",
    )

    assert updated.id == track_id
    assert updated.title == "Edited locally"
    assert updated.musicbrainz_recording_id is None
    assert AudioTagger().read_tags(path)[0].title == "Edited locally"
    assert await store.get_catalog_revision() > before_revision
    with sqlite3.connect(store.db_path) as connection:
        stat_revision_kind = connection.execute(
            "SELECT stat_revision_kind FROM local_tracks WHERE id = ?", (track_id,)
        ).fetchone()[0]
    assert stat_revision_kind == "exact"

    removed = await writer.remove_track(
        track_id, actor_user_id="user-1", delete_file=True
    )

    assert removed == [track_id]
    assert not path.exists()
    retained = await store.get_target_track(track_id)
    assert retained is not None
    assert retained["availability"] == "missing"
    with sqlite3.connect(store.db_path) as connection:
        actions = connection.execute(
            "SELECT action_kind, reason_code FROM library_catalog_actions "
            "WHERE local_track_id = ? ORDER BY created_at",
            (track_id,),
        ).fetchall()
    assert actions == [
        ("update_track_tags", "EXPLICIT_TAG_EDIT"),
        ("remove_track", "FILE_DELETED"),
    ]


@pytest.mark.asyncio
async def test_target_ownership_projection_is_conservative_and_provider_independent(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    ownership = LibraryOwnershipService(store)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET year = 2020 WHERE id = ?", (LOCAL_ALBUM_ID,)
        )

    projections = await ownership.project_albums(
        [
            AlbumOwnershipCandidate(RELEASE_GROUP_MBID, "Wrong", "Wrong", 1900),
            AlbumOwnershipCandidate(None, "Local Only", "Local Only Artist", None),
            AlbumOwnershipCandidate(None, "Local Only", "Local Only Artist", 1900),
            AlbumOwnershipCandidate(None, "Unknown album", "Unknown artist", None),
            AlbumOwnershipCandidate(None, "Local", "Local Only Artist", None),
            AlbumOwnershipCandidate(
                "different-provider-id", "Identified", "Identified Artist", None
            ),
            AlbumOwnershipCandidate(
                "unmatched-provider-id", "Local Only", "Local Only Artist", None
            ),
        ]
    )

    assert [(item.owned, item.local_album_id) for item in projections] == [
        (True, IDENTIFIED_ALBUM_ID),
        (True, LOCAL_ALBUM_ID),
        (False, None),
        (False, None),
        (False, None),
        (False, None),
        (True, LOCAL_ALBUM_ID),
    ]
    assert await ownership.provider_album_id(IDENTIFIED_ALBUM_ID) == RELEASE_GROUP_MBID
    assert await ownership.provider_track_id(IDENTIFIED_TRACK_ID) == RECORDING_MBID
    with pytest.raises(ProviderIdentityRequiredError):
        await ownership.provider_album_id(LOCAL_ALBUM_ID)
    with pytest.raises(ProviderIdentityRequiredError):
        await ownership.provider_track_id(LOCAL_TRACK_ID)


@pytest.mark.asyncio
async def test_provider_album_snapshot_coalesces_and_rebuilds_by_catalog_revision(
    target_services, monkeypatch: pytest.MonkeyPatch
) -> None:
    store, *_ = target_services
    original_read = store._read
    snapshot_builds = 0

    async def counted_read(operation):
        nonlocal snapshot_builds
        if "target_provider_album_snapshot" in operation.__qualname__:
            snapshot_builds += 1
        return await original_read(operation)

    monkeypatch.setattr(store, "_read", counted_read)

    results = await asyncio.gather(
        *(store.target_provider_album_snapshot() for _ in range(12))
    )

    assert snapshot_builds == 1
    assert all(RELEASE_GROUP_MBID in values for _revision, values in results)

    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1"
        )
    await store.target_provider_album_snapshot()
    assert snapshot_builds == 2


@pytest.mark.asyncio
async def test_local_ownership_backfill_normalizes_internal_whitespace(
    target_services,
) -> None:
    store, *_ = target_services
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET title = 'Local   Only', "
            "title_folded = 'local   only', "
            "album_artist_name = 'Local\tOnly Artist', "
            "album_artist_name_folded = 'local\tonly artist' WHERE id = ?",
            (LOCAL_ALBUM_ID,),
        )

    reopened = NativeLibraryStore(store.db_path, store._write_lock)
    projection = await LibraryOwnershipService(reopened).project_album(
        release_group_mbid=None,
        title="Local Only",
        album_artist="Local Only Artist",
    )

    assert projection.owned is True
    assert projection.local_album_id == LOCAL_ALBUM_ID


@pytest.mark.asyncio
async def test_target_discover_top_picks_use_shared_local_ownership_without_calls(
    target_services,
) -> None:
    from api.v1.schemas.discover import DiscoverResponse, TopPickItem, TopPicksSection
    from api.v1.schemas.home import HomeAlbum, HomeSection
    from services.discover.facade import DiscoverService

    store, _view, _favorites, _history, _root = target_services
    service = object.__new__(DiscoverService)
    service._ownership = LibraryOwnershipService(store)
    local_top_pick = HomeAlbum(name="Local Only", artist_name="Local Only Artist")
    local_home_album = HomeAlbum(name="Local Only", artist_name="Local Only Artist")
    missing = HomeAlbum(name="Missing", artist_name="Nobody")
    response = DiscoverResponse(
        top_picks=TopPicksSection(
            items=[TopPickItem(album=local_top_pick, match_pct=90)]
        ),
        fresh_releases=HomeSection(
            title="Fresh", type="albums", items=[local_home_album, missing]
        ),
    )

    await service._apply_album_ownership(response)

    assert local_top_pick.in_library is True
    assert local_home_album.in_library is True
    assert missing.in_library is False


@pytest.mark.asyncio
async def test_local_only_request_boundary_returns_typed_4xx_without_upstream_call(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    history = AsyncMock()
    acquisition = AsyncMock()
    service = RequestService(
        history,
        lambda: AsyncMock(),
        acquisition,
        ownership_service=LibraryOwnershipService(store),
    )
    app = FastAPI()
    app.include_router(requests.router)
    app.dependency_overrides[get_request_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="trusted", user_id="user-1"
    )

    response = build_test_client(app).post(
        "/requests/new",
        json={
            "musicbrainz_id": LOCAL_ALBUM_ID,
            "artist": "Local Only Artist",
            "album": "Local Only",
        },
    )

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "PROVIDER_IDENTITY_REQUIRED"
    history.async_get_record.assert_not_awaited()
    acquisition.request_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_local_only_album_track_and_artist_never_cross_provider_boundaries(
    target_services,
) -> None:
    from services.album_service import AlbumService

    store, _view, _favorites, _history, _root = target_services
    ownership = LibraryOwnershipService(store)
    download = object.__new__(DownloadService)
    download._ownership = ownership
    album_metadata = object.__new__(AlbumService)
    album_metadata._ownership = ownership

    with pytest.raises(ProviderIdentityRequiredError):
        await download.request_album(
            "user-1", LOCAL_ALBUM_ID, "Local Only Artist", "Local Only"
        )
    with pytest.raises(ProviderIdentityRequiredError):
        await download.request_track(
            "user-1", LOCAL_TRACK_ID, "Local Only Artist", "Local Only Track"
        )
    with pytest.raises(ProviderIdentityRequiredError):
        await download.search_album(
            "user-1",
            "Local Only Artist",
            "Local Only",
            release_group_mbid=LOCAL_ALBUM_ID,
        )
    with pytest.raises(ProviderIdentityRequiredError):
        await album_metadata.get_album_info(LOCAL_ALBUM_ID)
    with pytest.raises(ProviderIdentityRequiredError):
        await ownership.provider_artist_id(LOCAL_ARTIST_ID)

    assert await ownership.provider_album_id(IDENTIFIED_ALBUM_ID) == RELEASE_GROUP_MBID
    assert await ownership.provider_track_id(IDENTIFIED_TRACK_ID) == RECORDING_MBID
    assert await ownership.provider_artist_id(IDENTIFIED_ARTIST_ID) == ARTIST_MBID


@pytest.mark.asyncio
async def test_native_library_routes_browse_target_catalog_with_local_ids(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    manager = LibraryManager(TargetLibraryRepository(store))
    app = FastAPI()
    app.include_router(library.router)
    app.dependency_overrides[get_library_manager] = lambda: manager
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="user", user_id="user-1"
    )
    client = build_test_client(app)

    albums = client.get("/library/albums?page_size=10")
    artists = client.get("/library/artists?limit=10")
    tracks = client.get("/library/tracks?limit=10")
    stats = client.get("/library/stats")
    album_tracks = client.get(f"/library/albums/{LOCAL_ALBUM_ID}/tracks")

    assert albums.status_code == 200
    assert {item["release_group_mbid"] for item in albums.json()["items"]} == {
        IDENTIFIED_ALBUM_ID,
        LOCAL_ALBUM_ID,
    }
    assert artists.status_code == 200
    assert LOCAL_ARTIST_ID in {item["artist_mbid"] for item in artists.json()["items"]}
    assert tracks.status_code == 200
    assert LOCAL_TRACK_ID in {item["track_file_id"] for item in tracks.json()["items"]}
    assert stats.json()["total_tracks"] == 2
    assert [item["id"] for item in album_tracks.json()["items"]] == [LOCAL_TRACK_ID]


@pytest.mark.asyncio
async def test_target_artist_browse_sorts_by_aggregate_album_count(
    target_services,
) -> None:
    store, _view, _favorites, _history, root = target_services
    membership = _membership(
        album_id="10000000-0000-4000-8000-000000000003",
        track_id="20000000-0000-4000-8000-000000000003",
        artist_id=IDENTIFIED_ARTIST_ID,
        root=root,
        title="Identified Again",
    )
    membership.artists = []
    await store.create_catalog_membership(membership)
    service = TargetNativeLibraryService(store)

    artists, total = await service.artists(
        limit=10,
        offset=0,
        search=None,
        sort_by="album_count",
        sort_order="desc",
    )

    assert total == 2
    assert artists[0].id == IDENTIFIED_ARTIST_ID
    assert artists[0].album_count == 2


@pytest.mark.asyncio
async def test_album_status_resolves_provider_identity_without_alias(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    service = TargetNativeLibraryService(store)

    assert await service.canonical_id("album", RELEASE_GROUP_MBID) is None

    status = await service.album_status(
        RELEASE_GROUP_MBID,
        quality_cutoff="lossless",
        upgrade_allowed=True,
    )

    assert status.in_library is True
    assert status.album_id == IDENTIFIED_ALBUM_ID
    assert status.track_count == 1
    assert [track.id for track in status.tracks] == [IDENTIFIED_TRACK_ID]


@pytest.mark.asyncio
async def test_album_rescan_scope_freezes_its_root_path(target_services) -> None:
    store, _view, _favorites, _history, root = target_services
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(id="root-1", path=str(root), label="Music")
            ]
        )
    )

    scopes = await TargetNativeLibraryService(store).album_rescan_scopes(
        IDENTIFIED_ALBUM_ID, resolver
    )

    assert len(scopes) == 1
    assert scopes[0].root_id == "root-1"
    assert scopes[0].root_path == str(root)


@pytest.mark.asyncio
async def test_target_native_contract_separates_local_and_provider_ids_and_redirects_aliases(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    await store.add_album_aliases(
        IDENTIFIED_ALBUM_ID,
        [
            LocalAlbumAlias(
                alias=RELEASE_GROUP_MBID,
                local_album_id=IDENTIFIED_ALBUM_ID,
                kind="legacy_release_group",
                created_at=3,
            )
        ],
        expected_album_revision=1,
        updated_at=3,
    )
    service = TargetNativeLibraryService(store)
    preferences = SimpleNamespace(
        get_download_policy=lambda: SimpleNamespace(
            quality_cutoff="lossless", upgrade_allowed=True
        )
    )
    app = FastAPI()
    app.include_router(library_target.router)
    app.dependency_overrides[get_target_native_library_service] = lambda: service
    request_history = AsyncMock()
    request_history.async_get_requested_mbids.return_value = set()
    app.dependency_overrides[get_request_history_store] = lambda: request_history
    app.dependency_overrides[get_preferences_service] = lambda: preferences
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="user", user_id="user-1"
    )
    client = build_test_client(app)

    albums = client.get("/library/albums?page_size=10").json()["items"]
    identified = next(item for item in albums if item["id"] == IDENTIFIED_ALBUM_ID)
    local_only = next(item for item in albums if item["id"] == LOCAL_ALBUM_ID)
    provider_ids = client.get("/library/mbids").json()
    detail = client.get(f"/library/albums/{LOCAL_ALBUM_ID}").json()
    artist = client.get(f"/library/artists/{LOCAL_ARTIST_ID}").json()
    artist_albums = client.get(f"/library/artists/{LOCAL_ARTIST_ID}/albums").json()
    resolved = client.post(
        "/library/resolve-tracks",
        json={
            "items": [
                {
                    "release_group_mbid": LOCAL_ALBUM_ID,
                    "disc_number": 1,
                    "track_number": 0,
                }
            ]
        },
    ).json()
    redirect = client.get(
        f"/library/albums/{RELEASE_GROUP_MBID}/tracks", follow_redirects=False
    )
    detail_redirect = client.get(
        f"/library/albums/{RELEASE_GROUP_MBID}", follow_redirects=False
    )

    assert identified["musicbrainz_release_group_id"] == RELEASE_GROUP_MBID
    assert local_only["musicbrainz_release_group_id"] is None
    assert local_only["musicbrainz_artist_id"] is None
    assert detail["id"] == LOCAL_ALBUM_ID
    assert detail["musicbrainz_release_group_id"] is None
    assert artist["id"] == LOCAL_ARTIST_ID
    assert artist_albums["items"][0]["id"] == LOCAL_ALBUM_ID
    assert resolved["items"][0]["track_source_id"] == LOCAL_TRACK_ID
    assert resolved["items"][0]["stream_url"].endswith(LOCAL_TRACK_ID)
    assert provider_ids == {
        "mbids": [RELEASE_GROUP_MBID],
        "requested_mbids": [],
    }
    assert redirect.status_code == 308
    assert redirect.headers["location"].endswith(
        f"/library/albums/{IDENTIFIED_ALBUM_ID}/tracks"
    )
    assert detail_redirect.status_code == 308
    assert detail_redirect.headers["location"].endswith(
        f"/library/albums/{IDENTIFIED_ALBUM_ID}"
    )


@pytest.mark.asyncio
async def test_isolated_target_compat_routes_browse_play_and_write_stable_references(
    target_services, tmp_path: Path
) -> None:
    from api.compat.common.deps import CompatServices, get_compat_services
    from api.compat.common.path_case import CompatPathCaseMiddleware
    from api.compat.jellyfin.router import router as jellyfin_router
    from api.compat.subsonic.router import router as subsonic_router
    from api.v1.schemas.settings import ConnectAppsSettings, LibrarySettings
    from core.config import Settings
    from infrastructure.crypto import init_crypto
    from infrastructure.persistence.app_password_store import AppPasswordStore
    from services.compat.advanced_transcode_service import AdvancedTranscodeService
    from services.compat.app_password_service import AppPasswordService
    from services.compat.avatar_service import CompatAvatarService
    from services.compat.stream_concurrency import StreamConcurrencyService
    from services.native.target_consumer_composition import (
        build_target_consumer_composition,
    )
    from services.preferences_service import PreferencesService
    from tests.compat.conftest import subsonic_query

    store, _view, _favorites, _history, root = target_services
    lock = threading.Lock()
    auth = AuthStore(store.db_path, lock)
    init_crypto(tmp_path / "config")
    app_passwords = AppPasswordService(AppPasswordStore(store.db_path, lock), auth)
    _record, secret = await app_passwords.create("user-1", "target client")
    settings = Settings()
    settings.config_file_path = tmp_path / "target-compat.json"
    preferences = PreferencesService(settings)
    preferences.save_connect_apps_settings(
        ConnectAppsSettings(subsonic_enabled=True, jellyfin_enabled=True)
    )
    preferences.save_library_settings(LibrarySettings(library_paths=[str(root)]))
    provider_covers = AsyncMock()
    provider_covers.get_release_group_cover.return_value = None
    provider_covers.get_release_group_cover_etag.return_value = None
    provider_covers.get_artist_image.return_value = None
    provider_covers.get_artist_image_etag.return_value = None
    plugin_host = SimpleNamespace(dispatch_scrobble=AsyncMock())
    target = build_target_consumer_composition(
        store=store,
        preferences=preferences,
        auth_store=auth,
        provider_covers=provider_covers,
        cache=AsyncMock(),
        cache_dir=tmp_path,
        client_factory=SimpleNamespace(
            resolve_lastfm=AsyncMock(return_value=None),
            resolve_listenbrainz=AsyncMock(return_value=None),
        ),
        listening_prefs_store=SimpleNamespace(
            get=AsyncMock(
                return_value=SimpleNamespace(
                    scrobble_to_lastfm=False,
                    scrobble_to_listenbrainz=False,
                )
            )
        ),
        now_playing=SimpleNamespace(
            update=AsyncMock(),
            remove=AsyncMock(),
            compat_now_playing=lambda: [],
        ),
        plugin_host=plugin_host,
    )

    artwork_context = await store.get_target_artwork_context("album", LOCAL_ALBUM_ID)
    direct_embedded_cover = await target.covers.get_release_group_cover(LOCAL_ALBUM_ID)
    assert direct_embedded_cover is not None, artwork_context
    scan = AsyncMock()
    scan.status.return_value = (False, 0)
    bundle = CompatServices(
        app_passwords=app_passwords,
        view=target.view,
        favorites=target.favorites,
        playlists=target.playlists,
        scrobble=target.scrobble,
        discover=target.discover,
        id_map=target.id_map,
        local_files=target.local_files,
        coverart=target.covers,
        preferences=preferences,
        transcode=AsyncMock(),
        stream_concurrency=StreamConcurrencyService(),
        now_playing=AsyncMock(),
        version=SimpleNamespace(
            get_current_version=lambda: SimpleNamespace(version="test")
        ),
        play_queue=target.play_queue,
        bookmarks=target.bookmarks,
        lyrics=AsyncMock(),
        avatars=CompatAvatarService(tmp_path),
        playback_report=target.playback_report,
        scan=scan,
        advanced_transcode=AdvancedTranscodeService(),
    )
    app = FastAPI()
    app.include_router(subsonic_router)
    app.include_router(jellyfin_router)
    app.add_middleware(
        CompatPathCaseMiddleware,
        routes=[*subsonic_router.routes, *jellyfin_router.routes],
    )
    app.dependency_overrides[get_compat_services] = lambda: bundle
    client = build_test_client(app)
    query = subsonic_query(secret, "target")

    search = client.get("/subsonic/rest/search3", params={**query, "query": ""}).json()[
        "subsonic-response"
    ]["searchResult3"]
    songs = search["song"]
    local_song = next(item for item in songs if item["id"].endswith(LOCAL_TRACK_ID))
    album_list = client.get(
        "/subsonic/rest/getAlbumList2",
        params={**query, "type": "alphabeticalByName", "size": 20},
    ).json()["subsonic-response"]["albumList2"]["album"]
    local_album = next(
        item for item in album_list if item["id"].endswith(LOCAL_ALBUM_ID)
    )
    album_info = client.get(
        "/subsonic/rest/getAlbumInfo2", params={**query, "id": local_album["id"]}
    ).json()["subsonic-response"]["albumInfo2"]
    artists = client.get("/subsonic/rest/getArtists", params=query).json()[
        "subsonic-response"
    ]["artists"]["index"]
    flat_artists = [artist for index in artists for artist in index["artist"]]
    local_artist = next(
        item for item in flat_artists if item["name"] == "Local Only Artist"
    )
    artist_detail = client.get(
        "/subsonic/rest/getArtist", params={**query, "id": local_artist["id"]}
    ).json()["subsonic-response"]["artist"]
    album_detail = client.get(
        "/subsonic/rest/getAlbum", params={**query, "id": local_album["id"]}
    ).json()["subsonic-response"]["album"]
    genres = client.get("/subsonic/rest/getGenres", params=query).json()[
        "subsonic-response"
    ]["genres"]["genre"]
    genre_songs = client.get(
        "/subsonic/rest/getSongsByGenre",
        params={**query, "genre": "Local Only Genre"},
    ).json()["subsonic-response"]["songsByGenre"]["song"]
    subsonic_cover = client.get(
        "/subsonic/rest/getCoverArt", params={**query, "id": local_album["id"]}
    )
    created = client.get(
        "/subsonic/rest/createPlaylist",
        params=[*query.items(), ("name", "Target mix"), ("songId", songs[0]["id"])],
    ).json()["subsonic-response"]["playlist"]
    streamed = client.get(
        "/subsonic/rest/stream", params={**query, "id": local_song["id"]}
    )
    client.post(
        "/subsonic/rest/savePlayQueue",
        params=[
            *query.items(),
            ("id", local_song["id"]),
            ("current", local_song["id"]),
        ],
    )
    client.post(
        "/subsonic/rest/createBookmark",
        params=[*query.items(), ("id", local_song["id"]), ("position", "1200")],
    )
    client.get("/subsonic/rest/star", params={**query, "id": local_song["id"]})
    subsonic_scrobble = client.get(
        "/subsonic/rest/scrobble",
        params={
            **query,
            "id": local_song["id"],
            "submission": "true",
            "time": str(int((time.time() - 60) * 1000)),
        },
    )
    scan_status = client.get("/subsonic/rest/getScanStatus", params=query)
    scan_start = client.post("/subsonic/rest/startScan", params=query)

    jellyfin_headers = {
        "Authorization": f'MediaBrowser Token="{secret}", Client="pytest"'
    }
    jf_albums = client.get(
        "/jellyfin/Items",
        params={"IncludeItemTypes": "MusicAlbum"},
        headers=jellyfin_headers,
    ).json()["Items"]
    jf_local = next(item for item in jf_albums if item["Name"] == "Local Only")
    jf_tracks = client.get(
        "/jellyfin/Items",
        params={"ParentId": jf_local["Id"]},
        headers=jellyfin_headers,
    ).json()["Items"]
    jf_stream = client.get(
        f"/jellyfin/Audio/{jf_tracks[0]['Id']}/stream",
        params={"static": "true", "ApiKey": secret},
    )
    jf_artists = client.get(
        "/jellyfin/Artists/AlbumArtists", headers=jellyfin_headers
    ).json()["Items"]
    jf_genres = client.get("/jellyfin/Genres", headers=jellyfin_headers).json()["Items"]
    jf_search = client.get(
        "/jellyfin/Items",
        params={"SearchTerm": "Local Only", "Recursive": "true"},
        headers=jellyfin_headers,
    ).json()["Items"]
    jf_cover = client.get(f"/jellyfin/Items/{jf_local['Id']}/Images/Primary")
    jf_favorite = client.post(
        f"/jellyfin/UserFavoriteItems/{jf_tracks[0]['Id']}",
        params={"userId": "user-1"},
        headers=jellyfin_headers,
    )
    jf_stopped = client.post(
        "/jellyfin/Sessions/Playing/Stopped",
        headers=jellyfin_headers,
        json={
            "ItemId": jf_tracks[0]["Id"],
            "PositionTicks": jf_tracks[0]["RunTimeTicks"],
            "RunTimeTicks": jf_tracks[0]["RunTimeTicks"],
        },
    )
    jf_id_again = await target.id_map.to_jf("album", LOCAL_ALBUM_ID)

    assert len(songs) == 2
    assert len(album_list) == 2
    assert "musicBrainzId" not in local_album
    assert "musicBrainzId" not in album_info
    assert artist_detail["album"][0]["id"] == local_album["id"]
    assert album_detail["song"][0]["id"] == local_song["id"]
    assert any(item["value"] == "Local Only Genre" for item in genres)
    assert [item["id"] for item in genre_songs] == [local_song["id"]]
    assert subsonic_cover.status_code == 200
    assert subsonic_cover.headers["content-type"] == "image/png"
    assert created["songCount"] == 1
    assert streamed.status_code == 200
    assert streamed.content.startswith(b"fLaC")
    assert jf_local.get("ProviderIds") in (None, {})
    assert jf_tracks[0].get("ProviderIds") in (None, {})
    assert any(item["Name"] == "Local Only Artist" for item in jf_artists)
    assert any(item["Name"] == "Local Only Genre" for item in jf_genres)
    assert any(item["Name"] == "Local Only" for item in jf_search)
    assert jf_cover.status_code == 200
    assert jf_cover.headers["content-type"] == "image/png"
    assert jf_stream.status_code == 200
    assert jf_stream.content.startswith(b"fLaC")
    assert jf_favorite.status_code == 200
    assert jf_favorite.json()["IsFavorite"] is True
    assert subsonic_scrobble.json()["subsonic-response"]["status"] == "ok"
    assert jf_stopped.status_code == 204
    assert scan_status.json()["subsonic-response"]["status"] == "ok"
    assert scan_start.json()["subsonic-response"]["status"] == "ok"
    scan.start.assert_awaited_once()
    assert jf_id_again == jf_local["Id"]
    provider_covers.get_release_group_cover.assert_not_awaited()
    with sqlite3.connect(store.db_path) as connection:
        assert connection.execute(
            "SELECT local_track_id FROM library_playlist_tracks"
        ).fetchone() == (songs[0]["id"][3:],)
        assert connection.execute(
            "SELECT local_track_id FROM library_play_history ORDER BY played_at"
        ).fetchall() == [(LOCAL_TRACK_ID,), (LOCAL_TRACK_ID,)]
        assert connection.execute(
            "SELECT local_track_id FROM library_compat_bookmarks"
        ).fetchone() == (LOCAL_TRACK_ID,)
        assert connection.execute(
            "SELECT local_track_id FROM library_compat_play_queue_items"
        ).fetchone() == (LOCAL_TRACK_ID,)

    plugin_calls = plugin_host.dispatch_scrobble.await_count
    await target.scrobble_service.submit_scrobble(
        ScrobbleRequest(
            track_name="Target Track",
            artist_name="Target Artist",
            timestamp=int(time.time()),
            duration_ms=180_000,
        ),
        user_id="user-1",
    )
    await asyncio.gather(*target.scrobble_service._plugin_tasks)
    assert plugin_host.dispatch_scrobble.await_count == plugin_calls + 1
