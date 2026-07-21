"""One-walk, bounded-queue discovery for the inactive target catalog."""

from __future__ import annotations

import asyncio
import os
import threading
import time
import logging
from collections.abc import Awaitable, Callable, Iterator
from pathlib import Path, PurePosixPath

import msgspec

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_work import ScanInventoryItem, ScanRun, ScanScope
from services.local_files_service import AUDIO_EXTENSIONS
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.file_revision import revision_from_stat

INVENTORY_QUEUE_SIZE = 256
INVENTORY_BATCH_SIZE = 256

Checkpoint = Callable[[str, str], Awaitable[bool]]
DirectoryWalker = Callable[..., Iterator[tuple[str, list[str], list[str]]]]
logger = logging.getLogger(__name__)


class LibraryInventoryScanner:
    def __init__(
        self,
        store: NativeLibraryStore,
        *,
        directory_walker: DirectoryWalker = os.walk,
    ) -> None:
        self._store = store
        self._directory_walker = directory_walker

    async def discover(
        self,
        run: ScanRun,
        scopes: list[ScanScope],
        root_paths: dict[str, Path],
        resolver: LibraryPolicyResolver,
        checkpoint: Checkpoint,
    ) -> ScanRun:
        current = run
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
            discovery_generation = (
                await self._store.get_scan_scope_discovery_generation(
                    run.id, scope.root_id, scope.relative_path
                )
            )
            root = root_paths.get(scope.root_id)
            if root is None and scope.root_path is not None:
                root = Path(scope.root_path)
            if root is None:
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="unavailable",
                    error_code="ROOT_UNAVAILABLE",
                )
                return await self._store.transition_scan_run(
                    run.id,
                    expected_state=current.state,
                    expected_revision=current.row_revision,
                    new_state="failed",
                    now=current.updated_at,
                    terminal_code="ROOT_UNAVAILABLE",
                )
            selected = (
                root if scope.relative_path == "." else root / scope.relative_path
            )
            exists = await asyncio.to_thread(selected.is_dir)
            if not exists:
                await self._store.complete_scan_scope_discovery(
                    run.id,
                    scope.root_id,
                    scope.relative_path,
                    state="unavailable",
                    error_code="ROOT_UNAVAILABLE",
                )
                return await self._store.transition_scan_run(
                    run.id,
                    expected_state=current.state,
                    expected_revision=current.row_revision,
                    new_state="failed",
                    now=current.updated_at,
                    terminal_code="ROOT_UNAVAILABLE",
                )
            current, completed = await self._walk_scope(
                current,
                scope,
                root,
                selected,
                resolver,
                checkpoint,
                discovery_generation,
            )
            if not completed:
                current = (await self._store.get_scan_run(run.id))[0]
                if current.state == "discovering":
                    current = await self._store.transition_scan_run(
                        run.id,
                        expected_state="discovering",
                        expected_revision=current.row_revision,
                        new_state="failed",
                        now=current.updated_at,
                        terminal_code="ROOT_PERMISSION_DENIED",
                    )
                return current
            await self._store.complete_scan_scope_discovery(
                run.id,
                scope.root_id,
                scope.relative_path,
                state="completed",
                error_code=None,
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
    ) -> tuple[ScanRun, bool]:
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue[tuple[Path, os.stat_result] | BaseException | None] = (
            asyncio.Queue(maxsize=INVENTORY_QUEUE_SIZE)
        )
        stopped = threading.Event()

        def producer() -> None:
            try:

                def onerror(error: OSError) -> None:
                    raise error

                for directory, _, filenames in self._directory_walker(
                    selected, followlinks=False, onerror=onerror
                ):
                    if stopped.is_set():
                        break
                    for filename in filenames:
                        path = Path(directory) / filename
                        if path.suffix.casefold() not in AUDIO_EXTENSIONS:
                            continue
                        resolved = path.resolve(strict=False)
                        if not resolved.is_relative_to(root):
                            continue
                        try:
                            item: tuple[Path, os.stat_result] | BaseException = (
                                resolved,
                                resolved.stat(),
                            )
                        except OSError as exc:
                            item = exc
                        asyncio.run_coroutine_threadsafe(queue.put(item), loop).result()
            except (OSError, RuntimeError) as exc:
                asyncio.run_coroutine_threadsafe(queue.put(exc), loop).result()
            finally:
                asyncio.run_coroutine_threadsafe(queue.put(None), loop).result()

        producer_task = asyncio.create_task(asyncio.to_thread(producer))
        batch: list[tuple[Path, os.stat_result]] = []
        current = run
        completed = True
        discard_remaining = False
        discovered = 0
        stale_cleanup_pending = True
        last_checkpoint = time.monotonic()
        last_log = last_checkpoint
        try:
            while True:
                try:
                    item = await asyncio.wait_for(queue.get(), timeout=0.25)
                except TimeoutError:
                    if not await checkpoint(run.id, scope.policy_revision):
                        completed = False
                        stopped.set()
                        discard_remaining = True
                    last_checkpoint = time.monotonic()
                    continue
                if item is None:
                    break
                if discard_remaining:
                    continue
                if isinstance(item, BaseException):
                    completed = False
                    stopped.set()
                    discard_remaining = True
                    continue
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
                    last_checkpoint = time.monotonic()
                elif time.monotonic() - last_checkpoint >= 0.25:
                    if not await checkpoint(run.id, scope.policy_revision):
                        completed = False
                        stopped.set()
                        discard_remaining = True
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
                    continue
            await asyncio.shield(producer_task)
            raise
        finally:
            stopped.set()
            if not producer_task.done():
                await producer_task
        if not completed:
            await self._store.complete_scan_scope_discovery(
                run.id,
                scope.root_id,
                scope.relative_path,
                state="partially_read",
                error_code="ROOT_PERMISSION_DENIED",
            )
        return current, completed

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
        for path, stat in batch:
            relative = PurePosixPath(*path.relative_to(root).parts).as_posix()
            raw.append((path, relative, stat, revision_from_stat(stat)))
        comparisons = await self._store.classify_scan_paths(
            scope.root_id,
            [
                (relative, stat.st_size, stat.st_mtime_ns, stat.st_mtime, revision)
                for _, relative, stat, revision in raw
            ],
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
        return msgspec.structs.replace(run, row_revision=revision, updated_at=updated_at)
