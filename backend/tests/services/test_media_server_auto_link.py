"""SSO auto-link (issue #138, D4): a Jellyfin/Plex login hands us a fresh
user-scoped token, so the login flow also upserts the matching per-user media
connection - and a failed upsert must never fail the login itself."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import AuthenticationError, PlexApiError
from services.jellyfin_user_auth_service import JellyfinUserAuthService
from services.plex_user_auth_service import PlexUserAuthService

_JF_PROFILE = {
    "jellyfin_user_id": "jf-uid-1",
    "username": "alice",
    "email": None,
    "thumb": None,
    "access_token": "jf-tok",
}

_PLEX_PROFILE = {
    "uuid": "px-uid-1",
    "email": "a@example.com",
    "display_name": "Alice Plex",
    "thumb": None,
    "auth_token": "px-tok",
}


def _auth_store() -> MagicMock:
    store = MagicMock()
    store.issue_token = MagicMock(return_value=("raw-token", "token-hash"))
    store.store_token = AsyncMock()
    store.update_last_login = AsyncMock()
    return store


def _user() -> SimpleNamespace:
    return SimpleNamespace(id="user-1", display_name="Alice", role="user")


@pytest.fixture
def jf_service():
    repo = MagicMock()
    repo.is_configured = MagicMock(return_value=True)
    connections = MagicMock()
    connections.upsert = AsyncMock()
    svc = JellyfinUserAuthService(
        auth_store=_auth_store(),
        jellyfin_repository=repo,
        preferences_service=MagicMock(),
        connections_store=connections,
    )
    svc._authenticate_with_jellyfin = AsyncMock(return_value=dict(_JF_PROFILE))
    svc._find_or_create_user = AsyncMock(return_value=_user())
    return svc, connections


@pytest.mark.asyncio
async def test_jellyfin_login_auto_links_connection(jf_service):
    svc, connections = jf_service
    user, token = await svc.login(username="alice", password="pw")
    assert token == "raw-token"
    connections.upsert.assert_awaited_once_with(
        "user-1",
        "jellyfin",
        {"access_token": "jf-tok", "jellyfin_user_id": "jf-uid-1", "username": "alice"},
    )


@pytest.mark.asyncio
async def test_jellyfin_login_survives_auto_link_failure(jf_service):
    svc, connections = jf_service
    connections.upsert.side_effect = RuntimeError("db locked")
    user, token = await svc.login(username="alice", password="pw")
    assert token == "raw-token"


@pytest.mark.asyncio
async def test_jellyfin_login_without_connections_store_still_works():
    repo = MagicMock()
    repo.is_configured = MagicMock(return_value=True)
    svc = JellyfinUserAuthService(
        auth_store=_auth_store(),
        jellyfin_repository=repo,
        preferences_service=MagicMock(),
    )
    svc._authenticate_with_jellyfin = AsyncMock(return_value=dict(_JF_PROFILE))
    svc._find_or_create_user = AsyncMock(return_value=_user())
    _, token = await svc.login(username="alice", password="pw")
    assert token == "raw-token"


@pytest.mark.asyncio
async def test_jellyfin_authenticate_credentials_requires_configured_server():
    repo = MagicMock()
    repo.is_configured = MagicMock(return_value=False)
    svc = JellyfinUserAuthService(
        auth_store=_auth_store(),
        jellyfin_repository=repo,
        preferences_service=MagicMock(),
    )
    with pytest.raises(AuthenticationError):
        await svc.authenticate_credentials("alice", "pw")


@pytest.fixture
def plex_service():
    repo = MagicMock()
    repo.poll_oauth_pin = AsyncMock(return_value="px-tok")
    prefs = MagicMock()
    prefs.get_or_create_setting = MagicMock(return_value="client-1")
    connections = MagicMock()
    connections.upsert = AsyncMock()
    svc = PlexUserAuthService(
        auth_store=_auth_store(),
        plex_repository=repo,
        preferences_service=prefs,
        connections_store=connections,
    )
    svc._get_user_profile = AsyncMock(return_value=dict(_PLEX_PROFILE))
    svc._get_server_machine_id = AsyncMock(return_value="machine-1")
    svc._check_server_membership = AsyncMock(return_value=True)
    svc._get_server_access_token = AsyncMock(return_value="server-tok")
    svc._find_or_create_user = AsyncMock(return_value=_user())
    return svc, repo, connections


@pytest.mark.asyncio
async def test_plex_login_auto_links_connection(plex_service):
    svc, _, connections = plex_service
    result = await svc.poll_and_login(pin_id=1)
    assert result is not None
    connections.upsert.assert_awaited_once_with(
        "user-1",
        "plex",
        {
            "auth_token": "px-tok",
            "server_access_token": "server-tok",
            "plex_user_id": "px-uid-1",
            "username": "Alice Plex",
        },
    )


@pytest.mark.asyncio
async def test_plex_login_survives_auto_link_failure(plex_service):
    svc, _, connections = plex_service
    connections.upsert.side_effect = RuntimeError("db locked")
    result = await svc.poll_and_login(pin_id=1)
    assert result is not None


@pytest.mark.asyncio
async def test_plex_poll_for_link_pending_returns_none(plex_service):
    svc, repo, connections = plex_service
    repo.poll_oauth_pin = AsyncMock(return_value=None)
    assert await svc.poll_for_link(pin_id=1) is None
    connections.upsert.assert_not_awaited()


@pytest.mark.asyncio
async def test_plex_poll_for_link_returns_profile_without_login_side_effects(plex_service):
    svc, _, connections = plex_service
    profile = await svc.poll_for_link(pin_id=1)
    assert profile == {**_PLEX_PROFILE, "server_access_token": "server-tok"}
    # link flow persists nothing itself (the route owns the upsert) and never logs in
    connections.upsert.assert_not_awaited()
    svc._find_or_create_user.assert_not_awaited()


@pytest.mark.asyncio
async def test_plex_poll_for_link_enforces_server_membership(plex_service):
    svc, _, _ = plex_service
    svc._get_server_machine_id = AsyncMock(return_value="machine-1")
    svc._check_server_membership = AsyncMock(return_value=False)
    with pytest.raises(AuthenticationError):
        await svc.poll_for_link(pin_id=1)

@pytest.mark.asyncio
async def test_plex_transient_resources_surface_verify_worded_errors(plex_service):
    # A transient/unverifiable /resources body reaches the service as
    # PlexApiError and must surface as verify-worded errors, never the
    # "does not have access" accusation.
    svc, repo, _ = plex_service
    svc._check_server_membership = PlexUserAuthService._check_server_membership.__get__(svc)
    svc._get_server_access_token = PlexUserAuthService._get_server_access_token.__get__(svc)

    repo.get_account_server_ids = AsyncMock(
        side_effect=PlexApiError("Unexpected Plex /resources response shape")
    )
    with pytest.raises(AuthenticationError, match="Could not verify server access"):
        await svc._check_server_membership("tok", "client-1", "machine-1")

    repo.get_server_access_token = AsyncMock(
        side_effect=PlexApiError("Unexpected Plex /resources response shape")
    )
    with pytest.raises(AuthenticationError, match="Could not verify Plex server access"):
        await svc._get_server_access_token("tok", "client-1", "machine-1")

    repo.get_server_access_token = AsyncMock(return_value=None)
    with pytest.raises(AuthenticationError) as exc_info:
        await svc._get_server_access_token("tok", "client-1", "machine-1")
    assert str(exc_info.value) == "Could not verify Plex server access"


@pytest.mark.asyncio
async def test_plex_genuine_membership_miss_keeps_access_message(plex_service):
    # A parsed list that genuinely lacks the server still yields the access
    # message on the membership path.
    svc, repo, _ = plex_service
    svc._check_server_membership = PlexUserAuthService._check_server_membership.__get__(svc)
    repo.get_account_server_ids = AsyncMock(return_value={"other-machine"})
    with pytest.raises(AuthenticationError, match="does not have access"):
        await svc.poll_for_link(pin_id=1)
