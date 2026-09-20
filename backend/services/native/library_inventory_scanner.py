"""One-walk, bounded-queue discovery for the inactive target catalog."""

from __future__ import annotations

import asyncio
import errno
import os
import stat
import threading
import time
import unicodedata
import logging
from collections.abc import Awaitable, Callable, Iterator
from pathlib import Path, PurePosixPath
from urllib.parse import quote_from_bytes

import msgspec

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_work import (
    ScanFailureRecord,
    ScanInventoryItem,
    ScanRun,
    ScanScope,
)
from services.local_files_service import AUDIO_EXTENSIONS
from services.native.library_filesystem_coordinator import (
    LibraryFilesystemCoordinator,
    is_management_artifact,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.file_revision import revision_from_stat

INVENTORY_QUEUE_SIZE = 256
INVENTORY_BATCH_SIZE = 256
# F-12: reap horizon for wedged detached walkers as a multiple of the walk
# deadline. The base knob (``walk_deadline_seconds``) and this multiplier are
# deliberately separate from the tag-read pair, which they only
# coincidentally equal (both default to 30.0s / 3x).
DETACHED_WALKER_REAP_MULTIPLIER = 3.0

Checkpoint = Callable[[str, str], Awaitable[bool]]
DirectoryWalker = Callable[..., Iterator[tuple[str, list[str], list[str]]]]
DirectoryProbe = Callable[[Path], bool]
logger = logging.getLogger(__name__)


class _WalkHeartbeat:
    """Thread-safe liveness signal written by the walk producer thread.

    F-025: WALK_TIMEOUT fires only when NO progress signal (directory yield or
    queued item) arrived for ``walk_deadline_seconds``. A single huge directory
    whose cold listing outlasts the deadline can still false-positive; that
    tradeoff is accepted for hashless scans (F-029) and contained by F-024's
    loud walker-cap refusal instead of silent thread leaks.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._touched_at = time.monotonic()
        self.last_directory = ""

    def touch(self, directory: str = "") -> None:
        with self._lock:
            self._touched_at = time.monotonic()
            if directory:
                self.last_directory = directory

    def age(self) -> float:
        with self._lock:
            return time.monotonic() - self._touched_at

class LibraryInventoryScanner:
    def __init__(
        self,
        store: NativeLibraryStore,
        *,
        directory_walker: DirectoryWalker = os.walk,
        filesystem_coordinator: LibraryFilesystemCoordinator | None = None,
        walk_deadline_seconds: float = 30.0,
        directory_probe: DirectoryProbe = Path.is_dir,
        max_detached_walkers: int = 4,
        probe_executor_max_workers: int = 1,
        clock: Callable[[], float] = time.time,
        detached_walker_reap_multiplier: float = DETACHED_WALKER_REAP_MULTIPLIER,
        monotonic_clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._store = store
        self._directory_walker = directory_walker
        self._filesystem = filesystem_coordinator
        self._walk_deadline_seconds = walk_deadline_seconds
        self._directory_probe = directory_probe
        self._max_detached_walkers = max_detached_walkers
        self._detached_walkers: set[asyncio.Task[None]] = set()
        self._detached_walker_reap_multiplier = detached_walker_reap_multiplier
        self._monotonic_clock = monotonic_clock
        # F-12: detach timestamps for the in-flight set above; entries older
        # than the reap horizon are evicted lazily on refusal and on
        # completion, never by a periodic task.
        self._detached_walker_started: dict[asyncio.Task[None], float] = {}
        self._probe_max_workers = probe_executor_max_workers
        self._probe_lock = threading.Lock()
        self._pending_probes: set[asyncio.Future[bool]] = set()
        self._closed = False
        # F-024: monotonic count of walkers that wedged beyond the detach cap;
        # unlike the in-flight set this never shrinks, so repeated leaks stay
        # observable for the life of the process.
        self._leaked_walkers = 0
        # F-023: probes abandoned past their deadline; each one means a stat
        # is still blocked somewhere on the filesystem.
        self._wedged_probes = 0
        self._clock = clock

    @property
    def wedged_probe_count(self) -> int:
        return self._wedged_probes

    @property
    def leaked_walker_count(self) -> int:
        return self._leaked_walkers

    def _finish_detached_walker(self, task: asyncio.Task[None]) -> None:
        # A late finish of an already-evicted walker is a safe noop: the
        # discard/pop tolerate the missing entry and the exception is still
        # consumed so no "never retrieved" warning escapes.
        self._detached_walkers.discard(task)
        self._detached_walker_started.pop(task, None)
        if not task.cancelled():
            task.exception()
        # F-12: opportunistically forget other wedged entries on completion
        # (past the horizon by design so scans keep moving; concurrency is
        # bounded by max-detached plus refusal/completion reaps).
        self._reap_stale_detached_walkers()

    def _reap_stale_detached_walkers(self) -> int:
        """Forget detached walkers older than the reap horizon.

        Evicted entries leave tracking entirely (cap freed, timestamp map
        cleaned): wedged entries are forgotten past the horizon by design so
        scans keep moving, and concurrency is bounded by max-detached plus
        refusal/completion reaps. A late finish of an evicted entry stays a
        safe noop via its still-attached done-callback.
        """
        horizon = self._walk_deadline_seconds * self._detached_walker_reap_multiplier
        now = self._monotonic_clock()
        stale = [
            task
            for task, started in self._detached_walker_started.items()
            if now - started > horizon
        ]
        for task in stale:
            self._detached_walkers.discard(task)
            self._detached_walker_started.pop(task, None)
        return len(stale)

    def _detach_walker(self, task: asyncio.Task[None]) -> bool:
        """Detach a wedged producer so it cannot wedge the scan worker.

        F-024: the cap is ENFORCED - beyond ``max_detached_walkers`` in-flight
        wedged walkers the task is refused (and counted as leaked) instead of
        being tracked silently; the caller fails the run with
        WALKER_UNAVAILABLE, mirroring the tag-read capacity contract.
        F-12: entries past the reap horizon drain first, so only a
        still-fresh full set refuses.
        """
        if task.done():
            return True
        if len(self._detached_walkers) >= self._max_detached_walkers:
            # F-12: wedged entries past the horizon are forgotten (drained,
            # cap freed) by design so scans keep moving; concurrency is
            # bounded by max-detached plus refusal/completion reaps. Only a
            # still-fresh full set refuses.
            self._reap_stale_detached_walkers()
            if len(self._detached_walkers) >= self._max_detached_walkers:
                self._leaked_walkers += 1
                logger.warning(
                    "library_scan event=detached_walker_cap_exceeded count=%s max=%s "
                    "leaked_total=%s",
                    len(self._detached_walkers) + 1,
                    self._max_detached_walkers,
                    self._leaked_walkers,
                )
                return False
        self._detached_walkers.add(task)
        self._detached_walker_started[task] = self._monotonic_clock()
        task.add_done_callback(self._finish_detached_walker)
        return True

    def _remove_pending_probe(self, fut: asyncio.Future[bool]) -> None:
        with self._probe_lock:
            self._pending_probes.discard(fut)

    @property
    def probe_pending_count(self) -> int:
        with self._probe_lock:
            return len(self._pending_probes)

    def close(self) -> None:
        with self._probe_lock:
            if self._closed:
                return
            self._closed = True
            pending = list(self._pending_probes)
            self._pending_probes.clear()
        for fut in pending:
            if fut.done():
                continue
            try:
                loop = fut.get_loop()  # type: ignore[attr-defined]
                if loop.is_running():
                    loop.call_soon_threadsafe(fut.cancel)
                else:
                    fut.cancel()
            except Exception:  # noqa: BLE001 - close must not fail on pending future cancel
                try:
                    fut.cancel()
                except Exception:  # noqa: BLE001 - close must not fail on pending future cancel
                    pass

    async def aclose(self) -> None:
        self.close()


    async def _record_failure(
        self,
        run_id: str,
        scope: ScanScope,
        *,
        relative_path: str,
        failure_code: str,
        failure_detail: str,
    ) -> None:
        await self._store.record_scan_failures(
            run_id,
            [
                ScanFailureRecord(
                    root_id=scope.root_id,
                    relative_path=relative_path,
                    failure_code=failure_code,
                    recorded_at=self._clock(),
                    failure_detail=failure_detail,
                    phase="discovering",
                )
            ],
        )

    @staticmethod
    def _text_safe_posix(path: Path) -> str:
        """POSIX text for a path that is always bindable as SQLite TEXT
        (F-021 / NEW-SCAN-04): names that are not valid UTF-8 arrive here as
        surrogateescape strings and would re-poison any failure-row insert,
        so their raw bytes are losslessly percent-encoded instead."""
        text = PurePosixPath(*path.parts).as_posix()
        try:
            text.encode("utf-8")
        except UnicodeEncodeError:
            return quote_from_bytes(os.fsencode(path), safe="/")
        return text

    @staticmethod
    def _relativize(path: Path, root: Path) -> str:
        try:
            relative = path.relative_to(root)
        except ValueError:
            relative = path
        return LibraryInventoryScanner._text_safe_posix(relative)

    @staticmethod
    def _inventory_key(path: Path, root: Path) -> str:
        """NFC-normalized inventory key for one in-root path (step 4.13).

        macOS writes NFD bytes while the walk historically stored whatever
        the filesystem produced, so an NFC<->NFD rename of an unchanged
        file must classify as a move (same key) instead of a delete+new
        pair. Only the key is normalized: display strings such as
        absolute_path keep their on-disk form, _relativize (failure and
        heartbeat paths) is untouched, non-UTF-8 names still take the
        WALK_NAME_ENCODING skip upstream, and case-only variants are
        unaffected (NFC never folds case).

        One-time churn: rows persisted before this normalization keep
        their raw key, so a pre-existing NFD row first re-presents as NFC
        (new key plus a missing old key) and settles on the next run. No
        stored-key migration: the rename is one honest move, not a silent
        history rewrite.
        """
        return unicodedata.normalize(
            "NFC", PurePosixPath(*path.relative_to(root).parts).as_posix()
        )

    @staticmethod
    def _walk_failure_detail(exc: BaseException) -> str:
        """Class-name-plus-errno detail; never str(error) or filesystem paths
        (F-032, consistent with the indexing-phase NEW-SCAN-04 standard)."""
        if isinstance(exc, OSError):
            code = (
                errno.errorcode.get(exc.errno, "EUNKNOWN")
                if exc.errno is not None
                else "EUNKNOWN"
            )
            return f"{type(exc).__name__} (errno={code}) while walking."
        return f"{type(exc).__name__} while walking."

    @staticmethod
    def _failure_relative_path(
        exc: BaseException, root: Path, heartbeat: _WalkHeartbeat
    ) -> str:
        filename = getattr(exc, "filename", None)
        candidate = (
            Path(str(filename))
            if filename
            else (Path(heartbeat.last_directory) if heartbeat.last_directory else None)
        )
        if candidate is None:
            return "."
        return LibraryInventoryScanner._relativize(candidate, root)

    async def discover(
        self,
        run: ScanRun,
        scopes: list[ScanScope],
        root_paths: dict[str, Path],
        resolver: LibraryPolicyResolver,
        checkpoint: Checkpoint,
    ) -> ScanRun:
        current = run
        # GH-296 skip-and-report accounting: a scope whose root cannot be
        # resolved or probed is recorded honestly (failure row + unavailable
        # scope) while remaining scopes keep walking. The run fails wholesale
        # only when nothing was discoverable at all.
        unavailable_scopes = 0
        for scope in scopes:
            if (
                await self._store.get_scan_scope_discovery_state(
                    run.id, scope.root_id, scope.relative_path
                )
                == "completed"
            ):
                continue
            if not await checkpoint(run.id, scope.policy_revision):
                return (await self._store.get_scan_run(run.id))[0]
            root = root_paths.get(scope.root_id)
            if root is None and scope.root_path is not None:
                root = Path(scope.root_path)
            if root is None:
                await self._record_failure(
                    run.id,
                    scope,
                    relative_path=scope.relative_path,
                    failure_code="ROOT_UNAVAILABLE",
                    failure_detail="The library root has no configured path.",
                )
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="unavailable",
                    error_code="ROOT_UNAVAILABLE",
                )
                # GH-296: report the unresolvable scope and continue with the
                # remaining scopes instead of failing the whole run.
                unavailable_scopes += 1
                continue
            selected = (
                root if scope.relative_path == "." else root / scope.relative_path
            )
            loop = asyncio.get_running_loop()
            probe_future: asyncio.Future[bool] | None = None
            should_fail_capacity = False
            should_fail_closed = False
            with self._probe_lock:
                if self._closed:
                    should_fail_closed = True
                elif len(self._pending_probes) >= self._probe_max_workers:
                    should_fail_capacity = True
                else:
                    probe_future = loop.create_future()
                    self._pending_probes.add(probe_future)

                    def _on_done(f: asyncio.Future[bool]) -> None:
                        with self._probe_lock:
                            self._pending_probes.discard(f)

                    probe_future.add_done_callback(_on_done)
            if should_fail_closed:
                logger.warning(
                    "library_scan event=probe_executor_closed run_id=%s root_id=%s",
                    run.id,
                    scope.root_id,
                )
                await self._record_failure(
                    run.id,
                    scope,
                    relative_path=scope.relative_path,
                    failure_code="WALK_TIMEOUT",
                    failure_detail=(
                        "The library root probe exceeded "
                        f"{self._walk_deadline_seconds:.1f}s."
                    ),
                )
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="unavailable",
                    error_code="WALK_TIMEOUT",
                )
                return await self._store.transition_scan_run(
                    run.id,
                    expected_state=current.state,
                    expected_revision=current.row_revision,
                    new_state="failed",
                    now=self._clock(),
                    terminal_code="WALK_TIMEOUT",
                )
            if should_fail_capacity:
                logger.warning(
                    "library_scan event=probe_capacity_exceeded pending=%s max=%s run_id=%s root_id=%s",
                    len(self._pending_probes) + 1,
                    self._probe_max_workers,
                    run.id,
                    scope.root_id,
                )
                # F-023: distinguish a wedged previous probe (slot occupied)
                # from this root's own timeout, and give the wedged stat ONE
                # bounded deadline to finish before failing the run.
                await asyncio.sleep(self._walk_deadline_seconds)
                with self._probe_lock:
                    recovered = (
                        not self._closed
                        and len(self._pending_probes) < self._probe_max_workers
                    )
                if recovered:
                    logger.info(
                        "library_scan event=probe_slot_recovered run_id=%s root_id=%s",
                        run.id,
                        scope.root_id,
                    )
                    should_fail_capacity = False
                    probe_future = None
                else:
                    await self._record_failure(
                        run.id,
                        scope,
                        relative_path=scope.relative_path,
                        failure_code="PROBE_UNAVAILABLE",
                        failure_detail=(
                            "A previous root probe never completed; scanning is "
                            "paused until the filesystem responds or the service "
                            "restarts."
                        ),
                    )
                    await self._store.complete_scan_scope_discovery(
                        run.id,
                        scope.root_id,
                        scope.relative_path,
                        state="unavailable",
                        error_code="PROBE_UNAVAILABLE",
                    )
                    return await self._store.transition_scan_run(
                        run.id,
                        expected_state=current.state,
                        expected_revision=current.row_revision,
                        new_state="failed",
                        now=self._clock(),
                        terminal_code="PROBE_UNAVAILABLE",
                    )

            assert probe_future is None or not should_fail_capacity

            if probe_future is None:
                # F-023 retry path: the wedged slot freed up, so start a fresh
                # probe for this scope instead of failing the run.
                probe_future = loop.create_future()

                def _on_retry_done(f: asyncio.Future[bool]) -> None:
                    with self._probe_lock:
                        self._pending_probes.discard(f)

                probe_future.add_done_callback(_on_retry_done)

            def _probe_runner() -> None:
                try:
                    result = self._directory_probe(selected)
                    exc: BaseException | None = None
                except BaseException as e:  # noqa: BLE001 - probe must propagate BaseException via future
                    result = False
                    exc = e

                def _complete() -> None:
                    if probe_future.done():
                        return
                    if exc is not None:
                        if not probe_future.done():
                            probe_future.set_exception(exc)
                    else:
                        if not probe_future.done():
                            probe_future.set_result(result)  # type: ignore[arg-type]

                try:
                    loop.call_soon_threadsafe(_complete)
                except RuntimeError:
                    logger.debug(
                        "library_scan event=probe_loop_closed run_id=%s root_id=%s",
                        run.id,
                        scope.root_id,
                    )
                    return

            thread = threading.Thread(target=_probe_runner, daemon=True, name="library-probe")
            thread.start()
            try:
                exists = await asyncio.wait_for(
                    asyncio.shield(probe_future),
                    timeout=self._walk_deadline_seconds,
                )
            except TimeoutError:
                # F-023: the shielded inner future is abandoned uncancelled by
                # wait_for; tombstone it (cancel on the loop thread - the late
                # _complete re-checks done()) so the slot is recovered instead
                # of staying occupied for the process lifetime.
                self._wedged_probes += 1
                try:
                    loop.call_soon_threadsafe(probe_future.cancel)
                except RuntimeError:
                    probe_future.cancel()
                self._remove_pending_probe(probe_future)
                logger.warning(
                    "library_scan event=walk_timeout run_id=%s root_id=%s path=%s",
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                )
                await self._record_failure(
                    run.id,
                    scope,
                    relative_path=scope.relative_path,
                    failure_code="WALK_TIMEOUT",
                    failure_detail=(
                        "The library root probe exceeded "
                        f"{self._walk_deadline_seconds:.1f}s."
                    ),
                )
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="unavailable",
                    error_code="WALK_TIMEOUT",
                )
                return await self._store.transition_scan_run(
                    run.id,
                    expected_state=current.state,
                    expected_revision=current.row_revision,
                    new_state="failed",
                    now=self._clock(),
                    terminal_code="WALK_TIMEOUT",
                )
            except asyncio.CancelledError:
                if probe_future.cancelled():
                    logger.warning(
                        "library_scan event=probe_cancelled run_id=%s root_id=%s",
                        run.id,
                        scope.root_id,
                    )
                    await self._record_failure(
                        run.id,
                        scope,
                        relative_path=scope.relative_path,
                        failure_code="WALK_TIMEOUT",
                        failure_detail=(
                            "The library root probe exceeded "
                            f"{self._walk_deadline_seconds:.1f}s."
                        ),
                    )
                    await self._store.complete_scan_scope_discovery(
                        run.id,
                        scope.root_id,
                        scope.relative_path,
                        state="unavailable",
                        error_code="WALK_TIMEOUT",
                    )
                    return await self._store.transition_scan_run(
                        run.id,
                        expected_state=current.state,
                        expected_revision=current.row_revision,
                        new_state="failed",
                        now=self._clock(),
                        terminal_code="WALK_TIMEOUT",
                    )
                raise
            if not exists:
                await self._record_failure(
                    run.id,
                    scope,
                    relative_path=scope.relative_path,
                    failure_code="ROOT_UNAVAILABLE",
                    failure_detail=(
                        "The library root path is missing: "
                        f"{scope.relative_path}"
                    ),
                )
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="unavailable",
                    error_code="ROOT_UNAVAILABLE",
                )
                # GH-296: report the missing path and continue with the
                # remaining scopes instead of failing the whole run.
                unavailable_scopes += 1
                continue
            restarts = 0
            superseded_scope = False
            while True:
                discovery_generation = (
                    await self._store.get_scan_scope_discovery_generation(
                        run.id, scope.root_id, scope.relative_path
                    )
                )
                filesystem_revision = (
                    self._filesystem.revision(scope.root_id)
                    if self._filesystem is not None
                    else None
                )
                current, completed, walk_failure_code = await self._walk_scope(
                    current,
                    scope,
                    root,
                    selected,
                    resolver,
                    checkpoint,
                    discovery_generation,
                )
                # GH-444: a transient mid-walk stall (contended disk, a wedged
                # syscall that clears) must not fail the whole run. Re-walk
                # the scope boundedly, sharing the F-030 restart budget (worst
                # case 3 walks + 2 inter-retry sleeps before honest terminal
                # failure); a still-wedged producer fails honestly after the
                # cap, and a coordinator pause/stop/supersede (or failed
                # checkpoint) wins over retry. Only the exact WALK_TIMEOUT
                # code retries - never WALKER_UNAVAILABLE, permission codes,
                # or control exits.
                if not completed and walk_failure_code == "WALK_TIMEOUT":
                    current = (await self._store.get_scan_run(run.id))[0]
                    if (
                        restarts < 2
                        and current.state == "discovering"
                        and await checkpoint(run.id, scope.policy_revision)
                    ):
                        restarts += 1
                        await asyncio.sleep(self._walk_deadline_seconds)
                        current = (await self._store.get_scan_run(run.id))[0]
                        if current.state != "discovering" or not await checkpoint(
                            run.id, scope.policy_revision
                        ):
                            break
                        logger.warning(
                            "library_scan event=walk_timeout_retry run_id=%s "
                            "root_id=%s path=%s attempt=%d",
                            run.id,
                            scope.root_id,
                            scope.relative_path,
                            restarts,
                        )
                        await self._store.restart_scan_scope_discovery(
                            run.id, scope.root_id, scope.relative_path
                        )
                        current = (await self._store.get_scan_run(run.id))[0]
                        await self._store.cleanup_stale_scan_inventory(run.id)
                        continue
                if not completed or self._filesystem is None:
                    break
                async with self._filesystem.read(scope.root_id):
                    if self._filesystem.revision(scope.root_id) == filesystem_revision:
                        # F-022/F-030: only a CLEAN, un-degraded walk records
                        # the fence - a partially-read scope keeps the
                        # reconciler conservative (allow_missing stays false).
                        if walk_failure_code is None and not superseded_scope:
                            self._filesystem.record_scan_revision(
                                run.id, scope.root_id
                            )
                        break
                    restarts += 1
                    if restarts >= 3:
                        # F-030: sustained concurrent publication would
                        # otherwise re-walk this scope forever; stop after the
                        # bound and complete honestly - the follow-up request
                        # machinery requeues what the last generation missed.
                        logger.warning(
                            "library_scan event=walk_superseded run_id=%s "
                            "root_id=%s path=%s restarts=%d",
                            run.id,
                            scope.root_id,
                            scope.relative_path,
                            restarts,
                        )
                        superseded_scope = True
                        break
                    await self._store.restart_scan_scope_discovery(
                        run.id, scope.root_id, scope.relative_path
                    )
                    current = (await self._store.get_scan_run(run.id))[0]
                await self._store.cleanup_stale_scan_inventory(run.id)
            if not completed:
                current = (await self._store.get_scan_run(run.id))[0]
                if current.state == "discovering":
                    current = await self._store.transition_scan_run(
                        run.id,
                        expected_state="discovering",
                        expected_revision=current.row_revision,
                        new_state="failed",
                        now=self._clock(),
                        terminal_code=walk_failure_code or "ROOT_PERMISSION_DENIED",
                    )
                return current
            if superseded_scope:
                # F-030: honest partially-read completion for the capped scope;
                # inventory from the last completed generation stays durable.
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="partially_read",
                    error_code="WALK_SUPERSEDED",
                )
            elif walk_failure_code:
                # F-022: the walk degraded on unreadable paths - keep the
                # honest partially_read completion instead of overriding it
                # with a clean completed.
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="partially_read",
                    error_code=walk_failure_code,
                )
            else:
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="completed",
                    error_code=None,
                )
        if unavailable_scopes and unavailable_scopes == len(scopes):
            # GH-296: every scope proved unreachable, so the run terminates
            # honestly as failed instead of completing silently green.
            logger.warning(
                "library_scan event=all_scopes_unavailable run_id=%s count=%d",
                run.id,
                unavailable_scopes,
            )
            return await self._store.transition_scan_run(
                run.id,
                expected_state=current.state,
                expected_revision=current.row_revision,
                new_state="failed",
                now=self._clock(),
                terminal_code="ROOT_UNAVAILABLE",
            )
        return current

    async def _walk_scope(
        self,
        run: ScanRun,
        scope: ScanScope,
        root: Path,
        selected: Path,
        resolver: LibraryPolicyResolver,
        checkpoint: Checkpoint,
        discovery_generation: int = 1,
    ) -> tuple[ScanRun, bool, str | None]:
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue[
            tuple[Path, os.stat_result] | BaseException | list[tuple[str, str]] | None
        ] = (
            asyncio.Queue(maxsize=INVENTORY_QUEUE_SIZE)
        )
        stopped = threading.Event()
        heartbeat = _WalkHeartbeat()

        def producer() -> None:
            # F-022: unreadable directories are collected and reported instead
            # of aborting the walk (GH-296 skip-and-report for subpaths).
            walk_errors: list[OSError] = []

            def onerror(error: OSError) -> None:
                walk_errors.append(error)

            try:
                walker = iter(
                    self._directory_walker(selected, followlinks=False, onerror=onerror)
                )
                while True:
                    if stopped.is_set():
                        break
                    try:
                        directory, subdirectories, filenames = next(walker)
                    except StopIteration:
                        break
                    heartbeat.touch(directory)
                    subdirectories[:] = [
                        name
                        for name in subdirectories
                        if not is_management_artifact(Path(name))
                    ]
                    inspected: list[
                        tuple[Path, os.stat_result] | BaseException
                    ] = []
                    skips: list[tuple[str, str]] = []
                    for filename in filenames:
                        path = Path(directory) / filename
                        if (
                            path.suffix.casefold() not in AUDIO_EXTENSIONS
                            or is_management_artifact(path)
                        ):
                            continue
                        try:
                            path.as_posix().encode("utf-8")
                        except UnicodeEncodeError:
                            # F-021: surrogateescape names would poison every
                            # downstream TEXT bind; skip and report with a
                            # percent-encoded key instead.
                            skips.append(
                                (
                                    LibraryInventoryScanner._text_safe_posix(
                                        path.relative_to(root)
                                    ),
                                    "WALK_NAME_ENCODING",
                                )
                            )
                            continue
                        # In-root file symlinks resolve onto their target's own
                        # path by design; escape-out links are audited below
                        # (E11: symlinks are never followed into the library).
                        resolved = path.resolve(strict=False)
                        if not resolved.is_relative_to(root):
                            skips.append(
                                (
                                    LibraryInventoryScanner._text_safe_posix(
                                        path.relative_to(root)
                                    ),
                                    "SYMLINK_ESCAPE_OUT",
                                )
                            )
                            continue
                        try:
                            file_stat = resolved.stat()
                        except FileNotFoundError:
                            continue
                        except OSError as exc:
                            inspected.append(exc)
                            continue
                        if not stat.S_ISREG(file_stat.st_mode):
                            # F-13: FIFOs/sockets/devices with an audio suffix
                            # would wedge a tag reader in open(); skip them in
                            # the inventory phase without touching the
                            # tag-reader budget.
                            skips.append(
                                (
                                    LibraryInventoryScanner._text_safe_posix(
                                        path.relative_to(root)
                                    ),
                                    "NON_REGULAR_FILE",
                                )
                            )
                            continue
                        inspected.append((resolved, file_stat))
                    if skips:
                        asyncio.run_coroutine_threadsafe(
                            queue.put(skips), loop
                        ).result()
                    for item in inspected:
                        asyncio.run_coroutine_threadsafe(queue.put(item), loop).result()
                        # F-025: delivery is a progress signal, not just reads.
                        heartbeat.touch(directory)
                for error in walk_errors:
                    asyncio.run_coroutine_threadsafe(
                        queue.put(error), loop
                    ).result()
            except (OSError, RuntimeError) as exc:
                asyncio.run_coroutine_threadsafe(queue.put(exc), loop).result()
            finally:
                asyncio.run_coroutine_threadsafe(queue.put(None), loop).result()

        producer_task = asyncio.create_task(asyncio.to_thread(producer))
        batch: list[tuple[Path, os.stat_result]] = []
        current = run
        completed = True
        discard_remaining = False
        detached = False
        # F-INDEXREC-06: a checkpoint-false exit is pause/stop/policy-supersede,
        # not a filesystem error - the partial scope must not inherit a
        # permission code.
        control_exit = False
        walk_failure_code: str | None = None
        # F-022: a walk that finished despite per-path errors degrades instead
        # of failing the run; first degraded code becomes the scope diagnostic.
        degraded_code: str | None = None
        discovered = 0
        stale_cleanup_pending = True
        last_checkpoint = time.monotonic()
        last_log = last_checkpoint
        # F-025: delivery progress is tracked separately from producer touches.
        last_item_at = time.monotonic()
        # F-020: distinct persisted relative paths within one discovery
        # generation; memory cost is one path string per distinct file.
        seen_relative_paths: set[str] = set()
        try:
            while True:
                try:
                    item = await asyncio.wait_for(queue.get(), timeout=0.25)
                except TimeoutError:
                    if (
                        heartbeat.age() > self._walk_deadline_seconds
                        and time.monotonic() - last_item_at
                        > self._walk_deadline_seconds
                    ):
                        completed = False
                        stopped.set()
                        walk_failure_code = "WALK_TIMEOUT"
                        logger.warning(
                            "library_scan event=walk_timeout run_id=%s root_id=%s "
                            "path=%s",
                            run.id,
                            scope.root_id,
                            heartbeat.last_directory or scope.relative_path,
                        )
                        await self._record_failure(
                            run.id,
                            scope,
                            relative_path=(
                                self._relativize(Path(heartbeat.last_directory), root)
                                if heartbeat.last_directory
                                else scope.relative_path
                            ),
                            failure_code="WALK_TIMEOUT",
                            failure_detail=(
                                "The directory walk made no delivered progress "
                                f"for {self._walk_deadline_seconds:.1f}s."
                            ),
                        )
                        # The producer thread is wedged in a syscall; awaiting it
                        # would wedge the scan worker, so it is detached instead.
                        # F-024: beyond the in-flight cap the leak is counted and
                        # the run fails with a dedicated code (no silent leaks);
                        # either way the wedged producer is never awaited.
                        accepted = self._detach_walker(producer_task)
                        detached = True
                        if not accepted:
                            walk_failure_code = "WALKER_UNAVAILABLE"
                            await self._record_failure(
                                run.id,
                                scope,
                                relative_path=scope.relative_path,
                                failure_code="WALKER_UNAVAILABLE",
                                failure_detail=(
                                    "Too many wedged directory walks are still "
                                    "in flight; the walk was not started."
                                ),
                            )
                        break
                    if not await checkpoint(run.id, scope.policy_revision):
                        completed = False
                        stopped.set()
                        discard_remaining = True
                        control_exit = True
                    last_checkpoint = time.monotonic()
                    continue
                last_item_at = time.monotonic()
                if item is None:
                    break
                if discard_remaining:
                    continue
                if isinstance(item, list):
                    # F-020/F-021 skip records: escape-out symlinks and
                    # non-UTF-8 names become auditable failure rows.
                    for relative_path, failure_code in item:
                        if failure_code == "SYMLINK_ESCAPE_OUT":
                            failure_detail = (
                                "A symbolic link resolves outside its library "
                                "root; it was not followed."
                            )
                        elif failure_code == "NON_REGULAR_FILE":
                            failure_detail = (
                                "The path is not a regular file (FIFO, socket, "
                                "or device node); it was skipped without "
                                "reading tags."
                            )
                        else:
                            failure_detail = (
                                "A filename is not valid UTF-8; the file "
                                "was skipped."
                            )
                        await self._record_failure(
                            run.id,
                            scope,
                            relative_path=relative_path,
                            failure_code=failure_code,
                            failure_detail=failure_detail,
                        )
                    continue
                if isinstance(item, BaseException):
                    # F-022: record the per-path row but keep consuming - one
                    # unreadable file or directory no longer aborts the run
                    # (GH-296 skip-and-report for subpaths).
                    relative_path = self._failure_relative_path(item, root, heartbeat)
                    failure_code = (
                        "WALK_" + errno.errorcode.get(item.errno, "EUNKNOWN")
                        if isinstance(item, OSError) and item.errno is not None
                        else "WALK_ERROR"
                    )
                    if degraded_code is None:
                        degraded_code = failure_code
                    logger.warning(
                        "library_scan event=walk_error run_id=%s root_id=%s path=%s "
                        "error=%s",
                        run.id,
                        scope.root_id,
                        relative_path,
                        item,
                    )
                    await self._record_failure(
                        run.id,
                        scope,
                        relative_path=relative_path,
                        failure_code=failure_code,
                        failure_detail=self._walk_failure_detail(item),
                    )
                    continue
                # F-020: an in-root alias resolves onto its target's own path;
                # dedupe against everything already persisted in this
                # discovery generation so discovered_count counts distinct
                # files even when an alias batch lands after its target's.
                resolved_path, stat_result = item
                relative_key = LibraryInventoryScanner._inventory_key(
                    resolved_path, root
                )
                if relative_key in seen_relative_paths:
                    # Load-bearing collision rule: on Linux an NFC name and
                    # its NFD twin are two distinct files normalizing to one
                    # inventory key (the PK is run_id/root_id/relative_path).
                    # First wins; the loser gets an inventory-phase
                    # (discovering) failure row like SYMLINK_ESCAPE_OUT so a
                    # twin never surfaces as a UNIQUE violation run error.
                    # The row names the loser's own on-disk form and leaves
                    # degraded_code alone, so the run stays green.
                    await self._record_failure(
                        run.id,
                        scope,
                        relative_path=PurePosixPath(
                            *resolved_path.relative_to(root).parts
                        ).as_posix(),
                        failure_code="NFC_TWIN_COLLISION",
                        failure_detail=(
                            "Two on-disk names normalize to the same "
                            "inventory key; the first file won and this "
                            "twin was skipped."
                        ),
                    )
                    continue
                seen_relative_paths.add(relative_key)
                batch.append(item)
                if len(batch) >= INVENTORY_BATCH_SIZE:
                    current = await self._persist_batch(
                        current,
                        scope,
                        root,
                        batch,
                        resolver,
                        discovery_generation,
                    )
                    discovered += len(batch)
                    batch = []
                    if stale_cleanup_pending:
                        stale_cleanup_pending = bool(
                            await self._store.cleanup_stale_scan_inventory(run.id)
                        )
                    if not await checkpoint(run.id, scope.policy_revision):
                        completed = False
                        stopped.set()
                        discard_remaining = True
                        control_exit = True
                    last_checkpoint = time.monotonic()
                elif time.monotonic() - last_checkpoint >= 0.25:
                    if not await checkpoint(run.id, scope.policy_revision):
                        completed = False
                        stopped.set()
                        discard_remaining = True
                        control_exit = True
                    last_checkpoint = time.monotonic()
                if time.monotonic() - last_log >= 30.0:
                    logger.info(
                        "library_scan event=discovery_progress discovered=%d",
                        discovered + len(batch),
                    )
                    last_log = time.monotonic()
            if batch and completed:
                current = await self._persist_batch(
                    current,
                    scope,
                    root,
                    batch,
                    resolver,
                    discovery_generation,
                )
                discovered += len(batch)
                if stale_cleanup_pending:
                    await self._store.cleanup_stale_scan_inventory(run.id)
        except asyncio.CancelledError:
            stopped.set()
            while not producer_task.done():
                try:
                    await asyncio.wait_for(queue.get(), timeout=0.1)
                except TimeoutError:
                    if heartbeat.age() > self._walk_deadline_seconds:
                        # Detach even when the cap refuses: awaiting a wedged
                        # producer would wedge cancellation itself.
                        self._detach_walker(producer_task)
                        detached = True
                        break
                    continue
            if not detached:
                await asyncio.shield(producer_task)
            raise
        finally:
            stopped.set()
            if not detached and not producer_task.done():
                await producer_task
        if not completed:
            if control_exit and walk_failure_code is None:
                # F-INDEXREC-06: paused/cancelled/superseded is not a filesystem
                # failure - the durable run state is the control diagnostic.
                scope_error: str | None = None
            else:
                # Defensive fallback keeps an unclassified real failure visible.
                scope_error = walk_failure_code or "ROOT_PERMISSION_DENIED"
            await self._store.complete_scan_scope_discovery(
                run.id,
                scope.root_id,
                scope.relative_path,
                state="partially_read",
                error_code=scope_error,
            )
        elif degraded_code is not None:
            # F-022: the walk finished, but some paths were unreadable - the
            # scope completes honestly as partially read (the recorded failure
            # rows are the durable evidence) while the run proceeds to index
            # the inventory that DID land.
            await self._store.complete_scan_scope_discovery(
                run.id,
                scope.root_id,
                scope.relative_path,
                state="partially_read",
                error_code=degraded_code,
            )
        if degraded_code is not None:
            # F-022: surface the degrade through the return contract so
            # discover() completes this scope honestly instead of overriding.
            return current, completed, degraded_code
        return current, completed, walk_failure_code

    async def _persist_batch(
        self,
        run: ScanRun,
        scope: ScanScope,
        root: Path,
        batch: list[tuple[Path, os.stat_result]],
        resolver: LibraryPolicyResolver,
        discovery_generation: int,
    ) -> ScanRun:
        raw: list[tuple[Path, str, os.stat_result, str]] = []
        seen_keys: set[str] = set()
        for path, stat in batch:
            relative = LibraryInventoryScanner._inventory_key(path, root)
            if relative in seen_keys:
                # Same twin rule as the walk-time dedup above: _persist_batch
                # is also reachable without the walk, so it first-wins with a
                # loser row here too instead of letting one key reach the
                # (run_id, root_id, relative_path) PK twice.
                await self._record_failure(
                    run.id,
                    scope,
                    relative_path=PurePosixPath(
                        *path.relative_to(root).parts
                    ).as_posix(),
                    failure_code="NFC_TWIN_COLLISION",
                    failure_detail=(
                        "Two on-disk names normalize to the same "
                        "inventory key; the first file won and this "
                        "twin was skipped."
                    ),
                )
                continue
            seen_keys.add(relative)
            raw.append((path, relative, stat, revision_from_stat(stat)))
        comparisons = await self._store.classify_scan_paths(
            scope.root_id,
            [
                (relative, stat.st_size, stat.st_mtime_ns, stat.st_mtime, revision)
                for _, relative, stat, revision in raw
            ],
            # F-15/4.12 skew wiring: the store records beyond-band wall/FS
            # drift as MTIME_SKEW evidence only when the run id is passed
            # (it defaults to verdicts-only for its pre-existing caller).
            run_id=run.id,
        )
        items: list[ScanInventoryItem] = []
        for path, relative, stat, revision in raw:
            policy = resolver.resolve(path)
            effective_policy = (
                policy.policy if policy is not None else scope.effective_policy
            )
            comparison, track_id = comparisons[relative]
            if effective_policy == "excluded":
                comparison = "excluded"
            items.append(
                ScanInventoryItem(
                    root_id=scope.root_id,
                    relative_path=relative,
                    absolute_path=str(path),
                    file_size_bytes=stat.st_size,
                    file_mtime_ns=stat.st_mtime_ns,
                    stat_revision=revision,
                    policy_revision=scope.policy_revision,
                    effective_policy=effective_policy,
                    comparison_result=comparison,
                    local_track_id=track_id,
                    scope_relative_path=scope.relative_path,
                )
            )
        updated_at = time.time()
        revision, _ = await self._store.add_scan_inventory_batch(
            run.id,
            items,
            expected_run_revision=run.row_revision,
            updated_at=updated_at,
            discovery_generation=discovery_generation,
        )
        return msgspec.structs.replace(
            run, row_revision=revision, updated_at=updated_at
        )
