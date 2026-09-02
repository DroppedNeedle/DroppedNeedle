"""FreeMusicService - DroppedNeedle's own lawful download client (D24).

Requests are served from the Internet Archive, restricted to items carrying an
explicit Creative Commons or public-domain licence. Downloaded files are handed
to the drop-import pipeline (01c), which identifies them against MusicBrainz,
tags, organises, resolves the request, and notifies the requester.

Why this exists: DroppedNeedle's download capability is lawful *because* it has
a demonstrated lawful use. Free Music is that use. A broken Free Music client is
a P1, not a curiosity. See .dev-notes/Plans/FreeMusic/00-PLAN.md.
"""

import asyncio
import json
import logging
import shutil
import time
import uuid
from pathlib import Path
from typing import TYPE_CHECKING

from core.exceptions import ResourceNotFoundError, ValidationError
from models.download_manifest import DownloadManifest, ExpectedTrack
from models.free_music import FreeMusicCandidate, FreeMusicStatus, FreeMusicTask
from services.native.acquisition import quality as acq_quality
from services.native.acquisition.local_probe import (
    expected_vs_actual_copy,
    probe_files_sync,
)
from services.native.title_match import title_containment_score

if TYPE_CHECKING:
    from infrastructure.persistence.free_music_store import FreeMusicStore
    from infrastructure.sse_publisher import SSEPublisher
    from repositories.archive_repository import ArchiveRepository
    from services.native.drop_import_service import DropImportService
    from services.native.file_processor import FileProcessor
    from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

# A candidate whose title bears no resemblance to the requested album is a
# different record that merely shares an artist. The Archive is full of remasters,
# tributes, and live sets - none of them the album that was asked for.
_TITLE_MATCH_FLOOR = 0.60

# Write progress to SQLite at most this often; the download loop updates far faster.
_PROGRESS_WRITE_INTERVAL = 1.0
_CHUNK = 1024 * 256


class FreeMusicService:
    def __init__(
        self,
        *,
        store: "FreeMusicStore",
        archive: "ArchiveRepository",
        drop_import: "DropImportService",
        preferences_service: "PreferencesService",
        sse_publisher: "SSEPublisher",
        file_processor: "FileProcessor | None" = None,
        probe_tagger=None,  # shared AudioTagger for local quality probes (None in tests)
    ) -> None:
        self._store = store
        self._archive = archive
        self._drop_import = drop_import
        self._prefs = preferences_service
        self._sse = sse_publisher
        self._file_processor = file_processor
        self._probe_tagger = probe_tagger
        self._tasks: dict[str, asyncio.Task] = {}
        self._cancels: dict[str, asyncio.Event] = {}
        self._lifecycle_locks: dict[str, asyncio.Lock] = {}

    # mirrors DownloadService's dispatch surface

    def is_ready(self) -> bool:
        return self._prefs.get_free_music_settings().enabled

    async def request_album(
        self,
        *,
        user_id: str,
        release_group_mbid: str,
        artist_name: str,
        album_title: str,
        track_count: int = 0,
    ) -> str:
        return await self._start(
            user_id=user_id,
            kind="album",
            mbid=release_group_mbid,
            artist=artist_name,
            title=album_title,
            track_count=track_count,
        )

    async def request_track(
        self,
        *,
        user_id: str,
        recording_mbid: str,
        artist_name: str,
        track_title: str,
        origin: str = "user",
        release_group_mbid: str | None = None,
        release_mbid: str | None = None,
        release_track_mbid: str | None = None,
        duration_seconds: float | None = None,
        album_title: str | None = None,
        track_number: int | None = None,
        disc_number: int | None = None,
    ) -> str:
        return await self._start(
            user_id=user_id,
            kind="track",
            mbid=recording_mbid,
            artist=artist_name,
            title=track_title,
            origin=origin,
            release_group_mbid=release_group_mbid,
            release_mbid=release_mbid,
            release_track_mbid=release_track_mbid,
            recording_mbid=recording_mbid,
            duration_seconds=duration_seconds,
            album_title=album_title,
            track_number=track_number,
            disc_number=disc_number,
        )

    async def list_tasks(
        self, *, user_id: str, include_all: bool
    ) -> list[FreeMusicTask]:
        return await self._store.list_tasks(user_id=None if include_all else user_id)

    async def get_task(
        self, task_id: str, *, user_id: str, is_admin: bool
    ) -> FreeMusicTask:
        task = await self._store.get(task_id)
        if task is None or (task.user_id != user_id and not is_admin):
            raise ResourceNotFoundError("Download not found")
        return task

    async def cancel(
        self, task_id: str, *, user_id: str, is_admin: bool
    ) -> FreeMusicTask:
        task = await self.get_task(task_id, user_id=user_id, is_admin=is_admin)
        if task.status in FreeMusicStatus.TERMINAL:
            raise ValidationError("That download has already finished")
        lock = self._lifecycle_locks.get(task_id)
        if lock is None:
            raise ValidationError("That download is no longer running")
        async with lock:
            current = await self._store.get(task_id)
            if current is None:
                raise ResourceNotFoundError("Download not found")
            if current.status == FreeMusicStatus.IMPORTING:
                raise ValidationError(
                    "This download is already being added to your library"
                )
            event = self._cancels.get(task_id)
            if event is not None:
                event.set()
            cancelled = await self._store.cancel_active(task_id)
        if cancelled is None:
            raise ValidationError("That download has already finished")
        await self._publish(cancelled.user_id, task_id, FreeMusicStatus.CANCELLED)
        return cancelled

    async def retry(
        self, task_id: str, *, user_id: str, is_admin: bool
    ) -> FreeMusicTask:
        task = await self.get_task(task_id, user_id=user_id, is_admin=is_admin)
        if task.status not in (FreeMusicStatus.FAILED, FreeMusicStatus.CANCELLED):
            raise ValidationError("Only a failed or cancelled download can be retried")
        running = self._tasks.get(task_id)
        if running is not None and not running.done():
            raise ValidationError("Wait for this download to stop before retrying")
        if not await self._store.restart_terminal(task_id):
            raise ValidationError("That download is no longer available to retry")
        refreshed = await self._store.get(task_id)
        assert refreshed is not None
        self._spawn(task_id, refreshed)
        return refreshed

    async def remove(self, task_id: str, *, user_id: str, is_admin: bool) -> None:
        task = await self.get_task(task_id, user_id=user_id, is_admin=is_admin)
        if task.status not in FreeMusicStatus.TERMINAL:
            raise ValidationError("Cancel this download before removing it")
        removed = await self._store.delete_terminal(task_id)
        if removed is None:
            raise ValidationError("That download is no longer available to remove")
        await self._publish(removed.user_id, task_id, "removed")

    async def clear_history(self, *, user_id: str, include_all: bool) -> int:
        removed = await self._store.delete_terminal_tasks(
            user_id=None if include_all else user_id
        )
        for owner_id in {owner_id for _task_id, owner_id in removed}:
            await self._publish(owner_id, "", "removed")
        return len(removed)

    async def sweep_stale(self) -> None:
        failed = await self._store.fail_stale(
            "Interrupted by a restart. Request it again."
        )
        if failed:
            logger.info("free_music.stale_failed", extra={"tasks": failed})

    async def _start(
        self,
        *,
        user_id: str,
        kind: str,
        mbid: str,
        artist: str,
        title: str,
        track_count: int = 0,
        origin: str = "user",
        release_group_mbid: str | None = None,
        release_mbid: str | None = None,
        release_track_mbid: str | None = None,
        recording_mbid: str | None = None,
        duration_seconds: float | None = None,
        album_title: str | None = None,
        track_number: int | None = None,
        disc_number: int | None = None,
    ) -> str:
        if not self.is_ready():
            raise ValidationError("Free Music is not enabled")
        task_id = uuid.uuid4().hex
        task = FreeMusicTask(
            id=task_id,
            user_id=user_id,
            kind=kind,
            mbid=mbid,
            artist=artist,
            title=title,
            status=FreeMusicStatus.SEARCHING,
            created_at=time.time(),
            updated_at=time.time(),
            track_count=max(0, track_count),
            origin=origin,
            release_group_mbid=release_group_mbid,
            release_mbid=release_mbid,
            release_track_mbid=release_track_mbid,
            recording_mbid=recording_mbid,
            duration_seconds=duration_seconds,
            album_title=album_title,
            track_number=track_number,
            disc_number=disc_number,
        )
        # The policy snapshot is pinned at CREATION and never refreshed by later
        # settings saves (retry keeps it; the native restart action refreshes).
        snapshot = acq_quality.build_snapshot(self._prefs.get_download_policy())
        blob = acq_quality.encode_snapshot(snapshot)
        task.quality_snapshot_json = blob
        task.quality_snapshot_hash = snapshot.snapshot_hash
        task.quality_snapshot_summary = snapshot.summary
        await self._store.create(
            task_id,
            user_id,
            kind,
            mbid,
            artist,
            title,
            track_count=max(0, track_count),
            origin=origin,
            release_group_mbid=release_group_mbid,
            release_mbid=release_mbid,
            release_track_mbid=release_track_mbid,
            recording_mbid=recording_mbid,
            duration_seconds=duration_seconds,
            album_title=album_title,
            track_number=track_number,
            disc_number=disc_number,
            quality_snapshot_json=blob,
            quality_snapshot_hash=snapshot.snapshot_hash,
            quality_snapshot_summary=snapshot.summary,
        )
        self._spawn(task_id, task)
        return task_id

    def _spawn(self, task_id: str, task: FreeMusicTask) -> None:
        cancel = asyncio.Event()
        lifecycle_lock = asyncio.Lock()
        self._cancels[task_id] = cancel
        self._lifecycle_locks[task_id] = lifecycle_lock
        handle = asyncio.create_task(
            self._run_guarded(task_id, task, cancel, lifecycle_lock)
        )
        self._tasks[task_id] = handle
        handle.add_done_callback(lambda t, tid=task_id: self._on_done(tid, t))

    def _on_done(self, task_id: str, handle: asyncio.Task) -> None:
        if self._tasks.get(task_id) is handle:
            self._tasks.pop(task_id, None)
            self._cancels.pop(task_id, None)
            self._lifecycle_locks.pop(task_id, None)
        if not handle.cancelled() and handle.exception() is not None:
            logger.error(
                "free_music task %s crashed", task_id, exc_info=handle.exception()
            )

    async def _run_guarded(
        self,
        task_id: str,
        task: FreeMusicTask,
        cancel: asyncio.Event,
        lifecycle_lock: asyncio.Lock,
    ) -> None:
        try:
            await self._run(task_id, task, cancel, lifecycle_lock)
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("Free Music task %s failed", task_id)
            await self._fail(task_id, task.user_id, "Something went wrong. Try again.")

    async def _run(
        self,
        task_id: str,
        task: FreeMusicTask,
        cancel: asyncio.Event,
        lifecycle_lock: asyncio.Lock,
    ) -> None:
        try:
            snapshot = self._task_snapshot(task)
            candidates = await self._find_candidates(task, task.track_count, snapshot)
        except ValidationError:
            logger.warning("free_music.snapshot_invalid task=%s", task.id)
            await self._fail(
                task_id, task.user_id, "Stored quality policy snapshot is invalid."
            )
            return
        except Exception as exc:  # noqa: BLE001 - surfaced to the user, not swallowed
            logger.warning("free_music.search_failed mbid=%s: %s", task.mbid, exc)
            await self._fail(
                task_id, task.user_id, "Couldn't reach the Internet Archive."
            )
            return

        filtered: list[FreeMusicCandidate] = []
        for candidate in candidates:
            evidence = acq_quality.evidence_from_archive_format(candidate.format)
            if snapshot.flac_mp3_only and evidence.extension not in {"flac", "mp3"}:
                continue
            decision = acq_quality.evaluate(snapshot, evidence)
            # Keep soft outside-policy/review candidates provisionally: an exact
            # local probe may prove that the archive label understated quality.
            # Unknown-rejected and hard importability/cap failures never spend
            # bytes, regardless of whether the snapshot is v1 or v2.
            if (
                decision.disposition != "unknown_rejected"
                and not acq_quality.is_hard_quality_rejection(decision)
            ):
                filtered.append(candidate)
        candidates = filtered
        if not candidates:
            await self._fail(
                task_id, task.user_id, "No source has this - try buying it instead."
            )
            return
        if cancel.is_set():
            return

        stored_raw = task.tried_candidates_json or "[]"
        try:
            tried: list[dict] = json.loads(stored_raw)
        except (TypeError, ValueError) as exc:
            logger.warning("free_music.ladder_decode_failed task=%s", task.id)
            await self._fail(
                task_id, task.user_id, "Stored candidate history is invalid."
            )
            return
        if not isinstance(tried, list) or any(
            not isinstance(item, dict) for item in tried
        ):
            await self._fail(
                task_id, task.user_id, "Stored candidate history is invalid."
            )
            return
        if tried:
            candidates = self._order_candidates_from_ladder(candidates, tried)
            if not candidates:
                await self._fail(task_id, task.user_id, "No remaining source has this.")
                return
        await self._persist_candidate_ladder(task_id, task, candidates)
        if not tried:
            tried = [
                {"identifier": candidate.identifier, "format": candidate.format}
                for candidate in candidates
            ]

        excluded = {
            (entry.get("identifier"), entry.get("format"))
            for entry in tried
            if entry.get("reason")
            or entry.get("outcome")
            in {
                "completed",
                "post_download_quality_mismatch",
                "quality_probe_unavailable",
            }
        }
        queue = [c for c in candidates if (c.identifier, c.format) not in excluded]

        last_quality_note: str | None = None
        dest = self._drop_import.incoming_dir() / f"free-{task_id}"
        files: list[Path] | None = None
        best: FreeMusicCandidate | None = None

        while queue and not cancel.is_set() and files is None:
            candidate = queue.pop(0)
            downloading = await self._store.update(
                task_id,
                status=FreeMusicStatus.DOWNLOADING,
                identifier=candidate.identifier,
                licence_url=candidate.licence_url,
                format=candidate.extension,
                files_total=len(candidate.filenames),
                bytes_total=candidate.size_bytes,
                attempts=1,
                expected_statuses=(
                    FreeMusicStatus.SEARCHING,
                    FreeMusicStatus.DOWNLOADING,
                ),
            )
            if not downloading:
                return
            await self._publish(task.user_id, task_id, FreeMusicStatus.DOWNLOADING)

            try:
                downloaded = await self._download_with_retry(
                    task_id, task, candidate, dest, cancel
                )
            except _Cancelled:
                await asyncio.to_thread(shutil.rmtree, dest, True)
                return
            except Exception as exc:  # noqa: BLE001 - user is waiting; report it
                logger.warning("free_music.download_failed task=%s: %s", task_id, exc)
                await asyncio.to_thread(shutil.rmtree, dest, True)
                await self._fail(
                    task_id, task.user_id, "The download failed. Try again."
                )
                return

            if not downloaded:
                await asyncio.to_thread(shutil.rmtree, dest, True)
                continue  # move on to the next candidate/format

            expected_evidence = acq_quality.evidence_from_archive_format(
                candidate.format
            )
            expected_decision = acq_quality.evaluate(snapshot, expected_evidence)
            requires_exact_probe = expected_decision.disposition in {
                "needs_review",
                "outside_policy",
            }
            probed = self._probe_downloaded(downloaded)
            if requires_exact_probe and probed is None:
                last_quality_note = (
                    "Downloaded copy could not be locally quality-verified; "
                    "trying the next source."
                )
                self._record_candidate_outcome(
                    tried, candidate, "quality_probe_unavailable"
                )
                await self._store.update(
                    task_id,
                    tried_candidates_json=json.dumps(tried),
                    bytes_downloaded=0,
                    expected_statuses=(FreeMusicStatus.DOWNLOADING,),
                )
                await asyncio.to_thread(shutil.rmtree, dest, True)
                continue
            if probed is not None:
                from services.native.acquisition.local_probe import quality_mismatch

                if quality_mismatch(snapshot, expected_decision, probed):
                    logger.info(
                        "free_music.quality_mismatch task=%s fmt=%s",
                        task_id,
                        candidate.format,
                    )
                    last_quality_note = (
                        "Downloaded copy didn't match the server's quality "
                        f"policy ({expected_vs_actual_copy(expected_decision, probed)})"
                    )
                    self._record_candidate_outcome(
                        tried, candidate, "post_download_quality_mismatch"
                    )
                    await self._store.update(
                        task_id,
                        tried_candidates_json=json.dumps(tried),
                        bytes_downloaded=0,
                        expected_statuses=(FreeMusicStatus.DOWNLOADING,),
                    )
                    await asyncio.to_thread(shutil.rmtree, dest, True)
                    continue

            self._record_candidate_outcome(tried, candidate, "completed")
            await self._store.update(
                task_id,
                tried_candidates_json=json.dumps(tried),
                expected_statuses=(FreeMusicStatus.DOWNLOADING,),
            )
            files = downloaded
            best = candidate

        if cancel.is_set():
            return
        if files is None or best is None:
            message = last_quality_note or "The download produced no files."
            await asyncio.to_thread(shutil.rmtree, dest, True)
            await self._fail(task_id, task.user_id, message)
            return

        async with lifecycle_lock:
            if cancel.is_set():
                await asyncio.to_thread(shutil.rmtree, dest, True)
                return
            importing = await self._store.update(
                task_id,
                status=FreeMusicStatus.IMPORTING,
                expected_statuses=(FreeMusicStatus.DOWNLOADING,),
            )
        if not importing:
            await asyncio.to_thread(shutil.rmtree, dest, True)
            return
        await self._publish(task.user_id, task_id, FreeMusicStatus.IMPORTING)
        try:
            if task.origin == "edition_conversion":
                if (
                    self._file_processor is None
                    or not task.release_group_mbid
                    or not task.release_mbid
                    or not task.release_track_mbid
                    or not task.recording_mbid
                ):
                    raise ValidationError(
                        "The exact-edition conversion target is incomplete."
                    )
                result = await self._file_processor.process_downloaded_folder(
                    DownloadManifest(
                        task_id=task.id,
                        release_group_mbid=task.release_group_mbid,
                        release_mbid=task.release_mbid,
                        artist_name=task.artist,
                        album_title=task.album_title or task.title,
                        naming_template=(
                            self._prefs.get_download_policy().naming_template
                        ),
                        target_files=[],
                        expected_tracks=[
                            ExpectedTrack(
                                track_number=task.track_number or 1,
                                disc_number=task.disc_number or 1,
                                duration_seconds=task.duration_seconds,
                                recording_mbid=task.recording_mbid,
                                title=task.title,
                                release_track_mbid=task.release_track_mbid,
                            )
                        ],
                        is_track=True,
                        origin="edition_conversion",
                        requested_by_user_id=task.user_id,
                    ),
                    files,
                )
                if not result.succeeded:
                    raise ValidationError(
                        "Free Music could not verify the requested recording."
                    )
            else:
                # The drop importer identifies, tags, organises, resolves the
                # request, and notifies the requester.
                await self._drop_import.create_job(
                    user_id=task.user_id,
                    user_name="Free Music",
                    uploads=[(f.name, f) for f in files],
                )
        finally:
            await asyncio.to_thread(shutil.rmtree, dest, True)

        completed = await self._store.update(
            task_id,
            status=FreeMusicStatus.COMPLETED,
            expected_statuses=(FreeMusicStatus.IMPORTING,),
        )
        if not completed:
            return
        await self._publish(task.user_id, task_id, FreeMusicStatus.COMPLETED)
        logger.info(
            "free_music.completed",
            extra={
                "task_id": task_id,
                "identifier": best.identifier,
                "files": len(files),
            },
        )

    def _probe_downloaded(self, files: list[Path]):
        """Local codec-aware probe before publication. Returns None (probe
        skipped) only when no tagger was wired - tests build the service that
        way; production composition always passes one."""
        if self._probe_tagger is None:
            return None
        return probe_files_sync(files, self._probe_tagger)

    @staticmethod
    def _order_candidates_from_ladder(
        candidates: list[FreeMusicCandidate], ladder: list[dict]
    ) -> list[FreeMusicCandidate]:
        """Use the persisted candidate order; never let a fresh search reorder it."""
        by_key: dict[tuple[str, str], list[FreeMusicCandidate]] = {}
        for candidate in candidates:
            by_key.setdefault((candidate.identifier, candidate.format), []).append(
                candidate
            )
        ordered: list[FreeMusicCandidate] = []
        for entry in ladder:
            key = (entry.get("identifier"), entry.get("format"))
            matches = by_key.get(key)
            if matches:
                ordered.append(matches.pop(0))
        return ordered

    @staticmethod
    def _record_candidate_outcome(
        ladder: list[dict],
        candidate: FreeMusicCandidate,
        outcome: str,
    ) -> None:
        key = (candidate.identifier, candidate.format)
        for entry in ladder:
            if (entry.get("identifier"), entry.get("format")) == key:
                entry["outcome"] = outcome
                entry.pop("reason", None)
                return
        # Preserve compatibility with an older row whose ladder omitted this
        # candidate; never reorder or replace the entries already persisted.
        ladder.append({"identifier": key[0], "format": key[1], "outcome": outcome})

    async def _persist_candidate_ladder(
        self,
        task_id: str,
        task: FreeMusicTask,
        candidates: list[FreeMusicCandidate],
    ) -> None:
        """Write the ranked ladder once, before the first byte moves."""
        if task.tried_candidates_json and task.tried_candidates_json != "[]":
            return
        ladder = [{"identifier": c.identifier, "format": c.format} for c in candidates]
        await self._store.update(
            task_id,
            tried_candidates_json=json.dumps(ladder),
            expected_statuses=(FreeMusicStatus.SEARCHING,),
        )

    def _select_files(self, task: FreeMusicTask, entries: list) -> list:
        """An album takes every file of its format; a track takes the one whose
        title matches."""
        if task.kind == "album":
            return sorted(entries, key=lambda e: (e.track or 0, e.name))
        best = None
        best_score = 0.0
        for entry in entries:
            score = title_containment_score(task.title, entry.title or entry.name)
            if score > best_score:
                best, best_score = entry, score
        return [best] if best is not None and best_score >= _TITLE_MATCH_FLOOR else []

    async def _find_candidates(
        self, task: FreeMusicTask, track_count: int, snapshot
    ) -> list[FreeMusicCandidate]:
        items = await self._archive.search_audio(task.artist, task.title)

        candidates: list[FreeMusicCandidate] = []
        for item in items:
            if title_containment_score(task.title, item.title) < _TITLE_MATCH_FLOOR:
                continue
            licence, files = await self._archive.get_item_files(item.identifier)
            if not licence or not files:
                continue  # dark item, or one whose licence we cannot read

            by_format: dict[str, list] = {}
            for entry in files:
                by_format.setdefault(entry.format, []).append(entry)

            for fmt, entries in by_format.items():
                extension = self._archive.extension_for(fmt)
                if not extension:
                    continue
                chosen = self._select_files(task, entries)
                if not chosen:
                    continue
                candidates.append(
                    FreeMusicCandidate(
                        identifier=item.identifier,
                        title=item.title,
                        creator=item.creator,
                        licence_url=licence,
                        format=fmt,
                        extension=extension,
                        track_count=len(chosen),
                        size_bytes=sum(e.size_bytes for e in chosen),
                        filenames=[e.name for e in chosen],
                    )
                )

        candidates.sort(key=lambda c: self._quality_sort_key(c, snapshot, track_count))
        return candidates

    @staticmethod
    def _quality_sort_key(
        candidate: FreeMusicCandidate, snapshot, track_count: int
    ) -> tuple:
        """Lower sorts first.

        MusicBrainz track-count agreement stays FIRST (owner-signed authority).
        The admin format preference (`free_music.preferred_format`) is no
        longer read for ranking - order comes from the task's stored quality
        snapshot: global preference step, then evidence certainty, then size.
        Outside-policy/rejected candidates get a step past the unknown slot so
        they are only reached after every acceptable option is exhausted."""
        count_delta = abs(candidate.track_count - track_count) if track_count else 0
        evidence = acq_quality.evidence_from_archive_format(candidate.format)
        decision = acq_quality.evaluate(snapshot, evidence)
        step = decision.preference_step
        if step is None:
            step = (
                len(snapshot.quality_recipe) + 2
                if acq_quality.is_recipe_snapshot(snapshot)
                else len(snapshot.quality_preference_order) + 2
            )
        certainty = acq_quality.CERTAINTY_RANK[evidence.certainty]
        return (count_delta, step, -certainty, -candidate.size_bytes)

    def _task_snapshot(self, task: FreeMusicTask):
        """Return the stored snapshot, migrating only a NULL legacy row."""
        raw = task.quality_snapshot_json
        if raw is not None:
            try:
                return acq_quality.decode_snapshot(raw)
            except acq_quality.SnapshotValidationError as exc:
                logger.warning("free_music.snapshot_decode_failed task=%s", task.id)
                raise ValidationError(
                    "Stored quality policy snapshot is invalid"
                ) from exc
        return acq_quality.migration_snapshot(self._prefs.get_download_policy())

    async def _download_with_retry(
        self,
        task_id: str,
        task: FreeMusicTask,
        candidate: FreeMusicCandidate,
        dest: Path,
        cancel: asyncio.Event,
    ) -> list[Path]:
        """One retry on a transient failure, then give up. The Archive does not
        lie about its files, so there is nothing to fail over to."""
        last: Exception | None = None
        for attempt in (1, 2):
            updated = await self._store.update(
                task_id,
                attempts=attempt,
                expected_statuses=(FreeMusicStatus.DOWNLOADING,),
            )
            if not updated:
                raise _Cancelled
            try:
                return await self._download(task_id, task, candidate, dest, cancel)
            except _Cancelled:
                raise
            except Exception as exc:  # noqa: BLE001 - retried once, then surfaced
                last = exc
                logger.info(
                    "free_music.download_attempt_failed task=%s attempt=%s: %s",
                    task_id,
                    attempt,
                    exc,
                )
                await asyncio.to_thread(shutil.rmtree, dest, True)
                if cancel.is_set():
                    raise _Cancelled from exc
        raise last if last else RuntimeError("download failed")

    async def _download(
        self,
        task_id: str,
        task: FreeMusicTask,
        candidate: FreeMusicCandidate,
        dest: Path,
        cancel: asyncio.Event,
    ) -> list[Path]:
        await asyncio.to_thread(dest.mkdir, parents=True, exist_ok=True)
        written: list[Path] = []
        downloaded = 0
        last_write = 0.0

        for index, name in enumerate(candidate.filenames):
            if cancel.is_set():
                raise _Cancelled
            target = dest / Path(name).name
            with open(target, "wb") as out:
                async for chunk in self._archive.stream_file(
                    candidate.identifier, name
                ):
                    if cancel.is_set():
                        raise _Cancelled
                    out.write(chunk)
                    downloaded += len(chunk)
                    now = time.monotonic()
                    if now - last_write >= _PROGRESS_WRITE_INTERVAL:
                        last_write = now
                        updated = await self._store.update(
                            task_id,
                            bytes_downloaded=downloaded,
                            files_completed=index,
                            expected_statuses=(FreeMusicStatus.DOWNLOADING,),
                        )
                        if not updated:
                            raise _Cancelled
                        await self._publish(
                            task.user_id, task_id, FreeMusicStatus.DOWNLOADING
                        )
            written.append(target)

        updated = await self._store.update(
            task_id,
            bytes_downloaded=downloaded,
            files_completed=len(written),
            expected_statuses=(FreeMusicStatus.DOWNLOADING,),
        )
        if not updated:
            raise _Cancelled
        return written

    async def _fail(self, task_id: str, user_id: str, message: str) -> None:
        failed = await self._store.update(
            task_id,
            status=FreeMusicStatus.FAILED,
            error=message,
            expected_statuses=(
                FreeMusicStatus.SEARCHING,
                FreeMusicStatus.DOWNLOADING,
                FreeMusicStatus.IMPORTING,
            ),
        )
        if not failed:
            return
        await self._publish(user_id, task_id, FreeMusicStatus.FAILED)

    async def _publish(self, user_id: str, task_id: str, status: str) -> None:
        # status rides along so the client can sweep its library caches on a
        # completion without doing it on every once-a-second progress tick
        try:
            await self._sse.publish(
                f"user:{user_id}",
                "free_music_updated",
                {
                    "event_id": uuid.uuid4().hex,
                    "task_id": task_id,
                    "status": status,
                },
            )
        except Exception as exc:  # noqa: BLE001 - progress push is best-effort
            logger.debug("free_music_updated publish failed: %s", exc)


class _Cancelled(Exception):
    """Internal: the user cancelled mid-download."""
