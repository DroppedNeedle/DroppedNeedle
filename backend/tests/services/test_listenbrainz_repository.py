import pytest
from unittest.mock import AsyncMock, MagicMock

import httpx

from core.exceptions import ExternalServiceError
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from repositories.listenbrainz_repository import ListenBrainzRepository


def _make_repo(username: str = "user", user_token: str = "tok-abc") -> tuple[ListenBrainzRepository, AsyncMock]:
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


def _ok_response(json_data=None):
    resp = MagicMock()
    resp.status_code = 200
    resp.json.return_value = json_data or {"status": "ok"}
    resp.text = ""
    return resp


class TestSubmitNowPlaying:
    @pytest.mark.asyncio
    async def test_posts_playing_now_payload(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        result = await repo.submit_now_playing(
            artist_name="Artist", track_name="Track"
        )
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
        track_meta = http_client.request.call_args.kwargs["json"]["payload"][0]["track_metadata"]
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
        track_meta = http_client.request.call_args.kwargs["json"]["payload"][0]["track_metadata"]
        assert track_meta["release_name"] == "Album"
        assert track_meta["additional_info"]["duration_ms"] == 180000

    @pytest.mark.asyncio
    async def test_omits_optional_when_empty(self):
        repo, http_client = _make_repo()
        http_client.request = AsyncMock(return_value=_ok_response())
        await repo.submit_single_listen(
            artist_name="A", track_name="T", listened_at=1700000000
        )
        track_meta = http_client.request.call_args.kwargs["json"]["payload"][0]["track_metadata"]
        assert "release_name" not in track_meta
        assert "additional_info" not in track_meta

    @pytest.mark.asyncio
    async def test_raises_without_token(self):
        repo, http_client = _make_repo(user_token="")
        with pytest.raises(ExternalServiceError, match="token required"):
            await repo.submit_single_listen(
                artist_name="A", track_name="T", listened_at=1700000000
            )

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
                500, '{"code":500,"error":"Popularity API currently disabled due to high load..."}'
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
