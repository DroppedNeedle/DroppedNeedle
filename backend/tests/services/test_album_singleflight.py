import asyncio

import msgspec
import pytest
from unittest.mock import AsyncMock, MagicMock

from api.v1.schemas.album import AlbumInfo, AlbumTracksInfo
from core.exceptions import ResourceNotFoundError
from services.album_service import AlbumService

MBID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"


def _fake_album_info() -> AlbumInfo:
    return AlbumInfo(
        title="Test Album",
        artist_name="Test Artist",
        musicbrainz_id=MBID,
        artist_id="artist-" + MBID,
        release_date="2024",
    )


def _make_service() -> AlbumService:
    library_repo = AsyncMock()
    library_repo.is_configured.return_value = False
    mb = AsyncMock()
    lib_cache = AsyncMock()
    mem_cache = AsyncMock()
    mem_cache.get = AsyncMock(return_value=None)
    mem_cache.set = AsyncMock()
    disk_cache = MagicMock()
    disk_cache.get_album = AsyncMock(return_value=None)
    disk_cache.set_album = AsyncMock()
    disk_cache.delete_album = AsyncMock()
    prefs = MagicMock()
    audiodb_img = MagicMock()
    audiodb_img.fetch_and_cache_album_images = AsyncMock(return_value=None)

    svc = AlbumService(
        library_repo=library_repo,
        mb_repo=mb,
        library_db=lib_cache,
        memory_cache=mem_cache,
        disk_cache=disk_cache,
        preferences_service=prefs,
        audiodb_image_service=audiodb_img,
    )
    return svc


class TestAlbumSingleflight:
    @pytest.mark.asyncio
    async def test_concurrent_calls_fetch_once(self):
        """Multiple concurrent get_album_info calls for the same ID
        should only invoke _do_get_album_info once."""
        svc = _make_service()
        call_count = 0
        fake = _fake_album_info()

        async def counting_fetch(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            await asyncio.sleep(0.05)
            return fake

        svc._do_get_album_info = counting_fetch

        results = await asyncio.gather(
            svc.get_album_info(MBID),
            svc.get_album_info(MBID),
            svc.get_album_info(MBID),
        )

        assert call_count == 1
        assert all(r.title == "Test Album" for r in results)

    @pytest.mark.asyncio
    async def test_singleflight_cleared_after_completion(self):
        """After completion, the in-flight dict should be empty."""
        svc = _make_service()
        fake = _fake_album_info()

        async def quick_fetch(*args, **kwargs):
            return fake

        svc._do_get_album_info = quick_fetch

        await svc.get_album_info(MBID)
        assert svc._album_in_flight == {}

    @pytest.mark.asyncio
    async def test_singleflight_propagates_exception(self):
        """If fetch raises, all concurrent callers should get the exception."""
        svc = _make_service()

        async def failing_fetch(*args, **kwargs):
            await asyncio.sleep(0.05)
            raise RuntimeError("upstream timeout")

        svc._do_get_album_info = failing_fetch

        results = await asyncio.gather(
            svc.get_album_info(MBID),
            svc.get_album_info(MBID),
            svc.get_album_info(MBID),
            return_exceptions=True,
        )

        assert all(isinstance(r, ResourceNotFoundError) for r in results)
        assert svc._album_in_flight == {}

    @pytest.mark.asyncio
    async def test_cache_hit_bypasses_singleflight(self):
        """Cache hit should not trigger _do_get_album_info at all."""
        svc = _make_service()
        fake = _fake_album_info()
        svc._get_cached_album_info = AsyncMock(return_value=fake)
        svc._apply_audiodb_album_images = AsyncMock(return_value=fake)
        call_count = 0

        async def should_not_run(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            return fake

        svc._do_get_album_info = should_not_run

        result = await svc.get_album_info(MBID)
        assert result.title == "Test Album"
        assert call_count == 0

    @pytest.mark.asyncio
    async def test_different_ids_run_independently(self):
        """Different release_group_ids should run in parallel."""
        svc = _make_service()
        call_ids: list[str] = []

        async def tracking_fetch(rgid, *args, **kwargs):
            call_ids.append(rgid)
            await asyncio.sleep(0.02)
            return _fake_album_info()

        svc._do_get_album_info = tracking_fetch

        mbid_a = "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee"
        mbid_b = "bbbb2222-bbbb-cccc-dddd-eeeeeeeeeeee"
        await asyncio.gather(
            svc.get_album_info(mbid_a),
            svc.get_album_info(mbid_b),
        )

        assert len(call_ids) == 2
        assert mbid_a in call_ids
        assert mbid_b in call_ids

    @pytest.mark.asyncio
    async def test_follower_cancellation_does_not_break_leader(self):
        """Cancelling a follower task must not poison the shared future."""
        svc = _make_service()
        gate = asyncio.Event()

        async def slow_fetch(*args, **kwargs):
            await gate.wait()
            return _fake_album_info()

        svc._do_get_album_info = slow_fetch

        leader_task = asyncio.create_task(svc.get_album_info(MBID))
        await asyncio.sleep(0)
        follower_task = asyncio.create_task(svc.get_album_info(MBID))
        await asyncio.sleep(0)

        follower_task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await follower_task

        gate.set()
        result = await leader_task
        assert isinstance(result, AlbumInfo)
        assert svc._album_in_flight == {}

    @pytest.mark.asyncio
    async def test_source_switch_separates_leaders_and_followers(self, monkeypatch):
        import repositories.musicbrainz_base as mb_base

        svc = _make_service()
        old_gate = asyncio.Event()
        new_gate = asyncio.Event()
        old_started = asyncio.Event()
        new_started = asyncio.Event()
        calls: list[int] = []
        original_source = mb_base.capture_mb_source_context()
        original_runtime = mb_base.brainzmash_runtime_enabled()
        mb_base.set_mb_api_base(
            "https://old.example/ws/2",
            source_mode="mirror",
            source_id="album-info-old",
            generation=original_source.generation + 1,
        )
        old_generation = mb_base.get_mb_source_generation()

        async def fetch(*_args, **_kwargs):
            generation = mb_base.get_mb_source_generation()
            calls.append(generation)
            if generation == old_generation:
                old_started.set()
                await old_gate.wait()
                title = "Old source"
            else:
                new_started.set()
                await new_gate.wait()
                title = "New source"
            return msgspec.structs.replace(_fake_album_info(), title=title)

        svc._do_get_album_info = fetch
        try:
            old_leader = asyncio.create_task(svc.get_album_info(MBID))
            await old_started.wait()
            old_follower = asyncio.create_task(svc.get_album_info(MBID))
            await asyncio.sleep(0)

            mb_base.set_mb_api_base(
                "https://new.example/ws/2",
                source_mode="mirror",
                source_id="album-info-new",
                generation=old_generation + 1,
            )
            new_generation = mb_base.get_mb_source_generation()
            new_leader = asyncio.create_task(svc.get_album_info(MBID))
            await new_started.wait()
            new_follower = asyncio.create_task(svc.get_album_info(MBID))
            await asyncio.sleep(0)

            assert len(svc._album_in_flight) == 2
            assert (MBID, old_generation) in svc._album_in_flight
            assert (MBID, new_generation) in svc._album_in_flight

            new_gate.set()
            new_leader_result, new_follower_result = await asyncio.gather(
                new_leader, new_follower
            )
            old_gate.set()
            old_leader_result, old_follower_result = await asyncio.gather(
                old_leader, old_follower
            )
        finally:
            old_gate.set()
            new_gate.set()
            mb_base.set_mb_api_base(
                original_source.source_url,
                source_mode=original_source.source_mode,
                source_id=original_source.source_id,
                generation=original_source.generation,
                brainzmash_binding_valid=original_runtime,
            )

        assert calls == [old_generation, new_generation]
        assert old_leader_result.title == "Old source"
        assert old_follower_result.title == "Old source"
        assert new_leader_result.title == "New source"
        assert new_follower_result.title == "New source"
        assert svc._album_in_flight == {}


@pytest.mark.asyncio
async def test_refresh_replacement_does_not_evict_album_singleflight():

    svc = _make_service()
    old_gate = asyncio.Event()
    replacement_gate = asyncio.Event()
    old_started = asyncio.Event()
    replacement_started = asyncio.Event()
    calls = 0

    async def fetch(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            old_started.set()
            await old_gate.wait()
            return msgspec.structs.replace(_fake_album_info(), title="Old")
        replacement_started.set()
        await replacement_gate.wait()
        return msgspec.structs.replace(_fake_album_info(), title="Replacement")

    svc._do_get_album_info = fetch
    try:
        old_leader = asyncio.create_task(svc.get_album_info(MBID))
        await asyncio.wait_for(old_started.wait(), timeout=1)

        refresh_task = asyncio.create_task(svc.refresh_album(MBID))
        await asyncio.wait_for(replacement_started.wait(), timeout=1)
        follower = asyncio.create_task(svc.get_album_info(MBID))
        await asyncio.sleep(0)

        assert (MBID, generation) in svc._album_in_flight
        old_gate.set()
        old_result = await old_leader
        assert old_result.title == "Old"
        assert (MBID, generation) in svc._album_in_flight

        replacement_gate.set()
        refreshed, followed = await asyncio.gather(refresh_task, follower)
        assert refreshed.title == "Replacement"
        assert followed.title == "Replacement"
        assert calls == 2
        assert svc._album_in_flight == {}
    finally:
        old_gate.set()
        replacement_gate.set()


@pytest.mark.asyncio
async def test_refresh_replacement_does_not_evict_tracks_singleflight():

    svc = _make_service()
    old_gate = asyncio.Event()
    replacement_gate = asyncio.Event()
    old_started = asyncio.Event()
    replacement_started = asyncio.Event()
    calls = 0
    old_tracks = AlbumTracksInfo(total_tracks=1)
    replacement_tracks = AlbumTracksInfo(total_tracks=2)

    async def build_tracks(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            old_started.set()
            await old_gate.wait()
            return old_tracks, False
        replacement_started.set()
        await replacement_gate.wait()
        return replacement_tracks, False

    svc._build_album_tracks_info = build_tracks
    svc.get_album_info = AsyncMock(return_value=_fake_album_info())
    try:
        old_leader = asyncio.create_task(svc.get_album_tracks_info(MBID))
        await asyncio.wait_for(old_started.wait(), timeout=1)

        refresh_task = asyncio.create_task(svc.refresh_album(MBID))
        await refresh_task
        replacement_leader = asyncio.create_task(svc.get_album_tracks_info(MBID))
        await asyncio.wait_for(replacement_started.wait(), timeout=1)
        follower = asyncio.create_task(svc.get_album_tracks_info(MBID))
        await asyncio.sleep(0)

        assert (MBID, generation) in svc._tracks_in_flight
        old_gate.set()
        old_result = await old_leader
        assert old_result.total_tracks == 1
        assert (MBID, generation) in svc._tracks_in_flight

        replacement_gate.set()
        replacement_result, followed = await asyncio.gather(
            replacement_leader, follower
        )
        assert replacement_result.total_tracks == 2
        assert followed.total_tracks == 2
        assert calls == 2
        assert svc._tracks_in_flight == {}
    finally:
        old_gate.set()
        replacement_gate.set()
