import asyncio
from typing import TypeVar, Awaitable, Callable, Any
from functools import wraps

from core.exceptions import ClientDisconnectedError

T = TypeVar("T")

_MAX_DEDUP_RETRIES = 1


class RequestDeduplicator:
    """
    Prevents duplicate concurrent requests by coalescing identical requests.

    If request A is in-flight and request B arrives with the same key,
    request B will wait for A's result instead of making a duplicate call.

    Uses ``asyncio.shield`` so that a waiter's task cancellation propagates
    cleanly without poisoning the shared future.  If the leader disconnects
    (``ClientDisconnectedError``), the shared future is cancelled and one
    waiting follower is allowed to retry as the new leader (bounded to
    ``_MAX_DEDUP_RETRIES`` attempts).
    """

    def __init__(self):
        self._pending: dict[str, asyncio.Future[Any]] = {}
        self._lock = asyncio.Lock()

    def clear(self) -> None:
        """Clear all pending deduplication entries (e.g. after endpoint change)."""
        self._pending.clear()

    async def dedupe(self, key: str, coro_factory: Callable[[], Awaitable[T]]) -> T:
        retries = 0
        while True:
            async with self._lock:
                if key in self._pending:
                    future = self._pending[key]
                    should_execute = False
                else:
                    future = asyncio.get_running_loop().create_future()
                    self._pending[key] = future
                    should_execute = True

            if should_execute:
                try:
                    result = await coro_factory()
                    if not future.done():
                        future.set_result(result)
                    return result
                except (ClientDisconnectedError, asyncio.CancelledError):
                    # CANCEL the shared future (don't set_exception). Setting a CancelledError
                    # leaves the future holding an exception nobody retrieves when the leader is
                    # cancelled with no follower waiting -> asyncio logs "Future exception was
                    # never retrieved" at ERROR (routine during the LB outage, when budgeted
                    # discover builds cancel in-flight 1/s MusicBrainz lookups). A cancelled
                    # future is quiet and still lets a follower retry as the new leader.
                    if not future.done():
                        future.cancel()
                    raise
                except BaseException as exc:
                    if not future.done():
                        future.set_exception(exc)
                        # The leader re-raises directly. Mark the exception retrieved so
                        # a request with no followers does not emit "Future exception was
                        # never retrieved"; followers can still await and receive it.
                        future.exception()
                    raise
                finally:
                    if not future.done():
                        future.cancel()
                    async with self._lock:
                        # A clear() followed by a new leader can install a
                        # different future for this key.  Never let the old
                        # leader's finalizer remove that replacement.
                        if self._pending.get(key) is future:
                            self._pending.pop(key, None)

            # Follower path: shield prevents waiter cancellation from poisoning the shared future.
            try:
                return await asyncio.shield(future)
            except asyncio.CancelledError:
                task = asyncio.current_task()
                if task is not None and task.cancelling() > 0:
                    raise
                # Future was cancelled by the leader (disconnect). Retry once
                # so this follower can take over as leader.
                retries += 1
                if retries > _MAX_DEDUP_RETRIES:
                    raise
                continue


_global_deduplicator = RequestDeduplicator()


def get_deduplicator() -> RequestDeduplicator:
    return _global_deduplicator


def deduplicate(key_func: Callable[..., str]):
    """
    Decorator that deduplicates concurrent calls to the same function
    with the same key.

    Usage:
        @deduplicate(lambda self, artist_id: f"artist:{artist_id}")
        async def get_artist(self, artist_id: str) -> Artist:
            ...
    """

    def decorator(func: Callable[..., Awaitable[T]]) -> Callable[..., Awaitable[T]]:
        @wraps(func)
        async def wrapper(*args, **kwargs) -> T:
            key = key_func(*args, **kwargs)
            dedup = get_deduplicator()
            return await dedup.dedupe(key, lambda: func(*args, **kwargs))

        return wrapper

    return decorator
