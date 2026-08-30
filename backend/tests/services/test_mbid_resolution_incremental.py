"""resolve_lastfm_release_group_mbids banks each release->RG hit the moment it lands, so a
build cancelled mid-drain (the norm under the MusicBrainz 1/s limit during the LB outage)
keeps every resolution it earned - the store warms and personalisation converges."""

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

import repositories.musicbrainz_base as mb_base
from services.discover.mbid_resolution_service import MbidResolutionService


def _store():
    """Mock the canonical store's release_to_rg interface."""
    store = MagicMock()
    store.get_release_to_rg_batch = AsyncMock(return_value={})
    saved: list[dict] = []

    async def capture_save(mapping, source_host):
        saved.append(dict(mapping))

    store.save_release_to_rg = AsyncMock(side_effect=capture_save)
    return store, saved


@pytest.mark.asyncio
async def test_each_hit_is_persisted_incrementally_not_batched():
    store, saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(
        side_effect=lambda mbid: f"rg-{mbid}"
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

    async def resolve(mbid):
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
async def test_thorough_mode_resolves_all_not_just_max_lookups():
    from services.discover.mbid_resolution_service import discover_build_thorough

    store, _saved = _store()
    mb = MagicMock()
    mb.get_release_group_id_from_release = AsyncMock(side_effect=lambda m: f"rg-{m}")
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)
    mbids = [f"rel-{i}" for i in range(25)]  # 25 > the default max_lookups of 10

    # on-visit (default): capped at max_lookups, the rest pass through unresolved
    r_capped = await svc.resolve_lastfm_release_group_mbids(list(mbids), max_lookups=10)
    assert sum(1 for m in mbids if r_capped[m] == f"rg-{m}") == 10

    # thorough (warmer): resolves ALL 25 so Top Picks fully personalises in one pass
    token = discover_build_thorough.set(True)
    try:
        r_full = await svc.resolve_lastfm_release_group_mbids(
            list(mbids), max_lookups=10
        )
    finally:
        discover_build_thorough.reset(token)
    assert sum(1 for m in mbids if r_full[m] == f"rg-{m}") == 25


@pytest.mark.asyncio
async def test_resolver_skips_durable_write_from_stale_source():
    store, _saved = _store()
    mb = MagicMock()
    original_source = mb_base.get_mb_api_base()
    mb_base.set_mb_api_base("https://old.example/ws/2")

    async def resolve(_mbid):
        mb_base._mb_response_context.set(
            mb_base.MbSourceContext(
                source_url=mb_base.get_mb_api_base(),
                generation=mb_base.get_mb_source_generation(),
            )
        )
        mb_base.set_mb_api_base("https://new.example/ws/2")
        # Keep this task's response context alive until the resolver captures it.
        return "rg-stale"

    mb.get_release_group_id_from_release = AsyncMock(side_effect=resolve)
    svc = MbidResolutionService(mb, MagicMock(), MagicMock(), mb_canonical_store=store)
    try:
        result = await svc.resolve_lastfm_release_group_mbids(["rel-stale"])
    finally:
        mb_base.set_mb_api_base(original_source)

    assert result == {"rel-stale": "rg-stale"}
    store.save_release_to_rg.assert_not_awaited()
