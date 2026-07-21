from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException

from api.v1.routes.library_target import router
from core.dependencies import (
    get_download_service,
    get_request_history_store,
    get_library_policy_resolver,
    get_cached_local_artwork_service,
    get_preferences_service,
    get_target_catalog_writer_service,
    get_target_library_scan_coordinator,
    get_target_native_library_service,
    get_target_library_ownership_service,
    get_wanted_watcher_service,
)
from middleware import _get_current_admin, _get_current_curator
from tests.helpers import build_test_client, override_user_auth


@pytest.fixture
def app() -> FastAPI:
    application = FastAPI()
    application.include_router(router)
    native = AsyncMock()
    native.artists.return_value = ([], 0)
    native.albums.return_value = ([], 0)
    native.tracks.return_value = ([], 0)
    native.recently_added.return_value = []
    native.provider_ids.return_value = SimpleNamespace(musicbrainz_release_group_ids=[])
    native.canonical_id.return_value = None
    application.dependency_overrides[get_target_native_library_service] = lambda: native
    ownership = AsyncMock()
    ownership.existing_provider_album_ids.return_value = set()
    application.dependency_overrides[get_target_library_ownership_service] = (
        lambda: ownership
    )
    request_history = AsyncMock()
    request_history.async_get_requested_mbids.return_value = set()
    application.dependency_overrides[get_request_history_store] = (
        lambda: request_history
    )
    artwork = AsyncMock()
    artwork.get.return_value = None
    application.dependency_overrides[get_cached_local_artwork_service] = lambda: artwork
    writer = AsyncMock()
    download_service = AsyncMock()
    wanted = AsyncMock()
    application.dependency_overrides[get_target_catalog_writer_service] = lambda: writer
    application.dependency_overrides[get_download_service] = lambda: download_service
    application.dependency_overrides[get_wanted_watcher_service] = lambda: wanted
    application.dependency_overrides[get_target_library_scan_coordinator] = AsyncMock
    application.dependency_overrides[get_library_policy_resolver] = (
        lambda: SimpleNamespace(policy_revision="policy-1")
    )
    application.dependency_overrides[get_preferences_service] = lambda: SimpleNamespace(
        get_download_policy=lambda: SimpleNamespace(
            quality_cutoff="lossless", upgrade_allowed=True
        )
    )
    return application


def test_every_target_library_route_rejects_unauthenticated(app: FastAPI) -> None:
    client = build_test_client(app)
    requests = [
        ("GET", "/library/artists", None),
        ("GET", "/library/albums", None),
        ("GET", "/library/tracks", None),
        ("GET", "/library/stats", None),
        ("GET", "/library/mbids", None),
        ("POST", "/library/membership", {"album_ids": []}),
        ("GET", "/library/recently-added", None),
        ("GET", "/library/artists/a", None),
        ("GET", "/library/artists/a/albums", None),
        ("GET", "/library/albums/a", None),
        ("GET", "/library/albums/a/artwork/cached?v=1", None),
        ("POST", "/library/resolve-tracks", {"items": []}),
        ("GET", "/library/albums/a/tracks", None),
        ("GET", "/library/albums/a/status", None),
        ("DELETE", "/library/album/a", None),
        ("DELETE", "/library/tracks/t", None),
        ("GET", "/library/tracks/t/tags", None),
        (
            "POST",
            "/library/tracks/t",
            {"title": "T", "artist": "A", "album": "B", "track_number": 1},
        ),
        ("POST", "/library/albums/a/rescan", None),
    ]
    for method, path, body in requests:
        response = client.request(method, path, json=body)
        assert response.status_code == 401, (method, path, response.text)


def test_target_catalog_mutations_reject_regular_users(app: FastAPI) -> None:
    def reject() -> None:
        raise HTTPException(status_code=403, detail="Elevated access required")

    override_user_auth(app, role="user")
    app.dependency_overrides[_get_current_admin] = reject
    app.dependency_overrides[_get_current_curator] = reject
    client = build_test_client(app)
    assert client.delete("/library/album/a").status_code == 403
    assert client.delete("/library/tracks/t").status_code == 403
    assert client.get("/library/tracks/t/tags").status_code == 403
    assert (
        client.post(
            "/library/tracks/t",
            json={"title": "T", "artist": "A", "album": "B", "track_number": 1},
        ).status_code
        == 403
    )
    assert client.post("/library/albums/a/rescan").status_code == 403


def test_target_album_removal_stops_watch_by_default(app: FastAPI) -> None:
    override_user_auth(app, role="admin")
    app.dependency_overrides[_get_current_admin] = lambda: SimpleNamespace(id="admin-1")
    writer = app.dependency_overrides[get_target_catalog_writer_service]()
    writer.provider_release_group_id.return_value = "rg-1"
    writer.remove_album.return_value = ["track-1"]
    download_service = app.dependency_overrides[get_download_service]()
    wanted = app.dependency_overrides[get_wanted_watcher_service]()

    response = build_test_client(app).delete("/library/album/local-1?delete_files=true")

    assert response.status_code == 200
    download_service.purge_album_downloads.assert_awaited_once_with("rg-1")
    wanted.stop_after_library_removal.assert_awaited_once_with("rg-1")


def test_target_album_removal_can_keep_watch(app: FastAPI) -> None:
    override_user_auth(app, role="admin")
    app.dependency_overrides[_get_current_admin] = lambda: SimpleNamespace(id="admin-1")
    writer = app.dependency_overrides[get_target_catalog_writer_service]()
    writer.provider_release_group_id.return_value = "rg-1"
    writer.remove_album.return_value = ["track-1"]
    wanted = app.dependency_overrides[get_wanted_watcher_service]()

    response = build_test_client(app).delete("/library/album/local-1?stop_wanted=false")

    assert response.status_code == 200
    wanted.stop_after_library_removal.assert_not_awaited()
    wanted.continue_after_library_removal.assert_awaited_once_with("rg-1")


def test_target_artist_browse_forwards_supported_sort(app: FastAPI) -> None:
    override_user_auth(app, role="user")
    client = build_test_client(app)
    response = client.get(
        "/library/artists?limit=500&offset=-3&sort_by=album_count&sort_order=desc&q= Jazz "
    )
    service = app.dependency_overrides[get_target_native_library_service]()

    assert response.status_code == 200
    service.artists.assert_awaited_once_with(
        limit=100,
        offset=0,
        search="Jazz",
        sort_by="album_count",
        sort_order="desc",
    )


def test_target_provider_ids_preserve_existing_library_store_contract(
    app: FastAPI,
) -> None:
    override_user_auth(app, role="user")
    service = app.dependency_overrides[get_target_native_library_service]()
    service.provider_ids.return_value = SimpleNamespace(
        musicbrainz_release_group_ids=["owned-rg"]
    )
    history = app.dependency_overrides[get_request_history_store]()
    history.async_get_requested_mbids.return_value = {"requested-rg"}

    response = build_test_client(app).get("/library/mbids")

    assert response.status_code == 200
    assert response.json() == {
        "mbids": ["owned-rg"],
        "requested_mbids": ["requested-rg"],
    }


def test_target_membership_is_bounded_and_candidate_scoped(app: FastAPI) -> None:
    override_user_auth(app, role="user")
    ownership = app.dependency_overrides[get_target_library_ownership_service]()
    ownership.existing_provider_album_ids.return_value = {"owned-rg"}
    history = app.dependency_overrides[get_request_history_store]()
    history.async_existing_requested_mbids.return_value = {"requested-rg"}

    response = build_test_client(app).post(
        "/library/membership",
        json={"album_ids": ["OWNED-RG", "requested-rg", "owned-rg"]},
    )

    assert response.status_code == 200
    assert response.json() == {
        "owned_ids": ["owned-rg"],
        "requested_ids": ["requested-rg"],
    }
    ownership.existing_provider_album_ids.assert_awaited_once_with(
        ["owned-rg", "requested-rg"]
    )
    history.async_existing_requested_mbids.assert_awaited_once_with(
        ["owned-rg", "requested-rg"]
    )


def test_cached_artwork_route_is_immutable_and_never_warms(app: FastAPI) -> None:
    override_user_auth(app, role="user")
    service = app.dependency_overrides[get_cached_local_artwork_service]()
    service.get.return_value = (b"\xff\xd8\xffcover", "image/jpeg", "provider", "abc")
    client = build_test_client(app)

    response = client.get("/library/albums/local-uuid/artwork/cached?v=7")

    assert response.status_code == 200
    assert response.content == b"\xff\xd8\xffcover"
    assert response.headers["cache-control"] == "private, max-age=31536000, immutable"
    assert response.headers["etag"] == '"abc"'
    assert response.headers["x-cover-source"] == "provider"
    service.get.assert_awaited_once_with("local-uuid", 7)


def test_cached_artwork_route_returns_terminal_local_miss(app: FastAPI) -> None:
    override_user_auth(app, role="user")
    client = build_test_client(app)

    response = client.get("/library/albums/local-uuid/artwork/cached?v=7")

    assert response.status_code == 404
    assert response.content == b""
    assert response.headers["cache-control"] == "private, max-age=30"
    assert response.headers["x-cover-state"] == "missing"


def test_target_library_route_inventory_is_complete() -> None:
    inventory = {
        (method, route.path)
        for route in router.routes
        for method in getattr(route, "methods", set())
        if method in {"GET", "POST", "DELETE"}
    }
    assert inventory == {
        ("GET", "/library/artists"),
        ("GET", "/library/albums"),
        ("GET", "/library/tracks"),
        ("GET", "/library/stats"),
        ("GET", "/library/mbids"),
        ("POST", "/library/membership"),
        ("GET", "/library/recently-added"),
        ("GET", "/library/artists/{artist_id}"),
        ("GET", "/library/artists/{artist_id}/albums"),
        ("GET", "/library/albums/{album_id}"),
        ("GET", "/library/albums/{album_id}/artwork/cached"),
        ("POST", "/library/resolve-tracks"),
        ("GET", "/library/albums/{album_id}/tracks"),
        ("GET", "/library/albums/{album_id}/status"),
        ("DELETE", "/library/album/{album_id}"),
        ("DELETE", "/library/tracks/{track_id}"),
        ("GET", "/library/tracks/{track_id}/tags"),
        ("POST", "/library/tracks/{track_id}"),
        ("POST", "/library/albums/{album_id}/rescan"),
    }
