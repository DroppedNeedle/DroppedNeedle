"""Navidrome playlist-sync route: disabled short-circuits, enabled delegates
to the export service with saved settings and maps the result fields."""

from unittest.mock import AsyncMock, Mock

from fastapi import FastAPI

from api.v1.routes import settings as settings_routes
from api.v1.schemas.settings import NavidromeConnectionSettings
from core.dependencies import (
    get_navidrome_playlist_export_service,
    get_preferences_service,
)
from middleware import _get_current_admin
from services.navidrome_playlist_export_service import PlaylistSyncResult
from tests.helpers import build_test_client, mock_admin_user


def _client(prefs, export_service=None):
    app = FastAPI()
    app.include_router(settings_routes.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs
    if export_service is not None:
        app.dependency_overrides[get_navidrome_playlist_export_service] = lambda: (
            export_service
        )
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    return build_test_client(app)


def _prefs(**overrides):
    prefs = Mock()
    values = {
        "navidrome_url": "http://navidrome:4533",
        "username": "u",
        "password": "••••••••",
        "enabled": True,
        "playlist_sync_enabled": False,
        "playlist_sync_path": "",
        "playlist_sync_scope": "public",
        "playlist_sync_remove_deleted": True,
    }
    values.update(overrides)
    prefs.get_navidrome_connection.return_value = NavidromeConnectionSettings(**values)
    return prefs


def test_sync_requires_admin():
    app = FastAPI()
    app.include_router(settings_routes.router)
    client = build_test_client(app)
    assert client.post("/settings/navidrome/playlist-sync").status_code == 401


def test_sync_disabled_returns_message_without_touching_exporter():
    export_service = AsyncMock()
    client = _client(_prefs(), export_service)

    response = client.post("/settings/navidrome/playlist-sync")

    assert response.status_code == 200
    body = response.json()
    assert body["success"] is False
    assert "turned off" in body["message"]
    export_service.sync.assert_not_awaited()


def test_sync_enabled_delegates_with_saved_settings():
    export_service = AsyncMock()
    export_service.sync.return_value = PlaylistSyncResult(
        success=True,
        message="2 written.",
        written=2,
    )
    prefs = _prefs(playlist_sync_enabled=True, playlist_sync_path="/music/playlists")
    client = _client(prefs, export_service)

    response = client.post("/settings/navidrome/playlist-sync")

    assert response.status_code == 200
    export_service.sync.assert_awaited_once_with(
        target_dir="/music/playlists",
        scope="public",
        remove_deleted=True,
    )
    body = response.json()
    assert body["success"] is True
    assert body["written"] == 2
    assert body["message"] == "2 written."
