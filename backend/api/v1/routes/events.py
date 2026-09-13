"""Single multiplexed SSE stream for all global live updates.

One connection per tab replaces the previous fan-out (following, now-playing,
cache-sync, and library-activity streams): event names are unchanged, so each
consumer keeps its existing handler and only the transport moves.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncGenerator, AsyncIterator, Callable
import logging

import msgspec
from fastapi import APIRouter, Depends
from fastapi.responses import StreamingResponse

from core.dependencies import (
    get_cache_status_service,
    get_now_playing_service,
    get_sse_publisher,
)
from infrastructure.msgspec_fastapi import MsgSpecRoute
from infrastructure.sse_publisher import SSEPublisher
from middleware import CurrentUserDep
from services.cache_status_service import CacheStatusService
from services.native.library_revision_poller import (
    ACTIVITY_CHANGED_EVENT,
    LIBRARY_REVISIONS_CHANNEL,
)
from services.now_playing_service import NowPlayingService

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/events", tags=["events"])

_SSE_HEADERS = {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "X-Accel-Buffering": "no",
}

CACHE_SYNC_EVENT = "cache.sync"
_MUX_RETRY_MS = 5000
_MUX_KEEPALIVE_SECONDS = 30.0
_MUX_QUEUE_MAXSIZE = 200


async def _forward_bus(
    tag: str,
    subscribe: Callable[[], AsyncGenerator[dict, None]],
    out: asyncio.Queue,
) -> None:
    iterator = subscribe()
    try:
        async for message in iterator:
            if not message["event"]:
                continue
            SSEPublisher.offer_newest(out, (tag, message))
    except Exception:  # noqa: BLE001 - a dead feed must be visible, not silent
        logger.exception("Mux forwarder for %s died", tag)
    finally:
        await iterator.aclose()


async def _forward_cache_queue(
    queue: asyncio.Queue, out: asyncio.Queue
) -> None:
    try:
        while True:
            SSEPublisher.offer_newest(out, ("cache", await queue.get()))
    except Exception:  # noqa: BLE001 - a dead feed must be visible, not silent
        logger.exception("Mux cache forwarder died")


def _format_frame(tag: str, message: object) -> str:
    if tag == "cache":
        return f"event: {CACHE_SYNC_EVENT}\ndata: {message}\n\n"
    assert isinstance(message, dict)
    event = message["event"]
    payload = msgspec.json.encode(message["data"]).decode("utf-8")
    if (
        event == ACTIVITY_CHANGED_EVENT
        and isinstance(message["data"], dict)
        and "id" in message["data"]
    ):
        return f"id: {message['data']['id']}\nevent: {event}\ndata: {payload}\n\n"
    return f"event: {event}\ndata: {payload}\n\n"


async def mux_events(
    user_id: str,
    publisher: SSEPublisher,
    now_playing: NowPlayingService,
    status_service: CacheStatusService,
) -> AsyncIterator[str]:
    """Merge the four global feeds into one SSE frame stream."""
    yield f"retry: {_MUX_RETRY_MS}\n\n"
    # Subscribe before snapshotting: the cache bus has no replay, so any
    # broadcast between the snapshot read and the subscribe would be lost.
    cache_queue = status_service.subscribe_sse()
    try:
        initial = msgspec.json.encode(status_service.progress_payload()).decode("utf-8")
        yield f"event: {CACHE_SYNC_EVENT}\ndata: {initial}\n\n"

        out: asyncio.Queue = asyncio.Queue(maxsize=_MUX_QUEUE_MAXSIZE)
        forwarders = [
            asyncio.create_task(
                _forward_bus("bus", lambda: publisher.subscribe(f"user:{user_id}"), out)
            ),
            asyncio.create_task(_forward_bus("bus", now_playing.subscribe, out)),
            asyncio.create_task(
                _forward_bus(
                    "bus", lambda: publisher.subscribe(LIBRARY_REVISIONS_CHANNEL), out
                )
            ),
            asyncio.create_task(_forward_cache_queue(cache_queue, out)),
        ]
        try:
            while True:
                try:
                    tag, message = await asyncio.wait_for(
                        out.get(), timeout=_MUX_KEEPALIVE_SECONDS
                    )
                except asyncio.TimeoutError:
                    yield ": keepalive\n\n"
                    continue
                yield _format_frame(tag, message)
        except asyncio.CancelledError:
            pass
        finally:
            for forwarder in forwarders:
                forwarder.cancel()
            await asyncio.gather(*forwarders, return_exceptions=True)
    finally:
        status_service.unsubscribe_sse(cache_queue)


@router.get("/stream")
async def events_stream(
    current_user: CurrentUserDep,
    publisher: SSEPublisher = Depends(get_sse_publisher),
    now_playing: NowPlayingService = Depends(get_now_playing_service),
    status_service: CacheStatusService = Depends(get_cache_status_service),
):
    return StreamingResponse(
        mux_events(current_user.id, publisher, now_playing, status_service),
        media_type="text/event-stream",
        headers=_SSE_HEADERS,
    )
