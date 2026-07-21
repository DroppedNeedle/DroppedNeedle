"""Explicit target-catalog mutations that may also touch an administrator's files."""

from __future__ import annotations

import asyncio
import hashlib
import shutil
import time
from collections.abc import Callable
from pathlib import Path

import msgspec

from core.exceptions import ExternalServiceError, ResourceNotFoundError, ValidationError
from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioTag
from services.local_files_service import LocalFilesService
from services.native.target_native_library_service import TargetNativeLibraryService
from services.native.recycle_bin import recycle
from services.native.file_revision import revision_from_stat


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

    async def update_tags(
        self,
        track_id: str,
        tag: AudioTag,
        *,
        actor_user_id: str,
    ):
        row = await self._store.get_target_track(track_id)
        if row is None or row["availability"] != "indexed":
            raise ResourceNotFoundError("Library track not found.")
        path = await self._validated_path(track_id)
        tag_to_write = msgspec.structs.replace(
            tag, compilation=bool(row.get("is_compilation"))
        )
        try:
            await asyncio.to_thread(self._tagger.write_mb_tags, path, tag_to_write)
            persisted_tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
            stat = await asyncio.to_thread(path.stat)
        except (OSError, ValueError) as error:
            raise ValidationError("Could not write tags to the audio file.") from error
        await self._store.update_target_track_tags(
            track_id,
            tag=persisted_tag,
            info=info,
            file_size_bytes=stat.st_size,
            file_mtime_ns=stat.st_mtime_ns,
            stat_revision=revision_from_stat(stat),
            tag_revision=hashlib.sha256(msgspec.json.encode(persisted_tag)).hexdigest(),
            actor_user_id=actor_user_id,
            updated_at=time.time(),
        )
        updated = await self._store.get_target_track(track_id)
        if updated is None:
            raise ResourceNotFoundError("Library track not found.")
        projected = await self._library.track(track_id)
        if projected is None:
            raise ResourceNotFoundError("Library track not found.")
        return projected

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
            path = await self._validated_path(track_id)
            try:
                await asyncio.to_thread(path.unlink)
            except FileNotFoundError:
                pass
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
            raise ResourceNotFoundError("Library album not found.")
        if recycle_files:
            return await self._recycle_album(rows, actor_user_id)
        removed: list[str] = []
        failures = 0
        if delete_files:
            for row in rows:
                track_id = str(row["id"])
                try:
                    path = await self._validated_path(track_id)
                    await asyncio.to_thread(path.unlink)
                except FileNotFoundError:
                    pass
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
