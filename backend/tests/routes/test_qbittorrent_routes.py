"""qBittorrent route tests: admin auth, masked API key on GET, save clears the
provider chain, Test reports the version."""

from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI, HTTPException

from api.v1.routes import qbittorrent
from api.v1.schemas.settings import (
    QBITTORRENT_API_KEY_MASK,
    QbittorrentConnectionSettings,
)
from core.dependencies import get_preferences_service
from middleware import _get_current_admin
from models.common import ServiceStatus
from tests.helpers import build_test_client, mock_admin_user


def _prefs():
    prefs = MagicMock()
    prefs.get_qbittorrent_connection.return_value = QbittorrentConnectionSettings(
        enabled=True, url="http://qbt:8080", api_key=QBITTORRENT_API_KEY_MASK,
    )
    prefs.get_qbittorrent_connection_raw.return_value = QbittorrentConnectionSettings(
        enabled=True, url="http://qbt:8080", api_key="real-key"
    )
    prefs.save_qbittorrent_connection.return_value = None
    return prefs


def _app(prefs=None) -> FastAPI:
    app = FastAPI()
    app.include_router(qbittorrent.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs or _prefs()
    return app


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def test_get_qbittorrent_admin_masked():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).get("/download-clients/qbittorrent")
    assert resp.status_code == 200
    assert resp.json()["api_key"] == QBITTORRENT_API_KEY_MASK


def test_get_qbittorrent_non_admin_forbidden():
    app = _app()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    resp = build_test_client(app).get("/download-clients/qbittorrent")
    assert resp.status_code == 403


def test_put_qbittorrent_saves_and_clears_providers(monkeypatch):
    from core.dependencies import get_qbittorrent_download_client

    spy = MagicMock()
    monkeypatch.setattr(get_qbittorrent_download_client, "cache_clear", spy)
    prefs = _prefs()
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).put(
        "/download-clients/qbittorrent",
        json={
            "enabled": True, "url": "http://qbt:8080",
            "api_key": "new-key",
        },
    )
    assert resp.status_code == 200
    prefs.save_qbittorrent_connection.assert_called_once()
    spy.assert_called_once()


def test_test_qbittorrent_reports_version(monkeypatch):
    client = AsyncMock()
    client.health_check.return_value = ServiceStatus(
        status="ok", version="5.2.1", message="qBittorrent 5.2.1"
    )
    monkeypatch.setattr(
        qbittorrent, "build_qbittorrent_download_client", lambda url, key: client
    )

    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).post(
        "/download-clients/qbittorrent/test",
        json={"enabled": True, "url": "http://qbt:8080", "api_key": "key"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is True
    assert body["version"] == "5.2.1"


def test_test_qbittorrent_unreachable_reports_invalid(monkeypatch):
    client = AsyncMock()
    client.health_check.return_value = ServiceStatus(status="error", message="conn refused")
    monkeypatch.setattr(
        qbittorrent, "build_qbittorrent_download_client", lambda url, key: client
    )
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).post(
        "/download-clients/qbittorrent/test",
        json={"enabled": True, "url": "http://qbt:8080", "api_key": "key"},
    )
    assert resp.json()["valid"] is False


def test_test_qbittorrent_missing_key_reports_invalid(monkeypatch):
    build = MagicMock()
    monkeypatch.setattr(qbittorrent, "build_qbittorrent_download_client", build)
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).post(
        "/download-clients/qbittorrent/test",
        json={"enabled": True, "url": "http://qbt:8080", "api_key": ""},
    )
    assert resp.json()["valid"] is False
    assert "required" in resp.json()["message"]
    build.assert_not_called()
