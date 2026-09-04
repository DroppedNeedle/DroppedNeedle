import pytest
from unittest.mock import AsyncMock, patch
from io import BytesIO
from pathlib import Path

from fastapi import FastAPI
from fastapi.testclient import TestClient

from api.v1.routes.playlists import router as playlists_router
from api.v1.schemas.request import BatchRequestResponse
from core.dependencies import get_playlist_service, get_jellyfin_library_service, get_local_files_service, get_navidrome_library_service, get_request_service
from core.exceptions import PlaylistNotFoundError, InvalidPlaylistDataError, ResourceNotFoundError, ValidationError
from core.exception_handlers import resource_not_found_handler, validation_error_handler, general_exception_handler
from repositories.playlist_repository import PlaylistRecord, PlaylistSummaryRecord, PlaylistTrackRecord
from services.playlist_service import PlaylistDetailView, PlaylistSummaryView
from tests.helpers import override_user_auth


def _playlist(id="p-1", name="Test", cover_image_path=None) -> PlaylistRecord:
    return PlaylistRecord(
        id=id, name=name, cover_image_path=cover_image_path,
        created_at="2025-01-01T00:00:00+00:00",
        updated_at="2025-01-01T00:00:00+00:00",
    )


def _summary(id="p-1", name="Test") -> PlaylistSummaryRecord:
    return PlaylistSummaryRecord(
        id=id, name=name, track_count=3, total_duration=600,
        cover_urls=["http://example.com/cover.jpg"],
        cover_image_path=None,
        created_at="2025-01-01T00:00:00+00:00",
        updated_at="2025-01-01T00:00:00+00:00",
    )


def _track(id="t-1", playlist_id="p-1", position=0) -> PlaylistTrackRecord:
    return PlaylistTrackRecord(
        id=id, playlist_id=playlist_id, position=position,
        track_name="Song", artist_name="Artist", album_name="Album",
        album_id=None, artist_id=None, track_source_id=None, cover_url="http://img/1",
        source_type="local", available_sources=None, format=None,
        track_number=1, disc_number=2, duration=180,
        created_at="2025-01-01T00:00:00+00:00",
    )


@pytest.fixture
def mock_playlist_service():
    mock = AsyncMock()
    mock.create_playlist.return_value = _playlist()
    mock.get_playlist.return_value = _playlist()
    mock.get_all_playlists.return_value = [PlaylistSummaryView(record=_summary(), is_owner=True)]
    mock.get_playlist_with_tracks.return_value = PlaylistDetailView(
        record=_playlist(), tracks=[_track()], is_owner=True,
    )
    mock.update_playlist.return_value = _playlist()
    mock.update_playlist_with_detail.return_value = (_playlist(), [_track()])
    mock.delete_playlist.return_value = None
    mock.add_tracks.return_value = [_track()]
    mock.remove_track.return_value = None
    mock.reorder_track.return_value = 2
    mock.update_track_source.return_value = _track()
    mock.get_tracks.return_value = [_track()]
    mock.upload_cover.return_value = "/api/v1/playlists/p-1/cover"
    mock.get_cover_path.return_value = None
    mock.remove_cover.return_value = None
    return mock


@pytest.fixture
def mock_jf_service():
    return AsyncMock()


@pytest.fixture
def mock_local_service():
    return AsyncMock()


@pytest.fixture
def mock_nd_service():
    return AsyncMock()


@pytest.fixture
def client(mock_playlist_service, mock_jf_service, mock_local_service, mock_nd_service):
    app = FastAPI()
    app.include_router(playlists_router)
    app.dependency_overrides[get_playlist_service] = lambda: mock_playlist_service
    app.dependency_overrides[get_jellyfin_library_service] = lambda: mock_jf_service
    app.dependency_overrides[get_local_files_service] = lambda: mock_local_service
    app.dependency_overrides[get_navidrome_library_service] = lambda: mock_nd_service
    override_user_auth(app)
    app.add_exception_handler(ResourceNotFoundError, resource_not_found_handler)
    app.add_exception_handler(ValidationError, validation_error_handler)
    app.add_exception_handler(Exception, general_exception_handler)
    return TestClient(app)


class TestListPlaylists:
    def test_success(self, client):
        resp = client.get("/playlists")
        assert resp.status_code == 200
        data = resp.json()
        assert len(data["playlists"]) == 1
        assert data["playlists"][0]["name"] == "Test"
        assert data["playlists"][0]["track_count"] == 3

    def test_empty(self, client, mock_playlist_service):
        mock_playlist_service.get_all_playlists.return_value = []
        resp = client.get("/playlists")
        assert resp.status_code == 200
        assert len(resp.json()["playlists"]) == 0


class TestCreatePlaylist:
    def test_success(self, client):
        resp = client.post(
            "/playlists",
            content=b'{"name": "My Playlist"}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 201
        data = resp.json()
        assert data["id"] == "p-1"
        assert data["tracks"] == []
        assert "cover_image_path" not in data

    def test_validation_error(self, client, mock_playlist_service):
        mock_playlist_service.create_playlist.side_effect = InvalidPlaylistDataError("empty name")
        resp = client.post(
            "/playlists",
            content=b'{"name": ""}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 400


class TestGetPlaylist:
    def test_success(self, client):
        resp = client.get("/playlists/p-1")
        assert resp.status_code == 200
        data = resp.json()
        assert data["id"] == "p-1"
        assert len(data["tracks"]) == 1
        assert data["track_count"] == 1
        assert data["tracks"][0]["disc_number"] == 2
        assert "cover_image_path" not in data

    def test_not_found(self, client, mock_playlist_service):
        mock_playlist_service.get_playlist_with_tracks.side_effect = PlaylistNotFoundError("nope")
        resp = client.get("/playlists/nonexistent")
        assert resp.status_code == 404


class TestUpdatePlaylist:
    def test_success(self, client):
        resp = client.put(
            "/playlists/p-1",
            content=b'{"name": "Renamed"}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 200
        assert resp.json()["id"] == "p-1"
        assert "cover_image_path" not in resp.json()

    def test_not_found(self, client, mock_playlist_service):
        mock_playlist_service.update_playlist_with_detail.side_effect = PlaylistNotFoundError("nope")
        resp = client.put(
            "/playlists/nonexistent",
            content=b'{"name": "X"}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 404


class TestDeletePlaylist:
    def test_success(self, client):
        resp = client.delete("/playlists/p-1")
        assert resp.status_code == 200
        assert resp.json()["status"] == "ok"

    def test_not_found(self, client, mock_playlist_service):
        mock_playlist_service.delete_playlist.side_effect = PlaylistNotFoundError("nope")
        resp = client.delete("/playlists/nonexistent")
        assert resp.status_code == 404


class TestAddTracks:
    def test_success(self, client):
        body = {
            "tracks": [
                {"track_name": "S", "artist_name": "A", "album_name": "AL", "disc_number": 2},
            ],
        }
        resp = client.post(
            "/playlists/p-1/tracks",
            content=__import__("json").dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 201
        assert len(resp.json()["tracks"]) == 1
        assert resp.json()["tracks"][0]["disc_number"] == 2

    def test_empty_tracks(self, client, mock_playlist_service):
        mock_playlist_service.add_tracks.side_effect = InvalidPlaylistDataError("empty")
        resp = client.post(
            "/playlists/p-1/tracks",
            content=b'{"tracks": []}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 400

    def test_playlist_not_found(self, client, mock_playlist_service):
        mock_playlist_service.add_tracks.side_effect = PlaylistNotFoundError("nope")
        body = {"tracks": [{"track_name": "S", "artist_name": "A", "album_name": "AL"}]}
        resp = client.post(
            "/playlists/nonexistent/tracks",
            content=__import__("json").dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 404


class TestRemoveTrack:
    def test_success(self, client):
        resp = client.delete("/playlists/p-1/tracks/t-1")
        assert resp.status_code == 200
        assert resp.json()["status"] == "ok"

    def test_not_found(self, client, mock_playlist_service):
        mock_playlist_service.remove_track.side_effect = PlaylistNotFoundError("nope")
        resp = client.delete("/playlists/p-1/tracks/nonexistent")
        assert resp.status_code == 404


class TestReorderTrack:
    def test_success(self, client):
        resp = client.patch(
            "/playlists/p-1/tracks/reorder",
            content=b'{"track_id": "t-1", "new_position": 2}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["status"] == "ok"
        assert data["actual_position"] == 2

    def test_invalid_position(self, client, mock_playlist_service):
        mock_playlist_service.reorder_track.side_effect = InvalidPlaylistDataError("neg")
        resp = client.patch(
            "/playlists/p-1/tracks/reorder",
            content=b'{"track_id": "t-1", "new_position": -1}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 400


class TestUpdateTrack:
    def test_success(self, client, mock_playlist_service):
        updated = _track()
        updated.source_type = "youtube"
        mock_playlist_service.update_track_source.return_value = updated
        resp = client.patch(
            "/playlists/p-1/tracks/t-1",
            content=b'{"source_type": "youtube"}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 200
        assert resp.json()["source_type"] == "youtube"

    def test_not_found(self, client, mock_playlist_service):
        mock_playlist_service.update_track_source.side_effect = PlaylistNotFoundError("nope")
        resp = client.patch(
            "/playlists/p-1/tracks/nonexistent",
            content=b'{"source_type": "youtube"}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 404


class TestUploadCover:
    def test_success(self, client):
        resp = client.post(
            "/playlists/p-1/cover",
            files={"cover_image": ("cover.png", b"PNG_DATA", "image/png")},
        )
        assert resp.status_code == 200
        assert resp.json()["cover_url"] == "/api/v1/playlists/p-1/cover"

    def test_invalid_type(self, client, mock_playlist_service):
        mock_playlist_service.upload_cover.side_effect = InvalidPlaylistDataError("bad type")
        resp = client.post(
            "/playlists/p-1/cover",
            files={"cover_image": ("doc.pdf", b"data", "application/pdf")},
        )
        assert resp.status_code == 400


class TestGetCover:
    def test_no_cover(self, client):
        resp = client.get("/playlists/p-1/cover")
        assert resp.status_code == 404

    def test_with_cover(self, client, mock_playlist_service, tmp_path):
        cover_file = tmp_path / "cover.png"
        cover_file.write_bytes(b"\x89PNG\r\n\x1a\n")
        mock_playlist_service.get_cover_path.return_value = cover_file
        resp = client.get("/playlists/p-1/cover")
        assert resp.status_code == 200
        assert resp.headers["content-type"] == "image/png"
        assert "max-age=3600" in resp.headers.get("cache-control", "")


class TestRemoveCover:
    def test_success(self, client):
        resp = client.delete("/playlists/p-1/cover")
        assert resp.status_code == 200
        assert resp.json()["status"] == "ok"


class TestCheckTrackMembership:
    def test_success(self, client, mock_playlist_service):
        mock_playlist_service.check_track_membership.return_value = {
            "p-1": [0, 1],
            "p-2": [1],
        }
        resp = client.post(
            "/playlists/check-tracks",
            json={
                "tracks": [
                    {"track_name": "Song A", "artist_name": "Artist", "album_name": "Album"},
                    {"track_name": "Song B", "artist_name": "Artist2", "album_name": "Album2"},
                ]
            },
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["membership"]["p-1"] == [0, 1]
        assert data["membership"]["p-2"] == [1]

    def test_empty_tracks(self, client, mock_playlist_service):
        mock_playlist_service.check_track_membership.return_value = {}
        resp = client.post(
            "/playlists/check-tracks",
            json={"tracks": []},
        )
        assert resp.status_code == 200
        assert resp.json()["membership"] == {}


class TestResolveSources:
    def test_success(self, client, mock_playlist_service):
        mock_playlist_service.resolve_track_sources.return_value = {
            "t-1": ["jellyfin", "local"],
            "t-2": ["jellyfin"],
        }
        resp = client.post("/playlists/p-1/resolve-sources")
        assert resp.status_code == 200
        data = resp.json()
        assert data["sources"]["t-1"] == ["jellyfin", "local"]
        assert data["sources"]["t-2"] == ["jellyfin"]

    def test_empty_sources(self, client, mock_playlist_service):
        mock_playlist_service.resolve_track_sources.return_value = {}
        resp = client.post("/playlists/p-1/resolve-sources")
        assert resp.status_code == 200
        assert resp.json()["sources"] == {}

    def test_not_found(self, client, mock_playlist_service):
        mock_playlist_service.resolve_track_sources.side_effect = PlaylistNotFoundError("nope")
        resp = client.post("/playlists/p-1/resolve-sources")
        assert resp.status_code == 404


class TestUpdateTrackSourceResolution:
    def test_returns_updated_track_source_id(self, client, mock_playlist_service):
        updated = _track()
        updated.source_type = "jellyfin"
        updated.track_source_id = "jf-resolved-id"
        updated.available_sources = ["jellyfin", "local"]
        mock_playlist_service.update_track_source.return_value = updated
        resp = client.patch(
            "/playlists/p-1/tracks/t-1",
            content=b'{"source_type": "jellyfin"}',
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["source_type"] == "jellyfin"
        assert data["track_source_id"] == "jf-resolved-id"
        assert data["available_sources"] == ["jellyfin", "local"]


class TestRequestMissingTracks:
    def _override_requests(self, client, request_service):
        client.app.dependency_overrides[get_request_service] = lambda: request_service

    def test_skips_tracks_with_library_file_id(self, client, mock_playlist_service):
        request_service = AsyncMock()
        self._override_requests(client, request_service)
        try:
            owned = _track(id="t-1")
            owned.album_id = "mbid-owned"
            owned.available_sources = []
            owned.library_file_id = "file-1"
            mock_playlist_service.get_playlist_with_tracks.return_value = (
                PlaylistDetailView(record=_playlist(), tracks=[owned], is_owner=True)
            )
            resp = client.post("/playlists/p-1/request-missing")
            assert resp.status_code == 202
            assert resp.json()["message"].startswith("No missing albums")
            request_service.request_batch.assert_not_called()
        finally:
            del client.app.dependency_overrides[get_request_service]

    def test_requests_truly_missing_album(self, client, mock_playlist_service):
        request_service = AsyncMock()
        request_service.request_batch.return_value = BatchRequestResponse(
            success=True, message="requested", requested=1,
        )
        self._override_requests(client, request_service)
        try:
            missing = _track(id="t-1")
            missing.album_id = "mbid-missing"
            missing.available_sources = []
            mock_playlist_service.get_playlist_with_tracks.return_value = (
                PlaylistDetailView(record=_playlist(), tracks=[missing], is_owner=True)
            )
            resp = client.post("/playlists/p-1/request-missing")
            assert resp.status_code == 202
            request_service.request_batch.assert_called_once()
            items = request_service.request_batch.call_args[1]["items"]
            assert [i["musicbrainz_id"] for i in items] == ["mbid-missing"]
        finally:
            del client.app.dependency_overrides[get_request_service]
