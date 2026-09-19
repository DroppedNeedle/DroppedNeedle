"""Tests that exception handlers do not leak internal details."""

import pytest
from fastapi import FastAPI
import httpx

from core.exceptions import ExternalServiceError, RateLimitedError
from core.exception_handlers import (
    external_service_error_handler,
    circuit_open_error_handler,
    general_exception_handler,
    rate_limited_error_handler,
)
from infrastructure.resilience.retry import CircuitOpenError


def _build_app() -> FastAPI:
    app = FastAPI()

    @app.get("/raise-general")
    async def raise_general():
        raise RuntimeError("secret internal path /app/main.py")

    @app.get("/raise-external")
    async def raise_external():
        raise ExternalServiceError("connection to 10.0.0.5:8096 refused")

    @app.get("/raise-circuit")
    async def raise_circuit():
        raise CircuitOpenError(
            "Circuit breaker 'jellyfin' is OPEN",
            breaker_name="jellyfin",
        )

    @app.get("/raise-rate-limited")
    async def raise_rate_limited():
        raise RateLimitedError(
            "MusicBrainz rate limited (429): /ws/2/release-group from 10.0.0.5:8096",
            retry_after_seconds=7.9,
        )

    @app.get("/raise-rate-limited-no-hint")
    async def raise_rate_limited_no_hint():
        raise RateLimitedError("slskd: only one concurrent operation is permitted")

    app.add_exception_handler(RateLimitedError, rate_limited_error_handler)
    app.add_exception_handler(ExternalServiceError, external_service_error_handler)
    app.add_exception_handler(CircuitOpenError, circuit_open_error_handler)
    app.add_exception_handler(Exception, general_exception_handler)

    return app


@pytest.mark.asyncio
async def test_general_exception_handler_hides_details():
    app = _build_app()
    transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/raise-general")

    body = resp.json()
    assert resp.status_code == 500
    assert body["error"]["message"] == "Internal server error"
    assert "/app/main.py" not in resp.text


@pytest.mark.asyncio
async def test_external_service_error_hides_details():
    app = _build_app()
    transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/raise-external")

    body = resp.json()
    assert resp.status_code == 503
    assert body["error"]["message"] == "External service unavailable"
    assert "10.0.0.5" not in resp.text


@pytest.mark.asyncio
async def test_circuit_open_error_hides_details():
    app = _build_app()
    transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/raise-circuit")

    body = resp.json()
    assert resp.status_code == 503
    assert body["error"]["message"] == "Jellyfin is temporarily unavailable due to repeated connection failures. Check your settings or wait for the service to recover."
    assert "circuit breaker" not in resp.text.lower() or "CIRCUIT_BREAKER_OPEN" in resp.text


@pytest.mark.asyncio
async def test_rate_limited_error_maps_to_429_with_retry_after():
    app = _build_app()
    transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/raise-rate-limited")

    body = resp.json()
    assert resp.status_code == 429
    assert body["error"]["code"] == "RATE_LIMITED"
    assert body["error"]["message"] == "Too many requests"
    assert body["error"]["details"] is None
    assert resp.headers["Retry-After"] == "7"
    assert "10.0.0.5" not in resp.text
    assert "/ws/2/release-group" not in resp.text


@pytest.mark.asyncio
async def test_rate_limited_error_without_hint_omits_retry_after():
    app = _build_app()
    transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/raise-rate-limited-no-hint")

    body = resp.json()
    assert resp.status_code == 429
    assert body["error"]["code"] == "RATE_LIMITED"
    assert "Retry-After" not in resp.headers


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "hint, expected",
    [
        (float("inf"), None),
        (float("-inf"), None),
        (float("nan"), None),
        ("garbage", None),
        ("inf", None),
        (None, None),
        (1e30, "3600"),
        (7.9, "7"),
        (-5, "0"),
    ],
)
async def test_rate_limited_error_sanitizes_retry_after_hint(hint, expected):
    """Garbage/non-finite upstream Retry-After hints omit the header (or clamp
    to a sane max) - the 429 handler must never 500 on the hint itself."""
    app = FastAPI()

    @app.get("/raise-rate-limited-hint")
    async def raise_rate_limited_hint():
        raise RateLimitedError(
            "upstream sent a bad Retry-After", retry_after_seconds=hint
        )

    app.add_exception_handler(RateLimitedError, rate_limited_error_handler)
    transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/raise-rate-limited-hint")

    body = resp.json()
    assert resp.status_code == 429
    assert body["error"]["code"] == "RATE_LIMITED"
    assert body["error"]["message"] == "Too many requests"
    if expected is None:
        assert "Retry-After" not in resp.headers
    else:
        assert resp.headers["Retry-After"] == expected


@pytest.mark.asyncio
async def test_circuit_open_error_without_breaker_name_falls_back():
    app = FastAPI()

    @app.get("/raise-circuit-unnamed")
    async def raise_circuit_unnamed():
        raise CircuitOpenError("CB tripped")

    app.add_exception_handler(CircuitOpenError, circuit_open_error_handler)
    transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        resp = await client.get("/raise-circuit-unnamed")

    body = resp.json()
    assert resp.status_code == 503
    assert body["error"]["message"].startswith("Service is temporarily unavailable")
