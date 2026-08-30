import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from api.v1.routes.library_target import router
from api.v1.schemas.edition_conversion import (
    EditionConversionPreviewResponse,
    EditionConversionStatusResponse,
)
from api.v1.schemas.library_target import TargetNativeAlbumDetail
from core.dependencies import (
    get_download_service,
    get_edition_conversion_service,
    get_request_history_store,
    get_library_policy_resolver,
    get_cached_local_artwork_service,
    get_preferences_service,
    get_target_catalog_writer_service,
    get_target_library_scan_coordinator,
    get_target_native_library_service,
    get_target_library_ownership_service,
    get_target_album_edition_finder_service,
    get_wanted_watcher_service,
)
from infrastructure.persistence.request_history import RequestHistoryStore
from middleware import _get_current_admin, _get_current_curator
from tests.helpers import (
    add_production_exception_handlers,
    build_test_client,
    override_admin_auth,
    override_user_auth,
)


@pytest.fixture
def app() -> FastAPI:
    application = FastAPI()
    application.include_router(router)
    native = AsyncMock()
    native.artists.return_value = ([], 0)
    native.artist_scope_counts.return_value = (0, 0)
    native.artist_appearances.return_value = ([], 0, 0)
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
    edition_finder = AsyncMock()
    application.dependency_overrides[get_target_album_edition_finder_service] = (
        lambda: edition_finder
    )
    conversion = AsyncMock()
    conversion_response = EditionConversionStatusResponse(
        job_id="conversion-1",
        local_album_id="album-1",
        release_group_mbid="00000000-0000-4000-8000-000000000001",
        release_mbid="00000000-0000-4000-8000-000000000002",
        album_title="Album",
        artist_name="Artist",
        state="preflight",
        download_source_ready=True,
        required_temporary_bytes=1,
        kept_count=1,
        acquire_count=1,
        recycle_count=1,
        staged_count=0,
        failed_count=0,
        row_revision=1,
        created_at=1,
        updated_at=1,
    )
    conversion.create_preflight.return_value = conversion_response
    conversion.start.return_value = conversion_response
    conversion.status.return_value = conversion_response
    conversion.create_final_preview.return_value = EditionConversionPreviewResponse(
        status=conversion_response, preview_token="preview-token"
    )
    conversion.retry.return_value = conversion_response
    conversion.recheck.return_value = conversion_response
    conversion.cancel.return_value = conversion_response
    application.dependency_overrides[get_edition_conversion_service] = (
        lambda: conversion
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


def test_edition_conversion_routes_forward_sealed_inputs(app: FastAPI) -> None:
    override_admin_auth(app)
    conversion = app.dependency_overrides[get_edition_conversion_service]()
    client = build_test_client(app)

    preflight = client.post(
        "/library/albums/album-1/edition-conversions/preflight",
        json={
            "release_group_mbid": "00000000-0000-4000-8000-000000000001",
            "release_mbid": "00000000-0000-4000-8000-000000000002",
        },
    )
    started = client.post(
        "/library/edition-conversions/conversion-1/start",
        json={
            "preflight_token": "sealed",
            "expected_row_revision": 3,
            "confirmation": True,
        },
    )
    status = client.get("/library/edition-conversions/conversion-1")
    preview = client.post(
        "/library/edition-conversions/conversion-1/preview",
        json={"expected_row_revision": 4},
    )
    retried = client.post(
        "/library/edition-conversions/conversion-1/retry",
        json={"target_ordinals": [2, 4], "expected_row_revision": 5},
    )
    rechecked = client.post(
        "/library/edition-conversions/conversion-1/recheck",
        json={"expected_row_revision": 6},
    )
    cancelled = client.post(
        "/library/edition-conversions/conversion-1/cancel",
        json={"expected_row_revision": 7, "confirmation": True},
    )

    assert [
        preflight.status_code,
        started.status_code,
        status.status_code,
        preview.status_code,
        retried.status_code,
        rechecked.status_code,
        cancelled.status_code,
    ] == [200, 200, 200, 200, 200, 200, 200]
    conversion.create_preflight.assert_awaited_once_with(
        local_album_id="album-1",
        release_group_mbid="00000000-0000-4000-8000-000000000001",
        release_mbid="00000000-0000-4000-8000-000000000002",
        actor_user_id="test-admin-id",
    )
    conversion.start.assert_awaited_once_with(
        "conversion-1",
        preflight_token="sealed",
        expected_row_revision=3,
        confirmation=True,
    )
    conversion.status.assert_awaited_once_with("conversion-1")
    conversion.create_final_preview.assert_awaited_once_with(
        "conversion-1", expected_row_revision=4
    )
    conversion.retry.assert_awaited_once_with(
        "conversion-1", target_ordinals=[2, 4], expected_row_revision=5
    )
    conversion.recheck.assert_awaited_once_with("conversion-1", expected_row_revision=6)
    conversion.cancel.assert_awaited_once_with(
        "conversion-1", expected_row_revision=7, confirmation=True
    )


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
        ("GET", "/library/artists/a/appearances", None),
        ("GET", "/library/albums/a", None),
        ("GET", "/library/albums/a/reidentification/releases", None),
        ("POST", "/library/albums/a/management/re-enable", {}),
        ("POST", "/library/albums/a/edition-conversions/preflight", {}),
        ("POST", "/library/edition-conversions/j/start", {}),
        ("GET", "/library/edition-conversions/j", None),
        ("POST", "/library/edition-conversions/j/preview", {}),
        ("POST", "/library/edition-conversions/j/retry", {}),
        ("POST", "/library/edition-conversions/j/recheck", {}),
        ("POST", "/library/edition-conversions/j/cancel", {}),
        ("GET", "/library/albums/a/copies", None),
        ("GET", "/library/albums/a/artwork/cached?v=1", None),
        ("POST", "/library/resolve-tracks", {"items": []}),
        ("GET", "/library/albums/a/tracks", None),
        ("GET", "/library/albums/a/status", None),
        ("DELETE", "/library/album/a", None),
        ("DELETE", "/library/tracks/t", None),
        ("GET", "/library/tracks/t/tags", None),
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
    assert client.post("/library/albums/a/rescan").status_code == 403
    assert client.get("/library/albums/a/reidentification/releases").status_code == 403
    assert (
        client.post("/library/albums/a/management/re-enable", json={}).status_code
        == 403
    )
    assert (
        client.post(
            "/library/albums/a/edition-conversions/preflight", json={}
        ).status_code
        == 403
    )
    assert client.get("/library/edition-conversions/j").status_code == 403


def test_album_detail_exposes_current_management_identity_readiness(
    app: FastAPI,
) -> None:
    override_user_auth(app, role="user")
    native = app.dependency_overrides[get_target_native_library_service]()
    native.album_detail.return_value = TargetNativeAlbumDetail(
        id="album-1",
        title="Album",
        artist_name="Artist",
        artist_id="artist-1",
        management_identity_readiness="track_mapping_required",
    )

    response = build_test_client(app).get("/library/albums/album-1")

    assert response.status_code == 200
    assert response.json()["management_identity_readiness"] == (
        "track_mapping_required"
    )


def test_admin_can_search_exact_releases_with_canonical_metadata(app: FastAPI) -> None:
    from models.identification import ReleaseEdition, ReleaseEditionSearchPage

    override_user_auth(app, role="admin")
    app.dependency_overrides[_get_current_admin] = lambda: SimpleNamespace(id="admin-1")
    service = app.dependency_overrides[get_target_album_edition_finder_service]()
    service.search.return_value = (
        "Album",
        "Artist",
        "group-1",
        "release-1",
        ReleaseEditionSearchPage(
            items=[
                ReleaseEdition(
                    release_mbid="release-1",
                    release_group_mbid="group-1",
                    artist_name="Artist",
                    title="Album",
                    musicbrainz_url="https://musicbrainz.org/release/release-1",
                    score=100,
                )
            ],
            total=1,
            offset=0,
            limit=12,
        ),
    )

    response = build_test_client(app).get(
        "/library/albums/album-1/reidentification/releases"
        "?title=Album&artist=Artist&limit=12&offset=0"
    )

    assert response.status_code == 200
    assert response.json()["title_query"] == "Album"
    assert response.json()["artist_query"] == "Artist"
    assert response.json()["items"][0]["belongs_to_current_release_group"] is True
    assert response.json()["items"][0]["is_current_release"] is True
    assert response.json()["items"][0]["release_mbid"] == "release-1"
    service.search.assert_awaited_once_with(
        "album-1", title="Album", artist="Artist", limit=12, offset=0
    )


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
        scope="album",
    )


def test_target_contributor_browse_forwards_scope_and_appearance_sort(
    app: FastAPI,
) -> None:
    override_user_auth(app, role="user")

    response = build_test_client(app).get(
        "/library/artists?scope=contributors&sort_by=appearance_count"
    )
    service = app.dependency_overrides[get_target_native_library_service]()

    assert response.status_code == 200
    assert response.json() == {
        "items": [],
        "total": 0,
        "album_artist_total": 0,
        "contributor_total": 0,
    }
    service.artists.assert_awaited_once_with(
        limit=50,
        offset=0,
        search=None,
        sort_by="appearance_count",
        sort_order="asc",
        scope="contributors",
    )


def test_target_artist_appearances_forwards_bounded_pagination(app: FastAPI) -> None:
    override_user_auth(app, role="user")

    response = build_test_client(app).get(
        "/library/artists/artist-1/appearances?limit=500&offset=3"
    )
    service = app.dependency_overrides[get_target_native_library_service]()

    assert response.status_code == 422

    response = build_test_client(app).get(
        "/library/artists/artist-1/appearances?limit=25&offset=3"
    )

    assert response.status_code == 200
    assert response.json() == {
        "items": [],
        "total": 0,
        "total_tracks": 0,
        "offset": 3,
        "limit": 25,
    }
    service.artist_appearances.assert_awaited_once_with("artist-1", limit=25, offset=3)


def test_target_artist_appearances_alias_redirect_preserves_page(app: FastAPI) -> None:
    override_user_auth(app, role="user")
    service = app.dependency_overrides[get_target_native_library_service]()
    service.canonical_id.return_value = "local-artist"

    response = build_test_client(app).get(
        "/library/artists/provider-artist/appearances?limit=20&offset=40",
        follow_redirects=False,
    )

    assert response.status_code == 308
    assert response.headers["location"] == (
        "/library/artists/local-artist/appearances?limit=20&offset=40"
    )
    service.artist_appearances.assert_not_awaited()


@pytest.mark.parametrize(
    ("path", "expected"),
    [
        (
            "/library/artists/provider-artist",
            "/library/artists/local-artist",
        ),
        (
            "/library/artists/provider-artist/albums",
            "/library/artists/local-artist/albums",
        ),
        (
            "/library/artists/provider-artist/appearances?limit=20&offset=40",
            "/library/artists/local-artist/appearances?limit=20&offset=40",
        ),
        ("/library/albums/provider-album", "/library/albums/local-album"),
        (
            "/library/albums/provider-album/tracks",
            "/library/albums/local-album/tracks",
        ),
        (
            "/library/albums/provider-album/status",
            "/library/albums/local-album/status",
        ),
    ],
)
def test_all_target_alias_redirects_are_root_relative(
    app: FastAPI, path: str, expected: str
) -> None:
    override_user_auth(app, role="user")
    service = app.dependency_overrides[get_target_native_library_service]()

    async def canonical_id(kind: str, _identifier: str) -> str:
        return "local-artist" if kind == "artist" else "local-album"

    service.canonical_id.side_effect = canonical_id
    response = build_test_client(app).get(path, follow_redirects=False)

    assert response.status_code == 308
    assert response.headers["location"] == expected
    assert "://" not in response.headers["location"]


def test_target_alias_redirect_preserves_root_path(app: FastAPI) -> None:
    override_user_auth(app, role="user")
    service = app.dependency_overrides[get_target_native_library_service]()
    service.canonical_id.return_value = "local-artist"
    add_production_exception_handlers(app)
    client = TestClient(app, raise_server_exceptions=False, root_path="/music")

    response = client.get(
        "/library/artists/provider-artist/appearances?limit=20&offset=40",
        follow_redirects=False,
    )

    assert response.status_code == 308
    assert response.headers["location"] == (
        "/music/library/artists/local-artist/appearances?limit=20&offset=40"
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
    history.async_get_requested_mbids.assert_awaited_once_with(
        request_kind="album"
    )


def test_target_provider_ids_requested_mbids_exclude_track_keys(
    app: FastAPI, tmp_path
) -> None:
    override_user_auth(app, role="user")
    service = app.dependency_overrides[get_target_native_library_service]()
    service.provider_ids.return_value = SimpleNamespace(
        musicbrainz_release_group_ids=["owned-rg"]
    )
    history = RequestHistoryStore(tmp_path / "requests.db")

    async def _seed() -> None:
        await history.async_record_request(
            "requested-rg", "Requested Artist", "Requested Album"
        )
        await history.async_record_request(
            "recording-1",
            "Requested Artist",
            "Requested Album",
            track_title="Requested Track",
            request_kind="track",
        )

    asyncio.run(_seed())
    app.dependency_overrides[get_request_history_store] = lambda: history

    response = build_test_client(app).get("/library/mbids")

    assert response.status_code == 200
    payload = response.json()
    assert payload["mbids"] == ["owned-rg"]
    # The exact-track row persists under its track-prefixed key and must
    # never leak into the album requested-ID set.
    assert "track:recording-1" not in payload["requested_mbids"]
    assert payload["requested_mbids"] == ["requested-rg"]


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
        ("GET", "/library/artists/{artist_id}/appearances"),
        ("GET", "/library/albums/{album_id}"),
        ("GET", "/library/albums/{album_id}/reidentification/releases"),
        ("POST", "/library/albums/{album_id}/management/re-enable"),
        ("POST", "/library/albums/{album_id}/edition-conversions/preflight"),
        ("POST", "/library/edition-conversions/{job_id}/start"),
        ("GET", "/library/edition-conversions/{job_id}"),
        ("POST", "/library/edition-conversions/{job_id}/preview"),
        ("POST", "/library/edition-conversions/{job_id}/retry"),
        ("POST", "/library/edition-conversions/{job_id}/recheck"),
        ("POST", "/library/edition-conversions/{job_id}/cancel"),
        ("GET", "/library/albums/{album_id}/copies"),
        ("GET", "/library/albums/{album_id}/artwork/cached"),
        ("POST", "/library/resolve-tracks"),
        ("GET", "/library/albums/{album_id}/tracks"),
        ("GET", "/library/albums/{album_id}/status"),
        ("DELETE", "/library/album/{album_id}"),
        ("DELETE", "/library/tracks/{track_id}"),
        ("GET", "/library/tracks/{track_id}/tags"),
        ("POST", "/library/albums/{album_id}/rescan"),
    }
