"""Route + framing tests for the multiplexed global SSE stream."""

from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace

import pytest
from fastapi import FastAPI

import api.v1.routes.events as events_module
from api.v1.routes.events import mux_events, router
from core.dependencies import (
    get_cache_status_service,
    get_now_playing_service,
    get_sse_publisher,
)
from infrastructure.sse_publisher import SSEPublisher
from services.native.library_revision_poller import LIBRARY_REVISIONS_CHANNEL
from services.now_playing_service import CHANNEL as NOW_PLAYING_CHANNEL
from tests.helpers import build_test_client, override_user_auth


class _FakeCacheStatus:
    def __init__(self) -> None:
        self.queues: list[asyncio.Queue] = []
        self.unsubscribed: list[asyncio.Queue] = []

    def progress_payload(self) -> dict:
        return {"is_syncing": False, "phase": None}

    def subscribe_sse(self) -> asyncio.Queue:
        queue: asyncio.Queue = asyncio.Queue()
        self.queues.append(queue)
        return queue

    def unsubscribe_sse(self, queue: asyncio.Queue) -> None:
        self.unsubscribed.append(queue)


def _mux_deps(publisher: SSEPublisher, status=None):  # noqa: ANN001, ANN202
    now_playing = SimpleNamespace(
        subscribe=lambda: publisher.subscribe("now-playing")
    )
    return ("user-1", publisher, now_playing, status or _FakeCacheStatus())


def _client_with_mux_fakes(monkeypatch, publisher=None, status=None):  # noqa: ANN001, ANN202
    app = FastAPI()
    app.include_router(router)
    override_user_auth(app)
    app.dependency_overrides[get_sse_publisher] = lambda: publisher or SSEPublisher()
    app.dependency_overrides[get_now_playing_service] = lambda: SimpleNamespace()
    app.dependency_overrides[get_cache_status_service] = (
        lambda: status or _FakeCacheStatus()
    )
    return build_test_client(app)


def test_stream_rejects_unauthenticated() -> None:
    app = FastAPI()
    app.include_router(router)
    client = build_test_client(app)

    assert client.get("/events/stream").status_code == 401


def test_stream_admits_user_with_event_stream_headers(monkeypatch) -> None:  # noqa: ANN001
    async def one_shot(*args, **kwargs):  # noqa: ANN002, ANN003
        yield "retry: 5000\n\n"
        yield 'event: cache.sync\ndata: {"is_syncing": false}\n\n'

    monkeypatch.setattr(events_module, "mux_events", one_shot)
    client = _client_with_mux_fakes(monkeypatch)

    response = client.get("/events/stream")

    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/event-stream")
    assert "retry: 5000" in response.text
    assert "event: cache.sync" in response.text


@pytest.mark.asyncio
async def test_mux_first_frames_are_retry_and_cache_snapshot() -> None:
    gen = mux_events(*_mux_deps(SSEPublisher()))

    try:
        assert await asyncio.wait_for(anext(gen), timeout=1.0) == "retry: 5000\n\n"
        snapshot = await asyncio.wait_for(anext(gen), timeout=1.0)
    finally:
        await gen.aclose()

    assert snapshot.startswith("event: cache.sync\ndata: ")
    assert json.loads(snapshot.split("data: ", 1)[1])["is_syncing"] is False


@pytest.mark.asyncio
async def test_mux_forwards_named_bus_events() -> None:
    publisher = SSEPublisher()
    gen = mux_events(*_mux_deps(publisher))

    try:
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await publisher.publish(
            "user:user-1", "wanted_new_candidates", {"count": 2}
        )
        frame = await asyncio.wait_for(anext(gen), timeout=1.0)
    finally:
        await gen.aclose()

    assert frame.startswith("event: wanted_new_candidates\ndata: ")
    assert json.loads(frame.split("data: ", 1)[1]) == {"count": 2}


@pytest.mark.asyncio
async def test_mux_emits_id_line_for_activity_changed() -> None:
    publisher = SSEPublisher()
    gen = mux_events(*_mux_deps(publisher))

    try:
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await publisher.publish(
            LIBRARY_REVISIONS_CHANNEL,
            "activity.changed",
            {"id": "activity:abc123", "revisions": {"scan": 3}},
        )
        frame = await asyncio.wait_for(anext(gen), timeout=1.0)
    finally:
        await gen.aclose()

    assert frame.startswith("id: activity:abc123\nevent: activity.changed\ndata: ")
    assert json.loads(frame.split("data: ", 1)[1])["revisions"] == {"scan": 3}


@pytest.mark.asyncio
async def test_mux_forwards_cache_queue_messages_as_cache_sync() -> None:
    publisher = SSEPublisher()
    status = _FakeCacheStatus()
    gen = mux_events(*_mux_deps(publisher, status=status))

    try:
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await asyncio.wait_for(anext(gen), timeout=1.0)
        status.queues[0].put_nowait('{"is_syncing": true}')
        frame = await asyncio.wait_for(anext(gen), timeout=1.0)
    finally:
        await gen.aclose()

    assert frame == 'event: cache.sync\ndata: {"is_syncing": true}\n\n'


@pytest.mark.asyncio
async def test_mux_skips_bus_keepalives() -> None:
    async def scripted():
        yield {"event": "", "data": None}
        yield {"event": "snapshot", "data": {"sessions": []}}
        await asyncio.sleep(30)

    out: asyncio.Queue = asyncio.Queue()
    task = asyncio.create_task(events_module._forward_bus("bus", scripted, out))
    try:
        tag, message = await asyncio.wait_for(out.get(), timeout=1.0)
    finally:
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

    assert (tag, message) == ("bus", {"event": "snapshot", "data": {"sessions": []}})
    assert out.empty()


@pytest.mark.asyncio
async def test_mux_emits_keepalive_when_idle(monkeypatch) -> None:  # noqa: ANN001
    monkeypatch.setattr(events_module, "_MUX_KEEPALIVE_SECONDS", 0.05)
    gen = mux_events(*_mux_deps(SSEPublisher()))

    try:
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await asyncio.wait_for(anext(gen), timeout=1.0)
        frame = await asyncio.wait_for(anext(gen), timeout=1.0)
    finally:
        await gen.aclose()

    assert frame == ": keepalive\n\n"


@pytest.mark.asyncio
async def test_mux_forwards_now_playing_snapshots() -> None:
    publisher = SSEPublisher()
    gen = mux_events(*_mux_deps(publisher))

    try:
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await publisher.publish(
            NOW_PLAYING_CHANNEL, "snapshot", {"sessions": [{"id": "s1"}]}
        )
        frame = await asyncio.wait_for(anext(gen), timeout=1.0)
    finally:
        await gen.aclose()

    assert frame.startswith("event: snapshot\ndata: ")
    assert json.loads(frame.split("data: ", 1)[1]) == {"sessions": [{"id": "s1"}]}


@pytest.mark.asyncio
async def test_mux_does_not_leak_other_users_events() -> None:
    publisher = SSEPublisher()
    gen = mux_events(*_mux_deps(publisher))

    try:
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await asyncio.wait_for(anext(gen), timeout=1.0)
        await publisher.publish("user:user-2", "concerts_new", {"secret": True})
        await publisher.publish("user:user-1", "concerts_new", {"mine": True})
        frame = await asyncio.wait_for(anext(gen), timeout=1.0)
    finally:
        await gen.aclose()

    assert json.loads(frame.split("data: ", 1)[1]) == {"mine": True}


@pytest.mark.asyncio
async def test_mux_logs_and_survives_dead_bus_forwarder(
    caplog: pytest.LogCaptureFixture,
) -> None:
    async def dying():
        yield {"event": "snapshot", "data": {"sessions": []}}
        raise RuntimeError("boom")

    out: asyncio.Queue = asyncio.Queue()
    with caplog.at_level("ERROR", logger="api.v1.routes.events"):
        await events_module._forward_bus("bus", dying, out)

    assert any("Mux forwarder for bus died" in record.message for record in caplog.records)
    assert await asyncio.wait_for(out.get(), timeout=1.0) == (
        "bus",
        {"event": "snapshot", "data": {"sessions": []}},
    )


@pytest.mark.asyncio
async def test_mux_logs_dead_cache_forwarder(
    caplog: pytest.LogCaptureFixture,
) -> None:
    class _BoomQueue(asyncio.Queue):
        async def get(self):  # noqa: ANN201
            raise RuntimeError("boom")

    out: asyncio.Queue = asyncio.Queue()
    with caplog.at_level("ERROR", logger="api.v1.routes.events"):
        await events_module._forward_cache_queue(_BoomQueue(), out)

    assert any("Mux cache forwarder died" in record.message for record in caplog.records)


@pytest.mark.asyncio
async def test_mux_disconnect_cancels_forwarders_and_unsubscribes() -> None:
    publisher = SSEPublisher()
    status = _FakeCacheStatus()
    gen = mux_events(*_mux_deps(publisher, status=status))

    await asyncio.wait_for(anext(gen), timeout=1.0)
    await asyncio.wait_for(anext(gen), timeout=1.0)
    await publisher.publish("user:user-1", "concerts_new", {})
    await asyncio.wait_for(anext(gen), timeout=1.0)
    await gen.aclose()

    assert status.unsubscribed == [status.queues[0]]
    assert await publisher.subscriber_count("user:user-1") == 0
    assert await publisher.subscriber_count("now-playing") == 0
    assert await publisher.subscriber_count(LIBRARY_REVISIONS_CHANNEL) == 0
