"""resolve_lastfm_release_group_mbids banks each release->RG hit the moment it lands, so a
build cancelled mid-drain (the norm under the MusicBrainz 1/s limit during the LB outage)
keeps every resolution it earned - the store warms and personalisation converges."""

import asyncio
import threading
from unittest.mock import AsyncMock, MagicMock

import pytest

import repositories.musicbrainz_base as mb_base
from services.discover.mbid_resolution_service import MbidResolutionService
from infrastructure.persistence import DiscoverySnapshotStore
from infrastructure.observability.optional_work import OptionalWorkDeferred


def _store():
    """Mock the canonical store's release_to_rg interface."""
    store = MagicMock()
    store.get_release_to_rg_batch = AsyncMock(return_value={})
    saved: list[dict] = []

    async def capture_save(mapping, source_host=None, *, source_context=None):
        if source_context is None or mb_base.is_mb_source_current(source_context):
            saved.append(dict(mapping))

    store.save_release_to_rg = AsyncMock(side_effect=capture_save)
    return store, saved


@pytest.mark.asyncio
async def test_each_hit_is_persisted_incrementally_not_batched():
    store, saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda mbid, **kwargs: f"rg-{mbid}"
    )
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)

    result = await svc.resolve_lastfm_release_group_mbids(["rel-1", "rel-2"])

    assert result == {"rel-1": "rg-rel-1", "rel-2": "rg-rel-2"}
    # persisted per-completion (single-entry writes), never as one post-gather batch
    assert {"rel-1": "rg-rel-1"} in saved
    assert {"rel-2": "rg-rel-2"} in saved


@pytest.mark.asyncio
async def test_completed_hit_banks_even_when_resolve_is_cancelled():
    store, saved = _store()
    block = asyncio.Event()

    async def resolve(mbid, **kwargs):
        if mbid == "rel-slow":
            await (
                block.wait()
            )  # never completes - stands in for a lookup still queued at 1/s
            return None
        return f"rg-{mbid}"

    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(side_effect=resolve)
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)

    task = asyncio.create_task(
        svc.resolve_lastfm_release_group_mbids(["rel-fast", "rel-slow"])
    )
    for _ in range(200):  # let rel-fast resolve + persist while rel-slow hangs
        if saved:
            break
        await asyncio.sleep(0)
    task.cancel()  # budget fires while rel-slow is still draining
    with pytest.raises(asyncio.CancelledError):
        await task

    assert any(s.get("rel-fast") == "rg-rel-fast" for s in saved)
    block.set()


@pytest.mark.asyncio
async def test_resolution_is_capped_even_when_caller_requests_unbounded_tail():
    store, _saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda m, **kwargs: f"rg-{m}"
    )
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)
    mbids = [f"rel-{i:02}" for i in range(25)]
    result = await svc.resolve_lastfm_release_group_mbids(mbids, max_lookups=1000)
    assert [m for m in mbids if result[m] == f"rg-{m}"] == mbids[:10]


@pytest.mark.asyncio
async def test_failed_head_rotates_durably_across_restart_and_input_changes(tmp_path):
    path = tmp_path / "progress.db"
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda m, **kwargs: None if m == "a" else f"rg-{m}"
    )
    mb.get_release_group_by_id = AsyncMock(return_value=None)

    def service():
        return MbidResolutionService(
            mb, MagicMock(), MagicMock(),
            progress_store=DiscoverySnapshotStore(path, threading.Lock()),
        )

    kwargs = dict(user_id="u1", work_key="picks", max_lookups=1, allow_passthrough=False)
    assert await service().resolve_lastfm_release_group_mbids(["a", "b", "c"], **kwargs) == {}
    assert await service().resolve_lastfm_release_group_mbids(["c", "b", "a"], **kwargs) == {"b": "rg-b"}
    assert await service().resolve_lastfm_release_group_mbids(["a", "b", "d"], **kwargs) == {}
    assert await service().resolve_lastfm_release_group_mbids(
        ["a", "b", "c"], **{**kwargs, "user_id": "u2"}
    ) == {}


@pytest.mark.asyncio
async def test_cancelled_head_does_not_starve_next_input(tmp_path):
    progress = DiscoverySnapshotStore(tmp_path / "progress.db", threading.Lock())
    started = asyncio.Event()
    blocker = asyncio.Event()

    async def resolve(mbid, **kwargs):
        if mbid == "a":
            started.set()
            await blocker.wait()
        return f"rg-{mbid}"

    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(side_effect=resolve)
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), progress_store=progress)
    task = asyncio.create_task(svc.resolve_release_mbids(["a", "b"], user_id="u1"))
    await started.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    result = await svc.resolve_lastfm_release_group_mbids(
        ["a", "b"], user_id="u1", max_lookups=1, allow_passthrough=False
    )
    assert result == {"b": "rg-b"}


@pytest.mark.asyncio
async def test_deferred_fallback_never_banks_absence_and_keeps_completed_hit(tmp_path):
    # release_b must be lookup-shaped: the RG fallback probe pre-filters
    # invalid ids, so only a valid MBID reaches get_release_group_by_id
    # and raises OptionalWorkDeferred. Leading 'b' preserves the sorted
    # input order ["a", release_b, "c"] the cursor assertions depend on.
    release_b = "b3e9c1a4-7f2d-4b6e-9d0a-1c5f8e2a4b7d"
    progress = DiscoverySnapshotStore(tmp_path / "progress.db", threading.Lock())
    canonical, saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda m, **kwargs: "rg-a" if m == "a" else None
    )
    mb.get_release_group_by_id = AsyncMock(side_effect=OptionalWorkDeferred())
    svc = MbidResolutionService(
        mb, MagicMock(), MagicMock(), progress_store=progress, mb_canonical_store=canonical
    )
    cache = {}
    with pytest.raises(OptionalWorkDeferred):
        await svc.resolve_lastfm_release_group_mbids(
            ["a", release_b, "c"], user_id="u1", resolver_cache=cache, allow_passthrough=False
        )
    assert cache == {"a": "rg-a"}
    assert saved == [{"a": "rg-a"}]
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda m, **kwargs: f"rg-{m}"
    )
    assert await svc.resolve_lastfm_release_group_mbids(
        ["a", release_b, "c"], user_id="u1", max_lookups=1,
        resolver_cache=cache, allow_passthrough=False
    ) == {"a": "rg-a", "c": "rg-c"}


@pytest.mark.asyncio
async def test_overlapping_passes_consume_distinct_cursor_positions(tmp_path):
    progress = DiscoverySnapshotStore(tmp_path / "progress.db", threading.Lock())
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda m, **kwargs: f"rg-{m}"
    )
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), progress_store=progress)
    results = await asyncio.gather(*(
        svc.resolve_lastfm_release_group_mbids(
            ["a", "b", "c"], user_id="u1", max_lookups=1, allow_passthrough=False
        ) for _ in range(2)
    ))
    assert results == [{"a": "rg-a"}, {"b": "rg-b"}]


@pytest.mark.asyncio
async def test_banked_success_remains_usable_when_optional_work_is_deferred(tmp_path):
    progress = DiscoverySnapshotStore(tmp_path / "progress.db", threading.Lock())
    canonical, saved = _store()
    canonical.get_release_to_rg_batch.side_effect = (
        lambda ids, **kwargs: {
            mbid: rg for batch in saved for mbid, rg in batch.items() if mbid in ids
        }
    )
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(return_value="rg-a")
    first = MbidResolutionService(
        mb, MagicMock(), MagicMock(), progress_store=progress, mb_canonical_store=canonical
    )
    assert await first.resolve_release_mbids(["a"], user_id="u1") == {"a": "rg-a"}
    mb.get_release_group_id_from_release.side_effect = OptionalWorkDeferred()
    restarted = MbidResolutionService(
        mb, MagicMock(), MagicMock(), progress_store=progress, mb_canonical_store=canonical
    )
    assert await restarted.resolve_release_mbids(["a"], user_id="u1") == {"a": "rg-a"}


@pytest.mark.asyncio
async def test_source_change_resets_resolution_cursor(tmp_path):
    progress = DiscoverySnapshotStore(tmp_path / "progress.db", threading.Lock())
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda m, **kwargs: f"rg-{m}"
    )
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), progress_store=progress)
    options = dict(user_id="u1", max_lookups=1, allow_passthrough=False)
    assert await svc.resolve_lastfm_release_group_mbids(["a", "b"], **options) == {"a": "rg-a"}
    original = mb_base.capture_mb_source_context()
    runtime = mb_base.brainzmash_runtime_enabled()
    try:
        mb_base.set_mb_api_base(
            "https://changed.example/ws/2", source_mode="mirror",
            source_id="changed-resolution", generation=original.generation + 1,
        )
        assert await svc.resolve_lastfm_release_group_mbids(["a", "b"], **options) == {"a": "rg-a"}
    finally:
        mb_base.set_mb_api_base(
            original.source_url, source_mode=original.source_mode,
            source_id=original.source_id, generation=original.generation,
            brainzmash_binding_valid=runtime,
        )


@pytest.mark.asyncio
async def test_resolver_skips_durable_write_from_stale_source():
    store, _saved = _store()
    mb = MagicMock()
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="old-resolution",
        generation=old_generation,
    )

    async def resolve(_mbid, **kwargs):
        mb_base._mb_response_context.set(
            mb_base.MbSourceContext(
                source_url=mb_base.get_mb_api_base(),
                generation=mb_base.get_mb_source_generation(),
                source_mode="mirror",
                source_id="old-resolution",
            )
        )
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="new-resolution",
            generation=old_generation + 1,
        )
        # Keep this task's response context alive until the resolver captures it.
        return "rg-stale"

    mb.get_release_group_id_from_release = AsyncMock(side_effect=resolve)
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)
    try:
        result = await svc.resolve_lastfm_release_group_mbids(["rel-stale"])
    finally:
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )


@pytest.mark.asyncio
async def test_resolver_discards_canonical_rows_after_source_switch():
    store, _saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(return_value="rg-wire")
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="old-canonical",
        generation=old_generation,
    )

    async def switch_during_read(_mbids, *, source_context=None):
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="new-canonical",
            generation=old_generation + 1,
        )
        return {"rel-canonical": "rg-old"}

    store.get_release_to_rg_batch = AsyncMock(side_effect=switch_during_read)
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)
    try:
        result = await svc.resolve_lastfm_release_group_mbids(["rel-canonical"])
    finally:
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )

    assert result == {}
    mb.get_release_group_id_from_release.assert_not_awaited()
    assert _saved == []


@pytest.mark.asyncio
async def test_delayed_leader_and_follower_without_response_context_skip_stale_write():
    store, _saved = _store()
    mb = MagicMock()
    started = asyncio.Event()
    release = asyncio.Event()
    calls = 0

    async def resolve(_mbid, **kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            started.set()
        else:
            await started.wait()
        await release.wait()
        return "rg-old"

    mb.get_release_group_id_from_release = AsyncMock(side_effect=resolve)
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)
    original_source = mb_base.capture_mb_source_context()
    original_source_id = mb_base.get_mb_source_id()
    original_runtime = mb_base.brainzmash_runtime_enabled()
    old_generation = original_source.generation + 1
    mb_base.set_mb_api_base(
        "https://old.example/ws/2",
        source_mode="mirror",
        source_id="old-follower",
        generation=old_generation,
    )
    try:
        leader = asyncio.create_task(
            svc.resolve_lastfm_release_group_mbids(["rel-shared"])
        )
        await started.wait()
        follower = asyncio.create_task(
            svc.resolve_lastfm_release_group_mbids(["rel-shared"])
        )
        for _ in range(100):
            if calls == 2:
                break
            await asyncio.sleep(0)
        mb_base.set_mb_api_base(
            "https://new.example/ws/2",
            source_mode="mirror",
            source_id="new-follower",
            generation=old_generation + 1,
        )
        release.set()
        leader_result, follower_result = await asyncio.gather(leader, follower)
    finally:
        release.set()
        mb_base.set_mb_api_base(
            original_source.source_url,
            source_mode=original_source.source_mode,
            source_id=original_source_id,
            generation=original_source.generation,
            brainzmash_binding_valid=original_runtime,
        )

    assert leader_result == {}
    assert follower_result == {}
    assert _saved == []


@pytest.mark.asyncio
async def test_invalid_input_skips_release_group_fallback_probe():
    store, _saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(return_value=None)
    mb.get_release_group_by_id = AsyncMock(return_value={"id": "rg-should-not-wire"})
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)

    result = await svc.resolve_lastfm_release_group_mbids(["not-a-mbid"])

    assert mb.get_release_group_id_from_release.await_count == 1
    mb.get_release_group_by_id.assert_not_called()
    assert result == {"not-a-mbid": "not-a-mbid"}


@pytest.mark.asyncio
async def test_valid_mbid_still_probes_release_group_fallback():
    store, _saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(return_value=None)
    mb.get_release_group_by_id = AsyncMock(
        return_value={"id": "6a221da8-ecdb-4bd3-a563-9a59a4303c6e"}
    )
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)

    result = await svc.resolve_lastfm_release_group_mbids(
        ["6A221DA8-ECDB-4BD3-A563-9A59A4303C6E"]
    )

    mb.get_release_group_by_id.assert_awaited_once()
    assert result == {
        "6a221da8-ecdb-4bd3-a563-9a59a4303c6e": "6a221da8-ecdb-4bd3-a563-9a59a4303c6e"
    }
