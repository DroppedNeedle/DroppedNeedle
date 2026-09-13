"""Per-user post-auth rate limiting.

The global RateLimitMiddleware stays as the pre-auth backstop and aggregate
cap; this layer runs after auth and caps each authenticated user's burst rate
against their own budget, so routine per-user spikes are absorbed without
touching other users' budgets.
"""

from __future__ import annotations

from types import SimpleNamespace

import httpx
import pytest
from fastapi import FastAPI
from fastapi.responses import PlainTextResponse
from starlette.middleware.base import BaseHTTPMiddleware

from middleware import PerUserRateLimitMiddleware, RateLimitMiddleware


class _StubAuthMiddleware(BaseHTTPMiddleware):
    """Stand-in for AuthMiddleware: the X-User header becomes request.state."""

    async def dispatch(self, request, call_next):  # noqa: ANN001, ANN201
        user_id = request.headers.get("X-User")
        if user_id:
            request.state.user = SimpleNamespace(id=user_id)
        return await call_next(request)


def _build_app(**kwargs):  # noqa: ANN003
    app = FastAPI()

    @app.get("/api/v1/test")
    async def api_test():  # noqa: ANN201
        return PlainTextResponse("ok")

    @app.get("/health")
    async def health():  # noqa: ANN201
        return PlainTextResponse("healthy")

    # Last-added executes first: stub auth, then the per-user limiter.
    app.add_middleware(PerUserRateLimitMiddleware, **kwargs)
    app.add_middleware(_StubAuthMiddleware)
    return app


@pytest.mark.asyncio
async def test_per_user_buckets_are_independent() -> None:
    app = _build_app(default_rate=1.0, default_capacity=1)
    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        first = await client.get("/api/v1/test", headers={"X-User": "a"})
        second = await client.get("/api/v1/test", headers={"X-User": "a"})
        other = await client.get("/api/v1/test", headers={"X-User": "b"})

    assert first.status_code == 200
    assert second.status_code == 429
    assert other.status_code == 200


@pytest.mark.asyncio
async def test_per_user_429_uses_rate_limited_envelope() -> None:
    app = _build_app(default_rate=1.0, default_capacity=1)
    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        await client.get("/api/v1/test", headers={"X-User": "a"})
        resp = await client.get("/api/v1/test", headers={"X-User": "a"})

    assert resp.status_code == 429
    assert resp.json()["error"]["code"] == "RATE_LIMITED"
    assert "Retry-After" in resp.headers
    assert resp.headers["X-RateLimit-Remaining"] == "0"


@pytest.mark.asyncio
async def test_requests_without_user_skip_per_user_limiter() -> None:
    app = _build_app(default_rate=1.0, default_capacity=1)
    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        statuses = [
            (await client.get("/api/v1/test")).status_code for _ in range(3)
        ]

    assert statuses == [200, 200, 200]


@pytest.mark.asyncio
async def test_non_api_paths_skip_per_user_limiter() -> None:
    app = _build_app(default_rate=1.0, default_capacity=1)
    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        await client.get("/api/v1/test", headers={"X-User": "a"})
        await client.get("/api/v1/test", headers={"X-User": "a"})
        resp = await client.get("/health", headers={"X-User": "a"})

    assert resp.status_code == 200
    assert "X-RateLimit-Limit" not in resp.headers


@pytest.mark.asyncio
async def test_success_carries_rate_limit_headers() -> None:
    app = _build_app(default_rate=10.0, default_capacity=20)
    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/api/v1/test", headers={"X-User": "a"})

    assert resp.status_code == 200
    assert resp.headers["X-RateLimit-Limit"] == "20"
    assert resp.headers["X-RateLimit-Remaining"] == "19"


@pytest.mark.asyncio
async def test_stacked_global_limiter_still_caps_unauthenticated_floods() -> None:
    app = FastAPI()

    @app.get("/api/v1/test")
    async def api_test():  # noqa: ANN201
        return PlainTextResponse("ok")

    # Production execution order: global -> auth -> per-user -> route.
    app.add_middleware(PerUserRateLimitMiddleware, default_rate=100.0, default_capacity=200)
    app.add_middleware(_StubAuthMiddleware)
    app.add_middleware(RateLimitMiddleware, default_rate=1.0, default_capacity=1)

    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        first = await client.get("/api/v1/test")
        second = await client.get("/api/v1/test")

    assert first.status_code == 200
    assert second.status_code == 429


@pytest.mark.asyncio
async def test_stacked_per_user_429_keeps_per_user_headers() -> None:
    app = FastAPI()

    @app.get("/api/v1/test")
    async def api_test():  # noqa: ANN201
        return PlainTextResponse("ok")

    # Production execution order: global -> auth -> per-user -> route.
    app.add_middleware(PerUserRateLimitMiddleware, default_rate=1.0, default_capacity=1)
    app.add_middleware(_StubAuthMiddleware)
    app.add_middleware(RateLimitMiddleware, default_rate=100.0, default_capacity=200)

    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        await client.get("/api/v1/test", headers={"X-User": "a"})
        limited = await client.get("/api/v1/test", headers={"X-User": "a"})
        other = await client.get("/api/v1/test", headers={"X-User": "b"})

    assert limited.status_code == 429
    assert limited.headers["X-RateLimit-Limit"] == "1"
    assert limited.headers["X-RateLimit-Remaining"] == "0"
    assert other.status_code == 200
    assert other.headers["X-RateLimit-Limit"] == "1"


def test_production_middleware_order_is_global_auth_per_user() -> None:
    from middleware import AuthMiddleware
    from target_application import create_production_target_application

    # user_middleware is execution order (add_middleware inserts at 0), so the
    # global limiter must precede auth and the per-user limiter follows it.
    order = [entry.cls for entry in create_production_target_application().user_middleware]

    assert (
        order.index(RateLimitMiddleware)
        < order.index(AuthMiddleware)
        < order.index(PerUserRateLimitMiddleware)
    )
