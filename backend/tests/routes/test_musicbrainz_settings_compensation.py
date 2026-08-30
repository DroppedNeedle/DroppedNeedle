import asyncio
import math
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import HTTPException

from api.v1.routes.settings import update_musicbrainz_settings
from api.v1.schemas.settings import MusicBrainzConnectionSettings
from infrastructure.msgspec_fastapi import MsgSpecBody


def _settings(api_url: str, rate_limit: float = 2.0, concurrent_searches: int = 4):
    return MusicBrainzConnectionSettings(
        api_url=api_url,
        rate_limit=rate_limit,
        concurrent_searches=concurrent_searches,
    )


@pytest.mark.asyncio
async def test_musicbrainz_route_compensates_persistence_when_apply_fails():
    previous = _settings("https://old.example/ws/2")
    incoming = _settings(
        "https://new.example/ws/2", rate_limit=3.0, concurrent_searches=8
    )
    persisted = {"value": previous}
    runtime = {"value": previous}
    apply_attempts = 0

    preferences_service = MagicMock()

    def save(settings):
        persisted["value"] = settings

    preferences_service.get_musicbrainz_connection.return_value = previous
    preferences_service.save_musicbrainz_connection.side_effect = save

    settings_service = MagicMock()

    async def apply(settings):
        nonlocal apply_attempts
        apply_attempts += 1
        if apply_attempts == 1:
            raise RuntimeError("synthetic cache clear failure")
        runtime["value"] = settings

    settings_service.on_musicbrainz_settings_changed.side_effect = apply

    with pytest.raises(RuntimeError, match="synthetic cache clear failure"):
        await update_musicbrainz_settings(
            incoming,
            preferences_service=preferences_service,
            settings_service=settings_service,
        )

    assert persisted["value"] == previous
    assert runtime["value"] == previous

    result = await update_musicbrainz_settings(
        incoming,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )

    assert result == incoming
    assert persisted["value"] == incoming
    assert runtime["value"] == incoming
    assert apply_attempts == 2


@pytest.mark.asyncio
async def test_musicbrainz_route_compensates_persistence_on_cancellation():
    previous = _settings("https://old.example/ws/2")
    incoming = _settings("https://new.example/ws/2")
    saved = []

    preferences_service = MagicMock()
    preferences_service.get_musicbrainz_connection.return_value = previous
    preferences_service.save_musicbrainz_connection.side_effect = saved.append

    settings_service = MagicMock()
    settings_service.on_musicbrainz_settings_changed = AsyncMock(
        side_effect=asyncio.CancelledError
    )

    with pytest.raises(asyncio.CancelledError):
        await update_musicbrainz_settings(
            incoming,
            preferences_service=preferences_service,
            settings_service=settings_service,
        )

    assert saved == [incoming, previous]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "api_url",
    [
        "https://musicbrainz.org/ws/2",
        "https://mirror.example/ws/2",
    ],
    ids=["official", "custom"],
)
@pytest.mark.parametrize("concurrent_searches", [-2, 0])
async def test_musicbrainz_route_decoder_rejects_nonpositive_concurrency(
    api_url: str, concurrent_searches: int
) -> None:
    decoder = MsgSpecBody(MusicBrainzConnectionSettings).dependency

    with pytest.raises(HTTPException) as raised:
        await decoder(
            payload={
                "api_url": api_url,
                "rate_limit": 1.0,
                "concurrent_searches": concurrent_searches,
            }
        )

    assert raised.value.status_code == 422
    assert "concurrent_searches" in str(raised.value.detail)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "api_url",
    [
        "https://musicbrainz.org/ws/2",
        "https://mirror.example/ws/2",
    ],
    ids=["official", "custom"],
)
@pytest.mark.parametrize(
    "rate_limit",
    [
        pytest.param(math.nan, id="nan"),
        pytest.param(math.inf, id="positive-infinity"),
        pytest.param(-math.inf, id="negative-infinity"),
    ],
)
async def test_musicbrainz_route_decoder_rejects_nonfinite_rate(
    api_url: str, rate_limit: float
) -> None:
    decoder = MsgSpecBody(MusicBrainzConnectionSettings).dependency

    with pytest.raises(HTTPException) as raised:
        await decoder(
            payload={
                "api_url": api_url,
                "rate_limit": rate_limit,
                "concurrent_searches": 4,
            }
        )

    assert raised.value.status_code == 422
    assert "finite" in str(raised.value.detail)
