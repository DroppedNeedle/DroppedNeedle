import pytest
from unittest.mock import AsyncMock

from fastapi import FastAPI
from fastapi.testclient import TestClient

from api.v1.routes.taste_graph import router
from api.v1.schemas.taste_graph import (
    TasteGraphItem,
    TasteGraphReason,
    TasteGraphResponse,
    TasteGraphSeed,
)
from core.dependencies import get_taste_graph_service
from tests.helpers import override_user_auth

_UID = "test-user-id"


def _make_response() -> TasteGraphResponse:
    return TasteGraphResponse(
        cold_start=False,
        generated_at="2026-07-10T00:00:00+00:00",
        seeds=[TasteGraphSeed(artist_mbid="seed-1", name="Seed Artist", weight=1.0)],
        items=[
            TasteGraphItem(
                kind="artist",
                mbid="candidate-1",
                name="Candidate Artist",
                score=0.9,
                reasons=[
                    TasteGraphReason(
                        type="member",
                        label="Band-member connection with Seed Artist",
                        via_mbid="seed-1",
                        via_name="Seed Artist",
                    )
                ],
                in_library=False,
            ),
            TasteGraphItem(
                kind="album",
                mbid="rg-1",
                name="Candidate Album",
                artist_mbid="candidate-2",
                artist_name="Label Mate",
                score=0.7,
                reasons=[
                    TasteGraphReason(
                        type="label",
                        label="On Test Label with Seed Artist",
                        via_mbid="label-1",
                        via_name="Test Label",
                    )
                ],
                in_library=False,
            ),
        ],
    )


@pytest.fixture
def mock_service():
    mock = AsyncMock()
    mock.get_taste_graph = AsyncMock(return_value=_make_response())
    return mock


@pytest.fixture
def client(mock_service):
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_taste_graph_service] = lambda: mock_service
    override_user_auth(app, user_id=_UID)
    return TestClient(app)


class TestTasteGraphRoute:
    def test_builds_for_current_user(self, client, mock_service):
        resp = client.get("/discover/taste-graph")
        assert resp.status_code == 200
        mock_service.get_taste_graph.assert_awaited_once_with(_UID)

    def test_response_shape(self, client):
        resp = client.get("/discover/taste-graph")
        assert resp.status_code == 200
        data = resp.json()
        assert data["cold_start"] is False
        assert data["generated_at"]
        assert data["seeds"][0] == {
            "artist_mbid": "seed-1",
            "name": "Seed Artist",
            "weight": 1.0,
        }
        artist_item = data["items"][0]
        assert artist_item["kind"] == "artist"
        assert artist_item["mbid"] == "candidate-1"
        assert artist_item["in_library"] is False
        assert artist_item["reasons"][0]["type"] == "member"
        album_item = data["items"][1]
        assert album_item["kind"] == "album"
        assert album_item["artist_mbid"] == "candidate-2"
        assert album_item["reasons"][0]["type"] == "label"
        assert album_item["reasons"][0]["via_name"] == "Test Label"

    def test_cold_start_shape(self, client, mock_service):
        mock_service.get_taste_graph = AsyncMock(
            return_value=TasteGraphResponse(
                cold_start=True, generated_at="2026-07-10T00:00:00+00:00",
            )
        )
        resp = client.get("/discover/taste-graph")
        assert resp.status_code == 200
        data = resp.json()
        assert data["cold_start"] is True
        assert data["seeds"] == []
        assert data["items"] == []
