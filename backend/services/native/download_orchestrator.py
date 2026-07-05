"""DownloadOrchestrator - the download lifecycle (Phase 7).

Owns search -> score -> auto-pick -> enqueue -> poll -> process -> notify (C1), plus
``cancel_task``, ``retry_task`` and ``startup_resume``. It speaks only the
``IndexerProtocol`` (search) and ``DownloadClientProtocol`` (acquire/track/locate)
- never ``repositories/slskd`` directly - and never imports ``DownloadService``
(the dependency is one-way; no import cycle - A2).

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
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.sse_publisher import SSEPublisher
from models.download_manifest import (
    DownloadManifest,
    ExpectedFile,
    ExpectedTrack,
    ManifestCodec,
)
from repositories.protocols.download_client import (
    DownloadClientProtocol,
)
from repositories.protocols.indexer import IndexerProtocol
from services.native.acquisition.errors import OrchestrationError
from services.native.acquisition.status import DownloadStatus
from services.native.acquisition.strategy import (
    SoulseekStrategy,
    SourceStrategy,
    UsenetStrategy,
)
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.coverage import match_rows_to_tracks
from services.native.file_processor import (
    DOWNLOADS_MOUNT_UNAVAILABLE,
    IMPORT_FAILED,
    SOURCE_FILE_MISSING,
    FileProcessor,
)
from services.native.library_manager import LibraryManager
from services.native.track_matcher import TrackMatcher

logger = logging.getLogger(__name__)

# Fixed v1 source -> client_type map (the DownloadTask.download_client value).
_CLIENT_FOR_SOURCE = {"soulseek": "slskd", "usenet": "sabnzbd"}

# 6-hour ceiling on a single download's poll loop (absolute backstop; the
# minutes-scale stall/queued watchdogs normally resolve a stuck transfer long
# before this).
_POLL_DEADLINE_SECONDS = 3600 * 6

# A fresh enqueue normally produces a slskd transfer record within a poll or two; if
# none has materialized in this long the peer was offline / silently rejected it, so
# fail over fast instead of sitting in the queued watchdog's full window. Generous vs
# the seconds it actually takes, so a briefly-slow slskd never trips it.
_TRANSFER_MATERIALIZE_SECONDS = 90.0

# SABnzbd fail_message substrings that mean a LOCAL/environment fault (our disk or mount),
# NOT a bad release - never blocklist these; the backoff'd auto-retry re-grabs once the
# environment recovers (Lidarr treats disk/path errors as warnings, not release failures).
_LOCAL_FAULT_MARKERS = (
    "disk is full", "disk full", "no space", "not enough disk", "write error",
    "failed moving", "moving failed", "permission denied", "cannot write",
    "could not create", "read-only file system",
)


def _is_local_fault(message: str | None) -> bool:
    low = (message or "").lower()
    return any(m in low for m in _LOCAL_FAULT_MARKERS)

# _poll_until_done outcomes.
_OUT_COMPLETED = "completed"   # every transfer terminal and succeeded
_OUT_TERMINAL = "terminal"     # every transfer terminal, at least one failed
_OUT_STALLED = "stalled"       # an active transfer stopped making progress
_OUT_QUEUED = "queued_timeout"  # stuck in the peer's remote upload queue too long
_OUT_DEADLINE = "deadline"     # hit the 6-hour absolute ceiling
_OUT_NO_TRANSFER = "no_transfer"  # a fresh enqueue produced no transfer record

# Terminal "couldn't finish" messages. The mount one is used when slskd delivered the
# files but we then couldn't find them on the downloads mount - a local/config fault,
# not an absence of sources, so it must not read as "Soulseek had nothing".
# The "no source" wording is built per-task from the enabled sources (see
# _no_source_message) so a Usenet download never wrongly blames Soulseek.
_NO_SOURCE_MSG = "No working source found"
_FILES_NOT_FOUND_MSG = (
    "Files downloaded, but couldn't be found in the slskd downloads folder - check "
    "the slskd downloads path points to where slskd saves completed files"
)
# slskd delivered the files and we found them, but writing them into the library failed
# (perms, disk full, a cross-mount copy the filesystem rejected). Local fault, not the
# peer's - blaming Soulseek sends users chasing the wrong problem.
_IMPORT_FAILED_MSG = (
    "Files downloaded, but couldn't be saved into your library - check the library "
    "folder is writable and has free space"
)


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
        indexer: IndexerProtocol,
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
        auto_retry_enabled: bool = True,
        auto_retry_max_attempts: int = 6,
        auto_retry_base_interval_minutes: float = 15.0,
        request_history=None,  # RequestHistoryStore | None
        on_import_callback=None,  # Callable[[RequestHistoryRecord], Awaitable[None]] | None
        usenet_indexer=None,  # IndexerProtocol | None (NewznabIndexer)
        usenet_client=None,  # DownloadClientProtocol | None (SabnzbdDownloadClient)
        usenet_scorer=None,  # NewznabReleaseScorer | None
        usenet_enabled: bool = False,  # an indexer AND SABnzbd are both enabled
        soulseek_enabled: bool = True,  # the slskd enable toggle (separate from is_configured)
        source_priority=None,  # list[str] | None - default ["soulseek", "usenet"]
        album_service=None,  # AlbumService | None - for the Usenet MB tracklist
        usenet_category: str | None = None,
        usenet_priority: int | None = None,
        usenet_post_processing: int | None = None,
        usenet_min_release_age_minutes: float = 30.0,
        usenet_import_settle_seconds: float = 2.0,
    ) -> None:
        self._client = client
        self._naming_template = naming_template
        # Search/enqueue/import + the per-source policy all live on the strategies now; the
        # orchestrator keeps only the shared state below + the live enable toggles.
        self._usenet_enabled = usenet_enabled and usenet_indexer is not None and usenet_client is not None
        self._soulseek_enabled = soulseek_enabled
        self._source_priority = source_priority or ["soulseek", "usenet"]
        self._store = download_store
        self._library = library_manager
        # Coverage completeness (P4): the requested release's expected tracklist,
        # cache-aside via the album page's own resolver. None in minimal test
        # constructions -> the count-based check below is the fallback.
        self._album_service = album_service
        self._manifest_codec = manifest_codec
        self._bus = event_bus
        self._staging = Path(staging_path)
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
        self._auto_retry_enabled = auto_retry_enabled
        self._auto_retry_max_attempts = max(0, auto_retry_max_attempts)
        self._auto_retry_base_interval = auto_retry_base_interval_minutes
        self._request_history = request_history
        self._on_import = on_import_callback
        self._active_tasks: dict[str, asyncio.Task] = {}

        # Source strategies (step 4): all per-source behaviour (search, enqueue, import,
        # client, identity, blocklist-on-failure, poll/cancel/fault policy) lives here so the
        # orchestrator never branches on source. Enablement stays on the orchestrator
        # (``_source_enabled`` reads the live toggles); the Usenet strategy is created only
        # when a SABnzbd client is present.
        self._strategies: dict[str, SourceStrategy] = {
            "soulseek": SoulseekStrategy(
                indexer=indexer, scorer=scorer, track_matcher=track_matcher,
                client=client, store=download_store, file_processor=file_processor,
                staging=self._staging, manifest_codec=manifest_codec,
                naming_template=naming_template, library=library_manager,
            ),
        }
        # Created whenever a SABnzbd client exists (not gated on the indexer), so a Usenet
        # task can still IMPORT/enqueue even if search is disabled; search itself is gated by
        # ``_source_enabled`` (the live ``_usenet_enabled`` toggle, which requires the indexer).
        if usenet_client is not None:
            self._strategies["usenet"] = UsenetStrategy(
                indexer=usenet_indexer, scorer=usenet_scorer, client=usenet_client,
                store=download_store, file_processor=file_processor,
                import_settle_seconds=usenet_import_settle_seconds,
                staging=self._staging, manifest_codec=manifest_codec,
                naming_template=naming_template, album_service=album_service,
                category=usenet_category, priority=usenet_priority,
                post_processing=usenet_post_processing,
                min_release_age_seconds=usenet_min_release_age_minutes * 60.0,
                library=library_manager,
            )

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
                await self._store.update_status(task_id, DownloadStatus.FAILED, error_message=user_msg)
                logger.info(
                    "download.failed",
                    extra={"task_id": task_id, "error_message": user_msg},
                )
                await self._bus.publish(
                    f"download:{task_id}", "complete",
                    {"status": DownloadStatus.FAILED, "error": user_msg},
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
            if not self._source_enabled("soulseek") and not self._source_enabled("usenet"):
                # Disabled-but-configured slskd shouldn't read as "not configured".
                if self._client.is_configured():
                    raise OrchestrationError(
                        "No download source is enabled - turn on slskd or Usenet in Settings"
                    )
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
            await self._store.update_status(task_id, DownloadStatus.FAILED, error_message=user_msg)
            logger.info(
                "download.failed", extra={"task_id": task_id, "error_message": user_msg}
            )
            await self._bus.publish(
                f"download:{task_id}", "complete",
                {"status": DownloadStatus.FAILED, "error": user_msg},
            )
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)

    def _source_enabled(self, source: str) -> bool:
        if source == "soulseek":
            # Both the enable toggle AND a usable URL/key are required - a disabled-but-
            # configured slskd must not be routed to just because it's still configured.
            return self._soulseek_enabled and self._client.is_configured()
        if source == "usenet":
            return self._usenet_enabled
        return False

    def _enabled_source_names(self) -> list[str]:
        """Display names of the sources actually searched - so failure messages name what
        was tried, never a source that's switched off."""
        return [
            name
            for source, name in (("soulseek", "Soulseek"), ("usenet", "Usenet"))
            if self._source_enabled(source)
        ]

    def _no_source_message(self) -> str:
        """The 'nothing usable came back' message, naming the sources that were actually
        searched - so a Usenet-only setup reads "...on Usenet", never "...on Soulseek".
        Search hits every enabled source, so both are named when both are on."""
        names = self._enabled_source_names()
        return f"{_NO_SOURCE_MSG} on {' or '.join(names)}" if names else _NO_SOURCE_MSG

    def _no_match_message(self) -> str:
        """The 'the indexers returned nothing for this album' message, naming the sources
        actually searched. A Usenet-only setup reads "...on Usenet" - surfacing that the
        album may well be on Soulseek, which is currently disabled - instead of the
        misleading "...on any source"."""
        names = self._enabled_source_names()
        joined = " or ".join(names) if names else "any source"
        return f"No matching release found on {joined}"

    async def _search_and_score(self, task, source: str):  # noqa: ANN001, ANN201
        """Search ONE source and return its scored candidates (tagged with ``source``),
        via the source strategy (step 4). Called only for sources that ``_source_enabled``
        passed, so the strategy is always present."""
        timeout = 30.0 + 15.0 * min(task.retry_count, 4)
        return await self._strategies[source].search_and_score(
            task, timeout=timeout, auto=self._auto, manual=self._manual
        )

    async def _search_score_autopick(self, task) -> bool:  # noqa: ANN001 - DownloadTask
        """Route the automatic path across ``source_priority`` (D3: Soulseek-first,
        Usenet-fallback). A source's auto-accept candidate is picked immediately and
        later sources are NOT searched. If no source auto-accepts, all candidates are
        pooled into ONE source-grouped review job (D16) and parked, or failed.

        Returns True iff a candidate was auto-picked + linked; False when parked/failed."""
        job = await self._store.create_search_job(
            user_id=task.user_id,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
            artist_mbid=task.artist_mbid,
            search_query=f"{task.artist_name} - {task.album_title}",
        )

        remembered: list[list] = []
        for source in self._source_priority:
            if not self._source_enabled(source):
                continue
            candidates = await self._search_and_score(task, source)
            remembered.append(candidates)
            logger.info(
                "download.search.completed",
                extra={
                    "task_id": task.id,
                    "source": source,
                    "candidates_count": len(candidates),
                    "top_score": candidates[0].final_score if candidates else 0.0,
                },
            )
            top = candidates[0] if candidates else None
            if top and top.tier == "auto":
                pooled = [c for group in remembered for c in group]
                index = sum(len(group) for group in remembered[:-1])  # top is index 0 of last
                await self._store.set_search_job_candidates(job.id, pooled)
                await self._store.link_picked_candidate(
                    task_id=task.id, search_job_id=job.id, candidate_index=index,
                    source_username=top.username, source_directory=top.parent_directory,
                    preflight_score=top.final_score, source=top.source,
                    download_client=_CLIENT_FOR_SOURCE.get(top.source, "slskd"),
                )
                return True

        # No source auto-accepted: pool all candidates (source-grouped, D16) for review.
        pooled = [c for group in remembered for c in group]
        await self._store.set_search_job_candidates(job.id, pooled)
        if any(c.tier in ("auto", "manual") for c in pooled):
            await self._store.set_search_job_id_and_candidate(task.id, job.id, None)
            await self._store.update_search_job_status(job.id, "completed")
            await self._bus.publish(
                f"download:{task.id}", "status",
                {"status": DownloadStatus.AWAITING_REVIEW, "search_job_id": job.id},
            )
            return False

        await self._store.update_search_job_status(job.id, "completed")
        if task.origin == "upgrade":
            # No candidate beat the upgrade floor: NOT a failure - the library is
            # intact and nothing was attempted. End quietly (never enters the failed
            # bucket; upgrades are excluded from auto-retry anyway).
            await self._store.update_status(
                task.id, DownloadStatus.CANCELLED,
                error_message="No better copy found", cancelled_at=time.time(),
            )
            await self._bus.publish(
                f"download:{task.id}", "complete",
                {"status": DownloadStatus.CANCELLED, "error": "no better copy found"},
            )
            return False
        await self._store.update_status(
            task.id, DownloadStatus.FAILED, error_message=self._no_match_message()
        )
        await self._bus.publish(
            f"download:{task.id}", "complete", {"status": DownloadStatus.FAILED, "error": "no match"}
        )
        return False

    def _strategy(self, source: str) -> SourceStrategy:
        """The strategy for a source, falling back to Soulseek for an unknown/disabled
        source. This preserves the old ``_download_client_for`` fallback (a Usenet task with
        no SABnzbd client resolved to the slskd client): the Usenet strategy exists iff a
        SABnzbd client exists, so a missing one falls through to Soulseek's client here."""
        return self._strategies.get(source) or self._strategies["soulseek"]

    def _download_client_for(self, task) -> "DownloadClientProtocol":  # noqa: ANN001
        """The download client that owns this task's source (D2/D3)."""
        return self._strategy(task.source).client

    async def _enqueue(  # noqa: ANN001 - DownloadTask
        self, task, *, strict_track_duration: bool = True, hold_on_wrong_track: bool = False
    ) -> None:
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if task.candidate_index is None or task.candidate_index >= len(candidates):
            raise OrchestrationError("candidate no longer available")
        candidate = candidates[task.candidate_index]
        await self._strategies[task.source].enqueue(
            task, candidate, strict_track_duration=strict_track_duration,
            hold_on_wrong_track=hold_on_wrong_track,
        )

    async def _poll_until_done(self, task, *, expect_materialization: bool = False):  # noqa: ANN001, ANN201
        """Poll slskd until the transfer terminates, stalls, or hits the ceiling.

        Returns ``(outcome, last_status)``. The watchdog watches real byte
        progress: an actively-transferring peer that stops moving bytes for
        ``stall_timeout`` is stalled; one still sitting in the peer's remote upload
        queue for ``queued_timeout`` is given up on. ``expect_materialization`` (set
        only for a fresh enqueue) additionally bails fast if no transfer record ever
        appears. ``_run_with_failover`` decides what to do with the outcome - this
        method never discards progress."""
        manifest = self._read_manifest(task.id)
        handle = manifest.handle
        client = self._download_client_for(task)
        loop = asyncio.get_running_loop()
        enqueue_time = loop.time()
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
                if current is not None and current.status == DownloadStatus.CANCELLED:
                    raise _Cancelled()
                status = await client.get_status(handle)
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
                # A fresh enqueue that never produced a transfer record (peer offline /
                # silently rejected) is a no-show: fail over fast rather than wait out
                # the full queued window. A genuinely queued transfer HAS a record, so
                # this can't misfire on a slow-but-real peer.
                now = loop.time()
                if (
                    expect_materialization
                    and status.matched_transfers == 0
                    and now - enqueue_time >= _TRANSFER_MATERIALIZE_SECONDS
                ):
                    return _OUT_NO_TRANSFER, status
                # Non-terminal: run the stall/queued watchdog off real byte progress.
                if status.bytes_downloaded > last_progress_bytes:
                    last_progress_bytes = status.bytes_downloaded
                    last_progress_time = now
                else:
                    idle = now - last_progress_time
                    if status.has_active_transfer and idle >= self._stall_timeout:
                        return _OUT_STALLED, status
                    # SABnzbd Queued/Paused/post-processing move 0 bytes and aren't
                    # 'Downloading', so they'd accrue the queued clock - the Usenet strategy
                    # sets applies_queued_timeout False (the 6h deadline is its only backstop).
                    if (
                        self._strategy(task.source).applies_queued_timeout
                        and not status.has_active_transfer
                        and idle >= self._queued_timeout
                    ):
                        return _OUT_QUEUED, status
                await asyncio.sleep(self._poll_interval)
            if last_status is None:
                last_status = await client.get_status(handle)
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
            IMPORT_FAILED,
            SOURCE_FILE_MISSING,
            WRONG_TRACK,
        )

        attempts = 0
        tried_usernames: set[str] = set()
        first = True
        imported_any = False
        wrong_track = False
        source_missing = False
        import_failed = False
        while True:
            # resume's first iteration polls the transfers a restart left behind (no
            # enqueue), so the no-transfer fast-fail must not apply there - those
            # records may legitimately be gone (completed + cleaned).
            did_enqueue = not (first and resume)
            enqueued = True
            if did_enqueue:
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
                outcome, status = await self._poll_until_done(
                    task, expect_materialization=did_enqueue
                )
                # Re-check for an out-of-band cancel in the window between the poll
                # loop's last check and here, so we don't overwrite 'cancelled' and
                # import anyway (the failover loop runs this sequence up to N times).
                current = await self._store.get_task(task.id)
                if current is not None and current.status == DownloadStatus.CANCELLED:
                    raise _Cancelled()
                await self._store.update_status(task.id, DownloadStatus.PROCESSING)
                await self._bus.publish(
                    f"download:{task.id}", "status", {"status": DownloadStatus.PROCESSING}
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
                result, enumerated = await self._import_files(
                    task, only_filenames=only, completed=outcome == _OUT_COMPLETED
                )
                if result.succeeded:
                    imported_any = True
                # Per-attempt fault flags (NOT the accumulated ones below) decide whether
                # THIS candidate's shortfall is the release's fault or a local one.
                attempt_mount = not result.succeeded and any(
                    f.reason == DOWNLOADS_MOUNT_UNAVAILABLE for f in result.failed
                )
                attempt_import_fault = any(
                    f.reason in (IMPORT_FAILED, SOURCE_FILE_MISSING) for f in result.failed
                )
                if any(f.reason == WRONG_TRACK for f in result.failed):
                    wrong_track = True
                if any(f.reason == SOURCE_FILE_MISSING for f in result.failed):
                    source_missing = True
                if any(f.reason == IMPORT_FAILED for f in result.failed):
                    import_failed = True
                # An unreachable downloads mount, or a SABnzbd-reported disk/write/permission
                # error, is an ENVIRONMENT fault, not the release's fault: Lidarr treats an
                # unreachable download path / disk error as a warning, never a release
                # failure. Stop without failing over (another peer can't fix a local problem)
                # and let the backoff'd auto-retry try once the environment recovers.
                strategy = self._strategy(task.source)
                sab_local_fault = (
                    strategy.has_local_disk_faults
                    and outcome == _OUT_TERMINAL
                    and _is_local_fault(status.error if status else "")
                )
                local_fault = attempt_mount or sab_local_fault
                is_complete = await self._download_is_complete(task, imported_any, result)
                # A release that genuinely finished (e.g. SABnzbd Completed/Failed) but did NOT
                # deliver what was requested is blocklisted by source identity BEFORE failover so
                # a re-search/retry finds a COMPLETE release instead of re-grabbing this one
                # (Lidarr's "Redownload Failed" + blocklist). Skipped for an interrupted poll, a
                # local/environment fault, or a local IMPORT fault (the files arrived but we
                # failed to write them - not the release's fault, review H3). The strategy owns
                # the source-specific blocklist (Usenet: age-guarded title+size; Soulseek: a
                # no-op - its per-file quarantine already ran at import).
                if (
                    not is_complete
                    and outcome in (_OUT_COMPLETED, _OUT_TERMINAL)
                    and not local_fault
                    and not attempt_import_fault
                ):
                    await strategy.maybe_blocklist_on_failure(
                        task, status,
                        completed=outcome == _OUT_COMPLETED, enumerated_any=enumerated > 0,
                    )

                # A local/environment fault stops the task WITHOUT cleanup: cancel(del_files)
                # would tell the client to delete data we simply couldn't read (the mount may
                # recover), so bail BEFORE _cancel_transfers (review H1). Failing over can't
                # fix a local problem either.
                if local_fault:
                    await self._finalize(
                        task, DownloadStatus.FAILED,
                        error_message=strategy.local_fault_message(attempt_mount),
                    )
                    return

                await self._cancel_transfers(task)

                if is_complete:
                    await self._finalize(task, DownloadStatus.COMPLETED)
                    return

            # Incomplete (or this candidate's enqueue failed): fail over. Track the
            # tried release by its SOURCE identity (slskd peer username; Usenet title+
            # size) so failover dedups correctly within the source (review M2).
            await self._mark_candidate_tried(task, tried_usernames)
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
                await self._settle_incomplete(
                    task, imported_any,
                    source_missing=source_missing, import_failed=import_failed,
                )
                return
            task = nxt
            await self._bus.publish(
                f"download:{task.id}", "status",
                {"status": DownloadStatus.RETRYING, "attempt": attempts},
            )

    async def _fallback_track_repull(self, task) -> None:  # noqa: ANN001 - DownloadTask
        """Last resort for a per-track download whose every candidate was rejected on
        duration (the MB length is suspect): re-pull the top-ranked source and HOLD its
        file for human review on a repeat gate failure (D9) - the held-imports panel's
        "import anyway" is the path to the closest match, never a silent unverified import.
        A file that passes the gate on the re-pull (transient earlier failure) still
        imports normally."""
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if not candidates:
            await self._settle_incomplete(task, False)
            return
        cand = candidates[0]
        await self._store.link_picked_candidate(
            task.id, task.search_job_id, 0,
            cand.username, cand.parent_directory, cand.final_score,
            source=cand.source, download_client=_CLIENT_FOR_SOURCE.get(cand.source, "slskd"),
        )
        task = await self._store.get_task(task.id)
        logger.info("download.track_duration_fallback", extra={"task_id": task.id})
        try:
            await self._enqueue(task, hold_on_wrong_track=True)
        except OrchestrationError:
            await self._settle_incomplete(task, False)
            return
        # fresh enqueue -> fail fast if the peer never materialises a transfer
        outcome, status = await self._poll_until_done(task, expect_materialization=True)
        await self._store.update_status(task.id, DownloadStatus.PROCESSING)
        only = None if outcome in (_OUT_COMPLETED, _OUT_TERMINAL) else set(
            status.succeeded_filenames
        )
        result, _enumerated = await self._import_files(
            task, only_filenames=only, completed=outcome == _OUT_COMPLETED
        )
        await self._cancel_transfers(task)
        await self._finalize(task, DownloadStatus.COMPLETED if result.succeeded else DownloadStatus.FAILED)

    async def _import_files(self, task, manifest_override=None, *, only_filenames=None, completed=False):  # noqa: ANN001, ANN201
        """Import a subset of the manifest into the library via the source strategy (per-file
        for slskd, unpacked-folder for Usenet), quarantining only files that arrived but
        failed verification. Does not set the task's terminal status (the failover loop owns
        that). Returns ``(ProcessResult, audio_files_enumerated)``; the count lets
        _run_with_failover tell an under-delivering release (files present but short) from an
        ambiguous empty folder.

        ``completed`` = SABnzbd reported the job finished (vs an interrupted/failed poll);
        only then can a still-empty folder mean a mount fault rather than a slow unpack.
        ``manifest_override`` skips the on-disk read (used by reimport_task, which builds
        the manifest from DB data because _finalize already deleted the staging copy)."""
        manifest = manifest_override if manifest_override is not None else self._read_manifest(task.id)
        return await self._strategies[task.source].import_files(
            task, manifest, only_filenames=only_filenames, completed=completed
        )

    async def _cancel_transfers(self, task, manifest_override=None) -> None:  # noqa: ANN001 - DownloadTask
        """Clear this task's download records (post-import, per DEC-1) and stop any still
        running. For Usenet this also deletes the unpacked data (del_files) - the post-
        import cleanup that discards the album's other tracks on a per-track grab (D4).
        Best-effort; imported audio has already been MOVED out.
        ``manifest_override`` skips the on-disk read (used by reimport_task)."""
        manifest = manifest_override if manifest_override is not None else self._read_manifest(task.id)
        strategy = self._strategy(task.source)
        if not strategy.is_cancelable(task, manifest):
            return
        try:
            await strategy.client.cancel(manifest.handle)
        except Exception:  # noqa: BLE001 - cleanup must not fail the task
            logger.warning("Failed to remove/stop transfers for task %s", task.id)

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
            # Stay within the task's source (never cross Soulseek<->Usenet in the pooled
            # job) and skip an identity we've already tried (review M2).
            if cand.source != task.source:
                continue
            if self._candidate_source_identity(cand) in tried_usernames:
                continue
            await self._store.link_picked_candidate(
                task.id, task.search_job_id, idx,
                cand.username, cand.parent_directory, cand.final_score,
                source=cand.source, download_client=_CLIENT_FOR_SOURCE.get(cand.source, "slskd"),
            )
            return await self._store.get_task(task.id)
        return None

    def _candidate_source_identity(self, cand) -> str:  # noqa: ANN001 - ScoredCandidate
        return self._strategy(cand.source).candidate_identity(cand)

    async def _mark_candidate_tried(self, task, tried: set) -> None:  # noqa: ANN001
        if task.search_job_id is None or task.candidate_index is None:
            tried.add(task.source_username or "")
            return
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if 0 <= task.candidate_index < len(candidates):
            tried.add(self._candidate_source_identity(candidates[task.candidate_index]))

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

    async def _coverage(self, task, *, context: str) -> "tuple[int, int, list[str]] | None":  # noqa: ANN001
        """``(covered, expected_total, orphan_row_ids)`` for an album task, measured
        against the requested release's MusicBrainz tracklist - or ``None`` when the
        tracklist is unavailable (MB down, no album service wired, empty/free-text
        release group), falling the caller back to the count check. Each expected
        track is covered by at most one library row (recording MBID -> position +
        duration -> containment title, via ``row_covers_track``); rows covering
        nothing are the ORPHANS the ``download.coverage`` event surfaces (P4/P5).
        Fail-open by design: coverage is an upgrade over counting, never a blocker."""
        if self._album_service is None or not task.release_group_mbid:
            return None
        try:
            info = await self._album_service.get_album_tracks_info(
                task.release_group_mbid, priority=RequestPriority.BACKGROUND_SYNC
            )
        except Exception:  # noqa: BLE001 - MB failure must never block completion
            logger.warning(
                "coverage.tracklist_unavailable",
                extra={"task_id": task.id, "release_group_mbid": task.release_group_mbid},
            )
            return None
        tracks = list(info.tracks or [])
        if not tracks:
            return None
        try:
            rows = await self._library.get_file_rows_for_album(task.release_group_mbid)
        except Exception:  # noqa: BLE001 - completeness check must not crash the task
            return None

        covered, orphan_rows, _matched = match_rows_to_tracks(rows, tracks)
        orphans = [str(r.get("id")) for r in orphan_rows if r.get("id")]
        logger.info(
            "download.coverage",
            extra={
                "task_id": task.id,
                "context": context,
                "expected": len(tracks),
                "covered": covered,
                "orphan_row_ids": orphans,
            },
        )
        return covered, len(tracks), orphans

    async def _download_is_complete(self, task, imported_any: bool, result=None) -> bool:  # noqa: ANN001
        """Whether the download has delivered what it set out to. A per-track
        download is one file - complete the moment it imports (Soulseek rips rarely
        carry the recording MBID, so a tag-based check can't be trusted). An album is
        complete once the library COVERS the requested release's tracklist (P4:
        recording/position+duration/title matching - a wrong file at some position
        can no longer satisfy the request the way the 2026-07-05 single was
        satisfied), cumulative across failover attempts. When the tracklist is
        unavailable the pre-P4 count check is the fallback: at least ``track_count``
        distinct positions present."""
        if task.download_type == "track":
            return imported_any
        coverage = await self._coverage(task, context="completeness")
        if coverage is not None:
            covered, expected_total, _orphans = coverage
            return covered >= expected_total
        expected = self._expected_track_count(task)
        present = await self._imported_track_count(task)
        if expected > 0:
            return present >= expected
        # Unknown album track count (MusicBrainz gave none): completeness can't be
        # measured, so the best signal is "this source delivered all it had" - a clean
        # full import with no failures. A partial then fails over to try a fuller
        # source before settling, rather than declaring done on the first track.
        return bool(result and result.succeeded and not result.failed)

    async def _settle_incomplete(  # noqa: ANN001
        self, task, imported_any: bool, *,
        source_missing: bool = False, import_failed: bool = False,
    ) -> None:
        """No candidates/attempts left and the download still isn't whole. A track
        either imported (already finalized 'completed') or it didn't ('failed'); an
        album keeps whatever landed as 'partial', or 'failed' if nothing did.

        ``source_missing``/``import_failed`` flip the failure message off the default
        'no source on Soulseek': slskd delivered the files but we either couldn't find
        them on the mount (config) or couldn't write them into the library (perms/disk).
        Both are local faults - blaming Soulseek sent users chasing the wrong problem
        (AUD: 'watched it finish in slskd, then it said no source')."""
        if source_missing:
            fail_msg = _FILES_NOT_FOUND_MSG
        elif import_failed:
            fail_msg = _IMPORT_FAILED_MSG
        else:
            fail_msg = self._no_source_message()
        if task.download_type == "track":
            await self._finalize(task, DownloadStatus.FAILED, error_message=fail_msg)
            return
        if await self._imported_track_count(task) > 0:
            await self._finalize(task, DownloadStatus.PARTIAL)
        else:
            await self._finalize(task, DownloadStatus.FAILED, error_message=fail_msg)

    async def settle_after_manual_import(self, task_id: str | None) -> None:
        """A held track was manually imported into the library ('import anyway'). Re-measure
        the album against the library and reflect it on the source task, so a now-complete
        album stops showing a phantom 'retry scheduled': finalize it completed once every
        expected track is present; otherwise just advance its imported-file count (it stays
        partial - still paused on any other held track, or retryable for a genuinely missing
        one). Best-effort and idempotent."""
        if not task_id:
            return
        task = await self._store.get_task(task_id)
        if task is None or task.status in (DownloadStatus.COMPLETED, DownloadStatus.CANCELLED):
            return
        expected = self._expected_track_count(task)
        present = 1 if task.download_type == "track" else await self._imported_track_count(task)
        # D8: the human's "import anyway" is the escape hatch, so the DECISION stays
        # count-based (a force-imported file may deliberately not match MusicBrainz) -
        # but the coverage event still records honestly what is and isn't covered,
        # never a silent COMPLETED. (The force-import stamps the expected recording
        # MBID onto the file, so it usually covers anyway.)
        if task.download_type != "track":
            await self._coverage(task, context="manual_import")
        if expected and present >= expected:
            await self._finalize(task, DownloadStatus.COMPLETED)
        else:
            await self._store.update_status(task.id, task.status, files_completed=present)

    async def _finalize(self, task, status, *, error_message=None) -> None:  # noqa: ANN001
        if task.download_type == "track":
            present = 1 if status == DownloadStatus.COMPLETED else 0
            raw_expected = 1
        else:
            present = await self._imported_track_count(task)
            raw_expected = self._expected_track_count(task)
        # raw_expected==0 means the target size is UNKNOWN (MusicBrainz gave no track
        # count): completeness can't be measured, so 'completed' here is a best-effort
        # signal, not a verified full album. Collapse files_total to what landed for the
        # UI, but log expected_known so a 1/1 'completed' on an unmeasured album is
        # distinguishable from a genuine 1-track one.
        expected = raw_expected or present
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
        event = "download.failed" if status == DownloadStatus.FAILED else "download.completed"
        logger.info(
            event,
            extra={
                "task_id": task.id,
                "status": status,
                "files_completed": present,
                "files_total": expected,
                "expected_known": raw_expected > 0,
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
            DownloadStatus.COMPLETED: "imported",
            DownloadStatus.PARTIAL: "incomplete",
            DownloadStatus.FAILED: "failed",
            DownloadStatus.CANCELLED: "cancelled",
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
            active = await self._store.list_active_tasks([DownloadStatus.DOWNLOADING, DownloadStatus.PROCESSING])
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
                task.id, DownloadStatus.FAILED,
                error_message="Download interrupted - no progress after a restart",
                completed_at=now,
            )
            await self._bus.publish(
                f"download:{task.id}", "complete",
                {"status": DownloadStatus.FAILED, "error": "download interrupted"},
            )
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)
            logger.warning(
                "Reaped stale download task %s (no poller for %.0fs)", task.id, now - last
            )

    async def startup_resume(self) -> None:
        """Resume in-progress tasks after a restart (AUD-3): never block startup."""
        registry = TaskRegistry.get_instance()

        for orphan in await self._store.list_active_tasks([DownloadStatus.QUEUED]):
            # queued = created but never enqueued -> re-dispatch (failing would be
            # spurious; they never started).
            self.dispatch(orphan.id)

        for task in await self._store.list_active_tasks([DownloadStatus.DOWNLOADING, DownloadStatus.PROCESSING]):
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
                task_id, DownloadStatus.FAILED, error_message=_user_error_message(exc)
            )
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)
        except Exception as exc:  # noqa: BLE001 - resume failure -> mark failed
            logger.exception("Failed to resume task %s", task_id)
            await self._store.update_status(
                task_id, DownloadStatus.FAILED, error_message=_user_error_message(exc)
            )
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)

    async def cancel_task(self, task_id: str, user_id: str, user_role: str) -> None:
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if user_role != "admin" and task.user_id != user_id:
            raise PermissionDeniedError("Cannot cancel another user's download")

        manifest_path = self._staging / task_id / "manifest.json"
        if manifest_path.exists():
            try:
                manifest = self._manifest_codec.decode(manifest_path.read_bytes())
                strategy = self._strategy(task.source)
                if strategy.is_cancelable(task, manifest):
                    await strategy.client.cancel(manifest.handle)
            except Exception as exc:  # noqa: BLE001 - best-effort
                logger.warning("Failed to cancel transfers for %s: %s", task_id, exc)

        # Stop the live poll loop promptly if one is running in this process.
        handle = self._active_tasks.pop(task_id, None)
        if handle is not None and not handle.done():
            handle.cancel()

        await self._store.update_status(task_id, DownloadStatus.CANCELLED, cancelled_at=time.time())
        logger.info(
            "download.cancelled", extra={"task_id": task_id, "user_id": task.user_id}
        )
        # Flip the linked request to 'cancelled' too, so a cancelled (or stopped-retrying)
        # download clears the album UI's "retry scheduled" line instead of sitting failed.
        await self._sync_request_on_terminal(task, DownloadStatus.CANCELLED)
        await self._bus.publish(
            f"download:{task_id}", "complete", {"status": DownloadStatus.CANCELLED}
        )

    async def retry_task(self, task_id: str, user_id: str, user_role: str) -> str:
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if user_role != "admin" and task.user_id != user_id:
            raise PermissionDeniedError("Cannot retry another user's download")
        if task.status not in (DownloadStatus.FAILED, DownloadStatus.CANCELLED, DownloadStatus.PARTIAL):
            raise ValidationError("Only failed, cancelled or partial downloads can be retried")

        # Manual retry is an explicit "try again": clear the album's blocklist so a release
        # quarantined by the failed attempt is reconsidered. Album downloads only - a
        # per-track retry must not wipe the whole album's blocklist. Auto-retry
        # (retry_failed_tasks -> _create_retry_task) deliberately does NOT clear.
        if task.download_type == "album" and task.release_group_mbid:
            cleared = await self._store.delete_quarantine_for_album(task.release_group_mbid)
            if cleared:
                logger.info(
                    "download.blocklist_cleared_on_retry",
                    extra={"release_group_mbid": task.release_group_mbid, "cleared": cleared},
                )

        return await self._create_retry_task(task)

    async def reimport_task(self, task_id: str):  # noqa: ANN201
        """Re-run only the import half of the pipeline for a ``failed``/``partial``
        task whose download the user finished by hand in slskd (e.g. resumed a
        stalled/errored transfer in slskd's own UI after DroppedNeedle had already
        given up). This re-resolves the SAME candidate slskd already picked.
        Admin-gated at the route (``CurrentAdminDep``)."""
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if task.status not in ("failed", "partial"):
            raise ValidationError("Only failed or partial downloads can be reimported")
        if (
            task.search_job_id is None
            or task.candidate_index is None
            or not task.source_username
        ):
            raise ValidationError("This download never selected a source to reimport from")

        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if task.candidate_index >= len(candidates):
            raise ValidationError("Original source is no longer available")
        candidate = candidates[task.candidate_index]

        # A 1-track album (a single) reimports under the same canonical-duration
        # verification as a track download (2026-07-05 wrong-single incident).
        is_single = task.download_type == "album" and task.track_count == 1
        use_canonical = (
            task.download_type == "track" or is_single
        ) and bool(task.track_duration_seconds)
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
            expected_tracks=(
                [
                    ExpectedTrack(
                        track_number=task.track_number or 1,
                        disc_number=task.disc_number or 1,
                        duration_seconds=task.track_duration_seconds,
                        recording_mbid=task.recording_mbid,
                        title=task.track_title,
                    )
                ]
                if task.track_title and len(candidate.files) == 1
                else []
            ),
        )

        await self._store.update_status(task.id, DownloadStatus.PROCESSING)
        await self._bus.publish(
            f"download:{task.id}", "status", {"status": DownloadStatus.PROCESSING}
        )

        try:
            result, _ = await self._import_files(task, manifest, completed=True)

            # A mount fault stops the task WITHOUT cleanup: cancel(del_files) would tell
            # the client to delete data we couldn't read (the mount may recover), so bail
            # BEFORE _cancel_transfers - the failover loop does the same (review H1).
            if not result.succeeded and any(
                f.reason == DOWNLOADS_MOUNT_UNAVAILABLE for f in result.failed
            ):
                await self._finalize(
                    task, DownloadStatus.FAILED, error_message=DOWNLOADS_MOUNT_UNAVAILABLE
                )
                return await self._store.get_task(task.id)

            await self._cancel_transfers(task, manifest)

            if await self._download_is_complete(task, bool(result.succeeded), result):
                await self._finalize(task, DownloadStatus.COMPLETED)
            elif result.succeeded:
                await self._finalize(task, DownloadStatus.PARTIAL)
            else:
                if any(f.reason == SOURCE_FILE_MISSING for f in result.failed):
                    fail_msg = _FILES_NOT_FOUND_MSG
                elif any(f.reason == IMPORT_FAILED for f in result.failed):
                    fail_msg = _IMPORT_FAILED_MSG
                else:
                    fail_msg = _NO_SOURCE_MSG
                await self._finalize(task, DownloadStatus.FAILED, error_message=fail_msg)
        except Exception:
            logger.exception("Unexpected error during reimport of task %s", task.id)
            await self._finalize(
                task, DownloadStatus.FAILED, error_message="Reimport failed unexpectedly"
            )

        return await self._store.get_task(task.id)

    async def _create_retry_task(self, task) -> str:  # noqa: ANN001 - DownloadTask
        """Create a fresh queued task carrying ``retry_count + 1`` and dispatch it.
        The original is kept (terminal) for audit. Shared by manual retry and
        auto-retry."""
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
            track_number=task.track_number,
            disc_number=task.disc_number,
            year=task.year,
            track_count=task.track_count,
            track_duration_seconds=task.track_duration_seconds,
            search_query=task.search_query,
            # An upgrade's retry must stay an upgrade (keeps the origin-aware gate,
            # replace-on-import and cap/quota exemptions working across retries);
            # everything else becomes 'retry' so quota counts ignore it.
            origin=task.origin if task.origin == "upgrade" else "retry",
            retry_count=task.retry_count + 1,
        )
        await self._relink_request(task, new_task.id)
        self.dispatch(new_task.id)
        return new_task.id

    async def _relink_request(self, task, new_task_id: str) -> None:  # noqa: ANN001 - DownloadTask
        """Point the linked request at the replacement task. Without this a retried
        download imports the album but ``_sync_request_on_terminal`` (keyed on
        ``download_task_id == task.id``) ignores the new task, so the request stays
        ``failed`` and the import cache-bust never fires. Album downloads only, and
        only when THIS task still owns the link - a per-track retry must not hijack
        the album's request, and a request already re-linked to a newer task is left
        alone."""
        if (
            self._request_history is None
            or task.download_type != "album"
            or not task.release_group_mbid
        ):
            return
        try:
            record = await self._request_history.async_get_record(task.release_group_mbid)
            if record is not None and getattr(record, "download_task_id", None) == task.id:
                await self._request_history.async_update_download_task_id(
                    record.musicbrainz_id, new_task_id
                )
        except Exception:  # noqa: BLE001 - re-link must never fail the retry
            logger.warning("Could not re-link request for retry of %s", task.id)

    @property
    def auto_retry_max(self) -> int:
        """Configured max auto-retry attempts (0 when auto-retry is off)."""
        return self._auto_retry_max_attempts if self._auto_retry_enabled else 0

    def _retry_backoff_seconds(self, retry_count: int) -> float:
        # Per-task exponential backoff: base * 2^retry_count, capped at 24h. A task
        # retried 5 times waits far longer than one that failed on its first attempt.
        return min(self._auto_retry_base_interval * 60.0 * (2**retry_count), 86400.0)

    def retry_ladder_minutes(self) -> list[int]:
        """The FULL auto-retry backoff schedule (minutes) for the configured attempt
        cap - e.g. base 15m, max 6 -> [15, 30, 60, 120, 240, 480]. Empty when auto-retry
        is off / max is 0. Same formula the retry sweep uses, so the UI's ladder matches
        when each attempt actually fires."""
        return [round(self._retry_backoff_seconds(n) / 60) for n in range(self.auto_retry_max)]

    def next_retry_at(self, task) -> float | None:  # noqa: ANN001 - DownloadTask
        """Unix time the task's next auto-retry is due, or None if it won't auto-retry
        (disabled, not failed/partial, or attempts exhausted). Same anchor+formula the
        retry sweep uses, so the UI's "retry scheduled" lines up with when it fires."""
        if (
            not self._auto_retry_enabled
            or task.status not in (DownloadStatus.FAILED, DownloadStatus.PARTIAL)
            or task.retry_count >= self._auto_retry_max_attempts
        ):
            return None
        anchor = task.completed_at or task.created_at
        if not anchor:
            return None
        return anchor + self._retry_backoff_seconds(task.retry_count)

    async def retry_failed_tasks(self) -> None:
        """Periodic safety net: re-dispatch ``failed``/``partial`` downloads whose
        per-task exponential backoff has elapsed, up to ``auto_retry_max_attempts``.
        Mirrors the lidarr QueueCleaner pattern - a failed download sits until the
        system retries it, giving the Soulseek network time to surface new sources.
        Skips any task that already has a newer active task for the same album/track
        + user (e.g. a manual retry or a new request)."""
        if not self._auto_retry_enabled:
            return

        now = time.time()

        eligible = await self._store.list_retryable_tasks(self._auto_retry_max_attempts)
        for task in eligible:
            backoff = self._retry_backoff_seconds(task.retry_count)
            completed_at = task.completed_at or task.created_at or 0.0
            if now - completed_at < backoff:
                continue

            # Paused for review: this task left a track held ("couldn't verify"). Re-downloading
            # the same recording just fails the same way, so we wait for the human (import anyway
            # / discard); discarding the held track clears this and lets auto-retry resume.
            if await self._store.has_unresolved_held_for_task(task.id):
                continue

            # Skip if there's already a newer active task for the same target +
            # user (a manual retry or a new request). The check is per-album for
            # album downloads, per-recording for track downloads.
            if task.download_type == "track" and task.recording_mbid:
                active = await self._store.get_active_task_for_track(
                    task.recording_mbid, task.user_id
                )
            else:
                active = await self._store.get_active_task_for_album(
                    task.release_group_mbid, task.user_id
                )
            if active is not None and active.id != task.id:
                continue

            logger.info(
                "download.auto_retry",
                extra={
                    "task_id": task.id,
                    "retry_count": task.retry_count,
                    "download_type": task.download_type,
                    "release_group_mbid": task.release_group_mbid,
                },
            )
            await self._bus.publish(
                f"download:{task.id}", "auto_retry",
                {
                    "retry_count": task.retry_count + 1,
                    "max_attempts": self._auto_retry_max_attempts,
                },
            )
            await self._create_retry_task(task)

    def _read_manifest(self, task_id: str) -> DownloadManifest:
        path = self._staging / task_id / "manifest.json"
        if not path.exists():
            raise OrchestrationError("manifest missing")
        return self._manifest_codec.decode(path.read_bytes())
