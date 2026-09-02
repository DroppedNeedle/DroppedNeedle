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
from contextlib import suppress
from pathlib import Path
from types import SimpleNamespace

import msgspec
import msgspec as _msgspec

from core.exceptions import (
    ConflictError,
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from core.task_registry import TaskRegistry
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.sse_publisher import SSEPublisher
from models.acquisition_quality import AcquisitionQualitySnapshot
from services.native.acquisition import quality as acq_quality
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
    _expected_tracks_for_task,
)
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native.coverage import match_rows_to_tracks, uncovered_tracks
from services.native.quality_tiers import (
    candidate_tier,
    effective_extension,
    folder_hires_key,
    in_range,
    is_audio,
    is_flac_or_mp3,
    tier_rank,
)
from services.native.file_processor import (
    DOWNLOADS_MOUNT_UNAVAILABLE,
    IMPORT_FAILED,
    SOURCE_FILE_MISSING,
    FileProcessor,
    ProcessResult,
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
    "disk is full",
    "disk full",
    "no space",
    "not enough disk",
    "write error",
    "failed moving",
    "moving failed",
    "permission denied",
    "cannot write",
    "could not create",
    "read-only file system",
)


def _is_local_fault(message: str | None) -> bool:
    low = (message or "").lower()
    return any(m in low for m in _LOCAL_FAULT_MARKERS)


def _generation_of(value: object | None) -> int | None:
    generation = getattr(value, "generation", None)
    return generation if isinstance(generation, int) and not isinstance(generation, bool) else None


# _poll_until_done outcomes.
_OUT_COMPLETED = "completed"  # every transfer terminal and succeeded
_OUT_TERMINAL = "terminal"  # every transfer terminal, at least one failed
_OUT_STALLED = "stalled"  # an active transfer stopped making progress
_OUT_QUEUED = "queued_timeout"  # stuck in the peer's remote upload queue too long
_OUT_DEADLINE = "deadline"  # hit the 6-hour absolute ceiling
_OUT_NO_TRANSFER = "no_transfer"  # a fresh enqueue produced no transfer record
_OUT_PREFERRED_QUALITY = "preferred_quality_timeout"

# Terminal "couldn't finish" messages. The mount one is used when slskd delivered the
# files but we then couldn't find them on the downloads mount - a local/config fault,
# not an absence of sources, so it must not read as "Soulseek had nothing".
# The "no source" wording is built per-task from the enabled sources (see
# _no_source_message) so a Usenet download never wrongly blames Soulseek.
_NO_SOURCE_MSG = "No working source found"
# Prefix of _no_match_message. Module-level (like _NO_SOURCE_MSG) so the wanted
# watcher's enrolment classifier IMPORTS it instead of copying the string - the
# tie-test in test_wanted_watcher_service fails loudly if either side drifts.
_NO_MATCH_MSG = "No matching release found"
_FILES_NOT_FOUND_MSG = (
    "Files downloaded, but couldn't be found in the slskd downloads folder - check "
    "the slskd downloads path points to where slskd saves completed files"
)
_TAG_MISMATCH_MSG = (
    "Files downloaded and found, but their embedded tags did not match the requested music"
)
# slskd delivered the files and we found them, but writing them into the library failed
# (perms, disk full, a cross-mount copy the filesystem rejected). Local fault, not the
# peer's - blaming Soulseek sends users chasing the wrong problem.
_IMPORT_FAILED_MSG = (
    "Files downloaded, but couldn't be saved into your library - check the library "
    "folder is writable and has free space"
)
_MANAGEMENT_HELD_MSG = "Download complete. The files are secured while Library Management waits for attention."
_MANAGEMENT_HOLD_STORAGE_MSG = (
    "Download complete, but DroppedNeedle could not secure its Library Management "
    "review copy. The original download was preserved."
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


def json_dumps_safe(snapshot) -> str:
    import json as _json
    from infrastructure.serialization import to_jsonable as _to

    return _json.dumps(_to(snapshot))


class _DefaultPolicyShim:
    """Last-resort legacy-default policy mirror for tests constructing the
    orchestrator without a policy getter."""
    quality_min = "mp3_320"
    quality_max = "lossless"
    quality_preference_order: list[str] = []
    preferred_lossy_bitrate_kbps = None
    lossy_min_bitrate_kbps = None
    lossy_max_bitrate_kbps = None
    lossless_preference = "highest"
    lossless_max_bit_depth = None
    lossless_max_sample_rate_hz = None
    flac_mp3_only = True
    unknown_quality_behavior = "allow_as_fallback"
    source_selection_mode = "source_first"


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
        preferred_quality_wait_minutes: float = 15.0,
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
        # Fresh reader of the current download policy: ONLY used to synthesise a
        # migration snapshot for legacy rows lacking one. Quality for live work
        # comes from each task's STORED snapshot; restart-with-current-policy is
        # the explicit refresh.
        get_download_policy=None,
        # Live non-quality spec gates (max size / terms / retention), refreshed
        # per search - never quality-shaped.
        spec_policy_extras=None,
        probe_tagger=None,  # AudioTagger for the pre-publication quality probe
        wanted_store=None,  # WantedStore | None
        cleanup_service: AcquisitionCleanupService | None = None,
    ) -> None:
        self._client = client
        self._naming_template = naming_template
        # Search/enqueue/import + the per-source policy all live on the strategies now; the
        # orchestrator keeps only the shared state below + the live enable toggles.
        self._usenet_enabled = (
            usenet_enabled and usenet_indexer is not None and usenet_client is not None
        )
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
        self._preferred_quality_wait = preferred_quality_wait_minutes * 60.0
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
        self._get_download_policy = get_download_policy
        self._spec_policy_extras = spec_policy_extras
        self._probe_tagger = probe_tagger
        self._wanted_store = wanted_store
        self._cleanup = cleanup_service
        self._usenet_scorer = usenet_scorer  # for the Usenet re-gate tier (Phase 2)
        self._active_tasks: dict[str, asyncio.Task] = {}
        self._operation_locks: dict[str, asyncio.Lock] = {}

        # Source strategies (step 4): all per-source behaviour (search, enqueue, import,
        # client, identity, blocklist-on-failure, poll/cancel/fault policy) lives here so the
        # orchestrator never branches on source. Enablement stays on the orchestrator
        # (``_source_enabled`` reads the live toggles); the Usenet strategy is created only
        # when a SABnzbd client is present.
        self._strategies: dict[str, SourceStrategy] = {
            "soulseek": SoulseekStrategy(
                indexer=indexer,
                scorer=scorer,
                track_matcher=track_matcher,
                client=client,
                store=download_store,
                file_processor=file_processor,
                staging=self._staging,
                manifest_codec=manifest_codec,
                naming_template=naming_template,
                library=library_manager,
                album_service=album_service,
                policy_extras=self._spec_policy_extras,
                probe_tagger=probe_tagger,
            ),
        }
        # Created whenever a SABnzbd client exists (not gated on the indexer), so a Usenet
        # task can still IMPORT/enqueue even if search is disabled; search itself is gated by
        # ``_source_enabled`` (the live ``_usenet_enabled`` toggle, which requires the indexer).
        if usenet_client is not None:
            self._strategies["usenet"] = UsenetStrategy(
                indexer=usenet_indexer,
                scorer=usenet_scorer,
                client=usenet_client,
                store=download_store,
                file_processor=file_processor,
                import_settle_seconds=usenet_import_settle_seconds,
                staging=self._staging,
                manifest_codec=manifest_codec,
                naming_template=naming_template,
                album_service=album_service,
                category=usenet_category,
                priority=usenet_priority,
                post_processing=usenet_post_processing,
                min_release_age_seconds=usenet_min_release_age_minutes * 60.0,
                library=library_manager,
                policy_extras=self._spec_policy_extras,
                probe_tagger=probe_tagger,
            )

    def dispatch(self, task_id: str) -> "asyncio.Task":
        """Run ``process_task`` for ``task_id`` in the background (AUD-3): wrapped in
        the safe runner, registered in ``TaskRegistry`` so shutdown cancels it, and
        tracked in ``_active_tasks`` so ``cancel_task`` can stop the live poll loop."""
        task = asyncio.create_task(self._run_orchestrator_safely(task_id))
        self._active_tasks[task_id] = task
        task.add_done_callback(_log_task_exception)
        task.add_done_callback(
            lambda done, _id=task_id: self._forget_active_task(_id, done)
        )
        TaskRegistry.get_instance().register(f"download-{task_id}", task)
        return task

    def _dispatch_resume(self, task_id: str) -> "asyncio.Task":
        """Resume an existing manifest after a manual action lost its zero-byte race."""

        task = asyncio.create_task(self._resume_single_task(task_id))
        self._active_tasks[task_id] = task
        task.add_done_callback(_log_task_exception)
        task.add_done_callback(
            lambda done, _id=task_id: self._forget_active_task(_id, done)
        )
        TaskRegistry.get_instance().register(f"download-resume-{task_id}", task)
        return task

    def _operation_lock(self, task_id: str) -> asyncio.Lock:
        return self._operation_locks.setdefault(task_id, asyncio.Lock())

    def _forget_active_task(self, task_id: str, task: asyncio.Task) -> None:
        if self._active_tasks.get(task_id) is task:
            self._active_tasks.pop(task_id, None)

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
                await self._fail_task_preserving_attempt(task_id, user_msg)
                logger.info(
                    "download.failed",
                    extra={"task_id": task_id, "error_message": user_msg},
                )
                await self._bus.publish(
                    f"download:{task_id}",
                    "complete",
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
            if not self._source_enabled("soulseek") and not self._source_enabled(
                "usenet"
            ):
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
            await self._fail_task_preserving_attempt(task_id, user_msg)
            logger.info(
                "download.failed", extra={"task_id": task_id, "error_message": user_msg}
            )
            await self._bus.publish(
                f"download:{task_id}",
                "complete",
                {"status": DownloadStatus.FAILED, "error": user_msg},
            )
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)

    async def _fail_task_preserving_attempt(
        self, task_id: str, error_message: str, *, completed_at: float | None = None
    ) -> None:
        """Atomically fail a task while preserving any unresolved source bytes."""

        attempts = await self._store.list_download_attempts(task_id)
        attempt = next(
            (
                value
                for value in reversed(attempts)
                if value.state in {"acquiring", "in_use"}
            ),
            None,
        )
        fields = {
            "error_message": error_message,
            "completed_at": completed_at or time.time(),
            "queue_position_start": None,
            "queue_position_end": None,
            "remote_queued": False,
            "preferred_quality_fallback_at": None,
            "has_next_source": False,
        }
        await self._store.finalize_task_and_attempt(
            task_id,
            DownloadStatus.FAILED,
            task_fields=fields,
            attempt_id=attempt.id if attempt else None,
            disposition="preserve" if attempt else None,
        )

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
        return f"{_NO_MATCH_MSG} on {joined}"

    async def _search_and_score(
        self, task, source: str, *, snapshot=None
    ):  # noqa: ANN001, ANN201
        """Search ONE source under ``snapshot`` (resolved per task when omitted),
        returning its snapshot-ranked candidates via the source strategy."""
        if snapshot is None:
            snapshot = await self._task_quality_snapshot(task)
        timeout = 30.0 + 15.0 * min(task.retry_count, 4)
        return await self._strategies[source].search_and_score(
            task,
            timeout=timeout,
            auto=self._auto,
            manual=self._manual,
            snapshot=snapshot,
        )

    async def _task_quality_snapshot(self, task):  # noqa: ANN001
        """Decode-or-derive the snapshot for THIS task (stored governs; legacy
        rows synthesise a migration-tagged snapshot from the live policy so
        pre-backfill behaviour is preserved without ever re-snapshotting new
        work against mutable settings)."""
        raw = getattr(task, "quality_snapshot_json", None)
        if raw:
            try:
                return _msgspec.json.decode(raw, type=AcquisitionQualitySnapshot)
            except ValueError:
                logger.warning("download.snapshot_decode_failed task=%s", task.id)
        if self._get_download_policy is not None:
            return acq_quality.migration_snapshot(self._get_download_policy())
        return acq_quality.build_snapshot(
            _DefaultPolicyShim()
        )

    def _source_selection_mode(self) -> str:
        if self._get_download_policy is not None:
            return self._get_download_policy().source_selection_mode
        return "source_first"

    async def _concurrent_search_and_score(self, task):  # noqa: ANN001
        """quality_first (opt-in): search every enabled source CONCURRENTLY under
        the existing per-source timeout, pooling results per source. One failed
        or slow source never erases another's candidates."""
        enabled = [s for s in self._source_priority if self._source_enabled(s)]
        snapshot = await self._task_quality_snapshot(task)

        async def run_one(source):
            try:
                return source, await self._search_and_score(
                    task, source, snapshot=snapshot
                )
            except Exception:  # noqa: BLE001 - isolation by design
                logger.exception("download.source_search_failed source=%s", source)
                return source, []

        outcomes = await asyncio.gather(*(run_one(source) for source in enabled))
        return dict(outcomes)

    @staticmethod
    def _remember_offsets(groups):
        offsets = []
        running = 0
        for group in groups:
            offsets.append(running)
            running += len(group)
        return offsets

    def _global_preference_pick(self, pooled, offsets):
        """Earliest GLOBAL preference step among identity-automatic candidates;
        configured source order breaks ties (opt-in quality_first mode)."""
        best = None
        for group_index, candidate in enumerate(pooled):
            if candidate.tier != "auto":
                continue
            decision = getattr(candidate, "quality_decision", None)
            step = (
                decision.preference_step
                if decision is not None and decision.preference_step is not None
                else 10_000
            )
            key = (
                step,
                -acq_quality.CERTAINTY_RANK[
                    candidate.quality_evidence.certainty
                    if candidate.quality_evidence is not None
                    else __import__("models.acquisition_quality", fromlist=["EvidenceCertainty"]).EvidenceCertainty.PARTIAL
                ],
                next(
                    (
                        group_index - offset
                        for offset in reversed(offsets)
                        if group_index >= offset
                    ),
                    0,
                ),
            )
            if best is None or key < best[0]:
                best = (key, group_index, candidate)
        if best is None:
            return None
        return best[1], best[2]

    async def _finish_no_candidates(self, job_id, task):  # noqa: ANN001
        await self._store.update_search_job_status(job_id, "completed")
        if task.origin == "upgrade":
            await self._store.update_status(
                task.id,
                DownloadStatus.CANCELLED,
                error_message="No better copy found",
                cancelled_at=time.time(),
            )
            await self._bus.publish(
                f"download:{task.id}",
                "complete",
                {"status": DownloadStatus.CANCELLED, "error": "no better copy found"},
            )
            return
        await self._store.update_status(
            task.id, DownloadStatus.FAILED, error_message=self._no_match_message()
        )
        await self._bus.publish(
            f"download:{task.id}",
            "complete",
            {"status": DownloadStatus.FAILED, "error": "no match"},
        )

    async def _search_score_autopick(self, task) -> bool:  # noqa: ANN001 - DownloadTask
        """Route the automatic path across ``source_priority``. DEFAULT
        (``source_first``): walk configured order; use the first source with an
        identity-automatic candidate; candidates inside each source arrive
        ranked by the task snapshot's global preference step. Opt-in
        (``quality_first``): search all sources concurrently and take the
        earliest global preference step among automatics, source order breaking
        ties. No auto anywhere pools ONE source-grouped review job (D16)."""
        snapshot = await self._task_quality_snapshot(task)
        job = await self._store.create_search_job(
            user_id=task.user_id,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
            artist_mbid=task.artist_mbid,
            search_query=f"{task.artist_name} - {task.album_title}",
            quality_snapshot_json=json_dumps_safe(snapshot),
            quality_snapshot_hash=snapshot.snapshot_hash,
            quality_snapshot_summary=snapshot.summary,
        )

        remembered: list[list] = []
        if self._source_selection_mode() == "quality_first":
            by_source = await self._concurrent_search_and_score(task)
            for source in self._source_priority:
                if self._source_enabled(source):
                    remembered.append(by_source.get(source, []))
            pooled_flat = [c for group in remembered for c in group]
            await self._store.set_search_job_candidates(job.id, pooled_flat)
            picked = self._global_preference_pick(
                pooled_flat, self._remember_offsets(remembered)
            )
            if picked is not None:
                index, selected = picked
                await self._store.link_picked_candidate(
                    task_id=task.id,
                    search_job_id=job.id,
                    candidate_index=index,
                    source_username=selected.username,
                    source_directory=selected.parent_directory,
                    preflight_score=selected.final_score,
                    source=selected.source,
                    download_client=_CLIENT_FOR_SOURCE.get(selected.source, "slskd"),
                )
                return True
            if any(c.tier in ("auto", "manual") for c in pooled_flat):
                await self._store.set_search_job_id_and_candidate(task.id, job.id, None)
                await self._store.update_search_job_status(job.id, "completed")
                await self._bus.publish(
                    f"download:{task.id}",
                    "status",
                    {"status": DownloadStatus.AWAITING_REVIEW, "search_job_id": job.id},
                )
                return False
            await self._finish_no_candidates(job.id, task)
            return False

        for source in self._source_priority:
            if not self._source_enabled(source):
                continue
            candidates = await self._search_and_score(
                task, source, snapshot=snapshot
            )
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
            auto_match = next(
                (
                    (candidate_index, candidate)
                    for candidate_index, candidate in enumerate(candidates)
                    if candidate.tier == "auto"
                ),
                None,
            )
            if auto_match is not None:
                candidate_index, selected = auto_match
                pooled = [c for group in remembered for c in group]
                index = sum(len(group) for group in remembered[:-1]) + candidate_index
                await self._store.set_search_job_candidates(job.id, pooled)
                await self._store.link_picked_candidate(
                    task_id=task.id,
                    search_job_id=job.id,
                    candidate_index=index,
                    source_username=selected.username,
                    source_directory=selected.parent_directory,
                    preflight_score=selected.final_score,
                    source=selected.source,
                    download_client=_CLIENT_FOR_SOURCE.get(selected.source, "slskd"),
                )
                return True

        # No source auto-accepted: pool all candidates (source-grouped, D16).
        pooled = [c for group in remembered for c in group]
        await self._store.set_search_job_candidates(job.id, pooled)
        if any(c.tier in ("auto", "manual") for c in pooled):
            await self._store.set_search_job_id_and_candidate(task.id, job.id, None)
            await self._store.update_search_job_status(job.id, "completed")
            await self._bus.publish(
                f"download:{task.id}",
                "status",
                {"status": DownloadStatus.AWAITING_REVIEW, "search_job_id": job.id},
            )
            return False
        await self._finish_no_candidates(job.id, task)
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
        self,
        task,
        *,
        strict_track_duration: bool = True,
        hold_on_wrong_track: bool = False,
        remaining_positions: "frozenset[tuple[int, int]] | None" = None,
    ) -> None:
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if task.candidate_index is None or task.candidate_index >= len(candidates):
            raise OrchestrationError("candidate no longer available")
        candidate = candidates[task.candidate_index]
        await self._strategies[task.source].enqueue(
            task,
            candidate,
            strict_track_duration=strict_track_duration,
            hold_on_wrong_track=hold_on_wrong_track,
            remaining_positions=remaining_positions,
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
        preferred_deadline = task.preferred_quality_fallback_at
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
                remote_queued = (
                    status.status == "queued"
                    and not status.has_active_transfer
                    and status.matched_transfers > 0
                    and status.bytes_downloaded == 0
                )
                if status.bytes_downloaded > 0:
                    preferred_deadline = None
                await self._store.update_progress(
                    task.id,
                    bytes_downloaded=status.bytes_downloaded,
                    files_completed=status.files_completed,
                    progress_percent=int(status.progress_percent),
                    queue_position_start=status.queue_position_start,
                    queue_position_end=status.queue_position_end,
                    remote_queued=remote_queued,
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
                    f"download:{task.id}",
                    "progress",
                    {
                        **self._source_event_fields(task),
                        "bytes_downloaded": status.bytes_downloaded,
                        "bytes_total": status.bytes_total,
                        "files_completed": status.files_completed,
                        "files_total": status.files_total,
                        "progress_percent": status.progress_percent,
                        "queue_position_start": status.queue_position_start,
                        "queue_position_end": status.queue_position_end,
                        "remote_queued": remote_queued,
                        "preferred_quality_fallback_at": preferred_deadline,
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
                    preferred_deadline is not None
                    and remote_queued
                    and time.time() >= preferred_deadline
                ):
                    return _OUT_PREFERRED_QUALITY, status
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

    async def _abort_abandoned_transfer(self, task) -> None:  # noqa: ANN001
        """Remove an interrupted Soulseek attempt before another peer is linked.

        Completed files have already been harvested by the caller. The durable cleanup
        journal remains responsible for exact local source-file cleanup; this explicit
        abort prevents two peers from continuing to send the same acquisition at once.
        """

        if task.source != "soulseek":
            return
        manifest = self._read_manifest(task.id)
        try:
            aborted = await self._download_client_for(task).abort(manifest.handle)
        except Exception as exc:  # noqa: BLE001 - repository errors stay internal
            raise OrchestrationError("could not switch sources safely") from exc
        if not aborted:
            raise OrchestrationError("could not switch sources safely")

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

        task = await self._prepare_candidate_state(task)
        tried_usernames: set[str] = set()
        first = True
        imported_any = False
        wrong_track = False
        source_missing = False
        import_failed = False
        tag_mismatch = False
        # Per-file failover (#292): (disc, track) positions still missing after the
        # last attempt; consumed by the next iteration's enqueue so the following
        # candidate is asked for ONLY the missing tracks instead of the whole album.
        pending_positions: frozenset[tuple[int, int]] | None = None
        while True:
            attempt_result = ProcessResult(
                succeeded=[], failed=[], workspace_disposition="discard"
            )
            # resume's first iteration polls the transfers a restart left behind (no
            # enqueue), so the no-transfer fast-fail must not apply there - those
            # records may legitimately be gone (completed + cleaned).
            did_enqueue = not (first and resume)
            enqueued = True
            if did_enqueue:
                try:
                    await self._enqueue(task, remaining_positions=pending_positions)
                except OrchestrationError:
                    logger.warning(
                        "Enqueue failed for task %s candidate %s",
                        task.id,
                        task.candidate_index,
                    )
                    enqueued = False
            # Consumed: the NEXT shortfall recomputes positions from what landed.
            pending_positions = None
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
                if outcome == _OUT_PREFERRED_QUALITY:
                    await self._mark_candidate_tried(task, tried_usernames)
                    attempts = len(await self._store.list_download_attempts(task.id))
                    details = self._candidate_quality_details(
                        (
                            await self._store.get_search_job_candidates(
                                task.search_job_id
                            )
                        )[task.candidate_index]
                    )
                    entry = (
                        await self._next_candidate_entry(
                            task,
                            tried_usernames,
                            lower_than=details["rank"] if details else None,
                        )
                        if attempts < self._max_failover
                        else None
                    )
                    if entry is None:
                        # The policy/candidate set changed after the deadline was
                        # persisted. Keep this real transfer and fall back to the
                        # absolute queue timeout instead of re-enqueueing it.
                        await self._store.update_status(
                            task.id,
                            DownloadStatus.DOWNLOADING,
                            preferred_quality_fallback_at=None,
                            has_next_source=False,
                        )
                        task = await self._store.get_task(task.id)
                        first = True
                        resume = True
                        continue
                    latest = await self._download_client_for(task).get_status(
                        self._read_manifest(task.id).handle
                    )
                    if latest.bytes_downloaded > 0 or latest.status in {
                        "completed",
                        "partial",
                        "failed",
                    }:
                        await self._store.update_progress(
                            task.id,
                            bytes_downloaded=latest.bytes_downloaded,
                            files_completed=latest.files_completed,
                            progress_percent=int(latest.progress_percent),
                            queue_position_start=latest.queue_position_start,
                            queue_position_end=latest.queue_position_end,
                            remote_queued=False,
                        )
                        task = await self._store.get_task(task.id)
                        first = True
                        resume = True
                        continue
                    await self._abort_abandoned_transfer(task)
                    await self._schedule_attempt_cleanup(task, disposition="discard")
                    task = await self._link_candidate_entry(task, entry)
                    await self._bus.publish(
                        f"download:{task.id}",
                        "status",
                        {
                            "status": DownloadStatus.RETRYING,
                            **self._source_event_fields(task),
                        },
                    )
                    continue
                await self._store.update_status(task.id, DownloadStatus.PROCESSING)
                await self._bus.publish(
                    f"download:{task.id}",
                    "status",
                    {"status": DownloadStatus.PROCESSING},
                )
                # Import ONLY the transfers whose LATEST per-file attempt succeeded
                # (the aggregation dedups slskd's one-record-per-retry history, so a
                # stale Succeeded can't shadow a final TimedOut - #253/PR #222). Files
                # that never arrived are not processed: they can't be logged as
                # mount misconfigurations or quarantined as verify failures, which
                # wrongly blamed the mount and blacklisted slow-but-good peers (#131).
                # On _OUT_COMPLETED every enqueued file succeeded, so the full manifest
                # is exactly the succeeded set (kept None for the grace-period path).
                if outcome == _OUT_COMPLETED or status is None:
                    only = None
                else:
                    only = set(status.succeeded_filenames)
                result, enumerated = await self._import_files(
                    task, only_filenames=only, completed=outcome == _OUT_COMPLETED
                )
                if result.succeeded:
                    imported_any = True
                attempt_result = result
                # Per-attempt fault flags (NOT the accumulated ones below) decide whether
                # THIS candidate's shortfall is the release's fault or a local one.
                attempt_mount = not result.succeeded and any(
                    f.reason == DOWNLOADS_MOUNT_UNAVAILABLE for f in result.failed
                )
                attempt_import_fault = any(
                    f.reason in (IMPORT_FAILED, SOURCE_FILE_MISSING)
                    for f in result.failed
                )
                if any(f.reason == WRONG_TRACK for f in result.failed):
                    wrong_track = True
                if any(f.reason == SOURCE_FILE_MISSING for f in result.failed):
                    source_missing = True
                if any(f.reason == IMPORT_FAILED for f in result.failed):
                    import_failed = True
                if any(f.reason == "tag_mismatch" for f in result.failed):
                    tag_mismatch = True
                if result.management_hold_reason_code is not None:
                    # The peer delivered a verified acquisition unit and the app now
                    # owns durable held copies. A different peer cannot fix a local
                    # profile/provider/path hold, so stop here without blocklisting or
                    # fetching another copy.
                    await self._finalize(
                        task,
                        DownloadStatus.FAILED,
                        error_message=(
                            _MANAGEMENT_HELD_MSG
                            if result.management_hold_secured
                            else _MANAGEMENT_HOLD_STORAGE_MSG
                        ),
                        process_result=result,
                    )
                    return
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
                is_complete = await self._download_is_complete(
                    task, imported_any, result
                )
                # A release that genuinely finished (e.g. SABnzbd Completed/Failed) but did NOT
                # deliver what was requested is blocklisted by source identity BEFORE failover so
                # a re-search/retry finds a COMPLETE release instead of re-grabbing this one
                # (Lidarr's "Redownload Failed" + blocklist). Skipped for an interrupted poll, a
                # local/environment fault, or a local IMPORT fault (the files arrived but we
                # failed to write them - not the release's fault, review H3). The strategy owns
                # the source-specific blocklist (Usenet: age-guarded title+size).
                if (
                    not is_complete
                    and outcome in (_OUT_COMPLETED, _OUT_TERMINAL)
                    and not local_fault
                    and not attempt_import_fault
                ):
                    await strategy.maybe_blocklist_on_failure(
                        task,
                        status,
                        completed=outcome == _OUT_COMPLETED,
                        enumerated_any=enumerated > 0,
                    )
                    # Peer-folder exhaustion (#255 defect 2): a CLEAN import (zero file
                    # failures of any kind) that still under-delivers means the shared
                    # folder simply LACKS tracks - per-file quarantine never fired, so
                    # without a peer-level row the scorer re-picks this folder by score
                    # forever, re-downloading it each cycle. Block the PEER identity for
                    # this release-group; a manual re-request/retry still clears
                    # album-scoped rows (retry_task / request path semantics).
                    if (
                        task.source == "soulseek"
                        and not attempt_result.failed
                        and task.search_job_id is not None
                        and task.candidate_index is not None
                    ):
                        pool = await self._store.get_search_job_candidates(
                            task.search_job_id
                        )
                        if 0 <= task.candidate_index < len(pool):
                            await self._store.record_quarantine(
                                source="soulseek",
                                identity=self._candidate_source_identity(
                                    pool[task.candidate_index]
                                ),
                                reason="verify_failed",
                                release_group_mbid=task.release_group_mbid,
                            )
                            logger.info(
                                "download.quarantined",
                                extra={
                                    "task_id": task.id,
                                    "source": "soulseek",
                                    "reason": "folder_under_delivered",
                                    "identity": self._candidate_source_identity(
                                        pool[task.candidate_index]
                                    ),
                                },
                            )

                if local_fault:
                    preserved_result = ProcessResult(
                        succeeded=list(result.succeeded),
                        failed=list(result.failed),
                        publisher_bundle_ids=list(result.publisher_bundle_ids),
                        workspace_disposition="preserve",
                    )
                    await self._finalize(
                        task,
                        DownloadStatus.FAILED,
                        error_message=strategy.local_fault_message(attempt_mount),
                        process_result=preserved_result,
                    )
                    return

                if is_complete:
                    await self._finalize(
                        task, DownloadStatus.COMPLETED, process_result=result
                    )
                    return

            # Incomplete (or this candidate's enqueue failed): fail over. Track the
            # tried release by its SOURCE identity (slskd peer username; Usenet title+
            # size) so failover dedups correctly within the source (review M2).
            await self._mark_candidate_tried(task, tried_usernames)
            attempts = len(await self._store.list_download_attempts(task.id))
            entry = (
                await self._next_candidate_entry(task, tried_usernames)
                if attempts < self._max_failover
                else None
            )
            # Per-file failover (#292): measure what the library is still missing.
            # An EMPTY remaining set means earlier attempts (or a manual import) already
            # delivered everything - settle COMPLETED instead of re-downloading the
            # album; None means unmeasurable, so the next attempt keeps whole-album
            # semantics. Usenet keeps whole-album failover regardless: an NZB is the
            # smallest addressable unit.
            remaining = (
                await self._remaining_track_positions(task)
                if task.download_type == "album"
                else None
            )
            if remaining is not None and not remaining:
                logger.info(
                    "download.cumulative_coverage_complete",
                    extra={"task_id": task.id},
                )
                await self._finalize(task, DownloadStatus.COMPLETED)
                return
            pending_positions = remaining
            if entry is None:
                # Every source for a single track failed the canonical-duration gate:
                # the MB length is probably wrong (not the files), so re-pull the best
                # source with the gate off rather than strand the user.
                if task.download_type == "track" and wrong_track and not imported_any:
                    await self._schedule_attempt_cleanup(
                        task,
                        disposition=attempt_result.workspace_disposition,
                        publisher_bundle_ids=attempt_result.publisher_bundle_ids,
                    )
                    await self._fallback_track_repull(task)
                    return
                await self._settle_incomplete(
                    task,
                    imported_any,
                    source_missing=source_missing,
                    import_failed=import_failed,
                    tag_mismatch=tag_mismatch,
                    process_result=attempt_result,
                )
                return
            if enqueued and outcome not in (_OUT_COMPLETED, _OUT_TERMINAL):
                await self._abort_abandoned_transfer(task)
            await self._schedule_attempt_cleanup(
                task,
                disposition=attempt_result.workspace_disposition,
                publisher_bundle_ids=attempt_result.publisher_bundle_ids,
            )
            task = await self._link_candidate_entry(task, entry)
            await self._bus.publish(
                f"download:{task.id}",
                "status",
                {
                    "status": DownloadStatus.RETRYING,
                    **self._source_event_fields(task),
                },
            )

    async def _remaining_track_positions(
        self, task
    ) -> "frozenset[tuple[int, int]] | None":  # noqa: ANN001 - DownloadTask
        """``(disc, track)`` positions of the manifest's expected edition that the
        library does NOT cover yet - the per-file failover target set (#292). Judged
        by the SAME matcher as the completeness gate/album annotation (shared P4
        rules in ``coverage.py``), so per-file dispatch can never disagree with what
        the gate accepts.

        ``None`` when unmeasurable (no manifest yet, non-album task, or an empty
        expected map): callers keep whole-album semantics rather than guess. An EMPTY
        frozenset means every position is already covered."""
        if task.download_type != "album" or not task.release_group_mbid:
            return None
        try:
            manifest = self._read_manifest(task.id)
        except OrchestrationError:
            return None
        tracks = list(manifest.expected_tracks)
        if not tracks:
            return None
        try:
            rows = await self._library.get_file_rows_for_album(task.release_group_mbid)
        except Exception:  # noqa: BLE001 - rows trouble reads as "measure nothing"
            return None
        # Adapt ExpectedTrack to the matcher's MusicBrainz Track shape (position/
        # disc_number/length-ms/recording_id/title) without a network round-trip:
        # the manifest already IS the requested edition's tracklist.
        proxies = [
            SimpleNamespace(
                position=value.track_number,
                disc_number=value.disc_number,
                length=(value.duration_seconds * 1000.0)
                if value.duration_seconds
                else None,
                recording_id=value.recording_mbid,
                title=value.title,
            )
            for value in tracks
        ]
        uncovered = uncovered_tracks(rows, proxies)
        return frozenset(
            (value.disc_number or 1, value.position) for value in uncovered
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
        # Re-gate before re-pulling: don't fetch a candidate the live policy now rejects.
        if not await self._candidate_passes_quality(task, cand):
            await self._settle_incomplete(task, False)
            return
        await self._store.link_picked_candidate(
            task.id,
            task.search_job_id,
            0,
            cand.username,
            cand.parent_directory,
            cand.final_score,
            source=cand.source,
            download_client=_CLIENT_FOR_SOURCE.get(cand.source, "slskd"),
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
        only = (
            None
            if outcome in (_OUT_COMPLETED, _OUT_TERMINAL)
            else set(status.succeeded_filenames)
        )
        result, _enumerated = await self._import_files(
            task, only_filenames=only, completed=outcome == _OUT_COMPLETED
        )
        await self._finalize(
            task,
            DownloadStatus.COMPLETED if result.succeeded else DownloadStatus.FAILED,
            error_message=(
                (
                    _MANAGEMENT_HELD_MSG
                    if result.management_hold_secured
                    else _MANAGEMENT_HOLD_STORAGE_MSG
                )
                if result.management_hold_reason_code is not None
                else None
            ),
            process_result=result,
        )

    async def _import_files(
        self, task, manifest_override=None, *, only_filenames=None, completed=False
    ):  # noqa: ANN001, ANN201
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
        manifest = (
            manifest_override
            if manifest_override is not None
            else self._read_manifest(task.id)
        )
        return await self._strategies[task.source].import_files(
            task, manifest, only_filenames=only_filenames, completed=completed
        )

    async def _schedule_attempt_cleanup(
        self,
        task,
        manifest_override=None,
        *,
        disposition: str,
        publisher_bundle_ids: list[str] | None = None,
    ) -> str | None:  # noqa: ANN001 - DownloadTask
        manifest = (
            manifest_override
            if manifest_override is not None
            else self._read_manifest(task.id)
        )
        attempt = await self._attempt_for_manifest(task, manifest)
        if attempt is None:
            return None
        scheduled = await self._store.schedule_download_attempt_cleanup(
            attempt.id,
            disposition=disposition,
            publisher_bundle_ids=publisher_bundle_ids or [],
        )
        if disposition == "discard" and self._cleanup is not None:
            try:
                await self._cleanup.cleanup_now(
                    scheduled.id, worker_id=f"download-{task.id}"
                )
            except Exception:  # noqa: BLE001 - worker retries persisted cleanup debt
                logger.warning("Immediate cleanup failed for attempt %s", scheduled.id)
        return scheduled.id

    async def _attempt_for_manifest(self, task, manifest):  # noqa: ANN001, ANN201
        attempt = None
        if manifest.attempt_id:
            attempt = await self._store.get_download_attempt(manifest.attempt_id)
        if attempt is None and manifest.handle and manifest.handle.job_name:
            attempt = await self._store.get_download_attempt_for_job(
                task.source, manifest.handle.job_name
            )
        if attempt is None:
            candidates = await self._store.list_download_attempts(task.id)
            attempt = next(
                (
                    value
                    for value in reversed(candidates)
                    if value.state in {"acquiring", "in_use"}
                ),
                None,
            )
        if attempt is None and manifest.handle is not None:
            attempt = await self._store.create_download_attempt(
                task_id=task.id,
                source=task.source,
                candidate_index=task.candidate_index or 0,
                job_name=manifest.handle.job_name,
                handle=manifest.handle,
            )
            attempt = await self._store.update_download_attempt_handle(
                attempt.id, manifest.handle
            )
            manifest.attempt_id = attempt.id
        return attempt

    def _candidate_quality_details(self, candidate):  # noqa: ANN001, ANN201
        """Return the selected Soulseek candidate's durable quality/queue signals.

        The quality pool includes the configured tier before resolution, matching the
        scorer's ordering. Usenet has no trustworthy per-file resolution metadata and
        therefore keeps its existing timeout behavior.
        """

        if candidate.source != "soulseek":
            return None
        audio = [file for file in candidate.files if is_audio(file)]
        if not audio:
            return None
        rank_bit_depth, rank_sample_rate = folder_hires_key(audio)
        tier = candidate_tier(audio)
        formats = sorted({effective_extension(file) for file in audio})
        queue_depths = [
            file.queue_length for file in audio if file.queue_length is not None
        ]
        decision = getattr(candidate, "quality_decision", None)
        step = decision.preference_step if decision is not None else None
        if step is None:
            # Legacy blob: derive from canonical tier via fidelity rank so the
            # deadline ordering degrades gracefully pre-backfill.
            legacy_step = {k: i for i, k in enumerate(
                ("low", "mp3_192", "mp3_256", "mp3_320", "lossless")
            )}.get(tier)
            step = 10_000 - (legacy_step or 0)
        return {
            "format": "/".join(formats) or None,
            "bit_depth": (
                rank_bit_depth if all(file.bit_depth for file in audio) else None
            ),
            "sample_rate": (
                rank_sample_rate if all(file.sample_rate for file in audio) else None
            ),
            "queue_depth": max(queue_depths) if queue_depths else None,
            # STABLE policy-step key (Acquisition plan): a same-step replacement
            # never resets the zero-byte clock; only a strictly less-preferred
            # STEP starts/countinues it.
            "pool_key": f"step:{step}",
            "step": step,
            "rank": (step, tier_rank(tier), rank_bit_depth, rank_sample_rate),
        }

    @staticmethod
    def _source_event_fields(task):  # noqa: ANN001, ANN205
        """Selected-source details shared by progress and source-change events."""

        return {
            "candidate_index": task.candidate_index,
            "source": task.source,
            "quality_format": task.quality_format,
            "quality_bit_depth": task.quality_bit_depth,
            "quality_sample_rate": task.quality_sample_rate,
            "advertised_queue_depth": task.advertised_queue_depth,
            "queue_position_start": task.queue_position_start,
            "queue_position_end": task.queue_position_end,
            "remote_queued": task.remote_queued,
            "preferred_quality_fallback_at": task.preferred_quality_fallback_at,
            "attempt": task.attempt_number,
            "attempt_number": task.attempt_number,
            "attempt_total": task.attempt_total,
            "has_next_source": task.has_next_source,
        }

    async def _next_candidate_entry(
        self,
        task,
        tried_usernames,
        *,
        lower_than=None,
    ):  # noqa: ANN001, ANN201
        """The next eligible stored candidate without mutating durable task state."""

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
            # re-gate: failover must not fall through to a now out-of-policy candidate (D2)
            if not await self._candidate_passes_quality(task, cand):
                continue
            if lower_than is not None:
                details = self._candidate_quality_details(cand)
                if details is None or details["rank"] >= lower_than:
                    continue
            return idx, cand
        return None

    async def _prepare_candidate_state(
        self, task, *, reset_transfer_state: bool = False
    ):  # noqa: ANN001, ANN201
        """Persist presentation state and the shared quality-pool deadline."""

        if task.search_job_id is None or task.candidate_index is None:
            return task
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if not (0 <= task.candidate_index < len(candidates)):
            return task
        candidate = candidates[task.candidate_index]
        details = self._candidate_quality_details(candidate)
        attempts = await self._store.list_download_attempts(task.id)
        current_attempt_exists = any(
            attempt.source == task.source
            and attempt.candidate_index == task.candidate_index
            for attempt in attempts
        )
        attempt_number = max(1, len(attempts) + (0 if current_attempt_exists else 1))
        tried = {
            self._candidate_source_identity(candidates[attempt.candidate_index])
            for attempt in attempts
            if attempt.source == task.source
            and 0 <= attempt.candidate_index < len(candidates)
        }
        next_entry = (
            await self._next_candidate_entry(task, tried)
            if attempt_number < self._max_failover
            else None
        )
        remaining = 0
        scan_task = task
        scan_tried = set(tried)
        while attempt_number + remaining < self._max_failover:
            entry = await self._next_candidate_entry(scan_task, scan_tried)
            if entry is None:
                break
            idx, cand = entry
            remaining += 1
            scan_tried.add(self._candidate_source_identity(cand))
            scan_task = msgspec.structs.replace(scan_task, candidate_index=idx)

        fallback_at = task.preferred_quality_fallback_at
        pool_key = details["pool_key"] if details else None
        if details is None or task.downloaded_bytes > 0:
            fallback_at = None
        elif task.quality_pool_key != pool_key:
            lower = (
                await self._next_candidate_entry(
                    task, tried, lower_than=details["rank"]
                )
                if attempt_number < self._max_failover
                else None
            )
            fallback_at = (
                time.time() + self._preferred_quality_wait
                if lower is not None
                else None
            )

        fields = {
            "quality_format": details["format"] if details else None,
            "quality_bit_depth": details["bit_depth"] if details else None,
            "quality_sample_rate": details["sample_rate"] if details else None,
            "advertised_queue_depth": details["queue_depth"] if details else None,
            "preferred_quality_fallback_at": fallback_at,
            "quality_pool_key": pool_key,
            "attempt_number": attempt_number,
            "attempt_total": min(
                self._max_failover, max(attempt_number, attempt_number + remaining)
            ),
            "has_next_source": next_entry is not None,
        }
        status = task.status
        if reset_transfer_state:
            status = DownloadStatus.QUEUED
            fields.update(
                {
                    "progress_percent": 0,
                    "total_size_bytes": None,
                    "downloaded_bytes": 0,
                    "files_total": 0,
                    "files_completed": 0,
                    "files_failed": 0,
                    "queue_position_start": None,
                    "queue_position_end": None,
                    "remote_queued": False,
                    "error_message": None,
                    "started_at": None,
                }
            )
        await self._store.update_status(task.id, status, **fields)
        return await self._store.get_task(task.id)

    async def _link_candidate_entry(self, task, entry):  # noqa: ANN001, ANN201
        idx, candidate = entry
        await self._store.link_picked_candidate(
            task.id,
            task.search_job_id,
            idx,
            candidate.username,
            candidate.parent_directory,
            candidate.final_score,
            source=candidate.source,
            download_client=_CLIENT_FOR_SOURCE.get(candidate.source, "slskd"),
        )
        refreshed = await self._store.get_task(task.id)
        return await self._prepare_candidate_state(refreshed, reset_transfer_state=True)

    async def _advance_candidate(self, task, tried_usernames):  # noqa: ANN001, ANN201
        """Compatibility helper used by focused tests and the track fallback path."""

        entry = await self._next_candidate_entry(task, tried_usernames)
        if entry is None:
            return None
        return await self._link_candidate_entry(task, entry)

    def _candidate_source_identity(self, cand) -> str:  # noqa: ANN001 - ScoredCandidate
        return self._strategy(cand.source).candidate_identity(cand)

    def _stored_snapshot(self, task):  # noqa: ANN001
        """The task's persisted creation-time snapshot; legacy rows (pre-backfill
        blobs without JSON) fall back to a migration-tagged derivation from the
        LIVE getter so they still re-gate against today's range instead of none."""
        raw = getattr(task, "quality_snapshot_json", None)
        if raw:
            try:
                return _msgspec.json.decode(
                    raw, type=AcquisitionQualitySnapshot
                )
            except ValueError:
                logger.warning("download.snapshot_decode_failed task=%s", task.id)
        if self._get_download_policy is not None:
            return acq_quality.migration_snapshot(self._get_download_policy())
        return None

    async def _candidate_passes_quality(self, task, cand) -> bool:  # noqa: ANN001
        """Re-check a STORED candidate under the TASK'S STORED snapshot before an
        AUTOMATIC re-dispatch (failover / track-repull). Deliberate resolution of
        live-vs-stored (Acquisition plan): what governed the search governs the
        re-pull; ``restart-with-current-policy`` is the explicit refresh. Explicit
        user picks (``pick_candidate``) and ``reimport_task`` are intentionally NOT
        gated (owner decision D2). Candidates scored after the cutover carry their
        quality_decision; legacy blobs are re-evaluated from source fields under
        the SAME snapshot. True when unsnapshotted and unwired, or undeterminable,
        preserving fail-open behaviour."""
        snapshot = self._stored_snapshot(task)
        if snapshot is None:
            return True
        # Codec gate mirrors the score-time filter for Soulseek folders.
        if (
            getattr(snapshot, "flac_mp3_only", False)
            and cand.source != "usenet"
        ):
            from services.native.quality_tiers import is_audio as _is_audio
            from services.native.quality_tiers import is_flac_or_mp3 as _is_flac

            audio_files = [f for f in cand.files if _is_audio(f)]
            if audio_files and not all(_is_flac(f) for f in audio_files):
                return False
        if cand.quality_evidence is not None:
            # Post-cutover candidate: its evaluation is embedded in the blob.
            decision = acq_quality.evaluate(snapshot, cand.quality_evidence)
            return bool(decision.eligible)
        # Legacy blob projection.
        if cand.source == "usenet":
            if cand.usenet_release is None or self._usenet_scorer is None:
                return True  # can't judge -> don't block
            from services.native.newznab_release_scorer import _release_evidence

            tier = self._usenet_scorer.release_tier(cand.usenet_release, task.track_count)
            evidence = _release_evidence(cand.usenet_release, tier)
        else:
            from services.native.album_preflight_scorer import _file_evidence

            audio = [f for f in cand.files if is_audio(f)]
            if not audio:
                return True  # no judgeable audio -> don't block
            decision_files = [_file_evidence(f) for f in audio]
            merged = acq_quality.evaluate_worst(snapshot, decision_files)
            evidence = merged.evidence
        decision = acq_quality.evaluate(snapshot, evidence)
        return bool(decision.eligible)

    def _candidate_preference_step(self, cand, track_count=None):  # noqa: ANN001
        """Stable step index for THIS candidate under its own evaluation -
        the zero-byte fallback deadline keys to this (not (tier,depth,rate))."""
        if cand.quality_decision is not None:
            return cand.quality_decision.preference_step
        return None

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

    async def _coverage(
        self, task, *, context: str
    ) -> "tuple[int, int, list[str]] | None":  # noqa: ANN001
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
                extra={
                    "task_id": task.id,
                    "release_group_mbid": task.release_group_mbid,
                },
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

    async def _download_is_complete(
        self, task, imported_any: bool, result=None
    ) -> bool:  # noqa: ANN001
        """Whether the download has delivered what it set out to. A per-track
        download is one file - complete the moment it imports (Soulseek rips rarely
        carry the recording MBID, so a tag-based check can't be trusted).

        An album is complete once the library COVERS the requested release's tracklist
        (P4: recording/position+duration/title matching), cumulative across failover
        attempts. ONE delivery-trust exception guards against tracker drift (#131
        family, 2026-08): a CLEAN full delivery of the attempt's whole manifest while
        the requested release-group has ZERO library rows means the rows were stamped
        under a different RG (import-time identity drift) - holding that against the
        download loops failover + whole-album re-downloads forever. Rows PRESENT but
        short or mismatched keep the full P4 veto: a wrong file at a covered position
        must never satisfy a request (the 2026-07-05 wrong-single incident), and an
        edition tracklist with bonus tracks keeps failing over for a fuller source.

        When the tracklist itself is unavailable the pre-P4 count check is the
        fallback: at least ``track_count`` distinct positions present."""
        if task.download_type == "track":
            return imported_any
        coverage = await self._coverage(task, context="completeness")
        if coverage is not None:
            covered, expected_total, _orphans = coverage
            if covered >= expected_total:
                return True
            if (
                covered == 0
                and result is not None
                and result.succeeded
                and not result.failed
                and result.management_hold_reason_code is None
            ):
                try:
                    manifest = self._read_manifest(task.id)
                    # A Usenet grab is one opaque NZB, so its manifest carries no
                    # per-file entries (acquisition/strategy.py builds it with
                    # ``target_files=[]``) and the requested tracklist is what this
                    # source was asked for. Reading ``target_files`` alone makes
                    # ``asked`` 0 for every Usenet album, which then always trips the
                    # ``asked < task.track_count`` under-delivery test below and
                    # defeats this exception on that path entirely.
                    asked = len(manifest.target_files) or len(manifest.expected_tracks)
                except OrchestrationError:
                    asked = None
                rows = []
                if asked is not None:
                    try:
                        rows = await self._library.get_file_rows_for_album(
                            task.release_group_mbid
                        )
                    except Exception:  # noqa: BLE001 - rows trouble reads as "some"
                        rows = ["unknown"]
                # A candidate folder smaller than the requested tracklist UNDER-
                # DELIVERED even when it published cleanly: 'everything asked of THIS
                # source' is not 'everything requested' (whole-album-repull guard).
                if (
                    asked is not None
                    and not rows
                    and len(result.succeeded) >= asked
                    and not (task.track_count and asked < task.track_count)
                ):
                    logger.info(
                        "download.delivery_complete",
                        extra={
                            "task_id": task.id,
                            "delivered": len(result.succeeded),
                            "manifest_files": asked,
                            "reason": "no_rows_for_release_group",
                        },
                    )
                    return True
            return False
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
        self,
        task,
        imported_any: bool,
        *,
        source_missing: bool = False,
        import_failed: bool = False,
        tag_mismatch: bool = False,
        process_result=None,
    ) -> None:
        """No candidates/attempts left and the download still isn't whole. A track
        either imported (already finalized 'completed') or it didn't ('failed'); an
        album keeps whatever landed as 'partial', or 'failed' if nothing did.

        ``source_missing``/``import_failed``/``tag_mismatch`` flip the failure message
        off the default 'no source on Soulseek': slskd delivered the files but we either
        couldn't find them on the mount (config), couldn't write them into the library
        (perms/disk), or found files whose embedded tags identify different music.
        Local faults take precedence over a content mismatch, and both take precedence
        over the generic no-source message."""
        if source_missing:
            fail_msg = _FILES_NOT_FOUND_MSG
        elif import_failed:
            fail_msg = _IMPORT_FAILED_MSG
        elif tag_mismatch:
            fail_msg = _TAG_MISMATCH_MSG
        else:
            fail_msg = self._no_source_message()
        if task.download_type == "track":
            await self._finalize(
                task,
                DownloadStatus.FAILED,
                error_message=fail_msg,
                process_result=process_result,
            )
            return
        if await self._imported_track_count(task) > 0:
            await self._finalize(
                task, DownloadStatus.PARTIAL, process_result=process_result
            )
        else:
            await self._finalize(
                task,
                DownloadStatus.FAILED,
                error_message=fail_msg,
                process_result=process_result,
            )

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
        if task is None or task.status in (
            DownloadStatus.COMPLETED,
            DownloadStatus.CANCELLED,
        ):
            return
        expected = self._expected_track_count(task)
        present = (
            1
            if task.download_type == "track"
            else await self._imported_track_count(task)
        )
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
            await self._store.update_status(
                task.id, task.status, files_completed=present
            )

    async def _finalize(
        self,
        task,
        status,
        *,
        error_message=None,
        process_result=None,
        manifest_override=None,
    ) -> None:  # noqa: ANN001
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
            "queue_position_start": None,
            "queue_position_end": None,
            "remote_queued": False,
            "preferred_quality_fallback_at": None,
            "has_next_source": False,
            "error_message": error_message,
        }
        attempt_id = None
        disposition = None
        bundle_ids: list[str] = []
        if process_result is not None:
            try:
                manifest = (
                    manifest_override
                    if manifest_override is not None
                    else self._read_manifest(task.id)
                )
            except OrchestrationError:
                manifest = None
            if manifest is not None:
                attempt = await self._attempt_for_manifest(task, manifest)
            else:
                attempt = await self._store.get_download_attempt_for_candidate(
                    task.id, task.source, task.candidate_index or 0
                )
            if attempt is not None:
                attempt_id = attempt.id
                disposition = process_result.workspace_disposition
                bundle_ids = list(process_result.publisher_bundle_ids)
        await self._store.finalize_task_and_attempt(
            task.id,
            status,
            task_fields=fields,
            attempt_id=attempt_id,
            disposition=disposition,
            publisher_bundle_ids=bundle_ids,
        )
        if (
            attempt_id is not None
            and disposition == "discard"
            and self._cleanup is not None
        ):
            try:
                await self._cleanup.cleanup_now(
                    attempt_id, worker_id=f"download-{task.id}"
                )
            except Exception:  # noqa: BLE001 - worker retries persisted cleanup debt
                logger.warning("Immediate cleanup failed for attempt %s", attempt_id)
        await asyncio.to_thread(
            shutil.rmtree, self._staging / task.id, ignore_errors=True
        )
        # keep the established log-event contract: completed/partial -> download.completed,
        # failed -> download.failed (consumed by log monitoring + tests)
        event = (
            "download.failed"
            if status == DownloadStatus.FAILED
            else "download.completed"
        )
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
        """Bridge a terminal download status into its exact request generation."""
        mapping = {
            DownloadStatus.COMPLETED: "imported",
            DownloadStatus.PARTIAL: "incomplete",
            DownloadStatus.FAILED: "failed",
            DownloadStatus.CANCELLED: "cancelled",
        }
        new_status = mapping.get(status)
        if new_status is None:
            return
        if (
            new_status == "imported"
            and task.download_type == "album"
            and task.release_group_mbid
            and self._wanted_store is not None
        ):
            try:
                await self._wanted_store.mark_fulfilled(
                    task.release_group_mbid, "imported"
                )
            except Exception:  # noqa: BLE001 - watch settlement is best-effort
                logger.warning(
                    "Could not fulfil wanted watch for %s", task.release_group_mbid
                )
        if self._request_history is None:
            return
        try:
            # The task ID is the only identifier shared by album and exact-track
            # requests. Looking up by MBID would silently default tracks to album.
            record = await self._request_history.async_get_record_by_download_task_id(
                task.id
            )
        except Exception:  # noqa: BLE001 - request sync must never fail the download
            logger.warning("Could not load request for task %s", task.id)
            return
        if record is None or getattr(record, "download_task_id", None) != task.id:
            return
        from datetime import datetime, timezone

        request_kind = getattr(record, "request_kind", "album")
        completed_at = (
            datetime.now(timezone.utc).isoformat()
            if new_status in ("imported", "failed", "cancelled")
            else None
        )
        kwargs: dict[str, object] = {
            "completed_at": completed_at,
            "request_kind": request_kind,
        }
        generation = _generation_of(record)
        if generation is not None:
            kwargs["expected_generation"] = generation
        try:
            changed = await self._request_history.async_update_status(
                record.musicbrainz_id,
                new_status,
                **kwargs,
            )
            if changed is False:
                return
            # An import (full or partial) added library files - bust the
            # album/library caches and materialise the row for the UI.
            if new_status in ("imported", "incomplete") and self._on_import is not None:
                await self._on_import(record)
        except Exception:  # noqa: BLE001
            logger.warning(
                "Failed to sync request %s -> %s", record.musicbrainz_id, new_status
            )

    async def _notify_completion(self, task) -> None:  # noqa: ANN001 - DownloadTask
        final = await self._store.get_task(task.id)
        await self._bus.publish(
            f"download:{task.id}",
            "complete",
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
            active = await self._store.list_active_tasks(
                [DownloadStatus.DOWNLOADING, DownloadStatus.PROCESSING]
            )
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
            await self._fail_task_preserving_attempt(
                task.id,
                "Download interrupted - no progress after a restart",
                completed_at=now,
            )
            await self._bus.publish(
                f"download:{task.id}",
                "complete",
                {"status": DownloadStatus.FAILED, "error": "download interrupted"},
            )
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)
            logger.warning(
                "Reaped stale download task %s (no poller for %.0fs)",
                task.id,
                now - last,
            )

    async def startup_resume(self) -> None:
        """Resume in-progress tasks after a restart (AUD-3): never block startup."""
        registry = TaskRegistry.get_instance()

        for orphan in await self._store.list_active_tasks([DownloadStatus.QUEUED]):
            # queued = created but never enqueued -> re-dispatch (failing would be
            # spurious; they never started).
            self.dispatch(orphan.id)

        for task in await self._store.list_active_tasks(
            [DownloadStatus.DOWNLOADING, DownloadStatus.PROCESSING]
        ):
            handle = asyncio.create_task(self._resume_single_task(task.id))
            # Track the live handle so cancel_task can stop the resumed poll loop
            # (mirrors dispatch); without this a resumed download is uncancellable.
            self._active_tasks[task.id] = handle
            handle.add_done_callback(_log_task_exception)
            handle.add_done_callback(
                lambda done, _id=task.id: self._forget_active_task(_id, done)
            )
            registry.register(f"download-resume-{task.id}", handle)

    async def _resume_single_task(self, task_id: str) -> None:
        task = await self._store.get_task(task_id)
        if task is None:
            return
        try:
            if not (self._staging / task_id / "manifest.json").exists():
                # Never got as far as writing a manifest -> start from scratch in this
                # registered resume task so cancellation keeps the correct live handle.
                await self.process_task(task_id)
                return
            manifest = self._read_manifest(task_id)
            attempt = await self._attempt_for_manifest(task, manifest)
            if (
                attempt is not None
                and task.candidate_index is not None
                and attempt.candidate_index != task.candidate_index
            ):
                # The prior candidate was made cleanup-eligible, then the process died
                # before the next enqueue replaced its manifest.
                await self.process_task(task_id)
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
            await self._fail_task_preserving_attempt(task_id, _user_error_message(exc))
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)
        except Exception as exc:  # noqa: BLE001 - resume failure -> mark failed
            logger.exception("Failed to resume task %s", task_id)
            await self._fail_task_preserving_attempt(task_id, _user_error_message(exc))
            await self._sync_request_on_terminal(task, DownloadStatus.FAILED)

    async def try_next_source(
        self,
        task_id: str,
        user_id: str,
        user_role: str,
        expected_candidate_index: int,
    ):
        """Abort a zero-byte remotely queued Soulseek attempt and advance once.

        ``expected_candidate_index`` makes retries and double-submits idempotent: a
        stale command cannot skip the newly selected peer. The live client status is
        checked after the poll task is stopped, closing the race where bytes begin
        between the UI render and the click.
        """

        async with self._operation_lock(task_id):
            task = await self._store.get_task(task_id)
            if task is None:
                raise ResourceNotFoundError("Download task not found")
            if user_role != "admin" and task.user_id != user_id:
                raise PermissionDeniedError(
                    "Cannot change another user's download source"
                )
            if task.candidate_index != expected_candidate_index:
                raise ConflictError("The download has already moved to another source")
            if task.source != "soulseek" or task.status != DownloadStatus.DOWNLOADING:
                raise ConflictError("The download is not waiting in a Soulseek queue")

            manifest = self._read_manifest(task.id)
            initial_status = await self._download_client_for(task).get_status(
                manifest.handle
            )
            if initial_status.bytes_downloaded > 0:
                raise ConflictError("The transfer has already started")
            if (
                initial_status.status != "queued"
                or initial_status.has_active_transfer
                or initial_status.matched_transfers == 0
            ):
                raise ConflictError(
                    "The download is no longer waiting in a remote queue"
                )

            handle = self._active_tasks.get(task_id)
            if handle is not None and not handle.done():
                handle.cancel()
                with suppress(asyncio.CancelledError):
                    await handle
            else:
                registry = TaskRegistry.get_instance()
                await registry.cancel(f"download-{task_id}")
                await registry.cancel(f"download-resume-{task_id}")

            task = await self._store.get_task(task_id)
            if task is None:
                raise ResourceNotFoundError("Download task not found")
            if task.candidate_index != expected_candidate_index:
                raise ConflictError("The download has already moved to another source")
            if task.downloaded_bytes > 0:
                raise ConflictError("The transfer has already started")

            status = await self._download_client_for(task).get_status(manifest.handle)
            if status.bytes_downloaded > 0:
                await self._store.update_progress(
                    task.id,
                    bytes_downloaded=status.bytes_downloaded,
                    files_completed=status.files_completed,
                    progress_percent=int(status.progress_percent),
                    queue_position_start=status.queue_position_start,
                    queue_position_end=status.queue_position_end,
                    remote_queued=False,
                )
                self._dispatch_resume(task.id)
                raise ConflictError("The transfer has already started")
            if (
                status.status != "queued"
                or status.has_active_transfer
                or status.matched_transfers == 0
            ):
                self._dispatch_resume(task.id)
                raise ConflictError(
                    "The download is no longer waiting in a remote queue"
                )

            candidates = await self._store.get_search_job_candidates(task.search_job_id)
            attempts = await self._store.list_download_attempts(task.id)
            tried = {
                self._candidate_source_identity(candidates[attempt.candidate_index])
                for attempt in attempts
                if attempt.source == task.source
                and 0 <= attempt.candidate_index < len(candidates)
            }
            await self._mark_candidate_tried(task, tried)
            entry = (
                await self._next_candidate_entry(task, tried)
                if len(attempts) < self._max_failover
                else None
            )
            if entry is None:
                self._dispatch_resume(task.id)
                raise ConflictError("No other eligible source is available")

            aborted = False
            try:
                await self._abort_abandoned_transfer(task)
                aborted = True
                await self._schedule_attempt_cleanup(task, disposition="discard")
                advanced = await self._link_candidate_entry(task, entry)
            except Exception as error:  # noqa: BLE001 - preserve one safe terminal state
                if aborted:
                    message = (
                        "The queued source was stopped, but DroppedNeedle could not select "
                        "the next source safely. Retry the download."
                    )
                    await self._fail_task_preserving_attempt(task.id, message)
                    await self._bus.publish(
                        f"download:{task.id}",
                        "complete",
                        {"status": DownloadStatus.FAILED, "error": message},
                    )
                    await self._sync_request_on_terminal(task, DownloadStatus.FAILED)
                    raise OrchestrationError(message) from error
                self._dispatch_resume(task.id)
                raise
            await self._bus.publish(
                f"download:{task.id}",
                "status",
                {
                    "status": DownloadStatus.RETRYING,
                    **self._source_event_fields(advanced),
                },
            )
            self.dispatch(task.id)
            return advanced

    async def cancel_task(self, task_id: str, user_id: str, user_role: str) -> None:
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if user_role != "admin" and task.user_id != user_id:
            raise PermissionDeniedError("Cannot cancel another user's download")

        handle = self._active_tasks.get(task_id)
        if handle is not None and not handle.done():
            handle.cancel()
            with suppress(asyncio.CancelledError):
                await handle

        async with self._operation_lock(task_id):
            await self._cancel_task_locked(task)

    async def _cancel_task_locked(self, task) -> None:  # noqa: ANN001
        manifest_path = self._staging / task.id / "manifest.json"
        if manifest_path.exists():
            try:
                manifest = self._manifest_codec.decode(manifest_path.read_bytes())
                await self._attempt_for_manifest(task, manifest)
            except Exception:  # noqa: BLE001 - journal recovery still handles known rows
                logger.warning("Failed to journal cancellation for %s", task.id)

        publisher_bundle_ids: list[str] = []
        cleanup_disposition = "discard"
        if self._cleanup is not None:
            try:
                publisher_bundle_ids = (
                    await self._cleanup.publisher_bundle_ids_for_task(task.id)
                )
            except Exception:  # noqa: BLE001 - unknown barriers preserve source bytes
                logger.exception("Could not resolve cancellation publication barriers")
                cleanup_disposition = "preserve"
        attempt_ids = await self._store.cancel_task_and_schedule_attempts(
            task.id,
            publisher_bundle_ids=publisher_bundle_ids,
            cleanup_disposition=cleanup_disposition,
            cancelled_at=time.time(),
        )
        if self._cleanup is not None and cleanup_disposition == "discard":
            for attempt_id in attempt_ids:
                try:
                    await self._cleanup.cleanup_now(
                        attempt_id, worker_id=f"cancel-{task.id}"
                    )
                except Exception:  # noqa: BLE001 - worker retries persisted debt
                    logger.warning(
                        "Immediate cancellation cleanup failed for attempt %s",
                        attempt_id,
                    )
        await asyncio.to_thread(
            shutil.rmtree, self._staging / task.id, ignore_errors=True
        )
        logger.info(
            "download.cancelled", extra={"task_id": task.id, "user_id": task.user_id}
        )
        # Flip the linked request to 'cancelled' too, so a cancelled (or stopped-retrying)
        # download clears the album UI's "retry scheduled" line instead of sitting failed.
        await self._sync_request_on_terminal(task, DownloadStatus.CANCELLED)
        await self._bus.publish(
            f"download:{task.id}", "complete", {"status": DownloadStatus.CANCELLED}
        )

    async def retry_task(self, task_id: str, user_id: str, user_role: str) -> str:
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if user_role != "admin" and task.user_id != user_id:
            raise PermissionDeniedError("Cannot retry another user's download")
        if task.status not in (
            DownloadStatus.FAILED,
            DownloadStatus.CANCELLED,
            DownloadStatus.PARTIAL,
        ):
            raise ValidationError(
                "Only failed, cancelled or partial downloads can be retried"
            )

        # The library may already cover the request (an earlier attempt imported it
        # while the task stayed partial, or the user moved files in by hand): settling
        # COMPLETED is honest and beats re-downloading the whole album (#131).
        if task.download_type == "album" and task.release_group_mbid:
            coverage = await self._coverage(task, context="manual_retry_skip")
            if coverage is not None:
                covered, expected_total, _orphans = coverage
                if expected_total > 0 and covered >= expected_total:
                    logger.info(
                        "download.retry_already_satisfied",
                        extra={
                            "task_id": task.id,
                            "expected": expected_total,
                            "covered": covered,
                        },
                    )
                    await self._finalize(task, DownloadStatus.COMPLETED)
                    return task.id

        # Manual retry is an explicit "try again": clear the album's blocklist so a release
        # quarantined by the failed attempt is reconsidered. Album downloads only - a
        # per-track retry must not wipe the whole album's blocklist. Auto-retry
        # (retry_failed_tasks -> _create_retry_task) deliberately does NOT clear.
        if task.download_type == "album" and task.release_group_mbid:
            cleared = await self._store.delete_quarantine_for_album(
                task.release_group_mbid
            )
            if cleared:
                logger.info(
                    "download.blocklist_cleared_on_retry",
                    extra={
                        "release_group_mbid": task.release_group_mbid,
                        "cleared": cleared,
                    },
                )

        return await self._create_retry_task(task)

    async def reimport_task(self, task_id: str):  # noqa: ANN201
        async with self._operation_lock(task_id):
            return await self._reimport_task_locked(task_id)

    async def _reimport_task_locked(self, task_id: str):  # noqa: ANN201
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
            raise ValidationError(
                "This download never selected a source to reimport from"
            )

        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if task.candidate_index >= len(candidates):
            raise ValidationError("Original source is no longer available")
        candidate = candidates[task.candidate_index]
        # NOTE: reimport is deliberately NOT quality-re-gated (owner D2). It re-imports
        # files the admin already fetched by hand in slskd; blocking on a since-tightened
        # policy would only strand already-downloaded bytes, so honour the explicit action.

        # A 1-track album (a single) reimports under the same canonical-duration
        # verification as a track download (2026-07-05 wrong-single incident).
        is_single = task.download_type == "album" and task.track_count == 1
        use_canonical = (task.download_type == "track" or is_single) and bool(
            task.track_duration_seconds
        )
        release_mbid, expected_tracks = await _expected_tracks_for_task(
            task, self._album_service, self._store
        )
        if self._album_service is not None and not expected_tracks:
            raise ValidationError(
                "The original exact MusicBrainz track map is unavailable for reimport"
            )
        manifest = DownloadManifest(
            task_id=task.id,
            source_username=candidate.username,
            release_group_mbid=task.release_group_mbid,
            release_mbid=release_mbid,
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
                    duration=task.track_duration_seconds
                    if use_canonical
                    else f.duration,
                )
                for f in candidate.files
            ],
            expected_tracks=expected_tracks,
        )
        reimport_attempt = await self._store.get_download_attempt_for_candidate(
            task.id, task.source, task.candidate_index
        )
        if reimport_attempt is None:
            reimport_attempt = await self._store.create_download_attempt(
                task_id=task.id,
                source=task.source,
                candidate_index=task.candidate_index,
                job_name=manifest.handle.job_name if manifest.handle else "",
                handle=manifest.handle,
            )
            reimport_attempt = await self._store.update_download_attempt_handle(
                reimport_attempt.id, manifest.handle
            )
        else:
            reimport_attempt = await self._store.acquire_download_attempt_for_reimport(
                reimport_attempt.id
            )
            if reimport_attempt is None:
                raise ValidationError(
                    "The source files are being cleaned up or have already been removed"
                )
            manifest.handle = reimport_attempt.handle
        manifest.attempt_id = reimport_attempt.id

        await self._store.update_status(task.id, DownloadStatus.PROCESSING)
        await self._bus.publish(
            f"download:{task.id}", "status", {"status": DownloadStatus.PROCESSING}
        )

        try:
            result, _ = await self._import_files(task, manifest, completed=True)

            if result.management_hold_reason_code is not None:
                await self._finalize(
                    task,
                    DownloadStatus.FAILED,
                    error_message=(
                        _MANAGEMENT_HELD_MSG
                        if result.management_hold_secured
                        else _MANAGEMENT_HOLD_STORAGE_MSG
                    ),
                    process_result=result,
                    manifest_override=manifest,
                )
                return await self._store.get_task(task.id)

            if not result.succeeded and any(
                f.reason == DOWNLOADS_MOUNT_UNAVAILABLE for f in result.failed
            ):
                await self._finalize(
                    task,
                    DownloadStatus.FAILED,
                    error_message=DOWNLOADS_MOUNT_UNAVAILABLE,
                    process_result=result,
                    manifest_override=manifest,
                )
                return await self._store.get_task(task.id)

            if await self._download_is_complete(task, bool(result.succeeded), result):
                await self._finalize(
                    task,
                    DownloadStatus.COMPLETED,
                    process_result=result,
                    manifest_override=manifest,
                )
            elif result.succeeded:
                await self._finalize(
                    task,
                    DownloadStatus.PARTIAL,
                    process_result=result,
                    manifest_override=manifest,
                )
            else:
                if any(f.reason == SOURCE_FILE_MISSING for f in result.failed):
                    fail_msg = _FILES_NOT_FOUND_MSG
                elif any(f.reason == IMPORT_FAILED for f in result.failed):
                    fail_msg = _IMPORT_FAILED_MSG
                elif any(f.reason == "tag_mismatch" for f in result.failed):
                    fail_msg = _TAG_MISMATCH_MSG
                else:
                    fail_msg = _NO_SOURCE_MSG
                await self._finalize(
                    task,
                    DownloadStatus.FAILED,
                    error_message=fail_msg,
                    process_result=result,
                    manifest_override=manifest,
                )
        except Exception:
            logger.exception("Unexpected error during reimport of task %s", task.id)
            await self._schedule_attempt_cleanup(task, manifest, disposition="preserve")
            await self._finalize(
                task,
                DownloadStatus.FAILED,
                error_message="Reimport failed unexpectedly",
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
            release_track_mbid=task.release_track_mbid,
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
            # Retry REUSES the stored snapshot (spec): the original policy
            # governs; restart-with-current-policy is the explicit refresh.
            quality_snapshot_json=getattr(task, "quality_snapshot_json", None),
            quality_snapshot_hash=getattr(task, "quality_snapshot_hash", None),
            quality_snapshot_summary=getattr(task, "quality_snapshot_summary", None),
            quality_preference_step=getattr(task, "quality_preference_step", None),
        )
        # The retried task owns its staging dir from birth (#285 class): every
        # manifest write in the enqueue path targets <staging>/<new_id>/, and a
        # missing parent must never be able to fail a retry before its transfer
        # even starts. Strategies mkdir too; this is the belt to their braces.
        await asyncio.to_thread(
            lambda: (self._staging / new_task.id).mkdir(parents=True, exist_ok=True)
        )
        linked = await self._relink_request(task, new_task.id)
        if linked:
            self.dispatch(new_task.id)
        return new_task.id


    async def _locate_track_request(self, task) -> object | None:  # noqa: ANN001 - DownloadTask
        """The exact-track history row tied to ``task`` - looked up by the old
        download task ID first, then by its recording-MBID key."""
        record = await self._request_history.async_get_record_by_download_task_id(
            task.id, request_kind="track"
        )
        if record is not None or not task.recording_mbid:
            return record
        return await self._request_history.async_get_record(
            task.recording_mbid, request_kind="track"
        )

    async def _relink_request(self, task, new_task_id: str) -> bool:  # noqa: ANN001 - DownloadTask
        """Point the linked request - album or exact-track - at the replacement
        task via its generation CAS. Exact-track retries relink only their own
        ``request_kind='track'`` row, never an album row. Losing the CAS means a
        newer retry/re-request owns this generation: the fresh task is cancelled
        instead of racing that successor unlinked."""
        if self._request_history is None:
            return True
        try:
            kwargs: dict[str, object] = {}
            if task.download_type == "track":
                record = await self._locate_track_request(task)
                kwargs["request_kind"] = "track"
            elif task.release_group_mbid:
                record = await self._request_history.async_get_record(
                    task.release_group_mbid
                )
            else:
                return True
            if (
                record is None
                or getattr(record, "download_task_id", None) != task.id
            ):
                return True
            generation = _generation_of(record)
            if generation is not None:
                kwargs["expected_generation"] = generation
            linked = await self._request_history.async_update_download_task_id(
                record.musicbrainz_id,
                new_task_id,
                **kwargs,
            )
            if linked is False:
                await self.cancel_task(new_task_id, task.user_id, "user")
                return False
            return True
        except Exception:  # noqa: BLE001 - re-link must never fail the retry
            logger.warning("Could not re-link request for retry of %s", task.id)
            return True

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
        return [
            round(self._retry_backoff_seconds(n) / 60)
            for n in range(self.auto_retry_max)
        ]

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
            # The library may already cover the request (#131): an earlier attempt
            # imported the album while the task stayed partial, or the user moved the
            # files in by hand. Settling COMPLETED here is honest and stops the
            # 15-minute re-download loop at its trigger instead of after it.
            if task.download_type == "album" and task.release_group_mbid:
                coverage = await self._coverage(task, context="auto_retry_skip")
                if coverage is not None:
                    covered, expected_total, _orphans = coverage
                    if expected_total > 0 and covered >= expected_total:
                        logger.info(
                            "download.auto_retry_already_satisfied",
                            extra={
                                "task_id": task.id,
                                "expected": expected_total,
                                "covered": covered,
                            },
                        )
                        await self._finalize(task, DownloadStatus.COMPLETED)
                        continue
            await self._bus.publish(
                f"download:{task.id}",
                "auto_retry",
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
