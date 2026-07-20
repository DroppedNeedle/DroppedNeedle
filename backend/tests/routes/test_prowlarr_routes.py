"""Prowlarr route tests: admin auth, masked key on GET, save clears the provider
chain, Test reports version + per-protocol indexer counts."""

from unittest.mock import MagicMock

from fastapi import FastAPI, HTTPException

from api.v1.routes import prowlarr
from api.v1.schemas.settings import PROWLARR_API_KEY_MASK, ProwlarrConnectionSettings
from core.dependencies import get_preferences_service
from middleware import _get_current_admin
from tests.helpers import build_test_client, mock_admin_user


def _prefs():
    prefs = MagicMock()
    prefs.get_prowlarr_connection.return_value = ProwlarrConnectionSettings(
        enabled=True, url="http://prowlarr:9696", api_key=PROWLARR_API_KEY_MASK
    )
    prefs.get_prowlarr_connection_raw.return_value = ProwlarrConnectionSettings(
        enabled=True, url="http://prowlarr:9696", api_key="real-key"
    )
    prefs.save_prowlarr_connection.return_value = None
    return prefs


def _app(prefs=None) -> FastAPI:
    app = FastAPI()
    app.include_router(prowlarr.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs or _prefs()
    return app


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def test_get_prowlarr_admin_masked():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).get("/prowlarr")
    assert resp.status_code == 200
    assert resp.json()["api_key"] == PROWLARR_API_KEY_MASK


def test_get_prowlarr_non_admin_forbidden():
    app = _app()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    assert build_test_client(app).get("/prowlarr").status_code == 403


def test_put_prowlarr_saves_and_clears_providers(monkeypatch):
    from core.dependencies import get_prowlarr_indexer

    spy = MagicMock()
    monkeypatch.setattr(get_prowlarr_indexer, "cache_clear", spy)
    prefs = _prefs()
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).put(
        "/prowlarr",
        json={"enabled": True, "url": "http://prowlarr:9696", "api_key": "new-key"},
    )
    assert resp.status_code == 200
    prefs.save_prowlarr_connection.assert_called_once()
    spy.assert_called_once()


def test_test_prowlarr_reports_indexer_counts(monkeypatch):
    from unittest.mock import AsyncMock

    from repositories.prowlarr.prowlarr_models import (
        ProwlarrIndexerInfo,
        ProwlarrSystemStatus,
    )

    client = AsyncMock()
    client.system_status.return_value = ProwlarrSystemStatus(version="2.1.0")
    client.indexers.return_value = [
        ProwlarrIndexerInfo(id=1, name="geek", enable=True, protocol="usenet"),
        ProwlarrIndexerInfo(id=2, name="red", enable=True, protocol="torrent"),
        ProwlarrIndexerInfo(id=3, name="off", enable=False, protocol="torrent"),
    ]
    monkeypatch.setattr(prowlarr, "build_prowlarr_client", lambda url, key: client)

    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    resp = build_test_client(app).post(
        "/prowlarr/test",
        json={"enabled": True, "url": "http://prowlarr:9696", "api_key": "k"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["valid"] is True
    assert body["version"] == "2.1.0"
    assert body["indexers_total"] == 2
    assert body["indexers_usenet"] == 1
    assert body["indexers_torrent"] == 1
