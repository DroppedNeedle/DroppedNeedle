import asyncio
from datetime import datetime, timedelta, timezone
from unittest.mock import AsyncMock

import pytest

from infrastructure.persistence.auth_store import TokenRecord, UserRecord
from services.auth_service import AuthService


def _token(*, last_seen_at: str) -> TokenRecord:
    return TokenRecord(
        id="token-1",
        user_id="user-1",
        token_hash="hash",
        issued_at="2026-01-01T00:00:00+00:00",
        expires_at="2027-01-01T00:00:00+00:00",
        last_seen_at=last_seen_at,
        revoked=False,
    )


def _user() -> UserRecord:
    return UserRecord(
        id="user-1",
        display_name="User",
        role="user",
        created_at="2026-01-01T00:00:00+00:00",
    )


@pytest.mark.asyncio
async def test_verify_token_does_not_wait_for_activity_write_and_coalesces() -> None:
    release_touch = asyncio.Event()
    store = AsyncMock()
    store.verify_token_with_user.return_value = (
        _user(),
        _token(
            last_seen_at=(datetime.now(timezone.utc) - timedelta(minutes=10)).isoformat()
        ),
    )

    async def blocked_touch(_token_id: str) -> None:
        await release_touch.wait()

    store.touch_token.side_effect = blocked_touch
    service = AuthService(store)

    first = await service.verify_token("raw-token")
    second = await service.verify_token("raw-token")

    assert first is not None
    assert second is not None
    await asyncio.sleep(0)
    store.touch_token.assert_awaited_once_with("token-1")
    release_touch.set()
    await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_recent_token_skips_activity_write() -> None:
    store = AsyncMock()
    store.verify_token_with_user.return_value = (
        _user(),
        _token(last_seen_at=datetime.now(timezone.utc).isoformat()),
    )
    service = AuthService(store)

    assert await service.verify_token("raw-token") is not None
    await asyncio.sleep(0)

    store.touch_token.assert_not_awaited()
