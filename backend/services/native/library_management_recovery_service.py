"""Bounded, idempotent recovery for durable Library Management journals."""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import shutil
import stat
import time
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Literal

from core.exceptions import ConflictError, StaleRevisionError, ValidationError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_management import (
    LibraryFileMutationJournal,
    LibraryManagementJobSnapshot,
    LibraryManagementPlanItem,
)
from models.library_management_planning import PinnedLibraryManagementProfile
from services.native.library_filesystem_coordinator import (
    LibraryFilesystemCoordinator,
)
from services.native.library_management_publisher import LibraryManagementPublisher

logger = logging.getLogger(__name__)

RECOVERY_LEASE_SECONDS = 60.0
RECOVERY_BATCH_SIZE = 100
STARTUP_RECOVERY_MAX_BUNDLES = 500

RecoveryDisposition = Literal["recovered", "rolled_back", "needs_attention", "skipped"]


@dataclass(frozen=True)
class LibraryManagementRecoveryRun:
    examined_bundles: int = 0
    recovered_bundles: int = 0
    rolled_back_bundles: int = 0
    needs_attention_bundles: int = 0
    skipped_bundles: int = 0


@dataclass(frozen=True)
class _FileEvidence:
    kind: Literal["missing", "regular", "symlink", "other", "error"]
    fingerprint: str | None = None
    error: str | None = None

    def exact(self, expected: str | None) -> bool:
        return (
            expected is not None
            and self.kind == "regular"
            and self.fingerprint == expected
        )


@dataclass(frozen=True)
class _JournalPaths:
    journal: LibraryFileMutationJournal
    source: Path | None
    temporary: Path | None
    backup: Path | None
    destination: Path


class _RecoveryUncertainError(Exception):
    def __init__(self, reason: str, evidence: dict[str, object]) -> None:
        super().__init__(reason)
        self.reason = reason
        self.evidence = evidence


class LibraryManagementRecoveryService:
    """Recover album bundles without trusting path existence as proof of ownership."""

    def __init__(
        self,
        store: NativeLibraryStore,
        publisher: LibraryManagementPublisher,
        filesystem: LibraryFilesystemCoordinator,
        *,
        clock=time.time,
    ) -> None:
        self._store = store
        self._publisher = publisher
        self._filesystem = filesystem
        self._clock = clock

    @staticmethod
    async def _finish_critical_task(
        task: asyncio.Task[RecoveryDisposition],
    ) -> tuple[RecoveryDisposition, bool]:
        cancelled = False
        while not task.done():
            try:
                await asyncio.shield(task)
            except asyncio.CancelledError:
                cancelled = True
        return task.result(), cancelled

    async def recover_startup(
        self, *, limit: int = RECOVERY_BATCH_SIZE
    ) -> LibraryManagementRecoveryRun:
        """Drain bounded pages before startup, ignoring dead process leases."""

        if limit < 1 or limit > RECOVERY_BATCH_SIZE:
            raise ValidationError("Startup recovery page size is out of range.")
        totals = LibraryManagementRecoveryRun()
        while totals.examined_bundles < STARTUP_RECOVERY_MAX_BUNDLES:
            current = await self.recover_once(
                limit=min(
                    limit,
                    STARTUP_RECOVERY_MAX_BUNDLES - totals.examined_bundles,
                ),
                force_expired_process_leases=True,
                include_committed_imports=False,
            )
            totals = LibraryManagementRecoveryRun(
                examined_bundles=(totals.examined_bundles + current.examined_bundles),
                recovered_bundles=(
                    totals.recovered_bundles + current.recovered_bundles
                ),
                rolled_back_bundles=(
                    totals.rolled_back_bundles + current.rolled_back_bundles
                ),
                needs_attention_bundles=(
                    totals.needs_attention_bundles + current.needs_attention_bundles
                ),
                skipped_bundles=totals.skipped_bundles + current.skipped_bundles,
            )
            if current.examined_bundles == 0:
                break
        remaining_manual = await self._store.list_recoverable_management_bundles(
            limit=1
        )
        remaining_imports = (
            await self._store.list_recoverable_library_management_import_bundles(
                limit=1, include_committed_cleanup=False
            )
        )
        if remaining_manual or remaining_imports:
            raise ConflictError(
                "Library Management recovery did not reach a safe startup boundary."
            )
        return totals

    async def recover_once(
        self,
        *,
        limit: int = RECOVERY_BATCH_SIZE,
        force_expired_process_leases: bool = False,
        include_committed_imports: bool = True,
    ) -> LibraryManagementRecoveryRun:
        counts = {
            "recovered": 0,
            "rolled_back": 0,
            "needs_attention": 0,
            "skipped": 0,
        }
        imports = await self._store.list_recoverable_library_management_import_bundles(
            limit=limit,
            include_committed_cleanup=include_committed_imports,
        )
        for record in imports:
            try:
                disposition = await self._publisher.recover_import_bundle(record)
            except asyncio.CancelledError:
                raise
            except Exception:  # noqa: BLE001 - one corrupt import cannot stop recovery
                logger.exception(
                    "Library Management import recovery failed for %s", record.id
                )
                await self._store.mark_library_management_import_needs_attention(
                    record.id,
                    failure_code="RECOVERY_INTERNAL_ERROR",
                    updated_at=self._clock(),
                )
                disposition = "needs_attention"
            counts[disposition] += 1
        bundles = (
            await self._store.list_recoverable_management_bundles(
                limit=limit - len(imports)
            )
            if len(imports) < limit
            else []
        )
        for bundle in bundles:
            job_id = str(bundle["job_id"])
            bundle_ordinal = int(bundle["bundle_ordinal"])
            try:
                disposition = await self._recover_bundle(
                    job_id,
                    bundle_ordinal,
                    force_expired_process_leases=force_expired_process_leases,
                )
            except asyncio.CancelledError:
                raise
            except Exception:  # noqa: BLE001 - one corrupt bundle cannot stop recovery
                logger.exception(
                    "Library Management recovery failed for bundle %s/%s",
                    job_id,
                    bundle_ordinal,
                )
                disposition = await self._attention_bundle(
                    job_id,
                    bundle_ordinal,
                    "RECOVERY_INTERNAL_ERROR",
                    {"reason": "RECOVERY_INTERNAL_ERROR"},
                )
            counts[disposition] += 1
        return LibraryManagementRecoveryRun(
            examined_bundles=len(imports) + len(bundles),
            recovered_bundles=counts["recovered"],
            rolled_back_bundles=counts["rolled_back"],
            needs_attention_bundles=counts["needs_attention"],
            skipped_bundles=counts["skipped"],
        )

    async def diagnostics(self) -> dict[str, object]:
        return await self._store.library_management_recovery_diagnostics()

    async def _recover_bundle(
        self,
        job_id: str,
        bundle_ordinal: int,
        *,
        force_expired_process_leases: bool,
    ) -> RecoveryDisposition:
        journals = await self._store.list_file_mutation_journals_for_bundle(
            job_id, bundle_ordinal
        )
        active = [
            journal
            for journal in journals
            if journal.state not in {"completed", "rolled_back", "needs_attention"}
        ]
        if not active:
            return "skipped"
        snapshot = await self._store.get_library_management_job_snapshot(job_id)
        items = await self._store.get_library_management_bundle_plan_items(
            job_id, bundle_ordinal
        )
        if snapshot is None or not items:
            return await self._attention_bundle(
                job_id,
                bundle_ordinal,
                "RECOVERY_DURABLE_STATE_MISSING",
                {"reason": "RECOVERY_DURABLE_STATE_MISSING"},
            )

        committed = {
            "catalog_committed",
            "cleanup_pending",
            "completed",
        }
        precommit = {
            "planned",
            "snapshot_saved",
            "staged",
            "validated",
            "source_backed_up",
            "published",
            "rollback_pending",
            "rolled_back",
        }
        states = {journal.state for journal in journals}
        if states & committed:
            if states - committed:
                return await self._attention_bundle(
                    job_id,
                    bundle_ordinal,
                    "RECOVERY_MIXED_COMMIT_STATE",
                    {"reason": "RECOVERY_MIXED_COMMIT_STATE", "states": sorted(states)},
                )
            return await self._recover_committed_bundle(
                snapshot, items, journals, bundle_ordinal
            )
        if not states <= precommit:
            return await self._attention_bundle(
                job_id,
                bundle_ordinal,
                "RECOVERY_UNKNOWN_STATE",
                {"reason": "RECOVERY_UNKNOWN_STATE", "states": sorted(states)},
            )
        if states & {"rollback_pending", "rolled_back"}:
            return await self._compensate_bundle(
                snapshot,
                journals,
                bundle_ordinal,
                reason="RECOVERY_RESUME_COMPENSATION",
            )

        job = await self._store.get_operation_job(job_id)
        if job is None or str(job["state"]) in {
            "paused",
            "stopped",
            "failed",
            "cancelled",
            "succeeded",
        }:
            return await self._compensate_bundle(
                snapshot,
                journals,
                bundle_ordinal,
                reason="RECOVERY_OPERATION_NOT_RESUMABLE",
            )
        claimed = await self._store.claim_management_bundle_for_recovery(
            job_id,
            bundle_ordinal,
            self._worker_id(),
            now=self._clock(),
            lease_seconds=RECOVERY_LEASE_SECONDS,
            force_expired_process_lease=force_expired_process_leases,
        )
        if not claimed:
            if force_expired_process_leases:
                return await self._attention_bundle(
                    job_id,
                    bundle_ordinal,
                    "RECOVERY_OPERATION_STATE_MISMATCH",
                    {"reason": "RECOVERY_OPERATION_STATE_MISMATCH"},
                )
            return "skipped"

        early_states = {"planned", "snapshot_saved", "staged", "validated"}
        if states <= early_states and states != {"validated"}:
            return await self._resume_prepared_bundle(
                snapshot, journals, bundle_ordinal
            )
        return await self._finish_published_bundle(
            snapshot, items, journals, bundle_ordinal
        )

    async def _resume_prepared_bundle(
        self,
        snapshot: LibraryManagementJobSnapshot,
        journals: list[LibraryFileMutationJournal],
        bundle_ordinal: int,
    ) -> RecoveryDisposition:
        try:
            await self._publisher.publish_bundle(
                snapshot.job_id, bundle_ordinal, self._worker_id()
            )
        except (OSError, ConflictError, StaleRevisionError, ValidationError) as error:
            logger.warning(
                "Library Management recovery rolled back prepared bundle %s/%s: %s",
                snapshot.job_id,
                bundle_ordinal,
                type(error).__name__,
            )
            current = await self._store.list_file_mutation_journals_for_bundle(
                snapshot.job_id, bundle_ordinal
            )
            return await self._compensate_bundle(
                snapshot,
                current or journals,
                bundle_ordinal,
                reason="RECOVERY_RESUME_STALE",
            )
        await self._store.release_management_recovery_lease(
            snapshot.job_id, self._worker_id(), now=self._clock()
        )
        return "recovered"

    async def _finish_published_bundle(
        self,
        snapshot: LibraryManagementJobSnapshot,
        items: list[LibraryManagementPlanItem],
        journals: list[LibraryFileMutationJournal],
        bundle_ordinal: int,
    ) -> RecoveryDisposition:
        try:
            pinned, roots = self._publisher.recovery_configuration(snapshot)
            self._validate_complete_prepared_bundle(items, journals)
            paths = self._resolve_paths(journals, roots)
        except (OSError, ConflictError, StaleRevisionError, ValidationError) as error:
            return await self._compensate_bundle(
                snapshot,
                journals,
                bundle_ordinal,
                reason=f"RECOVERY_CONFIGURATION_{type(error).__name__.upper()}",
            )

        async def critical() -> RecoveryDisposition:
            try:
                async with self._filesystem.write_many(self._affected_roots(journals)):
                    current = await self._publish_remaining(paths)
                    item_by_ordinal = {item.ordinal: item for item in items}
                    mutations = []
                    for journal_path in current:
                        journal = journal_path.journal
                        if journal.subject_kind != "audio":
                            continue
                        mutations.append(
                            await self._publisher.build_recovery_catalog_mutation(
                                item_by_ordinal[journal.plan_item_ordinal],
                                journal,
                                pinned,
                                journal_path.destination,
                                profile_revision=snapshot.profile_revision,
                                naming_revision=snapshot.naming_revision,
                            )
                        )
                    await self._store.commit_library_management_bundle(
                        snapshot.job_id,
                        bundle_ordinal,
                        self._worker_id(),
                        mutations,
                        now=self._clock(),
                    )
                    committed = (
                        await self._store.list_file_mutation_journals_for_bundle(
                            snapshot.job_id, bundle_ordinal
                        )
                    )
                    committed_paths = self._resolve_paths(committed, roots)
                    await self._cleanup_committed_locked(
                        snapshot, committed_paths, pinned, roots
                    )
                await self._store.release_management_recovery_lease(
                    snapshot.job_id, self._worker_id(), now=self._clock()
                )
                return "recovered"
            except _RecoveryUncertainError as error:
                return await self._attention_bundle(
                    snapshot.job_id,
                    bundle_ordinal,
                    error.reason,
                    error.evidence,
                )
            except (OSError, ConflictError, StaleRevisionError, ValidationError):
                current = await self._store.list_file_mutation_journals_for_bundle(
                    snapshot.job_id, bundle_ordinal
                )
                return await self._compensate_bundle(
                    snapshot,
                    current,
                    bundle_ordinal,
                    reason="RECOVERY_COMMIT_REJECTED",
                )

        task = asyncio.create_task(critical())
        result, cancelled = await self._finish_critical_task(task)
        if cancelled:
            raise asyncio.CancelledError
        return result

    async def _recover_committed_bundle(
        self,
        snapshot: LibraryManagementJobSnapshot,
        items: list[LibraryManagementPlanItem],
        journals: list[LibraryFileMutationJournal],
        bundle_ordinal: int,
    ) -> RecoveryDisposition:
        del items
        try:
            pinned, roots = self._publisher.recovery_filesystem_configuration(snapshot)
            paths = self._resolve_paths(journals, roots)
        except (OSError, ConflictError, StaleRevisionError, ValidationError) as error:
            return await self._attention_bundle(
                snapshot.job_id,
                bundle_ordinal,
                "RECOVERY_COMMITTED_CONFIGURATION_STALE",
                {
                    "reason": "RECOVERY_COMMITTED_CONFIGURATION_STALE",
                    "error_type": type(error).__name__,
                },
            )

        async def critical() -> RecoveryDisposition:
            try:
                async with self._filesystem.write_many(self._affected_roots(journals)):
                    await self._cleanup_committed_locked(snapshot, paths, pinned, roots)
                await self._store.settle_management_recovery_job(
                    snapshot.job_id, now=self._clock()
                )
                return "recovered"
            except _RecoveryUncertainError as error:
                if error.reason in {
                    "RECOVERY_COMMITTED_DESTINATION_MISSING",
                    "RECOVERY_COMMITTED_DESTINATION_CHANGED",
                }:
                    await self._mark_missing_catalog_destinations(paths)
                return await self._attention_bundle(
                    snapshot.job_id,
                    bundle_ordinal,
                    error.reason,
                    error.evidence,
                )

        task = asyncio.create_task(critical())
        result, cancelled = await self._finish_critical_task(task)
        if cancelled:
            raise asyncio.CancelledError
        return result

    async def _publish_remaining(
        self, paths: list[_JournalPaths]
    ) -> list[_JournalPaths]:
        await asyncio.to_thread(self._preflight_publish, paths)
        current: list[_JournalPaths] = []
        for value in paths:
            journal = value.journal
            delete_only = self._is_delete_mutation(journal)
            recycle_move = self._is_recycle_mutation(journal)
            if journal.state == "validated":
                if value.source == value.destination or recycle_move:
                    assert value.backup is not None and value.source is not None
                    source = await asyncio.to_thread(self._inspect, value.source)
                    backup = await asyncio.to_thread(self._inspect, value.backup)
                    if source.exact(journal.source_fingerprint):
                        await asyncio.to_thread(os.replace, value.source, value.backup)
                    elif not (
                        source.kind == "missing"
                        and backup.exact(journal.source_fingerprint)
                    ):
                        raise _RecoveryUncertainError(
                            "RECOVERY_SOURCE_OR_BACKUP_CHANGED",
                            {"journal_id": journal.id},
                        )
                    journal = await self._store.transition_file_mutation_journal(
                        journal.id,
                        expected_state="validated",
                        new_state="source_backed_up",
                        expected_row_revision=journal.row_revision,
                        updated_at=self._clock(),
                        increment_attempts=True,
                    )
                if delete_only:
                    journal = await self._store.transition_file_mutation_journal(
                        journal.id,
                        expected_state=journal.state,
                        new_state="published",
                        expected_row_revision=journal.row_revision,
                        updated_at=self._clock(),
                    )
                    current.append(
                        _JournalPaths(
                            journal=journal,
                            source=value.source,
                            temporary=value.temporary,
                            backup=value.backup,
                            destination=value.destination,
                        )
                    )
                    continue
                temporary = await asyncio.to_thread(self._inspect, value.temporary)
                destination = await asyncio.to_thread(self._inspect, value.destination)
                if destination.exact(journal.staged_fingerprint):
                    if temporary.kind != "missing":
                        raise _RecoveryUncertainError(
                            "RECOVERY_PUBLISH_FINGERPRINT_AMBIGUOUS",
                            {"journal_id": journal.id},
                        )
                else:
                    assert value.temporary is not None
                    await asyncio.to_thread(
                        os.replace, value.temporary, value.destination
                    )
                journal = await self._store.transition_file_mutation_journal(
                    journal.id,
                    expected_state=journal.state,
                    new_state="published",
                    expected_row_revision=journal.row_revision,
                    updated_at=self._clock(),
                )
            elif journal.state == "source_backed_up":
                if not delete_only:
                    destination = await asyncio.to_thread(
                        self._inspect, value.destination
                    )
                    if destination.kind == "missing":
                        assert value.temporary is not None
                        await asyncio.to_thread(
                            os.replace, value.temporary, value.destination
                        )
                journal = await self._store.transition_file_mutation_journal(
                    journal.id,
                    expected_state="source_backed_up",
                    new_state="published",
                    expected_row_revision=journal.row_revision,
                    updated_at=self._clock(),
                    increment_attempts=True,
                )
            current.append(
                _JournalPaths(
                    journal=journal,
                    source=value.source,
                    temporary=value.temporary,
                    backup=value.backup,
                    destination=value.destination,
                )
            )
        await asyncio.to_thread(self._fsync_directories, current)
        return current

    def _preflight_publish(self, paths: list[_JournalPaths]) -> None:
        for value in paths:
            journal = value.journal
            source, temporary, backup, destination = self._inspect_paths(value)
            evidence = self._evidence(value, source, temporary, backup, destination)
            same_path = value.source is not None and value.source == value.destination
            if self._is_delete_mutation(journal):
                if not same_path or not temporary.exact(journal.staged_fingerprint):
                    raise _RecoveryUncertainError(
                        "RECOVERY_DELETE_EVIDENCE_CHANGED", evidence
                    )
                before_backup = (
                    source.exact(journal.source_fingerprint)
                    and backup.kind == "missing"
                )
                after_backup = source.kind == "missing" and backup.exact(
                    journal.source_fingerprint
                )
                if journal.state == "validated" and not (before_backup or after_backup):
                    raise _RecoveryUncertainError(
                        "RECOVERY_SOURCE_OR_BACKUP_CHANGED", evidence
                    )
                if journal.state in {"source_backed_up", "published"} and not (
                    source.kind == "missing"
                    and backup.exact(journal.source_fingerprint)
                ):
                    raise _RecoveryUncertainError("RECOVERY_BACKUP_CHANGED", evidence)
                continue
            if self._is_recycle_mutation(journal):
                if temporary.kind in {"symlink", "other", "error"}:
                    raise _RecoveryUncertainError(
                        "RECOVERY_STAGED_OUTPUT_CHANGED", evidence
                    )
                before_backup = source.exact(journal.source_fingerprint) and (
                    backup.kind == "missing"
                )
                after_backup = source.kind == "missing" and backup.exact(
                    journal.source_fingerprint
                )
                if journal.state == "validated" and not (before_backup or after_backup):
                    raise _RecoveryUncertainError(
                        "RECOVERY_SOURCE_OR_BACKUP_CHANGED", evidence
                    )
                if journal.state in {"source_backed_up", "published"} and not (
                    after_backup
                ):
                    raise _RecoveryUncertainError("RECOVERY_BACKUP_CHANGED", evidence)
                before_publish = temporary.exact(journal.staged_fingerprint) and (
                    destination.kind == "missing"
                )
                after_publish = temporary.kind == "missing" and destination.exact(
                    journal.staged_fingerprint
                )
                if not (before_publish or after_publish):
                    raise _RecoveryUncertainError(
                        "RECOVERY_PUBLISH_FINGERPRINT_AMBIGUOUS", evidence
                    )
                continue
            if journal.state == "validated":
                if same_path:
                    before_backup = (
                        source.exact(journal.source_fingerprint)
                        and backup.kind == "missing"
                    )
                    after_backup = source.kind == "missing" and backup.exact(
                        journal.source_fingerprint
                    )
                    if not (before_backup or after_backup):
                        raise _RecoveryUncertainError(
                            "RECOVERY_SOURCE_OR_BACKUP_CHANGED", evidence
                        )
                    if not temporary.exact(journal.staged_fingerprint):
                        raise _RecoveryUncertainError(
                            "RECOVERY_STAGED_OUTPUT_CHANGED", evidence
                        )
                else:
                    if value.source is not None and not source.exact(
                        journal.source_fingerprint
                    ):
                        raise _RecoveryUncertainError(
                            "RECOVERY_SOURCE_CHANGED", evidence
                        )
                    before_publish = (
                        temporary.exact(journal.staged_fingerprint)
                        and destination.kind == "missing"
                    )
                    after_publish = temporary.kind == "missing" and destination.exact(
                        journal.staged_fingerprint
                    )
                    if not (before_publish or after_publish):
                        raise _RecoveryUncertainError(
                            "RECOVERY_PUBLISH_FINGERPRINT_AMBIGUOUS", evidence
                        )
                    if backup.kind != "missing":
                        raise _RecoveryUncertainError(
                            "RECOVERY_UNEXPECTED_BACKUP", evidence
                        )
            elif journal.state == "source_backed_up":
                if not same_path or not backup.exact(journal.source_fingerprint):
                    raise _RecoveryUncertainError("RECOVERY_BACKUP_CHANGED", evidence)
                if destination.kind == "missing":
                    if not temporary.exact(journal.staged_fingerprint):
                        raise _RecoveryUncertainError(
                            "RECOVERY_STAGED_OUTPUT_CHANGED", evidence
                        )
                elif not destination.exact(journal.staged_fingerprint):
                    raise _RecoveryUncertainError(
                        "RECOVERY_DESTINATION_CHANGED", evidence
                    )
            elif journal.state == "published":
                if not destination.exact(journal.staged_fingerprint):
                    raise _RecoveryUncertainError(
                        "RECOVERY_DESTINATION_CHANGED", evidence
                    )
                if same_path:
                    if not backup.exact(journal.source_fingerprint):
                        raise _RecoveryUncertainError(
                            "RECOVERY_BACKUP_CHANGED", evidence
                        )
                elif value.source is not None and not source.exact(
                    journal.source_fingerprint
                ):
                    raise _RecoveryUncertainError("RECOVERY_SOURCE_CHANGED", evidence)
                elif backup.kind != "missing":
                    raise _RecoveryUncertainError(
                        "RECOVERY_UNEXPECTED_BACKUP", evidence
                    )
            else:
                raise _RecoveryUncertainError(
                    "RECOVERY_UNEXPECTED_PUBLISH_STATE", evidence
                )

    async def _cleanup_committed_locked(
        self,
        snapshot: LibraryManagementJobSnapshot,
        paths: list[_JournalPaths],
        pinned: PinnedLibraryManagementProfile,
        roots: dict[str, Path],
    ) -> None:
        remove_source = (
            snapshot.mode in {"undo", "baseline_restore", "duplicate_resolution"}
            or pinned.profile.organization.source_cleanup
            == "remove_after_confirmed_move"
        )
        evidence_rows: list[
            tuple[
                _JournalPaths,
                _FileEvidence,
                _FileEvidence,
                _FileEvidence,
                _FileEvidence,
            ]
        ] = []
        for value in paths:
            source, temporary, backup, destination = await asyncio.to_thread(
                self._inspect_paths, value
            )
            if self._is_delete_mutation(value.journal):
                if (
                    source.kind != "missing"
                    or destination.kind != "missing"
                    or not backup.exact(value.journal.source_fingerprint)
                    or not temporary.exact(value.journal.staged_fingerprint)
                ):
                    raise _RecoveryUncertainError(
                        "RECOVERY_COMMITTED_DELETE_CHANGED",
                        self._evidence(value, source, temporary, backup, destination),
                    )
                evidence_rows.append((value, source, temporary, backup, destination))
                continue
            if value.journal.subject_kind == "audio":
                row = await self._store.get_target_track(
                    str(value.journal.local_track_id)
                )
                if row is None or (
                    str(row["root_id"]) != value.journal.destination_root_id
                    or str(row["relative_path"])
                    != value.journal.destination_relative_path
                ):
                    raise _RecoveryUncertainError(
                        "RECOVERY_CATALOG_DESTINATION_MISMATCH",
                        self._evidence(value, source, temporary, backup, destination),
                    )
            if not destination.exact(value.journal.staged_fingerprint):
                if destination.kind != "missing":
                    raise _RecoveryUncertainError(
                        "RECOVERY_COMMITTED_DESTINATION_CHANGED",
                        self._evidence(value, source, temporary, backup, destination),
                    )
                candidates = [
                    candidate
                    for candidate, candidate_evidence in (
                        (value.temporary, temporary),
                        (value.source, source),
                        (value.backup, backup),
                    )
                    if candidate is not None
                    and candidate_evidence.exact(value.journal.staged_fingerprint)
                ]
                if not candidates:
                    raise _RecoveryUncertainError(
                        "RECOVERY_COMMITTED_DESTINATION_MISSING",
                        self._evidence(value, source, temporary, backup, destination),
                    )
                await asyncio.to_thread(
                    self._restore_committed_destination,
                    candidates[0],
                    value.destination,
                    str(value.journal.staged_fingerprint),
                )
                destination = await asyncio.to_thread(self._inspect, value.destination)
            evidence_rows.append((value, source, temporary, backup, destination))

        for value, source, temporary, backup, _destination in evidence_rows:
            journal = value.journal
            same_path = value.source is not None and value.source == value.destination
            if backup.kind != "missing":
                if not backup.exact(journal.source_fingerprint):
                    raise _RecoveryUncertainError(
                        "RECOVERY_CLEANUP_BACKUP_CHANGED",
                        self._evidence(value, source, temporary, backup, _destination),
                    )
                assert value.backup is not None
                await asyncio.to_thread(value.backup.unlink)
            if (
                remove_source
                and value.source is not None
                and not same_path
                and source.kind != "missing"
            ):
                if not source.exact(journal.source_fingerprint):
                    raise _RecoveryUncertainError(
                        "RECOVERY_CLEANUP_SOURCE_CHANGED",
                        self._evidence(value, source, temporary, backup, _destination),
                    )
                await asyncio.to_thread(value.source.unlink)
            if temporary.kind != "missing":
                if temporary.kind != "regular":
                    raise _RecoveryUncertainError(
                        "RECOVERY_CLEANUP_TEMP_AMBIGUOUS",
                        self._evidence(value, source, temporary, backup, _destination),
                    )
                assert value.temporary is not None
                await asyncio.to_thread(value.temporary.unlink)
            if journal.state in {"catalog_committed", "cleanup_pending"}:
                await self._store.transition_file_mutation_journal(
                    journal.id,
                    expected_state=journal.state,
                    new_state="completed",
                    expected_row_revision=journal.row_revision,
                    updated_at=self._clock(),
                    increment_attempts=True,
                )
        await asyncio.to_thread(self._fsync_directories, paths)
        if remove_source and pinned.profile.organization.remove_empty_directories:
            await asyncio.to_thread(self._remove_empty_source_directories, paths, roots)

    async def _compensate_bundle(
        self,
        snapshot: LibraryManagementJobSnapshot,
        journals: list[LibraryFileMutationJournal],
        bundle_ordinal: int,
        *,
        reason: str,
    ) -> RecoveryDisposition:
        active = [
            journal
            for journal in journals
            if journal.state not in {"completed", "rolled_back", "needs_attention"}
        ]
        if not active:
            await self._store.fail_management_bundle_recovery(
                snapshot.job_id,
                bundle_ordinal,
                failure_code="RECOVERY_ROLLED_BACK",
                now=self._clock(),
            )
            return "rolled_back"
        try:
            _pinned, roots = self._publisher.recovery_filesystem_configuration(snapshot)
            paths = self._resolve_paths(journals, roots)
        except (OSError, ConflictError, StaleRevisionError, ValidationError) as error:
            return await self._attention_bundle(
                snapshot.job_id,
                bundle_ordinal,
                "RECOVERY_COMPENSATION_CONFIGURATION_STALE",
                {
                    "reason": reason,
                    "error_type": type(error).__name__,
                },
            )

        async def critical() -> RecoveryDisposition:
            try:
                async with self._filesystem.write_many(self._affected_roots(journals)):
                    await asyncio.to_thread(self._preflight_compensation, paths)
                    current = await self._transition_rollback_pending(paths, reason)
                    await asyncio.to_thread(self._restore_originals, current)
                    for value in current:
                        journal = value.journal
                        if journal.state == "rollback_pending":
                            await self._store.transition_file_mutation_journal(
                                journal.id,
                                expected_state="rollback_pending",
                                new_state="rolled_back",
                                expected_row_revision=journal.row_revision,
                                updated_at=self._clock(),
                                increment_attempts=True,
                                recovery_evidence_json=json.dumps(
                                    {"reason": reason},
                                    separators=(",", ":"),
                                    sort_keys=True,
                                ),
                            )
                    await asyncio.to_thread(self._fsync_directories, current)
                await self._store.fail_management_bundle_recovery(
                    snapshot.job_id,
                    bundle_ordinal,
                    failure_code="RECOVERY_ROLLED_BACK",
                    now=self._clock(),
                )
                return "rolled_back"
            except _RecoveryUncertainError as error:
                return await self._attention_bundle(
                    snapshot.job_id,
                    bundle_ordinal,
                    error.reason,
                    error.evidence,
                )

        task = asyncio.create_task(critical())
        result, cancelled = await self._finish_critical_task(task)
        if cancelled:
            raise asyncio.CancelledError
        return result

    def _preflight_compensation(self, paths: list[_JournalPaths]) -> None:
        for value in paths:
            journal = value.journal
            if journal.state in {"completed", "rolled_back", "needs_attention"}:
                continue
            source = self._inspect(value.source)
            temporary = self._inspect(value.temporary)
            backup = self._inspect(value.backup)
            destination = self._inspect(value.destination)
            evidence = self._evidence(value, source, temporary, backup, destination)
            same_path = value.source is not None and value.source == value.destination
            recycle_move = self._is_recycle_mutation(journal)
            if recycle_move:
                original_available = backup.exact(journal.source_fingerprint) or (
                    backup.kind == "missing"
                    and source.exact(journal.source_fingerprint)
                )
                if not original_available:
                    raise _RecoveryUncertainError(
                        "RECOVERY_ORIGINAL_FINGERPRINT_AMBIGUOUS", evidence
                    )
                if not (
                    destination.kind == "missing"
                    or destination.exact(journal.staged_fingerprint)
                ):
                    raise _RecoveryUncertainError(
                        "RECOVERY_DESTINATION_CHANGED", evidence
                    )
            elif same_path:
                original_available = backup.exact(journal.source_fingerprint) or (
                    backup.kind == "missing"
                    and destination.exact(journal.source_fingerprint)
                )
                if not original_available:
                    raise _RecoveryUncertainError(
                        "RECOVERY_ORIGINAL_FINGERPRINT_AMBIGUOUS", evidence
                    )
                if not (
                    destination.kind == "missing"
                    or destination.exact(journal.staged_fingerprint)
                    or destination.exact(journal.source_fingerprint)
                ):
                    raise _RecoveryUncertainError(
                        "RECOVERY_DESTINATION_CHANGED", evidence
                    )
            else:
                if value.source is not None and not source.exact(
                    journal.source_fingerprint
                ):
                    raise _RecoveryUncertainError(
                        "RECOVERY_ORIGINAL_FINGERPRINT_AMBIGUOUS", evidence
                    )
                if not (
                    destination.kind == "missing"
                    or destination.exact(journal.staged_fingerprint)
                ):
                    raise _RecoveryUncertainError(
                        "RECOVERY_DESTINATION_CHANGED", evidence
                    )
                if backup.kind != "missing":
                    raise _RecoveryUncertainError(
                        "RECOVERY_UNEXPECTED_BACKUP", evidence
                    )
            if temporary.kind in {"symlink", "other", "error"}:
                raise _RecoveryUncertainError(
                    "RECOVERY_TEMP_FINGERPRINT_AMBIGUOUS", evidence
                )

    async def _transition_rollback_pending(
        self, paths: list[_JournalPaths], reason: str
    ) -> list[_JournalPaths]:
        current = []
        encoded = json.dumps({"reason": reason}, separators=(",", ":"), sort_keys=True)
        for value in paths:
            journal = value.journal
            if journal.state not in {
                "completed",
                "rolled_back",
                "needs_attention",
                "rollback_pending",
            }:
                journal = await self._store.transition_file_mutation_journal(
                    journal.id,
                    expected_state=journal.state,
                    new_state="rollback_pending",
                    expected_row_revision=journal.row_revision,
                    updated_at=self._clock(),
                    increment_attempts=True,
                    recovery_evidence_json=encoded,
                )
            current.append(
                _JournalPaths(
                    journal=journal,
                    source=value.source,
                    temporary=value.temporary,
                    backup=value.backup,
                    destination=value.destination,
                )
            )
        return current

    def _restore_originals(self, paths: list[_JournalPaths]) -> None:
        for value in reversed(paths):
            journal = value.journal
            if journal.state != "rollback_pending":
                continue
            source = self._inspect(value.source)
            temporary = self._inspect(value.temporary)
            backup = self._inspect(value.backup)
            destination = self._inspect(value.destination)
            same_path = value.source is not None and value.source == value.destination
            if self._is_recycle_mutation(journal):
                if destination.exact(journal.staged_fingerprint):
                    value.destination.unlink()
                if backup.exact(journal.source_fingerprint):
                    assert value.source is not None and value.backup is not None
                    os.replace(value.backup, value.source)
            elif same_path:
                if backup.exact(journal.source_fingerprint):
                    if destination.exact(journal.staged_fingerprint):
                        value.destination.unlink()
                    os.replace(value.backup, value.destination)
            elif destination.exact(journal.staged_fingerprint):
                value.destination.unlink()
            if temporary.kind == "regular" and value.temporary is not None:
                value.temporary.unlink()

    async def _attention_bundle(
        self,
        job_id: str,
        bundle_ordinal: int,
        failure_code: str,
        evidence: dict[str, object],
    ) -> RecoveryDisposition:
        encoded = json.dumps(evidence, separators=(",", ":"), sort_keys=True)
        journals = await self._store.list_file_mutation_journals_for_bundle(
            job_id, bundle_ordinal
        )
        for journal in journals:
            if journal.state in {"completed", "rolled_back", "needs_attention"}:
                continue
            try:
                await self._store.transition_file_mutation_journal(
                    journal.id,
                    expected_state=journal.state,
                    new_state="needs_attention",
                    expected_row_revision=journal.row_revision,
                    updated_at=self._clock(),
                    failure_code=failure_code,
                    increment_attempts=True,
                    recovery_evidence_json=encoded,
                )
            except (StaleRevisionError, ValidationError):
                logger.warning(
                    "Library Management attention transition raced for journal %s",
                    journal.id,
                )
        await self._store.fail_management_bundle_recovery(
            job_id,
            bundle_ordinal,
            failure_code="RECOVERY_NEEDS_ATTENTION",
            now=self._clock(),
        )
        return "needs_attention"

    async def _mark_missing_catalog_destinations(
        self, paths: list[_JournalPaths]
    ) -> None:
        for value in paths:
            journal = value.journal
            if journal.subject_kind != "audio" or journal.local_track_id is None:
                continue
            destination = await asyncio.to_thread(self._inspect, value.destination)
            if destination.exact(journal.staged_fingerprint):
                continue
            await self._store.mark_management_recovery_destination_missing(
                journal.local_track_id,
                str(journal.destination_root_id),
                str(journal.destination_relative_path),
                now=self._clock(),
            )

    @staticmethod
    def _validate_complete_prepared_bundle(
        items: list[LibraryManagementPlanItem],
        journals: list[LibraryFileMutationJournal],
    ) -> None:
        audio_ordinals = {
            journal.plan_item_ordinal
            for journal in journals
            if journal.subject_kind == "audio"
        }
        if audio_ordinals != {item.ordinal for item in items}:
            raise ValidationError(
                "Recovery found an incompletely prepared management bundle."
            )

    @staticmethod
    def _affected_roots(journals: list[LibraryFileMutationJournal]) -> set[str]:
        roots = {
            root_id
            for journal in journals
            for root_id in (
                journal.source_root_id,
                journal.temporary_root_id,
                journal.backup_root_id,
                journal.destination_root_id,
            )
            if root_id is not None
        }
        if not roots:
            raise ValidationError("A recovery bundle has no library root.")
        return roots

    def _resolve_paths(
        self,
        journals: list[LibraryFileMutationJournal],
        roots: dict[str, Path],
    ) -> list[_JournalPaths]:
        resolved = []
        for journal in journals:
            if (
                journal.destination_root_id is None
                or journal.destination_relative_path is None
            ):
                raise ValidationError("A recovery journal has no destination.")
            destination = self._required_path(
                roots,
                journal.destination_root_id,
                journal.destination_relative_path,
            )
            source = self._optional_path(
                roots, journal.source_root_id, journal.source_relative_path
            )
            if (
                source is None
                and journal.subject_kind == "external_art"
                and journal.source_fingerprint is not None
            ):
                source = destination
            resolved.append(
                _JournalPaths(
                    journal=journal,
                    source=source,
                    temporary=self._optional_path(
                        roots,
                        journal.temporary_root_id,
                        journal.temporary_relative_path,
                    ),
                    backup=self._optional_path(
                        roots, journal.backup_root_id, journal.backup_relative_path
                    ),
                    destination=destination,
                )
            )
        return resolved

    def _optional_path(
        self,
        roots: dict[str, Path],
        root_id: str | None,
        relative: str | None,
    ) -> Path | None:
        if root_id is None and relative is None:
            return None
        if root_id is None or relative is None:
            raise ValidationError("A recovery journal path is incomplete.")
        return self._required_path(roots, root_id, relative)

    @staticmethod
    def _required_path(roots: dict[str, Path], root_id: str, relative: str) -> Path:
        root = roots.get(root_id)
        if root is None:
            raise StaleRevisionError("A recovery library root changed.")
        pure = PurePosixPath(relative)
        if (
            pure.is_absolute()
            or not pure.parts
            or any(part in {"", ".", ".."} for part in pure.parts)
        ):
            raise ValidationError("A recovery path is not a safe relative path.")
        metadata = root.lstat()
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValidationError("A recovery library root is unsafe.")
        current = root
        for part in pure.parts[:-1]:
            current = current / part
            try:
                metadata = current.lstat()
            except FileNotFoundError:
                continue
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise ValidationError("A recovery path contains a symlink.")
        return root.joinpath(*pure.parts)

    @staticmethod
    def _inspect(path: Path | None) -> _FileEvidence:
        if path is None:
            return _FileEvidence("missing")
        try:
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                return _FileEvidence("symlink")
            if not stat.S_ISREG(metadata.st_mode):
                return _FileEvidence("other")
            digest = hashlib.sha256()
            flags = (
                os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
            )
            descriptor = os.open(path, flags)
            with os.fdopen(descriptor, "rb") as handle:
                if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                    return _FileEvidence("other")
                while chunk := handle.read(1024 * 1024):
                    digest.update(chunk)
            return _FileEvidence("regular", digest.hexdigest())
        except FileNotFoundError:
            return _FileEvidence("missing")
        except OSError as error:
            return _FileEvidence("error", error=type(error).__name__)

    @classmethod
    def _inspect_paths(
        cls, value: _JournalPaths
    ) -> tuple[_FileEvidence, _FileEvidence, _FileEvidence, _FileEvidence]:
        return (
            cls._inspect(value.source),
            cls._inspect(value.temporary),
            cls._inspect(value.backup),
            cls._inspect(value.destination),
        )

    @staticmethod
    def _evidence(
        value: _JournalPaths,
        source: _FileEvidence,
        temporary: _FileEvidence,
        backup: _FileEvidence,
        destination: _FileEvidence,
    ) -> dict[str, object]:
        def encoded(item: _FileEvidence) -> dict[str, object | None]:
            return {
                "kind": item.kind,
                "fingerprint": item.fingerprint,
                "error": item.error,
            }

        return {
            "journal_id": value.journal.id,
            "journal_state": value.journal.state,
            "source": encoded(source),
            "temporary": encoded(temporary),
            "backup": encoded(backup),
            "destination": encoded(destination),
        }

    @staticmethod
    def _is_delete_mutation(journal: LibraryFileMutationJournal) -> bool:
        if journal.subject_kind != "external_art" or not journal.subject_key.startswith(
            "delete:"
        ):
            return False
        try:
            evidence = json.loads(journal.recovery_evidence_json)
        except (json.JSONDecodeError, TypeError):
            return False
        return evidence.get("mutation") == "delete"

    @staticmethod
    def _is_recycle_mutation(journal: LibraryFileMutationJournal) -> bool:
        try:
            evidence = json.loads(journal.recovery_evidence_json)
        except (json.JSONDecodeError, TypeError):
            return False
        return evidence.get("mutation") == "recycle"

    @staticmethod
    def _restore_committed_destination(
        source: Path, destination: Path, expected_fingerprint: str
    ) -> None:
        if destination.exists() or destination.is_symlink():
            raise ConflictError("A committed destination became occupied.")
        temporary = destination.parent / (
            f".droppedneedle-management-recovery-{expected_fingerprint[:16]}"
            f"{destination.suffix}"
        )
        if temporary.exists() or temporary.is_symlink():
            evidence = LibraryManagementRecoveryService._inspect(temporary)
            if not evidence.exact(expected_fingerprint):
                raise ConflictError("A recovery restore temporary is occupied.")
        else:
            shutil.copy2(source, temporary)
        if not LibraryManagementRecoveryService._inspect(temporary).exact(
            expected_fingerprint
        ):
            raise ConflictError("A restored management destination changed.")
        os.replace(temporary, destination)

    @staticmethod
    def _fsync_directories(paths: list[_JournalPaths]) -> None:
        directories = {
            path.parent
            for value in paths
            for path in (
                value.source,
                value.temporary,
                value.backup,
                value.destination,
            )
            if path is not None and path.parent.exists()
        }
        for directory in directories:
            try:
                descriptor = os.open(directory, os.O_RDONLY)
                try:
                    os.fsync(descriptor)
                finally:
                    os.close(descriptor)
            except OSError:
                continue

    @staticmethod
    def _remove_empty_source_directories(
        paths: list[_JournalPaths], roots: dict[str, Path]
    ) -> None:
        candidates: set[tuple[Path, Path]] = set()
        for value in paths:
            root_id = value.journal.source_root_id
            if (
                value.source is None
                or value.source == value.destination
                or root_id is None
                or root_id not in roots
            ):
                continue
            candidates.add((value.source.parent, roots[root_id]))
        for directory, root in sorted(
            candidates, key=lambda value: len(value[0].parts), reverse=True
        ):
            current = directory
            while current != root and root in current.parents:
                try:
                    current.rmdir()
                except OSError:
                    break
                current = current.parent

    @staticmethod
    def _worker_id() -> str:
        return f"library-management-recovery:{os.getpid()}"
