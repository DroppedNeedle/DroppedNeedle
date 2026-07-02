"""Edition selection routes (CollectionManagement Feature E): viewing is open,
pin/acquire are admin/trusted-only (D16)."""

from unittest.mock import AsyncMock

from fastapi import FastAPI

from api.v1.routes import albums
from core.dependencies import get_album_service, get_download_service
from middleware import _get_current_curator, _get_current_user
from tests.helpers import build_test_client, mock_user

RG = "11111111-1111-4111-8111-111111111111"
REL = "22222222-2222-4222-8222-222222222222"


def _app(album_service, download_service=None, *, curator: bool = False) -> FastAPI:
    app = FastAPI()
    app.include_router(albums.router)
    app.dependency_overrides[get_album_service] = lambda: album_service
    app.dependency_overrides[get_download_service] = lambda: download_service or AsyncMock()
    app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user", user_id="u1")
    if curator:
        app.dependency_overrides[_get_current_curator] = lambda: mock_user(
            role="trusted", user_id="cur-1"
        )
    return app


def _editions_payload() -> dict:
    return {
        "items": [
            {
                "release_mbid": REL, "title": "OK Computer", "disambiguation": None,
                "date": "1997-06-16", "country": "GB", "packaging": None,
                "status": "Official", "track_count": 12, "is_owned": True,
                "is_pinned": False,
            }
        ],
        "pinned_release_mbid": None,
        "owned_release_mbid": REL,
    }


def test_editions_list_is_viewable_by_any_user():
    album_service = AsyncMock()
    album_service.list_editions.return_value = _editions_payload()

    resp = build_test_client(_app(album_service)).get(f"/albums/{RG}/editions")

    assert resp.status_code == 200
    body = resp.json()
    assert body["items"][0]["release_mbid"] == REL
    assert body["items"][0]["is_owned"] is True
    assert body["owned_release_mbid"] == REL


def test_pin_and_acquire_require_curator_role():
    album_service = AsyncMock()
    download_service = AsyncMock()
    client = build_test_client(_app(album_service, download_service))

    # no curator auth state -> 401 before any service call
    assert client.put(f"/albums/{RG}/edition", json={"release_mbid": REL}).status_code == 401
    assert client.delete(f"/albums/{RG}/edition").status_code == 401
    assert client.post(f"/albums/{RG}/edition/acquire").status_code == 401
    album_service.set_edition_pin.assert_not_awaited()
    download_service.acquire_edition.assert_not_awaited()


def test_pin_set_clear_and_acquire_for_curator():
    album_service = AsyncMock()
    download_service = AsyncMock()
    download_service.acquire_edition.return_value = {
        "release_mbid": REL, "total_tracks": 12, "requested": 2, "upgrades": 1, "skipped": 9,
    }
    client = build_test_client(_app(album_service, download_service, curator=True))

    resp = client.put(f"/albums/{RG}/edition", json={"release_mbid": REL})
    assert resp.status_code == 200
    assert resp.json()["pinned_release_mbid"] == REL
    album_service.set_edition_pin.assert_awaited_once_with(RG, REL, "cur-1")

    resp = client.delete(f"/albums/{RG}/edition")
    assert resp.status_code == 200
    assert resp.json()["pinned_release_mbid"] is None
    album_service.clear_edition_pin.assert_awaited_once_with(RG)

    resp = client.post(f"/albums/{RG}/edition/acquire")
    assert resp.status_code == 200
    assert resp.json() == {
        "release_mbid": REL, "total_tracks": 12, "requested": 2, "upgrades": 1, "skipped": 9,
    }
    download_service.acquire_edition.assert_awaited_once_with("cur-1", RG)
