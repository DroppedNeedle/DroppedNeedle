import os
import tempfile

os.environ.setdefault("ROOT_APP_DIR", tempfile.mkdtemp())

import pytest
from unittest.mock import AsyncMock

from fastapi import FastAPI
from fastapi.testclient import TestClient

from api.v1.routes.artists import router
from core.dependencies import get_artist_service, get_artist_discovery_service, get_artist_enrichment_service
from core.exceptions import ExternalServiceError, ResourceNotFoundError
from models.artist import ArtistInfo, ReleaseItem
from tests.helpers import add_production_exception_handlers


VALID_MBID = "f4a31f0a-51dd-4fa7-986d-3095c40c5ed9"


def _minimal_artist_info(mbid: str = VALID_MBID) -> ArtistInfo:
    return ArtistInfo(
        name="Test Artist",
        musicbrainz_id=mbid,
        albums=[ReleaseItem(id="rg-1", title="Album One", type="Album", year=2024)],
        singles=[],
        eps=[],
        release_group_count=1,
        in_library=False,
    )


@pytest.fixture
def mock_artist_service():
    mock = AsyncMock()
    mock.get_artist_info_basic = AsyncMock(return_value=_minimal_artist_info())
    mock.get_artist_info = AsyncMock(side_effect=AssertionError(
        "get_artist_info should NOT be called - route must use get_artist_info_basic"
    ))
    mock.get_artist_releases = AsyncMock()
    mock.get_artist_extended_info = AsyncMock()
    return mock


@pytest.fixture
def mock_discovery_service():
    return AsyncMock()


@pytest.fixture
def mock_enrichment_service():
    return AsyncMock()


@pytest.fixture
def client(mock_artist_service, mock_discovery_service, mock_enrichment_service):
    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    app.dependency_overrides[get_artist_service] = lambda: mock_artist_service
    app.dependency_overrides[get_artist_discovery_service] = lambda: mock_discovery_service
    app.dependency_overrides[get_artist_enrichment_service] = lambda: mock_enrichment_service
    add_production_exception_handlers(app)
    return TestClient(app, raise_server_exceptions=False)


class TestGetArtistBasicRoute:
    def test_get_artist_calls_basic_method(self, client, mock_artist_service):
        response = client.get(f"/api/v1/artists/{VALID_MBID}")

        assert response.status_code == 200
        mock_artist_service.get_artist_info_basic.assert_awaited_once_with(VALID_MBID)

    def test_get_artist_does_not_call_full_method(self, client, mock_artist_service):
        response = client.get(f"/api/v1/artists/{VALID_MBID}")

        assert response.status_code == 200
        mock_artist_service.get_artist_info.assert_not_awaited()

    def test_get_artist_returns_valid_response(self, client):
        response = client.get(f"/api/v1/artists/{VALID_MBID}")
        body = response.json()

        assert response.status_code == 200
        assert body["name"] == "Test Artist"
        assert body["musicbrainz_id"] == VALID_MBID
        assert body["description"] is None
        assert body["image"] is None
        assert len(body["albums"]) == 1
        assert body["release_group_count"] == 1

    def test_get_artist_value_error_returns_400(self, client, mock_artist_service):
        mock_artist_service.get_artist_info_basic = AsyncMock(
            side_effect=ValueError("Invalid artist request")
        )
        response = client.get(f"/api/v1/artists/{VALID_MBID}")

        assert response.status_code == 400

    def test_get_artist_not_found_uses_safe_envelope(self, client, mock_artist_service):
        mock_artist_service.get_artist_info_basic = AsyncMock(
            side_effect=ResourceNotFoundError("Artist not found")
        )

        response = client.get(f"/api/v1/artists/{VALID_MBID}")

        assert response.status_code == 404
        assert response.json() == {
            "error": {
                "code": "NOT_FOUND",
                "message": "Artist not found",
                "details": None,
            }
        }

    def test_get_artist_provider_failure_uses_generic_envelope(
        self, client, mock_artist_service
    ):
        mock_artist_service.get_artist_info_basic = AsyncMock(
            side_effect=ExternalServiceError("upstream secret")
        )

        response = client.get(f"/api/v1/artists/{VALID_MBID}")

        assert response.status_code == 503
        assert response.json() == {
            "error": {
                "code": "EXTERNAL_SERVICE_UNAVAILABLE",
                "message": "External service unavailable",
                "details": None,
            }
        }
        assert "upstream secret" not in response.text
