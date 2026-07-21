"""Download/import library contract backed only by the target catalog store."""

from __future__ import annotations

import asyncio
import hashlib
import os
import time
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

import msgspec

from models.local_catalog import LocalAlbum, LocalArtist, LocalArtistCredit, LocalTrack
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
from core.exceptions import StaleRevisionError

if TYPE_CHECKING:
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from models.audio import AudioInfo, AudioTag
    from services.native.identification_queue_service import IdentificationQueueService
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
    ) -> None:
        self._store = store
        self._resolver_getter = resolver_getter
        self._queue = identification_queue
        self._policy_transition_lock = policy_transition_lock or asyncio.Lock()

    def is_configured(self) -> bool:
        return bool(self._resolver_getter().settings.library_roots)

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

        def walk() -> set[str]:
            return {
                str(Path(directory) / filename)
                for target in targets
                for directory, _subdirs, filenames in os.walk(target)
                for filename in filenames
                if Path(filename).suffix.casefold() in AUDIO_EXTENSIONS
            }

        present = await asyncio.to_thread(walk)
        rows = await self._store.get_target_tracks_under_paths(
            [str(path.resolve()) for path in targets]
        )
        missing = [str(row["id"]) for row in rows if row["file_path"] not in present]
        if missing:
            await self._store.mark_target_tracks_missing(
                missing,
                actor_user_id=None,
                reason_code="IMPORT_RECONCILE_MISSING",
                missing_at=time.time(),
            )
        return len(missing)

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
