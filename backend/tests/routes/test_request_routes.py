"""Request-flow route tests: POST /requests/new approval gate and per-track
POST /tracks/{recording_mbid}/request.

Drives a real RequestService with mocked request_history/download_service so the
approval logic (user -> awaiting_approval; trusted/admin -> auto-approve + task link)
is exercised, not just the route wiring.
"""

from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI

from api.v1.routes import requests, tracks
from core.dependencies import get_request_service
from middleware import _get_current_user
from services.native.download_service import ALREADY_IN_LIBRARY
from services.request_service import RequestService
from tests.helpers import build_test_client, make_builtin_dispatcher, mock_user

_NEW_BODY = {"musicbrainz_id": "rg-1", "artist": "Radiohead", "album": "OK Computer", "year": 1997}


def _request_service(download_service: AsyncMock) -> tuple[RequestService, AsyncMock]:
    history = AsyncMock()
    history.async_get_record.return_value = None
    get_ds = lambda: download_service  # noqa: E731
    service = RequestService(
        request_history=history,
        get_download_service=get_ds,
        acquisition=make_builtin_dispatcher(get_ds),
    )
    return service, history


def _requests_app(service: RequestService, role: str) -> FastAPI:
    app = FastAPI()
    app.include_router(requests.router)
    app.dependency_overrides[get_request_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role=role, user_id="u1")
    return app


def _tracks_app(service: RequestService, role: str) -> FastAPI:
    app = FastAPI()
    app.include_router(tracks.router)
    app.dependency_overrides[get_request_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role=role, user_id="u1")
    return app


def test_request_new_user_role_awaits_approval():
    ds = AsyncMock()
    service, _history = _request_service(ds)
    response = build_test_client(_requests_app(service, role="user")).post("/requests/new", json=_NEW_BODY)
    assert response.status_code == 202
    assert response.json()["status"] == "awaiting_approval"
    # user request must not dispatch the download until an admin approves
    ds.request_album.assert_not_awaited()


def test_request_new_trusted_auto_approves_and_links_task():
    ds = AsyncMock()
    ds.request_album.return_value = "task-123"
    service, history = _request_service(ds)
    response = build_test_client(_requests_app(service, role="trusted")).post("/requests/new", json=_NEW_BODY)
    assert response.status_code == 202
    assert response.json()["status"] == "pending"
    ds.request_album.assert_awaited_once()
    history.async_update_download_task_id.assert_awaited_once_with("rg-1", "task-123")


def test_request_new_already_in_library_does_not_link_task():
    ds = AsyncMock()
    ds.request_album.return_value = ALREADY_IN_LIBRARY
    service, history = _request_service(ds)
    response = build_test_client(_requests_app(service, role="admin")).post("/requests/new", json=_NEW_BODY)
    assert response.status_code == 202
    assert "already in the library" in response.json()["message"].lower()
    history.async_update_download_task_id.assert_not_awaited()


def test_request_new_unauthenticated_401():
    ds = AsyncMock()
    service, _history = _request_service(ds)
    app = FastAPI()
    app.include_router(requests.router)
    app.dependency_overrides[get_request_service] = lambda: service
    response = build_test_client(app).post("/requests/new", json=_NEW_BODY)
    assert response.status_code == 401


def test_track_request_user_role_awaits_approval_without_dispatch():
    ds = AsyncMock()
    service, history = _request_service(ds)
    response = build_test_client(_tracks_app(service, "user")).post(
        "/tracks/rec-1/request", json={"artist_name": "Radiohead", "track_title": "Airbag"}
    )
    assert response.status_code == 200
    body = response.json()
    assert body["status"] == "awaiting_approval"
    assert body["task_id"] is None
    ds.request_track.assert_not_awaited()
    assert history.async_record_request.await_args.kwargs["request_kind"] == "track"


def test_track_request_trusted_returns_task_id():
    ds = AsyncMock()
    ds.request_track.return_value = "task-track-1"
    service, history = _request_service(ds)
    response = build_test_client(_tracks_app(service, "trusted")).post(
        "/tracks/rec-1/request", json={"artist_name": "Radiohead", "track_title": "Airbag"}
    )
    assert response.status_code == 200
    assert response.json()["status"] == "queued"
    assert response.json()["task_id"] == "task-track-1"
    ds.request_track.assert_awaited_once()
    history.async_update_download_task_id.assert_awaited_once_with(
        "rec-1", "task-track-1"
    )


def test_track_request_already_in_library():
    ds = AsyncMock()
    ds.request_track.return_value = ALREADY_IN_LIBRARY
    service, history = _request_service(ds)
    response = build_test_client(_tracks_app(service, "admin")).post(
        "/tracks/rec-1/request", json={"artist_name": "Radiohead", "track_title": "Airbag"}
    )
    assert response.status_code == 200
    assert response.json()["status"] == "already_in_library"
    history.async_update_status.assert_awaited_once()


def test_track_request_unauthenticated_401():
    ds = AsyncMock()
    app = FastAPI()
    app.include_router(tracks.router)
    service, _history = _request_service(ds)
    app.dependency_overrides[get_request_service] = lambda: service
    response = build_test_client(app).post(
        "/tracks/rec-1/request", json={"artist_name": "Radiohead", "track_title": "Airbag"}
    )
    assert response.status_code == 401
