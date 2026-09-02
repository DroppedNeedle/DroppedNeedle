"""ALLOW_PASSWORD_LOGIN: SSO-only enforcement.

Covers the three surfaces the switch has to hold: env parsing (and that the
on-disk config file cannot override it), the credential-accepting routes, and
the provider list the login page renders from.
"""

from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace

import pytest
from fastapi import FastAPI

import core.config as config_module
from api.v1.routes.auth import router as auth_router
from api.v1.routes.profile import router as profile_router
from core.config import Settings
from core.dependencies.auth_providers import get_auth_service
from infrastructure.persistence.auth_store import AuthStore, UserRecord
from middleware import _get_current_user
from services.auth_service import AuthService
from services.oidc_user_auth_service import OIDCUserAuthService
from tests.helpers import build_test_client

PASSWORD = "correct horse battery staple"


@pytest.fixture(autouse=True)
def _no_hibp(monkeypatch):
    async def _noop(_password: str) -> None:
        return None

    monkeypatch.setattr("services.auth_service._check_hibp", _noop)


@pytest.fixture
def set_password_login(monkeypatch):
    """Flip the switch on the live settings singleton for the duration of a test."""

    def _set(enabled: bool) -> None:
        monkeypatch.setattr(
            config_module.get_settings(), "allow_password_login", enabled
        )

    return _set


def _app(tmp_path) -> tuple[FastAPI, AuthService, UserRecord]:
    store = AuthStore(tmp_path / "library.db")
    service = AuthService(store)
    user, _token = asyncio.run(
        service.create_first_admin(
            display_name="Jane", username="jane", password=PASSWORD
        )
    )
    app = FastAPI()
    app.include_router(auth_router)
    app.include_router(profile_router)
    app.dependency_overrides[get_auth_service] = lambda: service
    app.dependency_overrides[_get_current_user] = lambda: user
    return app, service, user


# --- environment parsing -----------------------------------------------------


def test_defaults_to_enabled(monkeypatch):
    monkeypatch.delenv("ALLOW_PASSWORD_LOGIN", raising=False)
    assert Settings().allow_password_login is True


@pytest.mark.parametrize("raw", ["false", "False", "0", "no"])
def test_env_disables(monkeypatch, raw):
    monkeypatch.setenv("ALLOW_PASSWORD_LOGIN", raw)
    assert Settings().allow_password_login is False


def test_config_file_cannot_re_enable(monkeypatch, tmp_path, caplog):
    """A write to config.json must not override the environment for this switch.

    config.json is loaded after the env and is writable by anything with access
    to the config volume, so honouring it would be a silent bypass.
    """
    monkeypatch.setenv("ALLOW_PASSWORD_LOGIN", "false")
    config_path = tmp_path / "config.json"
    config_path.write_text(json.dumps({"allow_password_login": True}))

    settings = Settings(config_file_path=config_path)
    settings.load_from_file()

    assert settings.allow_password_login is False
    assert "environment-only" in caplog.text


# --- routes ------------------------------------------------------------------


def test_login_rejected_when_disabled(tmp_path, set_password_login):
    app, _service, _user = _app(tmp_path)
    set_password_login(False)
    client = build_test_client(app)

    response = client.post(
        "/auth/login", json={"username": "jane", "password": PASSWORD}
    )

    assert response.status_code == 403
    # the guard runs before the credential is checked, so a valid password is
    # refused exactly like an invalid one - no oracle
    wrong = client.post("/auth/login", json={"username": "jane", "password": "nope"})
    assert wrong.status_code == 403


def test_login_still_works_when_enabled(tmp_path, set_password_login):
    app, _service, _user = _app(tmp_path)
    set_password_login(True)
    client = build_test_client(app)

    response = client.post(
        "/auth/login", json={"username": "jane", "password": PASSWORD}
    )

    assert response.status_code == 200
    assert response.json()["token"]


@pytest.mark.parametrize(
    "path,body",
    [
        (
            "/auth/password-recovery/reset",
            {"username": "jane", "code": "x", "new_password": PASSWORD},
        ),
        (
            "/profile/password",
            {"current_password": PASSWORD, "new_password": PASSWORD},
        ),
        ("/profile/set-password", {"new_password": PASSWORD}),
    ],
)
def test_password_writing_routes_rejected_when_disabled(
    tmp_path, set_password_login, path, body
):
    """No path may mint a local credential while the switch is off, or an
    operator could quietly restore the surface they just closed."""
    app, _service, _user = _app(tmp_path)
    set_password_login(False)
    client = build_test_client(app)

    assert client.post(path, json=body).status_code == 403


def test_setup_remains_open_when_disabled(tmp_path, set_password_login):
    """Bootstrap is the deliberate exception: /setup self-closes with 409 once a
    user exists, and OIDC first-login provisions User, not Admin."""
    store = AuthStore(tmp_path / "library.db")
    service = AuthService(store)
    app = FastAPI()
    app.include_router(auth_router)
    app.dependency_overrides[get_auth_service] = lambda: service
    set_password_login(False)
    client = build_test_client(app)

    response = client.post(
        "/auth/setup",
        json={"display_name": "Jane", "username": "jane", "password": PASSWORD},
    )

    assert response.status_code == 201


# --- provider list -----------------------------------------------------------


@pytest.mark.parametrize("enabled", [True, False])
def test_providers_reports_local_from_switch(set_password_login, enabled):
    set_password_login(enabled)
    prefs = SimpleNamespace(
        get_jellyfin_connection=lambda: SimpleNamespace(login_enabled=False),
        get_plex_connection=lambda: SimpleNamespace(login_enabled=False),
        get_oidc_connection=lambda: SimpleNamespace(
            enabled=True, issuer="https://idp.example.com", client_id="dn"
        ),
    )
    service = OIDCUserAuthService(auth_store=None, preferences_service=prefs, cache=None)

    providers = service.get_enabled_providers()

    assert providers.local is enabled
    assert providers.oidc is True
