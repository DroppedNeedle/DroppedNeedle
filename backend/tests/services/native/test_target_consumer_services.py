import asyncio
import hashlib
import sqlite3
import shutil
import threading
import base64
import time
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, call, patch

import pytest
import pytest_asyncio
import msgspec
from fastapi import FastAPI
from mutagen.flac import FLAC, Picture

from api.compat.jellyfin.builders import JellyfinBuilder
from api.compat.subsonic.models import to_album_id3, to_child
from api.v1.routes import library_target, local_library, requests
from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from api.v1.schemas.discover import RadioPlanRequest, RadioSeedItem
from api.v1.schemas.scrobble import ScrobbleRequest
from core.dependencies import (
    get_local_files_service,
    get_preferences_service,
    get_request_history_store,
    get_request_service,
    get_target_native_library_service,
)
from core.exceptions import (
    ExternalServiceError,
    ProviderIdentityRequiredError,
    ResourceNotFoundError,
)
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
from services.spotify_import_service import (
    SpotifyImportService,
    cover_fetcher_for,
)
from tests.mocks.spotify_cdn_mock import (
    COVER_URL,
    JPEG_BYTES,
    SpotifyCdnMock,
)
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
COMPILATION_OTHER_TRACK_ID = "20000000-0000-4000-8000-000000000004"
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
async def test_target_identity_states_remain_separate_from_review_status(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    service = TargetNativeLibraryService(store)

    albums, _ = await service.albums(
        limit=10, offset=0, sort="name", search=None, file_format=None
    )
    artists, _ = await service.artists(
        limit=10, offset=0, search=None, sort_order="asc"
    )

    identified = next(album for album in albums if album.id == IDENTIFIED_ALBUM_ID)
    local_only = next(album for album in albums if album.id == LOCAL_ALBUM_ID)
    identified_artist = next(
        artist for artist in artists if artist.id == IDENTIFIED_ARTIST_ID
    )
    local_artist = next(artist for artist in artists if artist.id == LOCAL_ARTIST_ID)

    assert identified.album_identity_state == "release_group_linked"
    assert identified.musicbrainz_release_id is None
    assert local_only.album_identity_state == "local_only"
    assert identified_artist.artist_identity_state == "musicbrainz_linked"
    assert local_artist.artist_identity_state == "local_only"

    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET release_mbid = ? "
            "WHERE local_album_id = ?",
            ("70000000-0000-4000-8000-000000000001", IDENTIFIED_ALBUM_ID),
        )
        connection.execute(
            "INSERT INTO library_identification_reviews "
            "(id, local_album_id, state, reason_code, input_revision, created_at, updated_at) "
            "VALUES ('review-identified', ?, 'needs_review', 'TEST_REVIEW', "
            "'review-input-1', 4, 4)",
            (IDENTIFIED_ALBUM_ID,),
        )
        connection.execute(
            "INSERT INTO library_identification_reviews "
            "(id, local_album_id, state, reason_code, input_revision, created_at, updated_at) "
            "VALUES ('review-local', ?, 'needs_review', 'TEST_REVIEW', "
            "'review-input-2', 4, 4)",
            (LOCAL_ALBUM_ID,),
        )

    linked_detail = await service.album_detail(IDENTIFIED_ALBUM_ID)
    local_detail = await service.album_detail(LOCAL_ALBUM_ID)

    assert linked_detail is not None
    assert linked_detail.album_identity_state == "release_linked"
    assert linked_detail.musicbrainz_release_id == (
        "70000000-0000-4000-8000-000000000001"
    )
    assert linked_detail.identification_status == "manual_identity_needs_review"
    assert local_detail is not None
    assert local_detail.album_identity_state == "local_only"
    assert local_detail.identification_status == "needs_review"


@pytest.mark.asyncio
async def test_album_lists_project_active_custom_edition_identity(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    with sqlite3.connect(store.db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO library_custom_edition_manifests "
            "(id,local_album_id,version,release_group_mbid,album_title,"
            "album_artist_name,album_metadata_json,source_album_revision,"
            "input_revision,content_hash,sealed_by_user_id,sealed_at) VALUES "
            "('custom-manifest',?,1,?,'Local Only','Local Only Artist','{}',1,"
            "'input','content','user-1',4)",
            (LOCAL_ALBUM_ID, RELEASE_GROUP_MBID),
        )
        connection.execute(
            "INSERT INTO library_custom_edition_active "
            "(local_album_id,manifest_id,activated_at) VALUES "
            "(?,'custom-manifest',4)",
            (LOCAL_ALBUM_ID,),
        )
    service = TargetNativeLibraryService(store)

    albums, _ = await service.albums(
        limit=10, offset=0, sort="name", search=None, file_format=None
    )
    artist_albums = await service.artist_albums(LOCAL_ARTIST_ID)

    listed = next(album for album in albums if album.id == LOCAL_ALBUM_ID)
    assert listed.album_identity_state == "custom_edition"
    assert listed.musicbrainz_release_id is None
    assert artist_albums[0].album_identity_state == "custom_edition"


@pytest.mark.asyncio
async def test_album_detail_projects_current_management_identity_readiness(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    service = TargetNativeLibraryService(store)

    detail = await service.album_detail(IDENTIFIED_ALBUM_ID)
    assert detail is not None
    assert detail.management_identity_readiness == "exact_release_required"
    assert detail.mapped_track_count == 0

    release_mbid = "70000000-0000-4000-8000-000000000001"
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET release_mbid = ? "
            "WHERE local_album_id = ?",
            (release_mbid, IDENTIFIED_ALBUM_ID),
        )
        connection.execute(
            "UPDATE local_track_external_identities SET release_mbid = ?, "
            "release_track_mbid = 'release-track-1', medium_position = 1, "
            "release_track_position = 1 WHERE local_track_id = ?",
            (release_mbid, IDENTIFIED_TRACK_ID),
        )

    detail = await service.album_detail(IDENTIFIED_ALBUM_ID)
    assert detail is not None
    assert detail.management_identity_readiness == "ready"
    assert detail.mapped_track_count == 1

    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET release_track_position = NULL "
            "WHERE local_track_id = ?",
            (IDENTIFIED_TRACK_ID,),
        )

    detail = await service.album_detail(IDENTIFIED_ALBUM_ID)
    assert detail is not None
    assert detail.management_identity_readiness == "track_mapping_required"
    assert detail.mapped_track_count == 0

    assert service._management_identity_readiness(None, []) == "not_applicable"


def test_management_identity_readiness_rejects_duplicate_release_tracks() -> None:
    identity = {
        "release_group_mbid": RELEASE_GROUP_MBID,
        "release_mbid": "release-1",
    }
    track = {
        "identity_row_revision": 1,
        "recording_mbid": "recording-1",
        "identity_release_mbid": "release-1",
        "release_track_mbid": "release-track-1",
        "medium_position": 1,
        "release_track_position": 1,
    }

    assert (
        TargetNativeLibraryService._management_identity_readiness(
            identity, [track, {**track, "recording_mbid": "recording-2"}]
        )
        == "track_mapping_required"
    )


@pytest.mark.asyncio
async def test_artist_album_projection_includes_active_contribution_state(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO library_contribution_drafts "
            "(id, local_album_id, state, album_row_revision, input_revision, "
            "local_snapshot_json, resolved_draft_json, source_selection_json, "
            "created_at, updated_at) VALUES "
            "('contribution-1', ?, 'draft', 1, 'input-1', '{}', '{}', '{}', 4, 4)",
            (LOCAL_ALBUM_ID,),
        )

    albums = await TargetNativeLibraryService(store).artist_albums(LOCAL_ARTIST_ID)
    local_only = next(album for album in albums if album.id == LOCAL_ALBUM_ID)

    assert local_only.contribution_id == "contribution-1"
    assert local_only.contribution_state == "draft"


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
async def test_manual_identity_release_year_backfill_is_idempotent(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    release_mbid = "70000000-0000-4000-8000-000000000001"
    payload = '{"date":{"precision":"year","value":"1974"}}'
    payload_hash = hashlib.sha256(payload.encode()).hexdigest()
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET release_mbid = ? "
            "WHERE local_album_id = ?",
            (release_mbid, IDENTIFIED_ALBUM_ID),
        )
        connection.execute(
            "INSERT INTO library_management_metadata_snapshots "
            "(id, provider, entity_kind, entity_id, input_hash, "
            "canonical_payload_json, payload_sha256, fetched_at, expires_at, "
            "provider_version_notes) VALUES "
            "('snapshot-1', 'musicbrainz', 'release', ?, ?, ?, ?, 5, 10, 'test')",
            (release_mbid, "0" * 64, payload, payload_hash),
        )

    assert await store.backfill_manual_identity_release_years(updated_at=6) == 1
    assert await store.backfill_manual_identity_release_years(updated_at=7) == 0

    with sqlite3.connect(store.db_path) as connection:
        album = connection.execute(
            "SELECT year, updated_at FROM local_albums WHERE id = ?",
            (IDENTIFIED_ALBUM_ID,),
        ).fetchone()
        track = connection.execute(
            "SELECT year FROM local_tracks WHERE id = ?",
            (IDENTIFIED_TRACK_ID,),
        ).fetchone()
    assert album == (1974, 6)
    assert track == (1974,)


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

    selected, owned, pinned, basis = await service._effective_release_id(
        release_group_mbid, payload
    )

    assert selected == release_20
    assert owned is None
    assert pinned is None
    assert basis == "file_count"


@pytest.mark.asyncio
async def test_target_two_albums_sharing_a_group_yield_no_library_evidence(
    target_services,
) -> None:
    """B1 durable proof, unmocked: two active albums under one group (one
    fully tagged, one untagged) must not combine into file-count or embedded
    evidence - 20 files would otherwise infer the 20-track edition."""
    store, _view, _favorites, _history, root = target_services
    release_group_mbid = "70000000-0000-4000-8000-000000000002"
    release_11 = "71000000-0000-4000-8000-000000000002"
    release_20 = "72000000-0000-4000-8000-000000000002"

    async def _seed_album(suffix: str, embedded: str | None) -> str:
        album_id = f"10000000-0000-4000-8000-0000000000{suffix}"
        membership = _membership(
            album_id=album_id,
            track_id=f"20000000-0000-4000-8000-0000000000{suffix}",
            artist_id=f"30000000-0000-4000-8000-0000000000{suffix}",
            root=root,
            title=f"Split {suffix}",
        )
        template = membership.tracks[0]
        first = msgspec.structs.replace(template, embedded_release_mbid=embedded)
        membership.tracks[0] = first
        membership.track_credits[first.id] = list(
            membership.track_credits[template.id]
        )
        for position in range(2, 11):
            track_id = f"29{suffix}00000-0000-4000-8000-{position:012d}"
            path = root / f"{track_id}.flac"
            path.write_bytes(b"fLaC" + b"\0" * 64)
            track = msgspec.structs.replace(
                template,
                id=track_id,
                file_path=str(path),
                relative_path=path.name,
                path_hash=f"hash:{track_id}",
                stat_revision=f"stat:{track_id}",
                title=f"Split Track {position}",
                track_number=position,
                embedded_release_mbid=embedded,
            )
            membership.tracks.append(track)
            membership.track_credits[track_id] = list(
                membership.track_credits[template.id]
            )
        await store.create_catalog_membership(membership)
        return album_id

    album_a = await _seed_album("91", release_11)
    album_b = await _seed_album("92", None)
    with sqlite3.connect(store.db_path) as connection:
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'manual', 3)",
            [(album_a, release_group_mbid), (album_b, release_group_mbid)],
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

    selected, owned, pinned, basis = await service._effective_release_id(
        release_group_mbid, payload
    )

    assert selected == release_11
    assert owned is None
    assert pinned is None
    assert basis == "ranked"


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
async def test_target_repository_requested_mbids_exclude_track_keys(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    requests = RequestHistoryStore(store.db_path, threading.Lock())
    await requests.async_record_request(
        "requested-release", "Requested Artist", "Requested Album"
    )
    await requests.async_record_request(
        "recording-1",
        "Requested Artist",
        "Requested Album",
        track_title="Requested Track",
        request_kind="track",
    )

    repository = TargetLibraryRepository(store, requests)

    requested = await repository.get_requested_mbids()
    assert "track:recording-1" not in requested
    assert requested == {"requested-release"}


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
    service = TargetNativeLibraryService(store)

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
    assert await service.canonical_id("album", RELEASE_GROUP_MBID) is None
    assert {album.id for album in await service.album_copies(RELEASE_GROUP_MBID)} == {
        IDENTIFIED_ALBUM_ID,
        duplicate_album_id,
    }
    assert await service.canonical_id("album", "release-1") == IDENTIFIED_ALBUM_ID
    assert await service.canonical_id("album", "release-2") == duplicate_album_id

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
    provider.is_release_cover_warming = lambda identifier: identifier == "release-id"
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
    assert service.is_release_cover_warming("release-id") is True
    assert service.is_release_cover_warming("other-release") is False
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
            "DELETE FROM local_track_genres WHERE local_track_id IN (?, ?)",
            (IDENTIFIED_TRACK_ID, LOCAL_TRACK_ID),
        )
        connection.executemany(
            "INSERT INTO local_track_genres "
            "(local_track_id, position, name, folded_name, source) "
            "VALUES (?, 0, 'Ambient', 'ambient', 'local')",
            [(IDENTIFIED_TRACK_ID,), (LOCAL_TRACK_ID,)],
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
async def test_album_artist_ownership_and_contributor_appearances_stay_distinct(
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
    other_path = root / f"{COMPILATION_OTHER_TRACK_ID}.flac"
    other_path.write_bytes(b"fLaC" + b"\0" * 64)
    other_track = LocalTrack(
        id=COMPILATION_OTHER_TRACK_ID,
        local_album_id=COMPILATION_ALBUM_ID,
        root_id="root-1",
        file_path=str(other_path),
        relative_path=other_path.name,
        path_hash="hash:compilation-other",
        file_size_bytes=other_path.stat().st_size,
        file_mtime_ns=other_path.stat().st_mtime_ns,
        stat_revision="stat:compilation-other",
        title="Other Track",
        artist_name="Various Artists",
        album_title="Compilation",
        album_artist_name="Various Artists",
        file_format="flac",
        imported_at=4,
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[album_artist, track_artist],
            tracks=[track, other_track],
            album_credits=[
                LocalArtistCredit(local_artist_id=COMPILATION_ARTIST_ID, position=0)
            ],
            track_credits={
                COMPILATION_TRACK_ID: [
                    LocalArtistCredit(local_artist_id=TRACK_ARTIST_ID, position=0),
                    LocalArtistCredit(local_artist_id=IDENTIFIED_ARTIST_ID, position=1),
                ],
                COMPILATION_OTHER_TRACK_ID: [
                    LocalArtistCredit(local_artist_id=COMPILATION_ARTIST_ID, position=0)
                ],
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

    album_artists, album_artist_total = await view.get_artists(limit=50)
    contributors, contributor_total = await view.get_artists(
        limit=50, scope="contributors"
    )
    guest_detail = await view.get_artist_with_albums(TRACK_ARTIST_ID)
    native = TargetNativeLibraryService(store)
    guest = await native.artist(TRACK_ARTIST_ID)
    identified = await native.artist(IDENTIFIED_ARTIST_ID)
    guest_albums = await native.artist_albums(TRACK_ARTIST_ID)
    (
        appearances,
        appearance_total,
        appearance_track_total,
    ) = await native.artist_appearances(TRACK_ARTIST_ID, limit=20, offset=0)
    album_scope_count, contributor_scope_count = await native.artist_scope_counts()
    repository = TargetLibraryRepository(store)
    provider_artist_ids = await repository.get_artist_mbids()
    stats = await native.stats()

    assert album_artist_total == 3
    assert {artist.artist_mbid for artist in album_artists} == {
        IDENTIFIED_ARTIST_ID,
        LOCAL_ARTIST_ID,
        COMPILATION_ARTIST_ID,
    }
    assert contributor_total == 2
    assert {artist.artist_mbid for artist in contributors} == {
        IDENTIFIED_ARTIST_ID,
        TRACK_ARTIST_ID,
    }
    assert guest_detail is not None
    guest_view, owned_albums = guest_detail
    assert guest_view.artist_mbid == TRACK_ARTIST_ID
    assert owned_albums == []
    assert guest is not None
    assert guest.album_count == 0
    assert guest.track_count == 0
    assert guest.appearance_release_count == 1
    assert guest.appearance_track_count == 1
    assert guest.library_relationship == "contributor"
    assert identified is not None
    assert identified.library_relationship == "both"
    assert identified.album_count == 1
    assert identified.appearance_release_count == 1
    assert guest_albums == []
    assert appearance_total == 1
    assert appearance_track_total == 1
    assert len(appearances) == 1
    assert appearances[0].album.id == COMPILATION_ALBUM_ID
    assert appearances[0].album.track_count == 2
    assert [item.id for item in appearances[0].tracks] == [COMPILATION_TRACK_ID]
    assert (album_scope_count, contributor_scope_count) == (3, 2)
    assert await store.target_provider_artist_relationship(ARTIST_MBID) == (True, True)
    assert await store.target_provider_artist_relationship(TRACK_ARTIST_MBID) == (
        False,
        True,
    )
    assert provider_artist_ids == {ARTIST_MBID}
    assert await repository.get_all_artist_mbids() == provider_artist_ids
    assert stats.total_artists == 3

    await store.mark_target_tracks_missing(
        [COMPILATION_TRACK_ID, COMPILATION_OTHER_TRACK_ID],
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
        cache=InMemoryCache(),
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
async def test_spotify_import_persists_cover_in_target_store(
    target_services, tmp_path: Path
) -> None:
    """GH-287: the target composition must persist the fetched Spotify playlist
    cover into library_playlists.cover_image_path via the shared PlaylistService
    helper, and a reimport must preserve it (fill-only policy)."""
    store, _view, _favorites, _history, _root = target_services
    repository = TargetPlaylistRepository(store)
    playlists = PlaylistService(
        None,
        tmp_path,
        library_db=TargetLibraryRepository(store),
        async_repo=repository,
    )

    cdn = SpotifyCdnMock()
    spotify_client = AsyncMock()
    spotify_client.get_playlist.return_value = {
        "id": "spotify-1",
        "name": "Spotify",
        "images": [{"url": COVER_URL, "width": 640, "height": 640}],
    }
    spotify_client.get_playlist_tracks.return_value = []
    factory = AsyncMock()
    factory.resolve_spotify.return_value = spotify_client
    spotify = SpotifyImportService(
        client_factory=factory,
        playlist_repo=None,
        mb_repo=AsyncMock(),
        playlist_service=playlists,
        cache=InMemoryCache(),
        async_playlist_repo=repository,
        cover_fetcher=cover_fetcher_for(cdn.client()),
    )

    spotify_playlist_id = await spotify.ensure_playlist_record(
        "user-1", "spotify-1", "Imported Spotify"
    )
    await spotify.populate_playlist("user-1", "spotify-1", spotify_playlist_id)

    record = await playlists.get_playlist(spotify_playlist_id)
    assert record.cover_image_path is not None
    stored = Path(record.cover_image_path)
    assert stored.read_bytes() == JPEG_BYTES
    with sqlite3.connect(store.db_path) as connection:
        row = connection.execute(
            "SELECT cover_image_path FROM library_playlists WHERE id = ?",
            (spotify_playlist_id,),
        ).fetchone()
    assert row == (record.cover_image_path,)

    # Re-import: the imported cover already exists -> preserved byte-for-byte.
    before_bytes = stored.read_bytes()
    await spotify.populate_playlist("user-1", "spotify-1", spotify_playlist_id)
    after = (await playlists.get_playlist(spotify_playlist_id)).cover_image_path
    assert after == record.cover_image_path
    assert Path(after).read_bytes() == before_bytes


@pytest.mark.asyncio
@pytest.mark.parametrize("with_provider_identity", [True, False])
async def test_cutoff_repository_normalizes_target_artist_metadata(
    target_services, with_provider_identity: bool
) -> None:
    """NEW-TARGET-01: target cutoff rows expose the shared artist contract -
    display name plus ONLY the provider identity as artist_mbid (never the
    local album-artist ID alias)."""
    store, _view, _favorites, _history, root = target_services
    track_id = "20000000-0000-4000-8000-000000000095"
    album_id = "10000000-0000-4000-8000-000000000095"
    artist_id = "30000000-0000-4000-8000-000000000095"
    path = root / "cutoff.flac"
    shutil.copy2(Path(__file__).parents[2] / "fixtures/library/flac_no_tags.flac", path)
    stat = path.stat()
    _tag, info = AudioTagger().read_tags(path)
    membership = _membership(
        album_id=album_id,
        track_id=track_id,
        artist_id=artist_id,
        root=root,
        title="Cutoff",
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
            "UPDATE local_tracks SET file_format='mp3', bit_rate=192 "
            "WHERE id = ?",
            (track_id,),
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', 'rg-cutoff', 'manual', 1)",
            (album_id,),
        )
        if with_provider_identity:
            connection.execute(
                "INSERT INTO local_artist_external_identities "
                "(local_artist_id, provider, provider_artist_id, decision_source, selected_at) "
                "VALUES (?, 'musicbrainz', 'provider-artist-cutoff', 'manual', 1)",
                (artist_id,),
            )
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    local_files = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )
    repository = TargetLibraryRepository(store)

    rows = await repository.list_cutoff_unmet("lossless")

    assert len(rows) == 1
    row = rows[0]
    # Shared contract keys are present and unambiguous.
    assert row["artist_name"] == "Cutoff Artist"
    if with_provider_identity:
        assert row["artist_mbid"] == "provider-artist-cutoff"
    else:
        assert row["artist_mbid"] is None
    # The local album-artist ID never masquerades as a provider MBID.
    assert row["artist_mbid"] != artist_id
    assert row["current_tier"] == "mp3_192"
    # Target rows carry the provider identity in provider_release_group_mbid;
    # release_group_mbid falls back to the local album id when the seed had no
    # legacy release group.
    assert row["provider_release_group_mbid"] == "rg-cutoff"


@pytest.mark.asyncio
async def test_target_removal_writer_audits_without_deleting_stable_rows(
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
    assert actions == [("remove_track", "FILE_DELETED")]


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
        # F-TARGETCATALOG-05 (owner-signed): an unmatched provider-bearing
        # candidate is NOT owned by a local-only album - no folded fallback.
        (False, None),
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
async def test_album_status_resolves_unique_provider_identity_without_alias(
    target_services,
) -> None:
    store, _view, _favorites, _history, _root = target_services
    service = TargetNativeLibraryService(store)

    assert (
        await service.canonical_id("album", RELEASE_GROUP_MBID) == IDENTIFIED_ALBUM_ID
    )

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
    copies = client.get(f"/library/albums/{RELEASE_GROUP_MBID}/copies").json()
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
    artist_redirect = client.get(
        f"/library/artists/{ARTIST_MBID}", follow_redirects=False
    )

    assert identified["musicbrainz_release_group_id"] == RELEASE_GROUP_MBID
    assert identified["musicbrainz_release_id"] is None
    assert identified["album_identity_state"] == "release_group_linked"
    assert local_only["musicbrainz_release_group_id"] is None
    assert local_only["album_identity_state"] == "local_only"
    assert local_only["musicbrainz_artist_id"] is None
    assert detail["id"] == LOCAL_ALBUM_ID
    assert detail["musicbrainz_release_group_id"] is None
    assert artist["id"] == LOCAL_ARTIST_ID
    assert artist["artist_identity_state"] == "local_only"
    assert artist_albums["items"][0]["id"] == LOCAL_ALBUM_ID
    assert copies["total"] == 1
    assert copies["items"][0]["id"] == IDENTIFIED_ALBUM_ID
    assert resolved["items"][0]["track_source_id"] == LOCAL_TRACK_ID
    assert resolved["items"][0]["stream_url"].endswith(LOCAL_TRACK_ID)
    assert provider_ids == {
        "mbids": [RELEASE_GROUP_MBID],
        "requested_mbids": [],
    }
    request_history.async_get_requested_mbids.assert_awaited_once_with(
        request_kind="album"
    )
    assert redirect.status_code == 308
    assert redirect.headers["location"].endswith(
        f"/library/albums/{IDENTIFIED_ALBUM_ID}/tracks"
    )
    assert detail_redirect.status_code == 308
    assert detail_redirect.headers["location"].endswith(
        f"/library/albums/{IDENTIFIED_ALBUM_ID}"
    )
    assert artist_redirect.status_code == 308
    assert artist_redirect.headers["location"].endswith(
        f"/library/artists/{IDENTIFIED_ARTIST_ID}"
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
    plugin_host = SimpleNamespace(dispatch_event=AsyncMock(), dispatch_scrobble=AsyncMock())
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

    plugin_calls = plugin_host.dispatch_event.await_count
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
    assert plugin_host.dispatch_event.await_count == plugin_calls + 2
    kinds = [
        call.args[0].kind
        for call in plugin_host.dispatch_event.await_args_list[plugin_calls:]
    ]
    assert kinds == ["scrobble", "playback_started"]

    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO library_identification_reviews "
            "(id, local_album_id, state, reason_code, input_revision, created_at, updated_at) "
            "VALUES ('missing-final-track-review', ?, 'needs_review', "
            "'TEST_STALE_MEDIA', 'stale-input', 10, 10)",
            (LOCAL_ALBUM_ID,),
        )
    await store.mark_target_tracks_missing(
        [LOCAL_TRACK_ID],
        actor_user_id="user-1",
        reason_code="TEST_STALE_MEDIA",
        missing_at=11,
    )

    native_albums, _ = await target.view.get_albums(page_size=20)
    native_artists, _ = await target.view.get_artists(limit=20)
    subsonic_after = client.get(
        "/subsonic/rest/getAlbumList2",
        params={**query, "type": "alphabeticalByName", "size": 20},
    ).json()["subsonic-response"]["albumList2"]["album"]
    jellyfin_after = client.get(
        "/jellyfin/Items",
        params={"IncludeItemTypes": "MusicAlbum"},
        headers=jellyfin_headers,
    ).json()["Items"]

    assert LOCAL_ALBUM_ID not in {album.rg_mbid for album in native_albums}
    assert LOCAL_ARTIST_ID not in {artist.artist_mbid for artist in native_artists}
    assert all(item["name"] != "Local Only" for item in subsonic_after)
    assert all(item["Name"] != "Local Only" for item in jellyfin_after)
    with sqlite3.connect(store.db_path) as connection:
        assert connection.execute(
            "SELECT state FROM library_identification_reviews WHERE id = ?",
            ("missing-final-track-review",),
        ).fetchone() == ("needs_review",)


@pytest.mark.asyncio
async def test_target_removal_writer_handles_already_absent_file_idempotently(
    target_services, tmp_path: Path
) -> None:
    """F-TARGETCATALOG-01: an indexed row whose validated file is already gone
    removes idempotently - no 404, the row becomes missing, FILE_DELETED audit."""
    store, _view, _favorites, _history, root = target_services
    track_id = "20000000-0000-4000-8000-000000000098"
    album_id = "10000000-0000-4000-8000-000000000098"
    artist_id = "30000000-0000-4000-8000-000000000098"
    path = root / "vanished.flac"
    shutil.copy2(Path(__file__).parents[2] / "fixtures/library/flac_no_tags.flac", path)
    stat = path.stat()
    _tag, info = AudioTagger().read_tags(path)
    membership = _membership(
        album_id=album_id,
        track_id=track_id,
        artist_id=artist_id,
        root=root,
        title="Vanished",
    )
    membership.tracks[0].file_path = str(path)
    membership.tracks[0].relative_path = path.name
    membership.tracks[0].file_size_bytes = stat.st_size
    membership.tracks[0].file_mtime_ns = stat.st_mtime_ns
    membership.tracks[0].stat_revision = f"{stat.st_size}:{stat.st_mtime_ns}"
    membership.tracks[0].file_format = info.file_format
    membership.tracks[0].duration_seconds = info.duration_seconds
    await store.create_catalog_membership(membership)
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    local_files = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )
    writer = TargetCatalogWriterService(
        store, local_files, TargetNativeLibraryService(store)
    )
    path.unlink()  # the file disappears outside the service

    removed = await writer.remove_track(track_id, actor_user_id="user-1", delete_file=True)

    assert removed == [track_id]
    assert not path.exists()
    retained = await store.get_target_track(track_id)
    assert retained is not None and retained["availability"] == "missing"
    with sqlite3.connect(store.db_path) as connection:
        actions = connection.execute(
            "SELECT action_kind, reason_code FROM library_catalog_actions "
            "WHERE local_track_id = ?",
            (track_id,),
        ).fetchall()
    assert actions == [("remove_track", "FILE_DELETED")]
    # Retry: the row is now missing (not indexed), so the existing not-found
    # contract holds - no second destructive mutation is possible.


@pytest.mark.asyncio
async def test_target_album_removal_marks_mixed_existing_and_absent_tracks(
    target_services, tmp_path: Path
) -> None:
    """F-TARGETCATALOG-01: album removal must not abort at an already-absent
    path; existing files are unlinked and every safe row becomes missing."""
    store, _view, _favorites, _history, root = target_services
    album_id = "10000000-0000-4000-8000-000000000097"
    existing_id = "20000000-0000-4000-8000-000000000091"
    absent_id = "20000000-0000-4000-8000-000000000092"
    existing_path = root / "kept.flac"
    shutil.copy2(
        Path(__file__).parents[2] / "fixtures/library/flac_no_tags.flac", existing_path
    )
    # One membership holding BOTH album tracks (the store seeds one album).
    artist_id = "30000000-0000-4000-8000-000000000097"
    membership = _membership(
        album_id=album_id,
        track_id=existing_id,
        artist_id=artist_id,
        root=root,
        title="Mixed",
    )
    info = AudioTagger().read_tags(existing_path)[1]
    existing_track = membership.tracks[0]
    existing_track.file_path = str(existing_path)
    existing_track.relative_path = existing_path.name
    stat = existing_path.stat()
    existing_track.file_size_bytes = stat.st_size
    existing_track.file_mtime_ns = stat.st_mtime_ns
    existing_track.stat_revision = f"{stat.st_size}:{stat.st_mtime_ns}"
    existing_track.file_format = info.file_format
    existing_track.duration_seconds = info.duration_seconds
    absent_track = msgspec.structs.replace(
        existing_track,
        id=absent_id,
        file_path=str(root / "gone.flac"),
        relative_path="gone.flac",
        path_hash=f"hash:{absent_id}",
        title="Mixed Track 2",
    )
    membership.tracks.append(absent_track)
    membership.track_credits[absent_id] = list(membership.track_credits[existing_id])
    await store.create_catalog_membership(membership)
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    local_files = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )
    writer = TargetCatalogWriterService(
        store, local_files, TargetNativeLibraryService(store)
    )

    changed = await writer.remove_album(
        album_id, actor_user_id="user-1", delete_files=True
    )

    assert set(changed) == {existing_id, absent_id}
    assert not existing_path.exists()
    for track_id in (existing_id, absent_id):
        row = await store.get_target_track(track_id)
        assert row is not None and row["availability"] == "missing"


@pytest.mark.asyncio
async def test_target_album_removal_keeps_partial_failure_reporting(
    target_services, tmp_path: Path
) -> None:
    """F-TARGETCATALOG-01 negative boundary: a non-ENOENT unlink failure keeps
    its row indexed while safe rows are still marked missing."""
    store, _view, _favorites, _history, root = target_services
    album_id = "10000000-0000-4000-8000-000000000096"
    good_id = "20000000-0000-4000-8000-000000000093"
    bad_id = "20000000-0000-4000-8000-000000000094"
    good_path = root / "good.flac"
    shutil.copy2(
        Path(__file__).parents[2] / "fixtures/library/flac_no_tags.flac", good_path
    )
    bad_path = root / "bad.flac"
    bad_path.write_bytes(b"fLaC" + b"\0" * 64)
    # One membership holding BOTH album tracks.
    artist_id = "30000000-0000-4000-8000-000000000096"
    membership = _membership(
        album_id=album_id,
        track_id=good_id,
        artist_id=artist_id,
        root=root,
        title="Partial",
    )
    info = AudioTagger().read_tags(good_path)[1]
    good_stat = good_path.stat()
    bad_stat = bad_path.stat()
    good_track = membership.tracks[0]
    good_track.file_path = str(good_path)
    good_track.relative_path = good_path.name
    good_track.file_size_bytes = good_stat.st_size
    good_track.file_mtime_ns = good_stat.st_mtime_ns
    good_track.stat_revision = f"{good_stat.st_size}:{good_stat.st_mtime_ns}"
    good_track.file_format = info.file_format
    good_track.duration_seconds = info.duration_seconds
    bad_track = msgspec.structs.replace(
        good_track,
        id=bad_id,
        file_path=str(bad_path),
        relative_path=bad_path.name,
        path_hash=f"hash:{bad_id}",
        file_size_bytes=bad_stat.st_size,
        file_mtime_ns=bad_stat.st_mtime_ns,
        stat_revision=f"{bad_stat.st_size}:{bad_stat.st_mtime_ns}",
        title="Partial Track 2",
    )
    membership.tracks.append(bad_track)
    membership.track_credits[bad_id] = list(membership.track_credits[good_id])
    await store.create_catalog_membership(membership)
    preferences = SimpleNamespace(
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[SimpleNamespace(path=str(root))]
        )
    )
    local_files = LocalFilesService(
        TargetLibraryRepository(store), preferences, AsyncMock()
    )
    writer = TargetCatalogWriterService(
        store, local_files, TargetNativeLibraryService(store)
    )

    real_unlink = Path.unlink

    def failing_unlink(self, *args, **kwargs):
        if self == bad_path:
            raise PermissionError(1, "Operation not permitted")
        return real_unlink(self, *args, **kwargs)

    with patch("pathlib.Path.unlink", failing_unlink):
        with pytest.raises(ExternalServiceError):
            await writer.remove_album(album_id, actor_user_id="user-1", delete_files=True)

    good_row = await store.get_target_track(good_id)
    bad_row = await store.get_target_track(bad_id)
    assert good_row is not None and good_row["availability"] == "missing"
    assert bad_row is not None and bad_row["availability"] == "indexed"
    assert bad_path.exists()  # real failure is not hidden or retried destructively

@pytest.mark.asyncio
async def test_provider_bearing_candidates_never_consume_folded_fallback(
    target_services,
) -> None:
    """F-TARGETCATALOG-05 signed rule matrix: provider match owns; a
    provider-bearing candidate with only same-name local-only rows is NOT
    owned; providerless candidates keep the folded fallback with the existing
    one-year/unknown-year behaviour; placeholders, big year gaps, retired
    albums, and unindexed albums never create ownership."""
    store, _view, _favorites, _history, _root = target_services
    ownership = LibraryOwnershipService(store)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET year = 2020 WHERE id = ?", (LOCAL_ALBUM_ID,)
        )

    projections = await ownership.project_albums(
        [
            # Provider match stays owned.
            AlbumOwnershipCandidate(
                RELEASE_GROUP_MBID, "Identified", "Identified Artist", None
            ),
            # Provider-bearing mismatch with same-name local-only row: not owned.
            AlbumOwnershipCandidate(
                "different-provider-id", "Local Only", "Local Only Artist", 2020
            ),
            # Provider-bearing mismatch ignores years entirely (no fallback).
            AlbumOwnershipCandidate(
                "another-provider-id", "Local Only", "Local Only Artist", None
            ),
            # Providerless fallback keeps one-year tolerance.
            AlbumOwnershipCandidate(None, "Local Only", "Local Only Artist", 2021),
            # Providerless fallback keeps unknown-year acceptance.
            AlbumOwnershipCandidate(None, "Local Only", "Local Only Artist", None),
            # Providerless with >1 year difference is not owned.
            AlbumOwnershipCandidate(None, "Local Only", "Local Only Artist", 1999),
            # Placeholders prove nothing for providerless candidates.
            AlbumOwnershipCandidate(None, "Unknown album", "Unknown artist", None),
        ]
    )

    assert [(item.owned, item.local_album_id) for item in projections] == [
        (True, IDENTIFIED_ALBUM_ID),
        (False, None),
        (False, None),
        (True, LOCAL_ALBUM_ID),
        (True, LOCAL_ALBUM_ID),
        (False, None),
        (False, None),
    ]

    # Retired and unindexed local-only rows are invisible to the fallback.
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id="retired-local",
                root_id="root",
                grouping_key="retired-key",
                title="Local Only",
                album_artist_id=LOCAL_ARTIST_ID,
                album_artist_name="Local Only Artist",
                year=2020,
                created_at=1,
                updated_at=1,
                retired_into_album_id=LOCAL_ALBUM_ID,
            ),
            artists=[],
            tracks=[
                LocalTrack(
                    id="retired-track",
                    local_album_id="retired-local",
                    root_id="root",
                    file_path="/music/retired.flac",
                    relative_path="retired.flac",
                    path_hash="hash-retired",
                    file_size_bytes=1,
                    file_mtime_ns=1,
                    stat_revision="stat-retired",
                    tag_revision="tag-retired",
                    title="T",
                    artist_name="Artist",
                    album_title="Local Only",
                    album_artist_name="Local Only Artist",
                    duration_seconds=180,
                    file_format="flac",
                    imported_at=1,
                    applied_policy="automatic",
                )
            ],
            track_credits={},
        )
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability='missing' "
            "WHERE id='retired-track'"
        )
    projection = await ownership.project_album(
        release_group_mbid=None,
        title="Local Only",
        album_artist="Local Only Artist",
        year=2020,
    )
    # Still owned via LOCAL_ALBUM_ID; the retired duplicate adds no ambiguity.
    assert projection.owned is True


@pytest.mark.asyncio
async def test_discover_queue_keeps_mismatched_provider_candidate_visible(
    target_services,
) -> None:
    """F-MATCH-05 consumer check via F-TARGETCATALOG-05: the discover queue
    retains a candidate whose provider ID mismatches the library (owned=False),
    while the provider-matched candidate is filtered as in-library."""
    from types import SimpleNamespace
    from unittest.mock import AsyncMock

    from services.discover.queue_service import DiscoverQueueService

    store, *_rest = target_services
    integration = SimpleNamespace(
        get_queue_settings=lambda: SimpleNamespace(queue_size=10),
        is_jellyfin_enabled=lambda: False,
        is_library_configured=lambda: True,
    )
    mbid_resolution = AsyncMock()
    mbid_resolution.get_user_listened_release_group_mbids.return_value = set()
    mbid_resolution.get_library_album_mbids.return_value = set()
    service = DiscoverQueueService(
        listenbrainz_repo=AsyncMock(),
        jellyfin_repo=AsyncMock(),
        musicbrainz_repo=AsyncMock(),
        integration=integration,
        mbid_resolution=mbid_resolution,
        ownership_service=LibraryOwnershipService(store),
    )
    service._build_anonymous_queue = AsyncMock(
        return_value=[
            SimpleNamespace(
                release_group_mbid="mismatched-provider-id",
                album_name="Local Only",
                artist_name="Local Only Artist",
                in_library=False,
            ),
            SimpleNamespace(
                release_group_mbid=RELEASE_GROUP_MBID,
                album_name="Identified",
                artist_name="Identified Artist",
                in_library=False,
            ),
        ]
    )
    service._resolve_user_music = AsyncMock(
        return_value=(None, None, "", "", False, False, "listenbrainz")
    )

    response = await service.build_queue("user-1")

    by_mbid = {item.release_group_mbid: item for item in response.items}
    assert not by_mbid["mismatched-provider-id"].in_library  # stays visible
    # The provider-matched candidate is correctly filtered out as in-library.
    assert RELEASE_GROUP_MBID not in by_mbid


# F-TARGETCATALOG-06: batch canonical + album-track resolution


def _resolve_item(rg: str | None, disc: int | None, track: int | None):
    from api.v1.schemas.library import TrackResolveItem

    return TrackResolveItem(
        release_group_mbid=rg, disc_number=disc, track_number=track
    )


@pytest.mark.asyncio
async def test_batch_resolution_preserves_provider_alias_retired_and_ambiguous(
    target_services,
) -> None:
    """F-TARGETCATALOG-06: provider IDs (RG and release), aliases, local IDs,
    and retired IDs resolve identically under the batch resolver; an ambiguous
    provider identity stays unresolved; duplicates deduplicate."""
    store, *_ = target_services
    service = TargetNativeLibraryService(store)

    # Alias for the identified album.
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO local_album_aliases "
            "(alias, local_album_id, kind, created_at) VALUES (?, ?, 'merged_album', 1)",
            ("alias-identified", IDENTIFIED_ALBUM_ID),
        )
        # Retired album resolving to its survivor.
        connection.execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, "
            "title_folded, album_artist_id, grouping_source, created_at, updated_at, "
            "retired_into_album_id) VALUES ('album-retired', 'root-1', "
            "'g-retired', 'Old Name', 'old name', "
            "(SELECT album_artist_id FROM local_albums WHERE id = ?), "
            "'automatic', 1, 1, ?)",
            (IDENTIFIED_ALBUM_ID, IDENTIFIED_ALBUM_ID),
        )
        # A second ACTIVE indexed album sharing the identified release-group
        # MBID -> the provider identity is now AMBIGUOUS for that RG.
        connection.execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, "
            "title_folded, album_artist_id, grouping_source, created_at, "
            "updated_at) VALUES ('album-shadow', 'root-1', 'g-shadow', "
            "'Shadow Copy', 'shadow copy', (SELECT album_artist_id FROM "
            "local_albums WHERE id = ?), 'automatic', 1, 1)",
            (IDENTIFIED_ALBUM_ID,),
        )
        connection.execute(
            "INSERT INTO local_tracks (id, local_album_id, root_id, "
            "relative_path, file_path, path_hash, file_size_bytes, "
            "file_mtime_ns, stat_revision, tag_revision, title, title_folded, "
            "artist_name, artist_name_folded, album_title, album_title_folded, "
            "album_artist_name, album_artist_name_folded, ingest_source, "
            "stat_revision_kind, membership_source, disc_number, track_number, "
            "duration_seconds, file_format, availability, imported_at) "
            "VALUES ('track-shadow', 'album-shadow', 'root-1', "
            "'shadow/01.flac', '/music/shadow/01.flac', 'h-shadow', 1, 1, "
            "'stat-shadow', 'tag-shadow', 'Track', 'track', 'Artist', "
            "'artist', 'Shadow Copy', 'shadow copy', 'Artist', 'artist', "
            "'scan', 'exact', 'automatic', 1, 1, 180.0, 'flac', 'indexed', 1)"
        )

    resolved_map = await store.resolve_canonical_target_ids(
        "album",
        [
            RELEASE_GROUP_MBID,      # ambiguous: two active albums share this RG
            IDENTIFIED_ALBUM_ID,     # local ID
            "alias-identified",      # alias
            "album-retired",         # retired ID -> survivor
            "unknown-provider-id",   # missing
            RELEASE_GROUP_MBID,      # duplicate of the first item
        ],
    )
    assert resolved_map.get(IDENTIFIED_ALBUM_ID) == IDENTIFIED_ALBUM_ID
    assert resolved_map.get("alias-identified") == IDENTIFIED_ALBUM_ID
    assert resolved_map.get("album-retired") == IDENTIFIED_ALBUM_ID
    assert "unknown-provider-id" not in resolved_map
    # Ambiguous RG stays unresolved - no first-match behavior.
    assert RELEASE_GROUP_MBID.casefold() not in {
        value.casefold() for value in resolved_map.values()
    } or resolved_map.get(RELEASE_GROUP_MBID) != IDENTIFIED_ALBUM_ID


@pytest.mark.asyncio
async def test_resolve_tracks_batches_and_keeps_wire_contract(target_services):
    """F-TARGETCATALOG-06: one canonical batch lookup and one track-map batch
    read per request; invalid items keep base results; order preserved; the
    200-item boundary excludes later items."""
    store, *_ = target_services

    calls = {
        "canonical_single": 0,
        "canonical_batch": 0,
        "album_tracks_single": 0,
        "album_tracks_batch": 0,
    }
    original_canonical = type(store).resolve_canonical_target_id
    original_canonical_batch = type(store).resolve_canonical_target_ids
    original_tracks_single = type(store).get_target_album_tracks
    original_tracks_batch = type(store).get_target_album_tracks_batch

    async def spy_canonical(self, kind, identifier):
        calls["canonical_single"] += 1
        return await original_canonical(self, kind, identifier)

    async def spy_canonical_batch(self, kind, identifiers):
        calls["canonical_batch"] += 1
        return await original_canonical_batch(self, kind, identifiers)

    async def spy_tracks_single(self, album_identifier, **kwargs):
        calls["album_tracks_single"] += 1
        return await original_tracks_single(self, album_identifier, **kwargs)

    async def spy_tracks_batch(self, album_ids):
        calls["album_tracks_batch"] += 1
        return await original_tracks_batch(self, album_ids)

    type(store).resolve_canonical_target_id = spy_canonical
    type(store).resolve_canonical_target_ids = spy_canonical_batch
    type(store).get_target_album_tracks = spy_tracks_single
    type(store).get_target_album_tracks_batch = spy_tracks_batch
    try:
        service = TargetNativeLibraryService(store)
        # The fixture track sits at disc 1 / track number 0.
        items = [
            _resolve_item(RELEASE_GROUP_MBID, 1, 0),  # valid
            _resolve_item(RELEASE_GROUP_MBID, 1, 99),  # valid, no such track
            _resolve_item("unknown-rg", 1, 1),  # unknown ID
            _resolve_item(None, 1, None),  # missing RG+track: never triggers
            _resolve_item(RELEASE_GROUP_MBID, 1, 0),  # exact duplicate of item 0
        ]
        response = await service.resolve_tracks(items)
    finally:
        type(store).resolve_canonical_target_id = original_canonical
        type(store).resolve_canonical_target_ids = original_canonical_batch
        type(store).get_target_album_tracks = original_tracks_single
        type(store).get_target_album_tracks_batch = original_tracks_batch

    assert calls["canonical_batch"] == 1
    assert calls["album_tracks_batch"] == 1
    # No per-item single-item lookups.
    assert calls["canonical_single"] == 0
    assert calls["album_tracks_single"] == 0

    assert [item.release_group_mbid for item in response.items] == [
        RELEASE_GROUP_MBID,
        RELEASE_GROUP_MBID,
        "unknown-rg",
        None,
        RELEASE_GROUP_MBID,
    ]
    # Item 0 and its duplicate resolve to the same local track.
    assert response.items[0].source == "local"
    assert (
        response.items[4].track_source_id == response.items[0].track_source_id
    )
    # Track-number miss stays unresolved-but-shaped.
    assert response.items[1].source is None
    # Unknown ID keeps base result.
    assert response.items[2].source is None


@pytest.mark.asyncio
async def test_resolve_tracks_200_item_boundary_unchanged():
    from unittest.mock import AsyncMock, MagicMock

    store = MagicMock(spec=[
        "resolve_canonical_target_ids", "get_target_album_tracks_batch",
    ])

    async def batch_ids(kind, identifiers):
        return {identifier: identifier for identifier in identifiers}

    async def batch_tracks(album_ids):
        return {a: [] for a in album_ids}

    store.resolve_canonical_target_ids = AsyncMock(side_effect=batch_ids)
    store.get_target_album_tracks_batch = AsyncMock(side_effect=batch_tracks)
    service = TargetNativeLibraryService(store)

    items = [_resolve_item(f"rg-{i}", 1, 1) for i in range(205)]
    response = await service.resolve_tracks(items)

    # Exactly the first 200 items processed; the rest excluded.
    assert len(response.items) == 200
    requested = store.resolve_canonical_target_ids.await_args.args[1]
    assert len(requested) == 200


EMBEDDED_RELEASE_MBID = "80000000-0000-4000-8000-000000000001"
PINNED_RELEASE_MBID = "81000000-0000-4000-8000-000000000001"
OWNED_RELEASE_MBID = "82000000-0000-4000-8000-000000000001"


def _display_tracks(*embedded: str | None) -> list[dict]:
    return [{"embedded_release_mbid": value} for value in embedded]


def test_display_pick_prefers_pin_over_owned_and_tags() -> None:
    pick = TargetNativeLibraryService._display_pick(
        {"release_mbid": OWNED_RELEASE_MBID},
        _display_tracks(EMBEDDED_RELEASE_MBID, EMBEDDED_RELEASE_MBID),
        PINNED_RELEASE_MBID,
    )

    assert pick == (PINNED_RELEASE_MBID, "pin")


def test_display_pick_rejects_garbage_pin_and_returns_canonical_mbid() -> None:
    # A stale non-MBID pin row must never render as a confident pick.
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": OWNED_RELEASE_MBID},
        _display_tracks(EMBEDDED_RELEASE_MBID, EMBEDDED_RELEASE_MBID),
        "not-a-mbid",
    ) == (OWNED_RELEASE_MBID, "owned")
    # Unanimous tags differing only by case return the stored original.
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": None},
        _display_tracks(
            EMBEDDED_RELEASE_MBID.upper(), EMBEDDED_RELEASE_MBID.upper()
        ),
        None,
    ) == (EMBEDDED_RELEASE_MBID.upper(), "embedded_tags")
    # A malformed owned MBID falls through to tags instead of rendering.
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": "not-a-mbid"},
        _display_tracks(EMBEDDED_RELEASE_MBID, EMBEDDED_RELEASE_MBID),
        None,
    ) == (EMBEDDED_RELEASE_MBID, "embedded_tags")


def test_display_pick_owned_beats_embedded_tags() -> None:
    pick = TargetNativeLibraryService._display_pick(
        {"release_mbid": OWNED_RELEASE_MBID},
        _display_tracks(EMBEDDED_RELEASE_MBID, EMBEDDED_RELEASE_MBID),
        None,
    )

    assert pick == (OWNED_RELEASE_MBID, "owned")


def test_display_pick_embedded_tags_requires_full_coverage() -> None:
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": None},
        _display_tracks(EMBEDDED_RELEASE_MBID, EMBEDDED_RELEASE_MBID),
        None,
    ) == (EMBEDDED_RELEASE_MBID, "embedded_tags")
    # Partial coverage falls through.
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": None},
        _display_tracks(EMBEDDED_RELEASE_MBID, None),
        None,
    ) == (None, None)
    # Conflicting tags fall through.
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": None},
        _display_tracks(EMBEDDED_RELEASE_MBID, OWNED_RELEASE_MBID),
        None,
    ) == (None, None)
    # Invalid MBIDs fall through.
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": None},
        _display_tracks("not-a-mbid", "not-a-mbid"),
        None,
    ) == (None, None)
    # No tracks, no pick (this lane has no ranked fallback).
    assert TargetNativeLibraryService._display_pick(
        {"release_mbid": None}, [], None
    ) == (None, None)
    assert TargetNativeLibraryService._display_pick(None, [], None) == (None, None)


@pytest.mark.asyncio
async def test_album_detail_surfaces_embedded_tags_pick(target_services) -> None:
    store, _view, _favorites, _history, root = target_services
    album_id = "10000000-0000-4000-8000-000000000091"
    membership = _membership(
        album_id=album_id,
        track_id="20000000-0000-4000-8000-000000000091",
        artist_id="30000000-0000-4000-8000-000000000091",
        root=root,
        title="Tagged",
    )
    membership = msgspec.structs.replace(
        membership,
        tracks=[
            msgspec.structs.replace(
                membership.tracks[0], embedded_release_mbid=EMBEDDED_RELEASE_MBID
            )
        ],
    )
    await store.create_catalog_membership(membership)

    detail = await TargetNativeLibraryService(store).album_detail(album_id)

    assert detail is not None
    assert detail.display_release_mbid == EMBEDDED_RELEASE_MBID
    assert detail.pick_basis == "embedded_tags"
    # The display pick must not fabricate owned identity.
    assert detail.musicbrainz_release_id is None


@pytest.mark.asyncio
async def test_album_detail_surfaces_pin_pick(target_services) -> None:
    store, _view, _favorites, _history, _root = target_services
    await store.set_target_album_release_pin(
        IDENTIFIED_ALBUM_ID, PINNED_RELEASE_MBID, "user-1", "2026-09-09"
    )

    detail = await TargetNativeLibraryService(store).album_detail(IDENTIFIED_ALBUM_ID)

    assert detail is not None
    assert detail.display_release_mbid == PINNED_RELEASE_MBID
    assert detail.pick_basis == "pin"


@pytest.mark.asyncio
async def test_album_detail_drops_pin_from_another_group(target_services) -> None:
    """A well-formed pin for the wrong group (re-identified since pinning)
    must not render as a confident pick; the row stays for re-pinning."""
    store, _view, _favorites, _history, _root = target_services
    await store.set_target_album_release_pin(
        IDENTIFIED_ALBUM_ID, PINNED_RELEASE_MBID, "user-1", "2026-09-09"
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET release_group_mbid = ? "
            "WHERE local_album_id = ?",
            ("0f0f0f0f-0000-4000-8000-000000000000", IDENTIFIED_ALBUM_ID),
        )
        connection.commit()
        pin_row = connection.execute(
            "SELECT release_mbid FROM library_album_release_pins "
            "WHERE local_album_id = ?",
            (IDENTIFIED_ALBUM_ID,),
        ).fetchone()

    detail = await TargetNativeLibraryService(store).album_detail(IDENTIFIED_ALBUM_ID)

    assert pin_row is not None
    assert detail is not None
    assert detail.display_release_mbid is None
    assert detail.pick_basis is None
