"""Bounded, external-service-free indexing for discovered target inventory."""

from __future__ import annotations

import asyncio
import hashlib
import os
import time
import unicodedata
import uuid
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Protocol

import msgspec

from core.exceptions import AudioFormatError
from infrastructure.audio.metadata_engine import legacy_audio_projection
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioArtistCredit, AudioInfo, AudioTag
from models.library_work import ScanRun, ScannedTrackWrite
from models.local_catalog import (
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
    LocalTrackGenre,
)
from services.native.identification_queue_service import IdentificationQueueService
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.local_album_grouper import grouping_directory
from services.native.local_album_grouping_service import LocalAlbumGroupingService
from services.native.file_revision import revision_from_stat

INDEX_FETCH_SIZE = 256
TAG_BATCH_SIZE = 64
INDEX_BATCH_SIZE = TAG_BATCH_SIZE
CHECKPOINT_INTERVAL_SECONDS = 0.25
_SCAN_NAMESPACE = uuid.UUID("c65f5557-43c6-4550-bd8a-ea7dcaac6411")


def _fold_value(value: str) -> str:
    decomposed = unicodedata.normalize("NFKD", value.strip())
    without_marks = "".join(
        character for character in decomposed if not unicodedata.combining(character)
    ).casefold()
    return " ".join(without_marks.split())


class AudioTagReaderProtocol(Protocol):
    def read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]: ...


IndexProgressCallback = Callable[[ScanRun], Awaitable[None]]


class LibraryIndexer:
    def __init__(
        self,
        store: NativeLibraryStore,
        tag_reader: AudioTagReaderProtocol,
        grouping: LocalAlbumGroupingService | None = None,
        filesystem_coordinator: LibraryFilesystemCoordinator | None = None,
    ) -> None:
        self._store = store
        self._tag_reader = tag_reader
        self._grouping = grouping or LocalAlbumGroupingService(
            store, IdentificationQueueService(store)
        )
        self._filesystem = filesystem_coordinator

    async def index(
        self,
        run: ScanRun,
        frozen_policy_revision: str = "",
        checkpoint: Callable[[str, str], Awaitable[bool]] | None = None,
        progress: IndexProgressCallback | None = None,
    ) -> dict[str, int]:
        counts = {
            "indexed": 0,
            "new": 0,
            "changed": 0,
            "unchanged": 0,
            "excluded": 0,
            "errored": 0,
            "tag_reads": 0,
            "identification_enqueued": 0,
        }
        while True:
            await self._store.normalize_pending_legacy_inventory(
                run.id, limit=INDEX_FETCH_SIZE
            )
            batch = await self._store.get_scan_inventory_batch(
                run.id, processing_state="pending", limit=INDEX_FETCH_SIZE
            )
            if not batch:
                counts["identification_enqueued"] += await self._grouping.regroup_run(
                    run.id,
                    now=run.updated_at or run.queued_at,
                    checkpoint=checkpoint,
                    frozen_policy_revision=frozen_policy_revision,
                )
                return counts
            batch_counts = {
                "indexed": 0,
                "new": 0,
                "changed": 0,
                "unchanged": 0,
                "excluded": 0,
                "errored": 0,
            }
            writes: list[ScannedTrackWrite] = []
            states: dict[str, list[tuple[str, str]]] = {
                "unchanged": [],
                "excluded": [],
            }
            failures: list[tuple[str, str, str]] = []
            last_checkpoint = time.monotonic()
            for item in batch:
                if len(writes) >= TAG_BATCH_SIZE:
                    break
                if checkpoint is not None and (
                    time.monotonic() - last_checkpoint >= CHECKPOINT_INTERVAL_SECONDS
                ):
                    if not await checkpoint(run.id, frozen_policy_revision):
                        return counts
                    last_checkpoint = time.monotonic()
                key = [(str(item["root_id"]), str(item["relative_path"]))]
                if item["comparison_result"] == "excluded" or (
                    item["comparison_result"] == "unchanged"
                    and run.kind != "rescan_files"
                ):
                    state = (
                        "unchanged"
                        if item["comparison_result"] == "unchanged"
                        else "excluded"
                    )
                    states[state].extend(key)
                    batch_counts["unchanged"] += (
                        item["comparison_result"] == "unchanged"
                    )
                    batch_counts["excluded"] += item["comparison_result"] == "excluded"
                    continue
                try:
                    stable = await self._read_stable_tags(
                        item,
                        run_id=run.id,
                        frozen_policy_revision=frozen_policy_revision,
                        checkpoint=checkpoint,
                    )
                    counts["tag_reads"] += stable[3]
                    if stable[4]:
                        return counts
                    if stable[0] is None or stable[1] is None or stable[2] is None:
                        failures.append((*key[0], "FILE_CHANGED_DURING_READ"))
                        batch_counts["errored"] += 1
                        continue
                    tag, info, stat = stable[:3]
                    tagged_item = {
                        **item,
                        "file_size_bytes": stat.st_size,
                        "file_mtime_ns": stat.st_mtime_ns,
                        "stat_revision": revision_from_stat(stat),
                    }
                    writes.append(self._prepare_tagged(run.id, tagged_item, tag, info))
                    if checkpoint is not None and not await checkpoint(
                        run.id, frozen_policy_revision
                    ):
                        return counts
                    batch_counts["indexed"] += 1
                    if item["comparison_result"] == "new":
                        batch_counts["new"] += 1
                    elif item["comparison_result"] == "changed":
                        batch_counts["changed"] += 1
                except (AudioFormatError, OSError, ValueError):
                    failures.append((*key[0], "TAG_READ_FAILED"))
                    batch_counts["errored"] += 1
            if checkpoint is not None and not await checkpoint(
                run.id, frozen_policy_revision
            ):
                return counts
            increments = {
                "inspected_count": sum(
                    batch_counts[name]
                    for name in ("indexed", "unchanged", "excluded", "errored")
                ),
                "new_count": batch_counts["new"],
                "changed_count": batch_counts["changed"],
                "indexed_count": batch_counts["indexed"],
                "unchanged_count": batch_counts["unchanged"],
                "excluded_count": batch_counts["excluded"],
                "errored_count": batch_counts["errored"],
            }
            run = await self._store.commit_scan_index_batch(
                run.id,
                writes=writes,
                states=states,
                failures=failures,
                increments=increments,
                updated_at=time.time(),
            )
            for name in (
                "indexed",
                "new",
                "changed",
                "unchanged",
                "excluded",
                "errored",
            ):
                counts[name] += batch_counts[name]
            await self._report_progress(run, progress)

    async def _read_stable_tags(
        self,
        item: dict[str, object],
        *,
        run_id: str,
        frozen_policy_revision: str,
        checkpoint: Callable[[str, str], Awaitable[bool]] | None,
    ) -> tuple[AudioTag | None, AudioInfo | None, os.stat_result | None, int, bool]:
        path = Path(str(item["absolute_path"]))
        expected_size = int(item["file_size_bytes"])
        expected_mtime_ns = int(item["file_mtime_ns"])
        root_id = str(item["root_id"])
        reads = 0
        for _attempt in range(2):
            if self._filesystem is None:
                tag, info = await asyncio.to_thread(self._read_tags, path)
                stat = await asyncio.to_thread(path.stat)
            else:
                async with self._filesystem.read(root_id):
                    tag, info = await asyncio.to_thread(self._read_tags, path)
                    stat = await asyncio.to_thread(path.stat)
            reads += 1
            if checkpoint is not None and not await checkpoint(
                run_id, frozen_policy_revision
            ):
                return None, None, None, reads, True
            if stat.st_size == expected_size and stat.st_mtime_ns == expected_mtime_ns:
                return tag, info, stat, reads, False
            expected_size = stat.st_size
            expected_mtime_ns = stat.st_mtime_ns
        return None, None, None, reads, False

    def _read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]:
        richer_reader = getattr(self._tag_reader, "read_document", None)
        if callable(richer_reader):
            return legacy_audio_projection(richer_reader(path))
        return self._tag_reader.read_tags(path)

    @staticmethod
    async def _report_progress(
        run: ScanRun, progress: IndexProgressCallback | None
    ) -> None:
        if progress is None:
            return
        await progress(run)

    def _prepare_tagged(
        self,
        run_id: str,
        item: dict[str, object],
        tag: AudioTag,
        info: AudioInfo,
    ) -> ScannedTrackWrite:
        root_id = str(item["root_id"])
        relative_path = str(item["relative_path"])
        now = float(item["file_mtime_ns"]) / 1_000_000_000
        directory = grouping_directory(relative_path)
        album_title = tag.album.strip()
        if not album_title:
            album_title = Path(relative_path).stem
        album_artist = (tag.album_artist or tag.artist or "Unknown Artist").strip()
        album_audio_credits = tag.album_artists or [
            AudioArtistCredit(
                name=album_artist,
                credited_name=album_artist,
                sort_name=tag.album_artist_sort,
                musicbrainz_artist_id=tag.musicbrainz_album_artist_id,
            )
        ]
        track_artist = (tag.artist or album_artist).strip()
        track_audio_credits = tag.artists or [
            AudioArtistCredit(
                name=track_artist,
                credited_name=track_artist,
                sort_name=tag.artist_sort,
                musicbrainz_artist_id=tag.musicbrainz_artist_id,
            )
        ]

        def candidate_artist(credit: AudioArtistCredit) -> LocalArtist:
            candidate_id = str(
                uuid.uuid5(
                    _SCAN_NAMESPACE,
                    f"artist:{credit.name}:{credit.sort_name or ''}:group",
                )
            )
            return LocalArtist(
                id=candidate_id,
                display_name=credit.name,
                folded_name=_fold_value(credit.name),
                kind="group",
                normalized_name=_fold_value(credit.name),
                sort_name=credit.sort_name,
                created_at=now,
                updated_at=now,
            )

        artist_by_candidate_id = {
            artist.id: artist
            for artist in (
                candidate_artist(credit)
                for credit in [*album_audio_credits, *track_audio_credits]
            )
        }
        album_artists = [candidate_artist(credit) for credit in album_audio_credits]
        track_artists = [candidate_artist(credit) for credit in track_audio_credits]
        primary_album_artist = album_artists[0]
        artist_candidate_id = primary_album_artist.id
        grouping_key = (
            f"{directory}\x00{album_title.casefold()}\x00{album_artist.casefold()}"
        )
        album_id = str(uuid.uuid5(_SCAN_NAMESPACE, f"album:{root_id}:{grouping_key}"))
        track_id = str(
            item["local_track_id"]
            or uuid.uuid5(_SCAN_NAMESPACE, f"track:{root_id}:{relative_path}")
        )
        tag_revision = hashlib.sha256(msgspec.json.encode(tag)).hexdigest()
        artist = primary_album_artist
        album = LocalAlbum(
            id=album_id,
            root_id=root_id,
            grouping_key=grouping_key,
            title=album_title or "Unknown Album",
            album_artist_id=artist_candidate_id,
            album_artist_name=album_artist,
            album_artist_sort_name=tag.album_artist_sort,
            year=tag.year,
            original_release_date=tag.original_release_date,
            primary_genre=tag.genre,
            is_compilation=tag.compilation,
            created_at=now,
            updated_at=now,
        )
        track = LocalTrack(
            id=track_id,
            local_album_id=album_id,
            root_id=root_id,
            file_path=str(item["absolute_path"]),
            relative_path=relative_path,
            path_hash=hashlib.sha256(relative_path.encode()).hexdigest(),
            file_size_bytes=int(item["file_size_bytes"]),
            file_mtime_ns=int(item["file_mtime_ns"]),
            stat_revision=str(item["stat_revision"]),
            tag_revision=tag_revision,
            tags_read_at=now,
            title=tag.title.strip() or Path(relative_path).stem,
            artist_name=tag.artist or album_artist,
            album_title=album.title,
            album_artist_name=album_artist,
            tag_album_title=tag.album.strip(),
            tag_album_artist_name=(tag.album_artist or "").strip(),
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
            embedded_release_group_mbid=tag.musicbrainz_release_group_id,
            embedded_release_mbid=tag.musicbrainz_release_id,
            embedded_recording_mbid=tag.musicbrainz_recording_id,
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
            imported_at=now,
            desired_policy_revision=str(item.get("policy_revision", "")),
            applied_policy_revision=str(item.get("policy_revision", "")),
            applied_policy=item["effective_policy"],
        )
        album_credits = [
            LocalArtistCredit(
                local_artist_id=album_artists[position].id,
                position=position,
                credited_name=credit.credited_name or credit.name,
                join_phrase=credit.join_phrase,
            )
            for position, credit in enumerate(album_audio_credits)
        ]
        track_credits = [
            LocalArtistCredit(
                local_artist_id=track_artists[position].id,
                position=position,
                credited_name=credit.credited_name or credit.name,
                join_phrase=credit.join_phrase,
            )
            for position, credit in enumerate(track_audio_credits)
        ]
        genre_values = tag.genres or ([tag.genre] if tag.genre else [])
        genres = [
            LocalTrackGenre(
                local_track_id=track_id,
                position=position,
                name=value,
                folded_name=_fold_value(value),
                source="local",
                source_document_revision=tag_revision,
            )
            for position, value in enumerate(genre_values)
        ]
        return ScannedTrackWrite(
            artist=artist,
            album=album,
            track=track,
            credit=LocalArtistCredit(
                local_artist_id=artist_candidate_id,
                position=0,
                credited_name=album_artist,
            ),
            root_id=root_id,
            relative_path=relative_path,
            comparison_result=item["comparison_result"],
            grouping_context=directory,
            artists=list(artist_by_candidate_id.values()),
            album_credits=album_credits,
            track_credits=track_credits,
            genres=genres,
        )
