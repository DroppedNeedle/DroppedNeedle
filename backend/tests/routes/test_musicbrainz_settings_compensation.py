import asyncio
import math
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import HTTPException

from api.v1.routes.settings import update_musicbrainz_settings
from api.v1.schemas.settings import (
    MusicBrainzConnectionSettings,
    MusicBrainzSettingsUpdate,
)
from infrastructure.msgspec_fastapi import MsgSpecBody


def _settings(
    api_url: str,
    rate_limit: float = 2.0,
    concurrent_searches: int = 4,
    *,
    source_id: str = "source-id",
    generation: int = 2,
):
    return MusicBrainzConnectionSettings(
        source_mode="mirror",
        selected_source_mode="mirror",
        api_url=api_url,
        rate_limit=rate_limit,
        concurrent_searches=concurrent_searches,
        source_id=source_id,
        generation=generation,
    )


def _update(
    api_url: str,
    rate_limit: float = 2.0,
    concurrent_searches: int = 4,
):
    return MusicBrainzSettingsUpdate(
        source_mode="mirror",
        api_url=api_url,
        rate_limit=rate_limit,
        concurrent_searches=concurrent_searches,
    )


@pytest.mark.asyncio
async def test_musicbrainz_route_delegates_atomic_update_to_settings_service():
    incoming = _update(
        "https://new.example/ws/2", rate_limit=3.0, concurrent_searches=8
    )
    new_settings = _settings(
        "https://new.example/ws/2",
        rate_limit=3.0,
        concurrent_searches=8,
        source_id="new-source-id",
        generation=3,
    )
    settings_service = MagicMock()
    settings_service.save_musicbrainz_update = AsyncMock(
        side_effect=[RuntimeError("synthetic cache clear failure"), new_settings]
    )
    preferences_service = MagicMock()

    with pytest.raises(RuntimeError, match="synthetic cache clear failure"):
        await update_musicbrainz_settings(
            incoming,
            preferences_service=preferences_service,
            settings_service=settings_service,
        )
    result = await update_musicbrainz_settings(
        incoming,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )

    assert result == new_settings
    settings_service.save_musicbrainz_update.assert_awaited()


@pytest.mark.asyncio
async def test_musicbrainz_route_preserves_cancellation_from_coordinator():
    incoming = _update("https://new.example/ws/2")
    preferences_service = MagicMock()
    settings_service = MagicMock()
    settings_service.save_musicbrainz_update = AsyncMock(
        side_effect=asyncio.CancelledError
    )

    with pytest.raises(asyncio.CancelledError):
        await update_musicbrainz_settings(
            incoming,
            preferences_service=preferences_service,
            settings_service=settings_service,
        )


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
