"""Short per-root filesystem leases shared by scans and publishers."""

from __future__ import annotations

import asyncio
import ctypes
import errno
import threading
from collections.abc import AsyncIterator, Callable, Iterable, Iterator
from contextlib import asynccontextmanager, contextmanager
import os
from pathlib import Path
from pathlib import PurePosixPath
import stat

from core.exceptions import LibraryManagementDestinationConflictError

_RENAME_NOREPLACE = 1 << 0
# Kernels/filesystems without renameat2 support report these errnos; the
# publication then falls back to the previous recheck-then-replace behavior.
_NOREPLACE_UNSUPPORTED_ERRNOS = frozenset(
    {errno.ENOSYS, errno.EINVAL, errno.EOPNOTSUPP, errno.ENOTTY}
)
_LIBC = ctypes.CDLL(None, use_errno=True)


def _wake_async_future(future: asyncio.Future[None]) -> None:
    if not future.done():
        future.set_result(None)


class _RootLeaseState:
    def __init__(self) -> None:
        self.condition = threading.Condition()
        self.readers = 0
        self.writer_active = False
        self.waiting_writers = 0
        self.revision = 0
        self._async_waiters: set[asyncio.Future[None]] = set()

    def _take_async_waiters_locked(self) -> set[asyncio.Future[None]]:
        waiters = self._async_waiters
        self._async_waiters = set()
        return waiters

    @staticmethod
    def _notify_async_waiters(
        waiters: set[asyncio.Future[None]],
    ) -> None:
        for waiter in waiters:
            loop = waiter.get_loop()
            if loop.is_closed():
                continue
            try:
                loop.call_soon_threadsafe(_wake_async_future, waiter)
            except RuntimeError:
                continue

    def acquire_read_or_wait(self, waiter: asyncio.Future[None]) -> bool:
        with self.condition:
            if self.writer_active or self.waiting_writers:
                self._async_waiters.add(waiter)
                return False
            self.readers += 1
            return True

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
                waiters = self._take_async_waiters_locked()
            else:
                waiters = set()
        self._notify_async_waiters(waiters)

    def register_write_waiter(self) -> None:
        with self.condition:
            self.waiting_writers += 1

    def unregister_write_waiter(self) -> None:
        with self.condition:
            self.waiting_writers -= 1
            if self.waiting_writers == 0:
                # a departing pending writer may unblock parked readers
                self.condition.notify_all()
                waiters = self._take_async_waiters_locked()
            else:
                waiters = set()
        self._notify_async_waiters(waiters)

    def acquire_registered_write_or_wait(self, waiter: asyncio.Future[None]) -> bool:
        with self.condition:
            if self.writer_active or self.readers:
                self._async_waiters.add(waiter)
                return False
            self.waiting_writers -= 1
            self.writer_active = True
            return True

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

    def cancel_async_waiter(self, waiter: asyncio.Future[None]) -> None:
        with self.condition:
            self._async_waiters.discard(waiter)

    def release_write(self) -> None:
        with self.condition:
            self.writer_active = False
            self.revision += 1
            self.condition.notify_all()
            waiters = self._take_async_waiters_locked()
        self._notify_async_waiters(waiters)

    def current_revision(self) -> int:
        with self.condition:
            return self.revision


class LibraryFilesystemCoordinator:
    """Writer-preferring read/write leases, isolated by stable library-root ID.

    Async waiters park on event-loop futures while synchronous ``read_sync``
    callers continue to use the blocking condition directly.
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

    async def _acquire_without_leaking_on_cancel(
        self,
        state: _RootLeaseState,
        acquire_or_wait: Callable[[asyncio.Future[None]], bool],
    ) -> None:
        loop = asyncio.get_running_loop()
        while True:
            waiter = loop.create_future()
            if acquire_or_wait(waiter):
                return
            try:
                await waiter
            except asyncio.CancelledError:
                state.cancel_async_waiter(waiter)
                raise

    @asynccontextmanager
    async def read(self, root_id: str) -> AsyncIterator[None]:
        async with self.read_many([root_id]):
            yield

    @asynccontextmanager
    async def read_many(self, root_ids: Iterable[str]) -> AsyncIterator[None]:
        states = self._ordered_states(root_ids)
        loop = asyncio.get_running_loop()
        while True:
            acquired: list[_RootLeaseState] = []
            blocked_state: _RootLeaseState | None = None
            blocked_waiter: asyncio.Future[None] | None = None
            try:
                for _root_id, state in states:
                    waiter = loop.create_future()
                    if state.acquire_read_or_wait(waiter):
                        acquired.append(state)
                        continue
                    blocked_state = state
                    blocked_waiter = waiter
                    break

                if blocked_waiter is None:
                    yield
                    return

                # Register the blocked root before releasing any partial reads.
                # The full root set is retried after this waiter wakes.
                for state in reversed(acquired):
                    state.release_read()
                acquired.clear()
                try:
                    await blocked_waiter
                except asyncio.CancelledError:
                    assert blocked_state is not None
                    blocked_state.cancel_async_waiter(blocked_waiter)
                    raise
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
            # F-150: register every requested root as writer-pending BEFORE the
            # acquisition loop, so a reader for a later root cannot overtake a
            # writer still queued on an earlier one.
            for _root_id, state in states:
                state.register_write_waiter()
            for _root_id, state in states:
                await self._acquire_without_leaking_on_cancel(
                    state, state.acquire_registered_write_or_wait
                )
                acquired.append(state)
            yield
        finally:
            for state in reversed(acquired):
                state.release_write()
            # acquire_registered_write_or_wait consumes its own registration,
            # so only the registered-but-not-acquired remainder needs unwinding.
            for _root_id, lease in states[len(acquired) :]:
                lease.unregister_write_waiter()

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


@contextmanager
def _rooted_parent(root: Path, relative_path: str) -> Iterator[tuple[int, str]]:
    relative = PurePosixPath(relative_path)
    if (
        relative.is_absolute()
        or not relative.parts
        or any(part in {"", ".", ".."} for part in relative.parts)
    ):
        raise ValueError("A rooted filesystem path must be a safe relative path.")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
    descriptor = os.open(root, flags)
    try:
        for component in relative.parts[:-1]:
            child = os.open(component, flags | os.O_NOFOLLOW, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        yield descriptor, relative.parts[-1]
    finally:
        os.close(descriptor)


def _renameat2_noreplace(
    old_dir_fd: int, old_name: str, new_dir_fd: int, new_name: str
) -> None:
    """renameat2(RENAME_NOREPLACE): fail with EEXIST instead of overwriting."""

    result = _LIBC.renameat2(
        ctypes.c_int(old_dir_fd),
        ctypes.c_char_p(os.fsencode(old_name)),
        ctypes.c_int(new_dir_fd),
        ctypes.c_char_p(os.fsencode(new_name)),
        ctypes.c_uint(_RENAME_NOREPLACE),
    )
    if result != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number), os.fspath(new_name))


def replace_rooted_publication(
    roots: dict[str, Path],
    source_root_id: str,
    source_relative_path: str,
    destination_root_id: str,
    destination_relative_path: str,
) -> None:
    """Publish one staged temp onto its destination with a NOREPLACE backstop.

    F-112: the recheck-then-replace window must not silently overwrite an
    out-of-model external writer's file. Unsupported platforms/filesystems
    fall back to plain os.replace (previous behavior); an existing destination
    becomes LibraryManagementDestinationConflictError.
    """

    try:
        source_root = roots[source_root_id]
        destination_root = roots[destination_root_id]
    except KeyError as error:
        raise ValueError("A rooted replacement references an unknown root.") from error
    with _rooted_parent(source_root, source_relative_path) as source:
        with _rooted_parent(destination_root, destination_relative_path) as destination:
            try:
                _renameat2_noreplace(
                    source[0], source[1], destination[0], destination[1]
                )
            except OSError as error:
                if error.errno == errno.EEXIST:
                    raise LibraryManagementDestinationConflictError(
                        "A management destination was created after preview."
                    ) from error
                if error.errno not in _NOREPLACE_UNSUPPORTED_ERRNOS:
                    raise
                # renameat2 unsupported here: previous recheck-then-replace
                # behavior is the only option on this filesystem.
                os.replace(
                    source[1],
                    destination[1],
                    src_dir_fd=source[0],
                    dst_dir_fd=destination[0],
                )


def replace_rooted(
    roots: dict[str, Path],
    source_root_id: str,
    source_relative_path: str,
    destination_root_id: str,
    destination_relative_path: str,
) -> None:
    """Replace one rooted path without following a swapped parent symlink."""

    try:
        source_root = roots[source_root_id]
        destination_root = roots[destination_root_id]
    except KeyError as error:
        raise ValueError("A rooted replacement references an unknown root.") from error
    with _rooted_parent(source_root, source_relative_path) as source:
        with _rooted_parent(destination_root, destination_relative_path) as destination:
            os.replace(
                source[1],
                destination[1],
                src_dir_fd=source[0],
                dst_dir_fd=destination[0],
            )


def unlink_rooted(
    roots: dict[str, Path],
    root_id: str,
    relative_path: str,
    *,
    missing_ok: bool = False,
) -> None:
    """Unlink one rooted path without following a swapped parent symlink."""

    try:
        root = roots[root_id]
    except KeyError as error:
        raise ValueError("A rooted unlink references an unknown root.") from error
    with _rooted_parent(root, relative_path) as target:
        try:
            os.unlink(target[1], dir_fd=target[0])
        except FileNotFoundError:
            if not missing_ok:
                raise


def copy_rooted(
    roots: dict[str, Path],
    source_root_id: str,
    source_relative_path: str,
    destination_root_id: str,
    destination_relative_path: str,
) -> None:
    """Copy one regular file through stable rooted directory descriptors."""

    try:
        source_root = roots[source_root_id]
        destination_root = roots[destination_root_id]
    except KeyError as error:
        raise ValueError("A rooted copy references an unknown root.") from error
    with _rooted_parent(source_root, source_relative_path) as source:
        with _rooted_parent(destination_root, destination_relative_path) as destination:
            source_fd = os.open(
                source[1], os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW, dir_fd=source[0]
            )
            try:
                source_stat = os.fstat(source_fd)
                if not stat.S_ISREG(source_stat.st_mode):
                    raise OSError("A rooted copy source is not a regular file.")
                destination_fd = os.open(
                    destination[1],
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
                    source_stat.st_mode & 0o777,
                    dir_fd=destination[0],
                )
                try:
                    while block := os.read(source_fd, 1024 * 1024):
                        view = memoryview(block)
                        while view:
                            written = os.write(destination_fd, view)
                            view = view[written:]
                    os.fchmod(destination_fd, source_stat.st_mode & 0o777)
                    os.utime(
                        destination_fd,
                        ns=(source_stat.st_atime_ns, source_stat.st_mtime_ns),
                    )
                    os.fsync(destination_fd)
                except BaseException:
                    os.close(destination_fd)
                    destination_fd = -1
                    os.unlink(destination[1], dir_fd=destination[0])
                    raise
                finally:
                    if destination_fd >= 0:
                        os.close(destination_fd)
            finally:
                os.close(source_fd)
