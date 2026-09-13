"""Central library-revision poller feeding the SSE event bus.

One process-wide 2s poll replaces the per-connection poll loop every library
activity/operations SSE stream used to run: N connections used to cost N x 4
DB reads per window; now every mux subscriber shares a single published feed
with snapshot-then-delta replay for late joiners.
"""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
import hashlib
import logging

import msgspec

from core.task_registry import TaskRegistry
from infrastructure.sse_publisher import SSEPublisher
from services.native.library_activity_events import (
    ACTIVITY_POLL_INTERVAL_SECONDS,
    ActivityRevisionSource,
)

logger = logging.getLogger(__name__)

LIBRARY_REVISION_POLLER_TASK_NAME = "library-revision-poller"
LIBRARY_REVISIONS_CHANNEL = "library-revisions"
ACTIVITY_CHANGED_EVENT = "activity.changed"


def _revision_event_id(revisions: dict[str, int]) -> str:
    revision_key = msgspec.json.encode(sorted(revisions.items()))
    return f"activity:{hashlib.sha256(revision_key).hexdigest()[:16]}"


async def poll_library_revisions_periodically(
    source_getter: Callable[[], ActivityRevisionSource],
    publisher_getter: Callable[[], SSEPublisher],
    *,
    interval: float = ACTIVITY_POLL_INTERVAL_SECONDS,
    sleep: Callable[[float], Awaitable[None]] = asyncio.sleep,
) -> None:
    """Poll once per process, publishing only when revisions change.

    Getters resolve every iteration so a settings-save singleton rebuild never
    strands the loop on a stale instance.
    """
    previous: dict[str, int] | None = None
    while True:
        try:
            revisions = await source_getter().stream_revisions()
            if revisions != previous:
                previous = revisions
                await publisher_getter().publish(
                    LIBRARY_REVISIONS_CHANNEL,
                    ACTIVITY_CHANGED_EVENT,
                    {
                        "id": _revision_event_id(revisions),
                        "revisions": revisions,
                    },
                )
            await sleep(interval)
        except asyncio.CancelledError:
            break
        except Exception:  # noqa: BLE001 - poll loop must survive transient DB errors
            logger.exception("Library revision poll failed")
            await sleep(interval)


def _log_poller_error(task: asyncio.Task[None]) -> None:
    if not task.cancelled() and task.exception():
        logger.error("Library revision poller died: %s", task.exception())


def start_library_revision_poller(
    source_getter: Callable[[], ActivityRevisionSource],
    publisher_getter: Callable[[], SSEPublisher],
) -> asyncio.Task[None]:
    task = asyncio.create_task(
        poll_library_revisions_periodically(source_getter, publisher_getter)
    )
    task.add_done_callback(_log_poller_error)
    TaskRegistry.get_instance().register(LIBRARY_REVISION_POLLER_TASK_NAME, task)
    return task
