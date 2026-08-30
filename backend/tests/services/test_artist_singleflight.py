import asyncio

import pytest
from unittest.mock import AsyncMock, MagicMock, ANY

from api.v1.schemas.artist import ArtistInfo
from core.exceptions import ExternalServiceError
from services.artist_service import ArtistService


MBID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"


def _fake_artist_info() -> ArtistInfo:
    return ArtistInfo(
        name="Test Artist",
        musicbrainz_id=MBID,
    )


def _make_service() -> ArtistService:
    mb = AsyncMock()
    library_repo = AsyncMock()
    library_repo.is_configured.return_value = False
    wikidata = AsyncMock()
    prefs = MagicMock()
    mem_cache = AsyncMock()
    mem_cache.get = AsyncMock(return_value=None)
    mem_cache.set = AsyncMock()
    disk_cache = MagicMock()
    disk_cache.get_artist = AsyncMock(return_value=None)
    disk_cache.set_artist = AsyncMock()
    audiodb_img = MagicMock()
    audiodb_img.fetch_and_cache_artist_images = AsyncMock(return_value=None)

    svc = ArtistService(
        mb_repo=mb,
        library_repo=library_repo,
        wikidata_repo=wikidata,
        preferences_service=prefs,
        memory_cache=mem_cache,
        disk_cache=disk_cache,
        audiodb_image_service=audiodb_img,
    )
    return svc


class TestArtistSingleflight:
    @pytest.mark.asyncio
    async def test_concurrent_calls_fetch_once(self):
        """Multiple concurrent get_artist_info calls should invoke
        _do_get_artist_info once."""
        svc = _make_service()
        call_count = 0
        fake = _fake_artist_info()

        async def counting_fetch(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            await asyncio.sleep(0.05)
            return fake

        svc._do_get_artist_info = counting_fetch

        results = await asyncio.gather(
            svc.get_artist_info(MBID),
            svc.get_artist_info(MBID),
            svc.get_artist_info(MBID),
        )

        assert call_count == 1
        assert all(r.name == "Test Artist" for r in results)

    @pytest.mark.asyncio
    async def test_singleflight_cleared_after_completion(self):
        """After completion, the in-flight dict should be empty."""
        svc = _make_service()
        fake = _fake_artist_info()

        async def quick_fetch(*args, **kwargs):
            return fake

        svc._do_get_artist_info = quick_fetch

        await svc.get_artist_info(MBID)
        assert not svc._artist_in_flight

    @pytest.mark.asyncio
    async def test_singleflight_propagates_exception(self):
        """If fetch raises, all concurrent callers should get the exception."""
        svc = _make_service()

        async def failing_fetch(*args, **kwargs):
            await asyncio.sleep(0.05)
            raise ExternalServiceError("upstream timeout")

        svc._do_get_artist_info = failing_fetch

        results = await asyncio.gather(
            svc.get_artist_info(MBID),
            svc.get_artist_info(MBID),
            svc.get_artist_info(MBID),
            return_exceptions=True,
        )

        assert all(isinstance(r, ExternalServiceError) for r in results)
        assert not svc._artist_in_flight

    @pytest.mark.asyncio
    @pytest.mark.parametrize("path", ["full", "basic"])
    async def test_leader_only_failure_retrieves_shared_future_exception(self, path):
        svc = _make_service()

        async def failing_fetch(*args, **kwargs):
            raise ExternalServiceError("upstream timeout")

        if path == "full":
            svc._do_get_artist_info = failing_fetch
            request = svc.get_artist_info(MBID)
            in_flight = svc._artist_in_flight
        else:
            svc._build_artist_from_musicbrainz = failing_fetch
            request = svc.get_artist_info_basic(MBID)
            in_flight = svc._artist_basic_in_flight

        loop = asyncio.get_running_loop()
        unhandled: list[dict] = []
        previous_handler = loop.get_exception_handler()
        loop.set_exception_handler(lambda _loop, context: unhandled.append(context))
        try:
            with pytest.raises(ExternalServiceError):
                await request
            await asyncio.sleep(0)
        finally:
            loop.set_exception_handler(previous_handler)

        assert not any(
            context.get("message") == "Future exception was never retrieved"
            for context in unhandled
        )
        assert not in_flight

    @pytest.mark.asyncio
    async def test_cancelled_follower_does_not_cancel_leader(self):
        svc = _make_service()
        started = asyncio.Event()
        release = asyncio.Event()
        fake = _fake_artist_info()

        async def waiting_fetch(*args, **kwargs):
            started.set()
            await release.wait()
            return fake

        svc._do_get_artist_info = waiting_fetch
        leader = asyncio.create_task(svc.get_artist_info(MBID))
        await started.wait()
        follower = asyncio.create_task(svc.get_artist_info(MBID))
        await asyncio.sleep(0)

        follower.cancel()
        with pytest.raises(asyncio.CancelledError):
            await follower

        release.set()
        assert await leader is fake
        assert not svc._artist_in_flight

    @pytest.mark.asyncio
    async def test_cache_hit_bypasses_singleflight(self):
        """Cache hit should skip the fetch entirely."""
        svc = _make_service()
        fake = _fake_artist_info()
        svc._cache.get = AsyncMock(return_value=fake)
        call_count = 0

        async def should_not_run(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            return fake

        svc._do_get_artist_info = should_not_run

        result = await svc.get_artist_info(MBID)
        assert result.name == "Test Artist"
        assert call_count == 0

    @pytest.mark.asyncio
    async def test_different_ids_run_independently(self):
        """Different artist_ids should run in parallel."""
        svc = _make_service()
        call_ids: list[str] = []

        async def tracking_fetch(aid, *args, **kwargs):
            call_ids.append(aid)
            await asyncio.sleep(0.02)
            return _fake_artist_info()

        svc._do_get_artist_info = tracking_fetch

        mbid_a = "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee"
        mbid_b = "bbbb2222-bbbb-cccc-dddd-eeeeeeeeeeee"
        await asyncio.gather(
            svc.get_artist_info(mbid_a),
            svc.get_artist_info(mbid_b),
        )

        assert len(call_ids) == 2
        assert mbid_a in call_ids
        assert mbid_b in call_ids

    @pytest.mark.asyncio
    async def test_basic_uses_separate_inflight_dict(self):
        """get_artist_info_basic should use a separate in-flight dict from
        get_artist_info, so they don't cross-contaminate results."""
        svc = _make_service()
        fake = _fake_artist_info()

        full_count = 0
        basic_count = 0

        async def full_fetch(*args, **kwargs):
            nonlocal full_count
            full_count += 1
            await asyncio.sleep(0.05)
            return fake

        svc._do_get_artist_info = full_fetch

        original_build = svc._build_artist_from_musicbrainz

        async def basic_build(*args, **kwargs):
            nonlocal basic_count
            basic_count += 1
            await asyncio.sleep(0.05)
            return fake

        svc._build_artist_from_musicbrainz = basic_build

        results = await asyncio.gather(
            svc.get_artist_info(MBID),
            svc.get_artist_info_basic(MBID),
        )

        assert full_count == 1
        assert basic_count == 1
        assert all(r.name == "Test Artist" for r in results)

    @pytest.mark.asyncio
    async def test_basic_concurrent_calls_fetch_once(self):
        """Multiple concurrent get_artist_info_basic calls should only fetch once."""
        svc = _make_service()
        call_count = 0
        fake = _fake_artist_info()

        async def counting_build(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            await asyncio.sleep(0.05)
            return fake

        svc._build_artist_from_musicbrainz = counting_build

        results = await asyncio.gather(
            svc.get_artist_info_basic(MBID),
            svc.get_artist_info_basic(MBID),
            svc.get_artist_info_basic(MBID),
        )

        assert call_count == 1
        assert all(r.name == "Test Artist" for r in results)
