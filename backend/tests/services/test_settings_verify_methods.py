"""Tests for SettingsService connection verification methods."""

import time
import logging
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from core.exceptions import RateLimitedError

from services.settings_service import (
    SettingsService,
    ListenBrainzVerifyResult,
    NavidromeVerifyResult,
    YouTubeVerifyResult,
    LastFmVerifyResult,
)


def _make_service(*, preferences=None):
    prefs = preferences or MagicMock()
    cache = MagicMock()
    cache.clear_prefix = AsyncMock(return_value=0)
    service = SettingsService(
        preferences_service=prefs,
        cache=cache,
    )
    return service


@pytest.mark.asyncio
async def test_verify_listenbrainz_does_not_reset_circuit_breaker():
    from api.v1.schemas.settings import ListenBrainzConnectionSettings

    service = _make_service()
    settings = ListenBrainzConnectionSettings(username="alice")
    mock_repo_instance = MagicMock()
    mock_repo_instance.validate_username = AsyncMock(
        return_value=(True, "User found with 12 listens")
    )

    with (
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch(
            "repositories.listenbrainz_repository.ListenBrainzRepository"
        ) as MockRepo,
    ):
        MockRepo.return_value = mock_repo_instance
        MockRepo.reset_circuit_breaker = MagicMock()

        result = await service.verify_listenbrainz(settings)

    assert isinstance(result, ListenBrainzVerifyResult)
    assert result.valid is True
    MockRepo.reset_circuit_breaker.assert_not_called()


@pytest.mark.parametrize("user_token", ["", "secret-token"])
@pytest.mark.asyncio
async def test_verify_listenbrainz_propagates_rate_limited_error(user_token):
    from api.v1.schemas.settings import ListenBrainzConnectionSettings

    service = _make_service()
    settings = ListenBrainzConnectionSettings(username="alice", user_token=user_token)
    error = RateLimitedError(
        "provider body sentinel",
        details={"credential": "credential sentinel"},
        retry_after_seconds=9,
    )
    mock_repo_instance = MagicMock()
    mock_repo_instance.validate_username = AsyncMock(side_effect=error)
    mock_repo_instance.validate_token = AsyncMock(side_effect=error)

    with (
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch(
            "repositories.listenbrainz_repository.ListenBrainzRepository"
        ) as MockRepo,
    ):
        MockRepo.return_value = mock_repo_instance

        with pytest.raises(RateLimitedError) as raised:
            await service.verify_listenbrainz(settings)

    assert raised.value is error
    if user_token:
        mock_repo_instance.validate_token.assert_awaited_once_with()
        mock_repo_instance.validate_username.assert_not_awaited()
    else:
        mock_repo_instance.validate_username.assert_awaited_once_with("alice")
        mock_repo_instance.validate_token.assert_not_awaited()


@pytest.mark.asyncio
async def test_listenbrainz_connection_change_resets_shared_state_only():
    service = _make_service()
    service.clear_home_cache = AsyncMock()

    with (
        patch(
            "repositories.listenbrainz_repository.ListenBrainzRepository"
        ) as MockRepo,
        patch("core.dependencies.clear_listenbrainz_dependent_caches") as clear_caches,
        patch(
            "repositories.listenbrainz_repository._reset_listenbrainz_rate_limit_state"
        ) as reset_rate_limit,
    ):
        MockRepo.reset_circuit_breaker = MagicMock()

        await service.on_listenbrainz_connection_changed()

    MockRepo.reset_circuit_breaker.assert_called_once_with()
    clear_caches.assert_called_once_with()
    service.clear_home_cache.assert_awaited_once_with()
    reset_rate_limit.assert_not_called()


@pytest.mark.asyncio
async def test_verify_navidrome_success():
    prefs = MagicMock()
    raw = MagicMock()
    raw.password = "real-password"
    prefs.get_navidrome_connection_raw = MagicMock(return_value=raw)

    service = _make_service(preferences=prefs)

    from api.v1.schemas.settings import NavidromeConnectionSettings

    settings = NavidromeConnectionSettings(
        enabled=True,
        navidrome_url="http://navidrome.local",
        username="admin",
        password="••••••••",
    )

    mock_repo_instance = MagicMock()
    mock_repo_instance.ping = AsyncMock(return_value=True)

    with (
        patch("infrastructure.validators.validate_service_url"),
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch("repositories.navidrome_repository.NavidromeRepository") as MockRepo,
    ):
        MockRepo.return_value = mock_repo_instance
        MockRepo.reset_circuit_breaker = MagicMock()

        result = await service.verify_navidrome(settings)

    assert isinstance(result, NavidromeVerifyResult)
    assert result.valid is True
    assert "success" in result.message.lower()


@pytest.mark.asyncio
async def test_verify_navidrome_ping_fail():
    prefs = MagicMock()
    raw = MagicMock()
    raw.password = "real-password"
    prefs.get_navidrome_connection_raw = MagicMock(return_value=raw)

    service = _make_service(preferences=prefs)

    from api.v1.schemas.settings import NavidromeConnectionSettings

    settings = NavidromeConnectionSettings(
        enabled=True,
        navidrome_url="http://navidrome.local",
        username="admin",
        password="real-password",
    )

    mock_repo_instance = MagicMock()
    mock_repo_instance.ping = AsyncMock(return_value=False)

    with (
        patch("infrastructure.validators.validate_service_url"),
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch("repositories.navidrome_repository.NavidromeRepository") as MockRepo,
    ):
        MockRepo.return_value = mock_repo_instance
        MockRepo.reset_circuit_breaker = MagicMock()

        result = await service.verify_navidrome(settings)

    assert result.valid is False


@pytest.mark.asyncio
async def test_verify_youtube_success():
    service = _make_service()

    from api.v1.schemas.settings import YouTubeConnectionSettings

    settings = YouTubeConnectionSettings(
        enabled=True,
        api_key="test-key",
        daily_quota_limit=100,
    )

    mock_repo_instance = MagicMock()
    mock_repo_instance.verify_api_key = AsyncMock(return_value=(True, "Valid"))

    with (
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch("repositories.youtube.YouTubeRepository") as MockRepo,
    ):
        MockRepo.return_value = mock_repo_instance

        result = await service.verify_youtube(settings)

    assert isinstance(result, YouTubeVerifyResult)
    assert result.valid is True


@pytest.mark.asyncio
async def test_verify_lastfm_api_key_invalid():
    prefs = MagicMock()
    current = MagicMock()
    current.shared_secret = "real-secret"
    current.session_key = ""
    prefs.get_lastfm_connection = MagicMock(return_value=current)

    service = _make_service(preferences=prefs)

    from api.v1.schemas.settings import LastFmConnectionSettings

    settings = LastFmConnectionSettings(
        enabled=True,
        api_key="bad-key",
        shared_secret="real-secret",
        session_key="",
    )

    mock_repo_instance = MagicMock()
    mock_repo_instance.validate_api_key = AsyncMock(
        return_value=(False, "Invalid API key")
    )

    with (
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch("repositories.lastfm_repository.LastFmRepository") as MockRepo,
    ):
        MockRepo.return_value = mock_repo_instance

        result = await service.verify_lastfm(settings)

    assert isinstance(result, LastFmVerifyResult)
    assert result.valid is False
    assert "invalid" in result.message.lower()


@pytest.mark.asyncio
async def test_verify_lastfm_with_session_key():
    prefs = MagicMock()
    current = MagicMock()
    current.shared_secret = "real-secret"
    current.session_key = "real-session-key"
    prefs.get_lastfm_connection = MagicMock(return_value=current)

    service = _make_service(preferences=prefs)

    from api.v1.schemas.settings import LastFmConnectionSettings, LASTFM_SECRET_MASK

    settings = LastFmConnectionSettings(
        enabled=True,
        api_key="good-key",
        shared_secret=LASTFM_SECRET_MASK,
        session_key=LASTFM_SECRET_MASK,
    )

    mock_repo_instance = MagicMock()
    mock_repo_instance.validate_api_key = AsyncMock(return_value=(True, "OK"))
    mock_repo_instance.validate_session = AsyncMock(
        return_value=(True, "Session valid")
    )

    with (
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch("repositories.lastfm_repository.LastFmRepository") as MockRepo,
    ):
        MockRepo.return_value = mock_repo_instance

        result = await service.verify_lastfm(settings)

    assert result.valid is True
    assert "session" in result.message.lower()


@pytest.mark.asyncio
async def test_verify_download_client_uses_submitted_values():
    from models.common import ServiceStatus
    from api.v1.schemas.settings import DownloadClientConnectionSettings

    service = _make_service()
    settings = DownloadClientConnectionSettings(
        url="https://slskd.example.com", api_key="typed-key"
    )

    repo = MagicMock()
    repo.health_check = AsyncMock(
        return_value=ServiceStatus(
            status="ok", version="0.25.1.0", message="slskd 0.25.1.0"
        )
    )
    with patch("core.dependencies.build_slskd_repository", return_value=repo) as build:
        result = await service.verify_download_client(settings)

    assert result.status == "ok"
    assert result.version == "0.25.1.0"
    build.assert_called_once_with("https://slskd.example.com", "typed-key")


@pytest.mark.asyncio
async def test_verify_download_client_masked_key_falls_back_to_stored():
    from models.common import ServiceStatus
    from api.v1.schemas.settings import (
        DownloadClientConnectionSettings,
        DOWNLOAD_CLIENT_API_KEY_MASK,
    )

    prefs = MagicMock()
    prefs.get_download_client_settings_raw = MagicMock(
        return_value=MagicMock(api_key="real-stored-key")
    )
    service = _make_service(preferences=prefs)
    settings = DownloadClientConnectionSettings(
        url="https://slskd.example.com", api_key=DOWNLOAD_CLIENT_API_KEY_MASK
    )

    repo = MagicMock()
    repo.health_check = AsyncMock(
        return_value=ServiceStatus(status="ok", version="1", message="ok")
    )
    with patch("core.dependencies.build_slskd_repository", return_value=repo) as build:
        result = await service.verify_download_client(settings)

    assert result.status == "ok"
    build.assert_called_once_with("https://slskd.example.com", "real-stored-key")


@pytest.mark.asyncio
async def test_verify_download_client_invalid_url_returns_error():
    from api.v1.schemas.settings import DownloadClientConnectionSettings

    service = _make_service()
    # Empty url survives __post_init__ unchanged, so validate_service_url rejects it.
    settings = DownloadClientConnectionSettings(url="", api_key="k")

    result = await service.verify_download_client(settings)

    assert result.status == "error"
    assert "URL" in result.message


@pytest.mark.asyncio
async def test_verify_musicbrainz_uses_conservative_probe_and_keeps_503_semantics():
    import httpx

    from api.v1.schemas.settings import MusicBrainzConnectionSettings

    service = _make_service()
    settings = MusicBrainzConnectionSettings(api_url="https://musicbrainz.org/ws/2")
    probe = AsyncMock(return_value=httpx.Response(503))

    with (
        patch("infrastructure.validators.validate_service_url"),
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch("repositories.musicbrainz_base.mb_api_probe", probe),
    ):
        result = await service.verify_musicbrainz(settings)

    assert result.valid is True
    assert "rate-limited" in result.message
    probe.assert_awaited_once()
    assert probe.await_args.kwargs["params"] == {"query": "test", "limit": 1}


@pytest.mark.asyncio
async def test_musicbrainz_probe_preserves_open_breaker_and_normal_funnel_rejection():
    import httpx

    import repositories.musicbrainz_base as mb_base
    from api.v1.schemas.settings import MusicBrainzConnectionSettings
    from infrastructure.resilience.retry import CircuitOpenError, CircuitState

    service = _make_service()
    settings = MusicBrainzConnectionSettings(api_url="https://musicbrainz.org/ws/2")
    probe = AsyncMock(return_value=httpx.Response(200, content=b"{}"))
    client = MagicMock()
    client.get = AsyncMock()

    breaker = mb_base.mb_circuit_breaker
    previous = breaker.get_state()
    try:
        breaker.state = CircuitState.OPEN
        breaker.failure_count = 5
        breaker.success_count = 0
        breaker.last_failure_time = time.time()
        before_probe = breaker.get_state()

        with (
            patch("infrastructure.validators.validate_service_url"),
            patch("services.settings_service.get_settings", return_value=MagicMock()),
            patch(
                "services.settings_service.get_http_client", return_value=MagicMock()
            ),
            patch("repositories.musicbrainz_base.mb_api_probe", probe),
            patch.object(mb_base, "_http_client", client),
        ):
            result = await service.verify_musicbrainz(settings)
            assert result.valid is True
            with pytest.raises(CircuitOpenError):
                await mb_base.mb_api_get("/artist")

        assert breaker.get_state() == before_probe
        client.get.assert_not_awaited()
    finally:
        breaker.state = CircuitState(previous["state"])
        breaker.failure_count = previous["failure_count"]
        breaker.success_count = previous["success_count"]
        breaker.last_failure_time = previous["last_failure_time"]
        breaker._last_open_warning = 0.0


@pytest.mark.asyncio
async def test_verify_musicbrainz_failure_log_redacts_configured_endpoint(caplog):
    from api.v1.schemas.settings import MusicBrainzConnectionSettings

    service = _make_service()
    settings = MusicBrainzConnectionSettings(
        api_url="https://user:secret@mirror.example/ws/2?token=private"
    )
    error = RuntimeError(f"request failed for {settings.api_url}")

    with (
        patch("infrastructure.validators.validate_service_url"),
        patch("services.settings_service.get_settings", return_value=MagicMock()),
        patch("services.settings_service.get_http_client", return_value=MagicMock()),
        patch(
            "repositories.musicbrainz_base.mb_api_probe", AsyncMock(side_effect=error)
        ),
        caplog.at_level(logging.WARNING, logger="services.settings_service"),
    ):
        result = await service.verify_musicbrainz(settings)

    assert result.valid is False
    assert str(error) not in caplog.text
    assert settings.api_url not in caplog.text
    assert "Failed to verify MusicBrainz connection" in caplog.text


@pytest.mark.asyncio
async def test_quarantined_alternate_probe_rejects_settings_change_during_wire(
    monkeypatch,
):
    import httpx

    import repositories.musicbrainz_base as mb_base
    from api.v1.schemas.settings import (
        BRAINZMASH_DISCLOSURE_VERSION,
        BRAINZMASH_ENDPOINT,
        BrainzMashActiveBinding,
        MusicBrainzConnectionSettings,
    )
    from core.exceptions import ConfigurationError

    before = mb_base.capture_mb_source_context()
    before_runtime = mb_base.brainzmash_runtime_enabled()
    current = MusicBrainzConnectionSettings(
        source_mode="brainzmash",
        api_url=BRAINZMASH_ENDPOINT,
        source_id="quarantined-source",
        generation=8,
        source_quarantined=True,
        active_brainzmash=BrainzMashActiveBinding(
            endpoint=BRAINZMASH_ENDPOINT,
            access_revision="access-8",
            source_id="quarantined-source",
            generation=8,
            disclosure_version=BRAINZMASH_DISCLOSURE_VERSION,
            consented=True,
            verified=True,
        ),
    )
    changed = MusicBrainzConnectionSettings(
        source_mode="brainzmash",
        api_url=BRAINZMASH_ENDPOINT,
        source_id="quarantined-source",
        generation=8,
        source_quarantined=False,
        active_brainzmash=current.active_brainzmash,
    )
    state = {"settings": current, "revision": 12}
    prefs = MagicMock()
    prefs.get_musicbrainz_connection.side_effect = lambda: state["settings"]
    prefs.get_musicbrainz_settings_revision.side_effect = lambda: state["revision"]
    prefs.musicbrainz_settings_match.side_effect = lambda expected: (
        state["settings"] == expected
    )
    service = _make_service(preferences=prefs)
    candidate = MusicBrainzConnectionSettings(
        source_mode="mirror",
        api_url="https://mirror.example/ws/2",
    )
    mb_base.set_mb_api_base(
        BRAINZMASH_ENDPOINT,
        source_mode="brainzmash",
        source_id=current.source_id,
        generation=current.generation,
        brainzmash_binding_valid=False,
    )

    async def probe(*_args, **kwargs):
        assert kwargs["allow_quarantined_alternate"] is True
        assert kwargs["admission_check"]() is True
        state["settings"] = changed
        state["revision"] += 1
        if not kwargs["admission_check"]():
            raise ConfigurationError("MusicBrainz source changed during the request")
        return httpx.Response(200)

    monkeypatch.setattr(mb_base, "mb_api_probe", probe)
    try:
        with (
            patch("infrastructure.validators.validate_service_url"),
            patch("services.settings_service.get_settings", return_value=MagicMock()),
            patch(
                "services.settings_service.get_http_client", return_value=MagicMock()
            ),
        ):
            result = await service.verify_musicbrainz(candidate)
    finally:
        mb_base.set_mb_api_base(
            before.source_url,
            source_mode=before.source_mode,
            source_id=before.source_id,
            generation=before.generation,
            brainzmash_binding_valid=before_runtime,
        )

    assert result.valid is False
