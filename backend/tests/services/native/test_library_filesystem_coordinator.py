from __future__ import annotations

import asyncio

import pytest

from services.native.library_filesystem_coordinator import (
    LibraryFilesystemCoordinator,
)


@pytest.mark.asyncio
async def test_writer_waits_for_reader_and_later_reader_does_not_overtake() -> None:
    coordinator = LibraryFilesystemCoordinator()
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
async def test_multi_root_requests_are_sorted_and_cannot_deadlock() -> None:
    coordinator = LibraryFilesystemCoordinator()
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
async def test_different_roots_do_not_block_each_other() -> None:
    coordinator = LibraryFilesystemCoordinator()
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
async def test_cancelled_waiter_does_not_leak_a_lease() -> None:
    coordinator = LibraryFilesystemCoordinator()
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
async def test_repeatedly_cancelled_writer_does_not_leak_a_lease() -> None:
    coordinator = LibraryFilesystemCoordinator()
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
async def test_repeatedly_cancelled_reader_does_not_leak_a_lease() -> None:
    coordinator = LibraryFilesystemCoordinator()
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
async def test_repeated_cancellation_releases_partially_acquired_roots() -> None:
    coordinator = LibraryFilesystemCoordinator()
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
