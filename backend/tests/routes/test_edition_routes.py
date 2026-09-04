"""Edition selection routes (CollectionManagement Feature E): viewing is open,
pin/acquire are admin/trusted-only (D16)."""

import asyncio
import sqlite3
import threading
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import FastAPI

from api.v1.routes import albums
from api.v1.routes import library_target
from api.v1.schemas.album import AlbumTracksInfo
from core.dependencies import get_album_service, get_download_service
from core.exceptions import ConflictError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from middleware import _get_current_curator, _get_current_user
from models.album import AlbumInfo, Track
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
from services.album_service import AlbumService
from services.native.target_reference_adapters import TargetAlbumReleasePinStore
from tests.helpers import build_test_client, mock_user

RG = "11111111-1111-4111-8111-111111111111"
REL = "22222222-2222-4222-8222-222222222222"
REL_B = "33333333-3333-4333-8333-333333333333"
FOREIGN_REL = "44444444-4444-4444-8444-444444444444"


def _app(album_service, download_service=None, *, curator: bool = False) -> FastAPI:
    app = FastAPI()
    app.include_router(albums.router)
    app.dependency_overrides[get_album_service] = lambda: album_service
    app.dependency_overrides[get_download_service] = lambda: download_service or AsyncMock()
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user", user_id="u1")
    if curator:
        app.dependency_overrides[_get_current_curator] = lambda: mock_user(
            role="trusted", user_id="cur-1"
        )
    return app


def _editions_payload() -> dict:
    return {
        "items": [
            {
                "release_mbid": REL, "title": "OK Computer", "disambiguation": None,
                "date": "1997-06-16", "country": "GB", "packaging": None,
                "status": "Official", "track_count": 12, "is_owned": True,
                "is_pinned": False,
            }
        ],
        "pinned_release_mbid": None,
        "owned_release_mbid": REL,
        "selected_release_mbid": REL,
    }


def test_editions_list_is_viewable_by_any_user():
    album_service = AsyncMock()
    album_service.list_editions.return_value = _editions_payload()

    resp = build_test_client(_app(album_service)).get(f"/albums/{RG}/editions")

    assert resp.status_code == 200
    body = resp.json()
    assert body["items"][0]["release_mbid"] == REL
    assert body["items"][0]["is_owned"] is True
    assert body["owned_release_mbid"] == REL
    assert body["selected_release_mbid"] == REL


def test_album_and_track_routes_serialize_selected_release():
    album_service = AsyncMock()
    album_service.get_album_info.return_value = AlbumInfo(
        title="OK Computer",
        musicbrainz_id=RG,
        artist_name="Radiohead",
        artist_id="artist-1",
        selected_release_mbid=REL,
    )
    album_service.get_album_tracks_info.return_value = AlbumTracksInfo(
        tracks=[Track(position=1, title="Airbag")],
        total_tracks=1,
        selected_release_mbid=REL,
    )
    client = build_test_client(_app(album_service))

    assert client.get(f"/albums/{RG}").json()["selected_release_mbid"] == REL
    assert client.get(f"/albums/{RG}/tracks").json()["selected_release_mbid"] == REL


def test_pin_and_acquire_require_curator_role():
    album_service = AsyncMock()
    download_service = AsyncMock()
    client = build_test_client(_app(album_service, download_service))

    # no curator auth state -> 401 before any service call
    assert client.put(f"/albums/{RG}/edition", json={"release_mbid": REL}).status_code == 401
    assert client.delete(f"/albums/{RG}/edition").status_code == 401
    assert client.post(f"/albums/{RG}/edition/acquire").status_code == 401
    album_service.set_edition_pin.assert_not_awaited()
    download_service.acquire_edition.assert_not_awaited()


def test_pin_set_clear_and_acquire_for_curator():
    album_service = AsyncMock()
    download_service = AsyncMock()
    download_service.acquire_edition.return_value = {
        "release_mbid": REL, "total_tracks": 12, "requested": 2, "upgrades": 1, "skipped": 9,
    }
    client = build_test_client(_app(album_service, download_service, curator=True))

    resp = client.put(f"/albums/{RG}/edition", json={"release_mbid": REL})
    assert resp.status_code == 200
    assert resp.json()["pinned_release_mbid"] == REL
    album_service.set_edition_pin.assert_awaited_once_with(RG, REL, "cur-1")

    resp = client.delete(f"/albums/{RG}/edition")
    assert resp.status_code == 200
    assert resp.json()["pinned_release_mbid"] is None
    album_service.clear_edition_pin.assert_awaited_once_with(RG)

    resp = client.post(f"/albums/{RG}/edition/acquire")
    assert resp.status_code == 200
    assert resp.json() == {
        "release_mbid": REL, "total_tracks": 12, "requested": 2, "upgrades": 1, "skipped": 9,
    }
    download_service.acquire_edition.assert_awaited_once_with("cur-1", RG)


def _local_membership(suffix: str) -> CatalogMembership:
    artist = LocalArtist(
        id=f"artist-{suffix}",
        display_name=f"Artist {suffix}",
        folded_name=f"artist {suffix}",
        kind="person",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=f"album-{suffix}",
        root_id="root-1",
        grouping_key=f"group-{suffix}",
        title=f"Album {suffix}",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        is_compilation=False,
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=f"track-{suffix}",
        local_album_id=album.id,
        root_id="root-1",
        file_path=f"/music/{suffix}.flac",
        relative_path=f"{suffix}.flac",
        path_hash=f"hash-{suffix}",
        file_size_bytes=100,
        file_mtime_ns=200,
        stat_revision=f"stat-{suffix}",
        title=f"Track {suffix}",
        artist_name=artist.display_name,
        album_title=album.title,
        album_artist_name=artist.display_name,
        file_format="flac",
        imported_at=1,
    )
    return CatalogMembership(
        album=album,
        artists=[artist],
        tracks=[track],
        track_credits={track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]},
    )


def _seeded_shared_rg_store(tmp_path) -> NativeLibraryStore:
    """Two local copies sharing one RG plus one RG-less copy (naked)."""
    db_path = tmp_path / "library.db"
    with sqlite3.connect(db_path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.executemany(
            "INSERT INTO auth_users(id) VALUES (?)", [("admin",), ("worker",)]
        )

    async def _seed() -> NativeLibraryStore:
        store = NativeLibraryStore(db_path, threading.Lock())
        for suffix in ("pin-a", "pin-b", "naked"):
            await store.create_catalog_membership(_local_membership(suffix))
        with sqlite3.connect(db_path) as connection:
            connection.executemany(
                "INSERT INTO local_album_external_identities "
                "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
                "VALUES (?, 'musicbrainz', ?, 'manual', 2)",
                [("album-pin-a", RG), ("album-pin-b", RG)],
            )
        return store

    return asyncio.run(_seed())


def _target_album_service(store: NativeLibraryStore) -> AlbumService:
    mb_repo = MagicMock()
    mb_repo.get_release_group_by_id = AsyncMock(
        return_value={
            "id": RG,
            "releases": [
                {
                    "id": REL,
                    "title": "OK Computer",
                    "status": "Official",
                    "media": [{"track-count": 12}],
                },
                {
                    "id": REL_B,
                    "title": "OK Computer",
                    "status": "Official",
                    "media": [{"track-count": 20}],
                },
            ],
        }
    )
    memory_cache = AsyncMock()
    memory_cache.get.return_value = None
    disk_cache = AsyncMock()
    disk_cache.get_album.return_value = None
    library_db = MagicMock()
    library_db.get_library_files_for_album = AsyncMock(return_value=[])
    return AlbumService(
        library_repo=MagicMock(),
        mb_repo=mb_repo,
        library_db=library_db,
        memory_cache=memory_cache,
        disk_cache=disk_cache,
        preferences_service=MagicMock(),
        audiodb_image_service=MagicMock(),
        release_pin_store=TargetAlbumReleasePinStore(store),
        native_library_store=store,
    )


def _target_app(service: AlbumService, *, curator: bool = False) -> FastAPI:
    app = FastAPI()
    app.include_router(library_target.router)
    app.dependency_overrides[get_album_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role="user", user_id="u1"
    )
    if curator:
        app.dependency_overrides[_get_current_curator] = lambda: mock_user(
            role="trusted", user_id="cur-1"
        )
    return app


def test_local_edition_pins_round_trip_per_copy_independently(tmp_path):
    store = _seeded_shared_rg_store(tmp_path)
    service = _target_album_service(store)
    client = build_test_client(_target_app(service, curator=True))

    assert client.get("/library/albums/album-pin-a/edition").json() == {
        "pinned_release_mbid": None
    }

    resp = client.put("/library/albums/album-pin-a/edition", json={"release_mbid": REL})
    assert resp.status_code == 200
    assert resp.json() == {"pinned_release_mbid": REL}

    resp = client.put("/library/albums/album-pin-b/edition", json={"release_mbid": REL_B})
    assert resp.status_code == 200
    assert resp.json() == {"pinned_release_mbid": REL_B}

    assert client.get("/library/albums/album-pin-a/edition").json() == {
        "pinned_release_mbid": REL
    }
    assert client.get("/library/albums/album-pin-b/edition").json() == {
        "pinned_release_mbid": REL_B
    }

    # The RG-keyed read still rejects the shared identity while copies stay independent.
    with pytest.raises(ConflictError, match="multiple local albums"):
        asyncio.run(store.get_target_album_release_pin(RG))

    # The shared-RG editions listing degrades to unpinned instead of raising.
    data = asyncio.run(service.list_editions(RG))
    assert data["pinned_release_mbid"] is None
    assert {item["release_mbid"] for item in data["items"]} == {REL, REL_B}

    # Clearing one copy leaves the other pinned.
    resp = client.delete("/library/albums/album-pin-a/edition")
    assert resp.status_code == 200
    assert resp.json() == {"pinned_release_mbid": None}
    assert client.get("/library/albums/album-pin-a/edition").json() == {
        "pinned_release_mbid": None
    }
    assert client.get("/library/albums/album-pin-b/edition").json() == {
        "pinned_release_mbid": REL_B
    }


def test_local_edition_pin_unknown_and_rgless_albums_404(tmp_path):
    store = _seeded_shared_rg_store(tmp_path)
    client = build_test_client(_target_app(_target_album_service(store), curator=True))

    assert client.get("/library/albums/ghost/edition").status_code == 404
    assert (
        client.put("/library/albums/ghost/edition", json={"release_mbid": REL}).status_code
        == 404
    )
    assert client.delete("/library/albums/ghost/edition").status_code == 404
    assert client.get("/library/albums/album-naked/edition").status_code == 404
    assert (
        client.put(
            "/library/albums/album-naked/edition", json={"release_mbid": REL}
        ).status_code
        == 404
    )


def test_local_edition_pin_rejects_foreign_release(tmp_path):
    store = _seeded_shared_rg_store(tmp_path)
    client = build_test_client(_target_app(_target_album_service(store), curator=True))

    resp = client.put(
        "/library/albums/album-pin-a/edition", json={"release_mbid": FOREIGN_REL}
    )
    assert resp.status_code == 404
    assert client.get("/library/albums/album-pin-a/edition").json() == {
        "pinned_release_mbid": None
    }


def test_local_edition_pin_writes_require_curator(tmp_path):
    store = _seeded_shared_rg_store(tmp_path)
    service = _target_album_service(store)
    client = build_test_client(_target_app(service))

    assert client.get("/library/albums/album-pin-a/edition").status_code == 200
    assert (
        client.put(
            "/library/albums/album-pin-a/edition", json={"release_mbid": REL}
        ).status_code
        == 401
    )
    assert client.delete("/library/albums/album-pin-a/edition").status_code == 401
    assert asyncio.run(store.get_target_album_release_pin("album-pin-a")) is None
