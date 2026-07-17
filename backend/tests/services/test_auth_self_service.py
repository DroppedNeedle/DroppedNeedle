"""Phase 2 (AuthMultiUser D8) service-level self-service tests: change own
username/email/password and set a local password on an SSO-only account, driven
through a real AuthService + temp AuthStore."""

from __future__ import annotations

import asyncio
from datetime import datetime, timezone

import pytest

from core.exceptions import AuthenticationError, RegistrationError
from infrastructure.persistence.auth_store import AuthStore, _derive_username
from services.auth_service import AuthService

PASSWORD = "correct horse battery staple"
PASSWORD2 = "another correct staple value"


@pytest.fixture(autouse=True)
def _no_hibp(monkeypatch):
    async def _noop(_password: str) -> None:
        return None

    monkeypatch.setattr("services.auth_service._check_hibp", _noop)


def _setup(tmp_path) -> tuple[AuthStore, AuthService]:
    store = AuthStore(tmp_path / "library.db")
    return store, AuthService(store)


def test_rename_syncs_local_provider_uid(tmp_path):
    """M3 fix: after a rename, login resolves by the NEW username, not the old one."""
    async def scenario():
        store, auth = _setup(tmp_path)
        user = await auth.admin_create_user(display_name="Alice", username="alice", password=PASSWORD)

        renamed = await auth.update_username(user.id, "Alice2")
        assert renamed.username == "alice2"
        assert renamed.username_display == "Alice2"

        u, _ = await auth.login_local(username="alice2", password=PASSWORD)
        assert u.id == user.id

        with pytest.raises(AuthenticationError):
            await auth.login_local(username="alice", password=PASSWORD)

    asyncio.run(scenario())


def test_update_username_collision_raises(tmp_path):
    async def scenario():
        _store, auth = _setup(tmp_path)
        await auth.admin_create_user(display_name="Alice", username="alice", password=PASSWORD)
        bob = await auth.admin_create_user(display_name="Bob", username="bob", password=PASSWORD)
        with pytest.raises(RegistrationError):
            await auth.update_username(bob.id, "alice")

    asyncio.run(scenario())


def test_change_password_flow(tmp_path):
    async def scenario():
        _store, auth = _setup(tmp_path)
        user = await auth.admin_create_user(display_name="Cara", username="cara", password=PASSWORD)

        with pytest.raises(AuthenticationError):
            await auth.change_password(user.id, "wrong password here", PASSWORD2)

        recovery_code, _ = await auth.create_password_recovery_code(user.id)
        await auth.change_password(user.id, PASSWORD, PASSWORD2)
        u, _ = await auth.login_local(username="cara", password=PASSWORD2)
        assert u.id == user.id
        with pytest.raises(AuthenticationError):
            await auth.login_local(username="cara", password=PASSWORD)

        with pytest.raises(RegistrationError):
            await auth.change_password(user.id, PASSWORD2, "short")
        with pytest.raises(AuthenticationError, match="Invalid or expired"):
            await auth.reset_password_with_recovery_code(
                username="cara",
                recovery_code=recovery_code,
                new_password=PASSWORD,
            )

    asyncio.run(scenario())


def test_password_length_respects_bcrypt_byte_limit(tmp_path):
    async def scenario():
        _store, auth = _setup(tmp_path)
        await auth.admin_create_user(
            display_name="ASCII Limit",
            username="ascii-limit",
            password="a" * 72,
        )
        await auth.admin_create_user(
            display_name="Unicode Limit",
            username="unicode-limit",
            password="é" * 36,
        )
        with pytest.raises(RegistrationError, match="72 UTF-8 bytes"):
            await auth.admin_create_user(
                display_name="ASCII Too Long",
                username="ascii-too-long",
                password="a" * 73,
            )
        with pytest.raises(RegistrationError, match="72 UTF-8 bytes"):
            await auth.admin_create_user(
                display_name="Unicode Too Long",
                username="unicode-too-long",
                password="é" * 37,
            )

    asyncio.run(scenario())


def test_set_local_password_for_sso_only_account(tmp_path):
    async def scenario():
        store, auth = _setup(tmp_path)
        username, display = await _derive_username(store, display_name="SSO User")
        user = await store.create_user(
            id="sso-1", display_name="SSO User", role="user",
            username=username, username_display=display,
        )
        await store.create_auth_provider(
            id="p-jf", user_id=user.id, provider="jellyfin", provider_uid="jf-123",
        )

        await auth.set_local_password(user.id, PASSWORD)

        providers = await store.list_providers_for_user(user.id)
        local = next(p for p in providers if p.provider == "local")
        assert local.provider_uid == username

        u, _ = await auth.login_local(username=username, password=PASSWORD)
        assert u.id == user.id

        # Second call now finds an existing local provider -> rejected.
        with pytest.raises(RegistrationError):
            await auth.set_local_password(user.id, PASSWORD2)

    asyncio.run(scenario())


def test_set_local_password_rejected_when_local_exists(tmp_path):
    async def scenario():
        _store, auth = _setup(tmp_path)
        user = await auth.admin_create_user(display_name="Local", username="localu", password=PASSWORD)
        with pytest.raises(RegistrationError):
            await auth.set_local_password(user.id, PASSWORD2)

    asyncio.run(scenario())


def test_password_recovery_code_resets_password_and_is_single_use(tmp_path):
    async def scenario():
        _store, auth = _setup(tmp_path)
        user = await auth.admin_create_user(
            display_name="Locked Admin",
            username="locked-admin",
            password=PASSWORD,
            role="admin",
        )
        _, existing_token = await auth.login_local(
            username="locked-admin", password=PASSWORD
        )
        code, expires_at = await auth.create_password_recovery_code(user.id)

        assert len(code.split("-")) == 5
        assert datetime.fromisoformat(expires_at) > datetime.now(timezone.utc)

        await auth.reset_password_with_recovery_code(
            username="LOCKED-ADMIN",
            recovery_code=code.lower(),
            new_password=PASSWORD2,
        )
        assert await auth.verify_token(existing_token) is None
        recovered, _ = await auth.login_local(
            username="locked-admin", password=PASSWORD2
        )
        assert recovered.id == user.id
        with pytest.raises(AuthenticationError, match="Invalid or expired"):
            await auth.reset_password_with_recovery_code(
                username="locked-admin",
                recovery_code=code,
                new_password=PASSWORD,
            )

    asyncio.run(scenario())


def test_password_recovery_rejects_unknown_and_sso_only_accounts(tmp_path):
    async def scenario():
        store, auth = _setup(tmp_path)
        sso_user = await store.create_user(
            id="sso-1",
            display_name="SSO User",
            role="user",
            username="sso-user",
        )
        await store.create_auth_provider(
            id="sso-provider",
            user_id=sso_user.id,
            provider="oidc",
            provider_uid="oidc-1",
        )

        with pytest.raises(AuthenticationError, match="not available"):
            await auth.create_password_recovery_code(sso_user.id)
        with pytest.raises(AuthenticationError, match="not available"):
            await auth.create_password_recovery_code_for_username("missing")

    asyncio.run(scenario())


def test_update_email_set_conflict_and_clear(tmp_path):
    async def scenario():
        _store, auth = _setup(tmp_path)
        await auth.admin_create_user(
            display_name="A", username="usera", password=PASSWORD, email="a@example.com",
        )
        b = await auth.admin_create_user(display_name="B", username="userb", password=PASSWORD)

        updated = await auth.update_email(b.id, "b@example.com")
        assert updated.email == "b@example.com"

        # Case-insensitive dedupe against another user's email.
        with pytest.raises(RegistrationError):
            await auth.update_email(b.id, "A@Example.com")

        cleared = await auth.update_email(b.id, "")
        assert cleared.email is None

    asyncio.run(scenario())
