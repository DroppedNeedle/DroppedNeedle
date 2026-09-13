"""Tests for the central library-revision poller (single process-wide poll)."""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
import re

import pytest

from core.task_registry import TaskRegistry
from infrastructure.sse_publisher import SSEPublisher
from services.native.library_revision_poller import (
    ACTIVITY_CHANGED_EVENT,
    LIBRARY_REVISION_POLLER_TASK_NAME,
    LIBRARY_REVISIONS_CHANNEL,
    poll_library_revisions_periodically,
    start_library_revision_poller,
)


class _ScriptedSource:
    """ActivityRevisionSource double replaying one payload per poll."""

    def __init__(self, script: list[dict[str, int] | BaseException]) -> None:
        self._script = list(script)
        self.polls = 0

    async def stream_revisions(self) -> dict[str, int]:
        self.polls += 1
        outcome = self._script[min(self.polls - 1, len(self._script) - 1)]
        if isinstance(outcome, BaseException):
            raise outcome
        return dict(outcome)


def _cancel_after(calls: int) -> Callable[[float], Awaitable[None]]:
    seen: list[float] = []

    async def sleep(delay: float) -> None:
        seen.append(delay)
        if len(seen) >= calls:
            raise asyncio.CancelledError

    sleep.seen = seen  # type: ignore[attr-defined]
    return sleep


@pytest.mark.asyncio
async def test_poller_publishes_initial_revisions() -> None:
    source = _ScriptedSource([{"scan": 1, "catalog": 2}])
    publisher = SSEPublisher()
    sleep = _cancel_after(1)

    await poll_library_revisions_periodically(
        lambda: source, lambda: publisher, interval=2.0, sleep=sleep
    )

    assert source.polls == 1
    assert sleep.seen == [2.0]
    subscriber = publisher.subscribe(LIBRARY_REVISIONS_CHANNEL)
    try:
        first = await asyncio.wait_for(anext(subscriber), timeout=1.0)
    finally:
        await subscriber.aclose()
    assert first["event"] == ACTIVITY_CHANGED_EVENT
    assert first["data"]["revisions"] == {"scan": 1, "catalog": 2}
    assert first["data"]["id"].startswith("activity:")


@pytest.mark.asyncio
async def test_poller_skips_publish_when_revisions_unchanged() -> None:
    source = _ScriptedSource([{"scan": 1}, {"scan": 1}, {"scan": 1}])
    publisher = SSEPublisher()
    published = 0
    real_publish = publisher.publish

    async def counting_publish(*args, **kwargs) -> None:  # noqa: ANN001, ANN002, ANN003
        nonlocal published
        published += 1
        await real_publish(*args, **kwargs)

    publisher.publish = counting_publish  # type: ignore[method-assign]
    await poll_library_revisions_periodically(
        lambda: source, lambda: publisher, sleep=_cancel_after(3)
    )

    assert source.polls == 3
    assert published == 1


@pytest.mark.asyncio
async def test_poller_republishes_on_change_with_new_event_id() -> None:
    source = _ScriptedSource([{"scan": 1}, {"scan": 2}])
    publisher = SSEPublisher()
    published: list[dict] = []
    real_publish = publisher.publish

    async def recording_publish(channel: str, event: str, data: dict) -> None:
        published.append({"channel": channel, "event": event, "data": data})
        await real_publish(channel, event, data)

    publisher.publish = recording_publish  # type: ignore[method-assign]
    await poll_library_revisions_periodically(
        lambda: source, lambda: publisher, sleep=_cancel_after(2)
    )

    assert [p["data"]["revisions"] for p in published] == [{"scan": 1}, {"scan": 2}]
    first_id, second_id = (p["data"]["id"] for p in published)
    assert first_id != second_id
    assert re.fullmatch(r"activity:[0-9a-f]{16}", first_id)
    assert re.fullmatch(r"activity:[0-9a-f]{16}", second_id)


@pytest.mark.asyncio
async def test_poller_survives_transient_source_errors(
    caplog: pytest.LogCaptureFixture,
) -> None:
    source = _ScriptedSource([RuntimeError("db locked"), {"scan": 4}])
    publisher = SSEPublisher()
    sleep = _cancel_after(2)

    with caplog.at_level("ERROR", logger="services.native.library_revision_poller"):
        await poll_library_revisions_periodically(
            lambda: source, lambda: publisher, sleep=sleep
        )

    assert source.polls == 2
    assert len(sleep.seen) == 2
    assert any("revision poll failed" in record.message for record in caplog.records)
    subscriber = publisher.subscribe(LIBRARY_REVISIONS_CHANNEL)
    try:
        first = await asyncio.wait_for(anext(subscriber), timeout=1.0)
    finally:
        await subscriber.aclose()
    assert first["data"]["revisions"] == {"scan": 4}


@pytest.mark.asyncio
async def test_poller_fans_out_to_every_subscriber() -> None:
    source = _ScriptedSource([{"scan": 9}])
    publisher = SSEPublisher()
    first_subscriber = publisher.subscribe(LIBRARY_REVISIONS_CHANNEL)
    second_subscriber = publisher.subscribe(LIBRARY_REVISIONS_CHANNEL)
    try:
        await poll_library_revisions_periodically(
            lambda: source, lambda: publisher, sleep=_cancel_after(1)
        )
        first = await asyncio.wait_for(anext(first_subscriber), timeout=1.0)
        second = await asyncio.wait_for(anext(second_subscriber), timeout=1.0)
    finally:
        await first_subscriber.aclose()
        await second_subscriber.aclose()
    assert first["data"]["revisions"] == {"scan": 9}
    assert second["data"]["revisions"] == {"scan": 9}
    assert source.polls == 1


@pytest.mark.asyncio
async def test_starter_registers_task_and_cancels_cleanly() -> None:
    source = _ScriptedSource([{"scan": 1}])
    publisher = SSEPublisher()
    registry = TaskRegistry.get_instance()
    try:
        task = start_library_revision_poller(lambda: source, lambda: publisher)
        assert registry.is_running(LIBRARY_REVISION_POLLER_TASK_NAME)
        assert not task.done()
    finally:
        await registry.cancel(LIBRARY_REVISION_POLLER_TASK_NAME)
    assert not registry.is_running(LIBRARY_REVISION_POLLER_TASK_NAME)
