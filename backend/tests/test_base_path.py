"""BASE_PATH / reverse-proxy subpath support: config normalization + base-aware redirects.

The subpath is carried by FastAPI's ``root_path`` (set from ``base_path``); browser-facing
redirects to SPA routes are generated via ``request.url_for`` so they inherit the prefix.
"""

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from unittest.mock import AsyncMock, MagicMock

from core.config import Settings


class TestBasePathNormalization:
    @pytest.mark.parametrize(
        "raw, expected",
        [
            ("", ""),
            ("/droppedneedle", "/droppedneedle"),
            ("droppedneedle", "/droppedneedle"),
            ("/dn/", "/dn"),
            ("/a/b/", "/a/b"),
            ("   ", ""),
            ("/x///", "/x"),
        ],
    )
    def test_normalizes(self, raw: str, expected: str) -> None:
        assert Settings(base_path=raw).base_path == expected

    def test_default_is_root(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("BASE_PATH", raising=False)
        assert Settings().base_path == ""


def _spa_app(base_path: str) -> FastAPI:
    """Minimal app exercising the base-aware redirect handlers.

    Includes the SPA catch-all so ``url_for('serve_spa_routes', ...)`` resolves, and
    overrides the callbacks' dependencies (unused on the redirect paths under test).
    """
    from api.v1.routes.auth import router as auth_router
    from api.v1.routes.me_connections import router as me_router
    from core.dependencies import (
        get_auth_store,
        get_preferences_service,
        get_user_connections_store,
    )
    from core.dependencies.auth_providers import get_oidc_user_auth_service

    app = FastAPI(root_path=base_path)
    app.include_router(auth_router)
    app.include_router(me_router)

    @app.get("/{full_path:path}")
    async def serve_spa_routes(full_path: str):
        return {}

    auth_store = AsyncMock()
    auth_store.consume_spotify_state = AsyncMock(return_value=None)
    oidc = AsyncMock()
    oidc.handle_callback = AsyncMock(return_value="exch-123")

    app.dependency_overrides[get_auth_store] = lambda: auth_store
    app.dependency_overrides[get_user_connections_store] = lambda: AsyncMock()
    app.dependency_overrides[get_preferences_service] = lambda: MagicMock()
    app.dependency_overrides[get_oidc_user_auth_service] = lambda: oidc
    return app


@pytest.mark.parametrize("base_path", ["", "/droppedneedle"])
def test_spotify_callback_redirect_carries_base_path(base_path: str) -> None:
    client = TestClient(_spa_app(base_path), raise_server_exceptions=False)
    resp = client.get(
        "/me/connections/spotify/auth/callback?error=denied", follow_redirects=False
    )
    assert resp.status_code == 307
    assert resp.headers["location"] == f"http://testserver{base_path}/profile?spotify=error"


@pytest.mark.parametrize("base_path", ["", "/droppedneedle"])
def test_spotify_callback_invalid_state_redirect_carries_base_path(base_path: str) -> None:
    client = TestClient(_spa_app(base_path), raise_server_exceptions=False)
    resp = client.get(
        "/me/connections/spotify/auth/callback?code=c&state=s", follow_redirects=False
    )
    assert resp.status_code == 307
    location = resp.headers["location"]
    assert location.startswith(f"http://testserver{base_path}/profile")
    assert "spotify=error" in location
    assert "reason=state" in location


@pytest.mark.parametrize("base_path", ["", "/droppedneedle"])
def test_oidc_callback_redirect_carries_base_path(base_path: str) -> None:
    client = TestClient(_spa_app(base_path), raise_server_exceptions=False)
    resp = client.get("/auth/oidc/callback?code=c&state=s", follow_redirects=False)
    assert resp.status_code == 307
    assert (
        resp.headers["location"]
        == f"http://testserver{base_path}/auth/callback?code=exch-123"
    )
