"""Download/import library contract backed only by the target catalog store."""

from __future__ import annotations

import asyncio
import hashlib
import os
import sqlite3
import time
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

import msgspec

from models.local_catalog import (
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
    LocalTrackGenre,
)
from models.library_management import (
    BUNDLE_BLOCKED,
    FIELD_UNSUPPORTED_BY_FORMAT,
    PATH_COLLISION_DIFFERENT,
    POLICY_CHANGED,
    ROOT_UNAVAILABLE,
    LibraryManagementImportBundle,
    LibraryManagementImportResult,
    LibraryManagementPublishedImportFile,
)
from models.library_work import ScannedTrackWrite
from services.local_files_service import AUDIO_EXTENSIONS
from services.native.identification_revisions import album_input_revisions
from services.native.file_revision import revision_from_stat
from services.native.local_album_grouper import (
    grouping_directory,
    normalize_group_value,
)
from services.native.local_album_grouping_service import (
    grouping_album_id,
    grouping_artist_candidate_id,
)
from core.exceptions import (
    AutomaticManagementHoldError,
    ConflictError,
    StaleRevisionError,
    ValidationError,
)

if TYPE_CHECKING:
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from models.audio import AudioInfo, AudioTag
    from services.native.identification_queue_service import IdentificationQueueService
    from services.native.automatic_import_management_service import (
        AutomaticImportManagementService,
    )
    from services.native.library_filesystem_coordinator import (
        LibraryFilesystemCoordinator,
    )
    from services.native.library_management_publisher import LibraryManagementPublisher
    from services.native.library_policy_resolver import LibraryPolicyResolver

_TRACK_NAMESPACE = uuid.UUID("1a8da1ca-9ca8-4bc5-bdb7-512fcf58ef67")


class TargetImportLibraryService:
    def __init__(
        self,
        store: "NativeLibraryStore",
        resolver_getter: "Callable[[], LibraryPolicyResolver]",
        identification_queue: "IdentificationQueueService",
        *,
        policy_transition_lock: asyncio.Lock | None = None,
        filesystem_coordinator: "LibraryFilesystemCoordinator | None" = None,
        management_publisher: "LibraryManagementPublisher | None" = None,
        automatic_management: "AutomaticImportManagementService | None" = None,
    ) -> None:
        self._store = store
        self._resolver_getter = resolver_getter
        self._queue = identification_queue
        self._policy_transition_lock = policy_transition_lock or asyncio.Lock()
        self._filesystem = filesystem_coordinator
        self._management_publisher = management_publisher
        self._automatic_management = automatic_management

    def is_configured(self) -> bool:
        return bool(self._resolver_getter().settings.library_roots)

    async def publish_import_bundle(
        self, bundle: LibraryManagementImportBundle
    ) -> LibraryManagementImportResult:
        if self._management_publisher is None:
            raise RuntimeError("The staged import publisher is not configured.")

        if self._automatic_management is not None:
            bundle = await self._automatic_management.prepare(bundle)

        async def commit(
            bundle_id: str,
            files: tuple[LibraryManagementPublishedImportFile, ...],
        ) -> tuple[str, ...]:
            async with self._policy_transition_lock:
                return await self._commit_published_import_bundle(
                    bundle_id,
                    files,
                    expected_policy_revision=bundle.policy_revision,
                )

        automatic = any(value.pinned_profile is not None for value in bundle.files)
        try:
            return await self._management_publisher.publish_import_bundle(
                bundle, commit
            )
        except AutomaticManagementHoldError:
            raise
        except ConflictError as error:
            if not automatic:
                raise
            raise AutomaticManagementHoldError(
                PATH_COLLISION_DIFFERENT,
                "Library Management found a destination conflict. Resolve it and retry.",
            ) from error
        except StaleRevisionError as error:
            if not automatic:
                raise
            raise AutomaticManagementHoldError(
                POLICY_CHANGED,
                "The library policy changed during publication. Retry this import.",
            ) from error
        except ValidationError as error:
            if not automatic:
                raise
            raise AutomaticManagementHoldError(
                FIELD_UNSUPPORTED_BY_FORMAT,
                "The automatic write no longer passes its safety checks.",
            ) from error
        except OSError as error:
            if not automatic:
                raise
            raise AutomaticManagementHoldError(
                ROOT_UNAVAILABLE,
                "The library destination is unavailable. Restore it and retry.",
            ) from error
        except sqlite3.Error as error:
            if not automatic:
                raise
            raise AutomaticManagementHoldError(
                BUNDLE_BLOCKED,
                "The automatic import could not be committed. Retry it later.",
            ) from error

    @staticmethod
    def _root_for(path: Path, resolver: "LibraryPolicyResolver"):  # noqa: ANN205 - typed settings root variant
        resolved = path.resolve()
        for root in resolver.settings.library_roots:
            root_path = Path(root.path).resolve()
            try:
                relative = resolved.relative_to(root_path)
            except ValueError:
                continue
            return root, relative.as_posix()
        raise ValueError("Imported file is outside every configured library root.")

    async def upsert_file(
        self,
        audio_path: Path,
        tag: "AudioTag",
        info: "AudioInfo",
        *,
        release_group_mbid: str | None,
        release_mbid: str | None = None,
        recording_mbid: str | None = None,
        confidence: float = 1.0,
        source: str = "download",
        download_task_id: str | None = None,
        source_path: str | None = None,
        file_mtime: float | None = None,
    ) -> str:
        async with self._policy_transition_lock:
            for attempt in range(2):
                resolver = self._resolver_getter()
                try:
                    return await self._upsert_file_once(
                        audio_path,
                        tag,
                        info,
                        resolver=resolver,
                        release_group_mbid=release_group_mbid,
                        release_mbid=release_mbid,
                        recording_mbid=recording_mbid,
                        confidence=confidence,
                        source=source,
                        download_task_id=download_task_id,
                        source_path=source_path,
                        file_mtime=file_mtime,
                    )
                except StaleRevisionError:
                    if attempt == 1:
                        raise
            raise AssertionError("Policy retry loop did not return")

    async def _upsert_file_once(
        self,
        audio_path: Path,
        tag: "AudioTag",
        info: "AudioInfo",
        *,
        resolver: "LibraryPolicyResolver",
        release_group_mbid: str | None,
        release_mbid: str | None,
        recording_mbid: str | None,
        confidence: float,
        source: str,
        download_task_id: str | None,
        source_path: str | None,
        file_mtime: float | None,
    ) -> str:
        del confidence
        root, relative_path = self._root_for(audio_path, resolver)
        stat = await asyncio.to_thread(audio_path.stat)
        resolution = resolver.resolve(audio_path)
        if resolution is None:
            raise ValueError("Imported file is outside every configured library root.")
        policy = resolution.policy
        now = file_mtime if file_mtime is not None else stat.st_mtime
        album_title = tag.album.strip() or audio_path.parent.name or "Unknown Album"
        album_artist = (tag.album_artist or tag.artist or "Unknown Artist").strip()
        artist_id = await self._store.find_target_artist_by_name(album_artist)
        artist_id = artist_id or grouping_artist_candidate_id(album_artist)
        directory = grouping_directory(relative_path)
        grouping_key = (
            f"{root.id}:{directory}:{normalize_group_value(album_title)}:"
            f"{normalize_group_value(album_artist)}"
        )
        album_id = (
            await self._store.resolve_target_id("album", release_group_mbid)
            if release_group_mbid
            else None
        ) or grouping_album_id(grouping_key)
        existing = await self._store.get_target_track_by_path(str(audio_path))
        track_id = (
            str(existing["id"])
            if existing is not None
            else str(uuid.uuid5(_TRACK_NAMESPACE, f"{root.id}:{relative_path}"))
        )
        tag_revision = hashlib.sha256(msgspec.json.encode(tag)).hexdigest()
        artist = LocalArtist(
            id=artist_id,
            display_name=album_artist,
            folded_name=album_artist.casefold(),
            normalized_name=normalize_group_value(album_artist),
            sort_name=tag.album_artist_sort,
            kind="group",
            created_at=now,
            updated_at=now,
        )
        album = LocalAlbum(
            id=album_id,
            root_id=root.id,
            grouping_key=grouping_key,
            title=album_title,
            album_artist_id=artist_id,
            album_artist_name=album_artist,
            album_artist_sort_name=tag.album_artist_sort,
            year=tag.year,
            original_release_date=tag.original_release_date,
            primary_genre=tag.genre,
            is_compilation=tag.compilation,
            grouping_source="automatic",
            created_at=now,
            updated_at=now,
        )
        track = LocalTrack(
            id=track_id,
            local_album_id=album_id,
            root_id=root.id,
            file_path=str(audio_path),
            relative_path=relative_path,
            path_hash=hashlib.sha256(relative_path.encode()).hexdigest(),
            file_size_bytes=stat.st_size,
            file_mtime_ns=stat.st_mtime_ns,
            stat_revision=revision_from_stat(stat),
            tag_revision=tag_revision,
            tags_read_at=now,
            title=tag.title.strip() or audio_path.stem,
            artist_name=tag.artist or album_artist,
            album_title=album_title,
            album_artist_name=album_artist,
            tag_album_title=tag.album,
            tag_album_artist_name=tag.album_artist,
            disc_number=tag.disc_number,
            track_number=tag.track_number,
            year=tag.year,
            genre=tag.genre,
            title_sort=tag.title_sort,
            artist_sort=tag.artist_sort,
            album_sort=tag.album_sort,
            album_artist_sort=tag.album_artist_sort,
            disc_subtitle=tag.disc_subtitle,
            is_compilation=tag.compilation,
            embedded_release_group_mbid=(
                release_group_mbid or tag.musicbrainz_release_group_id
            ),
            embedded_release_mbid=release_mbid or tag.musicbrainz_release_id,
            embedded_recording_mbid=recording_mbid or tag.musicbrainz_recording_id,
            embedded_release_track_mbid=tag.musicbrainz_release_track_id,
            embedded_artist_mbid=tag.musicbrainz_artist_id,
            embedded_album_artist_mbid=tag.musicbrainz_album_artist_id,
            duration_seconds=info.duration_seconds,
            file_format=info.file_format,
            bit_rate=info.bitrate,
            sample_rate=info.sample_rate,
            bit_depth=info.bit_depth,
            channels=info.channels,
            replaygain_track_gain=tag.replaygain_track_gain,
            replaygain_album_gain=tag.replaygain_album_gain,
            replaygain_track_peak=tag.replaygain_track_peak,
            replaygain_album_peak=tag.replaygain_album_peak,
            ingest_source=source,
            download_task_id=download_task_id,
            source_path=source_path,
            imported_at=now,
            desired_policy_revision=resolver.policy_revision,
            applied_policy_revision=resolver.policy_revision,
            applied_policy=policy,
            availability="excluded" if policy == "excluded" else "indexed",
            excluded_at=now if policy == "excluded" else None,
        )
        persisted_id, _ = await self._store.upsert_scanned_track(
            artist=artist,
            album=album,
            track=track,
            credit=LocalArtistCredit(
                local_artist_id=artist_id,
                position=0,
                credited_name=album_artist,
            ),
            genres=[
                LocalTrackGenre(
                    local_track_id=track_id,
                    position=position,
                    name=value,
                    folded_name=value.casefold(),
                    source="local",
                    source_document_revision=tag_revision,
                )
                for position, value in enumerate(
                    tag.genres or ([tag.genre] if tag.genre else [])
                )
            ],
            expected_policy_revision=resolver.policy_revision,
        )
        embedded_identity = bool(
            release_group_mbid
            or release_mbid
            or tag.musicbrainz_release_group_id
            or tag.musicbrainz_release_id
        )
        if policy == "automatic" or (policy == "local_metadata" and embedded_identity):
            rows = await self._store.get_target_album_tracks(album_id)
            revisions = album_input_revisions(rows)
            await self._queue.enqueue_album(
                album_id,
                input_revision=":".join(revisions),
                kind="automatic" if policy == "automatic" else "post_processing",
                now=now,
                expected_policy_revision=resolver.policy_revision,
            )
        return persisted_id

    async def _commit_published_import_bundle(
        self,
        bundle_id: str,
        files: tuple[LibraryManagementPublishedImportFile, ...],
        *,
        expected_policy_revision: str,
    ) -> tuple[str, ...]:
        resolver = self._resolver_getter()
        if resolver.policy_revision != expected_policy_revision:
            raise StaleRevisionError("Library policy changed before import commit.")
        writes: list[tuple[int, ScannedTrackWrite]] = []
        replacements: dict[int, str] = {}
        for value in files:
            request = value.request
            write = await self._build_published_import_write(
                Path(value.destination_path),
                value.tag,
                value.info,
                resolver=resolver,
                release_group_mbid=request.release_group_mbid,
                release_mbid=request.release_mbid,
                recording_mbid=request.recording_mbid,
                source=request.source,
                download_task_id=request.download_task_id,
                source_path=request.source_path,
                file_mtime=request.file_mtime,
            )
            writes.append((request.ordinal, write))
            if request.replacement_local_track_id is not None:
                replacements[request.ordinal] = request.replacement_local_track_id
        track_ids = await self._store.commit_library_management_import_bundle(
            bundle_id,
            writes=writes,
            replacement_track_ids=replacements,
            automatic_requests={
                value.request.ordinal: value.request
                for value in files
                if value.request.pinned_profile is not None
            },
            expected_policy_revision=expected_policy_revision,
            result_paths=[value.destination_path for value in files],
            updated_at=time.time(),
        )
        automatic_ordinals = {
            value.request.ordinal
            for value in files
            if value.request.pinned_profile is not None
        }
        queue_writes = {
            write.album.id: write
            for ordinal, write in writes
            if ordinal not in automatic_ordinals
        }
        for write in queue_writes.values():
            embedded_identity = bool(
                write.track.embedded_release_group_mbid
                or write.track.embedded_release_mbid
            )
            if write.track.applied_policy == "automatic" or (
                write.track.applied_policy == "local_metadata" and embedded_identity
            ):
                rows = await self._store.get_target_album_tracks(write.album.id)
                revisions = album_input_revisions(rows)
                await self._queue.enqueue_album(
                    write.album.id,
                    input_revision=":".join(revisions),
                    kind=(
                        "automatic"
                        if write.track.applied_policy == "automatic"
                        else "post_processing"
                    ),
                    now=write.track.imported_at,
                    expected_policy_revision=expected_policy_revision,
                )
        return track_ids

    async def _build_published_import_write(
        self,
        audio_path: Path,
        tag: "AudioTag",
        info: "AudioInfo",
        *,
        resolver: "LibraryPolicyResolver",
        release_group_mbid: str | None,
        release_mbid: str | None,
        recording_mbid: str | None,
        source: str,
        download_task_id: str | None,
        source_path: str | None,
        file_mtime: float | None,
    ) -> ScannedTrackWrite:
        root, relative_path = self._root_for(audio_path, resolver)
        stat_result = await asyncio.to_thread(audio_path.stat)
        resolution = resolver.resolve(audio_path)
        if resolution is None:
            raise ValueError("Imported file is outside every configured library root.")
        policy = resolution.policy
        now = file_mtime if file_mtime is not None else stat_result.st_mtime
        album_title = tag.album.strip() or audio_path.parent.name or "Unknown Album"
        album_artist = (tag.album_artist or tag.artist or "Unknown Artist").strip()
        artist_id = await self._store.find_target_artist_by_name(album_artist)
        artist_id = artist_id or grouping_artist_candidate_id(album_artist)
        directory = grouping_directory(relative_path)
        grouping_key = (
            f"{root.id}:{directory}:{normalize_group_value(album_title)}:"
            f"{normalize_group_value(album_artist)}"
        )
        album_id = (
            await self._store.resolve_target_id("album", release_group_mbid)
            if release_group_mbid
            else None
        ) or grouping_album_id(grouping_key)
        existing = await self._store.get_target_track_by_path(str(audio_path))
        track_id = (
            str(existing["id"])
            if existing is not None
            else str(uuid.uuid5(_TRACK_NAMESPACE, f"{root.id}:{relative_path}"))
        )
        tag_revision = hashlib.sha256(msgspec.json.encode(tag)).hexdigest()
        artist = LocalArtist(
            id=artist_id,
            display_name=album_artist,
            folded_name=album_artist.casefold(),
            normalized_name=normalize_group_value(album_artist),
            sort_name=tag.album_artist_sort,
            kind="group",
            created_at=now,
            updated_at=now,
        )
        album = LocalAlbum(
            id=album_id,
            root_id=root.id,
            grouping_key=grouping_key,
            title=album_title,
            album_artist_id=artist_id,
            album_artist_name=album_artist,
            album_artist_sort_name=tag.album_artist_sort,
            year=tag.year,
            original_release_date=tag.original_release_date,
            primary_genre=tag.genre,
            is_compilation=tag.compilation,
            grouping_source="automatic",
            created_at=now,
            updated_at=now,
        )
        track = LocalTrack(
            id=track_id,
            local_album_id=album_id,
            root_id=root.id,
            file_path=str(audio_path),
            relative_path=relative_path,
            path_hash=hashlib.sha256(relative_path.encode()).hexdigest(),
            file_size_bytes=stat_result.st_size,
            file_mtime_ns=stat_result.st_mtime_ns,
            stat_revision=revision_from_stat(stat_result),
            tag_revision=tag_revision,
            tags_read_at=now,
            title=tag.title.strip() or audio_path.stem,
            artist_name=tag.artist or album_artist,
            album_title=album_title,
            album_artist_name=album_artist,
            tag_album_title=tag.album,
            tag_album_artist_name=tag.album_artist,
            disc_number=tag.disc_number,
            track_number=tag.track_number,
            year=tag.year,
            genre=tag.genre,
            title_sort=tag.title_sort,
            artist_sort=tag.artist_sort,
            album_sort=tag.album_sort,
            album_artist_sort=tag.album_artist_sort,
            disc_subtitle=tag.disc_subtitle,
            is_compilation=tag.compilation,
            embedded_release_group_mbid=(
                release_group_mbid or tag.musicbrainz_release_group_id
            ),
            embedded_release_mbid=release_mbid or tag.musicbrainz_release_id,
            embedded_recording_mbid=recording_mbid or tag.musicbrainz_recording_id,
            embedded_release_track_mbid=tag.musicbrainz_release_track_id,
            embedded_artist_mbid=tag.musicbrainz_artist_id,
            embedded_album_artist_mbid=tag.musicbrainz_album_artist_id,
            duration_seconds=info.duration_seconds,
            file_format=info.file_format,
            bit_rate=info.bitrate,
            sample_rate=info.sample_rate,
            bit_depth=info.bit_depth,
            channels=info.channels,
            replaygain_track_gain=tag.replaygain_track_gain,
            replaygain_album_gain=tag.replaygain_album_gain,
            replaygain_track_peak=tag.replaygain_track_peak,
            replaygain_album_peak=tag.replaygain_album_peak,
            ingest_source=source,
            download_task_id=download_task_id,
            source_path=source_path,
            imported_at=now,
            desired_policy_revision=resolver.policy_revision,
            applied_policy_revision=resolver.policy_revision,
            applied_policy=policy,
            availability="excluded" if policy == "excluded" else "indexed",
            excluded_at=now if policy == "excluded" else None,
        )
        credit = LocalArtistCredit(
            local_artist_id=artist_id,
            position=0,
            credited_name=album_artist,
        )
        genres = [
            LocalTrackGenre(
                local_track_id=track_id,
                position=position,
                name=value,
                folded_name=value.casefold(),
                source="local",
                source_document_revision=tag_revision,
            )
            for position, value in enumerate(
                tag.genres or ([tag.genre] if tag.genre else [])
            )
        ]
        return ScannedTrackWrite(
            artist=artist,
            album=album,
            track=track,
            credit=credit,
            root_id=root.id,
            relative_path=relative_path,
            comparison_result="changed" if existing is not None else "new",
            grouping_context=directory,
            artists=[artist],
            album_credits=[credit],
            track_credits=[credit],
            genres=genres,
        )

    async def soft_delete_file(self, file_path: str) -> None:
        row = await self._store.get_target_track_by_path(file_path)
        if row is not None:
            await self._store.mark_target_tracks_missing(
                [str(row["id"])],
                actor_user_id=None,
                reason_code="IMPORT_REPLACED",
                missing_at=time.time(),
            )

    async def reconcile_with_filesystem(self, targets: list[Path] | None = None) -> int:
        if not targets:
            return 0

        resolver = self._resolver_getter()
        total = 0
        for target in targets:
            resolved_target = target.resolve()
            resolved = resolver.resolve(resolved_target)
            if resolved is None:
                continue

            def walk() -> set[str]:
                return {
                    str(Path(directory) / filename)
                    for directory, _subdirs, filenames in os.walk(resolved_target)
                    for filename in filenames
                    if Path(filename).suffix.casefold() in AUDIO_EXTENSIONS
                }

            if self._filesystem is None:
                present = await asyncio.to_thread(walk)
                rows = await self._store.get_target_tracks_under_paths(
                    [str(resolved_target)]
                )
                missing = [
                    str(row["id"]) for row in rows if row["file_path"] not in present
                ]
                if missing:
                    await self._store.mark_target_tracks_missing(
                        missing,
                        actor_user_id=None,
                        reason_code="IMPORT_RECONCILE_MISSING",
                        missing_at=time.time(),
                    )
            else:
                async with self._filesystem.read(resolved.root_id):
                    present = await asyncio.to_thread(walk)
                    rows = await self._store.get_target_tracks_under_paths(
                        [str(resolved_target)]
                    )
                    missing = [
                        str(row["id"])
                        for row in rows
                        if row["file_path"] not in present
                    ]
                    if missing:
                        await self._store.mark_target_tracks_missing(
                            missing,
                            actor_user_id=None,
                            reason_code="IMPORT_RECONCILE_MISSING",
                            missing_at=time.time(),
                        )
            total += len(missing)
        return total

    async def get_attributions_for_paths(self, paths: list[str]) -> dict[str, dict]:
        rows = await self._store.get_target_attributions_for_paths(paths)
        for row in rows.values():
            row["release_group_mbid"] = row.get("provider_release_group_mbid")
            row["confidence"] = 1.0
            row["source"] = row.get("ingest_source")
        return rows

    async def get_file_at_position(
        self, album_id: str, disc_number: int, track_number: int
    ) -> dict | None:
        return next(
            (
                row
                for row in await self._store.get_target_album_tracks(album_id)
                if int(row["disc_number"] or 1) == disc_number
                and int(row["track_number"] or 0) == track_number
            ),
            None,
        )

    async def get_imported_file(
        self, download_task_id: str, filename: str
    ) -> dict | None:
        if not download_task_id:
            return None
        return await self._store.get_target_imported_track(download_task_id, filename)
