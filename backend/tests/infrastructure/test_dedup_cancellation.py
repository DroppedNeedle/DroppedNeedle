import asyncio
import pytest

from infrastructure.http.deduplication import RequestDeduplicator


@pytest.mark.anyio
async def test_follower_task_cancellation_propagates():
    """When a follower's own task is cancelled, CancelledError must propagate."""
    dedup = RequestDeduplicator()
    leader_started = asyncio.Event()
    leader_release = asyncio.Event()

    async def slow_coro():
        leader_started.set()
        await leader_release.wait()
        return "result"

    async def run_follower():
        await leader_started.wait()
        return await dedup.dedupe("key", slow_coro)

    leader_task = asyncio.create_task(dedup.dedupe("key", slow_coro))
    await asyncio.sleep(0)
    follower_task = asyncio.create_task(run_follower())
    await asyncio.sleep(0)

    follower_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await follower_task

    leader_release.set()
    result = await leader_task
    assert result == "result"


@pytest.mark.anyio
async def test_leader_exception_propagates_to_follower():
    """When the leader raises, the follower receives the same exception."""
    dedup = RequestDeduplicator()
    leader_started = asyncio.Event()

    async def failing_coro():
        leader_started.set()
        await asyncio.sleep(0)
        raise ValueError("boom")

    async def run_follower():
        await leader_started.wait()
        return await dedup.dedupe("key", failing_coro)

    leader_task = asyncio.create_task(dedup.dedupe("key", failing_coro))
    await asyncio.sleep(0)
    follower_task = asyncio.create_task(run_follower())

    with pytest.raises(ValueError, match="boom"):
        await leader_task

    with pytest.raises(ValueError, match="boom"):
        await follower_task


@pytest.mark.anyio
async def test_leader_cancellation_follower_retries_as_leader():
    """When leader is cancelled, follower retries as new leader (bounded retry)."""
    dedup = RequestDeduplicator()
    leader_started = asyncio.Event()
    call_count = 0

    async def coro():
        nonlocal call_count
        call_count += 1
        if call_count == 1:
            leader_started.set()
            await asyncio.sleep(60)
            return "never"
        return "retried"

    async def run_follower():
        await leader_started.wait()
        return await dedup.dedupe("key", coro)

    leader_task = asyncio.create_task(dedup.dedupe("key", coro))
    await asyncio.sleep(0)
    follower_task = asyncio.create_task(run_follower())
    await asyncio.sleep(0)

    leader_task.cancel()

    with pytest.raises(asyncio.CancelledError):
        await leader_task

    result = await follower_task
    assert result == "retried"
    assert call_count == 2


@pytest.mark.anyio
async def test_concurrent_followers_coalesce():
    """Multiple followers all receive the same result from one leader execution."""
    dedup = RequestDeduplicator()
    call_count = 0
    leader_started = asyncio.Event()
    release_leader = asyncio.Event()

    async def counted_coro():
        nonlocal call_count
        call_count += 1
        leader_started.set()
        await release_leader.wait()
        return "shared-result"

    async def run_follower():
        await leader_started.wait()
        return await dedup.dedupe("key", counted_coro)

    leader_task = asyncio.create_task(dedup.dedupe("key", counted_coro))
    await asyncio.sleep(0)

    followers = [asyncio.create_task(run_follower()) for _ in range(5)]
    # Let all followers register as waiters on the shared future
    for _ in range(10):
        await asyncio.sleep(0)

    release_leader.set()

    results = await asyncio.gather(leader_task, *followers)
    assert all(r == "shared-result" for r in results)
    assert call_count == 1


@pytest.mark.anyio
async def test_disconnect_leader_follower_retries_as_leader():
    """When leader disconnects, one follower retries as the new leader."""
    from core.exceptions import ClientDisconnectedError

    dedup = RequestDeduplicator()
    follower_registered = asyncio.Event()
    expected_result = ("image-bytes", "image/png", "source")
    leader_error = None

    async def leader_coro():
        await follower_registered.wait()
        raise ClientDisconnectedError("leader disconnected")

    async def run_leader():
        nonlocal leader_error
        try:
            await dedup.dedupe("key1", leader_coro)
        except ClientDisconnectedError as e:
            leader_error = e

    async def follower_coro():
        return expected_result

    async def run_follower():
        await asyncio.sleep(0)
        follower_registered.set()
        return await dedup.dedupe("key1", follower_coro)

    leader_task = asyncio.create_task(run_leader())
    await asyncio.sleep(0)
    follower_task = asyncio.create_task(run_follower())

    await asyncio.gather(leader_task, follower_task)

    assert isinstance(leader_error, ClientDisconnectedError)
    assert follower_task.result() == expected_result


@pytest.mark.anyio
async def test_key_cleanup_after_completion():
    """After dedupe completes, the key is removed from _pending."""
    dedup = RequestDeduplicator()

    async def simple_coro():
        return 42

    result = await dedup.dedupe("key", simple_coro)
    assert result == 42
    assert "key" not in dedup._pending


@pytest.mark.anyio
async def test_cancelled_leader_cancels_shared_future_not_set_exception():
    """A leader cancelled with no follower waiting must CANCEL the shared future rather than
    set a CancelledError on it - otherwise asyncio logs 'Future exception was never retrieved'
    at ERROR (routine when budgeted discover builds cancel in-flight MusicBrainz lookups)."""
    dedup = RequestDeduplicator()
    started = asyncio.Event()

    async def hang():
        started.set()
        await asyncio.Event().wait()  # never completes

    leader = asyncio.create_task(dedup.dedupe("k", hang))
    await started.wait()
    future = dedup._pending["k"]  # capture before the finally pops it

    leader.cancel()
    with pytest.raises(asyncio.CancelledError):
        await leader

    assert future.cancelled()  # cancelled, NOT holding a set exception
    assert "k" not in dedup._pending


@pytest.mark.anyio
async def test_clear_does_not_let_old_leader_remove_replacement():
    dedup = RequestDeduplicator()
    old_started = asyncio.Event()
    new_started = asyncio.Event()
    release_old = asyncio.Event()
    release_new = asyncio.Event()
    calls = 0

    async def request():
        nonlocal calls
        calls += 1
        if calls == 1:
            old_started.set()
            await release_old.wait()
            return "old"
        new_started.set()
        await release_new.wait()
        return "new"

    old = asyncio.create_task(dedup.dedupe("clear-key", request))
    await old_started.wait()
    dedup.clear()
    replacement = asyncio.create_task(dedup.dedupe("clear-key", request))
    await new_started.wait()

    release_old.set()
    assert await old == "old"
    assert dedup._pending["clear-key"] is not None

    release_new.set()
    assert await replacement == "new"
    assert "clear-key" not in dedup._pending
