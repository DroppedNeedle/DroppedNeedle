"""Retention of finished import bundles and the artwork only they referenced (#517)."""

from __future__ import annotations

import asyncio
from collections.abc import Callable
import logging
import time

import msgspec

from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from services.native.library_management_publisher import import_bundle_id
from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

_MAX_PRUNE_BATCHES = 5
_MAX_CLEANUP_BATCHES = 20
_CLEANUP_BATCH_SIZE = 500
# A younger reference or blob can belong to a publish or preview still running.
_IN_FLIGHT_GRACE_SECONDS = 86400.0


class LibraryManagementImportRetentionResult(msgspec.Struct, frozen=True, kw_only=True):
    bundles_removed: int
    references_removed: int
    blobs_removed: int


class LibraryManagementImportRetention:
    """Drops import bundles nothing needs any more, then the blobs left unreferenced.

    Bundles exist for idempotent retries, crash recovery and the cleanup barrier
    of their download attempts. Plan items stay: undo and the automatic scan
    still read them. Rolled-back bundles stay: the orphan workspace reconcile
    treats them as a reason to keep their task's folder.
    """

    def __init__(
        self,
        store: NativeLibraryStore,
        blobs: LibraryManagementBlobStore,
        preferences: PreferencesService,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._blobs = blobs
        self._preferences = preferences
        self._clock = clock

    async def prune(self) -> LibraryManagementImportRetentionResult:
        """One bounded pass: expired bundles, orphaned references, freed blobs."""

        now = self._clock()
        # Finished work stays as long as it can be undone.
        retention_days = (
            self._preferences.get_library_management_settings_raw().undo_retention_days
        )
        bundles = 0
        for _batch in range(_MAX_PRUNE_BATCHES):
            removed, more = await self._store.prune_terminal_import_bundles(
                updated_before=now - retention_days * 86400.0
            )
            bundles += removed
            if not more:
                break
            await asyncio.sleep(0)
        # A sealed edition conversion references its bundle's artwork before the
        # bundle exists, until the user applies it.
        sealed = {
            import_bundle_id(f"edition-conversion:{job_id}")
            for job_id in await self._store.list_ready_edition_conversion_ids()
        }
        references = await self._store.remove_orphaned_import_bundle_references(
            created_before=now - _IN_FLIGHT_GRACE_SECONDS, keep_bundle_ids=sealed
        )
        blobs = 0
        if bundles or references:
            for _batch in range(_MAX_CLEANUP_BATCHES):
                cleanup = await self._blobs.cleanup(
                    older_than=now - _IN_FLIGHT_GRACE_SECONDS,
                    limit=_CLEANUP_BATCH_SIZE,
                )
                removed = (
                    cleanup.unreferenced_blobs_removed + cleanup.unledgered_files_removed
                )
                blobs += removed
                if removed + cleanup.temporary_files_removed == 0:
                    break
        if bundles or references or blobs:
            logger.info(
                "Import retention removed %d bundles, %d artwork references and "
                "%d blobs",
                bundles,
                references,
                blobs,
            )
        return LibraryManagementImportRetentionResult(
            bundles_removed=bundles,
            references_removed=references,
            blobs_removed=blobs,
        )
