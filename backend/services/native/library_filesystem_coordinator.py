"""Short per-root filesystem leases shared by scans and publishers."""

from __future__ import annotations

import asyncio
import threading
from collections.abc import AsyncIterator, Callable, Iterable, Iterator
from contextlib import asynccontextmanager, contextmanager
from pathlib import Path


class _RootLeaseState:
    def __init__(self) -> None:
        self.condition = threading.Condition()
        self.readers = 0
        self.writer_active = False
        self.waiting_writers = 0
        self.revision = 0

    def acquire_read(self) -> None:
        with self.condition:
            while self.writer_active or self.waiting_writers:
                self.condition.wait()
            self.readers += 1

    def release_read(self) -> None:
        with self.condition:
            self.readers -= 1
            if self.readers == 0:
                self.condition.notify_all()

    def register_write_waiter(self) -> None:
        with self.condition:
            self.waiting_writers += 1

    def acquire_registered_write(self) -> None:
        with self.condition:
            try:
                while self.writer_active or self.readers:
                    self.condition.wait()
                self.writer_active = True
            finally:
                self.waiting_writers -= 1

    def acquire_write(self) -> None:
        self.register_write_waiter()
        self.acquire_registered_write()

    def release_write(self) -> None:
        with self.condition:
            self.writer_active = False
            self.revision += 1
            self.condition.notify_all()

    def current_revision(self) -> int:
        with self.condition:
            return self.revision


class LibraryFilesystemCoordinator:
    """Writer-preferring read/write leases, isolated by stable library-root ID.

    The coordinator is deliberately in-process: production uses one worker. Durable
    publication and recovery state belongs in SQLite and the filesystem journal, not
    in this object.
    """

    def __init__(self) -> None:
        self._states: dict[str, _RootLeaseState] = {}
        self._states_lock = threading.Lock()
        self._scan_revisions: dict[tuple[str, str], int] = {}

    def _state(self, root_id: str) -> _RootLeaseState:
        if not root_id:
            raise ValueError("A filesystem lease requires a library root ID.")
        with self._states_lock:
            return self._states.setdefault(root_id, _RootLeaseState())

    def _ordered_states(
        self, root_ids: Iterable[str]
    ) -> list[tuple[str, _RootLeaseState]]:
        ordered = sorted(set(root_ids))
        if not ordered:
            raise ValueError("A filesystem lease requires at least one library root.")
        return [(root_id, self._state(root_id)) for root_id in ordered]

    @staticmethod
    async def _acquire_without_leaking_on_cancel(
        acquire: Callable[[], None], release: Callable[[], None]
    ) -> None:
        pending = asyncio.create_task(asyncio.to_thread(acquire))
        try:
            await asyncio.shield(pending)
        except asyncio.CancelledError:
            while not pending.done():
                try:
                    await asyncio.shield(pending)
                except asyncio.CancelledError:
                    continue
            pending.result()
            release()
            raise

    @asynccontextmanager
    async def read(self, root_id: str) -> AsyncIterator[None]:
        async with self.read_many([root_id]):
            yield

    @asynccontextmanager
    async def read_many(self, root_ids: Iterable[str]) -> AsyncIterator[None]:
        states = self._ordered_states(root_ids)
        acquired: list[_RootLeaseState] = []
        try:
            for _root_id, state in states:
                await self._acquire_without_leaking_on_cancel(
                    state.acquire_read, state.release_read
                )
                acquired.append(state)
            yield
        finally:
            for state in reversed(acquired):
                state.release_read()

    @asynccontextmanager
    async def write(self, root_id: str) -> AsyncIterator[None]:
        async with self.write_many([root_id]):
            yield

    @asynccontextmanager
    async def write_many(self, root_ids: Iterable[str]) -> AsyncIterator[None]:
        states = self._ordered_states(root_ids)
        acquired: list[_RootLeaseState] = []
        try:
            for _root_id, state in states:
                state.register_write_waiter()
                await self._acquire_without_leaking_on_cancel(
                    state.acquire_registered_write, state.release_write
                )
                acquired.append(state)
            yield
        finally:
            for state in reversed(acquired):
                state.release_write()

    @contextmanager
    def read_sync(self, root_id: str) -> Iterator[None]:
        state = self._state(root_id)
        state.acquire_read()
        try:
            yield
        finally:
            state.release_read()

    def revision(self, root_id: str) -> int:
        return self._state(root_id).current_revision()

    def record_scan_revision(self, run_id: str, root_id: str) -> None:
        revision = self.revision(root_id)
        with self._states_lock:
            self._scan_revisions[(run_id, root_id)] = revision

    def scan_revision(self, run_id: str, root_id: str) -> int:
        with self._states_lock:
            recorded = self._scan_revisions.get((run_id, root_id))
        return self.revision(root_id) if recorded is None else recorded

    def forget_scan(self, run_id: str) -> None:
        with self._states_lock:
            keys = [key for key in self._scan_revisions if key[0] == run_id]
            for key in keys:
                self._scan_revisions.pop(key, None)


MANAGEMENT_ARTIFACT_PREFIX = ".droppedneedle-management-"


def is_management_artifact(path: Path) -> bool:
    """Return whether a path uses the reserved hidden management namespace."""

    return any(part.startswith(MANAGEMENT_ARTIFACT_PREFIX) for part in path.parts)
