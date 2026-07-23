"""Durable, filesystem-safe cleanup of acquisition source materialization."""

from __future__ import annotations

import asyncio
import hashlib
import logging
import os
import re
import stat
import time
from collections.abc import Callable
from pathlib import Path

from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.serialization import to_jsonable
from infrastructure.service_health import service_health
from models.download_attempt import DownloadAttempt, DownloadCleanupReconciliation
from repositories.protocols.download_client import (
    DownloadClientProtocol,
    DownloadMaterialization,
    TaskHandle,
)

logger = logging.getLogger(__name__)

_JOB_NAME = re.compile(r"^droppedneedle-([0-9a-f]{32})-(0|[1-9][0-9]*)$")
_TERMINAL_CLEANABLE = frozenset({"completed", "partial", "cancelled"})
_ACTIVE_TASK_STATES = frozenset({"queued", "downloading", "processing"})
_HEALTH_SERVICE = "acquisition_cleanup"
_HEALTH_CAPABILITY = "source files"
_RECONCILIATION_BATCH = 100
_MAX_TREE_ENTRIES = 100_000
_ATTENTION_RECHECK_SECONDS = 3600.0
_UNRESOLVED_JOB_RETRIES = 4


class _RetryableCleanup(Exception):
    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


class _UnsafeCleanup(Exception):
    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


class AcquisitionCleanupService:
    """Consumes cleanup debt without changing the acquisition's terminal result."""

    def __init__(
        self,
        download_store: DownloadStore,
        library_store: NativeLibraryStore,
        client_getter: Callable[[str], DownloadClientProtocol],
        sab_mount_getter: Callable[[], Path],
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = download_store
        self._library_store = library_store
        self._client_getter = client_getter
        self._sab_mount_getter = sab_mount_getter
        self._clock = clock

    async def run_once(self, worker_id: str) -> int:
        attempts = await self._store.claim_download_cleanup_attempts(
            worker_id, now=self._clock(), limit=25, lease_seconds=300.0
        )
        for attempt in attempts:
            await self._process_claimed(attempt)
        await self._refresh_health()
        return len(attempts)

    async def cleanup_now(self, attempt_id: str, *, worker_id: str) -> bool:
        attempt = await self._store.claim_download_cleanup_attempt(
            attempt_id, worker_id, now=self._clock(), lease_seconds=300.0
        )
        if attempt is None:
            return False
        await self._process_claimed(attempt)
        await self._refresh_health()
        return True

    async def recover_startup(self) -> None:
        """Reconcile pre-journal SAB workspaces, then drain one cleanup page.

        This finishes before download resumption is registered. Active legacy jobs are
        journaled as ``in_use`` and are never deleted by reconciliation.
        """

        await self.reconcile_legacy_mount()
        await self.run_once("acquisition-cleanup-startup")

    async def _process_claimed(self, attempt: DownloadAttempt) -> None:
        try:
            if attempt.state == "needs_attention":
                await self._recheck_attention(attempt)
                return
            attempt = await self._check_publisher_barriers(attempt)
            if attempt is None or attempt.state not in {
                "cleanup_pending",
                "workspace_removed",
            }:
                return
            await self._cleanup_source(attempt)
        except _UnsafeCleanup as error:
            current = await self._store.get_download_attempt(attempt.id)
            if current is None:
                return
            updated = await self._store.transition_download_attempt(
                attempt.id,
                expected_row_revision=current.row_revision,
                new_state="needs_attention",
                now=self._clock(),
                disposition="preserve",
                error_code=error.code,
                lease_owner=None,
                lease_expires_at=None,
                next_retry_at=self._clock() + _ATTENTION_RECHECK_SECONDS,
                completed_at=self._clock(),
            )
            if updated is not None:
                logger.warning(
                    "Acquisition cleanup needs attention for attempt %s (%s)",
                    attempt.id,
                    error.code,
                )
        except _RetryableCleanup as error:
            current = await self._store.get_download_attempt(attempt.id)
            if current is None:
                return
            updated = await self._store.record_download_cleanup_failure(
                attempt.id,
                expected_row_revision=current.row_revision,
                error_code=error.code,
                now=self._clock(),
            )
            if updated is not None:
                logger.warning(
                    "Acquisition cleanup deferred for attempt %s (%s, failure %d)",
                    attempt.id,
                    error.code,
                    updated.cleanup_failures,
                )
        except Exception:  # noqa: BLE001 - a durable cleanup worker survives one item
            logger.exception("Acquisition cleanup iteration failed for %s", attempt.id)
            current = await self._store.get_download_attempt(attempt.id)
            if current is None:
                return
            await self._store.record_download_cleanup_failure(
                attempt.id,
                expected_row_revision=current.row_revision,
                error_code="cleanup_unexpected",
                now=self._clock(),
            )

    async def publisher_bundle_ids_for_task(self, task_id: str) -> list[str]:
        bundles = (
            await self._library_store.list_acquisition_import_bundles_for_download_task(
                task_id
            )
        )
        return [bundle.id for bundle in bundles]

    async def _recheck_attention(self, attempt: DownloadAttempt) -> None:
        bundles_resolved = True
        for bundle_id in attempt.publisher_bundle_ids:
            bundle = await self._library_store.get_library_management_import_bundle(
                bundle_id
            )
            if bundle is None or bundle.state != "completed":
                bundles_resolved = False
                break
        if not bundles_resolved:
            await self._defer_attention(attempt)
            return

        client = self._client_getter(attempt.source)
        try:
            materialization = await client.inspect_materialization(attempt.handle)
        except Exception:  # noqa: BLE001 - attention debt remains safely preserved
            await self._defer_attention(attempt)
            return
        if not materialization.mount_healthy:
            await self._defer_attention(attempt)
            return

        if await asyncio.to_thread(
            _attention_source_absent, attempt, materialization
        ):
            try:
                discarded = await client.discard_client_artifacts(attempt.handle)
            except Exception:  # noqa: BLE001 - attention debt remains safely preserved
                await self._defer_attention(attempt)
                return
            if discarded:
                await self._mark_complete(attempt)
                return

        if attempt.error_code in {
            "publisher_barrier_missing",
            "publisher_needs_attention",
        }:
            updated = await self._store.transition_download_attempt(
                attempt.id,
                expected_row_revision=attempt.row_revision,
                new_state="cleanup_pending",
                now=self._clock(),
                disposition="discard",
                next_retry_at=self._clock(),
                lease_owner=None,
                lease_expires_at=None,
                error_code=None,
                completed_at=None,
            )
            if updated is None:
                raise _RetryableCleanup("attempt_revision_changed")
            return
        await self._defer_attention(attempt)

    async def _defer_attention(self, attempt: DownloadAttempt) -> None:
        updated = await self._store.transition_download_attempt(
            attempt.id,
            expected_row_revision=attempt.row_revision,
            new_state="needs_attention",
            now=self._clock(),
            next_retry_at=self._clock() + _ATTENTION_RECHECK_SECONDS,
            lease_owner=None,
            lease_expires_at=None,
        )
        if updated is None:
            raise _RetryableCleanup("attempt_revision_changed")

    async def _check_publisher_barriers(
        self, attempt: DownloadAttempt
    ) -> DownloadAttempt | None:
        for bundle_id in attempt.publisher_bundle_ids:
            bundle = await self._library_store.get_library_management_import_bundle(
                bundle_id
            )
            if bundle is None:
                raise _UnsafeCleanup("publisher_barrier_missing")
            if bundle.state == "completed":
                continue
            if bundle.state in {"needs_attention", "rolled_back"}:
                raise _UnsafeCleanup("publisher_needs_attention")
            updated = await self._store.transition_download_attempt(
                attempt.id,
                expected_row_revision=attempt.row_revision,
                new_state=attempt.state,
                now=self._clock(),
                next_retry_at=self._clock() + 30.0,
                lease_owner=None,
                lease_expires_at=None,
                error_code="publisher_cleanup_pending",
            )
            if updated is None:
                raise _RetryableCleanup("attempt_revision_changed")
            return None
        return attempt

    async def _cleanup_source(self, attempt: DownloadAttempt) -> None:
        client = self._client_getter(attempt.source)
        if attempt.state == "workspace_removed":
            if not await client.discard_client_artifacts(attempt.handle):
                raise _RetryableCleanup("client_artifact_discard_failed")
            await self._mark_complete(attempt)
            return

        try:
            materialization = await client.inspect_materialization(attempt.handle)
        except Exception as error:  # noqa: BLE001 - repository errors stay internal
            raise _RetryableCleanup("materialization_inspection_failed") from error

        handle = attempt.handle
        if materialization.nzo_id and not handle.nzo_id:
            handle = TaskHandle(
                source=handle.source,
                username=handle.username,
                filenames=list(handle.filenames),
                job_name=handle.job_name,
                nzo_id=materialization.nzo_id,
            )
        attempt = await self._record_and_validate_evidence(
            attempt,
            handle=handle,
            remote_storage=materialization.remote_storage or None,
            mount_root=materialization.mount_root or None,
            workspace_path=materialization.workspace_path or None,
            materialized_paths=materialization.file_paths,
        )

        if materialization.state == "active":
            try:
                aborted = await client.abort(handle)
            except Exception as error:  # noqa: BLE001 - repository errors stay internal
                raise _RetryableCleanup("active_abort_failed") from error
            if not aborted:
                raise _RetryableCleanup("active_abort_failed")
            if attempt.source == "usenet":
                await self._mark_complete(attempt)
                return

        if attempt.source == "usenet":
            await self._cleanup_usenet_workspace(
                attempt,
                materialization.state,
                mount_healthy=materialization.mount_healthy,
            )
            return

        await self._cleanup_slskd_files(
            attempt,
            mount_healthy=materialization.mount_healthy,
        )
        try:
            discarded = await client.discard_client_artifacts(handle)
        except Exception as error:  # noqa: BLE001 - repository errors stay internal
            raise _RetryableCleanup("client_artifact_discard_failed") from error
        if not discarded:
            raise _RetryableCleanup("client_artifact_discard_failed")
        await self._mark_complete(attempt)

    async def _record_and_validate_evidence(
        self,
        attempt: DownloadAttempt,
        *,
        handle: TaskHandle,
        remote_storage: str | None,
        mount_root: str | None,
        workspace_path: str | None,
        materialized_paths: list[str],
    ) -> DownloadAttempt:
        for persisted, fresh in (
            (attempt.remote_storage, remote_storage),
            (attempt.mount_root, mount_root),
            (attempt.workspace_path, workspace_path),
        ):
            if persisted and fresh and _absolute(persisted) != _absolute(fresh):
                raise _UnsafeCleanup("materialization_evidence_conflict")
        if attempt.materialized_paths and materialized_paths:
            persisted_paths = {
                _absolute(value) for value in attempt.materialized_paths
            }
            fresh_paths = {_absolute(value) for value in materialized_paths}
            valid_paths = (
                fresh_paths.issubset(persisted_paths)
                if attempt.source == "soulseek"
                else fresh_paths == persisted_paths
            )
            if not valid_paths:
                raise _UnsafeCleanup("materialization_evidence_conflict")
        updated = await self._store.transition_download_attempt(
            attempt.id,
            expected_row_revision=attempt.row_revision,
            new_state=attempt.state,
            now=self._clock(),
            handle_json=_json(to_jsonable(handle)),
            remote_storage=attempt.remote_storage or remote_storage,
            mount_root=attempt.mount_root or mount_root,
            workspace_path=attempt.workspace_path or workspace_path,
            materialized_paths_json=_json(
                attempt.materialized_paths or materialized_paths
            ),
        )
        if updated is None:
            raise _RetryableCleanup("attempt_revision_changed")
        return updated

    async def _cleanup_usenet_workspace(
        self,
        attempt: DownloadAttempt,
        materialization_state: str,
        *,
        mount_healthy: bool,
    ) -> None:
        client = self._client_getter("usenet")
        if materialization_state == "failed":
            try:
                discarded = await client.discard_client_artifacts(attempt.handle)
            except Exception as error:  # noqa: BLE001 - repository errors stay internal
                raise _RetryableCleanup("client_artifact_discard_failed") from error
            if not discarded:
                raise _RetryableCleanup("client_artifact_discard_failed")
            await self._mark_complete(attempt)
            return

        if not mount_healthy:
            raise _RetryableCleanup("mount_unavailable")
        if not attempt.mount_root:
            raise _UnsafeCleanup("mount_evidence_missing")
        if materialization_state == "missing" and not attempt.legacy_reconciled:
            if not attempt.workspace_path:
                if not attempt.handle.nzo_id and (
                    attempt.cleanup_failures < _UNRESOLVED_JOB_RETRIES
                ):
                    raise _RetryableCleanup("client_job_not_materialized")
                await self._mark_complete(attempt)
                return
            exists = await asyncio.to_thread(
                _path_exists_without_following, Path(attempt.workspace_path)
            )
            if exists:
                raise _UnsafeCleanup("fresh_workspace_evidence_missing")
            await self._mark_complete(attempt)
            return
        if not attempt.workspace_path:
            raise _UnsafeCleanup("workspace_evidence_missing")
        _validate_job_identity(attempt)
        try:
            await asyncio.to_thread(
                _remove_workspace_safely,
                Path(attempt.mount_root),
                Path(attempt.workspace_path),
            )
        except _UnsafeCleanup:
            raise
        except OSError as error:
            raise _RetryableCleanup("workspace_remove_failed") from error

        removed = await self._store.transition_download_attempt(
            attempt.id,
            expected_row_revision=attempt.row_revision,
            new_state="workspace_removed",
            now=self._clock(),
            lease_owner=attempt.lease_owner,
            lease_expires_at=attempt.lease_expires_at,
            error_code=None,
        )
        if removed is None:
            raise _RetryableCleanup("attempt_revision_changed")
        try:
            discarded = await client.discard_client_artifacts(removed.handle)
        except Exception as error:  # noqa: BLE001 - repository errors stay internal
            raise _RetryableCleanup("client_artifact_discard_failed") from error
        if not discarded:
            raise _RetryableCleanup("client_artifact_discard_failed")
        await self._mark_complete(removed)

    async def _cleanup_slskd_files(
        self, attempt: DownloadAttempt, *, mount_healthy: bool
    ) -> None:
        if not mount_healthy:
            raise _RetryableCleanup("mount_unavailable")
        if not attempt.mount_root:
            raise _UnsafeCleanup("mount_evidence_missing")
        try:
            for source in attempt.materialized_paths:
                await asyncio.to_thread(
                    _unlink_file_safely, Path(attempt.mount_root), Path(source)
                )
        except _UnsafeCleanup:
            raise
        except OSError as error:
            raise _RetryableCleanup("source_file_remove_failed") from error

    async def _mark_complete(self, attempt: DownloadAttempt) -> None:
        updated = await self._store.transition_download_attempt(
            attempt.id,
            expected_row_revision=attempt.row_revision,
            new_state="complete",
            now=self._clock(),
            lease_owner=None,
            lease_expires_at=None,
            error_code=None,
            completed_at=self._clock(),
        )
        if updated is None:
            raise _RetryableCleanup("attempt_revision_changed")

    async def _refresh_health(self) -> None:
        count = await self._store.cleanup_warning_count()
        if count:
            service_health.mark_degraded(
                _HEALTH_SERVICE,
                _HEALTH_CAPABILITY,
                severity="degraded",
                message=(
                    f"Source cleanup needs attention for {count} "
                    f"download{'s' if count != 1 else ''}."
                ),
                ttl_seconds=90.0,
            )
        else:
            service_health.heal(_HEALTH_SERVICE, _HEALTH_CAPABILITY)

    async def reconcile_legacy_mount(
        self, *, limit: int = _RECONCILIATION_BATCH
    ) -> int:
        mount = Path(self._sab_mount_getter())
        if not mount.is_absolute() or mount == Path(mount.anchor):
            logger.warning("Skipped acquisition cleanup reconciliation for unsafe mount")
            return 0
        mount_key = hashlib.sha256(_absolute(str(mount)).encode()).hexdigest()
        progress = await self._store.ensure_cleanup_reconciliation(
            mount_key, str(mount), now=self._clock()
        )
        if progress.completed:
            return 0
        if not await asyncio.to_thread(_mount_healthy, mount):
            return 0

        processed = 0
        while processed < max(1, limit):
            if progress.current_directory is None:
                if not progress.pending_directories:
                    progress.completed = True
                    break
                progress.current_directory = progress.pending_directories.pop(0)
                progress.last_entry = None
            current = mount / progress.current_directory
            entries = await asyncio.to_thread(_directory_entries, current)
            remaining = [
                entry
                for entry in entries
                if progress.last_entry is None or entry[0] > progress.last_entry
            ]
            if not remaining:
                progress.current_directory = None
                progress.last_entry = None
                continue
            for name, is_directory, is_symlink in remaining:
                progress.last_entry = name
                processed += 1
                relative = (
                    Path(name)
                    if progress.current_directory == "."
                    else Path(progress.current_directory) / name
                )
                match = _JOB_NAME.fullmatch(name)
                if match and (is_directory or is_symlink):
                    await self._reconcile_legacy_job(
                        mount,
                        relative,
                        match,
                        is_symlink=is_symlink,
                    )
                elif is_directory and not is_symlink:
                    progress.pending_directories.append(relative.as_posix())
                if processed >= max(1, limit):
                    break

        await self._store.save_cleanup_reconciliation(progress, now=self._clock())
        await self._refresh_health()
        return processed

    async def _reconcile_legacy_job(
        self,
        mount: Path,
        relative: Path,
        match: re.Match[str],
        *,
        is_symlink: bool,
    ) -> None:
        task_id, candidate_text = match.groups()
        job_name = relative.name
        if await self._store.get_download_attempt_for_job("usenet", job_name):
            return
        task = await self._store.get_task(task_id)
        bundles = (
            await self._library_store.list_acquisition_import_bundles_for_download_task(
                task_id
            )
        )
        bundle_ids = [bundle.id for bundle in bundles]
        state = "needs_attention"
        disposition = "preserve"
        error_code: str | None = "legacy_workspace_ambiguous"
        if is_symlink:
            error_code = "legacy_workspace_symlink"
        elif task is None:
            error_code = "legacy_unknown_task"
        elif task.status in _ACTIVE_TASK_STATES:
            state = "in_use"
            disposition = "undecided"
            error_code = None
        elif task.status not in _TERMINAL_CLEANABLE:
            error_code = "legacy_failed_task"
        elif any(
            bundle.state in {"needs_attention", "rolled_back"} for bundle in bundles
        ):
            error_code = "publisher_needs_attention"
        else:
            state = "cleanup_pending"
            disposition = "discard"
            error_code = None
        attempt_key = hashlib.sha256(
            f"{_absolute(str(mount))}\0{relative.as_posix()}".encode()
        ).hexdigest()
        await self._store.ensure_legacy_download_attempt(
            attempt_id=attempt_key,
            task_id=task_id,
            candidate_index=int(candidate_text),
            job_name=job_name,
            mount_root=str(mount),
            workspace_path=str(mount / relative),
            state=state,
            disposition=disposition,
            error_code=error_code,
            publisher_bundle_ids=bundle_ids,
            now=self._clock(),
        )


def _json(value: object) -> str:
    import json

    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))


def _absolute(value: str) -> str:
    return os.path.abspath(value)


def _validate_job_identity(attempt: DownloadAttempt) -> None:
    match = _JOB_NAME.fullmatch(attempt.job_name)
    if match is None:
        raise _UnsafeCleanup("job_identity_invalid")
    task_id, candidate = match.groups()
    if task_id != attempt.task_id or int(candidate) != attempt.candidate_index:
        raise _UnsafeCleanup("job_identity_conflict")
    if Path(attempt.workspace_path or "").name != attempt.job_name:
        raise _UnsafeCleanup("workspace_identity_conflict")


def _mount_healthy(root: Path) -> bool:
    try:
        if root.is_symlink() or not root.is_dir():
            return False
        next(root.iterdir(), None)
        return True
    except OSError:
        return False


def _path_exists_without_following(path: Path) -> bool:
    try:
        path.lstat()
        return True
    except FileNotFoundError:
        return False


def _attention_source_absent(
    attempt: DownloadAttempt, materialization: DownloadMaterialization
) -> bool:
    root_value = attempt.mount_root or materialization.mount_root
    if not root_value:
        return materialization.state == "missing"
    root = Path(root_value)
    if not _mount_healthy(root) or materialization.state == "active":
        return False
    if attempt.source == "usenet":
        candidates = {
            value
            for value in (attempt.workspace_path, materialization.workspace_path)
            if value
        }
    else:
        candidates = {
            *attempt.materialized_paths,
            *materialization.file_paths,
        }
    if not candidates:
        return materialization.state == "missing"
    for value in candidates:
        try:
            _confined_parts(root, Path(value))
        except _UnsafeCleanup:
            return False
        if _path_exists_without_following(Path(value)):
            return False
    return True


def _confined_parts(root: Path, target: Path) -> tuple[Path, tuple[str, ...]]:
    root_absolute = Path(_absolute(str(root)))
    target_absolute = Path(_absolute(str(target)))
    if root_absolute == Path(root_absolute.anchor):
        raise _UnsafeCleanup("mount_root_unsafe")
    cursor = Path(root_absolute.anchor)
    for component in root_absolute.parts[1:]:
        cursor /= component
        if cursor.is_symlink():
            raise _UnsafeCleanup("mount_path_symlink")
    try:
        relative = target_absolute.relative_to(root_absolute)
    except ValueError as error:
        raise _UnsafeCleanup("path_outside_mount") from error
    if not relative.parts or relative == Path("."):
        raise _UnsafeCleanup("mount_root_refused")
    if any(part in {"", ".", ".."} for part in relative.parts):
        raise _UnsafeCleanup("path_component_invalid")
    return root_absolute, relative.parts


def _open_root(root: Path) -> int:
    current_fd = os.open(root.anchor, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for component in root.parts[1:]:
            next_fd = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=current_fd,
            )
            os.close(current_fd)
            current_fd = next_fd
        return current_fd
    except BaseException:
        os.close(current_fd)
        raise


def _open_parent(root: Path, parts: tuple[str, ...]) -> tuple[int, int, str]:
    root_fd = _open_root(root)
    current_fd = root_fd
    try:
        for component in parts[:-1]:
            next_fd = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=current_fd,
            )
            if current_fd != root_fd:
                os.close(current_fd)
            current_fd = next_fd
        return root_fd, current_fd, parts[-1]
    except BaseException:
        if current_fd != root_fd:
            os.close(current_fd)
        os.close(root_fd)
        raise


def _unlink_file_safely(root: Path, target: Path) -> None:
    root, parts = _confined_parts(root, target)
    if not _mount_healthy(root):
        raise OSError("mount unavailable")
    try:
        root_fd, parent_fd, name = _open_parent(root, parts)
    except FileNotFoundError:
        return
    try:
        try:
            info = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
        except FileNotFoundError:
            return
        if stat.S_ISLNK(info.st_mode):
            raise _UnsafeCleanup("source_file_symlink")
        if not stat.S_ISREG(info.st_mode):
            raise _UnsafeCleanup("source_file_not_regular")
        os.unlink(name, dir_fd=parent_fd)
    finally:
        if parent_fd != root_fd:
            os.close(parent_fd)
        os.close(root_fd)


def _remove_workspace_safely(root: Path, target: Path) -> None:
    root, parts = _confined_parts(root, target)
    if not _mount_healthy(root):
        raise OSError("mount unavailable")
    try:
        root_fd, parent_fd, name = _open_parent(root, parts)
    except FileNotFoundError:
        return
    try:
        try:
            info = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
        except FileNotFoundError:
            return
        if stat.S_ISLNK(info.st_mode):
            raise _UnsafeCleanup("workspace_symlink")
        if not stat.S_ISDIR(info.st_mode):
            raise _UnsafeCleanup("workspace_not_directory")
        workspace_fd = os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=parent_fd,
        )
        try:
            _validate_tree(workspace_fd, [0])
            _remove_tree_contents(workspace_fd)
        finally:
            os.close(workspace_fd)
        os.rmdir(name, dir_fd=parent_fd)
    finally:
        if parent_fd != root_fd:
            os.close(parent_fd)
        os.close(root_fd)


def _validate_tree(directory_fd: int, seen: list[int]) -> None:
    with os.scandir(directory_fd) as entries:
        names = [entry.name for entry in entries]
    for name in names:
        seen[0] += 1
        if seen[0] > _MAX_TREE_ENTRIES:
            raise _UnsafeCleanup("workspace_too_large")
        info = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        if stat.S_ISLNK(info.st_mode):
            raise _UnsafeCleanup("workspace_contains_symlink")
        if stat.S_ISDIR(info.st_mode):
            child = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=directory_fd,
            )
            try:
                _validate_tree(child, seen)
            finally:
                os.close(child)


def _remove_tree_contents(directory_fd: int) -> None:
    with os.scandir(directory_fd) as entries:
        names = [entry.name for entry in entries]
    for name in names:
        info = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        if stat.S_ISDIR(info.st_mode):
            child = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=directory_fd,
            )
            try:
                _remove_tree_contents(child)
            finally:
                os.close(child)
            os.rmdir(name, dir_fd=directory_fd)
        else:
            os.unlink(name, dir_fd=directory_fd)


def _directory_entries(path: Path) -> list[tuple[str, bool, bool]]:
    with os.scandir(path) as entries:
        result = [
            (
                entry.name,
                entry.is_dir(follow_symlinks=False),
                entry.is_symlink(),
            )
            for entry in entries
        ]
    return sorted(result, key=lambda value: value[0])
