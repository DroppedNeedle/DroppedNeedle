import threading

from infrastructure.persistence._database import PriorityWriteLock


def _wait_for_queued(
    lock: PriorityWriteLock, *, foreground: int, background: int
) -> None:
    with lock._condition:
        assert lock._condition.wait_for(
            lambda: lock._foreground_waiters >= foreground
            and lock._background_waiters >= background,
            timeout=2,
        )


def test_foreground_waiter_precedes_background_work() -> None:
    lock = PriorityWriteLock(foreground_burst=8)
    order: list[str] = []
    for _ in range(8):
        with lock:
            pass
    lock.acquire()

    def background() -> None:
        with lock.background():
            order.append("background")

    def foreground() -> None:
        with lock:
            order.append("foreground")

    background_thread = threading.Thread(target=background)
    foreground_thread = threading.Thread(target=foreground)
    background_thread.start()
    foreground_thread.start()
    _wait_for_queued(lock, foreground=1, background=1)
    lock.release()
    background_thread.join(timeout=2)
    foreground_thread.join(timeout=2)

    assert order == ["foreground", "background"]


def test_background_gets_a_grant_after_eight_foreground_grants() -> None:
    lock = PriorityWriteLock(foreground_burst=8)
    order: list[str] = []
    lock.acquire()

    def background() -> None:
        with lock.background():
            order.append("background")

    def foreground(index: int) -> None:
        with lock:
            order.append(f"foreground-{index}")

    background_thread = threading.Thread(target=background)
    foreground_threads = [
        threading.Thread(target=foreground, args=(index,)) for index in range(10)
    ]
    background_thread.start()
    for thread in foreground_threads:
        thread.start()
    _wait_for_queued(lock, foreground=10, background=1)
    lock.release()
    background_thread.join(timeout=2)
    for thread in foreground_threads:
        thread.join(timeout=2)

    assert len(order) == 11
    assert order.index("background") <= 8
