"""Security: the auth matrix. Every native-engine endpoint that declares an auth
posture is exercised under three identities and must reject the wrong ones.

- No auth          -> 401 (the auth dependency rejects before the body runs).
- Authenticated user -> 403 on admin-only endpoints; admitted on user endpoints.
- Admin            -> admitted everywhere.

"Admitted" means "auth did not reject" (status not in {401, 403}); the body itself
runs against benign service mocks, so a 200/404/422/500 from the mock all count as
auth-passed. This test owns the auth posture; route unit tests own body behaviour.

Service providers are overridden with non-raising mocks so dependency resolution
never 500s before the auth dependency is evaluated (which would mask a 401).
SSE stream endpoints are covered separately in ``test_sse_auth.py`` (their infinite
generators can't be driven through ``TestClient`` for the admitted case).
"""

from unittest.mock import AsyncMock

from fastapi import APIRouter, FastAPI, HTTPException

from api.v1.routes import download_client as download_client_routes
from api.v1.routes import downloads as downloads_routes
from api.v1.routes import downloads_search as downloads_search_routes
from api.v1.routes import following as following_routes
from api.v1.routes import library as library_routes
from api.v1.routes import discovery_batches as discovery_batches_routes
from api.v1.routes import library_scan as library_scan_routes
from api.v1.routes import me_connections as me_routes
from api.v1.routes import playlists as playlists_routes
from api.v1.routes import quarantine as quarantine_routes
from api.v1.routes import settings as settings_routes
from api.v1.routes import spotify as spotify_routes
from api.v1.routes import system as system_routes
from api.v1.routes import tracks as tracks_routes
from core.dependencies import (
    get_auth_store,
    get_cache,
    get_discovery_batch_service,
    get_download_client_repository,
    get_download_service,
    get_download_store,
    get_follow_service,
    get_lastfm_auth_service,
    get_library_manager,
    get_library_scanner,
    get_library_service,
    get_now_playing_service,
    get_per_user_client_factory,
    get_personal_mix_service,
    get_playlist_service,
    get_preferences_service,
    get_request_service,
    get_scan_state_store,
    get_settings_service,
    get_spotify_import_service,
    get_sse_publisher,
    get_user_connections_store,
    get_user_listening_prefs_store,
    get_user_section_prefs_store,
)
from middleware import _get_current_admin, _get_current_user
from tests.helpers import build_test_client, mock_admin_user, mock_user

_SERVICE_PROVIDERS = (
    get_auth_store,
    get_cache,
    get_discovery_batch_service,
    get_download_client_repository,
    get_download_service,
    get_download_store,
    get_follow_service,
    get_lastfm_auth_service,
    get_library_manager,
    get_library_scanner,
    get_library_service,
    get_now_playing_service,
    get_per_user_client_factory,
    get_personal_mix_service,
    get_playlist_service,
    get_preferences_service,
    get_request_service,
    get_scan_state_store,
    get_settings_service,
    get_spotify_import_service,
    get_sse_publisher,
    get_user_connections_store,
    get_user_listening_prefs_store,
    get_user_section_prefs_store,
)

# (method, path, body-or-None). Path params use dummy values; bodies are valid so
# body-validation never preempts the auth check with a 422.
_ADMIN_ENDPOINTS = [
    ("POST", "/api/v1/library/scan/start", None),
    ("POST", "/api/v1/library/scan/cancel", None),
    ("GET", "/api/v1/library/scan/unmatched", None),
    ("POST", "/api/v1/library/scan/unmatched/1/resolve", {"resolution": "reject"}),
    ("POST", "/api/v1/library/scan/unmatched/resolve-batch",
     {"release_group_mbid": "rg-1", "items": []}),
    ("GET", "/api/v1/download-client/config", None),
    ("PUT", "/api/v1/download-client/config", {}),
    ("POST", "/api/v1/download-client/test", {}),
    ("GET", "/api/v1/downloads/quarantine", None),
    ("DELETE", "/api/v1/downloads/quarantine/1", None),
    ("POST", "/api/v1/downloads/task-1/reimport", None),
    ("GET", "/api/v1/library/tracks/file-1/tags", None),
    ("POST", "/api/v1/library/tracks/file-1",
     {"title": "t", "artist": "a", "album": "al", "track_number": 1}),
    ("POST", "/api/v1/library/albums/rg-1/rescan", None),
    # Spotify app credentials + home settings (admin-gated at the /settings router level).
    ("GET", "/api/v1/settings/spotify", None),
    ("PUT", "/api/v1/settings/spotify", {}),
    ("GET", "/api/v1/settings/home", None),
    ("PUT", "/api/v1/settings/home", {}),
]

_USER_ENDPOINTS = [
    ("GET", "/api/v1/library/scan/status", None),
    ("GET", "/api/v1/download-client/status", None),
    ("GET", "/api/v1/downloads", None),
    ("GET", "/api/v1/downloads/task-1/files", None),
    ("POST", "/api/v1/downloads/task-1/cancel", None),
    ("POST", "/api/v1/downloads/task-1/retry", None),
    ("GET", "/api/v1/downloads/task-1", None),
    ("POST", "/api/v1/downloads/search/album", {"artist_name": "A", "album_title": "B"}),
    ("GET", "/api/v1/downloads/search/job-1", None),
    ("POST", "/api/v1/downloads/search/job-1/pick", {"candidate_index": 0}),
    ("POST", "/api/v1/downloads/search/job-1/cancel", None),
    ("POST", "/api/v1/tracks/rec-1/request", {"artist_name": "A", "track_title": "T"}),
    ("GET", "/api/v1/library/artists", None),
    ("GET", "/api/v1/library/albums", None),
    ("GET", "/api/v1/library/tracks", None),
    ("GET", "/api/v1/library/stats", None),
    ("GET", "/api/v1/library/albums/rg-1/tracks", None),
    ("GET", "/api/v1/library/albums/rg-1/status", None),
    ("GET", "/api/v1/me/section-prefs", None),
    ("POST", "/api/v1/me/personal-mix/refresh", None),
    ("PUT", "/api/v1/me/section-prefs", {"page": "home", "sections": []}),
    ("GET", "/api/v1/discover/batches", None),
    ("POST", "/api/v1/discover/batches",
     {"name": "b", "items": [{"release_group_mbid": "rg-1"}]}),
    ("GET", "/api/v1/discover/batches/b-1", None),
    ("DELETE", "/api/v1/discover/batches/b-1", None),
    ("GET", "/api/v1/system/health", None),
    # Spotify per-user linking + browsing, and request-missing on an owned playlist.
    # (POST /me/spotify/playlists/{id}/import is intentionally omitted: it spawns a real
    # background task through the DI getters that can't be driven by the mock harness; it
    # shares the same CurrentUserDep gate as GET /me/spotify/playlists, covered here.)
    ("GET", "/api/v1/me/connections/spotify/auth/url", None),
    ("GET", "/api/v1/me/spotify/playlists", None),
    ("POST", "/api/v1/playlists/pl-1/request-missing", None),
    # Following hub. GET /following/events is omitted: it's an SSE stream whose
    # infinite generator can't be driven through TestClient for the admitted case.
    ("GET", "/api/v1/following/artists", None),
    ("GET", "/api/v1/following/new-releases", None),
    ("GET", "/api/v1/following/new-releases/unseen-count", None),
    ("POST", "/api/v1/following/new-releases/seen", None),
]

_ALL_ENDPOINTS = _ADMIN_ENDPOINTS + _USER_ENDPOINTS


def _deny_admin():
    raise HTTPException(status_code=403, detail="Admin access required")


def _client(scenario: str):
    """scenario in {"none", "user", "admin"}."""
    app = FastAPI()
    v1 = APIRouter(prefix="/api/v1")
    # Registration order mirrors main.py: literal /downloads/{quarantine,search}/*
    # routers MUST precede the /downloads/{task_id} catch-all.
    for router in (
        library_scan_routes.router,
        download_client_routes.router,
        quarantine_routes.router,
        downloads_search_routes.router,
        downloads_routes.router,
        following_routes.router,
        tracks_routes.router,
        library_routes.router,
        me_routes.router,
        discovery_batches_routes.router,
        system_routes.router,
        playlists_routes.router,
        settings_routes.router,
        spotify_routes.router,
    ):
        v1.include_router(router)
    app.include_router(v1)

    for provider in _SERVICE_PROVIDERS:
        app.dependency_overrides[provider] = lambda: AsyncMock()

    if scenario == "user":
        app.dependency_overrides[_get_current_user] = lambda: mock_user(role="user", user_id="user-1")
        app.dependency_overrides[_get_current_admin] = _deny_admin
    elif scenario == "admin":
        app.dependency_overrides[_get_current_user] = mock_admin_user
        app.dependency_overrides[_get_current_admin] = mock_admin_user
    # "none": no auth overrides -> real deps read request.state.user (unset) -> 401
    return build_test_client(app)


def _send(client, method: str, path: str, body):
    if body is None:
        return client.request(method, path)
    return client.request(method, path, json=body)


def test_every_endpoint_rejects_unauthenticated():
    client = _client("none")
    failures = []
    for method, path, body in _ALL_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status != 401:
            failures.append(f"{method} {path} -> {status} (expected 401)")
    assert not failures, "unauthenticated requests not rejected:\n" + "\n".join(failures)


def test_admin_endpoints_forbid_regular_users():
    client = _client("user")
    failures = []
    for method, path, body in _ADMIN_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status != 403:
            failures.append(f"{method} {path} -> {status} (expected 403)")
    assert not failures, "admin endpoints not forbidden to users:\n" + "\n".join(failures)


def test_user_endpoints_admit_regular_users():
    client = _client("user")
    failures = []
    for method, path, body in _USER_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status in (401, 403):
            failures.append(f"{method} {path} -> {status} (auth rejected an allowed user)")
    assert not failures, "user endpoints wrongly rejected a user:\n" + "\n".join(failures)


def test_admin_admitted_everywhere():
    client = _client("admin")
    failures = []
    for method, path, body in _ALL_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status in (401, 403):
            failures.append(f"{method} {path} -> {status} (auth rejected admin)")
    assert not failures, "admin wrongly rejected:\n" + "\n".join(failures)
