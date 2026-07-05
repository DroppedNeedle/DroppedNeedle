import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

from repositories.coverart_album import AlbumCoverFetcher
from repositories.coverart_artist import ArtistImageFetcher
from repositories.coverart_repository import CoverArtRepository

RG = "11111111-1111-1111-1111-111111111111"
RG2 = "44444444-4444-4444-4444-444444444444"
ARTIST = "33333333-3333-3333-3333-333333333333"


def _repo(tmp_path, http_client):
    repo = CoverArtRepository(http_client=http_client, cache=MagicMock(), cache_dir=tmp_path)
    repo._disk_cache.read = AsyncMock(return_value=None)
    repo._disk_cache.is_negative = AsyncMock(return_value=False)
    repo._disk_cache.write_negative = AsyncMock()
    return repo


async def _drain_background():
    for _ in range(6):
        await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_album_hot_path_defers_best_release_and_writes_no_negative(tmp_path):
    async with httpx.AsyncClient() as http_client:
        repo = _repo(tmp_path, http_client)
        repo._album_fetcher.fetch_release_group_cover = AsyncMock(return_value=None)

        result = await repo.get_release_group_cover(RG, "250", defer_best_release=True)

        assert result is None
        # Hot path ran the cheap sources only (best-release fallback disabled) ...
        first = repo._album_fetcher.fetch_release_group_cover.call_args_list[0]
        assert first.kwargs["include_best_release"] is False
        # ... and did NOT write a negative synchronously (the background resolve owns that,
        # or it would short-circuit its own fetch via is_negative).
        repo._disk_cache.write_negative.assert_not_awaited()

        await _drain_background()

        # The deferred resolve ran the FULL path (best-release enabled) and banked the negative.
        assert any(
            c.kwargs.get("include_best_release") is True
            for c in repo._album_fetcher.fetch_release_group_cover.call_args_list
        )
        repo._disk_cache.write_negative.assert_awaited()


@pytest.mark.asyncio
async def test_rg_warming_flag_set_while_resolve_in_flight_then_clears(tmp_path):
    async with httpx.AsyncClient() as http_client:
        repo = _repo(tmp_path, http_client)
        repo._album_fetcher.fetch_release_group_cover = AsyncMock(return_value=None)

        result = await repo.get_release_group_cover(RG, "250", defer_best_release=True)
        assert result is None
        # inflight entry is added synchronously before the background task runs.
        assert repo.is_rg_cover_warming(RG, "250") is True

        await _drain_background()
        # background resolve finished -> flag clears (next request gets placeholder, not 202).
        assert repo.is_rg_cover_warming(RG, "250") is False


@pytest.mark.asyncio
async def test_artist_warming_flag_set_while_resolve_in_flight_then_clears(tmp_path):
    async with httpx.AsyncClient() as http_client:
        repo = _repo(tmp_path, http_client)
        repo._artist_fetcher.fetch_artist_image = AsyncMock(return_value=None)

        result = await repo.get_artist_image(ARTIST, 250, defer_wikidata=True)
        assert result is None
        assert repo.is_artist_cover_warming(ARTIST, 250) is True

        await _drain_background()
        assert repo.is_artist_cover_warming(ARTIST, 250) is False


@pytest.mark.asyncio
async def test_album_default_path_runs_best_release_inline(tmp_path):
    """Compat / prewarm callers don't pass defer_best_release, so they keep the full inline
    fallback and write the negative themselves - no background hand-off."""
    async with httpx.AsyncClient() as http_client:
        repo = _repo(tmp_path, http_client)
        repo._album_fetcher.fetch_release_group_cover = AsyncMock(return_value=None)

        result = await repo.get_release_group_cover(RG2, "250")

        assert result is None
        calls = repo._album_fetcher.fetch_release_group_cover.call_args_list
        assert len(calls) == 1
        assert calls[0].kwargs["include_best_release"] is True
        repo._disk_cache.write_negative.assert_awaited_once()


@pytest.mark.asyncio
async def test_artist_hot_path_defers_wikidata_and_writes_no_negative(tmp_path):
    async with httpx.AsyncClient() as http_client:
        repo = _repo(tmp_path, http_client)
        repo._artist_fetcher.fetch_artist_image = AsyncMock(return_value=None)

        result = await repo.get_artist_image(ARTIST, 250, defer_wikidata=True)

        assert result is None
        first = repo._artist_fetcher.fetch_artist_image.call_args_list[0]
        assert first.kwargs["include_wikidata"] is False
        repo._disk_cache.write_negative.assert_not_awaited()

        await _drain_background()

        assert any(
            c.kwargs.get("include_wikidata") is True
            for c in repo._artist_fetcher.fetch_artist_image.call_args_list
        )
        repo._disk_cache.write_negative.assert_awaited()


@pytest.mark.asyncio
async def test_deferred_artist_request_does_not_block_on_inflight_full_resolve(tmp_path):
    """Regression: a deferred (fast) artist request must NOT coalesce onto an in-flight slow
    FULL Wikidata resolve via the deduplicator and block on it. Seen live as a 55s cover
    request - the dedupe key now encodes the defer mode so hot requests stay fast."""
    async with httpx.AsyncClient() as http_client:
        repo = _repo(tmp_path, http_client)
        release = asyncio.Event()

        async def fetch(artist_id, size, file_path, priority=None, is_disconnected=None, include_wikidata=True):
            if include_wikidata:
                await release.wait()  # the slow full resolve blocks here
                return None
            return None  # the fast deferred path: immediate miss

        repo._artist_fetcher.fetch_artist_image = AsyncMock(side_effect=fetch)

        # A slow FULL resolve is in flight (blocks on the event).
        full = asyncio.create_task(repo.get_artist_image(ARTIST, 250, defer_wikidata=False))
        await asyncio.sleep(0)  # let it register as the deduplicator leader

        # The deferred hot request must return promptly, not wait on the full resolve.
        result = await asyncio.wait_for(
            repo.get_artist_image(ARTIST, 250, defer_wikidata=True), timeout=1.0
        )
        assert result is None

        release.set()
        await full
        await _drain_background()


@pytest.mark.asyncio
async def test_deferred_album_request_does_not_block_on_inflight_full_resolve(tmp_path):
    async with httpx.AsyncClient() as http_client:
        repo = _repo(tmp_path, http_client)
        release = asyncio.Event()

        async def fetch(release_group_id, size, file_path, priority=None, is_disconnected=None, include_best_release=True):
            if include_best_release:
                await release.wait()
                return None
            return None

        repo._album_fetcher.fetch_release_group_cover = AsyncMock(side_effect=fetch)

        full = asyncio.create_task(repo.get_release_group_cover(RG, "250", defer_best_release=False))
        await asyncio.sleep(0)

        result = await asyncio.wait_for(
            repo.get_release_group_cover(RG, "250", defer_best_release=True), timeout=1.0
        )
        assert result is None

        release.set()
        await full
        await _drain_background()


@pytest.mark.asyncio
async def test_album_fetcher_skips_best_release_when_disabled():
    """Direct fetcher test: a CAA release-group front miss does NOT trigger the two-call
    best-release fallback when include_best_release is False."""
    fetcher = AlbumCoverFetcher(
        http_get_fn=AsyncMock(return_value=MagicMock(status_code=404)),
        write_cache_fn=AsyncMock(),
    )
    fetcher._fetch_from_audiodb = AsyncMock(return_value=None)
    fetcher._fetch_release_group_local_sources = AsyncMock(return_value=None)
    fetcher._get_cover_from_best_release = AsyncMock(
        return_value=(b"img", "image/jpeg", "cover-art-archive")
    )

    disabled = await fetcher.fetch_release_group_cover(
        RG, "250", Path("/tmp/c.bin"), include_best_release=False
    )
    assert disabled is None
    fetcher._get_cover_from_best_release.assert_not_awaited()

    enabled = await fetcher.fetch_release_group_cover(
        RG, "250", Path("/tmp/c.bin"), include_best_release=True
    )
    assert enabled is not None
    fetcher._get_cover_from_best_release.assert_awaited_once()


@pytest.mark.asyncio
async def test_artist_fetcher_skips_wikidata_when_disabled():
    fetcher = ArtistImageFetcher(
        http_get_fn=AsyncMock(),
        write_cache_fn=AsyncMock(),
        cache=MagicMock(),
    )
    fetcher._fetch_from_audiodb = AsyncMock(return_value=None)
    fetcher._fetch_local_sources = AsyncMock(return_value=(None, False))
    fetcher._fetch_from_wikidata = AsyncMock(
        return_value=(b"img", "image/jpeg", "wikidata")
    )

    disabled = await fetcher.fetch_artist_image(
        ARTIST, 250, Path("/tmp/a.bin"), include_wikidata=False
    )
    assert disabled is None
    fetcher._fetch_from_wikidata.assert_not_awaited()

    enabled = await fetcher.fetch_artist_image(
        ARTIST, 250, Path("/tmp/a.bin"), include_wikidata=True
    )
    assert enabled is not None
    fetcher._fetch_from_wikidata.assert_awaited_once()
