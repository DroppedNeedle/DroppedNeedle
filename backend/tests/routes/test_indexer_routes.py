"""Newznab indexer route tests: admin auth, list/create/update/delete/reorder,
masked-key passthrough, and the caps Test (audio-search reporting)."""

from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI, HTTPException

from api.v1.routes import indexers
from api.v1.schemas.settings import INDEXER_API_KEY_MASK, NewznabIndexerSettings
from core.dependencies import get_preferences_service
from middleware import _get_current_admin
from tests.helpers import build_test_client, mock_admin_user


def _prefs():
    prefs = MagicMock()
    prefs.get_indexers.return_value = [
        NewznabIndexerSettings(
            id="idx1", name="My Indexer", url="https://idx.test/api",
            api_key=INDEXER_API_KEY_MASK, priority=1,
        )
    ]
    prefs.save_indexer.return_value = "idx1"
    prefs.delete_indexer.return_value = None
    prefs.reorder_indexers.return_value = None
    prefs.get_indexers_raw.return_value = []
    return prefs


def _app(prefs=None) -> FastAPI:
    app = FastAPI()
    app.include_router(indexers.router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs or _prefs()
    return app


def _deny_admin():
    raise HTTPException(status_code=403, detail="admin only")


def test_list_indexers_admin_returns_masked_key():
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).get("/indexers")
    assert response.status_code == 200
    body = response.json()
    assert body[0]["api_key"] == INDEXER_API_KEY_MASK
    assert body[0]["name"] == "My Indexer"


def test_list_indexers_non_admin_forbidden():
    app = _app()
    app.dependency_overrides[_get_current_admin] = _deny_admin
    assert build_test_client(app).get("/indexers").status_code == 403


def test_list_indexers_unauthenticated():
    assert build_test_client(_app()).get("/indexers").status_code == 401


def test_create_indexer_saves_and_returns_id():
    prefs = _prefs()
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/indexers",
        json={"name": "New", "url": "https://new.test/api", "api_key": "secret"},
    )
    assert response.status_code == 200
    assert response.json()["id"] == "idx1"
    prefs.save_indexer.assert_called_once()


def test_delete_indexer_admin():
    prefs = _prefs()
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).delete("/indexers/idx1")
    assert response.status_code == 200
    assert response.json()["success"] is True
    prefs.delete_indexer.assert_called_once_with("idx1")


def test_reorder_indexers_persists_order():
    prefs = _prefs()
    app = _app(prefs)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/indexers/reorder", json={"ordered_ids": ["idx2", "idx1"]}
    )
    assert response.status_code == 200
    prefs.reorder_indexers.assert_called_once_with(["idx2", "idx1"])


def test_test_indexer_reports_audio_search_capability(monkeypatch):
    from repositories.newznab.newznab_client import NewznabClient
    from tests.mocks import newznab_mock

    # A real NewznabClient over the DrunkenSlug mock (audio-search=no); the route
    # calls build_newznab_client() directly, so patch it on the route module.
    caps_client = NewznabClient(
        newznab_mock.client_for(newznab_mock.drunkenslug_handler), "https://idx.test/api", "KEY"
    )
    monkeypatch.setattr(indexers, "build_newznab_client", lambda url, api_key: caps_client)
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/indexers/test", json={"url": "https://idx.test/api", "api_key": "KEY"}
    )
    assert response.status_code == 200
    body = response.json()
    assert body["valid"] is True
    assert body["supports_audio_search"] is False  # DrunkenSlug -> text search
    assert body["category_count"] >= 1


def test_test_indexer_reports_failure_on_bad_caps(monkeypatch):
    from repositories.newznab.newznab_client import NewznabClient
    from tests.mocks import newznab_mock

    bad = NewznabClient(
        newznab_mock.client_for(newznab_mock.auth_error_handler), "https://idx.test/api", "BAD"
    )
    monkeypatch.setattr(indexers, "build_newznab_client", lambda url, api_key: bad)
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/indexers/test", json={"url": "https://idx.test/api", "api_key": "BAD"}
    )
    assert response.status_code == 200
    assert response.json()["valid"] is False


def test_test_indexer_suggests_api_path_when_site_url_pasted(monkeypatch):
    """The user pastes the site URL (no /api) -> caps hits an HTML page and fails, but
    /api answers as a real newznab endpoint, so we return a 'did you mean' suggestion."""
    import httpx

    from repositories.newznab.newznab_client import NewznabClient
    from tests.mocks import newznab_mock

    def homepage_handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"<!doctype html><head><title>Site</title></head>",
                              headers={"Content-Type": "text/html"})

    def _build(url, api_key):
        handler = newznab_mock.drunkenslug_handler if url.endswith("/api") else homepage_handler
        return NewznabClient(newznab_mock.client_for(handler), url, api_key)

    monkeypatch.setattr(indexers, "build_newznab_client", _build)
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/indexers/test", json={"url": "https://idx.test", "api_key": "KEY"}
    )
    assert response.status_code == 200
    body = response.json()
    assert body["valid"] is False
    assert body["suggested_url"] == "https://idx.test/api"


def test_test_indexer_suggests_api_path_even_when_key_is_wrong(monkeypatch):
    """Site URL pasted AND a bad key: /api answers with an auth error, which still
    proves the endpoint - so we suggest the URL fix (the key error comes next)."""
    import httpx

    from repositories.newznab.newznab_client import NewznabClient
    from tests.mocks import newznab_mock

    def homepage_handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"<!doctype html><head></head>",
                              headers={"Content-Type": "text/html"})

    def _build(url, api_key):
        handler = newznab_mock.auth_error_handler if url.endswith("/api") else homepage_handler
        return NewznabClient(newznab_mock.client_for(handler), url, api_key)

    monkeypatch.setattr(indexers, "build_newznab_client", _build)
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/indexers/test", json={"url": "https://idx.test", "api_key": "WRONG"}
    )
    assert response.status_code == 200
    assert response.json()["suggested_url"] == "https://idx.test/api"


def test_test_indexer_no_suggestion_when_url_already_has_path(monkeypatch):
    """A URL that already carries a path gets no /api suggestion - we don't guess."""
    import httpx

    from repositories.newznab.newznab_client import NewznabClient
    from tests.mocks import newznab_mock

    def homepage_handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"<!doctype html><head></head>",
                              headers={"Content-Type": "text/html"})

    monkeypatch.setattr(
        indexers, "build_newznab_client",
        lambda url, api_key: NewznabClient(newznab_mock.client_for(homepage_handler), url, api_key),
    )
    app = _app()
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    response = build_test_client(app).post(
        "/indexers/test", json={"url": "https://idx.test/newznab", "api_key": "KEY"}
    )
    assert response.status_code == 200
    body = response.json()
    assert body["valid"] is False
    assert body["suggested_url"] is None
