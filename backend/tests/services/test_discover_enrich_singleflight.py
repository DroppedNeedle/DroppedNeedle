import asyncio

import pytest
from unittest.mock import AsyncMock, MagicMock, patch

from api.v1.schemas.discover import DiscoverQueueEnrichment
from api.v1.schemas.settings import (
    ListenBrainzConnectionSettings,
    LastFmConnectionSettings,
    PrimaryMusicSourceSettings,
)
from services.discover_service import DiscoverService


def _make_prefs() -> MagicMock:
    prefs = MagicMock()
    prefs.get_listenbrainz_connection.return_value = ListenBrainzConnectionSettings(
        user_token="tok", username="u", enabled=True
    )
    prefs.get_lastfm_connection.return_value = LastFmConnectionSettings(
        api_key="k", shared_secret="s", session_key="sk", username="u", enabled=False
    )
    prefs.is_lastfm_enabled.return_value = False
    prefs.get_primary_music_source.return_value = PrimaryMusicSourceSettings(
        source="listenbrainz"
    )
    jf = MagicMock()
    jf.enabled = False
    jf.jellyfin_url = ""
    jf.api_key = ""
    prefs.get_jellyfin_connection.return_value = jf
    download_client = MagicMock()
    download_client.enabled = False
    download_client.url = ""
    prefs.get_download_client_settings.return_value = download_client
    yt = MagicMock()
    yt.enabled = False
    yt.api_key = ""
    prefs.get_youtube_connection.return_value = yt
    lf = MagicMock()
    lf.enabled = False
    lf.music_path = ""
    prefs.get_local_files_connection.return_value = lf
    return prefs


def _make_service(
    memory_cache: MagicMock | None = None,
) -> tuple[DiscoverService, AsyncMock]:
    mb_repo = AsyncMock()
    service = DiscoverService(
        listenbrainz_repo=AsyncMock(),
        jellyfin_repo=AsyncMock(),
        library_repo=AsyncMock(),
        musicbrainz_repo=mb_repo,
        preferences_service=_make_prefs(),
        memory_cache=memory_cache,
    )
    return service, mb_repo


FAKE_ENRICHMENT = DiscoverQueueEnrichment(
    artist_mbid="artist-1",
    tags=["rock"],
    release_date="2020",
)

MBID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"


class TestEnrichSingleflight:
    @pytest.mark.asyncio
    async def test_concurrent_calls_run_enrichment_once(self):
        """Multiple concurrent enrich_queue_item calls for the same mbid should only invoke
        _do_enrich_queue_item once; all callers receive the same result."""
        service, mb_repo = _make_service(memory_cache=None)
        call_count = 0
        original_do_enrich = service._enrichment._do_enrich_queue_item

        async def counting_enrich(
            release_group_mbid: str,
            cache_key: str,
            *,
            priority=None,
            source_context=None,
        ):
            nonlocal call_count
            call_count += 1
            await asyncio.sleep(0.05)
            return FAKE_ENRICHMENT

        service._enrichment._do_enrich_queue_item = counting_enrich

        results = await asyncio.gather(
            service.enrich_queue_item(MBID),
            service.enrich_queue_item(MBID),
            service.enrich_queue_item(MBID),
        )

        assert call_count == 1
        assert all(r == FAKE_ENRICHMENT for r in results)

    @pytest.mark.asyncio
    async def test_singleflight_cleared_after_completion(self):
        """After enrichment completes, the in-flight dict should be empty so a second call
        runs the pipeline again (useful if the first result wasn't cached)."""
        service, _ = _make_service(memory_cache=None)

        async def quick_enrich(
            release_group_mbid: str,
            cache_key: str,
            *,
            priority=None,
            source_context=None,
        ):
            return FAKE_ENRICHMENT

        service._enrichment._do_enrich_queue_item = quick_enrich

        await service.enrich_queue_item(MBID)
        assert service._enrichment._enrich_in_flight == {}

    @pytest.mark.asyncio
    async def test_singleflight_propagates_exception_to_all_waiters(self):
        """If enrichment raises, all concurrent callers should receive the same exception."""
        service, _ = _make_service(memory_cache=None)

        async def failing_enrich(release_group_mbid: str, cache_key: str, **kwargs):
            await asyncio.sleep(0.05)
            raise RuntimeError("MB rate limit")

        service._enrichment._do_enrich_queue_item = failing_enrich

        results = await asyncio.gather(
            service.enrich_queue_item(MBID),
            service.enrich_queue_item(MBID),
            service.enrich_queue_item(MBID),
            return_exceptions=True,
        )

        assert all(isinstance(r, RuntimeError) for r in results)
        assert all(str(r) == "MB rate limit" for r in results)
        assert service._enrichment._enrich_in_flight == {}

    @pytest.mark.asyncio
    async def test_memory_cache_hit_skips_singleflight(self):
        """If the enrichment is in the memory cache, singleflight should not be consulted."""
        cache = AsyncMock()
        cache.get = AsyncMock(return_value=FAKE_ENRICHMENT)
        service, _ = _make_service(memory_cache=cache)

        call_count = 0

        async def should_not_run(release_group_mbid: str, cache_key: str):
            nonlocal call_count
            call_count += 1
            return FAKE_ENRICHMENT

        service._enrichment._do_enrich_queue_item = should_not_run

        result = await service.enrich_queue_item(MBID)
        assert result == FAKE_ENRICHMENT
        assert call_count == 0

    @pytest.mark.asyncio
    async def test_memory_cache_miss_triggers_enrichment(self):
        """If the memory cache returns None, the enrichment pipeline should run."""
        cache = AsyncMock()
        cache.get = AsyncMock(return_value=None)
        cache.set = AsyncMock()
        service, _ = _make_service(memory_cache=cache)

        async def simple_enrich(release_group_mbid: str, cache_key: str, **kwargs):
            return FAKE_ENRICHMENT

        service._enrichment._do_enrich_queue_item = simple_enrich

        result = await service.enrich_queue_item(MBID)
        assert result == FAKE_ENRICHMENT

    @pytest.mark.asyncio
    async def test_different_mbids_run_independently(self):
        """Enrichment for different mbids should run independently (no dedup)."""
        service, _ = _make_service(memory_cache=None)
        call_mbids: list[str] = []

        async def tracking_enrich(release_group_mbid: str, cache_key: str, **kwargs):
            call_mbids.append(release_group_mbid)
            await asyncio.sleep(0.02)
            return FAKE_ENRICHMENT

        service._enrichment._do_enrich_queue_item = tracking_enrich

        mbid_a = "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee"
        mbid_b = "bbbb2222-bbbb-cccc-dddd-eeeeeeeeeeee"
        await asyncio.gather(
            service.enrich_queue_item(mbid_a),
            service.enrich_queue_item(mbid_b),
        )

        assert len(call_mbids) == 2
        assert mbid_a in call_mbids
        assert mbid_b in call_mbids

    @pytest.mark.asyncio
    async def test_source_switch_separates_leaders_and_fences_cache_writes(self):
        import repositories.musicbrainz_base as mb_base

        cache = AsyncMock()
        cached_values: dict[str, DiscoverQueueEnrichment] = {}

        async def cache_get(key: str):
            return cached_values.get(key)

        async def cache_set(
            key: str, value: DiscoverQueueEnrichment, _ttl: int
        ) -> None:
            cached_values[key] = value

        cache.get.side_effect = cache_get
        cache.set.side_effect = cache_set
        service, mb_repo = _make_service(memory_cache=cache)
        service._enrichment._coalesce_popularity = AsyncMock(return_value=None)
        mb_repo.extract_youtube_url_from_relations = MagicMock(return_value=None)
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
            source_id="discover-enrich-old",
            generation=original_source.generation + 1,
        )
        old_generation = mb_base.get_mb_source_generation()

        async def delayed_release_group(*_args, **_kwargs):
            generation = mb_base.get_mb_source_generation()
            calls.append(generation)
            if generation == old_generation:
                old_started.set()
                await old_gate.wait()
                label = "old"
            else:
                new_started.set()
                await new_gate.wait()
                label = "new"
            return {"title": label, "tags": [{"name": label}]}

        mb_repo.get_release_group_by_id.side_effect = delayed_release_group
        try:
            old_leader = asyncio.create_task(service.enrich_queue_item(MBID))
            await old_started.wait()
            old_follower = asyncio.create_task(service.enrich_queue_item(MBID))
            await asyncio.sleep(0)

            mb_base.set_mb_api_base(
                "https://new.example/ws/2",
                source_mode="mirror",
                source_id="discover-enrich-new",
                generation=old_generation + 1,
            )
            new_generation = mb_base.get_mb_source_generation()
            new_leader = asyncio.create_task(service.enrich_queue_item(MBID))
            await new_started.wait()
            new_follower = asyncio.create_task(service.enrich_queue_item(MBID))
            await asyncio.sleep(0)

            assert len(service._enrichment._enrich_in_flight) == 2
            assert (MBID, old_generation) in service._enrichment._enrich_in_flight
            assert (MBID, new_generation) in service._enrichment._enrich_in_flight

            new_gate.set()
            new_leader_result, new_follower_result = await asyncio.gather(
                new_leader, new_follower
            )
            old_gate.set()
            old_leader_result, old_follower_result = await asyncio.gather(
                old_leader, old_follower
            )

            cached_result = await service.enrich_queue_item(MBID)
            assert cached_result.tags == ["new"]
            assert cached_values[
                "discover_queue_enrich:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
            ].tags == ["new"]
            assert cache.set.await_count == 1
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
        assert old_leader_result.tags == ["old"]
        assert old_follower_result.tags == ["old"]
        assert new_leader_result.tags == ["new"]
        assert new_follower_result.tags == ["new"]
        assert service._enrichment._enrich_in_flight == {}


@pytest.mark.asyncio
async def test_popularity_coalescer_batches_concurrent_listen_counts():
    service, _ = _make_service(memory_cache=None)
    lb_repo = service._enrichment._lb_repo
    lb_repo.get_release_group_popularity_batch = AsyncMock(
        return_value={"rg-a": 17, "rg-b": 4}
    )

    first = asyncio.create_task(service._enrichment._coalesce_popularity("rg-a"))
    second = asyncio.create_task(service._enrichment._coalesce_popularity("rg-b"))
    await asyncio.sleep(0)
    service._enrichment._flush_popularity_now()

    assert await asyncio.gather(first, second) == [17, 4]
    lb_repo.get_release_group_popularity_batch.assert_awaited_once_with(
        ["rg-a", "rg-b"]
    )
    flush_task = service._enrichment._popularity_flush_task
    if flush_task is not None:
        await flush_task
