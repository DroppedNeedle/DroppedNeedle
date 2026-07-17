"""Phase 1 (AuthMultiUser D3) route-level tests: /setup, /login, /admin/users, /me
exercised through the real auth router with a temp AuthStore."""

from __future__ import annotations

import asyncio

import pytest
from fastapi import FastAPI
from starlette.middleware.base import BaseHTTPMiddleware

from api.v1.routes.auth import router
from core.dependencies.auth_providers import get_auth_service
from infrastructure.persistence.auth_store import AuthStore, UserRecord
from middleware import AuthMiddleware, _get_current_admin, _get_current_user
from services.auth_service import AuthService
from tests.helpers import build_test_client, mock_admin_user, mock_user

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

    bad_pw = client.post("/auth/login", json={"username": "jane.doe", "password": "nope-nope-nope"})
    assert bad_pw.status_code == 401
    assert bad_pw.json()["error"]["message"] == "Invalid username or password"

    # Unknown username must not 500 (dummy-verify path).
    unknown = client.post("/auth/login", json={"username": "ghost", "password": PASSWORD})
    assert unknown.status_code == 401


def test_me_returns_username_fields(tmp_path):
    app, _ = _app(tmp_path)
    app.dependency_overrides[_get_current_user] = lambda: UserRecord(
        id="u", display_name="Jane", role="user", created_at="t",
        username="jane", username_display="Jane",
    )
    client = build_test_client(app)

    resp = client.get("/auth/me")
    assert resp.status_code == 200
    body = resp.json()
    assert body["username"] == "jane"
    assert body["username_display"] == "Jane"


def test_admin_create_user_with_username_and_duplicate_conflict(tmp_path):
    app, _ = _app(tmp_path)
    app.dependency_overrides[_get_current_admin] = mock_admin_user
    client = build_test_client(app)

    created = client.post(
        "/auth/admin/users",
        json={"display_name": "Bob", "username": "Bob", "password": PASSWORD, "role": "user"},
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
        auth.login_local(
            username="bob", password="another correct staple value"
        )
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
    assert response.json()["error"]["message"] == "Password is too long. Use 72 UTF-8 bytes or fewer."


def test_password_recovery_code_generation_forbidden_for_non_admin(tmp_path):
    app, _ = _app(tmp_path)
    app.add_middleware(_StateUserMiddleware, user=mock_user(role="user"))
    client = build_test_client(app)

    response = client.post("/auth/admin/users/user-1/password-recovery")
    assert response.status_code == 403
