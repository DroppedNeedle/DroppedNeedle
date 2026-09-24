import hashlib

import pytest
from unittest.mock import AsyncMock, MagicMock

import httpx

from core.exceptions import ConfigurationError, ExternalServiceError
from repositories.lastfm_repository import LastFmRepository, LASTFM_ERROR_MAP


def _make_cache() -> AsyncMock:
    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    return cache


def _make_repo(
    api_key: str = "key",
    shared_secret: str = "secret",
    session_key: str = "",
    cache: AsyncMock | None = None,
) -> LastFmRepository:
    http_client = AsyncMock(spec=httpx.AsyncClient)
    return LastFmRepository(
        http_client=http_client,
        cache=cache or _make_cache(),
        api_key=api_key,
        shared_secret=shared_secret,
        session_key=session_key,
    )


class TestBuildApiSig:
    def test_basic_signature(self):
        repo = _make_repo(shared_secret="mysecret")
        params = {"method": "auth.getToken", "api_key": "mykey"}
        sig = repo._build_api_sig(params)
        expected_str = "api_keymykeymethodauth.getTokenmysecret"
        expected = hashlib.md5(expected_str.encode("utf-8")).hexdigest()
        assert sig == expected

    def test_excludes_format_and_callback(self):
        repo = _make_repo(shared_secret="sec")
        params = {
            "method": "test",
            "api_key": "k",
            "format": "json",
            "callback": "cb",
        }
        sig = repo._build_api_sig(params)
        expected_str = "api_keykmethodtestsec"
        expected = hashlib.md5(expected_str.encode("utf-8")).hexdigest()
        assert sig == expected

    def test_params_sorted_alphabetically(self):
        repo = _make_repo(shared_secret="s")
        params = {"z_param": "z", "a_param": "a"}
        sig = repo._build_api_sig(params)
        expected_str = "a_paramaz_paramzs"
        expected = hashlib.md5(expected_str.encode("utf-8")).hexdigest()
        assert sig == expected


class TestHandleErrorResponse:
    def test_no_error_returns_none(self):
        repo = _make_repo()
        repo._handle_error_response({"artist": "test"})

    def test_invalid_api_key_raises_configuration_error(self):
        repo = _make_repo()
        with pytest.raises(ConfigurationError, match="(?i)invalid api key"):
            repo._handle_error_response({"error": 10, "message": "bad key"})

    def test_session_expired_raises_configuration_error(self):
        repo = _make_repo()
        with pytest.raises(ConfigurationError, match="Session key expired"):
            repo._handle_error_response({"error": 9, "message": "expired"})

    def test_service_offline_raises_external_error(self):
        repo = _make_repo()
        with pytest.raises(ExternalServiceError, match="temporarily offline"):
            repo._handle_error_response({"error": 11, "message": "offline"})

    def test_unknown_error_code_raises_external_error(self):
        repo = _make_repo()
        with pytest.raises(ExternalServiceError, match="Last.fm error \\(999\\)"):
            repo._handle_error_response({"error": 999, "message": "weird"})

    def test_error_17_raises_configuration_error(self):
        repo = _make_repo()
        with pytest.raises(ConfigurationError, match="re-authorize Last.fm"):
            repo._handle_error_response({
                "error": 17,
                "message": "Login: User required to be logged in",
            })


class TestConfigureMethod:
    def test_configure_updates_credentials(self):
        repo = _make_repo(api_key="old", shared_secret="old")
        repo.configure(api_key="new-key", shared_secret="new-secret", session_key="sk-1")
        assert repo._api_key == "new-key"
        assert repo._shared_secret == "new-secret"
        assert repo._session_key == "sk-1"


class TestConstructorDefaults:
    def test_default_empty_strings(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        repo = LastFmRepository(http_client=http_client, cache=_make_cache())
        assert repo._api_key == ""
        assert repo._shared_secret == ""
        assert repo._session_key == ""


class TestCanSignProperty:
    def test_true_when_both_credentials_present(self):
        repo = _make_repo(shared_secret="sec", session_key="sk")
        assert repo._can_sign is True

    def test_false_without_shared_secret(self):
        repo = _make_repo(shared_secret="", session_key="sk")
        assert repo._can_sign is False

    def test_false_without_session_key(self):
        repo = _make_repo(shared_secret="sec", session_key="")
        assert repo._can_sign is False

    def test_false_with_neither(self):
        repo = _make_repo(shared_secret="", session_key="")
        assert repo._can_sign is False

    def test_configure_enables_can_sign(self):
        repo = _make_repo(shared_secret="", session_key="")
        assert repo._can_sign is False
        repo.configure(api_key="k", shared_secret="s", session_key="sk")
        assert repo._can_sign is True


class TestSignedUserRequests:
    @pytest.mark.asyncio
    async def test_get_recent_tracks_sends_signed_request(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"recenttracks": {"track": []}},
                text="",
            )
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=cache,
            api_key="key",
            shared_secret="sec",
            session_key="sk-1",
        )
        await repo.get_user_recent_tracks("user1")
        call_params = http_client.get.call_args.kwargs.get("params", {})
        assert "api_sig" in call_params
        assert call_params["sk"] == "sk-1"

    @pytest.mark.asyncio
    async def test_get_recent_tracks_unsigned_without_session_key(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"recenttracks": {"track": []}},
                text="",
            )
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=cache,
            api_key="key",
        )
        await repo.get_user_recent_tracks("user1")
        call_params = http_client.get.call_args.kwargs.get("params", {})
        assert "api_sig" not in call_params
        assert "sk" not in call_params

    @pytest.mark.asyncio
    async def test_get_top_artists_sends_signed_request(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"topartists": {"artist": []}},
                text="",
            )
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=cache,
            api_key="key",
            shared_secret="sec",
            session_key="sk-1",
        )
        await repo.get_user_top_artists("user1")
        call_params = http_client.get.call_args.kwargs.get("params", {})
        assert "api_sig" in call_params
        assert call_params["sk"] == "sk-1"

    @pytest.mark.asyncio
    async def test_error_17_through_signed_request_raises_configuration_error(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "error": 17,
                    "message": "Login: User required to be logged in",
                },
                text="",
            )
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=cache,
            api_key="key",
            shared_secret="sec",
            session_key="sk-1",
        )
        with pytest.raises(ConfigurationError, match="re-authorize Last.fm"):
            await repo.get_user_recent_tracks("user1")


class TestUpdateNowPlaying:
    @pytest.mark.asyncio
    async def test_posts_with_required_params(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.post = AsyncMock(
            return_value=MagicMock(status_code=200, json=lambda: {"nowplaying": {}}, text="")
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=_make_cache(),
            api_key="key",
            shared_secret="secret",
            session_key="sk-1",
        )
        result = await repo.update_now_playing(artist="Artist", track="Track")
        assert result is True
        call_args = http_client.post.call_args
        posted_data = call_args.kwargs.get("data", call_args.args[1] if len(call_args.args) > 1 else {})
        assert posted_data["method"] == "track.updateNowPlaying"
        assert posted_data["artist"] == "Artist"
        assert posted_data["track"] == "Track"
        assert "api_sig" in posted_data

    @pytest.mark.asyncio
    async def test_includes_optional_params(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.post = AsyncMock(
            return_value=MagicMock(status_code=200, json=lambda: {"nowplaying": {}}, text="")
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=_make_cache(),
            api_key="key",
            shared_secret="secret",
            session_key="sk-1",
        )
        await repo.update_now_playing(
            artist="A", track="T", album="Album", duration=300, mbid="mb-123"
        )
        posted_data = http_client.post.call_args.kwargs.get(
            "data", http_client.post.call_args.args[1] if len(http_client.post.call_args.args) > 1 else {}
        )
        assert posted_data["album"] == "Album"
        assert posted_data["duration"] == "300"
        assert posted_data["mbid"] == "mb-123"

    @pytest.mark.asyncio
    async def test_omits_empty_optional_params(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.post = AsyncMock(
            return_value=MagicMock(status_code=200, json=lambda: {"nowplaying": {}}, text="")
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=_make_cache(),
            api_key="key",
            shared_secret="secret",
            session_key="sk-1",
        )
        await repo.update_now_playing(artist="A", track="T")
        posted_data = http_client.post.call_args.kwargs.get(
            "data", http_client.post.call_args.args[1] if len(http_client.post.call_args.args) > 1 else {}
        )
        assert "album" not in posted_data
        assert "duration" not in posted_data
        assert "mbid" not in posted_data

    @pytest.mark.asyncio
    async def test_signature_includes_session_key(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.post = AsyncMock(
            return_value=MagicMock(status_code=200, json=lambda: {"nowplaying": {}}, text="")
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=_make_cache(),
            api_key="key",
            shared_secret="secret",
            session_key="sk-1",
        )
        await repo.update_now_playing(artist="A", track="T")
        posted_data = http_client.post.call_args.kwargs.get(
            "data", http_client.post.call_args.args[1] if len(http_client.post.call_args.args) > 1 else {}
        )
        assert posted_data["sk"] == "sk-1"


class TestScrobble:
    @pytest.mark.asyncio
    async def test_posts_with_timestamp(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.post = AsyncMock(
            return_value=MagicMock(status_code=200, json=lambda: {"scrobbles": {}}, text="")
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=_make_cache(),
            api_key="key",
            shared_secret="secret",
            session_key="sk-1",
        )
        result = await repo.scrobble(artist="Artist", track="Track", timestamp=1700000000)
        assert result is True
        posted_data = http_client.post.call_args.kwargs.get(
            "data", http_client.post.call_args.args[1] if len(http_client.post.call_args.args) > 1 else {}
        )
        assert posted_data["method"] == "track.scrobble"
        assert posted_data["artist"] == "Artist"
        assert posted_data["track"] == "Track"
        assert posted_data["timestamp"] == "1700000000"
        assert "api_sig" in posted_data

    @pytest.mark.asyncio
    async def test_includes_album_and_duration(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.post = AsyncMock(
            return_value=MagicMock(status_code=200, json=lambda: {"scrobbles": {}}, text="")
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=_make_cache(),
            api_key="key",
            shared_secret="secret",
            session_key="sk-1",
        )
        await repo.scrobble(
            artist="A", track="T", timestamp=1700000000, album="Alb", duration=200
        )
        posted_data = http_client.post.call_args.kwargs.get(
            "data", http_client.post.call_args.args[1] if len(http_client.post.call_args.args) > 1 else {}
        )
        assert posted_data["album"] == "Alb"
        assert posted_data["duration"] == "200"

    @pytest.mark.asyncio
    async def test_raises_on_api_error(self):
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.post = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"error": 9, "message": "session expired"},
                text="",
            )
        )
        repo = LastFmRepository(
            http_client=http_client,
            cache=_make_cache(),
            api_key="key",
            shared_secret="secret",
            session_key="sk-1",
        )
        with pytest.raises(ConfigurationError, match="Session key expired"):
            await repo.scrobble(artist="A", track="T", timestamp=1700000000)


class TestGetUserTopArtists:
    @pytest.mark.asyncio
    async def test_parses_response_and_caches(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "topartists": {
                        "artist": [
                            {"name": "Radiohead", "mbid": "a74b1b7f", "playcount": "100"},
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_user_top_artists("user1", period="7day", limit=5)
        assert len(result) == 1
        assert result[0].name == "Radiohead"
        assert result[0].playcount == 100
        cache.set.assert_called_once()

    @pytest.mark.asyncio
    async def test_returns_cached_data(self):
        from repositories.lastfm_models import LastFmArtist

        cached_data = [LastFmArtist(name="Cached", playcount=50)]
        cache = _make_cache()
        cache.get = AsyncMock(return_value=cached_data)
        repo = _make_repo(cache=cache)
        result = await repo.get_user_top_artists("user1")
        assert result[0].name == "Cached"
        assert repo._client.get.call_count == 0

    @pytest.mark.asyncio
    async def test_invalid_period_defaults_to_overall(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"topartists": {"artist": []}},
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        await repo.get_user_top_artists("user1", period="invalid")
        call_params = http_client.get.call_args.kwargs.get("params", {})
        assert call_params.get("period") == "overall"


class TestGetSimilarArtists:
    @pytest.mark.asyncio
    async def test_parses_by_name(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "similarartists": {
                        "artist": [
                            {"name": "Muse", "mbid": "abc", "match": "0.9"},
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_similar_artists("Radiohead", limit=5)
        assert len(result) == 1
        assert result[0].name == "Muse"
        assert result[0].match == pytest.approx(0.9)

    @pytest.mark.asyncio
    async def test_uses_mbid_when_provided(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"similarartists": {"artist": []}},
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        await repo.get_similar_artists("Radiohead", mbid="abc-123", limit=5)
        call_params = http_client.get.call_args.kwargs.get("params", {})
        assert call_params.get("mbid") == "abc-123"
        assert "artist" not in call_params


class TestGetUserWeeklyAlbumChart:
    @pytest.mark.asyncio
    async def test_parses_weekly_album_chart(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "weeklyalbumchart": {
                        "album": [
                            {
                                "name": "OK Computer",
                                "artist": {"#text": "Radiohead"},
                                "mbid": "rg-1",
                                "playcount": "12",
                            },
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_user_weekly_album_chart("user1")
        assert len(result) == 1
        assert result[0].name == "OK Computer"
        assert result[0].artist_name == "Radiohead"
        assert result[0].playcount == 12


class TestGetUserTopAlbums:
    @pytest.mark.asyncio
    async def test_parses_albums(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "topalbums": {
                        "album": [
                            {
                                "name": "OK Computer",
                                "artist": {"name": "Radiohead"},
                                "mbid": "a1",
                                "playcount": "55",
                                "listeners": "200",
                            },
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_user_top_albums("user1", period="3month", limit=5)
        assert len(result) == 1
        assert result[0].name == "OK Computer"
        assert result[0].artist_name == "Radiohead"
        assert result[0].playcount == 55
        cache.set.assert_called_once()


class TestGetUserTopTracks:
    @pytest.mark.asyncio
    async def test_parses_tracks(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "toptracks": {
                        "track": [
                            {
                                "name": "Karma Police",
                                "artist": {"name": "Radiohead"},
                                "mbid": "t1",
                                "playcount": "30",
                            },
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_user_top_tracks("user1", period="1month", limit=10)
        assert len(result) == 1
        assert result[0].name == "Karma Police"
        assert result[0].artist_name == "Radiohead"
        cache.set.assert_called_once()


class TestGetUserRecentTracks:
    @pytest.mark.asyncio
    async def test_parses_recent_tracks(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "recenttracks": {
                        "track": [
                            {
                                "name": "Everything In Its Right Place",
                                "artist": {"#text": "Radiohead", "mbid": "a1"},
                                "album": {"#text": "Kid A", "mbid": "al1"},
                                "date": {"uts": "1700000000"},
                            },
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_user_recent_tracks("user1", limit=10)
        assert len(result) == 1
        assert result[0].track_name == "Everything In Its Right Place"
        assert result[0].artist_name == "Radiohead"
        assert result[0].album_name == "Kid A"
        assert result[0].timestamp == 1700000000

    @pytest.mark.asyncio
    async def test_now_playing_flag(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "recenttracks": {
                        "track": [
                            {
                                "name": "NOW",
                                "artist": {"#text": "Band"},
                                "album": {"#text": "Album"},
                                "@attr": {"nowplaying": "true"},
                            },
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_user_recent_tracks("user1")
        assert result[0].now_playing is True


class TestGetUserWeeklyArtistChart:
    @pytest.mark.asyncio
    async def test_parses_weekly_artist_chart(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "weeklyartistchart": {
                        "artist": [
                            {"name": "Radiohead", "playcount": "25", "mbid": "a1"},
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_user_weekly_artist_chart("user1")
        assert len(result) == 1
        assert result[0].name == "Radiohead"
        assert result[0].playcount == 25


class TestGetGlobalTopTracks:
    @pytest.mark.asyncio
    async def test_parses_global_tracks(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "toptracks": {
                        "track": [
                            {"name": "Blinding Lights", "artist": {"name": "The Weeknd"}, "playcount": "9999"},
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_global_top_tracks(limit=10)
        assert len(result) == 1
        assert result[0].name == "Blinding Lights"
        assert result[0].artist_name == "The Weeknd"


class TestGetTagTopArtists:
    @pytest.mark.asyncio
    async def test_parses_tag_artists(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "topartists": {
                        "artist": [
                            {"name": "Pink Floyd", "mbid": "pf1", "playcount": "500"},
                        ]
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_tag_top_artists("progressive rock", limit=10)
        assert len(result) == 1
        assert result[0].name == "Pink Floyd"


class TestGetAlbumInfo:
    @pytest.mark.asyncio
    async def test_parses_album_info_with_tracks(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "album": {
                        "name": "OK Computer",
                        "artist": "Radiohead",
                        "mbid": "al1",
                        "listeners": "3000",
                        "playcount": "50000",
                        "url": "https://last.fm/album",
                        "wiki": {"summary": "A classic album."},
                        "tags": {"tag": [{"name": "rock", "url": "https://last.fm/tag/rock"}]},
                        "tracks": {
                            "track": [
                                {"name": "Airbag", "duration": "283", "@attr": {"rank": "1"}, "url": "https://last.fm/track"},
                                {"name": "Paranoid Android", "duration": "390", "@attr": {"rank": "2"}, "url": ""},
                            ]
                        },
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_album_info("Radiohead", "OK Computer")
        assert result is not None
        assert result.name == "OK Computer"
        assert result.artist_name == "Radiohead"
        assert result.summary == "A classic album."
        assert result.listeners == 3000
        assert len(result.tags) == 1
        assert result.tags[0].name == "rock"
        assert len(result.tracks) == 2
        assert result.tracks[0].name == "Airbag"
        assert result.tracks[0].duration == 283
        assert result.tracks[0].rank == 1
        assert result.tracks[1].name == "Paranoid Android"

    @pytest.mark.asyncio
    async def test_not_found_returns_none(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"error": 6, "message": "Album not found"},
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_album_info("X", "Y")
        assert result is None


class TestGetArtistInfoNotFound:
    @pytest.mark.asyncio
    async def test_not_found_returns_none(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {"error": 6, "message": "Artist not found"},
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_artist_info("Nonexistent Artist")
        assert result is None


class TestGetArtistInfoParsing:
    @pytest.mark.asyncio
    async def test_parses_full_artist_info(self):
        cache = _make_cache()
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(
                status_code=200,
                json=lambda: {
                    "artist": {
                        "name": "Radiohead",
                        "mbid": "a1",
                        "stats": {"listeners": "5000", "playcount": "80000"},
                        "url": "https://last.fm/artist",
                        "bio": {"summary": "English rock band."},
                        "tags": {"tag": [{"name": "alternative", "url": "https://last.fm/tag/alt"}]},
                        "similar": {
                            "artist": [
                                {"name": "Muse", "mbid": "m1", "match": "0.85"},
                            ]
                        },
                    }
                },
                text="",
            )
        )
        repo = LastFmRepository(http_client=http_client, cache=cache, api_key="k")
        result = await repo.get_artist_info("Radiohead")
        assert result is not None
        assert result.name == "Radiohead"
        assert result.listeners == 5000
        assert result.playcount == 80000
        assert result.bio_summary == "English rock band."
        assert len(result.tags) == 1
        assert result.tags[0].name == "alternative"
        assert len(result.similar) == 1
        assert result.similar[0].name == "Muse"
        assert result.similar[0].match == pytest.approx(0.85)
        cache.set.assert_called_once()


class TestCacheBehavior:
    @pytest.mark.asyncio
    async def test_returns_cached_global_top_artists(self):
        from repositories.lastfm_models import LastFmArtist

        cached_data = [LastFmArtist(name="Cached", playcount=10)]
        cache = _make_cache()
        cache.get = AsyncMock(return_value=cached_data)
        repo = _make_repo(cache=cache)
        result = await repo.get_global_top_artists(limit=5)
        assert result[0].name == "Cached"
        assert repo._client.get.call_count == 0

    @pytest.mark.asyncio
    async def test_returns_cached_tag_top_artists(self):
        from repositories.lastfm_models import LastFmArtist

        cached_data = [LastFmArtist(name="TagCached", playcount=20)]
        cache = _make_cache()
        cache.get = AsyncMock(return_value=cached_data)
        repo = _make_repo(cache=cache)
        result = await repo.get_tag_top_artists("rock", limit=5)
        assert result[0].name == "TagCached"
        assert repo._client.get.call_count == 0

    @pytest.mark.asyncio
    async def test_returns_cached_album_info(self):
        from repositories.lastfm_models import LastFmAlbumInfo

        cached_data = LastFmAlbumInfo(name="Cached Album", artist_name="A")
        cache = _make_cache()
        cache.get = AsyncMock(return_value=cached_data)
        repo = _make_repo(cache=cache)
        result = await repo.get_album_info("A", "Cached Album")
        assert result.name == "Cached Album"
        assert repo._client.get.call_count == 0


class TestGetTrackAlbum:
    @staticmethod
    def _repo_with_response(payload: dict, cache: AsyncMock | None = None) -> LastFmRepository:
        http_client = AsyncMock(spec=httpx.AsyncClient)
        http_client.get = AsyncMock(
            return_value=MagicMock(status_code=200, json=lambda: payload, text="")
        )
        return LastFmRepository(http_client=http_client, cache=cache or _make_cache(), api_key="k")

    @pytest.mark.asyncio
    async def test_returns_album_title_and_caches_it(self):
        cache = _make_cache()
        repo = self._repo_with_response(
            {"track": {"name": "Crawling", "album": {"title": "Hybrid Theory"}}}, cache
        )

        result = await repo.get_track_album("Linkin Park", "Crawling")

        assert result == "Hybrid Theory"
        call_params = repo._client.get.call_args.kwargs.get("params", {})
        assert call_params["method"] == "track.getInfo"
        assert call_params["artist"] == "Linkin Park"
        assert call_params["track"] == "Crawling"
        assert call_params["autocorrect"] == "1"
        assert cache.set.call_args.args[1] == "Hybrid Theory"

    @pytest.mark.asyncio
    async def test_track_without_album_returns_none_and_caches_miss(self):
        cache = _make_cache()
        repo = self._repo_with_response({"track": {"name": "Demo"}}, cache)

        result = await repo.get_track_album("Artist", "Demo")

        assert result is None
        assert cache.set.call_args.args[1] == ""

    @pytest.mark.asyncio
    async def test_not_found_returns_none(self):
        repo = self._repo_with_response({"error": 6, "message": "Track not found"})

        assert await repo.get_track_album("Artist", "Missing") is None

    @pytest.mark.asyncio
    async def test_cached_title_skips_request(self):
        cache = _make_cache()
        cache.get = AsyncMock(return_value="Meteora")
        repo = _make_repo(cache=cache)

        assert await repo.get_track_album("Linkin Park", "Numb") == "Meteora"
        assert repo._client.get.call_count == 0

    @pytest.mark.asyncio
    async def test_cached_miss_skips_request(self):
        cache = _make_cache()
        cache.get = AsyncMock(return_value="")
        repo = _make_repo(cache=cache)

        assert await repo.get_track_album("Artist", "Demo") is None
        assert repo._client.get.call_count == 0
