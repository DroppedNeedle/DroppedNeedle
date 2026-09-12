"""B2: empty-tracklist negative caching matrix.

- empty + healthy -> cached @600 s (the AlbumTracksInfo doubles as sentinel)
- empty + musicbrainz-degraded -> NOT cached (outage must not pin "no tracks")
- non-empty -> cached at the settings TTL
"""

import pytest
from unittest.mock import AsyncMock, MagicMock

from api.v1.schemas.album import AlbumTracksInfo
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from infrastructure.integration_result import IntegrationResult
from services.album_service import AlbumService

RG = "074aa5b0-712e-4d6c-8d14-8aedc43e84fd"
TRACKS_KEY = f"album_tracks_info:{RG}"


def _make_service() -> tuple[AlbumService, AsyncMock]:
    memory_cache = AsyncMock()
    memory_cache.get = AsyncMock(return_value=None)
    memory_cache.set = AsyncMock()

    prefs = MagicMock()
    prefs.get_advanced_settings.return_value = MagicMock(
        cache_ttl_album_library=21600,
        cache_ttl_album_non_library=3600,
    )

    svc = AlbumService(
        library_repo=AsyncMock(),
        mb_repo=AsyncMock(),
        library_db=None,
        memory_cache=memory_cache,
        disk_cache=AsyncMock(),
        preferences_service=prefs,
    )
    return svc, memory_cache


def _patch_build(svc: AlbumService, tracks: list) -> AsyncMock:
    build = AsyncMock(
        return_value=AlbumTracksInfo(tracks=tracks, total_tracks=len(tracks))
    )
    svc._build_album_tracks_info = build
    svc._provider_album_id = AsyncMock(side_effect=lambda rg: rg)
    return build


class TestEmptyTracklistMatrix:
    @pytest.mark.asyncio
    async def test_empty_healthy_result_cached_at_600s(self):
        svc, cache = _make_service()
        _patch_build(svc, tracks=[])

        result = await svc.get_album_tracks_info(RG)

        assert result.tracks == [] and result.total_tracks == 0
        cache.set.assert_awaited_once_with(TRACKS_KEY, result, ttl_seconds=600)

    @pytest.mark.asyncio
    async def test_empty_degraded_result_not_cached(self):
        svc, cache = _make_service()
        _patch_build(svc, tracks=[])

        ctx = init_degradation_context()
        try:
            ctx.record(IntegrationResult.error(source="musicbrainz", msg="mb down"))
            await svc.get_album_tracks_info(RG)
        finally:
            summary = ctx.degraded_summary()
            clear_degradation_context()

        # Guard narrowed to the musicbrainz source specifically.
        assert "musicbrainz" in summary
        cache.set.assert_not_awaited()  # outage must not pin "no tracks"

    @pytest.mark.asyncio
    async def test_other_source_degradation_does_not_veto_caching(self):
        """Only a musicbrainz degradation vetoes; unrelated sources don't."""
        svc, cache = _make_service()
        _patch_build(svc, tracks=[])

        ctx = init_degradation_context()
        try:
            ctx.record(IntegrationResult.error(source="listenbrainz", msg="lb down"))
            await svc.get_album_tracks_info(RG)
        finally:
            clear_degradation_context()

        cache.set.assert_awaited_once_with(TRACKS_KEY, ANY_EMPTY, ttl_seconds=600)

    @pytest.mark.asyncio
    async def test_positive_result_keeps_settings_ttl(self):
        svc, cache = _make_service()
        tracks = [_track()]
        _patch_build(svc, tracks=tracks)

        await svc.get_album_tracks_info(RG)

        cache.set.assert_awaited_once_with(TRACKS_KEY, POSITIVE, ttl_seconds=3600)

    @pytest.mark.asyncio
    async def test_degraded_key_absent_repeat_view_repays_ladder(self):
        svc, cache = _make_service()
        build = _patch_build(svc, tracks=[])

        ctx = init_degradation_context()
        try:
            ctx.record(IntegrationResult.error(source="musicbrainz", msg="mb down"))
            await svc.get_album_tracks_info(RG)
            await svc.get_album_tracks_info(RG)
        finally:
            clear_degradation_context()

        # Key stayed absent both times: each degraded view re-pays the ladder.
        assert build.await_count == 2
        cache.set.assert_not_awaited()


def _track():
    from api.v1.schemas.album import Track

    return Track(position=1, title="Song")


ANY_EMPTY = AlbumTracksInfo(tracks=[], total_tracks=0)
POSITIVE = AlbumTracksInfo(tracks=[_track()], total_tracks=1)
