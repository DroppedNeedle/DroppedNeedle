"""Tests for SSEPublisher - pub/sub, snapshot, disconnect cleanup, ring-buffer."""

import asyncio

import pytest

from infrastructure.sse_publisher import SSEPublisher


async def _register(gen, publisher, channel):
    """Drive a subscriber generator to register its queue, returning the pending
    next() task (blocked on the first live delta)."""
    task = asyncio.ensure_future(gen.__anext__())
    while await publisher.subscriber_count(channel) == 0:
        await asyncio.sleep(0)
    return task


@pytest.mark.asyncio
async def test_subscriber_receives_published_event():
    pub = SSEPublisher()
    gen = pub.subscribe("scan")
    task = await _register(gen, pub, "scan")
    await pub.publish("scan", "status", {"x": 1})
    msg = await asyncio.wait_for(task, 1)
    assert msg == {"event": "status", "data": {"x": 1}}
    await gen.aclose()


@pytest.mark.asyncio
async def test_new_subscriber_gets_snapshot_first():
    pub = SSEPublisher()
    await pub.publish("scan", "status", {"phase": "done"})  # before anyone subscribes
    gen = pub.subscribe("scan")
    msg = await asyncio.wait_for(gen.__anext__(), 1)
    assert msg == {"event": "status", "data": {"phase": "done"}}
    await gen.aclose()


@pytest.mark.asyncio
async def test_multiple_subscribers_all_receive():
    pub = SSEPublisher()
    gen_a, gen_b = pub.subscribe("scan"), pub.subscribe("scan")
    task_a = await _register(gen_a, pub, "scan")
    task_b = await _register(gen_b, pub, "scan")
    await pub.publish("scan", "progress", {"n": 7})
    msg_a = await asyncio.wait_for(task_a, 1)
    msg_b = await asyncio.wait_for(task_b, 1)
    assert msg_a == msg_b == {"event": "progress", "data": {"n": 7}}
    await gen_a.aclose()
    await gen_b.aclose()


@pytest.mark.asyncio
async def test_disconnect_removes_subscriber():
    pub = SSEPublisher()
    gen = pub.subscribe("scan")
    task = await _register(gen, pub, "scan")
    assert await pub.subscriber_count("scan") == 1
    # Cancelling the streaming task throws into the generator's `await queue.get()`,
    # running its finally (unsubscribe), exactly what a client disconnect does.
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert await pub.subscriber_count("scan") == 0


@pytest.mark.asyncio
async def test_channels_are_isolated():
    pub = SSEPublisher()
    gen = pub.subscribe("scan")
    task = await _register(gen, pub, "scan")
    await pub.publish("download", "status", {"other": True})  # different channel
    await pub.publish("scan", "status", {"mine": True})
    msg = await asyncio.wait_for(task, 1)
    assert msg["data"] == {"mine": True}
    await gen.aclose()


def test_offer_drains_buffer_on_overflow_keeping_newest():
    queue: asyncio.Queue = asyncio.Queue(maxsize=2)
    for n in (1, 2, 3):  # third overflows a size-2 queue
        assert SSEPublisher.offer_newest(queue, {"event": "e", "data": {"n": n}}) is True
    assert queue.qsize() == 1
    assert queue.get_nowait()["data"]["n"] == 3  # buffer drained, newest retained


def test_offer_reports_dead_queue():
    class _DeadQueue(asyncio.Queue):
        def put_nowait(self, item):
            raise asyncio.QueueFull

    queue = _DeadQueue(maxsize=1)
    assert SSEPublisher.offer_newest(queue, {"event": "e", "data": {}}) is False


@pytest.mark.asyncio
async def test_keepalive_yielded_when_idle():
    from infrastructure.sse_publisher import KEEPALIVE

    pub = SSEPublisher()
    gen = pub.subscribe("scan", keepalive_interval=0.01)
    # No publish: the first live yield is the keepalive heartbeat after timeout.
    msg = await asyncio.wait_for(gen.__anext__(), 1)
    assert msg == KEEPALIVE
    await gen.aclose()


@pytest.mark.asyncio
async def test_dead_subscriber_evicted_on_publish():
    pub = SSEPublisher()
    gen = pub.subscribe("scan")
    task = await _register(gen, pub, "scan")
    assert await pub.subscriber_count("scan") == 1
    # Simulate a dead consumer: its queue refuses every write even after draining.
    [queue] = pub._subscribers["scan"]

    def _refuse(_item):
        raise asyncio.QueueFull

    queue.put_nowait = _refuse  # type: ignore[method-assign]
    queue.empty = lambda: True  # type: ignore[method-assign]
    await pub.publish("scan", "status", {"n": 1})
    assert await pub.subscriber_count("scan") == 0
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
