"""Route tests for the native library browse + album-detail endpoints."""

import threading
from pathlib import Path
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException

from api.v1.routes.library import router
from core.dependencies import get_album_service, get_library_manager, get_library_scanner
from core.exceptions import ResourceNotFoundError
from infrastructure.persistence.library_db import LibraryDB
from middleware import _get_current_admin, _get_current_curator
from models.audio import AudioInfo, AudioTag
from services.native.library_manager import LibraryManager, LibraryTrack
from tests.helpers import build_test_client, mock_user, override_admin_auth, override_user_auth


@pytest.fixture
def manager(tmp_path) -> LibraryManager:
    db = LibraryDB(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    return LibraryManager(db)


def _passthrough_album_service():
    """The status route's coverage annotation, stubbed to identity (P5)."""
    svc = AsyncMock()

    async def _annotate(mbid, status):
        return status

    svc.annotate_album_coverage = AsyncMock(side_effect=_annotate)
    return svc


@pytest.fixture
def app(manager):
    application = FastAPI()
    application.include_router(router)
    application.dependency_overrides[get_library_manager] = lambda: manager
    application.dependency_overrides[get_album_service] = _passthrough_album_service
    return application


@pytest.fixture
def client(app):
    override_user_auth(app, role="admin")
    override_admin_auth(app)
    return build_test_client(app)


async def _seed_album(manager: LibraryManager):
    tag = AudioTag(
        title="Airbag", artist="Radiohead", album="OK Computer",
        album_artist="Radiohead", track_number=1, year=1997,
        musicbrainz_release_group_id="rg-ok",
    )
    info = AudioInfo(
        duration_seconds=260.0, bitrate=900, sample_rate=44100, channels=2,
        file_format="flac", file_size_bytes=1000, bit_depth=16,
    )
    await manager.upsert_file(
        Path("/music/a.flac"), tag, info, release_group_mbid="rg-ok", recording_mbid="rec-1"
    )


def test_albums_empty_returns_items_total(client):
    resp = client.get("/library/albums")
    assert resp.status_code == 200
    assert resp.json() == {"items": [], "total": 0}


@pytest.mark.asyncio
async def test_albums_lists_seeded_album(app, manager):
    await _seed_album(manager)
    override_user_auth(app, role="admin")
    client = build_test_client(app)
    body = client.get("/library/albums").json()
    assert body["total"] == 1
    assert body["items"][0]["release_group_mbid"] == "rg-ok"
    assert body["items"][0]["track_count"] == 1


@pytest.mark.asyncio
async def test_albums_paginates(app, manager):
    await _seed_album(manager)
    tag = AudioTag(
        title="Teardrop", artist="Massive Attack", album="Mezzanine",
        album_artist="Massive Attack", track_number=1, year=1998,
        musicbrainz_release_group_id="rg-mezz",
    )
    info = AudioInfo(
        duration_seconds=300.0, bitrate=256, sample_rate=44100, channels=2,
        file_format="m4a", file_size_bytes=2000,
    )
    await manager.upsert_file(
        Path("/music/b.m4a"), tag, info, release_group_mbid="rg-mezz", recording_mbid="rec-2"
    )
    override_user_auth(app, role="admin")
    client = build_test_client(app)
    body = client.get("/library/albums?page=1&page_size=1").json()
    assert body["total"] == 2  # full count, not the page size
    assert len(body["items"]) == 1


def test_tracks_empty_returns_page_shape(client):
    resp = client.get("/library/tracks")
    assert resp.status_code == 200
    assert resp.json() == {"items": [], "total": 0, "offset": 0, "limit": 48}


@pytest.mark.asyncio
async def test_tracks_lists_seeded_track_with_album_context(app, manager):
    await _seed_album(manager)
    override_user_auth(app, role="admin")
    client = build_test_client(app)
    body = client.get("/library/tracks").json()
    assert body["total"] == 1
    item = body["items"][0]
    assert item["title"] == "Airbag"
    assert item["album_name"] == "OK Computer"
    assert item["artist_name"] == "Radiohead"
    assert item["album_mbid"] == "rg-ok"
    assert item["track_file_id"]  # library_files UUID the player streams by


def test_tracks_requires_auth(app):
    client = build_test_client(app)  # no auth override
    assert client.get("/library/tracks").status_code == 401


def test_artists_empty(client):
    resp = client.get("/library/artists")
    assert resp.status_code == 200
    assert resp.json() == {"items": [], "total": 0}


def test_stats_empty_shape(client):
    body = client.get("/library/stats").json()
    assert body["total_albums"] == 0
    assert body["total_tracks"] == 0
    assert body["format_breakdown"] == {}
    assert body["unmatched_count"] == 0


@pytest.mark.asyncio
async def test_album_tracks_and_status(app, manager):
    await _seed_album(manager)
    override_user_auth(app, role="admin")
    client = build_test_client(app)
    tracks = client.get("/library/albums/rg-ok/tracks").json()
    assert len(tracks["items"]) == 1
    status = client.get("/library/albums/rg-ok/status").json()
    assert status["in_library"] is True
    assert status["track_count"] == 1


def test_album_status_absent_album(client):
    body = client.get("/library/albums/unknown-mbid/status").json()
    assert body["in_library"] is False
    assert body["track_count"] == 0


def test_rescan_returns_202_for_admin(client):
    resp = client.post("/library/albums/rg-ok/rescan")
    assert resp.status_code == 202


def test_albums_requires_auth(app):
    client = build_test_client(app)  # no auth override
    assert client.get("/library/albums").status_code == 401


async def _seed_mezzanine(manager: LibraryManager):
    tag = AudioTag(
        title="Teardrop", artist="Massive Attack", album="Mezzanine",
        album_artist="Massive Attack", track_number=1, year=1998,
        musicbrainz_release_group_id="rg-mezz",
    )
    info = AudioInfo(
        duration_seconds=300.0, bitrate=256, sample_rate=44100, channels=2,
        file_format="m4a", file_size_bytes=2000,
    )
    await manager.upsert_file(
        Path("/music/b.m4a"), tag, info, release_group_mbid="rg-mezz", recording_mbid="rec-2"
    )


@pytest.mark.asyncio
async def test_albums_filter_by_search_query(app, manager):
    await _seed_album(manager)      # OK Computer / Radiohead (rg-ok)
    await _seed_mezzanine(manager)  # Mezzanine / Massive Attack (rg-mezz)
    override_user_auth(app, role="admin")
    client = build_test_client(app)
    body = client.get("/library/albums?q=mezz").json()
    assert body["total"] == 1
    assert body["items"][0]["release_group_mbid"] == "rg-mezz"


@pytest.mark.asyncio
async def test_albums_filter_by_format(app, manager):
    await _seed_album(manager)      # flac (rg-ok)
    await _seed_mezzanine(manager)  # m4a (rg-mezz)
    override_user_auth(app, role="admin")
    client = build_test_client(app)
    body = client.get("/library/albums?format=flac").json()
    assert body["total"] == 1
    assert body["items"][0]["release_group_mbid"] == "rg-ok"


# tag-write route wiring; scanner logic covered in test_library_scanner

_VALID_TAG_BODY = {
    "title": "New Title",
    "artist": "Radiohead",
    "album": "OK Computer",
    "track_number": 1,
    "musicbrainz_release_group_id": "rg-ok",
}


def test_update_track_tags_returns_updated_track(app):
    scanner = AsyncMock()
    scanner.update_track_tags = AsyncMock(
        return_value=LibraryTrack(track_title="New Title", file_path="/music/a.flac")
    )
    app.dependency_overrides[get_library_scanner] = lambda: scanner
    override_user_auth(app, role="admin")
    override_admin_auth(app)
    client = build_test_client(app)
    resp = client.post("/library/tracks/file-1", json=_VALID_TAG_BODY)
    assert resp.status_code == 200
    assert resp.json()["track_title"] == "New Title"
    scanner.update_track_tags.assert_awaited_once()


def test_update_track_tags_unknown_file_returns_404(app):
    scanner = AsyncMock()
    scanner.update_track_tags = AsyncMock(side_effect=ResourceNotFoundError("nope"))
    app.dependency_overrides[get_library_scanner] = lambda: scanner
    override_user_auth(app, role="admin")
    override_admin_auth(app)
    client = build_test_client(app)
    resp = client.post("/library/tracks/missing", json=_VALID_TAG_BODY)
    assert resp.status_code == 404


def test_update_track_tags_forbidden_for_non_admin(app):
    def reject_admin():
        raise HTTPException(status_code=403, detail="Admin access required")

    app.dependency_overrides[_get_current_admin] = reject_admin
    override_user_auth(app, role="user")
    client = build_test_client(app)
    resp = client.post("/library/tracks/file-1", json=_VALID_TAG_BODY)
    assert resp.status_code == 403


def test_get_track_tags_returns_tags(app):
    scanner = AsyncMock()
    scanner.read_track_tags = AsyncMock(
        return_value=AudioTag(
            title="T", artist="A", album="Al", track_number=1,
            musicbrainz_release_group_id="rg-ok",
        )
    )
    app.dependency_overrides[get_library_scanner] = lambda: scanner
    override_user_auth(app, role="admin")
    override_admin_auth(app)
    client = build_test_client(app)
    resp = client.get("/library/tracks/file-1/tags")
    assert resp.status_code == 200
    assert resp.json()["title"] == "T"


def test_get_track_tags_forbidden_for_non_admin(app):
    def reject_admin():
        raise HTTPException(status_code=403, detail="Admin access required")

    app.dependency_overrides[_get_current_admin] = reject_admin
    override_user_auth(app, role="user")
    client = build_test_client(app)
    assert client.get("/library/tracks/file-1/tags").status_code == 403


def _override_remove_album(app, *, removal=None, retries_side_effect=None):
    from api.v1.schemas.library import AlbumRemoveResponse
    from core.dependencies import get_download_service, get_library_service

    library_service = AsyncMock()
    library_service.remove_album.return_value = removal or AlbumRemoveResponse(
        success=True, artist_removed=False
    )
    download_service = AsyncMock()
    if retries_side_effect is not None:
        download_service.purge_album_downloads.side_effect = retries_side_effect
    app.dependency_overrides[get_library_service] = lambda: library_service
    app.dependency_overrides[get_download_service] = lambda: download_service
    override_user_auth(app, role="admin")
    override_admin_auth(app)
    return build_test_client(app), download_service


def test_remove_album_stops_pending_retries(app):
    client, download_service = _override_remove_album(app)
    resp = client.delete("/library/album/rg-ok?delete_files=true")
    assert resp.status_code == 200
    assert resp.json()["success"] is True
    download_service.purge_album_downloads.assert_awaited_once_with("rg-ok")


def test_remove_album_succeeds_even_if_stopping_retries_fails(app):
    # Stopping retries is best-effort: a failure there must not fail the removal the
    # user already confirmed.
    client, download_service = _override_remove_album(
        app, retries_side_effect=RuntimeError("boom")
    )
    resp = client.delete("/library/album/rg-ok")
    assert resp.status_code == 200
    assert resp.json()["success"] is True
    download_service.purge_album_downloads.assert_awaited_once_with("rg-ok")


# -- P5: coverage fields on the wire + the orphan-remove endpoint --


@pytest.mark.asyncio
async def test_album_status_carries_coverage_fields(app, manager):
    """The route hands the status through the AlbumService coverage annotation and
    the enriched fields serialise onto the wire (drives the honest badge, the
    matched-only Play All, and the orphan section)."""
    await _seed_album(manager)

    async def _annotate(mbid, status):
        status.expected_tracks = 1
        status.covered_tracks = 0
        status.matched_file_ids = []
        status.orphans = list(status.tracks)
        return status

    svc = AsyncMock()
    svc.annotate_album_coverage = AsyncMock(side_effect=_annotate)
    app.dependency_overrides[get_album_service] = lambda: svc
    override_user_auth(app, role="user")
    client = build_test_client(app)

    body = client.get("/library/albums/rg-ok/status").json()

    assert body["expected_tracks"] == 1
    assert body["covered_tracks"] == 0
    assert body["matched_file_ids"] == []
    assert len(body["orphans"]) == 1
    assert body["orphans"][0]["track_title"] == "Airbag"


def _remove_app(service=None, *, curator=True):
    from core.dependencies import get_library_service as _get_lib_svc

    application = FastAPI()
    application.include_router(router)
    if service is not None:
        application.dependency_overrides[_get_lib_svc] = lambda: service
    override_user_auth(application, role="user")
    if curator:
        application.dependency_overrides[_get_current_curator] = lambda: mock_user(
            role="trusted", user_id="cur-1"
        )
    return application


def test_remove_track_requires_curator():
    # auth matrix: plain user (no curator/admin) never reaches the service
    service = AsyncMock()
    client = build_test_client(_remove_app(service, curator=False))
    resp = client.delete("/library/tracks/file-1")
    assert resp.status_code in (401, 403)
    service.remove_file.assert_not_called()


def test_remove_track_unauthenticated_401():
    application = FastAPI()
    application.include_router(router)
    client = build_test_client(application)
    assert client.delete("/library/tracks/file-1").status_code == 401


def test_remove_track_curator_ok():
    from api.v1.schemas.common import StatusMessageResponse

    service = AsyncMock()
    service.remove_file.return_value = StatusMessageResponse(status="ok", message="File removed")
    client = build_test_client(_remove_app(service))
    resp = client.delete("/library/tracks/file-1")
    assert resp.status_code == 200
    assert resp.json()["status"] == "ok"
    service.remove_file.assert_awaited_once_with("file-1")


def test_remove_track_missing_404():
    service = AsyncMock()
    service.remove_file.side_effect = ResourceNotFoundError("Library file not found")
    client = build_test_client(_remove_app(service))
    assert client.delete("/library/tracks/nope").status_code == 404
