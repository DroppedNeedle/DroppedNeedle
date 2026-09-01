"""Phase 1 (AuthMultiUser D3) route-level tests: /setup, /login, /admin/users, /me
exercised through the real auth router with a temp AuthStore."""

from __future__ import annotations

import asyncio
import hashlib
import sqlite3
from types import SimpleNamespace

import pytest
from fastapi import FastAPI, Request
from fastapi.testclient import TestClient
from starlette.middleware.base import BaseHTTPMiddleware

from api.v1.routes.auth import router
from core.dependencies import get_preferences_service
from core.dependencies.auth_providers import get_auth_service
from infrastructure.persistence.auth_store import AuthStore, TokenRecord, UserRecord
from middleware import (
    AuthMiddleware,
    _get_current_admin,
    _get_current_token,
    _get_current_user,
)
from services.auth_service import AuthService
from core.base_path import BasePathMiddleware
from tests.helpers import (
    build_test_client,
    add_production_exception_handlers,
    mock_admin_user,
    mock_user,
)

PASSWORD = "correct horse battery staple"


@pytest.fixture(autouse=True)
def _no_hibp(monkeypatch):
    async def _noop(_password: str) -> None:
        return None

    monkeypatch.setattr("services.auth_service._check_hibp", _noop)


class _StateUserMiddleware(BaseHTTPMiddleware):
    """Inject a verified user onto request.state, the way AuthMiddleware would, so
    the real _get_current_admin / _get_current_user gate runs against it."""

    def __init__(self, app, user: UserRecord) -> None:
        super().__init__(app)
        self._user = user

    async def dispatch(self, request, call_next):
        request.state.user = self._user
        request.state.token = None
        return await call_next(request)


def _app(tmp_path) -> tuple[FastAPI, AuthService]:
    store = AuthStore(tmp_path / "library.db")
    service = AuthService(store)
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_auth_service] = lambda: service
    return app, service


def _override_authenticated_session(
    app: FastAPI, service: AuthService, user: UserRecord, raw_token: str
) -> TokenRecord:
    verified = asyncio.run(service.verify_token(raw_token))
    assert verified is not None
    verified_user, token = verified
    assert verified_user.id == user.id
    app.dependency_overrides[_get_current_user] = lambda: user
    app.dependency_overrides[_get_current_token] = lambda: token
    return token


def test_standard_session_can_mint_companion_session(tmp_path):
    app, service = _app(tmp_path)
    user, account_token = asyncio.run(
        service.create_first_admin(
            display_name="Jane",
            username="jane",
            password=PASSWORD,
        )
    )
    source_token = _override_authenticated_session(app, service, user, account_token)
    client = build_test_client(app)

    response = client.post(
        "/auth/device-sessions",
        json={"device_name": "  Kyle   Apple Watch Ultra  "},
    )

    assert response.status_code == 200
    assert response.json()["token"]
    sessions = asyncio.run(service.list_sessions(user.id))
    companion = next(session for session in sessions if session.id != source_token.id)
    assert companion.session_kind == "companion"
    assert companion.user_agent == "DroppedNeedle companion · Kyle Apple Watch Ultra"
    projected = client.get("/auth/sessions")
    assert projected.status_code == 200
    assert {session["session_kind"] for session in projected.json()["sessions"]} == {
        "standard",
        "companion",
    }


def test_device_session_is_no_store_and_persists_only_raw_token_hash(tmp_path):
    app, service = _app(tmp_path)
    user, account_token = asyncio.run(
        service.create_first_admin(
            display_name="Jane",
            username="jane",
            password=PASSWORD,
        )
    )
    _override_authenticated_session(app, service, user, account_token)
    client = build_test_client(app)

    response = client.post(
        "/auth/device-sessions",
        json={"device_name": "Kyle Apple Watch Ultra"},
    )

    assert response.status_code == 200
    assert response.headers["cache-control"] == "no-store"
    raw_token = response.json()["token"]
    assert raw_token
    listed = client.get("/auth/sessions")
    assert listed.status_code == 200
    assert raw_token not in listed.text
    with sqlite3.connect(tmp_path / "library.db") as connection:
        hashes = {
            row[0] for row in connection.execute("SELECT token_hash FROM auth_tokens")
        }
    assert hashlib.sha256(raw_token.encode()).hexdigest() in hashes
    assert raw_token not in hashes


def test_companion_session_cannot_mint_another_companion(tmp_path):
    app, service = _app(tmp_path)
    user, account_token = asyncio.run(
        service.create_first_admin(
            display_name="Jane",
            username="jane",
            password=PASSWORD,
        )
    )
    _override_authenticated_session(app, service, user, account_token)
    client = build_test_client(app)
    created = client.post(
        "/auth/device-sessions",
        json={"device_name": "Kyle Apple Watch Ultra"},
    )
    companion_raw = created.json()["token"]
    verified = asyncio.run(service.verify_token(companion_raw))
    assert verified is not None
    app.dependency_overrides[_get_current_token] = lambda: verified[1]

    denied = client.post(
        "/auth/device-sessions",
        json={"device_name": "Another Watch"},
    )

    assert denied.status_code == 403
    assert asyncio.run(service.verify_token(companion_raw)) is not None
    assert len(asyncio.run(service.list_sessions(user.id))) == 2


def test_cross_user_cannot_revoke_companion_session(tmp_path):
    app, service = _app(tmp_path)
    owner, account_token = asyncio.run(
        service.create_first_admin(
            display_name="Jane",
            username="jane",
            password=PASSWORD,
        )
    )
    _override_authenticated_session(app, service, owner, account_token)
    client = build_test_client(app)
    created = client.post(
        "/auth/device-sessions",
        json={"device_name": "Kyle Apple Watch Ultra"},
    )
    companion_raw = created.json()["token"]
    companion = next(
        session
        for session in asyncio.run(service.list_sessions(owner.id))
        if session.session_kind == "companion"
    )
    other_user = asyncio.run(
        service.admin_create_user(
            display_name="Alex",
            username="alex",
            password=PASSWORD,
        )
    )
    app.dependency_overrides[_get_current_user] = lambda: other_user

    denied = client.delete(f"/auth/sessions/{companion.id}")

    assert denied.status_code == 403
    assert asyncio.run(service.verify_token(companion_raw)) is not None


def test_ordinary_session_with_companion_label_is_not_revoked(tmp_path):
    app, service = _app(tmp_path)
    label = "DroppedNeedle companion · Kyle Apple Watch Ultra"
    user, account_token = asyncio.run(
        service.create_first_admin(
            display_name="Jane",
            username="jane",
            password=PASSWORD,
            user_agent=label,
        )
    )
    source_token = _override_authenticated_session(app, service, user, account_token)
    client = build_test_client(app)

    created = client.post(
        "/auth/device-sessions",
        json={"device_name": "Kyle Apple Watch Ultra"},
    )

    assert created.status_code == 200
    loaded_source = asyncio.run(service.verify_token(account_token))
    assert loaded_source is not None
    assert loaded_source[1].id == source_token.id
    assert loaded_source[1].session_kind == "standard"
    assert asyncio.run(service.verify_token(created.json()["token"])) is not None
    sessions = asyncio.run(service.list_sessions(user.id))
    assert len(sessions) == 2
    assert {session.session_kind for session in sessions} == {"standard", "companion"}


def test_device_session_rejects_empty_or_unbounded_label(tmp_path):
    app, service = _app(tmp_path)
    user, account_token = asyncio.run(
        service.create_first_admin(
            display_name="Jane",
            username="jane",
            password=PASSWORD,
        )
    )
    _override_authenticated_session(app, service, user, account_token)
    client = build_test_client(app)

    empty = client.post("/auth/device-sessions", json={"device_name": " \t "})
    unbounded = client.post(
        "/auth/device-sessions",
        json={"device_name": "x" * 81},
    )

    assert empty.status_code == 400
    assert unbounded.status_code == 400
    assert len(asyncio.run(service.list_sessions(user.id))) == 1


def test_same_label_replacement_invalidates_old_companion_bearer(tmp_path):
    app, service = _app(tmp_path)
    user, account_token = asyncio.run(
        service.create_first_admin(
            display_name="Jane",
            username="jane",
            password=PASSWORD,
        )
    )
    _override_authenticated_session(app, service, user, account_token)
    client = build_test_client(app)
    original = client.post(
        "/auth/device-sessions",
        json={"device_name": "Kyle Apple Watch Ultra"},
    )
    replacement = client.post(
        "/auth/device-sessions",
        json={"device_name": "Kyle Apple Watch Ultra"},
    )

    assert original.status_code == 200
    assert replacement.status_code == 200
    old_raw = original.json()["token"]
    new_raw = replacement.json()["token"]
    assert new_raw != old_raw
    assert asyncio.run(service.verify_token(old_raw)) is None
    assert asyncio.run(service.verify_token(new_raw)) is not None
    assert len(asyncio.run(service.list_sessions(user.id))) == 2


def test_setup_accepts_username_with_optional_email_omitted(tmp_path):
    app, _ = _app(tmp_path)
    client = build_test_client(app)

    resp = client.post(
        "/auth/setup",
        json={"display_name": "Jane", "username": "Jane", "password": PASSWORD},
    )
    assert resp.status_code == 201
    user = resp.json()["user"]
    assert user["username"] == "jane"
    assert user["username_display"] == "Jane"
    assert user["email"] is None


def test_setup_surfaces_specific_username_error(tmp_path):
    """First-admin setup returns the actionable RegistrationError, not a generic string."""
    app, _ = _app(tmp_path)
    client = build_test_client(app)

    resp = client.post(
        "/auth/setup",
        json={"display_name": "Jane", "username": "no", "password": PASSWORD},
    )
    assert resp.status_code == 400
    assert resp.json()["error"]["message"] == "Invalid username"


def test_setup_surfaces_breached_password_reason(tmp_path, monkeypatch):
    """A password rejected by the breach check reaches the user verbatim (not swallowed)."""
    from core.exceptions import RegistrationError

    async def _breached(_password: str) -> None:
        raise RegistrationError(
            "This password has appeared in a known data breach. Please choose a different password."
        )

    monkeypatch.setattr("services.auth_service._check_hibp", _breached)

    app, _ = _app(tmp_path)
    client = build_test_client(app)

    resp = client.post(
        "/auth/setup",
        json={"display_name": "Jane", "username": "jane", "password": PASSWORD},
    )
    assert resp.status_code == 400
    assert "known data breach" in resp.json()["error"]["message"]


def test_login_by_username_mixed_case_and_generic_401(tmp_path):
    app, _ = _app(tmp_path)
    client = build_test_client(app)
    client.post(
        "/auth/setup",
        json={"display_name": "Jane", "username": "Jane.Doe", "password": PASSWORD},
    )

    ok = client.post("/auth/login", json={"username": "JANE.DOE", "password": PASSWORD})
    assert ok.status_code == 200
    assert ok.json()["user"]["username"] == "jane.doe"

    bad_pw = client.post(
        "/auth/login", json={"username": "jane.doe", "password": "nope-nope-nope"}
    )
    assert bad_pw.status_code == 401
    assert bad_pw.json()["error"]["message"] == "Invalid username or password"

    # Unknown username must not 500 (dummy-verify path).
    unknown = client.post(
        "/auth/login", json={"username": "ghost", "password": PASSWORD}
    )
    assert unknown.status_code == 401


def test_me_returns_username_fields(tmp_path):
    app, _ = _app(tmp_path)
    app.dependency_overrides[_get_current_user] = lambda: UserRecord(
        id="u",
        display_name="Jane",
        role="user",
        created_at="t",
        username="jane",
        username_display="Jane",
    )
    client = build_test_client(app)

    resp = client.get("/auth/me")
    assert resp.status_code == 200
    body = resp.json()
    assert body["username"] == "jane"
    assert body["username_display"] == "Jane"


def test_me_includes_musicbrainz_source_for_non_admin_session_user(tmp_path):
    app, _ = _app(tmp_path)
    app.dependency_overrides[_get_current_user] = lambda: UserRecord(
        id="u",
        display_name="Jane",
        role="user",
        created_at="t",
        username="jane",
        username_display="Jane",
    )
    app.dependency_overrides[get_preferences_service] = lambda: SimpleNamespace(
        get_musicbrainz_connection=lambda: SimpleNamespace(
            source_mode="mirror",
            source_id="mirror-u",
            generation=7,
        )
    )

    body = build_test_client(app).get("/auth/me")

    assert body.status_code == 200
    assert body.json()["musicbrainz_source"] == {
        "source_mode": "mirror",
        "source_id": "mirror-u",
        "generation": 7,
    }


def test_admin_create_user_with_username_and_duplicate_conflict(tmp_path):
    app, _ = _app(tmp_path)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    client = build_test_client(app)

    created = client.post(
        "/auth/admin/users",
        json={
            "display_name": "Bob",
            "username": "Bob",
            "password": PASSWORD,
            "role": "user",
        },
    )
    assert created.status_code == 201
    body = created.json()
    assert body["username"] == "bob"
    assert body["username_display"] == "Bob"
    assert body["email"] is None

    dup = client.post(
        "/auth/admin/users",
        json={"display_name": "Bob2", "username": "BOB", "password": PASSWORD},
    )
    assert dup.status_code == 409


def test_admin_create_user_forbidden_for_non_admin(tmp_path):
    app, _ = _app(tmp_path)
    app.add_middleware(_StateUserMiddleware, user=mock_user(role="user"))
    client = build_test_client(app)

    resp = client.post(
        "/auth/admin/users",
        json={"display_name": "Bob", "username": "bob", "password": PASSWORD},
    )
    assert resp.status_code == 403


def test_admin_generates_code_and_public_route_resets_password(tmp_path):
    app, auth = _app(tmp_path)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    client = build_test_client(app)
    user = client.post(
        "/auth/admin/users",
        json={
            "display_name": "Bob",
            "username": "bob",
            "password": PASSWORD,
            "role": "user",
        },
    ).json()

    generated = client.post(f"/auth/admin/users/{user['id']}/password-recovery")
    assert generated.status_code == 200
    assert generated.headers["cache-control"] == "no-store"
    recovery_code = generated.json()["recovery_code"]

    reset = client.post(
        "/auth/password-recovery/reset",
        json={
            "username": "Bob",
            "recovery_code": recovery_code,
            "new_password": "another correct staple value",
        },
    )
    assert reset.status_code == 204
    recovered, _ = asyncio.run(
        auth.login_local(username="bob", password="another correct staple value")
    )
    assert recovered.id == user["id"]


def test_password_recovery_route_returns_generic_error(tmp_path):
    app, _ = _app(tmp_path)
    client = build_test_client(app)

    response = client.post(
        "/auth/password-recovery/reset",
        json={
            "username": "unknown",
            "recovery_code": "WRONG-WRONG-WRONG-WRONG-WRONG",
            "new_password": "another correct staple value",
        },
    )
    assert response.status_code == 400
    assert response.json()["error"]["message"] == "Invalid or expired recovery code"
    assert AuthMiddleware._is_public("/api/v1/auth/password-recovery/reset")


def test_password_recovery_rejects_passwords_over_bcrypt_limit(tmp_path):
    app, _ = _app(tmp_path)
    client = build_test_client(app)

    response = client.post(
        "/auth/password-recovery/reset",
        json={
            "username": "unknown",
            "recovery_code": "WRONG-WRONG-WRONG-WRONG-WRONG",
            "new_password": "a" * 73,
        },
    )
    assert response.status_code == 400
    assert (
        response.json()["error"]["message"]
        == "Password is too long. Use 72 UTF-8 bytes or fewer."
    )


def test_password_recovery_code_generation_forbidden_for_non_admin(tmp_path):
    app, _ = _app(tmp_path)
    app.add_middleware(_StateUserMiddleware, user=mock_user(role="user"))
    client = build_test_client(app)

    response = client.post("/auth/admin/users/user-1/password-recovery")
    assert response.status_code == 403


def _seed_admin(client) -> None:
    resp = client.post(
        "/auth/setup",
        json={"display_name": "Jane", "username": "jane", "password": PASSWORD},
    )
    assert resp.status_code == 201


def test_session_cookie_scopes_to_api_root_without_base(tmp_path):
    """Hosted at the domain root the session cookie must stay scoped to /api."""
    app, _ = _app(tmp_path)
    client = build_test_client(app)
    _seed_admin(client)

    login = client.post("/auth/login", json={"username": "jane", "password": PASSWORD})
    assert login.status_code == 200
    assert "droppedneedle_session=" in login.headers["set-cookie"]
    assert "Path=/api;" in login.headers["set-cookie"]

    logout = client.post("/auth/logout")
    assert logout.status_code == 204
    assert 'droppedneedle_session=""' in logout.headers["set-cookie"]
    assert "Path=/api;" in logout.headers["set-cookie"]


def test_session_cookie_scopes_to_base_api_and_logout_clears_same_path(tmp_path):
    """Under BASE_PATH=/music the cookie lives at /music/api only.

    A root-relative ``Path=/api`` cookie would be withheld from the intended
    ``/music/api/v1/*`` endpoints yet still ride along to any co-hosted
    domain-root application exposing ``/api``. Logout must delete against the
    identical scoped path or the browser keeps a stale session forever.
    """
    app, _ = _app(tmp_path)

    @app.get("/api/v1/auth/cookie-probe")
    async def _cookie_probe(request: Request) -> dict:
        return {"session": request.cookies.get("droppedneedle_session")}

    sent_cookies: list[bytes] = []

    def _capture_cookie_header(inner):
        async def record(scope, receive, send):
            for header_name, header_value in scope.get("headers", []):
                if header_name == b"cookie":
                    sent_cookies.append(header_value)
            await inner(scope, receive, send)

        return record

    add_production_exception_handlers(app)
    client = TestClient(
        _capture_cookie_header(BasePathMiddleware(app, "/music")),
        raise_server_exceptions=False,
    )

    setup = client.post(
        "/music/auth/setup",
        json={
            "display_name": "Jane",
            "username": "jane",
            "password": PASSWORD,
        },
    )
    assert setup.status_code == 201

    login = client.post(
        "/music/auth/login", json={"username": "jane", "password": PASSWORD}
    )
    assert login.status_code == 200
    login_cookie = login.headers["set-cookie"]
    assert "droppedneedle_session=" in login_cookie
    assert "Path=/music/api;" in login_cookie
    assert "Path=/api;" not in login_cookie
    raw_token = login.json()["token"]

    # Inside the base: the scoped cookie follows requests to <base>/api paths.
    inside = client.get("/music/api/v1/auth/cookie-probe")
    assert inside.status_code == 200
    assert inside.json()["session"] == raw_token

    # Domain-root surface under the same host: the browser path-match rule
    # (RFC 6265 Path=/music/api) must withhold the session cookie entirely.
    sent_cookies.clear()
    outside = client.get("/api/v1/auth/sessions")
    assert outside.status_code in (401, 404)
    assert b"droppedneedle_session=" not in "\n".join(sent_cookies).encode()

    logout = client.post("/music/auth/logout")
    assert logout.status_code == 204
    delete_cookie = logout.headers["set-cookie"]
    assert 'droppedneedle_session=""' in delete_cookie
    assert "Max-Age=0" in delete_cookie
    assert "Path=/music/api;" in delete_cookie
    assert "Path=/api;" not in delete_cookie
