import pytest
from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI
from fastapi.testclient import TestClient

from api.v1.schemas.discover import DiscoverQueueResponse, DiscoverQueueItemLight
from api.v1.routes.discover import router
from core.dependencies import get_discover_service, get_discover_queue_manager
from tests.helpers import override_user_auth

_UID = "test-user-id"


def _make_queue_response() -> DiscoverQueueResponse:
    return DiscoverQueueResponse(
        items=[
            DiscoverQueueItemLight(
                release_group_mbid="rg-mbid-1",
                album_name="Test Album",
                artist_name="Test Artist",
                artist_mbid="artist-mbid-1",
                cover_url="/covers/release-group/rg-mbid-1?size=500",
                recommendation_reason="Similar to someone",
                in_library=False,
            )
        ],
        queue_id="test-queue-id",
    )


@pytest.fixture
def mock_discover_service():
    mock = AsyncMock()
    mock.build_queue = AsyncMock(return_value=_make_queue_response())
    return mock


@pytest.fixture
def mock_queue_manager():
    mock = AsyncMock()
    mock.consume_queue = AsyncMock(return_value=None)
    mock.build_hydrated_queue = AsyncMock(return_value=_make_queue_response())
    return mock


@pytest.fixture
def client(mock_discover_service, mock_queue_manager):
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_discover_service] = lambda: mock_discover_service
    app.dependency_overrides[get_discover_queue_manager] = lambda: mock_queue_manager
    override_user_auth(app, user_id=_UID)
    return TestClient(app)


class TestDiscoverQueueRoute:
    def test_queue_builds_for_current_user(self, client, mock_queue_manager):
        resp = client.get("/discover/queue")
        assert resp.status_code == 200
        mock_queue_manager.build_hydrated_queue.assert_awaited_once_with(_UID, None)

    def test_queue_consumes_prebuilt_queue_first(self, client, mock_queue_manager):
        mock_queue_manager.consume_queue = AsyncMock(return_value=_make_queue_response())
        resp = client.get("/discover/queue")
        assert resp.status_code == 200
        mock_queue_manager.consume_queue.assert_awaited_once_with(_UID)
        mock_queue_manager.build_hydrated_queue.assert_not_awaited()

    def test_queue_respects_count_param(self, client, mock_queue_manager):
        resp = client.get("/discover/queue?count=5")
        assert resp.status_code == 200
        mock_queue_manager.build_hydrated_queue.assert_awaited_once_with(_UID, 5)

    def test_queue_caps_count_at_20(self, client, mock_queue_manager):
        resp = client.get("/discover/queue?count=50")
        assert resp.status_code == 200
        mock_queue_manager.build_hydrated_queue.assert_awaited_once_with(_UID, 20)

    def test_queue_returns_items(self, client):
        resp = client.get("/discover/queue")
        assert resp.status_code == 200
        data = resp.json()
        assert "items" in data
        assert "queue_id" in data
        assert len(data["items"]) == 1
        assert data["items"][0]["artist_name"] == "Test Artist"


class TestQueueStatusRoute:
    def test_status_returns_ok(self, client, mock_queue_manager):
        mock_queue_manager.get_status = MagicMock(return_value={"status": "idle"})
        resp = client.get("/discover/queue/status")
        assert resp.status_code == 200
        data = resp.json()
        assert data["status"] == "idle"
        mock_queue_manager.get_status.assert_called_once_with(_UID)

    def test_status_ready_includes_queue_info(self, client, mock_queue_manager):
        mock_queue_manager.get_status = MagicMock(
            return_value={
                "status": "ready",
                "queue_id": "abc",
                "item_count": 5,
                "built_at": 1000.0,
                "stale": False,
            }
        )
        resp = client.get("/discover/queue/status")
        assert resp.status_code == 200
        data = resp.json()
        assert data["status"] == "ready"
        assert data["item_count"] == 5
        assert data["stale"] is False


class TestQueueGenerateRoute:
    def test_generate_triggers_build(self, client, mock_queue_manager):
        mock_queue_manager.start_build = AsyncMock(
            return_value={"action": "started", "status": "building"}
        )
        resp = client.post("/discover/queue/generate", json={"force": False})
        assert resp.status_code == 200
        data = resp.json()
        assert data["action"] == "started"
        mock_queue_manager.start_build.assert_awaited_once_with(_UID, force=False)

    def test_generate_already_building(self, client, mock_queue_manager):
        mock_queue_manager.start_build = AsyncMock(
            return_value={"action": "already_building", "status": "building"}
        )
        resp = client.post("/discover/queue/generate", json={"force": False})
        assert resp.status_code == 200
        assert resp.json()["action"] == "already_building"

    def test_generate_force_rebuild(self, client, mock_queue_manager):
        mock_queue_manager.start_build = AsyncMock(
            return_value={"action": "started", "status": "building"}
        )
        resp = client.post("/discover/queue/generate", json={"force": True})
        assert resp.status_code == 200
        mock_queue_manager.start_build.assert_awaited_once_with(_UID, force=True)


class TestQueueIgnoreRoute:
    def test_ignore_refreshes_recommendations_and_rebuilds_queue(
        self, client, mock_discover_service, mock_queue_manager
    ):
        resp = client.post(
            "/discover/queue/ignore",
            json={
                "release_group_mbid": "rg-1",
                "artist_mbid": "artist-1",
                "release_name": "Album",
                "artist_name": "Artist",
            },
        )

        assert resp.status_code == 204
        mock_discover_service.ignore_release.assert_awaited_once_with(
            _UID, "rg-1", "artist-1", "Album", "Artist"
        )
        mock_discover_service.refresh_discover_data.assert_awaited_once_with(_UID)
        mock_queue_manager.start_build.assert_awaited_once_with(_UID, force=True)
