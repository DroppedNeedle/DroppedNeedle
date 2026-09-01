import math

import msgspec
import pytest

from api.v1.schemas.settings import (
    BRAINZMASH_DISCLOSURE_VERSION,
    BRAINZMASH_ENDPOINT,
    BrainzMashActiveBinding,
    MusicBrainzConnectionSettings,
    is_brainzmash_active_binding_valid,
    MusicBrainzSettingsUpdate,
)


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
            source_mode="mirror" if "mirror.example" in api_url else "official",
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
            source_mode="mirror" if "mirror.example" in api_url else "official",
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


@pytest.mark.parametrize("source_mode", ["mirror", "community"])
def test_non_official_update_rejects_malformed_endpoint(source_mode: str) -> None:
    with pytest.raises(msgspec.ValidationError, match="absolute HTTP"):
        MusicBrainzSettingsUpdate(source_mode=source_mode, api_url="not-an-url")


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
        source_mode="mirror",
        api_url="https://mirror.example/ws/2",
        rate_limit=500.0,
        concurrent_searches=64,
    )
    unlimited = MusicBrainzConnectionSettings(
        source_mode="mirror",
        api_url="https://mirror.example/ws/2",
        rate_limit=0.0,
        concurrent_searches=1,
    )

    assert (settings.rate_limit, settings.concurrent_searches) == (500.0, 64)
    assert settings.clamped_to_official_limits is False
    assert (unlimited.rate_limit, unlimited.concurrent_searches) == (0.0, 1)


def test_quarantined_complete_active_binding_is_not_valid() -> None:
    settings = MusicBrainzConnectionSettings(
        source_mode="brainzmash",
        api_url=BRAINZMASH_ENDPOINT,
        source_id="source-1",
        generation=3,
        source_quarantined=True,
        active_brainzmash=BrainzMashActiveBinding(
            endpoint=BRAINZMASH_ENDPOINT,
            access_revision="access-1",
            source_id="source-1",
            generation=3,
            disclosure_version=BRAINZMASH_DISCLOSURE_VERSION,
            consented=True,
            verified=True,
        ),
    )

    assert is_brainzmash_active_binding_valid(settings) is False
