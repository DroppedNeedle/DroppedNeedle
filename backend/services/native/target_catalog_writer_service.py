"""Explicit target-catalog mutations that may also touch an administrator's files."""

from __future__ import annotations

import asyncio
import logging
import shutil
import time
from collections.abc import Callable
from pathlib import Path

from core.exceptions import ExternalServiceError, ResourceNotFoundError, ValidationError
from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioTag
from services.local_files_service import LocalFilesService
from services.native.target_native_library_service import TargetNativeLibraryService
from services.native.recycle_bin import recycle


logger = logging.getLogger(__name__)


class TargetCatalogWriterService:
    def __init__(
        self,
        store: NativeLibraryStore,
        local_files: LocalFilesService,
        library: TargetNativeLibraryService,
        tagger: AudioTagger | None = None,
        recycle_bin_getter: Callable[[], Path | None] | None = None,
    ) -> None:
        self._store = store
        self._local_files = local_files
        self._library = library
        self._tagger = tagger or AudioTagger()
        self._recycle_bin_getter = recycle_bin_getter

    async def read_tags(self, track_id: str) -> AudioTag:
        path = await self._validated_path(track_id)
        try:
            tag, _ = await asyncio.to_thread(self._tagger.read_tags, path)
        except (OSError, ValueError) as error:
            raise ValidationError("Could not read the audio file.") from error
        return tag

    async def remove_track(
        self,
        track_id: str,
        *,
        actor_user_id: str,
        delete_file: bool = True,
    ) -> list[str]:
        row = await self._store.get_target_track(track_id)
        if row is None or row["availability"] != "indexed":
            raise ResourceNotFoundError("Library track not found.")
        if delete_file:
            path = await self._removal_target(track_id)
            if path is not None:
                try:
                    await asyncio.to_thread(path.unlink)
                except FileNotFoundError:
                    pass  # ENOENT race: already absent is still idempotent success
                except OSError as error:
                    raise ExternalServiceError("Could not remove this file.") from error
        return await self._store.mark_target_tracks_missing(
            [track_id],
            actor_user_id=actor_user_id,
            reason_code=("FILE_DELETED" if delete_file else "CATALOG_REMOVAL"),
            missing_at=time.time(),
        )

    async def remove_album(
        self,
        album_id: str,
        *,
        actor_user_id: str | None,
        delete_files: bool,
        recycle_files: bool = False,
    ) -> list[str]:
        rows = await self._store.get_target_album_tracks(album_id)
        if not rows:
            # Issue #301: an album whose files already vanished has no indexed
            # rows left, but the album itself still exists. Only an album with
            # no rows at all is a 404; ghost editions must still clean up.
            rows = await self._store.get_target_album_tracks(
                album_id, include_unavailable=True
            )
            if not rows:
                raise ResourceNotFoundError("Library album not found.")
            if recycle_files:
                # Nothing remains on disk to recycle; drop the catalog rows.
                return await self._store.mark_target_tracks_missing(
                    [str(row["id"]) for row in rows],
                    actor_user_id=actor_user_id,
                    reason_code="CATALOG_REMOVAL",
                    missing_at=time.time(),
                )
        if recycle_files:
            return await self._recycle_album(rows, actor_user_id)
        removed: list[str] = []
        failures = 0
        if delete_files:
            for row in rows:
                track_id = str(row["id"])
                try:
                    path = await self._removal_target(track_id)
                    if path is None:
                        # Issue #301: bytes already gone from disk are not a
                        # failure. Log the basename only, never the full path.
                        logger.warning(
                            "Target album file already absent; removing catalog "
                            "entry for %s",
                            Path(str(row.get("file_path") or track_id)).name,
                        )
                    else:
                        await asyncio.to_thread(path.unlink)
                except FileNotFoundError:
                    pass  # ENOENT race: already absent is still safe to mark
                except (OSError, ValidationError):
                    failures += 1
                    continue
                removed.append(track_id)
        else:
            removed = [str(row["id"]) for row in rows]
        changed = await self._store.mark_target_tracks_missing(
            removed,
            actor_user_id=actor_user_id,
            reason_code=("ALBUM_FILES_DELETED" if delete_files else "CATALOG_REMOVAL"),
            missing_at=time.time(),
        )
        if failures:
            raise ExternalServiceError("Could not remove every file in this album.")
        return changed

    async def provider_release_group_id(self, album_id: str) -> str | None:
        return await self._store.target_album_provider_identity(album_id)

    async def _recycle_album(
        self,
        rows: list[dict],
        actor_user_id: str | None,
    ) -> list[str]:
        bin_path = self._recycle_bin_getter() if self._recycle_bin_getter else None
        if bin_path is None:
            raise ValidationError("A recycle bin is not available for this library.")
        moved: list[tuple[Path, Path]] = []
        track_ids: list[str] = []
        try:
            for row in rows:
                track_id = str(row["id"])
                original = await self._validated_path(track_id)
                destination = await asyncio.to_thread(recycle, original, bin_path)
                moved.append((original, destination))
                track_ids.append(track_id)
            changed = await self._store.mark_target_tracks_missing(
                track_ids,
                actor_user_id=actor_user_id,
                reason_code="ALBUM_FILES_RECYCLED",
                missing_at=time.time(),
            )
        except Exception as error:  # noqa: BLE001 - restore every moved file before surfacing
            await self._restore_recycled_files(moved)
            if isinstance(error, (ResourceNotFoundError, ValidationError)):
                raise
            raise ExternalServiceError("Could not recycle this album.") from error
        return changed

    @staticmethod
    async def _restore_recycled_files(moved: list[tuple[Path, Path]]) -> None:
        failures = 0
        for original, destination in reversed(moved):
            try:
                await asyncio.to_thread(
                    original.parent.mkdir, parents=True, exist_ok=True
                )
                await asyncio.to_thread(shutil.move, str(destination), str(original))
            except OSError:
                failures += 1
        if failures:
            raise ExternalServiceError("Could not restore every recycled file.")

    async def _validated_path(self, track_id: str) -> Path:
        try:
            return await self._local_files.resolve_validated_path(track_id)
        except FileNotFoundError as error:
            raise ValidationError(
                "The audio file is no longer present on disk."
            ) from error

    async def _removal_target(self, track_id: str) -> Path | None:
        """F-TARGETCATALOG-01: resolve the file for a delete-file removal.

        Returns None only when the validated path is already absent. Every
        other failure (outside configured roots, permission, malformed or
        deleted catalog row) keeps its strict exception so it can never be
        misread as an idempotent absence."""
        try:
            return await self._local_files.resolve_validated_path(track_id)
        except ResourceNotFoundError:
            return None
