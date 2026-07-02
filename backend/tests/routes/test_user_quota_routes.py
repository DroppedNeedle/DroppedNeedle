"""Admin per-user quota routes (CollectionManagement Feature C)."""

from unittest.mock import AsyncMock

from fastapi import FastAPI

from api.v1.routes import auth as auth_routes
from core.dependencies import get_quota_service
from core.dependencies.auth_providers import get_auth_service
from middleware import _get_current_admin, _get_current_user
from services.quota_service import EffectiveQuota, QuotaUsage
from tests.helpers import build_test_client, mock_admin_user, mock_user


def _usage(user_id: str = "u1") -> QuotaUsage:
    return QuotaUsage(
        user_id=user_id,
        quota=EffectiveQuota(
            request_quota_count=5, request_quota_days=7, storage_quota_gb=10
        ),
        requests_in_window=2,
        storage_bytes=1024,
        exempt=False,
    )


def _app(quota, *, admin: bool = True) -> FastAPI:
    app = FastAPI()
    app.include_router(auth_routes.router)
    app.dependency_overrides[get_quota_service] = lambda: quota
    app.dependency_overrides[get_auth_service] = lambda: AsyncMock()
    if admin:
        app.dependency_overrides[_get_current_admin] = mock_admin_user
        app.dependency_overrides[_get_current_user] = lambda: mock_user(role="admin")
    return app


def test_get_user_quota_returns_override_effective_and_usage():
    quota = AsyncMock()
    quota.get_override.return_value = None
    quota.usage_for.return_value = _usage()

    resp = build_test_client(_app(quota)).get("/auth/admin/users/u1/quota")

    assert resp.status_code == 200
    body = resp.json()
    assert body["override"] == {
        "request_quota_count": None, "request_quota_days": None, "storage_quota_gb": None
    }
    assert body["effective_request_quota_count"] == 5
    assert body["requests_in_window"] == 2
    assert body["storage_bytes"] == 1024
    assert body["exempt"] is False


def test_put_user_quota_sets_override_and_returns_fresh_state():
    quota = AsyncMock()
    quota.get_override.return_value = None
    quota.usage_for.return_value = _usage()

    resp = build_test_client(_app(quota)).put(
        "/auth/admin/users/u1/quota",
        json={"request_quota_count": 9, "request_quota_days": None, "storage_quota_gb": 20},
    )

    assert resp.status_code == 200
    quota.set_override.assert_awaited_once_with(
        "u1", request_quota_count=9, request_quota_days=None, storage_quota_gb=20
    )


def test_quota_routes_require_admin():
    quota = AsyncMock()
    client = build_test_client(_app(quota, admin=False))
    assert client.get("/auth/admin/users/u1/quota").status_code == 401
    assert client.put(
        "/auth/admin/users/u1/quota", json={"request_quota_count": 1}
    ).status_code == 401
    quota.set_override.assert_not_awaited()
