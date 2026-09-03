"""SlskdClient tests: raw httpx calls against the slskd mock + targeted
MockTransport checks for body shape (searchTimeout ms, plain-array enqueue),
404/429 handling, and the AUD-10 error convention."""

import asyncio
import json

import httpx
import pytest

from core.exceptions import RateLimitedError, SlskdApiError
from repositories.slskd.slskd_client import SlskdClient
from repositories.slskd.slskd_models import SlskdEnqueueResponse
from tests.mocks import slskd_mock


@pytest.fixture
def mock_client() -> SlskdClient:
    slskd_mock.reset_state()
    transport = httpx.ASGITransport(app=slskd_mock.app)
    http = httpx.AsyncClient(transport=transport)
    return SlskdClient(http, "http://slskd", "test-key")


@pytest.mark.asyncio
async def test_health_check_returns_version(mock_client):
    info = await mock_client.health_check()
    assert info["version"]["current"] == "0.25.1.0"


@pytest.mark.asyncio
async def test_start_search_completes(mock_client):
    search = await mock_client.start_search("Radiohead - OK Computer", timeout_seconds=5)
    assert search.id
    state = await mock_client.get_search_state(search.id)
    assert state.is_complete is True


@pytest.mark.asyncio
async def test_search_responses_parse_lossless(mock_client):
    search = await mock_client.start_search("q", timeout_seconds=5)
    responses = await mock_client.get_search_responses(search.id)
    alice = next(r for r in responses if r.username == "alice")
    assert len(alice.files) == 12
    # bitRate is ABSENT for lossless -> None, not 0 (C6b).
    assert alice.files[0].bit_rate is None
    assert alice.files[0].bit_depth == 16


@pytest.mark.asyncio
async def test_enqueue_returns_enqueued_failed(mock_client):
    resp = await mock_client.enqueue("alice", [{"filename": "a.flac", "size": 100}])
    assert isinstance(resp, SlskdEnqueueResponse)
    assert len(resp.enqueued) == 1
    assert resp.failed == []


@pytest.mark.asyncio
async def test_get_downloads_reflects_enqueue(mock_client):
    await mock_client.enqueue("alice", [{"filename": "dir/a.flac", "size": 100}])
    transfers = await mock_client.get_downloads("alice")
    assert any(t.filename == "dir/a.flac" for t in transfers)


@pytest.mark.asyncio
async def test_get_downloads_unknown_user_empty(mock_client):
    transfers = await mock_client.get_downloads("nobody")
    assert transfers == []


@pytest.mark.asyncio
async def test_cancel_transfer_removes(mock_client):
    await mock_client.enqueue("alice", [{"filename": "dir/a.flac", "size": 100}])
    transfer_id = (await mock_client.get_downloads("alice"))[0].id
    assert await mock_client.cancel_transfer("alice", transfer_id) is True
    assert all(t.id != transfer_id for t in await mock_client.get_downloads("alice"))


@pytest.mark.asyncio
async def test_missing_api_key_raises(mock_client):
    # Search calls are not retry-wrapped -> the 401 surfaces immediately.
    transport = httpx.ASGITransport(app=slskd_mock.app)
    http = httpx.AsyncClient(transport=transport)
    client = SlskdClient(http, "http://slskd", "")  # empty key -> mock 401
    with pytest.raises(SlskdApiError):
        await client.start_search("q", timeout_seconds=1)


@pytest.mark.asyncio
async def test_search_timeout_is_milliseconds():
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "s1", "state": "InProgress", "isComplete": False})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    await client.start_search("q", timeout_seconds=30.0)
    assert captured["body"]["searchTimeout"] == 30000


@pytest.mark.asyncio
async def test_enqueue_sends_plain_array():
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(201, json={"Enqueued": [{"filename": "a", "size": 1}], "Failed": []})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    await client.enqueue("alice", [{"filename": "a", "size": 1}])
    assert isinstance(captured["body"], list)
    assert captured["body"][0]["filename"] == "a"
    assert "options" not in str(captured["body"])


@pytest.mark.asyncio
async def test_get_options_parses_directories():
    # Shape verified against a live slskd: directories is top-level with downloads/incomplete.
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith("/api/v0/options")
        return httpx.Response(200, json={
            "directories": {
                "incomplete": "/data/downloads/slskd_incomplete",
                "downloads": "/data/downloads/slskd",
            },
            "someOtherField": 123,  # unknown fields must be ignored
        })

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    options = await client.get_options()
    assert options.directories.downloads == "/data/downloads/slskd"
    assert options.directories.incomplete == "/data/downloads/slskd_incomplete"


@pytest.mark.asyncio
async def test_429_raises_rate_limited():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(429, text="Only one concurrent operation is permitted")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    with pytest.raises(RateLimitedError):
        await client.enqueue("alice", [{"filename": "a", "size": 1}])


@pytest.mark.asyncio
async def test_get_downloads_404_returns_empty():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(404)

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    assert await client.get_downloads("ghost") == []


def _reset_slskd_breakers() -> None:
    from repositories.slskd.slskd_client import (
        _slskd_circuit_breaker,
        _slskd_verify_circuit_breaker,
    )

    _slskd_circuit_breaker.reset()
    _slskd_verify_circuit_breaker.reset()


@pytest.mark.asyncio
async def test_401_raises_auth_error_single_attempt_breaker_closed():
    # Issue #193: a wrong key is deterministic misconfig, not an outage: one
    # attempt, auth-flagged, invisible to both breakers.
    from core.exceptions import SlskdAuthError
    from repositories.slskd.slskd_client import (
        _slskd_circuit_breaker,
        _slskd_verify_circuit_breaker,
    )

    _reset_slskd_breakers()
    calls = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(401)

    try:
        http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        client = SlskdClient(http, "http://slskd", "wrong-key")
        with pytest.raises(SlskdAuthError) as exc_info:
            await client.health_check()
        assert exc_info.value.auth is True
        assert exc_info.value.code == 401
        assert calls == 1
        assert not _slskd_circuit_breaker.is_open()
        assert _slskd_circuit_breaker.failure_count == 0
        assert _slskd_verify_circuit_breaker.failure_count == 0
    finally:
        _reset_slskd_breakers()


@pytest.mark.asyncio
async def test_403_raises_auth_error_single_attempt():
    # CIDR-shaped denies share the auth path: same single-attempt behaviour.
    from core.exceptions import SlskdAuthError

    _reset_slskd_breakers()
    calls = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(403, text="forbidden")

    try:
        http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        client = SlskdClient(http, "http://slskd", "cidr-denied-key")
        with pytest.raises(SlskdAuthError) as exc_info:
            await client.health_check()
        assert exc_info.value.auth is True
        assert exc_info.value.code == 403
        assert calls == 1
    finally:
        _reset_slskd_breakers()


@pytest.mark.asyncio
async def test_500_still_retries_and_records_breaker_failure(monkeypatch):
    # Scope pin: only 401/403 are auth; other >=400 keep retry + breaker.
    from core.exceptions import SlskdApiError, SlskdAuthError
    from repositories.slskd.slskd_client import _slskd_circuit_breaker

    async def no_sleep(delay: float) -> None:
        return None

    monkeypatch.setattr("asyncio.sleep", no_sleep)
    _reset_slskd_breakers()
    calls = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(500, text="boom")

    try:
        http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        client = SlskdClient(http, "http://slskd", "k")
        with pytest.raises(SlskdApiError) as exc_info:
            await client.health_check()
        assert not isinstance(exc_info.value, SlskdAuthError)
        assert calls == 3
        assert _slskd_circuit_breaker.failure_count == 1
    finally:
        _reset_slskd_breakers()


@pytest.mark.asyncio
async def test_verify_client_binds_isolated_breaker():
    # A poisoned live breaker must not block Test-connection traffic and a
    # poisoned verify breaker must not block live traffic.
    from repositories.slskd.slskd_client import (
        _slskd_circuit_breaker,
        _slskd_verify_circuit_breaker,
    )

    def ok(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"version": {"current": "0.25.1.0"}})

    _reset_slskd_breakers()
    try:
        for _ in range(5):
            _slskd_circuit_breaker.record_failure()
        assert _slskd_circuit_breaker.is_open()
        verify_http = httpx.AsyncClient(transport=httpx.MockTransport(ok))
        verify = SlskdClient(
            verify_http, "http://slskd", "k", use_verify_breaker=True
        )
        info = await verify.health_check()
        assert info["version"]["current"] == "0.25.1.0"

        _slskd_circuit_breaker.reset()
        for _ in range(5):
            _slskd_verify_circuit_breaker.record_failure()
        assert _slskd_verify_circuit_breaker.is_open()
        live_http = httpx.AsyncClient(transport=httpx.MockTransport(ok))
        live = SlskdClient(live_http, "http://slskd", "k")
        info = await live.health_check()
        assert info["version"]["current"] == "0.25.1.0"
    finally:
        _reset_slskd_breakers()


def test_build_slskd_repository_binds_verify_breaker():
    from core.dependencies.repo_providers import build_slskd_repository
    from infrastructure.http.client import HttpClientFactory

    try:
        repo = build_slskd_repository("http://slskd:5030", "k")
        assert repo._client._use_verify_breaker is True
    finally:
        HttpClientFactory.reset_for_tests()
