import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, call

import httpx
import pytest

import infrastructure.http.brainzmash_transport as brainzmash_transport
import infrastructure.resilience.retry as retry_module
import repositories.musicbrainz_base as mb_base
from core.exceptions import (
    ConfigurationError,
    ExternalServiceError,
    InvalidExternalPayloadError,
    NonRetriableExternalServiceError,
    RateLimitedError,
)
from infrastructure.queue.priority_queue import RequestPriority


@pytest.fixture(autouse=True)
def reset_musicbrainz_transport(monkeypatch):
    limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "mb_rate_limiter", limiter)
    monkeypatch.setattr(retry_module, "asyncio", SimpleNamespace(sleep=AsyncMock()))
    mb_base.mb_circuit_breaker.reset()
    mb_base.brainzmash_circuit_breaker.reset()
    mb_base.brainzmash_scheduler.reset()
    yield limiter
    mb_base.mb_circuit_breaker.reset()
    mb_base.brainzmash_circuit_breaker.reset()
    mb_base.brainzmash_scheduler.reset()


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


@pytest.mark.asyncio
async def test_brainzmash_breaker_does_not_poison_official_requests(
    reset_musicbrainz_transport, monkeypatch
) -> None:
    request = httpx.Request("GET", "https://api.brainzmash.cc/ws/2/artist")
    brainzmash = _RaisingClient(
        httpx.ConnectError("BrainzMash unavailable", request=request)
    )
    official = _SequenceClient([200])
    brainz_limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", brainzmash)
    monkeypatch.setattr(mb_base, "_http_client", official)
    monkeypatch.setattr(mb_base, "brainzmash_rate_limiter", brainz_limiter)
    monkeypatch.setattr(mb_base.brainzmash_circuit_breaker, "failure_threshold", 1)
    before = mb_base.capture_mb_source_context()

    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-breaker-test",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )
    assert (
        mb_base.get_mb_provider_circuit_breaker() is mb_base.brainzmash_circuit_breaker
    )
    try:
        with pytest.raises(httpx.ConnectError):
            await mb_base.mb_api_get("/artist")
        assert mb_base.brainzmash_circuit_breaker.is_open()

        mb_base.set_mb_api_base(
            "https://musicbrainz.org/ws/2",
            source_mode="official",
            source_id="official-breaker-test",
            generation=before.generation + 2,
        )
        assert mb_base.get_mb_provider_circuit_breaker() is mb_base.mb_circuit_breaker
        assert await mb_base.mb_api_get("/artist") == {"artist": []}
        assert mb_base.mb_circuit_breaker.get_state()["state"] == "closed"
    finally:
        _restore_source(before)

    assert brainzmash.calls == 1
    assert len(official.urls) == 1


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
    assert mb_base._parse_retry_after_seconds("0") == 0.0
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
    assert len(calls) == 1
    assert calls[0][:3] == ("musicbrainz", RequestPriority.USER_INITIATED, 200)
    probe_context = calls[0][3]
    assert probe_context.source_mode == "official"
    assert probe_context.generation == mb_base.get_mb_source_generation()
    assert probe_context.source_id.startswith("probe-")
    assert headers and headers[0][0] == "musicbrainz"


def test_brainzmash_url_and_path_validation_is_strict():
    assert (
        brainzmash_transport.validate_brainzmash_url("https://api.brainzmash.cc/ws/2/")
        == "https://api.brainzmash.cc/ws/2"
    )
    with pytest.raises(ValueError):
        brainzmash_transport.validate_brainzmash_url(
            "https://api.brainzmash.cc/ws/2?token=secret"
        )
    with pytest.raises(ValueError):
        brainzmash_transport.validate_brainzmash_url(
            "https://api.brainzmash.cc.evil/ws/2"
        )
    assert (
        brainzmash_transport.validate_brainzmash_path("/artist/example-id")
        == "/artist/example-id"
    )
    with pytest.raises(ValueError):
        brainzmash_transport.validate_brainzmash_path("/artist/../admin")


@pytest.mark.parametrize(
    "url",
    [
        "http://api.brainzmash.cc/ws/2",
        "https://api.brainzmash.cc.evil/ws/2",
        "https://api.brainzmash.cc:443/ws/2",
        "https://user:secret@api.brainzmash.cc/ws/2",
        "https://api.brainzmash.cc/ws/2?token=secret",
        "https://api.brainzmash.cc/ws/2#fragment",
        "https://api.brainzmash.cc/ws/2/../admin",
        "https://api.brainzmash.cc/%77s/2",
    ],
)
def test_brainzmash_url_rejects_downgrade_credentials_origin_and_escape(url):
    with pytest.raises(ValueError):
        brainzmash_transport.validate_brainzmash_url(url)


@pytest.mark.parametrize(
    "address",
    [
        "0.0.0.0",
        "10.0.0.8",
        "100.64.0.8",
        "127.0.0.1",
        "169.254.1.8",
        "224.0.0.8",
        "240.0.0.8",
        "::",
        "::1",
        "fc00::8",
        "fe80::8",
        "ff02::8",
    ],
)
@pytest.mark.asyncio
async def test_brainzmash_transport_rejects_non_global_ipv4_and_ipv6_dns(
    monkeypatch, address
):
    import httpcore

    delegate = AsyncMock()
    sockaddr = (address, 443, 0, 0) if ":" in address else (address, 443)
    monkeypatch.setattr(
        brainzmash_transport.socket,
        "getaddrinfo",
        lambda *args, **kwargs: [
            (
                brainzmash_transport.socket.AF_INET6
                if ":" in address
                else brainzmash_transport.socket.AF_INET,
                brainzmash_transport.socket.SOCK_STREAM,
                6,
                "",
                sockaddr,
            )
        ],
    )
    backend = brainzmash_transport._PinnedBrainzMashNetworkBackend(delegate)

    with pytest.raises(httpcore.ConnectError, match="non-public"):
        await backend.connect_tcp("api.brainzmash.cc", 443)
    delegate.connect_tcp.assert_not_awaited()


@pytest.mark.asyncio
async def test_brainzmash_transport_rejects_mixed_public_and_private_dns(monkeypatch):
    import httpcore

    delegate = AsyncMock()
    monkeypatch.setattr(
        brainzmash_transport.socket,
        "getaddrinfo",
        lambda *args, **kwargs: [
            (
                brainzmash_transport.socket.AF_INET,
                brainzmash_transport.socket.SOCK_STREAM,
                6,
                "",
                ("8.8.8.8", 443),
            ),
            (
                brainzmash_transport.socket.AF_INET,
                brainzmash_transport.socket.SOCK_STREAM,
                6,
                "",
                ("10.0.0.8", 443),
            ),
        ],
    )
    backend = brainzmash_transport._PinnedBrainzMashNetworkBackend(delegate)

    with pytest.raises(httpcore.ConnectError, match="non-public"):
        await backend.connect_tcp("api.brainzmash.cc", 443)
    delegate.connect_tcp.assert_not_awaited()


@pytest.mark.asyncio
async def test_brainzmash_transport_connects_only_to_validated_public_address(
    monkeypatch,
):
    delegate = AsyncMock()
    delegate.connect_tcp.return_value = "stream"
    monkeypatch.setattr(
        brainzmash_transport.socket,
        "getaddrinfo",
        lambda *args, **kwargs: [
            (
                brainzmash_transport.socket.AF_INET,
                brainzmash_transport.socket.SOCK_STREAM,
                6,
                "",
                ("8.8.8.8", 443),
            )
        ],
    )
    backend = brainzmash_transport._PinnedBrainzMashNetworkBackend(delegate)

    assert await backend.connect_tcp("api.brainzmash.cc", 443) == "stream"
    delegate.connect_tcp.assert_awaited_once()
    assert delegate.connect_tcp.await_args.args[0] == "8.8.8.8"


@pytest.mark.asyncio
async def test_brainzmash_transport_rejects_public_to_private_dns_rebinding(
    monkeypatch,
):
    import httpcore

    delegate = SimpleNamespace(
        connect_tcp=AsyncMock(side_effect=httpcore.ConnectError("connection failed"))
    )
    answers = iter(["8.8.8.8", "10.0.0.8"])

    def resolve(*args, **kwargs):
        address = next(answers)
        return [
            (
                brainzmash_transport.socket.AF_INET,
                brainzmash_transport.socket.SOCK_STREAM,
                6,
                "",
                (address, 443),
            )
        ]

    monkeypatch.setattr(brainzmash_transport.socket, "getaddrinfo", resolve)
    backend = brainzmash_transport._PinnedBrainzMashNetworkBackend(delegate)

    with pytest.raises(httpcore.ConnectError, match="Could not connect"):
        await backend.connect_tcp("api.brainzmash.cc", 443)
    with pytest.raises(httpcore.ConnectError, match="non-public"):
        await backend.connect_tcp("api.brainzmash.cc", 443)
    assert delegate.connect_tcp.await_args.args[0] == "8.8.8.8"


@pytest.mark.asyncio
async def test_brainzmash_transport_rejects_wrong_host_before_dns(monkeypatch):
    import httpcore

    resolve = AsyncMock()
    monkeypatch.setattr(brainzmash_transport.socket, "getaddrinfo", resolve)
    backend = brainzmash_transport._PinnedBrainzMashNetworkBackend(AsyncMock())

    with pytest.raises(httpcore.ConnectError, match="unapproved host"):
        await backend.connect_tcp("api.brainzmash.cc.evil", 443)
    resolve.assert_not_called()


@pytest.mark.parametrize(
    "path",
    [
        "artist",
        "/admin",
        "/artist/../admin",
        "/artist/%2e%2e/admin",
        "/artist//id",
        "/artist/id?secret=1",
        "/artist/id#fragment",
        "/unknown/id",
    ],
)
def test_brainzmash_path_rejects_unsupported_or_escaped_routes(path):
    with pytest.raises(ValueError):
        brainzmash_transport.validate_brainzmash_path(path)


@pytest.mark.asyncio
async def test_brainzmash_transport_rejects_private_dns_results(monkeypatch):
    import httpcore

    delegate = AsyncMock()
    monkeypatch.setattr(
        brainzmash_transport.socket,
        "getaddrinfo",
        lambda *args, **kwargs: [
            (
                brainzmash_transport.socket.AF_INET,
                brainzmash_transport.socket.SOCK_STREAM,
                6,
                "",
                ("10.0.0.8", 443),
            )
        ],
    )
    backend = brainzmash_transport._PinnedBrainzMashNetworkBackend(delegate)
    with pytest.raises(httpcore.ConnectError, match="non-public"):
        await backend.connect_tcp("api.brainzmash.cc", 443)
    delegate.connect_tcp.assert_not_awaited()


@pytest.mark.asyncio
async def test_brainzmash_client_isolated_from_redirects_and_pool():
    from infrastructure.http.client import HttpClientFactory, get_brainzmash_http_client

    HttpClientFactory.reset_for_tests()
    client = get_brainzmash_http_client()
    try:
        assert client.follow_redirects is False
        assert client.headers["accept"] == "application/json"
        assert client._transport.__class__.__name__ == "BrainzMashTransport"
    finally:
        await client.aclose()
        HttpClientFactory.reset_for_tests()


@pytest.mark.asyncio
async def test_brainzmash_request_sanitizer_drops_credentials_and_preserves_required_headers():
    from infrastructure.http.client import _sanitize_brainzmash_request

    request = httpx.Request(
        "GET",
        "https://api.brainzmash.cc/ws/2/artist",
        headers={
            "Accept": "application/json",
            "User-Agent": "DroppedNeedle/test",
            "Cookie": "session=secret",
            "Authorization": "Bearer secret",
            "Proxy-Authorization": "Basic secret",
            "X-BrainzMash-Key": "secret",
            "X-Api-Key": "secret",
        },
    )

    await _sanitize_brainzmash_request(request)

    assert request.headers["accept"] == "application/json"
    assert request.headers["user-agent"] == "DroppedNeedleApp"
    assert "cookie" not in request.headers
    assert "authorization" not in request.headers
    assert "proxy-authorization" not in request.headers
    assert "x-brainzmash-key" not in request.headers
    assert "x-api-key" not in request.headers


@pytest.mark.asyncio
async def test_brainzmash_client_dispatches_async_sanitizer_before_mock_transport(
    monkeypatch,
):
    from infrastructure.http import client as http_client

    observed: list[httpx.Request] = []

    async def handle(request: httpx.Request) -> httpx.Response:
        observed.append(request)
        return httpx.Response(200, json={"artist": []}, request=request)

    http_client.HttpClientFactory.reset_for_tests()
    monkeypatch.setattr(
        http_client,
        "BrainzMashTransport",
        lambda: httpx.MockTransport(handle),
    )
    client = http_client.get_brainzmash_http_client()
    try:
        response = await client.get(
            "https://api.brainzmash.cc/ws/2/artist",
            headers={
                "Accept": "application/json",
                "Accept-Encoding": "identity",
                "Connection": "keep-alive",
                "Host": "evil.example",
                "User-Agent": "evil-client",
                "Authorization": "Bearer secret",
                "Cookie": "session=secret",
                "Proxy-Authorization": "Basic secret",
                "X-BrainzMash-Key": "secret",
                "X-Api-Key": "secret",
            },
        )
    finally:
        await client.aclose()
        http_client.HttpClientFactory.reset_for_tests()

    assert response.status_code == 200
    assert len(observed) == 1
    wire_request = observed[0]
    assert set(wire_request.headers) == {
        "accept",
        "accept-encoding",
        "connection",
        "host",
        "user-agent",
    }
    assert wire_request.headers["accept"] == "application/json"
    assert wire_request.headers["accept-encoding"] == "identity"
    assert wire_request.headers["connection"] == "keep-alive"
    assert wire_request.headers["host"] == "api.brainzmash.cc"
    assert wire_request.headers["user-agent"] == "DroppedNeedleApp"


class _SequenceClient:
    def __init__(
        self,
        statuses: list[int],
        payload: dict[str, object] | None = None,
    ) -> None:
        self.statuses = list(statuses)
        self.payload = payload or {"artist": []}
        self.urls: list[str] = []

    async def get(self, url: str, params=None):
        self.urls.append(url)
        status = self.statuses.pop(0) if self.statuses else 200
        return httpx.Response(status, json=self.payload)


class _BlockingClient(_SequenceClient):
    def __init__(self, started, release) -> None:
        super().__init__([200])
        self.started = started
        self.release = release

    async def get(self, url: str, params=None):
        self.urls.append(url)
        self.started.set()
        await self.release.wait()
        return httpx.Response(200, json={"artist": []})


@pytest.mark.asyncio
async def test_brainzmash_3xx_is_rejected_without_following_redirects(monkeypatch):
    client = _StatusClient(302)
    limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", client)
    monkeypatch.setattr(mb_base, "brainzmash_rate_limiter", limiter)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-redirect-test",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )
    try:
        with pytest.raises(NonRetriableExternalServiceError, match="redirect rejected"):
            await mb_base.mb_api_get("/artist")
    finally:
        _restore_source(before)

    assert client.calls == 1
    assert limiter.acquire.await_count == client.calls


@pytest.mark.asyncio
async def test_200_malformed_payload_is_invalid_and_not_retried(monkeypatch):
    class _MalformedClient:
        calls = 0

        async def get(self, _url, params=None):
            self.calls += 1
            return httpx.Response(200, content=b"{not-json")

    client = _MalformedClient()
    monkeypatch.setattr(mb_base, "_http_client", client)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        "https://musicbrainz.org/ws/2",
        source_mode="official",
        source_id="official-payload-test",
        generation=before.generation + 1,
    )
    try:
        with pytest.raises(InvalidExternalPayloadError):
            await mb_base.mb_api_get("/artist")
    finally:
        _restore_source(before)

    assert client.calls == 1


@pytest.mark.asyncio
async def test_404_returns_empty_payload_without_breaker_failure(monkeypatch):
    client = _StatusClient(404)
    monkeypatch.setattr(mb_base, "_http_client", client)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        "https://musicbrainz.org/ws/2",
        source_mode="official",
        source_id="official-404-test",
        generation=before.generation + 1,
    )
    try:
        assert await mb_base.mb_api_get("/artist") == {}
    finally:
        _restore_source(before)

    assert client.calls == 1
    assert mb_base.mb_circuit_breaker.failure_count == 0


@pytest.mark.asyncio
async def test_brainzmash_deterministic_4xx_is_not_retried(monkeypatch):
    client = _StatusClient(418)
    limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", client)
    monkeypatch.setattr(mb_base, "brainzmash_rate_limiter", limiter)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-4xx-test",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )
    try:
        with pytest.raises(NonRetriableExternalServiceError, match="rejected"):
            await mb_base.mb_api_get("/artist")
    finally:
        _restore_source(before)

    assert client.calls == 1
    assert limiter.acquire.await_count == 1


@pytest.mark.asyncio
async def test_request_cancellation_bypasses_retry_and_breaker(monkeypatch):
    class _CancelledClient:
        calls = 0

        async def get(self, _url, params=None):
            self.calls += 1
            raise asyncio.CancelledError

    client = _CancelledClient()
    monkeypatch.setattr(mb_base, "_http_client", client)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        "https://musicbrainz.org/ws/2",
        source_mode="official",
        source_id="official-cancel-test",
        generation=before.generation + 1,
    )
    try:
        with pytest.raises(asyncio.CancelledError):
            await mb_base.mb_api_get("/artist")
    finally:
        _restore_source(before)

    assert client.calls == 1
    assert mb_base.mb_circuit_breaker.failure_count == 0


@pytest.mark.asyncio
async def test_brainzmash_verification_probe_is_pinned_and_rate_limited(monkeypatch):
    brainzmash = _SequenceClient(
        [200],
        payload={
            "id": mb_base._BRAINZMASH_PROBE_ARTIST_ID,
            "name": "Probe artist",
        },
    )
    official = _RaisingClient(httpx.ConnectError("official probe must not run"))
    probe_limiter = SimpleNamespace(acquire=AsyncMock())
    assert mb_base.brainzmash_probe_rate_limiter.rate == 10.0
    assert mb_base.brainzmash_probe_rate_limiter.capacity == 1
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", brainzmash)
    monkeypatch.setattr(mb_base, "brainzmash_probe_rate_limiter", probe_limiter)

    assert mb_base.brainzmash_probe_rate_limiter is probe_limiter
    response = await mb_base.mb_api_probe(
        brainzmash_transport.BRAINZMASH_ENDPOINT,
        client=official,
        allow_unbound_brainzmash=True,
        brainzmash=True,
    )

    assert response.status_code == 200
    assert brainzmash.urls == [
        f"https://api.brainzmash.cc/ws/2/artist/{mb_base._BRAINZMASH_PROBE_ARTIST_ID}"
    ]
    assert official.calls == 0
    probe_limiter.acquire.assert_awaited_once_with(
        priority=int(RequestPriority.USER_INITIATED)
    )


def _restore_source(source):
    mb_base.set_mb_api_base(
        source.source_url,
        source_mode=source.source_mode,
        source_id=source.source_id,
        generation=source.generation,
    )


@pytest.mark.asyncio
async def test_active_brainzmash_funnel_never_uses_official_client(
    reset_musicbrainz_transport, monkeypatch
) -> None:
    brainzmash = _SequenceClient([503, 200])
    official = _RaisingClient(httpx.ConnectError("official client must not be called"))
    assert mb_base.brainzmash_rate_limiter.rate == 10.0
    assert mb_base.brainzmash_rate_limiter.capacity == 1
    assert mb_base.brainzmash_probe_rate_limiter.rate == 10.0
    assert mb_base.brainzmash_probe_rate_limiter.capacity == 1
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", brainzmash)
    monkeypatch.setattr(mb_base, "_http_client", official)
    brainz_limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "brainzmash_rate_limiter", brainz_limiter)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-test",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )
    try:
        assert await mb_base.mb_api_get("/release-group/test-id") == {"artist": []}
        assert await mb_base.mb_api_get(
            "/isrc/US-ABC-123", priority=RequestPriority.BACKGROUND_SYNC
        ) == {"artist": []}
    finally:
        _restore_source(before)
    expected = "https://api.brainzmash.cc/ws/2/release-group/test-id"
    isrc_expected = "https://api.brainzmash.cc/ws/2/isrc/US-ABC-123"
    assert brainzmash.urls == [expected, expected, isrc_expected]
    assert official.calls == 0
    assert brainz_limiter.acquire.await_args_list == [
        call(priority=int(RequestPriority.USER_INITIATED)),
        call(priority=int(RequestPriority.USER_INITIATED)),
        call(priority=int(RequestPriority.BACKGROUND_SYNC)),
    ]
    assert reset_musicbrainz_transport.acquire.await_count == 0


@pytest.mark.asyncio
async def test_brainzmash_inflight_request_keeps_origin_after_source_switch(
    monkeypatch,
) -> None:
    started = asyncio.Event()
    release = asyncio.Event()
    brainzmash = _BlockingClient(started, release)
    official = _RaisingClient(httpx.ConnectError("official client must not be called"))
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", brainzmash)
    monkeypatch.setattr(mb_base, "_http_client", official)
    monkeypatch.setattr(mb_base, "_mb_limiter_bypassed", True)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-inflight",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )

    try:
        task = asyncio.create_task(mb_base.mb_api_get("/artist"))
        await started.wait()
        mb_base.set_mb_api_base(
            "https://musicbrainz.org/ws/2",
            source_mode="official",
            source_id="official-after-switch",
            generation=before.generation + 2,
        )
        release.set()
        with pytest.raises(ConfigurationError, match="source changed"):
            await task
    finally:
        release.set()
        _restore_source(before)

    assert brainzmash.urls == ["https://api.brainzmash.cc/ws/2/artist"]
    assert official.calls == 0


@pytest.mark.asyncio
async def test_brainzmash_scheduler_progression_is_shared_and_success_resets():
    now = [100.0]
    sleeps: list[float] = []

    async def sleep(delay: float) -> None:
        sleeps.append(delay)
        now[0] += delay

    scheduler = mb_base._BrainzMashScheduler(
        clock=lambda: now[0],
        random_fn=lambda: 0.0,
        sleep=sleep,
    )
    limiter = SimpleNamespace(acquire=AsyncMock())
    admitted: list[str] = []

    first_delay = scheduler.note_cooldown(None)
    second_delay = scheduler.note_cooldown(None)
    assert first_delay == 0.5
    assert second_delay == 1.0
    assert scheduler.cooldown_remaining() == 1.0

    async def operation() -> str:
        admitted.append("wire")
        return "ok"

    results = await asyncio.gather(
        scheduler.run(
            RequestPriority.USER_INITIATED,
            operation,
            limiter=limiter,
        ),
        scheduler.run(
            RequestPriority.BACKGROUND_SYNC,
            operation,
            limiter=limiter,
        ),
    )
    assert results == ["ok", "ok"]
    assert admitted == ["wire", "wire"]
    assert sleeps == [1.0]
    assert limiter.acquire.await_args_list == [
        call(priority=int(RequestPriority.USER_INITIATED)),
        call(priority=int(RequestPriority.BACKGROUND_SYNC)),
    ]

    assert scheduler.note_cooldown(7.0) == 7.0
    assert scheduler._consecutive_no_retry_after == 0
    assert scheduler.cooldown_remaining() == 7.0
    assert scheduler.note_cooldown(1000.0) == 60.0
    assert scheduler.cooldown_remaining() == 60.0
    scheduler.note_success()
    assert scheduler.cooldown_remaining() == 0.0
    assert scheduler._consecutive_no_retry_after == 0
    assert scheduler.note_cooldown(0.0) == 0.0


@pytest.mark.asyncio
async def test_global_brainzmash_scheduler_serializes_blocked_operations():
    limiter = SimpleNamespace(acquire=AsyncMock())
    first_started = asyncio.Event()
    release_first = asyncio.Event()
    second_admitted = asyncio.Event()
    completion: list[str] = []
    in_flight = 0
    max_in_flight = 0

    async def first_operation() -> str:
        nonlocal in_flight, max_in_flight
        in_flight += 1
        max_in_flight = max(max_in_flight, in_flight)
        first_started.set()
        await release_first.wait()
        completion.append("first")
        in_flight -= 1
        return "first"

    async def second_operation() -> str:
        nonlocal in_flight, max_in_flight
        in_flight += 1
        max_in_flight = max(max_in_flight, in_flight)
        second_admitted.set()
        completion.append("second")
        in_flight -= 1
        return "second"

    first = asyncio.create_task(
        mb_base.brainzmash_scheduler.run(
            RequestPriority.USER_INITIATED,
            first_operation,
            limiter=limiter,
        )
    )
    await first_started.wait()
    second = asyncio.create_task(
        mb_base.brainzmash_scheduler.run(
            RequestPriority.BACKGROUND_SYNC,
            second_operation,
            limiter=limiter,
        )
    )
    assert not second_admitted.is_set()

    release_first.set()
    results = await asyncio.gather(first, second)

    assert results == ["first", "second"]
    assert completion == ["first", "second"]
    assert max_in_flight == 1
    assert in_flight == 0
    assert second_admitted.is_set()
    assert limiter.acquire.await_count == 2


@pytest.mark.asyncio
async def test_brainzmash_429_retry_reenters_scheduler_without_duplicate_sleep(
    monkeypatch,
):
    brainzmash = _SequenceClient([429, 200])
    limiter = SimpleNamespace(acquire=AsyncMock())
    now = [200.0]
    sleeps: list[float] = []

    async def sleep(delay: float) -> None:
        sleeps.append(delay)
        now[0] += delay

    scheduler = mb_base._BrainzMashScheduler(
        clock=lambda: now[0],
        random_fn=lambda: 0.0,
        sleep=sleep,
    )
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", brainzmash)
    monkeypatch.setattr(mb_base, "brainzmash_rate_limiter", limiter)
    monkeypatch.setattr(mb_base, "brainzmash_scheduler", scheduler)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-429-retry",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )
    try:
        assert await mb_base.mb_api_get("/artist") == {"artist": []}
    finally:
        _restore_source(before)

    assert len(brainzmash.urls) == 2
    assert brainzmash.urls == [
        "https://api.brainzmash.cc/ws/2/artist",
        "https://api.brainzmash.cc/ws/2/artist",
    ]
    assert sleeps == [0.5]
    assert limiter.acquire.await_count == 2
    retry_module.asyncio.sleep.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [429, 503, 302, 418])
async def test_brainzmash_error_messages_are_source_only(monkeypatch, status):
    client = _StatusClient(status)
    limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", client)
    monkeypatch.setattr(mb_base, "brainzmash_rate_limiter", limiter)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-privacy",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )
    try:
        with pytest.raises(ExternalServiceError) as caught:
            await mb_base.mb_api_get("/artist/private-id", params={"inc": "secret"})
    finally:
        _restore_source(before)

    message = str(caught.value)
    assert "artist" not in message
    assert "private-id" not in message
    assert "secret" not in message
    assert "api.brainzmash.cc" not in message


@pytest.mark.asyncio
async def test_brainzmash_payload_error_is_source_only(monkeypatch):
    class _MalformedClient:
        async def get(self, _url, params=None):
            return httpx.Response(200, content=b"{not-json")

    client = _MalformedClient()
    limiter = SimpleNamespace(acquire=AsyncMock())
    monkeypatch.setattr(mb_base, "_brainzmash_http_client", client)
    monkeypatch.setattr(mb_base, "brainzmash_rate_limiter", limiter)
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        brainzmash_transport.BRAINZMASH_ENDPOINT.rstrip("/"),
        source_mode="brainzmash",
        source_id="brainzmash-payload-privacy",
        generation=before.generation + 1,
        brainzmash_binding_valid=True,
    )
    try:
        with pytest.raises(InvalidExternalPayloadError) as caught:
            await mb_base.mb_api_get("/artist/private-id", params={"inc": "secret"})
    finally:
        _restore_source(before)

    message = str(caught.value)
    assert "artist" not in message
    assert "private-id" not in message
    assert "secret" not in message
    assert "private-body" not in message
    assert "api.brainzmash.cc" not in message
