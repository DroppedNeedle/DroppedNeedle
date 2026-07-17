"""AuthStore schema idempotency + spotify_oauth_states roundtrip.

The Spotify OAuth flow (PR #108) added the ``spotify_oauth_states`` table inside
``AuthStore._ensure_tables``; per the house rule every new store/migration gets an
idempotency test (construct twice on the same path).
"""

import hashlib
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from infrastructure.persistence.auth_store import AuthStore


def test_migration_is_idempotent(tmp_path: Path):
    db_path = tmp_path / "library.db"
    lock = threading.Lock()
    AuthStore(db_path, write_lock=lock)
    # Second construction re-runs _ensure_tables (all CREATE TABLE IF NOT EXISTS +
    # guarded ALTERs); it must not raise.
    AuthStore(db_path, write_lock=lock)
    assert db_path.exists()


@pytest.mark.asyncio
async def test_spotify_state_roundtrip_is_single_use(tmp_path: Path):
    store = AuthStore(tmp_path / "auth.db")
    await store.store_spotify_state("state-abc", "user-1")

    assert await store.consume_spotify_state("state-abc") == "user-1"
    # Single-use: the state is deleted on consume, so a replay yields nothing.
    assert await store.consume_spotify_state("state-abc") is None


@pytest.mark.asyncio
async def test_spotify_state_unknown_returns_none(tmp_path: Path):
    store = AuthStore(tmp_path / "auth.db")
    assert await store.consume_spotify_state("never-stored") is None


@pytest.mark.asyncio
async def test_password_recovery_is_single_use_and_revokes_sessions(tmp_path: Path):
    store = AuthStore(tmp_path / "auth.db")
    user = await store.create_user(
        id="user-1",
        display_name="Alice",
        role="admin",
        username="alice",
        username_display="Alice",
    )
    provider = await store.create_auth_provider(
        id="provider-1",
        user_id=user.id,
        provider="local",
        provider_uid="alice",
        provider_data="old-password-data",
    )
    raw_token, token_hash = store.issue_token()
    await store.store_token(id="token-1", user_id=user.id, token_hash=token_hash)

    code_hash = hashlib.sha256(b"RECOVERYCODE").hexdigest()
    await store.store_password_recovery_code(
        user_id=user.id,
        code_hash=code_hash,
        expires_at=(datetime.now(timezone.utc) + timedelta(minutes=15)).isoformat(),
    )

    assert await store.reset_password_with_recovery_code(
        username="alice",
        code_hash=code_hash,
        provider_data="new-password-data",
    )
    changed = await store.get_auth_provider("local", "alice")
    assert changed is not None
    assert changed.id == provider.id
    assert changed.provider_data == "new-password-data"
    assert await store.verify_token(raw_token) is None
    assert not await store.reset_password_with_recovery_code(
        username="alice",
        code_hash=code_hash,
        provider_data="replayed-password-data",
    )


@pytest.mark.asyncio
async def test_new_password_recovery_code_invalidates_previous_code(tmp_path: Path):
    store = AuthStore(tmp_path / "auth.db")
    user = await store.create_user(
        id="user-1",
        display_name="Alice",
        role="user",
        username="alice",
    )
    await store.create_auth_provider(
        id="provider-1",
        user_id=user.id,
        provider="local",
        provider_uid="alice",
        provider_data="old",
    )
    expiry = (datetime.now(timezone.utc) + timedelta(minutes=15)).isoformat()
    await store.store_password_recovery_code(
        user_id=user.id, code_hash="first", expires_at=expiry
    )
    await store.store_password_recovery_code(
        user_id=user.id, code_hash="second", expires_at=expiry
    )

    assert not await store.reset_password_with_recovery_code(
        username="alice", code_hash="first", provider_data="new"
    )
    assert await store.reset_password_with_recovery_code(
        username="alice", code_hash="second", provider_data="new"
    )


@pytest.mark.asyncio
async def test_expired_password_recovery_code_cannot_be_used(tmp_path: Path):
    store = AuthStore(tmp_path / "auth.db")
    user = await store.create_user(
        id="user-1",
        display_name="Alice",
        role="user",
        username="alice",
    )
    await store.create_auth_provider(
        id="provider-1",
        user_id=user.id,
        provider="local",
        provider_uid="alice",
        provider_data="old",
    )
    await store.store_password_recovery_code(
        user_id=user.id,
        code_hash="expired",
        expires_at=(datetime.now(timezone.utc) - timedelta(seconds=1)).isoformat(),
    )

    assert not await store.reset_password_with_recovery_code(
        username="alice", code_hash="expired", provider_data="new"
    )


@pytest.mark.asyncio
async def test_normal_password_change_invalidates_recovery_code_atomically(tmp_path: Path):
    store = AuthStore(tmp_path / "auth.db")
    user = await store.create_user(
        id="user-1",
        display_name="Alice",
        role="user",
        username="alice",
    )
    await store.create_auth_provider(
        id="provider-1",
        user_id=user.id,
        provider="local",
        provider_uid="alice",
        provider_data="old",
    )
    await store.store_password_recovery_code(
        user_id=user.id,
        code_hash="recovery",
        expires_at=(datetime.now(timezone.utc) + timedelta(minutes=15)).isoformat(),
    )

    assert not await store.change_local_password(
        provider_id="provider-1",
        user_id=user.id,
        expected_provider_data="stale",
        provider_data="new",
    )
    assert await store.change_local_password(
        provider_id="provider-1",
        user_id=user.id,
        expected_provider_data="old",
        provider_data="new",
    )
    assert not await store.reset_password_with_recovery_code(
        username="alice",
        code_hash="recovery",
        provider_data="replayed",
    )
