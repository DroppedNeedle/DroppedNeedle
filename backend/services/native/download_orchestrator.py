"""DownloadOrchestrator - the download lifecycle (Phase 7).

Owns search -> score -> auto-pick -> enqueue -> poll -> process -> notify (C1), plus
``cancel_task``, ``retry_task`` and ``startup_resume``. It speaks only the
``DownloadClientProtocol`` (never ``repositories/slskd`` directly) and never imports
``DownloadService`` (the dependency is one-way; no import cycle - A2).

Durable cross-task state lives in ``download_tasks`` + ``staging/{task_id}/
manifest.json``; the audio itself is written by slskd into its own download dir
(C4) and MOVED into the library by ``FileProcessor``. The only in-memory state is
``_active_tasks`` (live ``asyncio.Task`` handles for prompt cancel), rebuilt by
``startup_resume`` - it holds no authoritative data, so the class is ``@singleton``.
"""

import asyncio
import logging
import shutil
import time
from pathlib import Path

from core.exceptions import (
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from core.task_registry import TaskRegistry
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.sse_publisher import SSEPublisher
from models.download import TargetTrack
from models.download_manifest import DownloadManifest, ExpectedFile, ManifestCodec
from repositories.protocols.download_client import (
    DownloadClientProtocol,
    DownloadFileRef,
    TaskRef,
)
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.file_processor import QUARANTINE_REASONS, FileProcessor
from services.native.library_manager import LibraryManager
from services.native.track_matcher import TrackMatcher

logger = logging.getLogger(__name__)

# 6-hour ceiling on a single download's poll loop (absolute backstop; the
# minutes-scale stall/queued watchdogs normally resolve a stuck transfer long
# before this).
_POLL_DEADLINE_SECONDS = 3600 * 6

# _poll_until_done outcomes.
_OUT_COMPLETED = "completed"   # every transfer terminal and succeeded
_OUT_TERMINAL = "terminal"     # every transfer terminal, at least one failed
_OUT_STALLED = "stalled"       # an active transfer stopped making progress
_OUT_QUEUED = "queued_timeout"  # stuck in the peer's remote upload queue too long
_OUT_DEADLINE = "deadline"     # hit the 6-hour absolute ceiling


class OrchestrationError(Exception):
    """Module-internal control-flow signal (enqueue/poll/timeout failures). Always
    caught inside the class; its message is a curated, sanitized string safe to
    persist/SSE (AUD-11)."""


class _Cancelled(Exception):
    """Internal signal: the task was cancelled out-of-band (by cancel_task) while a
    poll loop was running. Caught by process_task / _resume_single_task, which return
    without overwriting the already-set 'cancelled' status."""


def _user_error_message(exc: Exception) -> str:
    """AUD-11: map an exception to a small fixed set of user-facing strings. Raw
    ``str(exc)`` for arbitrary exceptions is never returned (logs only)."""
    if isinstance(exc, OrchestrationError):
        return str(exc)
    return "download failed"


def _basename(filename: str) -> str:
    """Last path segment (slskd filenames use backslashes); log basenames not full
    peer paths to keep log lines free of identifying directory structure."""
    return filename.replace("\\", "/").rsplit("/", 1)[-1]


def _log_task_exception(task: "asyncio.Task") -> None:
    if task.cancelled():
        return
    exc = task.exception()
    if exc:
        logger.error("Background download task failed: %s", exc, exc_info=exc)


class DownloadOrchestrator:
    def __init__(
        self,
        client: DownloadClientProtocol,
        download_store: DownloadStore,
        file_processor: FileProcessor,
        library_manager: LibraryManager,
        scorer: AlbumPreflightScorer,
        track_matcher: TrackMatcher,
        manifest_codec: ManifestCodec,
        event_bus: SSEPublisher,
        staging_path: Path,
        naming_template: str,
        poll_interval: float = 2.0,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        stall_timeout_minutes: float = 30.0,
        queued_timeout_minutes: float = 120.0,
        max_failover_attempts: int = 3,
        max_concurrent_downloads: int = 3,
        request_history=None,  # RequestHistoryStore | None
        on_import_callback=None,  # Callable[[RequestHistoryRecord], Awaitable[None]] | None
    ) -> None:
        self._client = client
        self._store = download_store
        self._file_processor = file_processor
        self._library = library_manager
        self._scorer = scorer
        self._track_matcher = track_matcher
        self._manifest_codec = manifest_codec
        self._bus = event_bus
        self._staging = Path(staging_path)
        self._naming_template = naming_template
        self._poll_interval = poll_interval
        self._auto = auto_accept_threshold
        self._manual = manual_threshold
        # No byte progress on an actively-transferring peer for this long -> stalled.
        # Production values are bounds-checked in DownloadClientConnectionSettings;
        # tests inject tiny values directly.
        self._stall_timeout = stall_timeout_minutes * 60.0
        # Sitting in a peer's remote upload queue (0 bytes) for this long -> give up
        # on that peer. Deliberately more generous than the stall timeout.
        self._queued_timeout = queued_timeout_minutes * 60.0
        self._max_failover = max(1, max_failover_attempts)
        # Caps concurrent actively-transferring downloads so a batch can't flood slskd
        # or starve others; a queued download holds no slot. Per-instance, not module-
        # global: a settings-save rebuild briefly doubles the cap (acceptable) but
        # avoids the event-loop-binding hazard of a shared global.
        self._download_slots = asyncio.Semaphore(max(1, max_concurrent_downloads))
        self._request_history = request_history
        self._on_import = on_import_callback
        self._active_tasks: dict[str, asyncio.Task] = {}

    def dispatch(self, task_id: str) -> "asyncio.Task":
        """Run ``process_task`` for ``task_id`` in the background (AUD-3): wrapped in
        the safe runner, registered in ``TaskRegistry`` so shutdown cancels it, and
        tracked in ``_active_tasks`` so ``cancel_task`` can stop the live poll loop."""
        task = asyncio.create_task(self._run_orchestrator_safely(task_id))
        self._active_tasks[task_id] = task
        task.add_done_callback(_log_task_exception)
        task.add_done_callback(lambda _t, _id=task_id: self._active_tasks.pop(_id, None))
        TaskRegistry.get_instance().register(f"download-{task_id}", task)
        return task

    async def _run_orchestrator_safely(self, task_id: str) -> None:
        """Wrap ``process_task`` so an unhandled exception updates the task to
        ``failed`` (sanitized message, AUD-11) instead of vanishing into a
        fire-and-forget create_task."""
        try:
            await self.process_task(task_id)
        except Exception as exc:  # noqa: BLE001 - last line of defence for a bg task
            logger.exception("Unhandled exception in orchestrator task %s", task_id)
            try:
                user_msg = _user_error_message(exc)
                await self._store.update_status(task_id, "failed", error_message=user_msg)
                logger.info(
                    "download.failed",
                    extra={"task_id": task_id, "error_message": user_msg},
                )
                await self._bus.publish(
                    f"download:{task_id}", "complete",
                    {"status": "failed", "error": user_msg},
                )
            except Exception:  # noqa: BLE001
                logger.exception("Failed to mark task %s failed after error", task_id)

    async def process_task(self, task_id: str) -> None:
        """Main lifecycle. Two entry shapes converge here: a direct request (no
        candidate linked -> search/score/auto-pick first) and a manual pick (a
        candidate already linked -> straight to enqueue)."""
        task = await self._store.get_task(task_id)
        if task is None:
            logger.error("Download task %s not found", task_id)
            return

        logger.info(
            "download.started",
            extra={
                "task_id": task.id,
                "user_id": task.user_id,
                "download_type": task.download_type,
                "release_group_mbid": task.release_group_mbid,
            },
        )

        try:
            if not self._client.is_configured():
                raise OrchestrationError(
                    "Download client is not configured - check the slskd URL in Settings"
                )
            if task.search_job_id is None or task.candidate_index is None:
                if not await self._search_score_autopick(task):
                    return  # parked for review or failed; status already set
                task = await self._store.get_task(task_id)
                if task is None:
                    return

            await self._run_with_failover(task)
        except _Cancelled:
            return  # cancel_task already set status='cancelled'; don't overwrite
        except OrchestrationError as exc:
            user_msg = _user_error_message(exc)
            await self._store.update_status(task_id, "failed", error_message=user_msg)
            logger.info(
                "download.failed", extra={"task_id": task_id, "error_message": user_msg}
            )
            await self._bus.publish(
                f"download:{task_id}", "complete",
                {"status": "failed", "error": user_msg},
            )
            await self._sync_request_on_terminal(task, "failed")

    async def _search_score_autopick(self, task) -> bool:  # noqa: ANN001 - DownloadTask
        """Search, score, and auto-pick the top candidate.

        Returns True iff a candidate was auto-picked and linked (proceed to
        enqueue). Returns False when parked for manual review or failed (status
        already set; caller must return)."""
        timeout = 30.0 + 15.0 * min(task.retry_count, 4)

        if task.download_type == "track":
            target = TargetTrack(
                artist_name=task.artist_name,
                track_title=task.track_title or "",
                album_title=task.album_title,
                duration_seconds=task.track_duration_seconds,
            )
            results = await self._client.search_track(
                task.artist_name, task.track_title or "", task.album_title, timeout=timeout
            )
            # Rank all viable sources (one per peer) so a per-track download can fail
            # over to a different peer, mirroring the album path.
            candidates = await self._track_matcher.rank(
                target, results, auto_accept_threshold=self._auto, manual_threshold=self._manual
            )
        else:
            from models.download import TargetAlbum

            target = TargetAlbum(
                artist_name=task.artist_name,
                album_title=task.album_title,
                year=task.year,
                track_count=task.track_count,
            )
            results = await self._client.search_album(
                task.artist_name, task.album_title, task.year, task.track_count, timeout=timeout
            )
            candidates = await self._scorer.rank(
                target, results, auto_accept_threshold=self._auto, manual_threshold=self._manual
            )

        logger.info(
            "download.search.completed",
            extra={
                "task_id": task.id,
                "download_type": task.download_type,
                "results_count": len(results),
                "candidates_count": len(candidates),
                "top_score": candidates[0].final_score if candidates else 0.0,
            },
        )

        job = await self._store.create_search_job(
            user_id=task.user_id,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
            search_query=f"{task.artist_name} - {task.album_title}",
        )
        await self._store.set_search_job_candidates(job.id, candidates)
        top = candidates[0] if candidates else None

        if top and top.final_score >= self._auto:
            await self._store.link_picked_candidate(
                task.id, job.id, 0, top.username, top.parent_directory, top.final_score
            )
            return True

        if top and top.final_score >= self._manual:
            # Park for review: search job 'completed', candidate_index NULL. The UI
            # derives 'awaiting_review'. Resumed when the user calls pick_candidate.
            await self._store.set_search_job_id_and_candidate(task.id, job.id, None)
            await self._store.update_search_job_status(job.id, "completed")
            await self._bus.publish(
                f"download:{task.id}", "status",
                {"status": "awaiting_review", "search_job_id": job.id},
            )
            return False

        await self._store.update_search_job_status(job.id, "completed")
        await self._store.update_status(
            task.id, "failed", error_message="No matching candidate found on Soulseek"
        )
        await self._bus.publish(
            f"download:{task.id}", "complete", {"status": "failed", "error": "no match"}
        )
        return False

    async def _enqueue(  # noqa: ANN001 - DownloadTask
        self, task, *, strict_track_duration: bool = True
    ) -> None:
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if task.candidate_index is None or task.candidate_index >= len(candidates):
            raise OrchestrationError("candidate no longer available")
        candidate = candidates[task.candidate_index]
        # For a per-track download, verify the imported file against the CANONICAL
        # track length so a wrong-length recording fails over instead of being
        # imported and mislabelled. strict_track_duration is turned off for the
        # last-resort fallback so the user is never stranded by a bad MB duration.
        use_canonical = (
            task.download_type == "track"
            and strict_track_duration
            and bool(task.track_duration_seconds)
        )

        files = [
            DownloadFileRef(username=candidate.username, filename=f.filename, size=f.size)
            for f in candidate.files
        ]
        total_size = sum(f.size for f in candidate.files)
        await self._store.update_status(
            task.id, "downloading", files_total=len(files), total_size_bytes=total_size,
            started_at=time.time(),
        )
        # No 'downloading' SSE status here: the UI reads the polled task.status for the
        # in-flight view, and not re-publishing it lets a 'retrying' status (set when
        # we fail over) persist through the next attempt instead of being clobbered.

        # Persist the manifest BEFORE enqueueing: it carries the correlation key
        # (source_username + the enqueued filenames) so a restart can re-correlate.
        manifest = DownloadManifest(
            task_id=task.id,
            source_username=candidate.username,
            release_group_mbid=task.release_group_mbid,
            release_mbid=task.release_mbid,
            artist_mbid=task.artist_mbid,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            naming_template=self._naming_template,
            is_track=use_canonical,
            target_files=[
                ExpectedFile(
                    filename=f.filename,
                    size=f.size,
                    duration=task.track_duration_seconds if use_canonical else f.duration,
                )
                for f in candidate.files
            ],
        )
        self._staging.joinpath(task.id).mkdir(parents=True, exist_ok=True)
        (self._staging / task.id / "manifest.json").write_bytes(
            self._manifest_codec.encode(manifest)
        )

        try:
            await self._client.enqueue(files)
        except Exception as exc:  # noqa: BLE001 - any client error -> task failed
            # Per review-triage: do NOT quarantine on enqueue failure (nothing was
            # downloaded). The safe runner / process_task persists the sanitized msg.
            logger.exception("Enqueue failed for task %s", task.id)
            raise OrchestrationError("enqueue failed") from exc

        logger.info(
            "download.enqueued",
            extra={
                "task_id": task.id,
                "user_id": task.user_id,
                "release_group_mbid": task.release_group_mbid,
                "files_total": len(files),
                "total_size_bytes": total_size,
            },
        )

    async def _poll_until_done(self, task):  # noqa: ANN001, ANN201 - DownloadTask
        """Poll slskd until the transfer terminates, stalls, or hits the ceiling.

        Returns ``(outcome, last_status)``. The watchdog watches real byte
        progress: an actively-transferring peer that stops moving bytes for
        ``stall_timeout`` is stalled; one still sitting in the peer's remote upload
        queue for ``queued_timeout`` is given up on. ``_run_with_failover`` decides
        what to do with the outcome - this method never discards progress."""
        manifest = self._read_manifest(task.id)
        task_ref = TaskRef(
            username=task.source_username or "",
            filenames=[f.filename for f in manifest.target_files],
        )
        loop = asyncio.get_running_loop()
        deadline = loop.time() + _POLL_DEADLINE_SECONDS
        last_logged_percent = -1
        last_progress_bytes = -1
        last_progress_time = loop.time()
        last_status = None
        slot_held = False
        try:
            while loop.time() < deadline:
                # An out-of-band cancel (cancel_task) may have set status='cancelled'
                # since this loop started - stop before processing so the import can't
                # proceed against an explicit cancel.
                current = await self._store.get_task(task.id)
                if current is not None and current.status == "cancelled":
                    raise _Cancelled()
                status = await self._client.get_status(task_ref)
                last_status = status
                # Concurrency cap: take a slot the moment this transfer is actively
                # moving bytes, and hold it until the loop exits. A purely queued
                # transfer never gets here, so it can't block a ready one (and while
                # blocked waiting for a slot we simply pause polling - the watchdog
                # can't false-fail a starved transfer because we're not in it).
                if status.has_active_transfer and not slot_held:
                    await self._download_slots.acquire()
                    slot_held = True
                await self._store.update_progress(
                    task.id,
                    bytes_downloaded=status.bytes_downloaded,
                    files_completed=status.files_completed,
                    progress_percent=int(status.progress_percent),
                )
                # Throttle to one log per whole-percent change so a multi-minute
                # transfer emits ~100 lines, not one every poll interval.
                percent = int(status.progress_percent)
                if percent != last_logged_percent:
                    last_logged_percent = percent
                    logger.info(
                        "download.progress",
                        extra={
                            "task_id": task.id,
                            "progress_percent": percent,
                            "files_completed": status.files_completed,
                            "files_total": status.files_total,
                            "bytes_downloaded": status.bytes_downloaded,
                        },
                    )
                await self._bus.publish(
                    f"download:{task.id}", "progress",
                    {
                        "bytes_downloaded": status.bytes_downloaded,
                        "bytes_total": status.bytes_total,
                        "files_completed": status.files_completed,
                        "files_total": status.files_total,
                        "progress_percent": status.progress_percent,
                    },
                )
                if status.status == "completed":
                    return _OUT_COMPLETED, status
                if status.status in ("partial", "failed"):
                    return _OUT_TERMINAL, status
                # Non-terminal: run the stall/queued watchdog off real byte progress.
                now = loop.time()
                if status.bytes_downloaded > last_progress_bytes:
                    last_progress_bytes = status.bytes_downloaded
                    last_progress_time = now
                else:
                    idle = now - last_progress_time
                    if status.has_active_transfer and idle >= self._stall_timeout:
                        return _OUT_STALLED, status
                    if not status.has_active_transfer and idle >= self._queued_timeout:
                        return _OUT_QUEUED, status
                await asyncio.sleep(self._poll_interval)
            if last_status is None:
                last_status = await self._client.get_status(task_ref)
            return _OUT_DEADLINE, last_status
        finally:
            if slot_held:
                self._download_slots.release()

    async def _run_with_failover(self, task, *, resume: bool = False) -> None:  # noqa: ANN001
        """Drive a task through enqueue -> poll -> harvest, failing over to the next
        ranked candidate when a peer stalls, errors, or delivers an incomplete
        album. Never loses progress: each attempt imports only the files that
        actually succeeded, so a partial download survives. On ``resume`` the first
        iteration skips the enqueue and polls the transfers a restart left behind."""
        from services.native.file_processor import (
            DOWNLOADS_MOUNT_UNAVAILABLE,
            WRONG_TRACK,
        )

        attempts = 0
        tried_usernames: set[str] = set()
        first = True
        imported_any = False
        wrong_track = False
        while True:
            enqueued = True
            if not (first and resume):
                try:
                    await self._enqueue(task)
                except OrchestrationError:
                    logger.warning(
                        "Enqueue failed for task %s candidate %s",
                        task.id, task.candidate_index,
                    )
                    enqueued = False
            first = False

            if enqueued:
                outcome, status = await self._poll_until_done(task)
                # Re-check for an out-of-band cancel in the window between the poll
                # loop's last check and here, so we don't overwrite 'cancelled' and
                # import anyway (the failover loop runs this sequence up to N times).
                current = await self._store.get_task(task.id)
                if current is not None and current.status == "cancelled":
                    raise _Cancelled()
                await self._store.update_status(task.id, "processing")
                await self._bus.publish(
                    f"download:{task.id}", "status", {"status": "processing"}
                )
                # On an interrupted (stalled/queued/deadline) outcome, import ONLY
                # the transfers that actually succeeded - files that never arrived
                # are not processed, so they can't be quarantined as verify failures
                # (which would wrongly blacklist a slow-but-good peer). On a terminal
                # outcome every transfer settled, so import the full manifest and let
                # a genuinely failed source be quarantined as before.
                if outcome in (_OUT_COMPLETED, _OUT_TERMINAL):
                    only = None
                else:
                    only = set(status.succeeded_filenames)
                result = await self._import_files(task, only_filenames=only)
                if result.succeeded:
                    imported_any = True
                if any(f.reason == WRONG_TRACK for f in result.failed):
                    wrong_track = True
                await self._cancel_transfers(task)

                # A missing/unwritable downloads mount is an environment fault, not
                # a bad source - failing over to another peer can't help, so stop.
                if not result.succeeded and any(
                    f.reason == DOWNLOADS_MOUNT_UNAVAILABLE for f in result.failed
                ):
                    await self._finalize(
                        task, "failed", error_message=DOWNLOADS_MOUNT_UNAVAILABLE
                    )
                    return

                if await self._download_is_complete(task, imported_any, result):
                    await self._finalize(task, "completed")
                    return

            # Incomplete (or this candidate's enqueue failed): fail over.
            tried_usernames.add(task.source_username or "")
            attempts += 1
            nxt = (
                await self._advance_candidate(task, tried_usernames)
                if attempts < self._max_failover
                else None
            )
            if nxt is None:
                # Every source for a single track failed the canonical-duration gate:
                # the MB length is probably wrong (not the files), so re-pull the best
                # source with the gate off rather than strand the user.
                if task.download_type == "track" and wrong_track and not imported_any:
                    await self._fallback_track_repull(task)
                    return
                await self._settle_incomplete(task, imported_any)
                return
            task = nxt
            await self._bus.publish(
                f"download:{task.id}", "status",
                {"status": "retrying", "attempt": attempts},
            )

    async def _fallback_track_repull(self, task) -> None:  # noqa: ANN001 - DownloadTask
        """Last resort for a per-track download whose every candidate was rejected on
        duration: re-pull the top-ranked source with the strict duration gate OFF so
        the user still gets the track (better the closest match than nothing)."""
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if not candidates:
            await self._settle_incomplete(task, False)
            return
        cand = candidates[0]
        await self._store.link_picked_candidate(
            task.id, task.search_job_id, 0,
            cand.username, cand.parent_directory, cand.final_score,
        )
        task = await self._store.get_task(task.id)
        logger.info("download.track_duration_fallback", extra={"task_id": task.id})
        try:
            await self._enqueue(task, strict_track_duration=False)
        except OrchestrationError:
            await self._settle_incomplete(task, False)
            return
        outcome, status = await self._poll_until_done(task)
        await self._store.update_status(task.id, "processing")
        only = None if outcome in (_OUT_COMPLETED, _OUT_TERMINAL) else set(
            status.succeeded_filenames
        )
        result = await self._import_files(task, only_filenames=only)
        await self._cancel_transfers(task)
        await self._finalize(task, "completed" if result.succeeded else "failed")

    async def _import_files(self, task, *, only_filenames=None):  # noqa: ANN001, ANN201
        """Import a subset of the manifest into the library, quarantining only files
        that arrived but failed verification. Does not set the task's terminal
        status (the failover loop owns that). Returns the ProcessResult."""
        manifest = self._read_manifest(task.id)
        logger.info(
            "download.processing",
            extra={"task_id": task.id, "files_total": len(manifest.target_files)},
        )
        result = await self._file_processor.process_downloaded(
            manifest, only_filenames=only_filenames
        )
        for failure in result.failed:
            if failure.reason in QUARANTINE_REASONS:
                await self._store.record_quarantine(
                    client_id="slskd",
                    username=task.source_username or "",
                    filename=failure.filename,
                    reason=failure.reason,
                    release_group_mbid=task.release_group_mbid,
                )
                logger.info(
                    "download.quarantined",
                    extra={
                        "task_id": task.id,
                        "file": _basename(failure.filename),
                        "reason": failure.reason,
                    },
                )
        if result.succeeded:
            await self._store.set_final_path(task.id, str(Path(result.succeeded[0]).parent))
        return result

    async def _cancel_transfers(self, task) -> None:  # noqa: ANN001 - DownloadTask
        """Clear this task's slskd transfer records (succeeded ones, post-import,
        per DEC-1) and stop any still running (stalled). Best-effort; imported
        audio has already been MOVED out, so ``?remove=true`` can't touch it."""
        if not task.source_username:
            return
        try:
            manifest = self._read_manifest(task.id)
            await self._client.cancel(
                TaskRef(
                    username=task.source_username,
                    filenames=[f.filename for f in manifest.target_files],
                )
            )
        except Exception:  # noqa: BLE001 - cleanup must not fail the task
            logger.warning("Failed to remove/stop slskd transfers for task %s", task.id)

    async def _advance_candidate(self, task, tried_usernames):  # noqa: ANN001, ANN201
        """Move the task to the next ranked candidate whose peer we haven't tried,
        updating candidate_index AND source_username (the latter is read by the
        poll/import/cancel paths). Returns the refreshed task, or None when none
        remain. Re-fetch is required: the loop reads the task object, not the DB."""
        if task.search_job_id is None:
            return None
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        start = (task.candidate_index or 0) + 1
        for idx in range(start, len(candidates)):
            cand = candidates[idx]
            if cand.username in tried_usernames:
                continue
            await self._store.link_picked_candidate(
                task.id, task.search_job_id, idx,
                cand.username, cand.parent_directory, cand.final_score,
            )
            return await self._store.get_task(task.id)
        return None

    async def _imported_track_count(self, task) -> int:  # noqa: ANN001 - DownloadTask
        """Distinct imported tracks for the release group. Counts distinct
        (disc, track) positions so a duplicate file for the same track (e.g. a flac
        and an mp3, or a re-pull) can't inflate the completeness check; rows with no
        track number are counted individually."""
        try:
            rows = await self._library.get_file_rows_for_album(task.release_group_mbid)
        except Exception:  # noqa: BLE001 - completeness check must not crash the task
            return 0
        positions: set[tuple] = set()
        untracked = 0
        for row in rows:
            track_no = row.get("track_number")
            if track_no:
                positions.add((row.get("disc_number") or 1, track_no))
            else:
                untracked += 1
        return len(positions) + untracked

    def _expected_track_count(self, task) -> int:  # noqa: ANN001 - DownloadTask
        if task.download_type == "track":
            return 1
        return task.track_count or 0

    async def _download_is_complete(self, task, imported_any: bool, result=None) -> bool:  # noqa: ANN001
        """Whether the download has delivered what it set out to. A per-track
        download is one file - complete the moment it imports (Soulseek rips rarely
        carry the recording MBID, so a tag-based check can't be trusted). An album is
        complete once the library holds at least ``track_count`` distinct tracks,
        which is cumulative across failover attempts (a partial harvest plus a later
        re-pull from a better source)."""
        if task.download_type == "track":
            return imported_any
        expected = self._expected_track_count(task)
        present = await self._imported_track_count(task)
        if expected > 0:
            return present >= expected
        # Unknown album track count (MusicBrainz gave none): completeness can't be
        # measured, so the best signal is "this source delivered all it had" - a clean
        # full import with no failures. A partial then fails over to try a fuller
        # source before settling, rather than declaring done on the first track.
        return bool(result and result.succeeded and not result.failed)

    async def _settle_incomplete(self, task, imported_any: bool) -> None:  # noqa: ANN001
        """No candidates/attempts left and the download still isn't whole. A track
        either imported (already finalized 'completed') or it didn't ('failed'); an
        album keeps whatever landed as 'partial', or 'failed' if nothing did."""
        if task.download_type == "track":
            await self._finalize(
                task, "failed", error_message="No working source found on Soulseek"
            )
            return
        if await self._imported_track_count(task) > 0:
            await self._finalize(task, "partial")
        else:
            await self._finalize(
                task, "failed", error_message="No working source found on Soulseek"
            )

    async def _finalize(self, task, status, *, error_message=None) -> None:  # noqa: ANN001
        if task.download_type == "track":
            present = 1 if status == "completed" else 0
            expected = 1
        else:
            present = await self._imported_track_count(task)
            expected = self._expected_track_count(task) or present
        fields = {
            "completed_at": time.time(),
            "files_completed": present,
            "files_total": max(expected, present),
            "files_failed": max(0, expected - present),
        }
        if error_message:
            fields["error_message"] = error_message
        await self._store.update_status(task.id, status, **fields)
        shutil.rmtree(self._staging / task.id, ignore_errors=True)
        # keep the established log-event contract: completed/partial -> download.completed,
        # failed -> download.failed (consumed by log monitoring + tests)
        event = "download.failed" if status == "failed" else "download.completed"
        logger.info(
            event,
            extra={
                "task_id": task.id,
                "status": status,
                "files_completed": present,
                "files_total": expected,
            },
        )
        await self._notify_completion(task)
        await self._sync_request_on_terminal(task, status)

    async def _sync_request_on_terminal(self, task, status: str) -> None:  # noqa: ANN001
        """Bridge a terminal download status into the linked request + caches, so a
        request no longer sticks on 'Pending' forever and a completed album flips to
        In-Library without a manual reload.

        Keyed on ``download_task_id == task.id``: a request is only touched by the
        task that actually dispatched it, so a stray per-track download of an album
        can't flip that album's request. Monitor/orphan downloads (no request row)
        are a safe no-op."""
        if self._request_history is None:
            return
        mapping = {
            "completed": "imported",
            "partial": "incomplete",
            "failed": "failed",
            "cancelled": "cancelled",
        }
        new_status = mapping.get(status)
        if new_status is None:
            return
        try:
            record = await self._request_history.async_get_record(task.release_group_mbid)
        except Exception:  # noqa: BLE001 - request sync must never fail the download
            logger.warning("Could not load request for %s", task.release_group_mbid)
            return
        if record is None or getattr(record, "download_task_id", None) != task.id:
            return
        from datetime import datetime, timezone

        completed_at = (
            datetime.now(timezone.utc).isoformat()
            if new_status in ("imported", "failed", "cancelled")
            else None
        )
        try:
            await self._request_history.async_update_status(
                record.musicbrainz_id, new_status, completed_at=completed_at
            )
            # An import (full or partial) added library files - bust the album/library
            # caches and materialise the album row so the UI reflects it.
            if new_status in ("imported", "incomplete") and self._on_import is not None:
                await self._on_import(record)
        except Exception:  # noqa: BLE001
            logger.warning("Failed to sync request %s -> %s", record.musicbrainz_id, new_status)

    async def _notify_completion(self, task) -> None:  # noqa: ANN001 - DownloadTask
        final = await self._store.get_task(task.id)
        await self._bus.publish(
            f"download:{task.id}", "complete",
            {
                "status": final.status if final else "unknown",
                "final_path": final.final_path if final else None,
            },
        )

    async def reap_stale_tasks(self) -> None:
        """Periodic safety net: fail tasks whose in-process poll loop died (a crash,
        or a restart that never resumed them) so they don't sit 'downloading'
        forever. A task owned by a live loop - on this instance (``_active_tasks``) or
        on a pre-rebuild instance (the global ``TaskRegistry``) - is skipped. Only a
        genuinely unowned, unpolled task (stale ``last_polled_at``) is aged out."""
        try:
            active = await self._store.list_active_tasks(["downloading", "processing"])
        except Exception:  # noqa: BLE001
            return
        if not active:
            return
        now = time.time()
        threshold = 1800.0  # 30 min with no poller at all -> the loop is dead
        registry = TaskRegistry.get_instance()
        for task in active:
            handle = self._active_tasks.get(task.id)
            if handle is not None and not handle.done():
                continue  # a live loop on this instance owns it
            # A live loop may instead belong to a PRE-REBUILD orchestrator instance
            # (a settings save rebuilds the singleton): those tasks are still in the
            # global TaskRegistry, so honour it before reaping. Without this a download
            # blocked on the old instance's concurrency slot (and so not polling) could
            # be force-failed despite being alive.
            if registry.is_running(f"download-{task.id}") or registry.is_running(
                f"download-resume-{task.id}"
            ):
                continue
            last = task.last_polled_at or task.started_at or task.created_at or 0.0
            if now - last < threshold:
                continue
            await self._store.update_status(
                task.id, "failed",
                error_message="Download interrupted - no progress after a restart",
                completed_at=now,
            )
            await self._bus.publish(
                f"download:{task.id}", "complete",
                {"status": "failed", "error": "download interrupted"},
            )
            await self._sync_request_on_terminal(task, "failed")
            logger.warning(
                "Reaped stale download task %s (no poller for %.0fs)", task.id, now - last
            )

    async def startup_resume(self) -> None:
        """Resume in-progress tasks after a restart (AUD-3): never block startup."""
        registry = TaskRegistry.get_instance()

        for orphan in await self._store.list_active_tasks(["queued"]):
            # queued = created but never enqueued -> re-dispatch (failing would be
            # spurious; they never started).
            self.dispatch(orphan.id)

        for task in await self._store.list_active_tasks(["downloading", "processing"]):
            handle = asyncio.create_task(self._resume_single_task(task.id))
            # Track the live handle so cancel_task can stop the resumed poll loop
            # (mirrors dispatch); without this a resumed download is uncancellable.
            self._active_tasks[task.id] = handle
            handle.add_done_callback(_log_task_exception)
            handle.add_done_callback(lambda _t, _id=task.id: self._active_tasks.pop(_id, None))
            registry.register(f"download-resume-{task.id}", handle)

    async def _resume_single_task(self, task_id: str) -> None:
        task = await self._store.get_task(task_id)
        if task is None:
            return
        try:
            if not (self._staging / task_id / "manifest.json").exists():
                # Never got as far as writing a manifest -> re-dispatch from scratch.
                self.dispatch(task_id)
                return
            # Poll the transfers slskd kept across the restart instead of force-
            # failing them. A still-'queued' transfer now resumes (the old "Transfer
            # lost during restart" bug); a genuinely dead one is aged out by the
            # stall watchdog and failover re-pulls from another peer.
            await self._run_with_failover(task, resume=True)
        except _Cancelled:
            return  # cancelled mid-resume; status already 'cancelled'
        except OrchestrationError as exc:
            logger.warning("Resume failed for task %s: %s", task_id, exc)
            await self._store.update_status(
                task_id, "failed", error_message=_user_error_message(exc)
            )
            await self._sync_request_on_terminal(task, "failed")
        except Exception as exc:  # noqa: BLE001 - resume failure -> mark failed
            logger.exception("Failed to resume task %s", task_id)
            await self._store.update_status(
                task_id, "failed", error_message=_user_error_message(exc)
            )
            await self._sync_request_on_terminal(task, "failed")

    async def cancel_task(self, task_id: str, user_id: str, user_role: str) -> None:
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if user_role != "admin" and task.user_id != user_id:
            raise PermissionDeniedError("Cannot cancel another user's download")

        manifest_path = self._staging / task_id / "manifest.json"
        if task.source_username and manifest_path.exists():
            try:
                manifest = self._manifest_codec.decode(manifest_path.read_bytes())
                await self._client.cancel(
                    TaskRef(
                        username=task.source_username,
                        filenames=[f.filename for f in manifest.target_files],
                    )
                )
            except Exception as exc:  # noqa: BLE001 - best-effort
                logger.warning("Failed to cancel slskd transfers for %s: %s", task_id, exc)

        # Stop the live poll loop promptly if one is running in this process.
        handle = self._active_tasks.pop(task_id, None)
        if handle is not None and not handle.done():
            handle.cancel()

        await self._store.update_status(task_id, "cancelled", cancelled_at=time.time())
        logger.info(
            "download.cancelled", extra={"task_id": task_id, "user_id": task.user_id}
        )
        await self._bus.publish(
            f"download:{task_id}", "complete", {"status": "cancelled"}
        )

    async def retry_task(self, task_id: str, user_id: str, user_role: str) -> str:
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if user_role != "admin" and task.user_id != user_id:
            raise PermissionDeniedError("Cannot retry another user's download")
        if task.status not in ("failed", "cancelled", "partial"):
            raise ValidationError("Only failed, cancelled or partial downloads can be retried")

        # Clean slate (Q3-A): a new task, carrying retry_count + 1 so the search
        # timeout escalates. The original is kept (terminal) for audit.
        new_task = await self._store.create_task(
            user_id=task.user_id,
            download_type=task.download_type,
            release_group_mbid=task.release_group_mbid,
            release_mbid=task.release_mbid,
            recording_mbid=task.recording_mbid,
            artist_mbid=task.artist_mbid,
            artist_name=task.artist_name,
            album_title=task.album_title,
            track_title=task.track_title,
            year=task.year,
            track_count=task.track_count,
            search_query=task.search_query,
            retry_count=task.retry_count + 1,
        )
        self.dispatch(new_task.id)
        return new_task.id

    def _read_manifest(self, task_id: str) -> DownloadManifest:
        path = self._staging / task_id / "manifest.json"
        if not path.exists():
            raise OrchestrationError("manifest missing")
        return self._manifest_codec.decode(path.read_bytes())
