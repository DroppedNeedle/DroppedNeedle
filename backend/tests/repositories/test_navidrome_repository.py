from __future__ import annotations

import hashlib
import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest

from core.exceptions import (
    ExternalServiceError,
    NavidromeApiError,
    NavidromeAuthError,
    NavidromeSubsonicError,
    NonRetriableExternalServiceError,
)
from repositories.navidrome_repository import _navidrome_circuit_breaker
from repositories.navidrome_models import (
    SubsonicAlbum,
    SubsonicArtist,
    SubsonicGenre,
    SubsonicSearchResult,
    SubsonicSong,
    parse_album,
    parse_artist,
    parse_genre,
    parse_song,
    parse_subsonic_response,
)
from repositories.navidrome_repository import NavidromeRepository
from infrastructure.cache.memory_cache import InMemoryCache
from tests.mocks.navidrome_mock import Navidrome062Mock


def _make_cache() -> MagicMock:
    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    cache.clear_prefix = AsyncMock(return_value=0)
    return cache


def _make_repo(
    configured: bool = True, cache_scope: str = "shared"
) -> tuple[NavidromeRepository, AsyncMock, MagicMock]:
    client = AsyncMock(spec=httpx.AsyncClient)
    cache = _make_cache()
    repo = NavidromeRepository(http_client=client, cache=cache, cache_scope=cache_scope)
    if configured:
        repo.configure("http://navidrome:4533", "admin", "secret")
    return repo, client, cache


def _ok_envelope(body: dict | None = None) -> dict:
    resp: dict = {"subsonic-response": {"status": "ok", "version": "1.16.1"}}
    if body:
        resp["subsonic-response"].update(body)
    return resp


def _mock_response(json_data: dict, status_code: int = 200) -> MagicMock:
    resp = MagicMock(spec=httpx.Response)
    resp.status_code = status_code
    resp.json.return_value = json_data
    return resp


class TestBuildAuthParams:
    def test_contains_required_keys(self):
        repo, _, _ = _make_repo()
        params = repo._build_auth_params()
        assert set(params.keys()) == {"u", "t", "s", "v", "c", "f"}

    def test_username_matches(self):
        repo, _, _ = _make_repo()
        params = repo._build_auth_params()
        assert params["u"] == "admin"
        assert params["v"] == "1.16.1"
        assert params["c"] == "droppedneedle"
        assert params["f"] == "json"

    def test_token_is_correct_md5(self):
        repo, _, _ = _make_repo()
        params = repo._build_auth_params()
        expected = hashlib.md5(("secret" + params["s"]).encode("utf-8")).hexdigest()
        assert params["t"] == expected

    def test_fresh_salt_per_call(self):
        repo, _, _ = _make_repo()
        salts = {repo._build_auth_params()["s"] for _ in range(10)}
        assert len(salts) > 1

    def test_server_identity_is_stable_and_does_not_expose_url(self):
        repo, _, _ = _make_repo()
        assert repo.server_identity == repo.server_identity
        assert "navidrome" not in repo.server_identity


class TestMusicFolderScope:
    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        ("method", "kwargs", "response"),
        [
            (
                "get_album_list",
                {"type": "newest"},
                {"albumList2": {"album": []}},
            ),
            ("get_artists", {}, {"artists": {"index": []}}),
            ("search", {"query": "radiohead"}, {"searchResult3": {}}),
            ("get_starred", {}, {"starred2": {}}),
            ("get_artists_index", {}, {"artists": {"index": []}}),
            (
                "get_songs_by_genre",
                {"genre": "Rock"},
                {"songsByGenre": {"song": []}},
            ),
            ("search_songs", {}, {"searchResult3": {}}),
            ("get_random_songs", {}, {"randomSongs": {"song": []}}),
        ],
    )
    async def test_selected_scope_serializes_repeated_folder_ids(
        self, method, kwargs, response
    ):
        repo, _, _ = _make_repo()
        repo._request = AsyncMock(return_value=response)

        await getattr(repo, method)(**kwargs, music_folder_ids=("folder-a", "folder-b"))

        params = repo._request.await_args.args[1]
        assert params["musicFolderId"] == ["folder-a", "folder-b"]

    @pytest.mark.asyncio
    async def test_all_scope_omits_music_folder_parameter(self):
        repo, _, _ = _make_repo()
        repo._request = AsyncMock(return_value={"artists": {"index": []}})
        await repo.get_artists()
        assert "musicFolderId" not in repo._request.await_args.args[1]

    @pytest.mark.asyncio
    async def test_empty_selected_scope_fails_closed_without_request(self):
        repo, _, _ = _make_repo()
        repo._request = AsyncMock()
        assert (
            await repo.search("anything", music_folder_ids=()) == SubsonicSearchResult()
        )
        repo._request.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_live_verified_062_asgi_repeats_same_folder_and_ignores_unknown(
        self,
    ):
        mock = Navidrome062Mock()
        transport = httpx.ASGITransport(app=mock.app)
        async with httpx.AsyncClient(transport=transport) as client:
            repo = NavidromeRepository(client, InMemoryCache())
            repo.configure("http://navidrome.test", "user", "password")
            folders = await repo.get_music_folders()
            same = (folders[0].id, folders[0].id)
            missing = ("missing-folder",)

            assert await repo.get_artists(same)
            assert await repo.get_album_list(
                "alphabeticalByName", music_folder_ids=same
            )
            assert await repo.search_songs("", music_folder_ids=same)
            assert await repo.get_songs_by_genre("Genre", music_folder_ids=same)
            assert await repo.get_random_songs(music_folder_ids=same)
            assert await repo.get_artists(missing)

        for endpoint in (
            "getArtists",
            "getAlbumList2",
            "search3",
            "getSongsByGenre",
            "getRandomSongs",
        ):
            assert mock.music_folder_requests[endpoint][0] == [
                "folder-1",
                "folder-1",
            ]
        assert mock.music_folder_requests["getArtists"][1] == ["missing-folder"]

        fixture_path = (
            Path(__file__).parents[1]
            / "fixtures/navidrome/0.62.0/music_folder_observations.json"
        )
        observations = json.loads(fixture_path.read_text())
        assert observations["server_version"].startswith("0.62.0")
        assert observations["unknown_folder"]["artists"] == "ignored_nonempty"


class TestParseSubsonicResponse:
    def test_ok_status(self):
        data = {"subsonic-response": {"status": "ok", "version": "1.16.1"}}
        resp = parse_subsonic_response(data)
        assert resp["status"] == "ok"

    def test_error_status_raises_subsonic_error(self):
        data = {
            "subsonic-response": {
                "status": "failed",
                "error": {"code": 70, "message": "Not found"},
            }
        }
        with pytest.raises(NavidromeSubsonicError, match="Not found"):
            parse_subsonic_response(data)

    def test_auth_error_code_40(self):
        data = {
            "subsonic-response": {
                "status": "failed",
                "error": {"code": 40, "message": "Wrong creds"},
            }
        }
        with pytest.raises(NavidromeAuthError):
            parse_subsonic_response(data)

    def test_auth_error_code_41(self):
        data = {
            "subsonic-response": {
                "status": "failed",
                "error": {"code": 41, "message": "Token expired"},
            }
        }
        with pytest.raises(NavidromeAuthError):
            parse_subsonic_response(data)

    def test_missing_envelope_raises(self):
        with pytest.raises(NavidromeApiError, match="Missing"):
            parse_subsonic_response({})


class TestParseHelpers:
    def test_parse_artist_valid(self):
        data = {
            "id": "a1",
            "name": "Muse",
            "albumCount": 9,
            "coverArt": "ca",
            "musicBrainzId": "mb1",
        }
        artist = parse_artist(data)
        assert artist.id == "a1"
        assert artist.name == "Muse"
        assert artist.albumCount == 9
        assert artist.musicBrainzId == "mb1"

    def test_parse_artist_missing_fields(self):
        artist = parse_artist({})
        assert artist.id == ""
        assert artist.name == "Unknown"
        assert artist.albumCount == 0

    def test_parse_song_valid(self):
        data = {
            "id": "s1",
            "title": "Uprising",
            "album": "The Resistance",
            "albumId": "al1",
            "artist": "Muse",
            "artistId": "a1",
            "track": 1,
            "year": 2009,
            "duration": 305,
            "bitRate": 320,
            "suffix": "mp3",
            "contentType": "audio/mpeg",
            "coverArt": "cover-1",
            "musicBrainzId": "mb-s1",
        }
        song = parse_song(data)
        assert song.id == "s1"
        assert song.title == "Uprising"
        assert song.duration == 305
        assert song.coverArt == "cover-1"
        assert song.musicBrainzId == "mb-s1"

    def test_parse_song_empty(self):
        song = parse_song({})
        assert song.title == "Unknown"
        assert song.track == 0

    def test_parse_album_valid(self):
        data = {
            "id": "al1",
            "name": "OK Computer",
            "artist": "Radiohead",
            "artistId": "ar1",
            "year": 1997,
            "genre": "Rock",
            "songCount": 12,
            "duration": 3300,
            "coverArt": "cover1",
            "musicBrainzId": "mb-al1",
        }
        album = parse_album(data)
        assert album.id == "al1"
        assert album.name == "OK Computer"
        assert album.songCount == 12
        assert album.song is None

    def test_parse_album_with_songs(self):
        data = {
            "id": "al1",
            "name": "Album",
            "song": [
                {"id": "s1", "title": "Track 1"},
                {"id": "s2", "title": "Track 2"},
            ],
        }
        album = parse_album(data)
        assert album.song is not None
        assert len(album.song) == 2
        assert album.song[0].title == "Track 1"

    def test_parse_album_empty(self):
        album = parse_album({})
        assert album.name == "Unknown"
        assert album.song is None

    def test_parse_album_title_fallback(self):
        album = parse_album({"id": "x", "title": "Fallback Title"})
        assert album.name == "Fallback Title"

    def test_parse_genre_with_value_key(self):
        genre = parse_genre({"value": "Rock", "songCount": 100, "albumCount": 10})
        assert genre.name == "Rock"
        assert genre.songCount == 100

    def test_parse_genre_with_name_key(self):
        genre = parse_genre({"name": "Jazz", "songCount": 50, "albumCount": 5})
        assert genre.name == "Jazz"

    def test_parse_genre_empty(self):
        genre = parse_genre({})
        assert genre.name == ""
        assert genre.songCount == 0


class TestEndpointWrappers:
    @pytest.mark.asyncio
    async def test_get_album_list_calls_correct_endpoint(self):
        repo, client, cache = _make_repo()
        client.get = AsyncMock(
            return_value=_mock_response(_ok_envelope({"albumList2": {"album": []}}))
        )
        result = await repo.get_album_list(type="recent", size=10, offset=0)
        assert result == []
        call_args = client.get.call_args
        assert "/rest/getAlbumList2" in call_args.args[0]

    @pytest.mark.asyncio
    async def test_get_album_calls_correct_endpoint(self):
        repo, client, cache = _make_repo()
        client.get = AsyncMock(
            return_value=_mock_response(
                _ok_envelope({"album": {"id": "a1", "name": "Test"}})
            )
        )
        result = await repo.get_album("a1")
        assert result.id == "a1"
        assert result.name == "Test"

    @pytest.mark.asyncio
    async def test_get_artists_parses_index_structure(self):
        repo, client, cache = _make_repo()
        body = {
            "artists": {
                "index": [
                    {
                        "artist": [
                            {"id": "a1", "name": "ABBA"},
                            {"id": "a2", "name": "AC/DC"},
                        ]
                    },
                    {"artist": [{"id": "a3", "name": "Blur"}]},
                ]
            }
        }
        client.get = AsyncMock(return_value=_mock_response(_ok_envelope(body)))
        result = await repo.get_artists()
        assert len(result) == 3
        assert result[0].name == "ABBA"
        assert result[2].name == "Blur"

    @pytest.mark.asyncio
    async def test_search_calls_search3(self):
        repo, client, cache = _make_repo()
        body = {"searchResult3": {"artist": [], "album": [], "song": []}}
        client.get = AsyncMock(return_value=_mock_response(_ok_envelope(body)))
        result = await repo.search("test")
        assert isinstance(result, SubsonicSearchResult)
        assert "/rest/search3" in client.get.call_args.args[0]

    @pytest.mark.asyncio
    async def test_get_genres_calls_correct_endpoint(self):
        repo, client, cache = _make_repo()
        body = {
            "genres": {"genre": [{"value": "Rock", "songCount": 5, "albumCount": 1}]}
        }
        client.get = AsyncMock(return_value=_mock_response(_ok_envelope(body)))
        result = await repo.get_genres()
        assert len(result) == 1
        assert result[0].name == "Rock"

    @pytest.mark.asyncio
    async def test_scrobble_returns_true_on_success(self):
        repo, client, cache = _make_repo()
        client.get = AsyncMock(return_value=_mock_response(_ok_envelope({})))
        result = await repo.scrobble("s1", time_ms=123456)
        assert result is True

    @pytest.mark.asyncio
    async def test_scrobble_returns_false_on_error(self):
        repo, client, cache = _make_repo()
        client.get = AsyncMock(side_effect=httpx.HTTPError("fail"))
        result = await repo.scrobble("s1")
        assert result is False


class TestCaching:
    @pytest.mark.asyncio
    async def test_cached_result_returned_on_second_call(self):
        repo, client, cache = _make_repo()
        cached_albums = [SubsonicAlbum(id="a1", name="Cached")]
        cache.get = AsyncMock(return_value=cached_albums)
        result = await repo.get_album_list(type="recent", size=10, offset=0)
        assert result == cached_albums
        client.get.assert_not_called()

    @pytest.mark.asyncio
    async def test_clear_cache_calls_prefix(self):
        repo, _, cache = _make_repo()
        await repo.clear_cache()
        cache.clear_prefix.assert_awaited_once_with("navidrome:")


class TestErrorHandling:
    def setup_method(self):
        _navidrome_circuit_breaker.reset()

    @pytest.mark.asyncio
    async def test_timeout_raises_external_service_error(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.TimeoutException("timeout"))
        with patch(
            "infrastructure.resilience.retry.asyncio.sleep", new_callable=AsyncMock
        ) as sleep:
            with pytest.raises(ExternalServiceError, match="timed out"):
                await repo._request("/rest/ping")
        assert client.get.await_count == 3
        assert sleep.await_count == 2

    @pytest.mark.asyncio
    async def test_http_error_raises_external_service_error(self):
        _navidrome_circuit_breaker.reset()
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.ConnectError("refused"))
        with pytest.raises(ExternalServiceError, match="failed"):
            await repo._request("/rest/ping")

    @pytest.mark.asyncio
    async def test_401_raises_auth_error(self):
        _navidrome_circuit_breaker.reset()
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response({}, status_code=401))
        with pytest.raises(NavidromeAuthError):
            await repo._request("/rest/ping")
        assert client.get.await_count == 3

    @pytest.mark.asyncio
    async def test_500_raises_api_error(self):
        _navidrome_circuit_breaker.reset()
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response({}, status_code=500))
        with pytest.raises(NavidromeApiError):
            await repo._request("/rest/ping")

    @pytest.mark.asyncio
    async def test_not_configured_raises(self):
        _navidrome_circuit_breaker.reset()
        repo, client, _ = _make_repo(configured=False)
        with patch(
            "infrastructure.resilience.retry.asyncio.sleep", new_callable=AsyncMock
        ) as mock_sleep:
            with pytest.raises(ExternalServiceError, match="not configured"):
                await repo._request("/rest/ping")
        client.get.assert_not_awaited()
        mock_sleep.assert_not_awaited()
        from infrastructure.resilience.retry import CircuitState

        assert _navidrome_circuit_breaker.state == CircuitState.CLOSED
        assert _navidrome_circuit_breaker.failure_count == 0

    @pytest.mark.asyncio
    async def test_not_configured_is_non_retriable_subtype(self):
        _navidrome_circuit_breaker.reset()
        repo, client, _ = _make_repo(configured=False)
        with patch(
            "infrastructure.resilience.retry.asyncio.sleep", new_callable=AsyncMock
        ) as mock_sleep:
            with pytest.raises(NonRetriableExternalServiceError, match="not configured"):
                await repo._request("/rest/ping")
        client.get.assert_not_awaited()
        mock_sleep.assert_not_awaited()
        from infrastructure.resilience.retry import CircuitState

        assert _navidrome_circuit_breaker.state == CircuitState.CLOSED
        assert _navidrome_circuit_breaker.failure_count == 0


class TestCircuitBreakerNonBreaking:
    """Verify NavidromeSubsonicError doesn't trip the circuit breaker."""

    def setup_method(self):
        _navidrome_circuit_breaker.reset()

    @pytest.mark.asyncio
    async def test_subsonic_error_does_not_open_circuit_breaker(self):
        """Repeated SubsonicErrors (e.g. 'Library not found') should NOT open the CB."""
        repo, client, _ = _make_repo()
        error_envelope = {
            "subsonic-response": {
                "status": "failed",
                "error": {"code": 70, "message": "Library not found or empty"},
            }
        }
        client.get = AsyncMock(return_value=_mock_response(error_envelope))

        with patch(
            "infrastructure.resilience.retry.asyncio.sleep", new_callable=AsyncMock
        ):
            for _ in range(10):
                with pytest.raises(NavidromeSubsonicError):
                    await repo._request("/rest/getAlbumList2")

        from infrastructure.resilience.retry import CircuitState

        assert _navidrome_circuit_breaker.state == CircuitState.CLOSED

    @pytest.mark.asyncio
    async def test_auth_error_does_not_trip_circuit_breaker(self):
        """Auth errors are non-breaking (issue #138, D6): with per-user credentials
        one user's stale password must not open the shared circuit for everyone."""
        from infrastructure.resilience.retry import CircuitState

        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response({}, status_code=401))

        with patch(
            "infrastructure.resilience.retry.asyncio.sleep", new_callable=AsyncMock
        ):
            for _ in range(10):
                with pytest.raises(NavidromeAuthError):
                    await repo._request("/rest/ping")

        assert _navidrome_circuit_breaker.state == CircuitState.CLOSED

    @pytest.mark.asyncio
    async def test_connection_error_still_trips_circuit_breaker(self):
        """Transport-level errors should still record CB failures."""
        from infrastructure.resilience.retry import CircuitOpenError, CircuitState

        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.ConnectError("refused"))

        with patch(
            "infrastructure.resilience.retry.asyncio.sleep", new_callable=AsyncMock
        ):
            for _ in range(10):
                with pytest.raises((ExternalServiceError, CircuitOpenError)):
                    await repo._request("/rest/ping")

        assert _navidrome_circuit_breaker.state == CircuitState.OPEN


class TestValidateConnection:
    def setup_method(self):
        _navidrome_circuit_breaker.reset()

    @pytest.mark.asyncio
    async def test_success(self):
        repo, client, _ = _make_repo()
        client.get = AsyncMock(
            return_value=_mock_response(_ok_envelope({"version": "1.16.1"}))
        )
        ok, msg = await repo.validate_connection()
        assert ok is True
        assert "Connected" in msg

    @pytest.mark.asyncio
    async def test_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        ok, msg = await repo.validate_connection()
        assert ok is False
        assert "not configured" in msg

    @pytest.mark.asyncio
    async def test_auth_failure(self):
        _navidrome_circuit_breaker.reset()
        repo, client, _ = _make_repo()
        client.get = AsyncMock(return_value=_mock_response({}, status_code=401))
        ok, msg = await repo.validate_connection()
        assert ok is False
        assert "Authentication" in msg or "failed" in msg.lower()

    @pytest.mark.asyncio
    async def test_timeout_failure(self):
        _navidrome_circuit_breaker.reset()
        repo, client, _ = _make_repo()
        client.get = AsyncMock(side_effect=httpx.TimeoutException("timed out"))
        ok, msg = await repo.validate_connection()
        assert ok is False
        assert "timed out" in msg.lower() or "Connection" in msg


class TestConfigure:
    def test_configure_sets_configured(self):
        repo, _, _ = _make_repo(configured=False)
        assert repo.is_configured() is False
        repo.configure("http://nd:4533", "user", "pass")
        assert repo.is_configured() is True

    def test_configure_strips_trailing_slash(self):
        repo, _, _ = _make_repo(configured=False)
        repo.configure("http://nd:4533/", "u", "p")
        assert repo._url == "http://nd:4533"

    def test_configure_empty_url_not_configured(self):
        repo, _, _ = _make_repo(configured=False)
        repo.configure("", "u", "p")
        assert repo.is_configured() is False


class TestPlaylistCacheScope:
    @pytest.mark.asyncio
    async def test_list_and_detail_keys_include_requesting_user(self):
        repo, _, cache = _make_repo(cache_scope="user:alice")
        repo._request = AsyncMock(
            side_effect=[{"playlists": {"playlist": []}}, {"playlist": {}}]
        )

        await repo.get_playlists()
        await repo.get_playlist("playlist-1")

        assert [call.args[0] for call in cache.get.await_args_list] == [
            "navidrome:playlists:user:alice",
            "navidrome:playlist:user:alice:playlist-1",
        ]

    @pytest.mark.asyncio
    async def test_playlist_folder_filter_search_key_includes_requesting_user(self):
        repo, _, cache = _make_repo(cache_scope="user:alice")
        repo._request = AsyncMock(return_value={"searchResult3": {"song": []}})

        await repo.search_songs(query="", count=500, offset=0)

        cache.get.assert_awaited_once_with(
            "navidrome:songs_browse:user:alice:scope:all::500:0"
        )


class TestCacheCopySemantics:
    @pytest.mark.asyncio
    async def test_album_list_mutation_does_not_corrupt_cache(self):
        # Regression test for issue #371: callers must not be able to mutate
        # the cached list in place.
        client = AsyncMock(spec=httpx.AsyncClient)
        repo = NavidromeRepository(http_client=client, cache=InMemoryCache())
        repo.configure("http://navidrome:4533", "admin", "secret")
        album = {
            "id": "a1", "name": "Album", "artist": "Artist",
            "artistId": "ar-1", "songCount": 10, "duration": 180,
        }
        client.get = AsyncMock(
            return_value=_mock_response(
                _ok_envelope({"albumList2": {"album": [album]}})
            )
        )
        first = await repo.get_album_list(
            type="alphabeticalByName", size=500, offset=0
        )
        first.extend([SubsonicAlbum(id="bogus", name="Bogus")])
        second = await repo.get_album_list(
            type="alphabeticalByName", size=500, offset=0
        )
        assert [a.id for a in second] == ["a1"]
        client.get.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_artists_mutation_does_not_corrupt_cache(self):
        client = AsyncMock(spec=httpx.AsyncClient)
        repo = NavidromeRepository(http_client=client, cache=InMemoryCache())
        repo.configure("http://navidrome:4533", "admin", "secret")
        artist = {"id": "ar-1", "name": "Artist", "albumCount": 1}
        client.get = AsyncMock(
            return_value=_mock_response(
                _ok_envelope({"artists": {"index": [{"name": "A", "artist": [artist]}]}})
            )
        )
        first = await repo.get_artists()
        first.append(SubsonicArtist(id="bogus", name="Bogus"))
        second = await repo.get_artists()
        assert [a.id for a in second] == ["ar-1"]
        client.get.assert_awaited_once()
