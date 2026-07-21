import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest


TEST_MBID = "cc197bad-dc9c-440d-a5b5-d52ba2e14234"


def _make_settings(audiodb_enabled: bool = True):
    s = MagicMock()
    s.audiodb_enabled = audiodb_enabled
    return s


def _make_prefs(settings=None, cursor=None):
    if settings is None:
        settings = _make_settings()
    prefs = MagicMock()
    prefs.get_advanced_settings.return_value = settings
    prefs.get_setting = MagicMock(return_value=cursor)
    prefs.save_setting = MagicMock()
    return prefs


def _make_library_db(artists=None, albums=None):
    cache = AsyncMock()
    candidates = [
        ("artist", item["mbid"], item)
        for item in artists or []
    ] + [
        ("album", item["mbid"], item)
        for item in albums or []
    ]
    cache.get_enrichment_candidates = AsyncMock(return_value=candidates)
    return cache


class TestSweepSkipsWhenDisabled:
    @pytest.mark.asyncio
    async def test_disabled_skips(self):
        from core.tasks import warm_audiodb_cache_periodically

        settings = _make_settings(audiodb_enabled=False)
        prefs = _make_prefs(settings)
        svc = AsyncMock()
        cache = _make_library_db()

        task = asyncio.create_task(
            warm_audiodb_cache_periodically(svc, cache, prefs)
        )
        # Replace sleep to avoid 120s wait
        with patch('core.tasks.asyncio.sleep', new_callable=AsyncMock) as mock_sleep:
            mock_sleep.side_effect = [None, asyncio.CancelledError()]
            try:
                await task
            except asyncio.CancelledError:
                pass

        svc.fetch_and_cache_artist_images.assert_not_called()


class TestSweepSkipsEmptyLibrary:
    @pytest.mark.asyncio
    async def test_empty_library_skips(self):
        from core.tasks import warm_audiodb_cache_periodically

        prefs = _make_prefs()
        svc = AsyncMock()
        cache = _make_library_db(artists=[], albums=[])

        task = asyncio.create_task(
            warm_audiodb_cache_periodically(svc, cache, prefs)
        )
        with patch('core.tasks.asyncio.sleep', new_callable=AsyncMock) as mock_sleep:
            mock_sleep.side_effect = [None, asyncio.CancelledError()]
            try:
                await task
            except asyncio.CancelledError:
                pass

        svc.get_cached_artist_images.assert_not_called()


class TestSweepCursorPersistence:
    @pytest.mark.asyncio
    async def test_cursor_saved_on_completion(self):
        from core.tasks import warm_audiodb_cache_periodically

        prefs = _make_prefs()
        svc = AsyncMock()
        svc.get_cached_artist_images = AsyncMock(return_value=None)
        svc.fetch_and_cache_artist_images = AsyncMock(return_value=None)

        artists = [{"mbid": TEST_MBID, "name": "Coldplay"}]
        cache = _make_library_db(artists=artists)

        call_count = 0
        async def smart_sleep(duration):
            nonlocal call_count
            call_count += 1
            if call_count >= 4:
                raise asyncio.CancelledError()

        with patch('core.tasks.asyncio.sleep', side_effect=smart_sleep):
            try:
                await warm_audiodb_cache_periodically(svc, cache, prefs)
            except asyncio.CancelledError:
                pass

        save_calls = prefs.save_setting.call_args_list
        cursor_clears = [c for c in save_calls if c[0] == ('audiodb_sweep_cursor', None)]
        assert len(cursor_clears) >= 1
