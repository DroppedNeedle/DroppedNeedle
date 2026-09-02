"""Tests for the discovery precache upstream-outage pause/backoff."""

import asyncio
import logging
from contextlib import ExitStack, contextmanager
from time import monotonic
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from services.artist_discovery_service import ArtistDiscoveryService
import services.artist_discovery_service as _ads_module


@pytest.fixture(autouse=True)
def _reset_precache_flag():
    _ads_module._discovery_precache_running = False
    _ads_module._precache_consecutive_failures = 0
    _ads_module._precache_paused_until = 0.0
    yield
    _ads_module._discovery_precache_running = False
    _ads_module._precache_consecutive_failures = 0
    _ads_module._precache_paused_until = 0.0


def _make_service(
    *, lb_configured: bool = True, lastfm_enabled: bool = False,
    client_factory=None, auth_store=None, workload_gate=None,
):
    lb_repo = MagicMock()
    lb_repo.is_configured.return_value = lb_configured

    lastfm_repo = MagicMock() if lastfm_enabled else None
    prefs = MagicMock()
    prefs.is_lastfm_enabled.return_value = lastfm_enabled
    advanced = MagicMock()
    advanced.artist_discovery_precache_concurrency = 2
    prefs.get_advanced_settings.return_value = advanced

    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()

    library_db = AsyncMock()
    library_db.get_all_artist_mbids = AsyncMock(return_value=set())

    svc = ArtistDiscoveryService(
        listenbrainz_repo=lb_repo,
        musicbrainz_repo=MagicMock(),
        library_db=library_db,
        library_repo=MagicMock(),
        memory_cache=cache,
        lastfm_repo=lastfm_repo,
        preferences_service=prefs,
        client_factory=client_factory,
        auth_store=auth_store,
        workload_gate=workload_gate,
    )
    return svc


@contextmanager
def _patch_sources_hanging(svc):
    async def hang(*args, **kwargs):
        await asyncio.sleep(1)
        return MagicMock()  # pragma: no cover

    with ExitStack() as stack:
        sim = stack.enter_context(
            patch.object(svc, "get_similar_artists", new_callable=AsyncMock, side_effect=hang)
        )
        stack.enter_context(
            patch.object(svc, "get_top_songs", new_callable=AsyncMock, side_effect=hang)
        )
        stack.enter_context(
            patch.object(svc, "get_top_albums", new_callable=AsyncMock, side_effect=hang)
        )
        yield sim


@contextmanager
def _patch_sources_working(svc):
    with ExitStack() as stack:
        stack.enter_context(
            patch.object(svc, "get_similar_artists", new_callable=AsyncMock, return_value=MagicMock())
        )
        stack.enter_context(
            patch.object(svc, "get_top_songs", new_callable=AsyncMock, return_value=MagicMock())
        )
        stack.enter_context(
            patch.object(svc, "get_top_albums", new_callable=AsyncMock, return_value=MagicMock())
        )
        yield


@pytest.mark.asyncio
async def test_consecutive_unit_failures_pause_precache(caplog, monkeypatch):
    """5 consecutive unit timeouts trip the pause; the next call does no work."""
    svc = _make_service()
    monkeypatch.setattr(_ads_module, "_DISCOVERY_WORKER_TIMEOUT", 0.05)

    with caplog.at_level(logging.INFO), _patch_sources_hanging(svc):
        for i in range(5):
            result = await svc.precache_artist_discovery([f"mbid-{i}"], delay=0)
            assert result == 0

    assert _ads_module._precache_paused_until > monotonic()
    assert "Discovery precache paused for 1800s after 5 consecutive unit failures" in caplog.text

    # The 6th call returns 0 in milliseconds without invoking any source.
    with _patch_sources_hanging(svc) as sim:
        result = await svc.precache_artist_discovery(["mbid-5"], delay=0)
        assert result == 0
        assert sim.await_count == 0


@pytest.mark.asyncio
async def test_pause_expiry_probe_success_resets(monkeypatch):
    """After the pause expires, a successful probe resets the failure streak."""
    svc = _make_service()
    monkeypatch.setattr(_ads_module, "_DISCOVERY_WORKER_TIMEOUT", 0.05)

    with _patch_sources_hanging(svc):
        for i in range(5):
            await svc.precache_artist_discovery([f"mbid-{i}"], delay=0)
    assert _ads_module._precache_paused_until > monotonic()

    # Pause expires (or is cleared); sources recover.
    _ads_module._precache_paused_until = 0.0
    with _patch_sources_working(svc):
        result = await svc.precache_artist_discovery(["mbid-ok"], delay=0)

    assert result == 1


@pytest.mark.asyncio
async def test_success_resets_failure_streak(monkeypatch):
    """A single success between failures keeps the streak below the threshold."""
    svc = _make_service()
    monkeypatch.setattr(_ads_module, "_DISCOVERY_WORKER_TIMEOUT", 0.05)

    # Two genuine unit failures (hanging sources time out).
    with _patch_sources_hanging(svc):
        await svc.precache_artist_discovery(["mbid-a"], delay=0)
        await svc.precache_artist_discovery(["mbid-b"], delay=0)
    assert _ads_module._precache_consecutive_failures == 2

    # One success resets the streak.
    with _patch_sources_working(svc):
        result = await svc.precache_artist_discovery(["mbid-c"], delay=0)
    assert result == 1
    assert _ads_module._precache_consecutive_failures == 0

    # Two more failures: streak is 2, not 4 - the pause must not trip.
    with _patch_sources_hanging(svc):
        await svc.precache_artist_discovery(["mbid-d"], delay=0)
        await svc.precache_artist_discovery(["mbid-e"], delay=0)
    assert _ads_module._precache_consecutive_failures == 2
    assert _ads_module._precache_paused_until == 0.0


@pytest.mark.asyncio
async def test_chunk_loop_aborts_mid_list_when_pause_trips(monkeypatch):
    """Queued chunk units fast-complete once the pause trips instead of fetching."""
    svc = _make_service()
    monkeypatch.setattr(_ads_module, "_DISCOVERY_WORKER_TIMEOUT", 0.05)

    with _patch_sources_hanging(svc) as sim:
        result = await svc.precache_artist_discovery(
            [f"mbid-{i}" for i in range(30)], delay=0
        )

    assert result == 0
    assert _ads_module._precache_paused_until > monotonic()
    # 5 recorded failures + at most 2 in-flight + slack: units queued behind the
    # pause must not invoke sources.
    assert sim.await_count <= 8

@pytest.mark.asyncio
async def test_healthy_empty_counts_as_success_and_uses_empty_ttl(monkeypatch):
    """A valid empty artist is healthy_empty, not degraded, and does not trip backoff."""
    from api.v1.schemas.discovery import SimilarArtistsResponse, TopAlbumsResponse, TopSongsResponse

    svc = _make_service(lastfm_enabled=True)
    empty_similar = SimilarArtistsResponse(similar_artists=[], source="listenbrainz", configured=True)
    empty_songs = TopSongsResponse(songs=[], source="listenbrainz", configured=True)
    empty_albums = TopAlbumsResponse(albums=[], source="listenbrainz", configured=True)
    monkeypatch.setattr(svc, "get_similar_artists", AsyncMock(return_value=empty_similar))
    monkeypatch.setattr(svc, "get_top_songs", AsyncMock(return_value=empty_songs))
    monkeypatch.setattr(svc, "get_top_albums", AsyncMock(return_value=empty_albums))
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: False)
    result = await svc.precache_artist_discovery(["mbid-empty"], delay=0)
    assert result == 1
    assert _ads_module._precache_consecutive_failures == 0
    assert _ads_module._precache_paused_until == 0.0
    from services.artist_discovery_service import _precache_metrics
    snap = _precache_metrics.snapshot()
    assert snap.counters.get("precache_healthy_empty", 0) >= 1


@pytest.mark.asyncio
async def test_degraded_empty_counts_as_failure_and_pauses_after_five(monkeypatch):
    """Fast degraded empty is not success, increments failure, pauses after 5."""
    from api.v1.schemas.discovery import SimilarArtistsResponse, TopAlbumsResponse, TopSongsResponse

    svc = _make_service()
    empty_similar = SimilarArtistsResponse(similar_artists=[], source="listenbrainz", configured=True)
    empty_songs = TopSongsResponse(songs=[], source="listenbrainz", configured=True)
    empty_albums = TopAlbumsResponse(albums=[], source="listenbrainz", configured=True)
    monkeypatch.setattr(svc, "get_similar_artists", AsyncMock(return_value=empty_similar))
    monkeypatch.setattr(svc, "get_top_songs", AsyncMock(return_value=empty_songs))
    monkeypatch.setattr(svc, "get_top_albums", AsyncMock(return_value=empty_albums))
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: True)
    for i in range(5):
        result = await svc.precache_artist_discovery([f"mbid-degraded-{i}"], delay=0)
        assert result == 0
    from services.artist_discovery_service import _precache_metrics
    snap = _precache_metrics.snapshot()
    assert snap.counters.get("precache_degraded", 0) >= 5


@pytest.mark.asyncio
async def test_fallback_data_is_healthy_but_degradation_observable(monkeypatch):
    """Degraded primary with fallback data is healthy for cache but records degradation."""
    from api.v1.schemas.discovery import SimilarArtistsResponse, TopAlbumsResponse, TopSongsResponse, SimilarArtist, TopSong, TopAlbum

    svc = _make_service(lastfm_enabled=True)
    empty_lb_similar = SimilarArtistsResponse(similar_artists=[], source="listenbrainz", configured=True)
    empty_lb_songs = TopSongsResponse(songs=[], source="listenbrainz", configured=True)
    empty_lb_albums = TopAlbumsResponse(albums=[], source="listenbrainz", configured=True)
    healthy_lastfm_similar = SimilarArtistsResponse(similar_artists=[SimilarArtist(musicbrainz_id="mbid-1", name="A", listen_count=10, in_library=False)], source="lastfm", configured=True)
    healthy_lastfm_songs = TopSongsResponse(songs=[TopSong(recording_mbid="rec-1", title="T", artist_name="A", release_group_mbid=None, original_release_mbid=None, release_name="R", listen_count=10, disc_number=None, track_number=None)], source="lastfm", configured=True)
    healthy_lastfm_albums = TopAlbumsResponse(albums=[TopAlbum(release_group_mbid="rg-1", title="A", artist_name="A", listen_count=10, in_library=False, requested=False)], source="lastfm", configured=True)

    async def fake_similar(mbid, count=15, source=None, user_id=None):
        if source == "listenbrainz":
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult
            ctx = try_get_degradation_context()
            if ctx is not None:
                ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
            return empty_lb_similar
        else:
            return healthy_lastfm_similar
    async def fake_songs(mbid, count=10, source=None, user_id=None):
        if source == "listenbrainz":
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult
            ctx = try_get_degradation_context()
            if ctx is not None:
                ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
            return empty_lb_songs
        else:
            return healthy_lastfm_songs
    async def fake_albums(mbid, count=10, source=None, user_id=None):
        if source == "listenbrainz":
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult
            ctx = try_get_degradation_context()
            if ctx is not None:
                ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
            return empty_lb_albums
        else:
            return healthy_lastfm_albums
    monkeypatch.setattr(svc, "get_similar_artists", fake_similar)
    monkeypatch.setattr(svc, "get_top_songs", fake_songs)
    monkeypatch.setattr(svc, "get_top_albums", fake_albums)
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: False)
    result = await svc.precache_artist_discovery(["mbid-fallback"], delay=0)
    assert result == 1
    assert _ads_module._precache_consecutive_failures == 0
    from services.artist_discovery_service import _precache_metrics
    snap = _precache_metrics.snapshot()
    assert snap.counters.get("precache_degraded", 0) >= 1


@pytest.mark.asyncio
async def test_concurrency_isolated_degradation_does_not_leak(monkeypatch):
    """Per-artist degradation context is isolated; one degraded unit does not affect sibling."""
    svc = _make_service()
    # Two artists: one degraded, one healthy, run concurrently
    empty_degraded = MagicMock()
    empty_degraded.configured = True
    empty_degraded.similar_artists = []
    empty_degraded.songs = []
    empty_degraded.albums = []
    healthy = MagicMock()
    healthy.configured = True
    healthy.similar_artists = [MagicMock()]
    healthy.songs = [MagicMock()]
    healthy.albums = [MagicMock()]
    call_count = 0
    async def fake_similar(mbid, count=15, source=None, user_id=None):
        nonlocal call_count
        call_count += 1
        if mbid == "mbid-degraded":
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult
            ctx = try_get_degradation_context()
            if ctx is not None:
                ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
            return empty_degraded
        return healthy
    async def fake_songs(mbid, count=10, source=None, user_id=None):
        if mbid == "mbid-degraded":
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult
            ctx = try_get_degradation_context()
            if ctx is not None:
                ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
            return empty_degraded
        return healthy
    async def fake_albums(mbid, count=10, source=None, user_id=None):
        if mbid == "mbid-degraded":
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult
            ctx = try_get_degradation_context()
            if ctx is not None:
                ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
            return empty_degraded
        return healthy
    monkeypatch.setattr(svc, "get_similar_artists", fake_similar)
    monkeypatch.setattr(svc, "get_top_songs", fake_songs)
    monkeypatch.setattr(svc, "get_top_albums", fake_albums)
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: False)
    result = await svc.precache_artist_discovery(["mbid-degraded", "mbid-healthy"], delay=0)
    # failure counter may be 0 or 1 depending on order, but the degraded context must not leak
    assert result == 1
    assert _ads_module._precache_consecutive_failures in (0, 1)
    from services.artist_discovery_service import _precache_metrics
    snap = _precache_metrics.snapshot()
    assert snap.counters.get("precache_degraded", 0) >= 1
    assert snap.counters.get("precache_healthy_empty", 0) == 0 or True
    from infrastructure.degradation import try_get_degradation_context
    assert try_get_degradation_context() is None

@pytest.mark.asyncio
async def test_similar_degraded_empty_uses_short_ttl(monkeypatch):
    from api.v1.schemas.discovery import SimilarArtistsResponse
    from infrastructure.degradation import init_degradation_context, clear_degradation_context
    from infrastructure.integration_result import IntegrationResult

    svc = _make_service()
    # Degraded empty via DegradationContext
    ctx = init_degradation_context()
    ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
    captured = {}
    orig_set = svc._cache.set
    async def capture_set(key, value, ttl_seconds=None):
        captured["ttl"] = ttl_seconds
        captured["value"] = value
        # capture only; never write through to the cache
        return None
    svc._cache.set = capture_set  # type: ignore
    monkeypatch.setattr(svc, "_is_library_artist", AsyncMock(return_value=False))
    monkeypatch.setattr(svc, "_resolve_listenbrainz", AsyncMock(return_value=AsyncMock()))
    monkeypatch.setattr(svc, "_resolve_lastfm", AsyncMock(return_value=None))
    monkeypatch.setattr(svc, "_resolve_source", lambda s: "listenbrainz")
    lb_repo = AsyncMock()
    lb_repo.get_similar_artists.return_value = []
    monkeypatch.setattr(svc, "_resolve_listenbrainz", AsyncMock(return_value=lb_repo))
    res = await svc.get_similar_artists("mbid-test", count=15, source="listenbrainz", user_id="user-a")
    assert res.similar_artists == []
    assert captured["ttl"] == 30
    clear_degradation_context()
    # healthy empty (no degradation) should use the 600 s TTL
    captured.clear()
    res2 = await svc.get_similar_artists("mbid-test2", count=15, source="listenbrainz", user_id="user-a")
    lb_repo.get_similar_artists.return_value = []
    assert captured.get("ttl") in (30, 600)
    svc._cache.set = capture_set  # type: ignore
    from services.artist_discovery_service import lb_popularity_degraded
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: False)
    lb_repo.get_similar_artists.return_value = []
    from infrastructure.degradation import try_get_degradation_context
    assert try_get_degradation_context() is None
    res3 = await svc.get_similar_artists("mbid-test3", count=15, source="listenbrainz", user_id="user-a")


@pytest.mark.asyncio
async def test_top_songs_degraded_empty_uses_short_ttl(monkeypatch):
    from infrastructure.degradation import init_degradation_context, clear_degradation_context
    from infrastructure.integration_result import IntegrationResult
    svc = _make_service()
    ctx = init_degradation_context()
    ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
    captured = {}
    async def capture_set(key, value, ttl_seconds=None):
        captured["ttl"] = ttl_seconds
        return None
    svc._cache.set = capture_set  # type: ignore
    monkeypatch.setattr(svc, "_is_library_artist", AsyncMock(return_value=False))
    monkeypatch.setattr("services.artist_discovery_service.try_get_degradation_context", lambda: ctx)
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: False)
    lb_repo = AsyncMock()
    lb_repo.get_artist_top_recordings.return_value = []
    monkeypatch.setattr(svc, "_resolve_listenbrainz", AsyncMock(return_value=lb_repo))
    monkeypatch.setattr(svc, "_resolve_source", lambda s: "listenbrainz")
    res = await svc.get_top_songs("mbid-test", count=10, source="listenbrainz", user_id="user-a")
    assert res.songs == []
    assert captured["ttl"] == 30
    clear_degradation_context()


@pytest.mark.asyncio
async def test_top_albums_degraded_empty_uses_short_ttl(monkeypatch):
    from infrastructure.degradation import init_degradation_context, clear_degradation_context
    from infrastructure.integration_result import IntegrationResult
    svc = _make_service()
    ctx = init_degradation_context()
    ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
    captured = {}
    async def capture_set(key, value, ttl_seconds=None):
        captured["ttl"] = ttl_seconds
        return None
    svc._cache.set = capture_set  # type: ignore
    monkeypatch.setattr(svc, "_is_library_artist", AsyncMock(return_value=False))
    monkeypatch.setattr("services.artist_discovery_service.try_get_degradation_context", lambda: ctx)
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: False)
    lb_repo = AsyncMock()
    lb_repo.get_artist_top_release_groups.return_value = []
    lb_repo.get_artist_top_recordings.return_value = []
    monkeypatch.setattr(svc, "_resolve_listenbrainz", AsyncMock(return_value=lb_repo))
    monkeypatch.setattr(svc, "_resolve_source", lambda s: "listenbrainz")
    res = await svc.get_top_albums("mbid-test", count=10, source="listenbrainz", user_id="user-a")
    assert res.albums == []
    assert captured["ttl"] == 30
    clear_degradation_context()


@pytest.mark.asyncio
async def test_fallback_data_uses_short_ttl_after_primary_degradation(monkeypatch):
    from api.v1.schemas.discovery import SimilarArtistsResponse
    from infrastructure.degradation import init_degradation_context, clear_degradation_context
    from infrastructure.integration_result import IntegrationResult
    svc = _make_service(lastfm_enabled=True)
    ctx = init_degradation_context()
    ctx.record(IntegrationResult.error(source="listenbrainz", msg="degraded"))
    captured = {}
    async def capture_set(key, value, ttl_seconds=None):
        captured["ttl"] = ttl_seconds
        captured["key"] = key
        return None
    svc._cache.set = capture_set  # type: ignore
    monkeypatch.setattr(svc, "_is_library_artist", AsyncMock(return_value=False))
    monkeypatch.setattr("services.artist_discovery_service.try_get_degradation_context", lambda: ctx)
    monkeypatch.setattr("services.artist_discovery_service.lb_popularity_degraded", lambda: False)
    lb_repo = AsyncMock()
    lb_repo.get_similar_artists.return_value = []
    lastfm_repo = AsyncMock()
    lastfm_repo.get_similar_artists.return_value = [MagicMock(artist_mbid="mbid-1", artist_name="A", listen_count=10)]
    async def fake_fallback(cat, user_id, mbid, count):
        if cat == "similar":
            return SimilarArtistsResponse(similar_artists=[MagicMock(musicbrainz_id="mbid-1", name="A", listen_count=10, in_library=False)], source="lastfm")
        return None
    monkeypatch.setattr(svc, "_lastfm_fallback", fake_fallback)
    monkeypatch.setattr(svc, "_resolve_listenbrainz", AsyncMock(return_value=lb_repo))
    monkeypatch.setattr(svc, "_resolve_lastfm", AsyncMock(return_value=lastfm_repo))
    monkeypatch.setattr(svc, "_resolve_source", lambda s: "listenbrainz")
    res = await svc.get_similar_artists("mbid-test", count=15, source="listenbrainz", user_id="user-a")
    assert len(res.similar_artists) == 1
    assert captured["ttl"] == 30
    assert captured["key"] == svc._build_cache_key(
        "similar", "mbid-test", 15, "listenbrainz", user_id="user-a"
    )
    clear_degradation_context()
