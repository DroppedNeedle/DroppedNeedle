import asyncio
import logging
from unittest.mock import AsyncMock, MagicMock

import pytest

from services.audiodb_browse_queue import AudioDBBrowseQueue, BrowseQueueItem


@pytest.fixture
def queue():
    return AudioDBBrowseQueue()


@pytest.fixture
def mock_audiodb_svc():
    svc = AsyncMock()
    svc.fetch_and_cache_artist_images = AsyncMock(return_value=None)
    svc.fetch_and_cache_album_images = AsyncMock(return_value=None)
    return svc


@pytest.fixture
def mock_prefs():
    prefs = MagicMock()
    settings = MagicMock()
    settings.audiodb_enabled = True
    prefs.get_advanced_settings.return_value = settings
    return prefs


class TestEnqueue:
    @pytest.mark.asyncio
    async def test_enqueue_adds_item(self, queue: AudioDBBrowseQueue):
        await queue.enqueue("artist", "abc-123", name="Coldplay")
        assert queue._queue.qsize() == 1
        assert queue.is_pending("artist", "abc-123") is True

    @pytest.mark.asyncio
    async def test_enqueue_dedup_same_mbid(self, queue: AudioDBBrowseQueue):
        await queue.enqueue("artist", "abc-123")
        await queue.enqueue("artist", "abc-123")
        assert queue._queue.qsize() == 1

    @pytest.mark.asyncio
    async def test_enqueue_different_mbids(self, queue: AudioDBBrowseQueue):
        await queue.enqueue("artist", "abc-123")
        await queue.enqueue("album", "def-456")
        assert queue._queue.qsize() == 2

    @pytest.mark.asyncio
    async def test_enqueue_full_queue_drops(self, queue: AudioDBBrowseQueue):
        queue._queue = asyncio.Queue(maxsize=2)
        await queue.enqueue("artist", "a")
        await queue.enqueue("artist", "b")
        await queue.enqueue("artist", "c")
        assert queue._queue.qsize() == 2

    @pytest.mark.asyncio
    async def test_dedup_expires(self, queue: AudioDBBrowseQueue):
        await queue.enqueue("artist", "abc-123")
        queue._recent["abc-123"] -= 4000
        await queue.enqueue("artist", "abc-123")
        assert queue._queue.qsize() == 2


class TestConsumer:
    @pytest.mark.asyncio
    async def test_consumer_processes_artist(self, queue, mock_audiodb_svc, mock_prefs):
        await queue.enqueue("artist", "abc-123", name="Coldplay")

        task = queue.start_consumer(mock_audiodb_svc, mock_prefs)
        await asyncio.sleep(0.05)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

        mock_audiodb_svc.fetch_and_cache_artist_images.assert_called_once_with(
            "abc-123", "Coldplay", is_monitored=False,
        )

    @pytest.mark.asyncio
    async def test_consumer_processes_album(self, queue, mock_audiodb_svc, mock_prefs):
        await queue.enqueue("album", "def-456", name="Parachutes", artist_name="Coldplay")

        task = queue.start_consumer(mock_audiodb_svc, mock_prefs)
        await asyncio.sleep(0.05)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

        mock_audiodb_svc.fetch_and_cache_album_images.assert_called_once_with(
            "def-456", artist_name="Coldplay", album_name="Parachutes", is_monitored=False,
        )
        assert queue.is_pending("album", "def-456") is False

    @pytest.mark.asyncio
    async def test_consumer_skips_when_disabled(self, queue, mock_audiodb_svc, mock_prefs):
        mock_prefs.get_advanced_settings.return_value.audiodb_enabled = False
        await queue.enqueue("artist", "abc-123", name="Coldplay")

        task = queue.start_consumer(mock_audiodb_svc, mock_prefs)
        await asyncio.sleep(0.05)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

        mock_audiodb_svc.fetch_and_cache_artist_images.assert_not_called()
        assert queue.is_pending("artist", "abc-123") is False

    @pytest.mark.asyncio
    async def test_consumer_handles_item_error(self, queue, mock_audiodb_svc, mock_prefs, caplog):
        mock_audiodb_svc.fetch_and_cache_artist_images.side_effect = RuntimeError("boom")
        await queue.enqueue("artist", "abc-123", name="Coldplay")

        caplog.set_level(logging.ERROR, logger="services.audiodb_browse_queue")
        task = queue.start_consumer(mock_audiodb_svc, mock_prefs)
        await asyncio.sleep(0.05)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

        assert queue._queue.qsize() == 0
        assert queue.is_pending("artist", "abc-123") is False
        assert any(
            record.levelno == logging.ERROR
            and "audiodb.browse_queue action=item_error" in record.message
            and "entity_type=artist" in record.message
            and "mbid=abc-123" in record.message
            for record in caplog.records
        )
