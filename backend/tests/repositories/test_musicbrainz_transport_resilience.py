from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest

import infrastructure.resilience.retry as retry_module
import repositories.musicbrainz_base as mb_base
from core.exceptions import ExternalServiceError, RateLimitedError
from infrastructure.queue.priority_queue import RequestPriority


@pytest.fixture(autouse=True)
def reset_musicbrainz_transport(monkeypatch):
    limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "mb_rate_limiter", limiter)
    monkeypatch.setattr(retry_module, "asyncio", SimpleNamespace(sleep=AsyncMock()))
    mb_base.mb_circuit_breaker.reset()
    yield limiter
    mb_base.mb_circuit_breaker.reset()


class _RaisingClient:
    def __init__(self, error: httpx.HTTPError) -> None:
        self.error = error
        self.calls = 0

    async def get(self, _url: str, params=None):
        self.calls += 1
        raise self.error


class _StatusClient:
    def __init__(self, status: int) -> None:
        self.status = status
        self.calls = 0

    async def get(self, _url: str, params=None):
        self.calls += 1
        return httpx.Response(self.status, content=b"{}")


@pytest.mark.asyncio
@pytest.mark.parametrize("error_type", [httpx.ConnectError, httpx.RemoteProtocolError])
async def test_connect_and_protocol_errors_fail_after_one_attempt(
    reset_musicbrainz_transport, monkeypatch, error_type
) -> None:
    request = httpx.Request("GET", "https://musicbrainz.org/ws/2/artist")
    client = _RaisingClient(error_type("blocked", request=request))
    monkeypatch.setattr(mb_base, "_http_client", client)

    with pytest.raises(error_type):
        await mb_base.mb_api_get("/artist")

    assert client.calls == 1
    assert mb_base.mb_circuit_breaker.failure_count == 1
    reset_musicbrainz_transport.acquire.assert_awaited_once_with(
        priority=int(RequestPriority.USER_INITIATED)
    )


@pytest.mark.asyncio
async def test_503_remains_retryable_within_budget(
    reset_musicbrainz_transport, monkeypatch
) -> None:
    client = _StatusClient(503)
    monkeypatch.setattr(mb_base, "_http_client", client)

    with pytest.raises(ExternalServiceError, match="rate limited"):
        await mb_base.mb_api_get("/artist")

    assert client.calls > 1
    assert client.calls <= 3
    assert mb_base.mb_circuit_breaker.failure_count == 1


class _HeaderStatusClient:
    def __init__(self, status: int, headers: dict[str, str]) -> None:
        self.status = status
        self.headers = headers
        self.calls = 0

    async def get(self, _url: str, params=None):
        self.calls += 1
        return httpx.Response(self.status, headers=self.headers, content=b"{}")


def test_retry_after_parser_is_bounded_and_rejects_invalid_values():
    assert mb_base._parse_retry_after_seconds("120") == 60.0
    assert mb_base._parse_retry_after_seconds("nan") is None
    assert mb_base._parse_retry_after_seconds("0") is None
    assert mb_base._parse_retry_after_seconds("not-a-delay") is None


@pytest.mark.asyncio
async def test_429_retry_after_is_honored_without_exceeding_retry_budget(
    reset_musicbrainz_transport, monkeypatch
) -> None:
    client = _HeaderStatusClient(429, {"Retry-After": "1"})
    monkeypatch.setattr(mb_base, "_http_client", client)

    with pytest.raises(RateLimitedError) as raised:
        await mb_base.mb_api_get("/artist")

    assert raised.value.retry_after_seconds == 1.0
    assert client.calls == 3
    assert retry_module.asyncio.sleep.await_args_list
    assert all(
        call.args == (1.0,) for call in retry_module.asyncio.sleep.await_args_list
    )


@pytest.mark.asyncio
async def test_settings_probe_uses_isolated_client_path_and_telemetry(monkeypatch):
    client = _HeaderStatusClient(200, {})
    limiter = SimpleNamespace(acquire=AsyncMock())
    calls = []
    headers = []
    monkeypatch.setattr(mb_base, "_mb_probe_rate_limiter", limiter)
    monkeypatch.setattr(
        mb_base,
        "record_provider_call",
        lambda *args: calls.append(args),
    )
    monkeypatch.setattr(
        mb_base,
        "record_rate_limit_headers",
        lambda *args: headers.append(args),
    )

    before_source = mb_base.get_mb_api_base()
    before_breaker = mb_base.mb_circuit_breaker.get_state()
    response = await mb_base.mb_api_probe(
        "https://mirror.example/ws/2",
        params={"query": "test"},
        client=client,
    )

    assert response.status_code == 200
    assert client.calls == 1
    assert mb_base.get_mb_api_base() == before_source
    assert mb_base.mb_circuit_breaker.get_state() == before_breaker
    assert calls == [("musicbrainz", RequestPriority.USER_INITIATED, 200)]
    assert headers and headers[0][0] == "musicbrainz"
