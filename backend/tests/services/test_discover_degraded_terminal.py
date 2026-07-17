"""Discover empty/failed builds must settle into a short-lived cached terminal state
carrying the build's degradation summary (issue #147) - not loop cache-miss -> rebuild
-> skeleton forever - and must never overwrite a previous meaningful copy."""

import pytest
import threading
from unittest.mock import AsyncMock, MagicMock, patch

from api.v1.schemas.discover import DiscoverResponse
from api.v1.schemas.home import HomeAlbum, HomeSection
from api.v1.schemas.settings import (
    LastFmConnectionSettings,
    ListenBrainzConnectionSettings,
    PrimaryMusicSourceSettings,
)
from services.discover.homepage_service import (
    DISCOVER_CACHE_TTL,
    STALE_REVALIDATE_SECONDS,
    DiscoverHomepageService,
)
from services.discover.integration_helpers import IntegrationHelpers
from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore


def _make_prefs() -> MagicMock:
    prefs = MagicMock()
    prefs.get_listenbrainz_connection.return_value = ListenBrainzConnectionSettings(
        user_token="tok", username="lbuser", enabled=True,
    )
    prefs.get_lastfm_connection.return_value = LastFmConnectionSettings(
        api_key="", shared_secret="", session_key="", username="", enabled=False,
    )
    prefs.is_lastfm_enabled.return_value = False
    prefs.get_primary_music_source.return_value = PrimaryMusicSourceSettings(
        source="listenbrainz"
    )
    for getter in (
        "get_jellyfin_connection",
        "get_download_client_settings",
        "get_youtube_connection",
        "get_local_files_connection",
    ):
        conn = MagicMock()
        conn.enabled = False
        getattr(prefs, getter).return_value = conn
    return prefs


class _FakeCache:
    """Dict-backed cache that records the TTL of every set."""

    def __init__(self) -> None:
        self.data: dict[str, object] = {}
        self.ttls: dict[str, int] = {}

    async def get(self, key: str):
        return self.data.get(key)

    async def set(self, key: str, value: object, ttl: int) -> None:
        self.data[key] = value
        self.ttls[key] = ttl


def _make_service(cache: _FakeCache) -> DiscoverHomepageService:
    return DiscoverHomepageService(
        listenbrainz_repo=AsyncMock(),
        jellyfin_repo=AsyncMock(),
        library_repo=AsyncMock(),
        musicbrainz_repo=AsyncMock(),
        integration=IntegrationHelpers(_make_prefs()),
        mbid_resolution=MagicMock(),
        memory_cache=cache,
    )


def _meaningful_response() -> DiscoverResponse:
    return DiscoverResponse(
        globally_trending=HomeSection(
            title="Trending",
            type="album",
            items=[HomeAlbum(name="Album", artist_name="Artist")],
        )
    )


def _cache_key(service: DiscoverHomepageService) -> str:
    return service._integration.get_discover_cache_key("u1", False, False)


@pytest.mark.asyncio
async def test_empty_build_caches_short_ttl_marker_with_degradation():
    cache = _FakeCache()
    service = _make_service(cache)
    with (
        patch.object(service, "build_discover_data", AsyncMock(return_value=DiscoverResponse())),
        patch("services.discover.homepage_service.lb_popularity_degraded", return_value=True),
    ):
        await service.warm_cache("u1")

    key = _cache_key(service)
    marker = cache.data[key]
    assert isinstance(marker, DiscoverResponse)
    assert marker.service_status == {"listenbrainz": "degraded"}
    assert cache.ttls[key] == STALE_REVALIDATE_SECONDS

    # the marker is now a cache hit: served as a settled (refreshing=false) response
    result = await service.get_discover_data("u1")
    assert result.refreshing is False
    assert result.service_status == {"listenbrainz": "degraded"}


@pytest.mark.asyncio
async def test_empty_build_never_overwrites_meaningful_copy():
    cache = _FakeCache()
    service = _make_service(cache)
    key = _cache_key(service)
    good = _meaningful_response()
    cache.data[key] = good
    with (
        patch.object(service, "build_discover_data", AsyncMock(return_value=DiscoverResponse())),
        patch("services.discover.homepage_service.lb_popularity_degraded", return_value=True),
    ):
        await service.warm_cache("u1")

    assert cache.data[key] is good


@pytest.mark.asyncio
async def test_failed_build_caches_marker_with_degradation():
    cache = _FakeCache()
    service = _make_service(cache)
    with (
        patch.object(
            service, "build_discover_data", AsyncMock(side_effect=RuntimeError("boom"))
        ),
        patch("services.discover.homepage_service.lb_popularity_degraded", return_value=True),
    ):
        await service.warm_cache("u1")

    marker = cache.data[_cache_key(service)]
    assert isinstance(marker, DiscoverResponse)
    assert marker.service_status == {"listenbrainz": "degraded"}


@pytest.mark.asyncio
async def test_meaningful_build_carries_status_and_full_ttl():
    cache = _FakeCache()
    service = _make_service(cache)
    with (
        patch.object(
            service, "build_discover_data", AsyncMock(return_value=_meaningful_response())
        ),
        patch("services.discover.homepage_service.lb_popularity_degraded", return_value=True),
    ):
        await service.warm_cache("u1")

    key = _cache_key(service)
    cached = cache.data[key]
    assert cached.service_status == {"listenbrainz": "degraded"}
    assert cache.ttls[key] == DISCOVER_CACHE_TTL


@pytest.mark.asyncio
async def test_healthy_meaningful_build_has_no_status():
    cache = _FakeCache()
    service = _make_service(cache)
    with (
        patch.object(
            service, "build_discover_data", AsyncMock(return_value=_meaningful_response())
        ),
        patch("services.discover.homepage_service.lb_popularity_degraded", return_value=False),
    ):
        await service.warm_cache("u1")

    assert cache.data[_cache_key(service)].service_status is None


@pytest.mark.asyncio
async def test_last_good_response_survives_service_restart(tmp_path):
    snapshot_store = DiscoverySnapshotStore(tmp_path / "library.db", threading.Lock())
    first_cache = _FakeCache()
    first = _make_service(first_cache)
    first._snapshot_store = snapshot_store
    with patch.object(
        first, "build_discover_data", AsyncMock(return_value=_meaningful_response())
    ):
        await first.warm_cache("u1")

    restarted = _make_service(_FakeCache())
    restarted._snapshot_store = snapshot_store
    response = await restarted.get_discover_data("u1")

    assert response.globally_trending is not None
    assert response.globally_trending.items[0].name == "Album"
    assert response.generated_at is not None
    assert response.refreshing is False
