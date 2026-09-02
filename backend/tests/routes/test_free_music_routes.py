"""Free Music routes: listing scope, ownership, and the curator gate on
cancel/retry. Also the settings roundtrip and the readiness widening."""

from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import APIRouter, FastAPI
from fastapi.testclient import TestClient

from api.v1.routes import free_music as free_music_routes
from api.v1.routes.settings import router as settings_router
from api.v1.schemas.settings import FreeMusicSettings
from core.dependencies import get_free_music_service, get_preferences_service
from middleware import _get_current_curator, _get_current_user
from models.free_music import FreeMusicTask
from tests.helpers import build_test_client, mock_user


def _task(task_id: str = "t1", user_id: str = "test-user-id") -> FreeMusicTask:
    return FreeMusicTask(
        id=task_id,
        user_id=user_id,
        kind="album",
        mbid="d0484284-1ee7-4157-951a-50f003cbcfb4",
        artist="Brad Sucks",
        title="Guess Who's a Mess",
        status="downloading",
        created_at=1.0,
        updated_at=2.0,
        identifier="jamendo-117853",
        licence_url="http://creativecommons.org/licenses/by-nc-sa/3.0/",
        files_total=10,
        files_completed=3,
        quality_snapshot_hash="snapshot-hash",
        quality_snapshot_summary="Try FLAC, then MP3 320 kbps.",
    )


def _client(role: str = "user"):
    service = AsyncMock()
    service.list_tasks = AsyncMock(return_value=[_task()])
    service.get_task = AsyncMock(return_value=_task())
    service.cancel = AsyncMock(return_value=_task())
    service.retry = AsyncMock(return_value=_task())
    service.remove = AsyncMock(return_value=None)
    service.clear_history = AsyncMock(return_value=2)

    app = FastAPI()
    v1 = APIRouter(prefix="/api/v1")
    v1.include_router(free_music_routes.router)
    app.include_router(v1)
    app.dependency_overrides[get_free_music_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role=role)
    app.dependency_overrides[_get_current_curator] = lambda: mock_user(role="trusted")
    return build_test_client(app), service


def test_list_returns_the_callers_tasks():
    client, service = _client()
    response = client.get("/api/v1/free-music/tasks")
    assert response.status_code == 200
    body = response.json()["tasks"][0]
    assert body["identifier"] == "jamendo-117853"
    assert body["licence_url"].startswith("http://creativecommons.org/")
    assert body["quality_snapshot_hash"] == "snapshot-hash"
    assert body["quality_snapshot_summary"] == "Try FLAC, then MP3 320 kbps."
    assert "quality_snapshot_json" not in body
    assert service.list_tasks.await_args.kwargs["include_all"] is False


def test_quality_snapshot_context_is_safe_and_available_on_detail():
    client, _service = _client()

    response = client.get("/api/v1/free-music/tasks/t1")

    assert response.status_code == 200
    body = response.json()
    assert body["quality_snapshot_hash"] == "snapshot-hash"
    assert body["quality_snapshot_summary"] == "Try FLAC, then MP3 320 kbps."
    assert "quality_snapshot_json" not in body


def test_all_is_admin_only():
    client, service = _client(role="user")
    client.get("/api/v1/free-music/tasks?all=true")
    assert service.list_tasks.await_args.kwargs["include_all"] is False

    admin_client, admin_service = _client(role="admin")
    admin_client.get("/api/v1/free-music/tasks?all=true")
    assert admin_service.list_tasks.await_args.kwargs["include_all"] is True


def test_get_forwards_ownership():
    client, service = _client()
    assert client.get("/api/v1/free-music/tasks/t1").status_code == 200
    assert service.get_task.await_args.kwargs["is_admin"] is False


def test_cancel_and_retry_reach_the_service():
    client, service = _client()
    assert client.post("/api/v1/free-music/tasks/t1/cancel").status_code == 200
    service.cancel.assert_awaited_once()
    assert client.post("/api/v1/free-music/tasks/t1/retry").status_code == 200
    service.retry.assert_awaited_once()


def test_remove_forwards_ownership_and_returns_count():
    client, service = _client()

    response = client.delete("/api/v1/free-music/tasks/t1")

    assert response.status_code == 200
    assert response.json() == {"cleared": 1}
    service.remove.assert_awaited_once_with(
        "t1", user_id="test-user-id", is_admin=False
    )


def test_clear_history_is_user_scoped_unless_an_admin_requests_all():
    client, service = _client(role="user")
    response = client.delete("/api/v1/free-music/tasks?all=true")

    assert response.json() == {"cleared": 2}
    service.clear_history.assert_awaited_once_with(
        user_id="test-user-id", include_all=False
    )

    admin_client, admin_service = _client(role="admin")
    admin_client.delete("/api/v1/free-music/tasks?all=true")
    admin_service.clear_history.assert_awaited_once_with(
        user_id="test-user-id", include_all=True
    )


def test_plain_user_is_forbidden_from_cancel():
    """The curator dependency itself rejects a plain user."""
    from types import SimpleNamespace

    request = SimpleNamespace(state=SimpleNamespace(user=mock_user(role="user")))
    with pytest.raises(Exception) as excinfo:
        _get_current_curator(request)
    assert getattr(excinfo.value, "status_code", None) == 403


# -- settings + readiness --


@pytest.fixture
def settings_client():
    prefs = MagicMock()
    prefs.get_free_music_settings.return_value = FreeMusicSettings()
    prefs.save_free_music_settings = MagicMock()
    app = FastAPI()
    app.include_router(settings_router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs
    from tests.helpers import override_admin_auth

    override_admin_auth(app)
    return TestClient(app), prefs


def test_free_music_settings_roundtrip(settings_client):
    client, prefs = settings_client

    response = client.get("/settings/free-music")
    assert response.status_code == 200
    assert response.json() == {"enabled": True, "preferred_format": "flac"}

    response = client.put(
        "/settings/free-music", json={"enabled": False, "preferred_format": "mp3"}
    )
    assert response.status_code == 200
    saved = prefs.save_free_music_settings.call_args.args[0]
    assert saved.enabled is False and saved.preferred_format == "mp3"


def test_free_music_settings_reject_an_unknown_format(settings_client):
    client, _ = settings_client
    response = client.put(
        "/settings/free-music", json={"enabled": True, "preferred_format": "wav"}
    )
    assert response.status_code == 422
