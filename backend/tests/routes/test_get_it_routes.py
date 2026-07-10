"""Get it (phase 01) routes: the album purchase-options endpoint and the
admin settings card roundtrip."""

from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import APIRouter, FastAPI
from fastapi.testclient import TestClient

from api.v1.routes import albums as albums_routes
from api.v1.routes.settings import router as settings_router
from api.v1.schemas.get_it import PurchaseLink, PurchaseOptionsResponse
from api.v1.schemas.settings import GetItSettings
from core.dependencies import get_get_it_service, get_preferences_service
from tests.helpers import build_test_client, override_admin_auth


@pytest.fixture
def get_it_service():
    service = AsyncMock()
    service.get_purchase_options = AsyncMock(
        return_value=PurchaseOptionsResponse(
            digital=[
                PurchaseLink(
                    store="bandcamp",
                    label="Bandcamp",
                    url="https://x.bandcamp.com/album/y",
                    kind="digital",
                )
            ],
            bandcamp_search_url="https://bandcamp.com/search?q=x&item_type=a",
        )
    )
    return service


@pytest.fixture
def albums_client(get_it_service):
    app = FastAPI()
    v1 = APIRouter(prefix="/api/v1")
    v1.include_router(albums_routes.router)
    app.include_router(v1)
    app.dependency_overrides[get_get_it_service] = lambda: get_it_service
    return build_test_client(app)


def test_purchase_options_returns_links(albums_client, get_it_service):
    response = albums_client.get(
        "/api/v1/albums/11111111-1111-1111-1111-111111111111/purchase-options"
    )
    assert response.status_code == 200
    body = response.json()
    assert body["digital"][0]["store"] == "bandcamp"
    assert body["bandcamp_search_url"].startswith("https://bandcamp.com/search")
    get_it_service.get_purchase_options.assert_awaited_once_with(
        "11111111-1111-1111-1111-111111111111"
    )


def test_purchase_options_rejects_unknown_mbid_placeholder(albums_client, get_it_service):
    response = albums_client.get("/api/v1/albums/unknown_123/purchase-options")
    assert response.status_code == 400
    get_it_service.get_purchase_options.assert_not_awaited()


@pytest.fixture
def settings_client():
    prefs = MagicMock()
    prefs.get_get_it_settings.return_value = GetItSettings(
        store_region="GB", support_droppedneedle=True
    )
    prefs.save_get_it_settings = MagicMock()
    app = FastAPI()
    app.include_router(settings_router)
    app.dependency_overrides[get_preferences_service] = lambda: prefs
    override_admin_auth(app)
    return TestClient(app), prefs


def test_get_it_settings_roundtrip(settings_client):
    client, prefs = settings_client

    response = client.get("/settings/get-it")
    assert response.status_code == 200
    assert response.json() == {"store_region": "GB", "support_droppedneedle": True}

    response = client.put(
        "/settings/get-it",
        json={"store_region": "DE", "support_droppedneedle": False},
    )
    assert response.status_code == 200
    saved = prefs.save_get_it_settings.call_args.args[0]
    assert saved.store_region == "DE"
    assert saved.support_droppedneedle is False


def test_get_it_settings_rejects_bad_region(settings_client):
    client, _ = settings_client
    response = client.put(
        "/settings/get-it",
        json={"store_region": "GBR", "support_droppedneedle": True},
    )
    assert response.status_code == 422
