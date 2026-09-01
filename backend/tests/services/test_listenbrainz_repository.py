import asyncio
import time
import pytest
from unittest.mock import AsyncMock, MagicMock

import httpx

from core.exceptions import ExternalServiceError, RateLimitedError
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from infrastructure.resilience.retry import CircuitOpenError, CircuitState
from infrastructure.service_health import service_health
from repositories.listenbrainz_repository import (
    ListenBrainzRepository,
    _listenbrainz_circuit_breaker,
    _listenbrainz_rate_limit_state,
    _listenbrainz_rate_limiter,
    _parse_retry_after,
    _reset_listenbrainz_rate_limit_state,
    listenbrainz_rate_limit_cooldown_active,
)


@pytest.fixture(autouse=True)
def _clean_rate_limit_state():
    _reset_listenbrainz_rate_limit_state()
    yield
    _reset_listenbrainz_rate_limit_state()


def _make_repo(
    username: str = "user", user_token: str = "tok-abc"
) -> tuple[ListenBrainzRepository, AsyncMock]:
    http_client = AsyncMock(spec=httpx.AsyncClient)
    cache = MagicMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    repo = ListenBrainzRepository(
        http_client=http_client,
        cache=cache,
        username=username,
        user_token=user_token,
    )
    return repo, http_client


def _ok_response(json_data=None, headers=None):
    resp = MagicMock()
    resp.status_code = 200
    resp.headers = headers or {}
    resp.json.return_value = json_data or {"status": "ok"}
    resp.text = ""
    return resp


def test_baseline_limiter_is_evenly_paced_without_cold_burst():
    assert _listenbrainz_rate_limiter.rate == pytest.approx(2.5)
    assert _listenbrainz_rate_limiter.capacity == 1


def test_dynamic_headers_reserve_budget_and_expire_cooldown():
    now = [100.0]
    _listenbrainz_rate_limit_state._clock = lambda: now[0]
    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "2", "X-RateLimit-Reset-In": "10"}
    )

    assert _listenbrainz_rate_limit_state.reserve() is None
    assert _listenbrainz_rate_limit_state.reserve() is None
    blocked_for = _listenbrainz_rate_limit_state.reserve()
    assert blocked_for == pytest.approx(10.5)
    assert listenbrainz_rate_limit_cooldown_active() is True

    now[0] = 111.0
    assert listenbrainz_rate_limit_cooldown_active() is False


def test_out_of_order_remaining_never_restores_budget():
    now = [200.0]
    _listenbrainz_rate_limit_state._clock = lambda: now[0]
    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "4", "X-RateLimit-Reset-In": "10"}
    )
    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "1", "X-RateLimit-Reset-In": "8"}
    )
    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "3", "X-RateLimit-Reset-In": "9"}
    )

    assert _listenbrainz_rate_limit_state._remaining == 1


def test_rate_window_deadline_never_shortens_for_delayed_response():
    now = [100.0]
    _listenbrainz_rate_limit_state._clock = lambda: now[0]
    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "4", "X-RateLimit-Reset-In": "20"}
    )

    now[0] = 105.0
    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "3", "X-RateLimit-Reset-In": "30"}
    )
    assert _listenbrainz_rate_limit_state._window_reset_at == pytest.approx(135.0)

    now[0] = 106.0
    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "0", "X-RateLimit-Reset-In": "0.1"}
    )

    assert _listenbrainz_rate_limit_state._window_reset_at == pytest.approx(135.0)
    assert _listenbrainz_rate_limit_state.cooldown_remaining() == pytest.approx(29.5)


def test_zero_remaining_without_reset_uses_safe_default_cooldown():
    now = [100.0]
    _listenbrainz_rate_limit_state._clock = lambda: now[0]

    _listenbrainz_rate_limit_state.observe({"X-RateLimit-Remaining": "0"})

    assert _listenbrainz_rate_limit_state.cooldown_remaining() == pytest.approx(2.5)


def test_partial_rate_headers_do_not_restore_reserved_budget():
    now = [100.0]
    _listenbrainz_rate_limit_state._clock = lambda: now[0]
    _listenbrainz_rate_limit_state.observe({"X-RateLimit-Remaining": "5"})

    assert _listenbrainz_rate_limit_state.reserve() is None
    assert _listenbrainz_rate_limit_state.reserve() is None
    assert _listenbrainz_rate_limit_state._remaining == 3

    _listenbrainz_rate_limit_state.observe(
        {"X-RateLimit-Remaining": "4", "X-RateLimit-Reset-In": "10"}
    )

    assert _listenbrainz_rate_limit_state._remaining == 3


def test_short_cooldown_does_not_clear_later_rate_window():
    now = [100.0]
    original_health_clock = service_health._clock
    service_health.clear()
    service_health._clock = lambda: now[0]
    try:
        _listenbrainz_rate_limit_state._clock = lambda: now[0]
        _listenbrainz_rate_limit_state.observe(
            {"X-RateLimit-Remaining": "1", "X-RateLimit-Reset-In": "100"}
        )
        assert _listenbrainz_rate_limit_state.reserve() is None
        retry_response = MagicMock()
        retry_response.headers = {"Retry-After": "2"}
        _listenbrainz_rate_limit_state.activate_cooldown(
            _parse_retry_after(retry_response)
        )

        now[0] = 103.0
        assert _listenbrainz_rate_limit_state.cooldown_remaining() == 0.0
        assert _listenbrainz_rate_limit_state.cooldown_active() is True
        assert _listenbrainz_rate_limit_state._window_reset_at == pytest.approx(200.0)
        assert _listenbrainz_rate_limit_state._remaining == 0
        assert service_health.is_degraded("listenbrainz", "rate limit")

        now[0] = 200.4
        assert service_health.is_degraded("listenbrainz", "rate limit")
        assert _listenbrainz_rate_limit_state.cooldown_active() is True

        assert _listenbrainz_rate_limit_state.reserve() == pytest.approx(0.1)
    finally:
        service_health._clock = original_health_clock
        service_health.clear()


def test_retry_after_ignores_invalid_reset_in_and_uses_retry_after():
    response = MagicMock()
    response.headers = {"X-RateLimit-Reset-In": "0", "Retry-After": "7"}
    assert _parse_retry_after(response) == pytest.approx(7.0)

    response.headers = {"X-RateLimit-Reset-In": "malformed", "Retry-After": "7"}
    assert _parse_retry_after(response) == pytest.approx(7.0)


@pytest.mark.asyncio
async def test_header_exhaustion_fast_fails_followup_and_marks_health(monkeypatch):
    service_health.clear()
    monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", AsyncMock())
    repo, http_client = _make_repo()
    http_client.request = AsyncMock(
        return_value=_ok_response(
            headers={
                "X-RateLimit-Remaining": "0",
                "X-RateLimit-Reset-In": "10",
            }
        )
    )

    assert await repo._get("/first") == {"status": "ok"}
    with pytest.raises(RateLimitedError):
        await repo._get("/second")

    assert http_client.request.await_count == 1
    assert listenbrainz_rate_limit_cooldown_active() is True
    assert any(
        entry.service == "listenbrainz" and entry.capability == "rate limit"
        for entry in service_health.current()
    )


@pytest.mark.asyncio
async def test_unknown_window_reservation_prevents_concurrent_budget_double_consume(
    monkeypatch,
):
    monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", AsyncMock())
    repo, http_client = _make_repo()
    first_started = asyncio.Event()
    second_started = asyncio.Event()
    release_second = asyncio.Event()
    wire_attempts = 0

    async def request(*args, **kwargs):
        nonlocal wire_attempts
        wire_attempts += 1
        if wire_attempts == 1:
            first_started.set()
            await second_started.wait()
            return _ok_response(
                {"request": 1},
                headers={
                    "X-RateLimit-Remaining": "1",
                    "X-RateLimit-Reset-In": "30",
                },
            )
        second_started.set()
        await release_second.wait()
        return _ok_response(
            {"request": 2},
            headers={
                "X-RateLimit-Remaining": "0",
                "X-RateLimit-Reset-In": "30",
            },
        )

    http_client.request = AsyncMock(side_effect=request)
    first = asyncio.create_task(repo._get("/first"))
    await first_started.wait()
    second = asyncio.create_task(repo._get("/second"))
    await second_started.wait()

    assert await first == {"request": 1}
    assert _listenbrainz_rate_limit_state._remaining == 0
    assert _listenbrainz_rate_limit_state._unknown_in_flight == 1

    release_second.set()
    assert await second == {"request": 2}
    assert _listenbrainz_rate_limit_state._unknown_in_flight == 0
    assert http_client.request.await_count == 2


def test_breaker_health_uses_music_data_capability():
    service_health.clear()
    _listenbrainz_circuit_breaker.reset()
    for _ in range(_listenbrainz_circuit_breaker.failure_threshold):
        _listenbrainz_circuit_breaker.record_failure()

    assert any(
        entry.service == "listenbrainz" and entry.capability == "music data"
        for entry in service_health.current()
    )

    _listenbrainz_circuit_breaker.reset()
    assert not service_health.is_degraded("listenbrainz", "music data")


@pytest.mark.asyncio
async def test_username_validation_uses_shared_request_funnel(monkeypatch):
    monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", AsyncMock())
    repo, http_client = _make_repo(user_token="")
    http_client.request = AsyncMock(
        return_value=_ok_response({"payload": {"count": 12}})
    )

    valid, message = await repo.validate_username("alice")

    assert valid is True
    assert "12" in message
    assert http_client.request.await_count == 1
    assert "/1/user/alice/listen-count" in http_client.request.call_args.args[1]


@pytest.mark.parametrize(
    ("validator", "args"),
    [("validate_username", ("alice",)), ("validate_token", ())],
)
@pytest.mark.asyncio
async def test_validation_propagates_rate_limited_error(monkeypatch, validator, args):
    repo, _ = _make_repo()
    error = RateLimitedError("provider body sentinel", retry_after_seconds=7)
    request = AsyncMock(side_effect=error)
    monkeypatch.setattr(repo, "_get", request)

    with pytest.raises(RateLimitedError) as raised:
        await getattr(repo, validator)(*args)

    assert raised.value is error
    assert raised.value.retry_after_seconds == 7
    request.assert_awaited_once()


@pytest.mark.parametrize("status_code", [401, 403])
@pytest.mark.asyncio
async def test_validation_preserves_not_found_and_invalid_token_single_attempts(
    monkeypatch, status_code
):
    monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", AsyncMock())
    _listenbrainz_circuit_breaker.reset()
    repo, http_client = _make_repo()
    not_found = _ok_response()
    not_found.status_code = 404
    http_client.request = AsyncMock(return_value=not_found)

    valid, message = await repo.validate_username("missing")
    assert valid is False
    assert message == "User 'missing' not found"
    assert http_client.request.await_count == 1
    assert _listenbrainz_circuit_breaker.failure_count == 0
    assert _listenbrainz_circuit_breaker.success_count == 0

    # A neutral validation outcome must neither count as success nor close a
    # HALF_OPEN breaker probe.
    _listenbrainz_circuit_breaker.state = CircuitState.HALF_OPEN
    _listenbrainz_circuit_breaker.failure_count = 3
    _listenbrainz_circuit_breaker.success_count = 0
    invalid_token = _ok_response()
    invalid_token.status_code = status_code
    http_client.request = AsyncMock(return_value=invalid_token)
    valid, message = await repo.validate_token()
    assert valid is False
    assert "invalid" in message.lower()
    assert http_client.request.await_count == 1
    assert _listenbrainz_circuit_breaker.state is CircuitState.HALF_OPEN
    assert _listenbrainz_circuit_breaker.failure_count == 3
    assert _listenbrainz_circuit_breaker.success_count == 0
    _listenbrainz_circuit_breaker.reset()


@pytest.mark.parametrize("token", ["bad\ntransport-secret", "x" * 1025])
@pytest.mark.asyncio
async def test_malformed_token_rejected_before_limiter_or_wire(
    monkeypatch, caplog, token
):
    limiter = AsyncMock()
    monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", limiter)
    _listenbrainz_circuit_breaker.reset()
    repo, http_client = _make_repo(user_token=token)

    with caplog.at_level("ERROR"):
        valid, message = await repo.validate_token()

    assert valid is False
    assert message == "Token invalid or expired"
    limiter.assert_not_awaited()
    http_client.request.assert_not_awaited()
    assert token not in caplog.text
    assert _listenbrainz_circuit_breaker.failure_count == 0
    assert _listenbrainz_circuit_breaker.success_count == 0
    _listenbrainz_circuit_breaker.reset()


@pytest.mark.parametrize("token_source", ["configured", "borrowed"])
@pytest.mark.asyncio
async def test_malformed_public_read_token_precedes_open_breaker(
    monkeypatch, caplog, token_source
):
    limiter = AsyncMock()
    monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", limiter)
    _listenbrainz_circuit_breaker.reset()
    _listenbrainz_circuit_breaker.state = CircuitState.OPEN
    _listenbrainz_circuit_breaker.failure_count = 3
    _listenbrainz_circuit_breaker.success_count = 2
    _listenbrainz_circuit_breaker.last_failure_time = time.time()
    _listenbrainz_circuit_breaker._last_open_warning = 0.0

    malformed = f"{token_source}-token\ntransport-secret"
    if token_source == "configured":
        repo, http_client = _make_repo(user_token=malformed)
        provider = None
    else:
        repo, http_client = _make_repo(user_token="")
        provider = AsyncMock(return_value=malformed)
        repo._fallback_token_provider = provider
        repo._fallback_resolved = False

    http_client.request = AsyncMock(return_value=_ok_response())
    breaker_snapshot = (
        _listenbrainz_circuit_breaker.state,
        _listenbrainz_circuit_breaker.failure_count,
        _listenbrainz_circuit_breaker.success_count,
        _listenbrainz_circuit_breaker.last_failure_time,
        _listenbrainz_circuit_breaker._last_open_warning,
    )

    try:
        with caplog.at_level("WARNING"):
            for request in (
                lambda: repo._get("/1/user/alice/listens"),
                lambda: repo._post(
                    "/1/metadata/recording/",
                    {"recording_mbids": ["recording-1"]},
                ),
            ):
                with pytest.raises(ExternalServiceError) as raised:
                    await request()
                assert str(raised.value) == "ListenBrainz credentials rejected"

        assert "Circuit breaker 'listenbrainz' is OPEN" not in caplog.text
        assert (
            _listenbrainz_circuit_breaker.state,
            _listenbrainz_circuit_breaker.failure_count,
            _listenbrainz_circuit_breaker.success_count,
            _listenbrainz_circuit_breaker.last_failure_time,
            _listenbrainz_circuit_breaker._last_open_warning,
        ) == breaker_snapshot
        limiter.assert_not_awaited()
        http_client.request.assert_not_awaited()
        if provider is not None:
            provider.assert_awaited_once()
    finally:
        _listenbrainz_circuit_breaker.reset()


@pytest.mark.asyncio
async def test_transport_and_upstream_body_details_are_not_logged_or_raised(
    monkeypatch, caplog
):
    monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", AsyncMock())
    _listenbrainz_circuit_breaker.reset()
    repo, http_client = _make_repo()
    transport_secret = "transport-secret-sentinel"
    http_client.request = AsyncMock(side_effect=httpx.ReadTimeout(transport_secret))

    with caplog.at_level("ERROR"), pytest.raises(ExternalServiceError) as raised:
        await repo.get_user_listens("alice")

    assert transport_secret not in str(raised.value)
    assert transport_secret not in caplog.text

    upstream_secret = "upstream-body-secret-sentinel"
    response = _ok_response()
    response.status_code = 500
    response.text = upstream_secret
    http_client.request = AsyncMock(return_value=response)
    caplog.clear()
    with caplog.at_level("ERROR"), pytest.raises(ExternalServiceError) as raised:
        await repo.get_user_listens("alice")

    assert upstream_secret not in str(raised.value)
    assert upstream_secret not in caplog.text
    _listenbrainz_circuit_breaker.reset()


class TestSubmitNowPlaying:
    @pytest.mark.asyncio
    async def test_posts_playing_now_payload(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        result = await repo.submit_now_playing(artist_name="Artist", track_name="Track")
        assert result is True
        call_args = http_client.request.call_args
        assert call_args.args[0] == "POST"
        assert "/1/submit-listens" in call_args.args[1]
        payload = call_args.kwargs["json"]
        assert payload["listen_type"] == "playing_now"
        assert len(payload["payload"]) == 1
        track_meta = payload["payload"][0]["track_metadata"]
        assert track_meta["artist_name"] == "Artist"
        assert track_meta["track_name"] == "Track"

    @pytest.mark.asyncio
    async def test_includes_release_name(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        await repo.submit_now_playing(
            artist_name="A", track_name="T", release_name="Album"
        )
        payload = http_client.request.call_args.kwargs["json"]
        assert payload["payload"][0]["track_metadata"]["release_name"] == "Album"

    @pytest.mark.asyncio
    async def test_includes_duration_ms(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        await repo.submit_now_playing(
            artist_name="A", track_name="T", duration_ms=200000
        )
        payload = http_client.request.call_args.kwargs["json"]
        additional = payload["payload"][0]["track_metadata"]["additional_info"]
        assert additional["duration_ms"] == 200000

    @pytest.mark.asyncio
    async def test_omits_optional_when_empty(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        await repo.submit_now_playing(artist_name="A", track_name="T")
        track_meta = http_client.request.call_args.kwargs["json"]["payload"][0][
            "track_metadata"
        ]
        assert "release_name" not in track_meta
        assert "additional_info" not in track_meta

    @pytest.mark.asyncio
    async def test_sends_auth_header(self):
        repo, http_client = _make_repo(user_token="my-token")
        http_client.request = AsyncMock(return_value=_ok_response())
        await repo.submit_now_playing(artist_name="A", track_name="T")
        headers = http_client.request.call_args.kwargs["headers"]
        assert headers["Authorization"] == "Token my-token"

    @pytest.mark.asyncio
    async def test_raises_without_token(self):
        repo, http_client = _make_repo(user_token="")
        with pytest.raises(ExternalServiceError, match="token required"):
            await repo.submit_now_playing(artist_name="A", track_name="T")
        http_client.request.assert_not_awaited()


class TestSubmitSingleListen:
    @pytest.mark.asyncio
    async def test_posts_single_listen_payload(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        result = await repo.submit_single_listen(
            artist_name="Artist",
            track_name="Track",
            listened_at=1700000000,
        )
        assert result is True
        payload = http_client.request.call_args.kwargs["json"]
        assert payload["listen_type"] == "single"
        listen = payload["payload"][0]
        assert listen["listened_at"] == 1700000000
        assert listen["track_metadata"]["artist_name"] == "Artist"
        assert listen["track_metadata"]["track_name"] == "Track"

    @pytest.mark.asyncio
    async def test_includes_release_and_duration(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        await repo.submit_single_listen(
            artist_name="A",
            track_name="T",
            listened_at=1700000000,
            release_name="Album",
            duration_ms=180000,
        )
        track_meta = http_client.request.call_args.kwargs["json"]["payload"][0][
            "track_metadata"
        ]
        assert track_meta["release_name"] == "Album"
        assert track_meta["additional_info"]["duration_ms"] == 180000

    @pytest.mark.asyncio
    async def test_omits_optional_when_empty(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        await repo.submit_single_listen(
            artist_name="A", track_name="T", listened_at=1700000000
        )
        track_meta = http_client.request.call_args.kwargs["json"]["payload"][0][
            "track_metadata"
        ]
        assert "release_name" not in track_meta
        assert "additional_info" not in track_meta

    @pytest.mark.asyncio
    async def test_raises_without_token(self):
        repo, http_client = _make_repo(user_token="")
        with pytest.raises(ExternalServiceError, match="token required"):
            await repo.submit_single_listen(
                artist_name="A", track_name="T", listened_at=1700000000
            )
        http_client.request.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_raises_on_http_error(self):
        repo, http_client = _make_repo()
        error_resp = MagicMock()
        error_resp.status_code = 500
        error_resp.text = "Internal Server Error"
        http_client.request = AsyncMock(return_value=error_resp)
        with pytest.raises(ExternalServiceError):
            await repo.submit_single_listen(
                artist_name="A", track_name="T", listened_at=1700000000
            )


class TestUpstreamPolicyBlocks:
    """LB's deterministic outage responses (popularity 500 'currently disabled', and
    the 2026-07 anti-scraper 401) must fail fast without tripping the SHARED breaker,
    so one token-less caller can't blind every other LB feature."""

    def _resp(self, status: int, text: str):
        resp = MagicMock()
        resp.status_code = status
        resp.text = text
        resp.json.return_value = {}
        return resp

    @pytest.mark.asyncio
    async def test_anti_scraper_401_is_non_breaking(self):
        from repositories.listenbrainz_repository import (
            _listenbrainz_circuit_breaker,
            ServiceDisabledUpstreamError,
        )
        from infrastructure.service_health import service_health

        service_health.clear()
        _listenbrainz_circuit_breaker.reset()
        repo, http_client = _make_repo(user_token="")
        http_client.request = AsyncMock(
            return_value=self._resp(
                401,
                '{"error":"Due to AI scrapers causing undue traffic on our sites, '
                'please provide an Auth token. Sorry for this mess."}',
            )
        )

        with pytest.raises(ServiceDisabledUpstreamError):
            await repo.get_release_group_popularity_batch(["rg-1"])

        # one attempt, no retry storm, breaker still closed
        assert http_client.request.await_count == 1
        assert not _listenbrainz_circuit_breaker.is_open()
        service_health.clear()

    @pytest.mark.asyncio
    async def test_popularity_disabled_500_is_non_breaking(self):
        from repositories.listenbrainz_repository import (
            _listenbrainz_circuit_breaker,
            ServiceDisabledUpstreamError,
        )
        from infrastructure.service_health import service_health

        service_health.clear()
        _listenbrainz_circuit_breaker.reset()
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(
            return_value=self._resp(
                500,
                '{"code":500,"error":"Popularity API currently disabled due to high load..."}',
            )
        )

        with pytest.raises(ServiceDisabledUpstreamError):
            await repo.get_artist_top_recordings("artist-1")
        assert http_client.request.await_count == 1
        assert not _listenbrainz_circuit_breaker.is_open()
        service_health.clear()

    @pytest.mark.asyncio
    async def test_known_popularity_outage_short_circuits_followup_calls(self):
        from repositories.listenbrainz_repository import (
            _mark_popularity_degraded,
        )
        from infrastructure.service_health import service_health

        service_health.clear()
        _mark_popularity_degraded()
        degradation = init_degradation_context()

        try:
            repo, http_client = _make_repo()
            http_client.request = AsyncMock(return_value=self._resp(200, "[]"))
            result = await repo.get_artist_top_recordings("artist-1")

            assert result == []
            assert degradation.degraded_summary() == {"listenbrainz": "error"}
            http_client.request.assert_not_awaited()
        finally:
            clear_degradation_context()
            service_health.clear()

    @pytest.mark.asyncio
    async def test_genuine_500_still_breaks(self):
        # a non-policy 500 remains a real error (retried, counts toward the breaker)
        _make_repo()
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=self._resp(500, "internal error"))

        with pytest.raises(ExternalServiceError):
            await repo.get_artist_top_recordings("artist-1")
        assert http_client.request.await_count > 1  # retried


class TestBorrowedReadToken:
    """A tokenless global/enrichment repo borrows a connected account's token to
    authenticate PUBLIC reads (LB's anti-scraper gate), but NEVER for writes."""

    def _list_response(self, items):
        resp = MagicMock()
        resp.status_code = 200
        resp.content = None
        resp.json.return_value = items
        resp.text = ""
        return resp

    @pytest.mark.asyncio
    async def test_read_uses_borrowed_token(self):
        repo, http_client = _make_repo(user_token="")

        async def provider():
            return "borrowed-tok"

        repo._fallback_token_provider = provider
        repo._fallback_resolved = False
        http_client.request = AsyncMock(return_value=self._list_response([]))

        await repo.get_release_group_popularity_batch(["rg-1"])

        sent_headers = http_client.request.await_args.kwargs["headers"]
        assert sent_headers.get("Authorization") == "Token borrowed-tok"

    @pytest.mark.asyncio
    async def test_concurrent_cold_reads_share_one_fallback_resolution(
        self, monkeypatch
    ):
        monkeypatch.setattr(_listenbrainz_rate_limiter, "acquire", AsyncMock())
        repo, http_client = _make_repo(user_token="")
        provider_started = asyncio.Event()
        release_provider = asyncio.Event()
        provider_calls = 0

        async def provider():
            nonlocal provider_calls
            provider_calls += 1
            provider_started.set()
            await release_provider.wait()
            return "borrowed+token/_valid"

        repo._fallback_token_provider = provider
        repo._fallback_resolved = False
        http_client.request = AsyncMock(
            side_effect=[
                self._list_response({"first": True}),
                self._list_response({"second": True}),
            ]
        )

        first = asyncio.create_task(repo._get("/first"))
        second = asyncio.create_task(repo._get("/second"))
        await provider_started.wait()
        await asyncio.sleep(0)
        assert provider_calls == 1
        release_provider.set()
        await asyncio.gather(first, second)

        assert provider_calls == 1
        assert http_client.request.await_count == 2
        assert all(
            call.kwargs["headers"].get("Authorization") == "Token borrowed+token/_valid"
            for call in http_client.request.await_args_list
        )

    @pytest.mark.asyncio
    async def test_write_never_borrows_a_token(self):
        # submitting a listen with someone else's token would write to the WRONG
        # account - require_auth must stay strict and reject
        repo, http_client = _make_repo(user_token="")

        async def provider():
            return "borrowed-tok"

        repo._fallback_token_provider = provider
        repo._fallback_resolved = False
        http_client.request = AsyncMock(return_value=self._list_response([]))

        with pytest.raises(ExternalServiceError):
            await repo.submit_now_playing("Artist", "Track")
        http_client.request.assert_not_called()


class TestStaleChartServing:
    """QW11 Part 3: while the 'listenbrainz' breaker is OPEN, chart/stats
    getters serve the expired cache entry (flagged via DegradationContext)
    instead of rendering empty; without any stale entry the CircuitOpenError
    propagates exactly as before."""

    def _breaker_open_repo(self) -> tuple[ListenBrainzRepository, MagicMock]:
        repo, http_client = _make_repo()
        cache = MagicMock()
        cache.get = AsyncMock(return_value=None)  # ordinary read misses
        cache.peek = AsyncMock(return_value=None)
        repo._cache = cache

        async def circuit_open(*args, **kwargs):
            raise CircuitOpenError(
                "Circuit breaker 'listenbrainz' is OPEN",
                breaker_name="listenbrainz",
                retry_after_seconds=42.0,
            )

        repo._get = circuit_open
        return repo, cache

    def _recording_context(self, monkeypatch) -> list:
        """Install a real DegradationContext wrapped by a record-spy, since
        DegradationContext is slotted and its methods cannot be patched."""
        recorded: list = []

        class _RecordSpy:
            def __init__(self, inner):
                self._inner = inner

            def record(self, result):
                recorded.append(result)
                self._inner.record(result)

            def __getattr__(self, name):
                return getattr(self._inner, name)

        ctx = init_degradation_context()
        monkeypatch.setattr(
            "repositories.listenbrainz_repository.try_get_degradation_context",
            lambda: _RecordSpy(ctx),
        )
        return recorded

    @pytest.mark.asyncio
    async def test_open_breaker_serves_stale_sitewide_artists_with_record(
        self, monkeypatch
    ):
        recorded = self._recording_context(monkeypatch)
        repo, cache = self._breaker_open_repo()
        stale_payload = [MagicMock(name="stale-artist")]
        cache.peek = AsyncMock(return_value=stale_payload)

        result = await repo.get_sitewide_top_artists()

        assert result is stale_payload
        # The degradation record IS the stale flag: a fallback without one is
        # the anti-pattern this test exists to prevent.
        assert len(recorded) == 1
        assert "stale" in recorded[0].error_message.lower()
        assert recorded[0].source == "listenbrainz"

    @pytest.mark.asyncio
    async def test_open_breaker_serves_stale_release_groups_and_recordings(
        self, monkeypatch
    ):
        recorded = self._recording_context(monkeypatch)
        repo, cache = self._breaker_open_repo()
        stale_groups = [MagicMock(name="stale-rg")]
        cache.peek = AsyncMock(return_value=stale_groups)

        assert await repo.get_sitewide_top_release_groups() is stale_groups
        assert "release-groups" in recorded[-1].error_message

        stale_recs = [MagicMock(name="stale-recording")]
        cache.peek = AsyncMock(return_value=stale_recs)
        assert await repo.get_sitewide_top_recordings() is stale_recs
        assert "recordings" in recorded[-1].error_message

    @pytest.mark.asyncio
    async def test_open_breaker_serves_stale_user_genre_activity(self, monkeypatch):
        recorded = self._recording_context(monkeypatch)
        repo, cache = self._breaker_open_repo()
        stale_genres = [MagicMock(name="stale-genre")]
        cache.peek = AsyncMock(return_value=stale_genres)

        result = await repo.get_user_genre_activity(username="user")
        assert result is stale_genres
        assert "genre activity" in recorded[-1].error_message

    @pytest.mark.asyncio
    async def test_no_stale_entry_reraises_circuit_open_error(self, monkeypatch):
        self._recording_context(monkeypatch)
        repo, cache = self._breaker_open_repo()
        cache.peek = AsyncMock(return_value=None)

        with pytest.raises(CircuitOpenError):
            await repo.get_sitewide_top_artists()

    @pytest.mark.asyncio
    async def test_fallback_without_degradation_record_fails_the_contract(
        self, monkeypatch
    ):
        """Guard the AGENTS.md rule directly: if a stale payload is served,
        a degradation record MUST exist. Simulate a broken implementation by
        asserting on both sides of the contract."""
        recorded = self._recording_context(monkeypatch)
        repo, cache = self._breaker_open_repo()
        stale_payload = [MagicMock()]
        cache.peek = AsyncMock(return_value=stale_payload)

        result = await repo.get_sitewide_top_artists()

        served_stale = result is stale_payload
        recorded_stale = any("stale" in r.error_message.lower() for r in recorded)
        # Contract: stale service and degradation record are inseparable.
        assert served_stale == recorded_stale == True  # noqa: E712

    @pytest.mark.asyncio
    async def test_ordinary_hit_short_circuits_before_breaker_path(self):
        repo, cache = self._breaker_open_repo()
        fresh_payload = [MagicMock(name="fresh")]
        cache.get = AsyncMock(return_value=fresh_payload)

        assert await repo.get_sitewide_top_artists() is fresh_payload
        cache.peek.assert_not_awaited()

    def teardown_method(self):
        clear_degradation_context()
