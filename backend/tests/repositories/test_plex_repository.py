from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest

from core.exceptions import ExternalServiceError, PlexApiError, PlexAuthError
from repositories.plex_models import (
    PlexAlbum,
    PlexArtist,
    PlexLibrarySection,
    PlexOAuthPin,
    PlexTrack,
    StreamProxyResult,
)
from repositories.plex_repository import PlexRepository, _plex_circuit_breaker


def _make_cache() -> MagicMock:
    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    cache.clear_prefix = AsyncMock(return_value=0)
    return cache


def _make_repo(configured: bool = True) -> tuple[PlexRepository, AsyncMock, MagicMock]:
    client = AsyncMock(spec=httpx.AsyncClient)
    cache = _make_cache()
    repo = PlexRepository(http_client=client, cache=cache)
    if configured:
        repo.configure("http://plex:32400", "test-token", "client-id-123")
    _plex_circuit_breaker.reset()
    return repo, client, cache


def _mock_response(
    json_data: dict | None = None,
    status_code: int = 200,
    content: bytes = b"",
    content_type: str = "application/json",
) -> MagicMock:
    resp = MagicMock(spec=httpx.Response)
    resp.status_code = status_code
    resp.json.return_value = json_data or {}
    resp.content = content
    resp.headers = {"content-type": content_type}
    resp.history = []  # real httpx.Response sets this; no redirects by default
    return resp


def _plex_container(metadata: list | None = None, total_size: int | None = None, directory: list | None = None) -> dict:
    container: dict = {"MediaContainer": {}}
    if metadata is not None:
        container["MediaContainer"]["Metadata"] = metadata
    if total_size is not None:
        container["MediaContainer"]["totalSize"] = total_size
    if directory is not None:
        container["MediaContainer"]["Directory"] = directory
    return container


def _plex_hub_container(
    albums: list | None = None,
    tracks: list | None = None,
    artists: list | None = None,
) -> dict:
    """Build a /hubs/search style response with typed Hub[] arrays."""
    hubs: list[dict] = []
    for hub_type, items in [("album", albums), ("track", tracks), ("artist", artists)]:
        hub: dict = {"type": hub_type, "size": len(items) if items else 0}
        if items:
            hub["Metadata"] = items
        hubs.append(hub)
    return {"MediaContainer": {"Hub": hubs}}


class TestConfigure:
    def test_configure_sets_state(self):
        repo, _, _ = _make_repo(configured=False)
        assert repo.is_configured() is False

        repo.configure("http://plex:32400", "my-token", "my-client")
        assert repo.is_configured() is True

    def test_configure_strips_trailing_slash(self):
        repo, _, _ = _make_repo(configured=False)
        repo.configure("http://plex:32400/", "tok")
        assert repo._url == "http://plex:32400"

    def test_configure_empty_url_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        repo.configure("", "tok")
        assert repo.is_configured() is False

    def test_configure_empty_token_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        repo.configure("http://plex:32400", "")
        assert repo.is_configured() is False


class TestBuildHeaders:
    def test_contains_required_keys(self):
        repo, _, _ = _make_repo()
        headers = repo._build_headers()
        assert headers["X-Plex-Token"] == "test-token"
        assert headers["X-Plex-Product"] == "DroppedNeedle"
        assert headers["X-Plex-Version"] == "1.0"
        assert headers["Accept"] == "application/json"

    def test_client_identifier_included_when_set(self):
        repo, _, _ = _make_repo()
        headers = repo._build_headers()
        assert headers["X-Plex-Client-Identifier"] == "client-id-123"

    def test_client_identifier_omitted_when_empty(self):
        repo, _, _ = _make_repo(configured=False)
        repo.configure("http://plex:32400", "tok", "")
        headers = repo._build_headers()
        assert "X-Plex-Client-Identifier" not in headers


class TestBaseUrlUpgrade:
    """A Plex behind a TLS-terminating proxy 302s http->https. We persist the
    upgrade once so later polls go straight to https instead of re-paying the
    redirect every time."""

    @staticmethod
    def _redirected(final_url: str) -> MagicMock:
        resp = _mock_response(status_code=200, json_data=_plex_container())
        resp.history = [MagicMock()]  # a redirect was followed
        resp.url = httpx.URL(final_url)
        return resp

    @pytest.mark.asyncio
    async def test_http_to_https_same_host_persists_upgrade(self):
        repo, client, _ = _make_repo()  # configured as http://plex:32400
        client.get = AsyncMock(
            return_value=self._redirected("https://plex:32400/status/sessions")
        )
        await repo._request("/status/sessions")
        assert repo._url == "https://plex:32400"

    @pytest.mark.asyncio
    async def test_no_redirect_leaves_url_unchanged(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(
            return_value=_mock_response(status_code=200, json_data=_plex_container())
        )
        await repo._request("/status/sessions")
        assert repo._url == "http://plex:32400"

    @pytest.mark.asyncio
    async def test_cross_host_redirect_not_persisted(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(
            return_value=self._redirected("https://evil.example/status/sessions")
        )
        await repo._request("/status/sessions")
        assert repo._url == "http://plex:32400"

    @pytest.mark.asyncio
    async def test_subsequent_request_uses_upgraded_base(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(
            return_value=self._redirected("https://plex:32400/status/sessions")
        )
        await repo._request("/status/sessions")
        await repo._request("/status/sessions")
        assert client.get.await_args_list[-1].args[0] == "https://plex:32400/status/sessions"

    @pytest.mark.asyncio
    async def test_subpath_base_is_preserved(self):
        repo, client, _ = _make_repo(configured=False)
        repo.configure("http://proxy.example/plex", "test-token")
        client.get = AsyncMock(
            return_value=self._redirected("https://proxy.example/plex/status/sessions")
        )
        await repo._request("/status/sessions")
        assert repo._url == "https://proxy.example/plex"

    @pytest.mark.asyncio
    async def test_port_changing_redirect_not_persisted(self):
        repo, client, _ = _make_repo()  # http://plex:32400
        client.get = AsyncMock(
            return_value=self._redirected("https://plex/status/sessions")  # 32400 -> 443
        )
        await repo._request("/status/sessions")
        assert repo._url == "http://plex:32400"


class TestCacheTTLs:
    def test_configure_cache_ttls_sets_values(self):
        repo, _, _ = _make_repo()
        repo.configure_cache_ttls(list_ttl=60, search_ttl=30, genres_ttl=120, detail_ttl=90, stats_ttl=45)
        assert repo._ttl_list == 60
        assert repo._ttl_search == 30
        assert repo._ttl_genres == 120
        assert repo._ttl_detail == 90
        assert repo._ttl_stats == 45

    def test_configure_cache_ttls_none_keeps_defaults(self):
        repo, _, _ = _make_repo()
        original_list = repo._ttl_list
        repo.configure_cache_ttls()
        assert repo._ttl_list == original_list


class TestPing:
    @pytest.mark.asyncio
    async def test_ping_success(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=200))
        assert await repo.ping() is True

    @pytest.mark.asyncio
    async def test_ping_failure_status(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=500))
        assert await repo.ping() is False

    @pytest.mark.asyncio
    async def test_ping_exception(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.ConnectError("refused"))
        assert await repo.ping() is False

    @pytest.mark.asyncio
    async def test_ping_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        assert await repo.ping() is False


class TestGetLibraries:
    @pytest.mark.asyncio
    async def test_returns_sections(self):
        repo, client, cache = _make_repo()
        response_data = _plex_container(directory=[
            {"key": "1", "title": "Music", "type": "artist"},
            {"key": "2", "title": "Movies", "type": "movie"},
        ])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        sections = await repo.get_libraries()
        assert len(sections) >= 1
        cache.set.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_returns_cached(self):
        repo, client, cache = _make_repo()
        cached_sections = [PlexLibrarySection(key="1", title="Music", type="artist")]
        cache.get = AsyncMock(return_value=cached_sections)
        result = await repo.get_libraries()
        assert result == cached_sections
        client.get.assert_not_called()


class TestGetMusicLibraries:
    @pytest.mark.asyncio
    async def test_filters_to_artist_type(self):
        repo, client, cache = _make_repo()
        response_data = _plex_container(directory=[
            {"key": "1", "title": "Music", "type": "artist"},
            {"key": "2", "title": "Movies", "type": "movie"},
        ])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        sections = await repo.get_music_libraries()
        assert all(s.type == "artist" for s in sections)


class TestGetAlbums:
    @pytest.mark.asyncio
    async def test_returns_albums_and_total(self):
        repo, client, cache = _make_repo()
        response_data = _plex_container(
            metadata=[
                {"ratingKey": "100", "title": "Album1", "type": "album", "parentTitle": "Artist1"},
            ],
            total_size=42,
        )
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        albums, total = await repo.get_albums("1")
        assert len(albums) == 1
        assert total == 42

    @pytest.mark.asyncio
    async def test_empty_response(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(metadata=[], total_size=0)
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        albums, total = await repo.get_albums("1")
        assert albums == []
        assert total == 0

    @pytest.mark.asyncio
    async def test_uses_cache(self):
        repo, client, cache = _make_repo()
        cached = ([PlexAlbum(ratingKey="100", title="Cached")], 1)
        cache.get = AsyncMock(return_value=cached)
        result = await repo.get_albums("1")
        assert result == cached
        client.get.assert_not_called()


class TestGetArtists:
    @pytest.mark.asyncio
    async def test_returns_artists(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(metadata=[
            {"ratingKey": "50", "title": "Artist1", "type": "artist"},
        ])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        artists = await repo.get_artists("1")
        assert len(artists) == 1

    @pytest.mark.asyncio
    async def test_empty(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(metadata=[])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        artists = await repo.get_artists("1")
        assert artists == []


class TestGetTrackCount:
    @pytest.mark.asyncio
    async def test_returns_total(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(total_size=500)
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        count = await repo.get_track_count("1")
        assert count == 500


class TestGetArtistCount:
    @pytest.mark.asyncio
    async def test_returns_total(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(total_size=42)
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        count = await repo.get_artist_count("1")
        assert count == 42

    @pytest.mark.asyncio
    async def test_uses_cache(self):
        repo, client, cache = _make_repo()
        cache.get = AsyncMock(return_value=10)
        count = await repo.get_artist_count("1")
        assert count == 10
        client.get.assert_not_called()


class TestGetGenres:
    @pytest.mark.asyncio
    async def test_returns_genre_titles(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(directory=[
            {"title": "Rock"},
            {"title": "Jazz"},
            {"title": ""},
        ])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        genres = await repo.get_genres("1")
        assert "Rock" in genres
        assert "Jazz" in genres
        assert "" not in genres


class TestGetRecentlyViewed:
    @pytest.mark.asyncio
    async def test_returns_albums_with_lastviewedat(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(metadata=[
            {"ratingKey": "10", "title": "Played Album", "parentTitle": "Artist", "lastViewedAt": 1700000000},
        ])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        albums = await repo.get_recently_viewed("1", limit=5)
        assert len(albums) == 1
        assert albums[0].title == "Played Album"
        assert albums[0].lastViewedAt == 1700000000

    @pytest.mark.asyncio
    async def test_empty_when_no_history(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(metadata=[])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        albums = await repo.get_recently_viewed("1")
        assert albums == []

    @pytest.mark.asyncio
    async def test_calls_recentlyviewed_endpoint(self):
        repo, client, _ = _make_repo()
        response_data = _plex_container(metadata=[])
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        await repo.get_recently_viewed("1")
        call_args = client.get.call_args
        assert "recentlyViewed" in str(call_args)


class TestSearch:
    @pytest.mark.asyncio
    async def test_categorizes_results(self):
        repo, client, _ = _make_repo()
        response_data = _plex_hub_container(
            albums=[{"ratingKey": "1", "title": "Album X", "type": "album", "parentTitle": "A"}],
            tracks=[{"ratingKey": "2", "title": "Track Y", "type": "track"}],
            artists=[{"ratingKey": "3", "title": "Artist Z", "type": "artist"}],
        )
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        result = await repo.search("test", section_id="1")
        assert len(result["albums"]) == 1
        assert len(result["tracks"]) == 1
        assert len(result["artists"]) == 1

    @pytest.mark.asyncio
    async def test_global_search_without_section(self):
        repo, client, _ = _make_repo()
        response_data = _plex_hub_container()
        client.get = AsyncMock(return_value=_mock_response(json_data=response_data))
        await repo.search("test")
        call_args = client.get.call_args
        assert "/hubs/search" in str(call_args)


class TestScrobble:
    @pytest.mark.asyncio
    async def test_success(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=200))
        assert await repo.scrobble("12345") is True

    @pytest.mark.asyncio
    async def test_failure_status(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=500))
        assert await repo.scrobble("12345") is False

    @pytest.mark.asyncio
    async def test_exception_returns_false(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.ConnectError("down"))
        assert await repo.scrobble("12345") is False

    @pytest.mark.asyncio
    async def test_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        assert await repo.scrobble("12345") is False


class TestNowPlaying:
    @pytest.mark.asyncio
    async def test_success(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=200))
        assert await repo.now_playing("12345") is True

    @pytest.mark.asyncio
    async def test_exception_returns_false(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.TimeoutException("slow"))
        assert await repo.now_playing("12345") is False

    @pytest.mark.asyncio
    async def test_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        assert await repo.now_playing("12345") is False


class TestBuildStreamUrl:
    def test_builds_url(self):
        repo, _, _ = _make_repo()
        track = MagicMock(spec=PlexTrack)
        part = MagicMock()
        part.key = "/library/parts/100/1234/file.flac"
        media = MagicMock()
        media.Part = [part]
        track.Media = [media]
        track.ratingKey = "99"
        url = repo.build_stream_url(track)
        assert url == "http://plex:32400/library/parts/100/1234/file.flac"

    def test_raises_when_no_media(self):
        repo, _, _ = _make_repo()
        track = MagicMock(spec=PlexTrack)
        track.Media = []
        track.ratingKey = "99"
        with pytest.raises(ValueError, match="no streamable media"):
            repo.build_stream_url(track)

    def test_raises_when_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        track = MagicMock(spec=PlexTrack)
        with pytest.raises(ValueError, match="not configured"):
            repo.build_stream_url(track)


class TestProxyThumb:
    @pytest.mark.asyncio
    async def test_returns_bytes_and_content_type(self):
        repo, client, _ = _make_repo()
        image_bytes = b"\x89PNG\r\n"
        resp = _mock_response(status_code=200, content=image_bytes, content_type="image/png")
        client.get = AsyncMock(return_value=resp)
        content, ctype = await repo.proxy_thumb("123", size=300)
        assert content == image_bytes
        assert ctype == "image/png"

    @pytest.mark.asyncio
    async def test_raises_on_non_200(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=404))
        with pytest.raises(ExternalServiceError, match="failed"):
            await repo.proxy_thumb("123")

    @pytest.mark.asyncio
    async def test_raises_on_timeout(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.TimeoutException("slow"))
        with pytest.raises(ExternalServiceError, match="timed out"):
            await repo.proxy_thumb("123")

    @pytest.mark.asyncio
    async def test_raises_when_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        with pytest.raises(ExternalServiceError, match="not configured"):
            await repo.proxy_thumb("123")


class TestValidateConnection:
    @pytest.mark.asyncio
    async def test_success(self):
        repo, client, _ = _make_repo()
        json_data = {"MediaContainer": {"friendlyName": "My Plex", "version": "1.40"}}
        client.get = AsyncMock(return_value=_mock_response(json_data=json_data))
        ok, msg = await repo.validate_connection()
        assert ok is True
        assert "My Plex" in msg
        assert "1.40" in msg

    @pytest.mark.asyncio
    async def test_auth_failure(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=401))
        ok, msg = await repo.validate_connection()
        assert ok is False
        assert "authentication" in msg.lower() or "token" in msg.lower()

    @pytest.mark.asyncio
    async def test_timeout(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.TimeoutException("slow"))
        ok, msg = await repo.validate_connection()
        assert ok is False
        assert "timed out" in msg.lower()

    @pytest.mark.asyncio
    async def test_connect_error(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.ConnectError("Connection refused"))
        ok, msg = await repo.validate_connection()
        assert ok is False

    @pytest.mark.asyncio
    async def test_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        ok, msg = await repo.validate_connection()
        assert ok is False
        assert "not configured" in msg.lower()


class TestOAuthPin:
    @pytest.mark.asyncio
    async def test_create_pin(self):
        repo, _, _ = _make_repo()
        mock_response = MagicMock(spec=httpx.Response)
        mock_response.status_code = 201
        mock_response.json.return_value = {"id": 42, "code": "ABCD1234"}

        with patch("httpx.AsyncClient") as MockClient:
            instance = AsyncMock()
            instance.__aenter__ = AsyncMock(return_value=instance)
            instance.__aexit__ = AsyncMock(return_value=False)
            instance.post = AsyncMock(return_value=mock_response)
            MockClient.return_value = instance

            pin = await repo.create_oauth_pin("client-123")
            assert pin.id == 42
            assert pin.code == "ABCD1234"

    @pytest.mark.asyncio
    async def test_create_pin_failure(self):
        repo, _, _ = _make_repo()
        mock_response = MagicMock(spec=httpx.Response)
        mock_response.status_code = 500

        with patch("httpx.AsyncClient") as MockClient:
            instance = AsyncMock()
            instance.__aenter__ = AsyncMock(return_value=instance)
            instance.__aexit__ = AsyncMock(return_value=False)
            instance.post = AsyncMock(return_value=mock_response)
            MockClient.return_value = instance

            with pytest.raises(PlexApiError, match="Failed to create"):
                await repo.create_oauth_pin("client-123")

    @pytest.mark.asyncio
    async def test_poll_pin_token_found(self):
        repo, _, _ = _make_repo()
        mock_response = MagicMock(spec=httpx.Response)
        mock_response.status_code = 200
        mock_response.json.return_value = {"authToken": "fresh-token"}

        with patch("httpx.AsyncClient") as MockClient:
            instance = AsyncMock()
            instance.__aenter__ = AsyncMock(return_value=instance)
            instance.__aexit__ = AsyncMock(return_value=False)
            instance.get = AsyncMock(return_value=mock_response)
            MockClient.return_value = instance

            token = await repo.poll_oauth_pin(42, "client-123")
            assert token == "fresh-token"

    @pytest.mark.asyncio
    async def test_poll_pin_not_ready(self):
        repo, _, _ = _make_repo()
        mock_response = MagicMock(spec=httpx.Response)
        mock_response.status_code = 200
        mock_response.json.return_value = {"authToken": ""}

        with patch("httpx.AsyncClient") as MockClient:
            instance = AsyncMock()
            instance.__aenter__ = AsyncMock(return_value=instance)
            instance.__aexit__ = AsyncMock(return_value=False)
            instance.get = AsyncMock(return_value=mock_response)
            MockClient.return_value = instance

            token = await repo.poll_oauth_pin(42, "client-123")
            assert token is None

    @pytest.mark.asyncio
    async def test_poll_pin_non_200(self):
        repo, _, _ = _make_repo()
        mock_response = MagicMock(spec=httpx.Response)
        mock_response.status_code = 404

        with patch("httpx.AsyncClient") as MockClient:
            instance = AsyncMock()
            instance.__aenter__ = AsyncMock(return_value=instance)
            instance.__aexit__ = AsyncMock(return_value=False)
            instance.get = AsyncMock(return_value=mock_response)
            MockClient.return_value = instance

            token = await repo.poll_oauth_pin(42, "client-123")
            assert token is None


def _mock_fresh_client(response: MagicMock):
    instance = AsyncMock()
    instance.__aenter__ = AsyncMock(return_value=instance)
    instance.__aexit__ = AsyncMock(return_value=False)
    instance.get = AsyncMock(return_value=response)
    return instance


class TestAccountAuthCalls:
    @pytest.mark.asyncio
    async def test_server_ids_ignores_tokenless_client_devices(self):
        # Regression: /api/v2/resources lists client devices (e.g. Plexamp) with
        # no accessToken. The old generated SDK model rejected those and broke
        # login for everyone. The lenient parse must skip them and still find the
        # server by clientIdentifier + provides.
        repo, _, _ = _make_repo()
        devices = [
            {"clientIdentifier": "server-machine-id", "provides": "server", "accessToken": "abc"},
            {"clientIdentifier": "plexamp-device", "provides": "client,player,pubsub-player"},
        ]
        response = MagicMock(spec=httpx.Response)
        response.status_code = 200
        response.json.return_value = devices

        with patch("httpx.AsyncClient") as MockClient:
            MockClient.return_value = _mock_fresh_client(response)
            server_ids = await repo.get_account_server_ids("user-token", "client-123")

        assert server_ids == {"server-machine-id"}

    @pytest.mark.asyncio
    async def test_server_ids_auth_failure_raises(self):
        repo, _, _ = _make_repo()
        response = MagicMock(spec=httpx.Response)
        response.status_code = 401

        with patch("httpx.AsyncClient") as MockClient:
            MockClient.return_value = _mock_fresh_client(response)
            with pytest.raises(PlexAuthError):
                await repo.get_account_server_ids("user-token", "client-123")

    @pytest.mark.asyncio
    async def test_account_profile_prefers_friendly_name(self):
        repo, _, _ = _make_repo()
        response = MagicMock(spec=httpx.Response)
        response.status_code = 200
        response.json.return_value = {
            "uuid": "user-uuid",
            "email": "a@b.com",
            "friendlyName": "Friendly",
            "username": "uname",
            "title": "Title",
            "thumb": "http://thumb",
        }

        with patch("httpx.AsyncClient") as MockClient:
            MockClient.return_value = _mock_fresh_client(response)
            profile = await repo.get_account_profile("user-token", "client-123")

        assert profile.uuid == "user-uuid"
        assert profile.email == "a@b.com"
        assert profile.display_name == "Friendly"
        assert profile.thumb == "http://thumb"

    @pytest.mark.asyncio
    async def test_account_profile_missing_uuid_raises(self):
        repo, _, _ = _make_repo()
        response = MagicMock(spec=httpx.Response)
        response.status_code = 200
        response.json.return_value = {"email": "a@b.com"}

        with patch("httpx.AsyncClient") as MockClient:
            MockClient.return_value = _mock_fresh_client(response)
            with pytest.raises(PlexApiError, match="missing uuid"):
                await repo.get_account_profile("user-token", "client-123")


class TestClearCache:
    @pytest.mark.asyncio
    async def test_clears_plex_prefix(self):
        repo, _, cache = _make_repo()
        await repo.clear_cache()
        cache.clear_prefix.assert_awaited_once()


class TestCircuitBreaker:
    def test_reset_circuit_breaker(self):
        PlexRepository.reset_circuit_breaker()
        assert _plex_circuit_breaker.failure_count == 0


class TestUnconfigured:
    @pytest.mark.asyncio
    async def test_request_raises_when_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        with pytest.raises(ExternalServiceError, match="not configured"):
            await repo._request("/test")

    @pytest.mark.asyncio
    async def test_proxy_head_raises(self):
        repo, _, _ = _make_repo(configured=False)
        with pytest.raises(ExternalServiceError, match="not configured"):
            await repo.proxy_head_stream("/part/key")

    @pytest.mark.asyncio
    async def test_proxy_get_raises(self):
        repo, _, _ = _make_repo(configured=False)
        with pytest.raises(ExternalServiceError, match="not configured"):
            await repo.proxy_get_stream("/part/key")


class TestErrorHandling:
    @pytest.mark.asyncio
    async def test_401_raises_auth_error(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=401))
        with pytest.raises(PlexAuthError, match="authentication"):
            await repo._request("/test")

    @pytest.mark.asyncio
    async def test_403_raises_auth_error(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=403))
        with pytest.raises(PlexAuthError, match="authentication"):
            await repo._request("/test")

    @pytest.mark.asyncio
    async def test_500_raises_api_error(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(status_code=500))
        with pytest.raises(PlexApiError, match="failed"):
            await repo._request("/test")

    @pytest.mark.asyncio
    async def test_timeout_raises_external_error(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.TimeoutException("timeout"))
        with pytest.raises(ExternalServiceError, match="timed out"):
            await repo._request("/test")

    @pytest.mark.asyncio
    async def test_http_error_raises_external_error(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.ConnectError("refused"))
        with pytest.raises(ExternalServiceError, match="failed"):
            await repo._request("/test")

    @pytest.mark.asyncio
    async def test_invalid_json_raises_api_error(self):
        repo, client, _ = _make_repo()
        resp = _mock_response(status_code=200)
        resp.json.side_effect = ValueError("invalid json")
        client.get = AsyncMock(return_value=resp)
        with pytest.raises(PlexApiError, match="invalid JSON"):
            await repo._request("/test")


class TestStreamProxyValidation:
    @pytest.mark.asyncio
    async def test_head_rejects_traversal(self):
        repo, _, _ = _make_repo()
        with pytest.raises(ValueError, match="Invalid Plex part key"):
            await repo.proxy_head_stream("/library/parts/../../library/sections")

    @pytest.mark.asyncio
    async def test_get_rejects_traversal(self):
        repo, _, _ = _make_repo()
        with pytest.raises(ValueError, match="Invalid Plex part key"):
            await repo.proxy_get_stream("/library/parts/../../library/sections")

    @pytest.mark.asyncio
    async def test_head_rejects_non_library_parts_prefix(self):
        repo, _, _ = _make_repo()
        with pytest.raises(ValueError, match="Invalid Plex part key"):
            await repo.proxy_head_stream("/some/other/path")

    @pytest.mark.asyncio
    async def test_get_rejects_non_library_parts_prefix(self):
        repo, _, _ = _make_repo()
        with pytest.raises(ValueError, match="Invalid Plex part key"):
            await repo.proxy_get_stream("/some/other/path")

    @pytest.mark.asyncio
    async def test_head_normalises_missing_leading_slash(self):
        repo, _, _ = _make_repo()
        with patch("repositories.plex_repository.httpx.AsyncClient") as mock_cls:
            mock_client = AsyncMock()
            mock_resp = MagicMock()
            mock_resp.status_code = 200
            mock_resp.headers = {"Content-Type": "audio/flac", "Content-Length": "999"}
            mock_client.head = AsyncMock(return_value=mock_resp)
            mock_client.__aenter__ = AsyncMock(return_value=mock_client)
            mock_client.__aexit__ = AsyncMock(return_value=False)
            mock_cls.return_value = mock_client

            result = await repo.proxy_head_stream("library/parts/100/file.flac")
            assert result.status_code == 200
            mock_client.head.assert_called_once()
            called_url = mock_client.head.call_args[0][0]
            assert "/library/parts/100/file.flac" in called_url

    @pytest.mark.asyncio
    async def test_get_rejects_traversal_without_leading_slash(self):
        repo, _, _ = _make_repo()
        with pytest.raises(ValueError, match="Invalid Plex part key"):
            await repo.proxy_get_stream("library/parts/../../etc/passwd")

    @pytest.mark.asyncio
    async def test_head_rejects_embedded_double_dot(self):
        repo, _, _ = _make_repo()
        with pytest.raises(ValueError, match="Invalid Plex part key"):
            await repo.proxy_head_stream("/library/parts/100/../../../etc/passwd")


class TestGetStreamProxy:
    @pytest.mark.asyncio
    async def test_get_stream_happy_path(self):
        repo, _, _ = _make_repo()
        with patch("repositories.plex_repository.httpx.AsyncClient") as mock_cls:
            mock_client = AsyncMock()
            mock_resp = MagicMock()
            mock_resp.status_code = 200
            mock_resp.headers = {"Content-Type": "audio/flac", "Content-Length": "5000"}
            mock_resp.aiter_bytes = MagicMock(return_value=AsyncMock().__aiter__())
            mock_resp.aclose = AsyncMock()
            mock_client.build_request = MagicMock(return_value=MagicMock())
            mock_client.send = AsyncMock(return_value=mock_resp)
            mock_client.aclose = AsyncMock()
            mock_cls.return_value = mock_client

            result = await repo.proxy_get_stream("/library/parts/200/file.flac")
            assert result.status_code == 200
            assert result.headers.get("Content-Type") == "audio/flac"

    @pytest.mark.asyncio
    async def test_get_stream_206_partial(self):
        repo, _, _ = _make_repo()
        with patch("repositories.plex_repository.httpx.AsyncClient") as mock_cls:
            mock_client = AsyncMock()
            mock_resp = MagicMock()
            mock_resp.status_code = 206
            mock_resp.headers = {
                "Content-Type": "audio/flac",
                "Content-Range": "bytes 0-999/5000",
                "Content-Length": "1000",
            }
            mock_resp.aiter_bytes = MagicMock(return_value=AsyncMock().__aiter__())
            mock_resp.aclose = AsyncMock()
            mock_client.build_request = MagicMock(return_value=MagicMock())
            mock_client.send = AsyncMock(return_value=mock_resp)
            mock_client.aclose = AsyncMock()
            mock_cls.return_value = mock_client

            result = await repo.proxy_get_stream(
                "/library/parts/200/file.flac", range_header="bytes=0-999"
            )
            assert result.status_code == 206

    @pytest.mark.asyncio
    async def test_get_stream_416_raises(self):
        repo, _, _ = _make_repo()
        with patch("repositories.plex_repository.httpx.AsyncClient") as mock_cls:
            mock_client = AsyncMock()
            mock_resp = MagicMock()
            mock_resp.status_code = 416
            mock_resp.headers = {}
            mock_resp.aclose = AsyncMock()
            mock_client.build_request = MagicMock(return_value=MagicMock())
            mock_client.send = AsyncMock(return_value=mock_resp)
            mock_client.aclose = AsyncMock()
            mock_cls.return_value = mock_client

            with pytest.raises(ExternalServiceError, match="416"):
                await repo.proxy_get_stream("/library/parts/200/file.flac")

    @pytest.mark.asyncio
    async def test_get_stream_transport_error_raises_external(self):
        repo, _, _ = _make_repo()
        with patch("repositories.plex_repository.httpx.AsyncClient") as mock_cls:
            mock_client = AsyncMock()
            mock_client.build_request = MagicMock(return_value=MagicMock())
            mock_client.send = AsyncMock(
                side_effect=httpx.ConnectError("connection refused")
            )
            mock_client.aclose = AsyncMock()
            mock_cls.return_value = mock_client

            with pytest.raises(ExternalServiceError, match="Plex streaming failed"):
                await repo.proxy_get_stream("/library/parts/200/file.flac")

    @pytest.mark.asyncio
    async def test_get_stream_timeout_raises_external(self):
        repo, _, _ = _make_repo()
        with patch("repositories.plex_repository.httpx.AsyncClient") as mock_cls:
            mock_client = AsyncMock()
            mock_client.build_request = MagicMock(return_value=MagicMock())
            mock_client.send = AsyncMock(
                side_effect=httpx.ReadTimeout("read timed out")
            )
            mock_client.aclose = AsyncMock()
            mock_cls.return_value = mock_client

            with pytest.raises(ExternalServiceError, match="Plex streaming failed"):
                await repo.proxy_get_stream("/library/parts/200/file.flac")

    @pytest.mark.asyncio
    async def test_get_stream_range_header_forwarded(self):
        repo, _, _ = _make_repo()
        with patch("repositories.plex_repository.httpx.AsyncClient") as mock_cls:
            mock_client = AsyncMock()
            mock_resp = MagicMock()
            mock_resp.status_code = 206
            mock_resp.headers = {"Content-Type": "audio/flac"}
            mock_resp.aiter_bytes = MagicMock(return_value=AsyncMock().__aiter__())
            mock_resp.aclose = AsyncMock()
            mock_client.build_request = MagicMock(return_value=MagicMock())
            mock_client.send = AsyncMock(return_value=mock_resp)
            mock_client.aclose = AsyncMock()
            mock_cls.return_value = mock_client

            await repo.proxy_get_stream(
                "/library/parts/200/file.flac", range_header="bytes=100-200"
            )
            build_call = mock_client.build_request.call_args
            headers = build_call[1].get("headers") or build_call[0][2] if len(build_call[0]) > 2 else build_call[1].get("headers", {})
            assert "Range" in headers
            assert headers["Range"] == "bytes=100-200"
