"""Per-track download route tests (Phase 7): POST /tracks/{recording_mbid}/request."""

from unittest.mock import AsyncMock

from fastapi import FastAPI

from api.v1.routes import tracks
from api.v1.schemas.download import TrackRequestResponse
from core.dependencies import get_request_service
from core.exceptions import ValidationError
from middleware import _get_current_user
from tests.helpers import build_test_client, mock_user


def _app(service, role="user") -> FastAPI:
    app = FastAPI()
    app.include_router(tracks.router)
    app.dependency_overrides[get_request_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role=role, user_id="u1")
    return app


def test_request_track_queued():
    service = AsyncMock()
    service.request_track.return_value = TrackRequestResponse(
        status="queued", task_id="task-1"
    )
    response = build_test_client(_app(service)).post(
        "/tracks/rec-1/request",
        json={"artist_name": "Radiohead", "track_title": "Airbag"},
    )
    assert response.status_code == 200
    body = response.json()
    assert body["status"] == "queued"
    assert body["task_id"] == "task-1"
    service.request_track.assert_awaited_once()
    kwargs = service.request_track.await_args.kwargs
    assert service.request_track.await_args.args[0] == "rec-1"
    assert kwargs["user_id"] == "u1"
    assert kwargs["user_role"] == "user"


def test_request_track_already_in_library():
    service = AsyncMock()
    service.request_track.return_value = TrackRequestResponse(
        status="already_in_library"
    )
    response = build_test_client(_app(service)).post(
        "/tracks/rec-1/request",
        json={"artist_name": "Radiohead", "track_title": "Airbag"},
    )
    assert response.status_code == 200
    body = response.json()
    assert body["status"] == "already_in_library"
    assert body["task_id"] is None


def test_request_track_unauthenticated_401():
    service = AsyncMock()
    app = FastAPI()
    app.include_router(tracks.router)
    app.dependency_overrides[get_request_service] = lambda: service
    response = build_test_client(app).post(
        "/tracks/rec-1/request",
        json={"artist_name": "Radiohead", "track_title": "Airbag"},
    )
    assert response.status_code == 401


def test_request_track_over_quota_rejected_at_submit():
    """Request-service quota failures remain a clear 400 at the route boundary."""
    service = AsyncMock()
    service.request_track.side_effect = ValidationError(
        "Request limit reached (5 per 7 days)"
    )

    response = build_test_client(_app(service)).post(
        "/tracks/rec-1/request",
        json={"artist_name": "Radiohead", "track_title": "Airbag"},
    )

    assert response.status_code == 400
    assert "Request limit reached" in response.json()["error"]["message"]
    service.request_track.assert_awaited_once()
