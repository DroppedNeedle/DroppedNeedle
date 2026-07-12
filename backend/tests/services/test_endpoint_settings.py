"""Self-hosted endpoint settings for ListenBrainz and Last.fm.

Mirrors the MusicBrainz api_url pattern: admin-configurable endpoint URLs with
official hosts as defaults, silent fallback to the default on invalid input,
trailing-slash normalization, persistence via PreferencesService, and threading
into the repositories so a custom base URL is actually used on the wire.
Behavior with defaults must be byte-identical to the previous hardcoded hosts.
"""

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

from api.v1.schemas.settings import (
    DEFAULT_LASTFM_API_URL,
    DEFAULT_LASTFM_AUTH_URL,
    DEFAULT_LISTENBRAINZ_API_URL,
    LastFmConnectionSettings,
    LastFmConnectionSettingsResponse,
    ListenBrainzConnectionSettings,
)
from core.config import Settings
from repositories.lastfm_repository import LASTFM_API_URL, LastFmRepository
from repositories.listenbrainz_repository import (
    LISTENBRAINZ_API_URL,
    ListenBrainzRepository,
)
from services.lastfm_auth_service import LASTFM_AUTH_URL, LastFmAuthService
from services.preferences_service import PreferencesService


def _prefs(tmp_path: Path) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    return PreferencesService(settings)


class TestSchemaDefaultsAndNormalization:
    def test_listenbrainz_default_matches_hardcoded_host(self):
        s = ListenBrainzConnectionSettings()
        assert s.api_url == DEFAULT_LISTENBRAINZ_API_URL == LISTENBRAINZ_API_URL
        assert s.api_url == "https://api.listenbrainz.org"

    def test_lastfm_defaults_match_hardcoded_hosts(self):
        s = LastFmConnectionSettings()
        assert s.api_url == DEFAULT_LASTFM_API_URL == LASTFM_API_URL
        assert s.api_url == "https://ws.audioscrobbler.com/2.0/"
        assert s.auth_url == DEFAULT_LASTFM_AUTH_URL == LASTFM_AUTH_URL
        assert s.auth_url == "https://www.last.fm/api/auth/"

    def test_listenbrainz_trailing_slash_stripped(self):
        s = ListenBrainzConnectionSettings(api_url="https://lb.example.com/")
        assert s.api_url == "https://lb.example.com"

    def test_listenbrainz_whitespace_stripped(self):
        s = ListenBrainzConnectionSettings(api_url="  https://lb.example.com  ")
        assert s.api_url == "https://lb.example.com"

    @pytest.mark.parametrize("bad", ["", "   ", "ftp://lb.example.com", "not a url"])
    def test_listenbrainz_invalid_falls_back_to_default(self, bad: str):
        s = ListenBrainzConnectionSettings(api_url=bad)
        assert s.api_url == DEFAULT_LISTENBRAINZ_API_URL

    def test_lastfm_api_url_gets_single_trailing_slash(self):
        s = LastFmConnectionSettings(api_url="https://libre.fm/2.0")
        assert s.api_url == "https://libre.fm/2.0/"
        s = LastFmConnectionSettings(api_url="https://maloja.example.com/apis/mlj/2.0//")
        assert s.api_url == "https://maloja.example.com/apis/mlj/2.0/"

    def test_lastfm_auth_url_gets_single_trailing_slash(self):
        s = LastFmConnectionSettings(auth_url="https://libre.fm/api/auth")
        assert s.auth_url == "https://libre.fm/api/auth/"

    @pytest.mark.parametrize("bad", ["", "gopher://x", "ws.audioscrobbler.com"])
    def test_lastfm_invalid_urls_fall_back_to_defaults(self, bad: str):
        s = LastFmConnectionSettings(api_url=bad, auth_url=bad)
        assert s.api_url == DEFAULT_LASTFM_API_URL
        assert s.auth_url == DEFAULT_LASTFM_AUTH_URL

    def test_lastfm_response_exposes_endpoint_urls(self):
        s = LastFmConnectionSettings(
            api_key="k",
            shared_secret="s",
            api_url="https://libre.fm/2.0/",
            auth_url="https://libre.fm/api/auth/",
        )
        resp = LastFmConnectionSettingsResponse.from_settings(s)
        assert resp.api_url == "https://libre.fm/2.0/"
        assert resp.auth_url == "https://libre.fm/api/auth/"


class TestPreferencesRoundTrip:
    def test_listenbrainz_api_url_round_trip(self, tmp_path: Path):
        settings = Settings()
        settings.config_file_path = tmp_path / "config.json"
        _prefs(tmp_path).save_listenbrainz_connection(
            ListenBrainzConnectionSettings(
                username="alice",
                user_token="tok",
                enabled=True,
                api_url="https://lb.example.com/",
            )
        )
        reloaded = PreferencesService(settings)
        lb = reloaded.get_listenbrainz_connection()
        assert lb.api_url == "https://lb.example.com"
        assert lb.username == "alice"
        assert lb.user_token == "tok"
        assert lb.enabled is True

    def test_listenbrainz_legacy_config_defaults(self, tmp_path: Path):
        # a config saved before api_url existed must read back as the official host
        prefs = _prefs(tmp_path)
        config = prefs._load_config().copy()
        config["listenbrainz_settings"] = {"username": "bob", "user_token": "", "enabled": False}
        prefs._save_config(config)
        assert prefs.get_listenbrainz_connection().api_url == DEFAULT_LISTENBRAINZ_API_URL

    def test_lastfm_endpoint_urls_round_trip(self, tmp_path: Path):
        settings = Settings()
        settings.config_file_path = tmp_path / "config.json"
        _prefs(tmp_path).save_lastfm_connection(
            LastFmConnectionSettings(
                api_key="key",
                shared_secret="secret",
                api_url="https://maloja.example.com/apis/mlj/2.0",
                auth_url="https://maloja.example.com/api/auth",
            )
        )
        lf = PreferencesService(settings).get_lastfm_connection()
        assert lf.api_url == "https://maloja.example.com/apis/mlj/2.0/"
        assert lf.auth_url == "https://maloja.example.com/api/auth/"
        assert lf.api_key == "key"
        assert lf.shared_secret == "secret"

    def test_lastfm_legacy_config_defaults(self, tmp_path: Path):
        prefs = _prefs(tmp_path)
        config = prefs._load_config().copy()
        config["lastfm_settings"] = {"api_key": "", "shared_secret": "", "enabled": False}
        prefs._save_config(config)
        lf = prefs.get_lastfm_connection()
        assert lf.api_url == DEFAULT_LASTFM_API_URL
        assert lf.auth_url == DEFAULT_LASTFM_AUTH_URL


def _lb_repo(base_url: str | None = None) -> tuple[ListenBrainzRepository, AsyncMock]:
    http_client = AsyncMock(spec=httpx.AsyncClient)
    cache = MagicMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    kwargs = {} if base_url is None else {"base_url": base_url}
    repo = ListenBrainzRepository(
        http_client=http_client,
        cache=cache,
        username="user",
        user_token="tok",
        **kwargs,
    )
    return repo, http_client


def _lb_ok_response(json_data=None):
    resp = MagicMock()
    resp.status_code = 200
    resp.json.return_value = json_data or {"status": "ok"}
    resp.content = None
    resp.text = ""
    return resp


class TestListenBrainzCustomBaseUrl:
    @pytest.mark.asyncio
    async def test_scrobble_submit_uses_custom_base_url(self):
        repo, http_client = _lb_repo(base_url="https://lb.example.com")
        http_client.request = AsyncMock(return_value=_lb_ok_response())
        ok = await repo.submit_now_playing(artist_name="Artist", track_name="Track")
        assert ok is True
        called_url = http_client.request.call_args.args[1]
        assert called_url == "https://lb.example.com/1/submit-listens"

    @pytest.mark.asyncio
    async def test_default_base_url_byte_identical(self):
        repo, http_client = _lb_repo()
        http_client.request = AsyncMock(return_value=_lb_ok_response())
        await repo.submit_now_playing(artist_name="A", track_name="T")
        called_url = http_client.request.call_args.args[1]
        assert called_url == "https://api.listenbrainz.org/1/submit-listens"

    def test_trailing_slash_normalized_at_construction(self):
        repo, _ = _lb_repo(base_url="https://lb.example.com/")
        assert repo._base_url == "https://lb.example.com"


def _lfm_ok_response(json_data=None):
    resp = MagicMock()
    resp.status_code = 200
    resp.json.return_value = json_data if json_data is not None else {"artists": {"artist": []}}
    resp.content = None
    resp.text = ""
    return resp


def _lfm_repo(base_url: str | None = None) -> tuple[LastFmRepository, AsyncMock]:
    LastFmRepository.reset_circuit_breaker()
    http_client = AsyncMock(spec=httpx.AsyncClient)
    cache = MagicMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    kwargs = {} if base_url is None else {"base_url": base_url}
    repo = LastFmRepository(
        http_client=http_client,
        cache=cache,
        api_key="key",
        shared_secret="secret",
        **kwargs,
    )
    return repo, http_client


class TestLastFmCustomBaseUrl:
    @pytest.mark.asyncio
    async def test_request_uses_custom_base_url(self):
        repo, http_client = _lfm_repo(base_url="https://libre.fm/2.0/")
        http_client.get = AsyncMock(return_value=_lfm_ok_response())
        valid, _ = await repo.validate_api_key()
        assert valid is True
        assert http_client.get.call_args.args[0] == "https://libre.fm/2.0/"

    @pytest.mark.asyncio
    async def test_default_base_url_byte_identical(self):
        repo, http_client = _lfm_repo()
        http_client.get = AsyncMock(return_value=_lfm_ok_response())
        await repo.validate_api_key()
        assert http_client.get.call_args.args[0] == "https://ws.audioscrobbler.com/2.0/"


class TestLastFmAuthServiceAuthUrl:
    @pytest.mark.asyncio
    async def test_custom_auth_url_in_authorize_link(self):
        token_result = MagicMock()
        token_result.token = "tok-1"
        repo = MagicMock()
        repo.get_token = AsyncMock(return_value=token_result)
        svc = LastFmAuthService(lastfm_repo=repo, auth_url="https://libre.fm/api/auth/")
        token, auth_url = await svc.request_token("api-key")
        assert token == "tok-1"
        assert auth_url == "https://libre.fm/api/auth/?api_key=api-key&token=tok-1"

    @pytest.mark.asyncio
    async def test_default_auth_url_byte_identical(self):
        token_result = MagicMock()
        token_result.token = "tok-2"
        repo = MagicMock()
        repo.get_token = AsyncMock(return_value=token_result)
        svc = LastFmAuthService(lastfm_repo=repo)
        _, auth_url = await svc.request_token("k")
        assert auth_url == "https://www.last.fm/api/auth/?api_key=k&token=tok-2"
