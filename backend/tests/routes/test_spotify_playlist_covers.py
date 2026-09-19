"""GH-287: an imported Spotify playlist cover must surface through the web API
(custom_cover_url + /cover serving) with the same visibility rules as a
user-uploaded cover. Real stores, real router; CDN is the MockTransport mock."""

from __future__ import annotations

import threading
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI

from api.v1.routes.playlists import router as playlists_router
from core.dependencies import get_playlist_service
from infrastructure.cache.memory_cache import InMemoryCache
from infrastructure.persistence.auth_store import AuthStore
from repositories.playlist_repository import PlaylistRepository
from services.playlist_service import PlaylistService
from services.spotify_import_service import SpotifyImportService, cover_fetcher_for
from tests.helpers import build_test_client, override_user_auth
from tests.mocks.spotify_cdn_mock import COVER_URL, JPEG_BYTES, SpotifyCdnMock

OWNER_ID = "owner-id"
OTHER_ID = "other-id"


@pytest.fixture
def env(tmp_path):
    db_path = tmp_path / "library.db"
    lock = threading.Lock()
    auth_store = AuthStore(db_path=db_path, write_lock=lock)
    repo = PlaylistRepository(db_path=db_path, write_lock=lock)
    service = PlaylistService(repo=repo, cache_dir=tmp_path, auth_store=auth_store)
    cdn = SpotifyCdnMock()

    client = AsyncMock()
    client.get_playlist.return_value = {
        "id": "spot-1",
        "name": "From Spotify",
        "images": [{"url": COVER_URL, "width": 640, "height": 640}],
    }
    client.get_playlist_tracks.return_value = []
    factory = AsyncMock()
    factory.resolve_spotify.return_value = client
    importer = SpotifyImportService(
        client_factory=factory,
        playlist_repo=repo,
        mb_repo=AsyncMock(),
        playlist_service=service,
        cache=InMemoryCache(),
        cover_fetcher=cover_fetcher_for(cdn.client()),
    )
    return SimpleNamespace(
        auth_store=auth_store, importer=importer, service=service, cdn=cdn
    )


async def _seed_users(auth_store):
    await auth_store.create_user(
        id=OWNER_ID, display_name="Olivia", role="user", username="owner"
    )
    await auth_store.create_user(
        id=OTHER_ID, display_name="Otto", role="user", username="other"
    )


def _client(env, *, user_id):
    app = FastAPI()
    app.include_router(playlists_router)
    app.dependency_overrides[get_playlist_service] = lambda: env.service
    override_user_auth(app, role="user", user_id=user_id)
    return build_test_client(app)


@pytest.mark.asyncio
async def test_imported_cover_surfaces_in_web_payload_and_serves(env):
    await _seed_users(env.auth_store)
    pid = await env.importer.ensure_playlist_record(OWNER_ID, "spot-1", "From Spotify")
    await env.importer.populate_playlist(OWNER_ID, "spot-1", pid)
    owner = _client(env, user_id=OWNER_ID)

    detail = owner.get(f"/playlists/{pid}")
    assert detail.status_code == 200
    assert detail.json()["custom_cover_url"] == f"/api/v1/playlists/{pid}/cover"

    listing = owner.get("/playlists")
    assert listing.status_code == 200
    mine = next(p for p in listing.json()["playlists"] if p["id"] == pid)
    assert mine["custom_cover_url"] == f"/api/v1/playlists/{pid}/cover"

    served = owner.get(f"/playlists/{pid}/cover")
    assert served.status_code == 200
    assert served.headers["content-type"].startswith("image/jpeg")
    assert served.content == JPEG_BYTES


@pytest.mark.asyncio
async def test_imported_private_cover_invisible_to_other_user(env):
    await _seed_users(env.auth_store)
    pid = await env.importer.ensure_playlist_record(OWNER_ID, "spot-1", "Private Mix")
    await env.importer.populate_playlist(OWNER_ID, "spot-1", pid)
    other = _client(env, user_id=OTHER_ID)

    assert other.get(f"/playlists/{pid}").status_code == 404
    assert other.get(f"/playlists/{pid}/cover").status_code == 404
