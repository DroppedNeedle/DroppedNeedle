from __future__ import annotations

import asyncio
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import threading

import pytest

from infrastructure.persistence.auth_store import AuthStore

from services.native.library_filesystem_coordinator import (
    LibraryFilesystemCoordinator,
    copy_rooted,
    replace_rooted,
    unlink_rooted,
)


@pytest.fixture
def coordinator() -> LibraryFilesystemCoordinator:
    return LibraryFilesystemCoordinator()


@pytest.mark.asyncio
async def test_lease_waiters_do_not_starve_default_executor_or_auth_store(
    tmp_path: Path,
) -> None:
    loop = asyncio.get_running_loop()
    default_executor = ThreadPoolExecutor(
        max_workers=1, thread_name_prefix="default-lease-test"
    )
    original_default_executor = loop._default_executor  # type: ignore[attr-defined]
    loop.set_default_executor(default_executor)
    coordinator = LibraryFilesystemCoordinator()
    auth_store = AuthStore(tmp_path / "auth.db")
    owner_entered = asyncio.Event()
    release_owner = asyncio.Event()
    waiter_started = asyncio.Event()
    state = coordinator._state("root-a")
    original_acquire = state.acquire_registered_write_or_wait

    def acquire_waiter(waiter) -> bool:
        acquired = original_acquire(waiter)
        if not acquired:
            loop.call_soon_threadsafe(waiter_started.set)
        return acquired

    async def owner() -> None:
        async with coordinator.write("root-a"):
            owner_entered.set()
            await release_owner.wait()

    async def waiter_body() -> None:
        async with coordinator.write("root-a"):
            pass

    owner_task = asyncio.create_task(owner())
    waiter: asyncio.Task[None] | None = None
    marker: asyncio.Task[None] | None = None
    auth_query: asyncio.Task[bool] | None = None
    try:
        await owner_entered.wait()
        state.acquire_registered_write_or_wait = acquire_waiter  # type: ignore[method-assign]
        waiter = asyncio.create_task(waiter_body())
        await waiter_started.wait()
        marker = asyncio.create_task(asyncio.to_thread(lambda: "default-executor-ok"))
        auth_query = asyncio.create_task(auth_store.has_any_users())

        done, _pending = await asyncio.wait(
            {marker, auth_query}, timeout=0.5, return_when=asyncio.ALL_COMPLETED
        )
        assert marker in done
        assert auth_query in done
        assert marker.result() == "default-executor-ok"
        assert await auth_query is False
    finally:
        release_owner.set()
        await owner_task
        tasks = [task for task in (waiter, marker, auth_query) if task is not None]
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        try:
            loop.set_default_executor(original_default_executor)  # type: ignore[arg-type]
        except TypeError:
            loop._default_executor = original_default_executor  # type: ignore[attr-defined]
        default_executor.shutdown(wait=True)

    assert state.writer_active is False
    assert state.waiting_writers == 0


@pytest.mark.asyncio
async def test_writer_preference_does_not_deadlock_reader_before_writer(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    loop = asyncio.get_running_loop()
    owner_entered = asyncio.Event()
    release_owner = asyncio.Event()
    reader_waiting = asyncio.Event()
    writer_waiting = asyncio.Event()
    state = coordinator._state("root-a")
    original_read = state.acquire_read_or_wait
    original_write = state.acquire_registered_write_or_wait
    order: list[str] = []

    def track_reader(waiter):
        acquired = original_read(waiter)
        if not acquired:
            loop.call_soon_threadsafe(reader_waiting.set)
        return acquired

    def track_writer(waiter):
        acquired = original_write(waiter)
        if not acquired:
            loop.call_soon_threadsafe(writer_waiting.set)
        return acquired

    async def owner() -> None:
        async with coordinator.write("root-a"):
            owner_entered.set()
            await release_owner.wait()

    async def reader() -> None:
        async with coordinator.read("root-a"):
            order.append("reader")

    async def writer() -> None:
        async with coordinator.write("root-a"):
            order.append("writer")

    owner_task = asyncio.create_task(owner())
    reader_task: asyncio.Task[None] | None = None
    writer_task: asyncio.Task[None] | None = None
    try:
        await owner_entered.wait()
        state.acquire_read_or_wait = track_reader  # type: ignore[method-assign]
        state.acquire_registered_write_or_wait = track_writer  # type: ignore[method-assign]
        reader_task = asyncio.create_task(reader())
        await reader_waiting.wait()
        writer_task = asyncio.create_task(writer())
        await writer_waiting.wait()
        assert state.waiting_writers == 1
        release_owner.set()
        await owner_task
        assert reader_task is not None
        assert writer_task is not None
        await asyncio.wait_for(asyncio.gather(reader_task, writer_task), timeout=1)
    finally:
        release_owner.set()
        await owner_task
        tasks = [task for task in (reader_task, writer_task) if task is not None]
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        state.acquire_read_or_wait = original_read  # type: ignore[method-assign]
        state.acquire_registered_write_or_wait = original_write  # type: ignore[method-assign]

    assert order == ["writer", "reader"]
    assert state.readers == 0
    assert state.writer_active is False
    assert state.waiting_writers == 0


@pytest.mark.asyncio
async def test_async_waiter_wakes_from_a_different_event_loop(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    owner_entered = asyncio.Event()
    release_owner = asyncio.Event()
    other_waiting = threading.Event()
    other_entered = threading.Event()
    other_errors: list[BaseException] = []
    state = coordinator._state("root-a")
    original_read = state.acquire_read_or_wait

    def track_other_loop_waiter(waiter):
        acquired = original_read(waiter)
        if not acquired:
            other_waiting.set()
        return acquired

    async def owner() -> None:
        async with coordinator.write("root-a"):
            owner_entered.set()
            await release_owner.wait()

    async def other_reader() -> None:
        try:
            async with coordinator.read("root-a"):
                other_entered.set()
        except Exception as exc:  # noqa: BLE001 - report cross-loop failures below
            other_errors.append(exc)

    owner_task = asyncio.create_task(owner())
    thread = threading.Thread(
        target=lambda: asyncio.run(other_reader()),
        name="filesystem-coordinator-other-loop",
        daemon=True,
    )
    try:
        await owner_entered.wait()
        state.acquire_read_or_wait = track_other_loop_waiter  # type: ignore[method-assign]
        thread.start()
        assert await asyncio.to_thread(other_waiting.wait, 1)
        release_owner.set()
        await owner_task
        assert await asyncio.to_thread(other_entered.wait, 1)
        await asyncio.to_thread(thread.join, 1)
        assert thread.is_alive() is False
    finally:
        release_owner.set()
        await owner_task
        if thread.is_alive():
            await asyncio.to_thread(thread.join, 1)
        state.acquire_read_or_wait = original_read  # type: ignore[method-assign]

    assert other_errors == []


@pytest.mark.asyncio
async def test_read_many_releases_partial_reads_before_waiting_on_later_root(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    initial_entered = asyncio.Event()
    release_initial = asyncio.Event()
    reader_waiting = asyncio.Event()
    writer_started = asyncio.Event()
    state_a = coordinator._state("root-a")
    state_b = coordinator._state("root-b")
    original_b_read = state_b.acquire_read_or_wait
    original_a_write = state_a.acquire_registered_write_or_wait
    order: list[str] = []

    def track_b_read(waiter: asyncio.Future[None]) -> bool:
        acquired = original_b_read(waiter)
        if not acquired:
            reader_waiting.set()
        return acquired

    def track_a_write(waiter: asyncio.Future[None]) -> bool:
        acquired = original_a_write(waiter)
        writer_started.set()
        return acquired

    async def initial_writer() -> None:
        async with coordinator.write("root-b"):
            initial_entered.set()
            await release_initial.wait()

    async def reader() -> None:
        async with coordinator.read_many(["root-a", "root-b"]):
            order.append("reader")

    async def writer() -> None:
        async with coordinator.write_many(["root-a", "root-b"]):
            order.append("writer")

    initial_task = asyncio.create_task(initial_writer())
    reader_task: asyncio.Task[None] | None = None
    writer_task: asyncio.Task[None] | None = None
    try:
        await initial_entered.wait()
        state_b.acquire_read_or_wait = track_b_read  # type: ignore[method-assign]
        state_a.acquire_registered_write_or_wait = track_a_write  # type: ignore[method-assign]
        reader_task = asyncio.create_task(reader())
        await reader_waiting.wait()
        writer_task = asyncio.create_task(writer())
        await writer_started.wait()
        release_initial.set()
        await initial_task
        assert reader_task is not None
        assert writer_task is not None
        await asyncio.wait_for(asyncio.gather(reader_task, writer_task), timeout=1)
    finally:
        release_initial.set()
        await initial_task
        tasks = [task for task in (reader_task, writer_task) if task is not None]
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        state_b.acquire_read_or_wait = original_b_read  # type: ignore[method-assign]
        state_a.acquire_registered_write_or_wait = original_a_write  # type: ignore[method-assign]

    assert order == ["writer", "reader"]
    assert state_a.readers == 0
    assert state_a.writer_active is False
    assert state_a.waiting_writers == 0
    assert state_b.readers == 0
    assert state_b.writer_active is False
    assert state_b.waiting_writers == 0


@pytest.mark.asyncio
async def test_cancelled_read_many_releases_partial_reads_and_blocked_waiter(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    root_b_entered = asyncio.Event()
    release_root_b = asyncio.Event()
    waiter_registered = asyncio.Event()
    state_a = coordinator._state("root-a")
    state_b = coordinator._state("root-b")
    original_b_read = state_b.acquire_read_or_wait

    def track_b_read(waiter: asyncio.Future[None]) -> bool:
        acquired = original_b_read(waiter)
        if not acquired:
            waiter_registered.set()
        return acquired

    async def hold_root_b() -> None:
        async with coordinator.write("root-b"):
            root_b_entered.set()
            await release_root_b.wait()

    async def read_both() -> None:
        async with coordinator.read_many(["root-a", "root-b"]):
            pass

    active_writer = asyncio.create_task(hold_root_b())
    cancelled_reader: asyncio.Task[None] | None = None
    try:
        await root_b_entered.wait()
        state_b.acquire_read_or_wait = track_b_read  # type: ignore[method-assign]
        cancelled_reader = asyncio.create_task(read_both())
        await waiter_registered.wait()
        cancelled_reader.cancel()
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(cancelled_reader, timeout=1)
        assert state_a.readers == 0
        assert not state_b._async_waiters
        release_root_b.set()
        await active_writer
        async with asyncio.timeout(1):
            async with coordinator.write_many(["root-a", "root-b"]):
                pass
    finally:
        release_root_b.set()
        await active_writer
        if cancelled_reader is not None:
            await asyncio.gather(cancelled_reader, return_exceptions=True)
        state_b.acquire_read_or_wait = original_b_read  # type: ignore[method-assign]


@pytest.mark.asyncio
async def test_writer_waits_for_reader_and_later_reader_does_not_overtake(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    order: list[str] = []
    reader_entered = asyncio.Event()
    release_reader = asyncio.Event()

    async def first_reader() -> None:
        async with coordinator.read("root-a"):
            order.append("reader-1")
            reader_entered.set()
            await release_reader.wait()

    async def writer() -> None:
        async with coordinator.write("root-a"):
            order.append("writer")

    async def second_reader() -> None:
        async with coordinator.read("root-a"):
            order.append("reader-2")

    first = asyncio.create_task(first_reader())
    await reader_entered.wait()
    waiting_writer = asyncio.create_task(writer())
    await asyncio.sleep(0)
    waiting_reader = asyncio.create_task(second_reader())
    await asyncio.sleep(0)

    assert order == ["reader-1"]
    release_reader.set()
    await asyncio.gather(first, waiting_writer, waiting_reader)

    assert order == ["reader-1", "writer", "reader-2"]
    assert coordinator.revision("root-a") == 1


@pytest.mark.asyncio
async def test_multi_root_requests_are_sorted_and_cannot_deadlock(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    entered: list[str] = []

    async def lease(name: str, roots: list[str]) -> None:
        async with coordinator.write_many(roots):
            entered.append(name)
            await asyncio.sleep(0)

    await asyncio.wait_for(
        asyncio.gather(
            lease("forward", ["root-a", "root-b"]),
            lease("reverse", ["root-b", "root-a"]),
        ),
        timeout=2,
    )

    assert sorted(entered) == ["forward", "reverse"]
    assert coordinator.revision("root-a") == 2
    assert coordinator.revision("root-b") == 2


@pytest.mark.asyncio
async def test_different_roots_do_not_block_each_other(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    root_a_entered = asyncio.Event()
    root_b_entered = asyncio.Event()
    release = asyncio.Event()

    async def hold(root_id: str, entered: asyncio.Event) -> None:
        async with coordinator.write(root_id):
            entered.set()
            await release.wait()

    first = asyncio.create_task(hold("root-a", root_a_entered))
    second = asyncio.create_task(hold("root-b", root_b_entered))
    await asyncio.wait_for(
        asyncio.gather(root_a_entered.wait(), root_b_entered.wait()), timeout=1
    )
    release.set()
    await asyncio.gather(first, second)


@pytest.mark.asyncio
async def test_cancelled_waiter_does_not_leak_a_lease(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    reader_entered = asyncio.Event()
    release_reader = asyncio.Event()

    async def reader() -> None:
        async with coordinator.read("root-a"):
            reader_entered.set()
            await release_reader.wait()

    async def writer() -> None:
        async with coordinator.write("root-a"):
            pass

    active_reader = asyncio.create_task(reader())
    await reader_entered.wait()
    cancelled_writer = asyncio.create_task(writer())
    await asyncio.sleep(0)
    cancelled_writer.cancel()
    release_reader.set()
    with pytest.raises(asyncio.CancelledError):
        await cancelled_writer
    await active_reader

    async with asyncio.timeout(1):
        async with coordinator.write("root-a"):
            pass


@pytest.mark.asyncio
async def test_repeatedly_cancelled_writer_does_not_leak_a_lease(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    reader_entered = asyncio.Event()
    release_reader = asyncio.Event()

    async def reader() -> None:
        async with coordinator.read("root-a"):
            reader_entered.set()
            await release_reader.wait()

    async def writer() -> None:
        async with coordinator.write("root-a"):
            pass

    active_reader = asyncio.create_task(reader())
    await reader_entered.wait()
    cancelled_writer = asyncio.create_task(writer())
    await asyncio.sleep(0)
    cancelled_writer.cancel()
    await asyncio.sleep(0)
    cancelled_writer.cancel()
    release_reader.set()
    with pytest.raises(asyncio.CancelledError):
        await cancelled_writer
    await active_reader

    async with asyncio.timeout(1):
        async with coordinator.write("root-a"):
            pass


@pytest.mark.asyncio
async def test_repeatedly_cancelled_reader_does_not_leak_a_lease(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    writer_entered = asyncio.Event()
    release_writer = asyncio.Event()

    async def writer() -> None:
        async with coordinator.write("root-a"):
            writer_entered.set()
            await release_writer.wait()

    async def reader() -> None:
        async with coordinator.read("root-a"):
            pass

    active_writer = asyncio.create_task(writer())
    await writer_entered.wait()
    cancelled_reader = asyncio.create_task(reader())
    await asyncio.sleep(0)
    cancelled_reader.cancel()
    await asyncio.sleep(0)
    cancelled_reader.cancel()
    release_writer.set()
    with pytest.raises(asyncio.CancelledError):
        await cancelled_reader
    await active_writer

    async with asyncio.timeout(1):
        async with coordinator.write("root-a"):
            pass


@pytest.mark.asyncio
async def test_repeated_cancellation_releases_partially_acquired_roots(
    coordinator: LibraryFilesystemCoordinator,
) -> None:
    root_b_entered = asyncio.Event()
    release_root_b = asyncio.Event()

    async def hold_root_b() -> None:
        async with coordinator.write("root-b"):
            root_b_entered.set()
            await release_root_b.wait()

    async def hold_both() -> None:
        async with coordinator.write_many(["root-a", "root-b"]):
            pass

    active_root_b = asyncio.create_task(hold_root_b())
    await root_b_entered.wait()
    cancelled_writer = asyncio.create_task(hold_both())
    while not coordinator._state("root-a").writer_active:
        await asyncio.sleep(0)

    cancelled_writer.cancel()
    await asyncio.sleep(0)
    cancelled_writer.cancel()
    release_root_b.set()
    with pytest.raises(asyncio.CancelledError):
        await cancelled_writer
    await active_root_b

    async with asyncio.timeout(1):
        async with coordinator.write_many(["root-a", "root-b"]):
            pass


def test_rooted_mutations_copy_replace_and_unlink_regular_files(tmp_path: Path) -> None:
    root = tmp_path / "music"
    source_parent = root / "source"
    destination_parent = root / "destination"
    source_parent.mkdir(parents=True)
    destination_parent.mkdir()
    (source_parent / "track.flac").write_bytes(b"source")
    roots = {"music": root}

    copy_rooted(
        roots,
        "music",
        "source/track.flac",
        "music",
        "destination/copied.flac",
    )
    replace_rooted(
        roots,
        "music",
        "destination/copied.flac",
        "music",
        "destination/published.flac",
    )
    unlink_rooted(roots, "music", "destination/published.flac")
    unlink_rooted(
        roots,
        "music",
        "destination/published.flac",
        missing_ok=True,
    )

    assert (source_parent / "track.flac").read_bytes() == b"source"
    assert not (destination_parent / "published.flac").exists()


@pytest.mark.parametrize("mutation", ["copy", "replace", "unlink"])
def test_rooted_mutations_reject_swapped_parent_symlink(
    tmp_path: Path, mutation: str
) -> None:
    root = tmp_path / "music"
    outside = tmp_path / "outside"
    root.mkdir()
    outside.mkdir()
    (root / "source.flac").write_bytes(b"source")
    (outside / "victim.flac").write_bytes(b"outside")
    (root / "swapped").symlink_to(outside, target_is_directory=True)
    roots = {"music": root}

    with pytest.raises(OSError):
        if mutation == "copy":
            copy_rooted(
                roots,
                "music",
                "source.flac",
                "music",
                "swapped/copied.flac",
            )
        elif mutation == "replace":
            replace_rooted(
                roots,
                "music",
                "source.flac",
                "music",
                "swapped/victim.flac",
            )
        else:
            unlink_rooted(roots, "music", "swapped/victim.flac")

    assert (outside / "victim.flac").read_bytes() == b"outside"
    assert not (outside / "copied.flac").exists()
    assert (root / "source.flac").read_bytes() == b"source"
