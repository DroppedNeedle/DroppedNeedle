import math

import msgspec
import pytest

from api.v1.schemas.settings import MusicBrainzConnectionSettings


@pytest.mark.parametrize(
    "api_url",
    [
        "https://musicbrainz.org/ws/2",
        "https://mirror.example/ws/2",
    ],
    ids=["official", "custom"],
)
@pytest.mark.parametrize("concurrent_searches", [-2, 0])
def test_concurrent_searches_must_be_positive_for_every_source(
    api_url: str, concurrent_searches: int
) -> None:
    with pytest.raises(msgspec.ValidationError, match="concurrent_searches"):
        MusicBrainzConnectionSettings(
            api_url=api_url,
            rate_limit=1.0,
            concurrent_searches=concurrent_searches,
        )


@pytest.mark.parametrize(
    "rate_limit",
    [
        pytest.param(math.nan, id="nan"),
        pytest.param(math.inf, id="positive-infinity"),
        pytest.param(-math.inf, id="negative-infinity"),
    ],
)
@pytest.mark.parametrize(
    "api_url",
    [
        "https://musicbrainz.org/ws/2",
        "https://mirror.example/ws/2",
    ],
    ids=["official", "custom"],
)
def test_rate_limit_must_be_finite_for_every_source(
    api_url: str, rate_limit: float
) -> None:
    with pytest.raises(msgspec.ValidationError, match="finite"):
        MusicBrainzConnectionSettings(
            api_url=api_url,
            rate_limit=rate_limit,
            concurrent_searches=4,
        )


def test_default_musicbrainz_settings_use_official_limits() -> None:
    settings = MusicBrainzConnectionSettings()

    assert settings.api_url == "https://musicbrainz.org/ws/2"
    assert settings.rate_limit == 1.0
    assert settings.concurrent_searches == 6
    assert settings.clamped_to_official_limits is False


def test_official_zero_rate_and_high_concurrency_are_clamped() -> None:
    settings = MusicBrainzConnectionSettings(
        api_url="https://musicbrainz.org/ws/2",
        rate_limit=0.0,
        concurrent_searches=8,
    )

    assert settings.rate_limit == 1.0
    assert settings.concurrent_searches == 6
    assert settings.clamped_to_official_limits is True


def test_custom_boundary_values_remain_valid() -> None:
    settings = MusicBrainzConnectionSettings(
        api_url="https://mirror.example/ws/2",
        rate_limit=500.0,
        concurrent_searches=64,
    )
    unlimited = MusicBrainzConnectionSettings(
        api_url="https://mirror.example/ws/2",
        rate_limit=0.0,
        concurrent_searches=1,
    )

    assert (settings.rate_limit, settings.concurrent_searches) == (500.0, 64)
    assert settings.clamped_to_official_limits is False
    assert (unlimited.rate_limit, unlimited.concurrent_searches) == (0.0, 1)
