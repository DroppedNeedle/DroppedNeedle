"""Wanted watcher - weeks-scale availability re-search for dead requests.

A request that failed for availability reasons (or completed only partially)
enrols as a ``wanted_watches`` row; ``run_sweep`` re-searches due watches on an
age-based cadence with jitter. An auto-tier find downloads silently through the
normal gated pipeline (origin='wanted' - the original request already
authorised exactly that, D2/D8); manual-tier-only results just raise a badge,
and only for candidates the want has never seen (the seen set). The scout
creates no search-job row and no task (D10), so review-tab spam is
structurally impossible.

Plan: .dev-notes/Wanted/01-wanted-watcher-plan.md (owner-signed D1-D10);
as-built deviations in DECISIONS-LIVE.md.
"""

import asyncio
import logging
import random
import time
import uuid
from datetime import date, datetime, timezone
from typing import TYPE_CHECKING, Callable

import msgspec

from core.exceptions import (
    ConfigurationError,
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from infrastructure.queue.priority_queue import RequestPriority
from models.download_identity import soulseek_identity, usenet_identity
from models.wanted import WantedRetrying, WantedWatch
from services.native.acquisition.status import is_terminal
from services.native.coverage import match_rows_to_tracks, uncovered_tracks

# The availability-failure prefixes (§4.5). IMPORTED, never copied: the tie-test
# in tests/services/test_wanted_watcher_service.py fails if either side drifts.
from services.native.download_orchestrator import _NO_MATCH_MSG, _NO_SOURCE_MSG
from services.native.download_service import ALREADY_IN_LIBRARY

if TYPE_CHECKING:
    from infrastructure.persistence.download_store import DownloadStore
    from infrastructure.persistence.request_history import (
        RequestHistoryRecord,
        RequestHistoryStore,
    )
    from infrastructure.persistence.wanted_store import WantedStore
    from infrastructure.sse_publisher import SSEPublisher
    from models.download import ScoredCandidate
    from services.album_service import AlbumService
    from services.native.download_service import DownloadService
    from services.native.library_manager import LibraryManager
    from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

_DAY = 86400.0
_ENROL_PAGE_SIZE = 200
# Per-cycle ceiling on partial-want track dispatches (D9); the drop is logged
# (no-silent-caps rule) and the next cycle picks up the remainder.
_MAX_TRACK_DISPATCH_PER_CYCLE = 5
# After an auto-dispatch or an active-work guard, look again soon (~1 day) so
# the next cycle's satisfaction/guard steps observe the outcome (§5.2.d).
_SHORT_RESCHEDULE_DAYS = 1.0
# The >=1y cadence doubles from 14d to 28d after this many consecutive
# no_results/seen_only cycles (§5.2 cadence table).
_QUIET_DOUBLING_STREAK = 10


class WantedSweepSummary(msgspec.Struct, frozen=True):
    enrolled: int = 0
    checked: int = 0
    dispatched: int = 0
    fulfilled: int = 0
    errors: int = 0


def _parse_partial_date(value: str | None) -> date | None:
    """Partial MB date (YYYY / YYYY-MM / YYYY-MM-DD) -> start of its period."""
    if not value:
        return None
    parts = str(value).split("-")
    try:
        year = int(parts[0])
        month = int(parts[1]) if len(parts) > 1 else 1
        day = int(parts[2]) if len(parts) > 2 else 1
        return date(year, month, day)
    except (ValueError, IndexError):
        return None


def _interval_days(first_release_date: str | None, quiet_streak: int, now: float) -> float:
    """The age-based cadence table (D3). Unknown release date = old."""
    released = _parse_partial_date(first_release_date)
    if released is None:
        age_days = None
    else:
        epoch = datetime(
            released.year, released.month, released.day, tzinfo=timezone.utc
        ).timestamp()
        age_days = max(0.0, (now - epoch) / _DAY)
    if age_days is not None and age_days < 30:
        return 2.0
    if age_days is not None and age_days < 90:
        return 4.0
    if age_days is not None and age_days < 365:
        return 7.0
    return 28.0 if quiet_streak >= _QUIET_DOUBLING_STREAK else 14.0


class WantedWatcherService:
    def __init__(
        self,
        wanted_store: "WantedStore",
        request_history: "RequestHistoryStore",
        download_store: "DownloadStore",
        get_download_service: "Callable[[], DownloadService]",
        library_manager: "LibraryManager",
        album_service: "AlbumService",
        mb_repo,  # concrete MusicBrainz repo, the NewReleaseService way (§5.2.1)
        sse_publisher: "SSEPublisher",
        preferences: "PreferencesService",
        inter_want_delay: float = 5.0,
    ) -> None:
        self._store = wanted_store
        self._requests = request_history
        self._download_store = download_store
        # A settings save rebuilds the DownloadService singleton, so the watcher
        # resolves it fresh at every use rather than capturing an instance.
        self._get_download_service = get_download_service
        self._library = library_manager
        self._album_service = album_service
        self._mb = mb_repo
        self._sse = sse_publisher
        self._preferences = preferences
        self._inter_want_delay = inter_want_delay

    async def run_sweep(self) -> WantedSweepSummary:
        # Read fresh every sweep so flipping the toggle needs no restart (§5.3).
        settings = self._preferences.get_wanted_settings()
        if not settings.enabled:
            return WantedSweepSummary()

        enrolled = await self._enrol(settings)

        due = await self._store.list_due(time.time(), settings.max_checks_per_sweep)
        checked = dispatched = fulfilled = errors = 0
        for index, want in enumerate(due):
            try:
                outcome = await self._check_want(want, settings)
                checked += 1
                if outcome == "auto_dispatched":
                    dispatched += 1
                elif outcome == "satisfied":
                    fulfilled += 1
            except asyncio.CancelledError:
                raise
            except Exception:  # noqa: BLE001 - one bad want must never kill the sweep
                errors += 1
                logger.error(
                    "wanted.check_failed",
                    extra={"release_group_mbid": want.release_group_mbid},
                    exc_info=True,
                )
                await self._record_error_cycle(want, settings)
            if index < len(due) - 1 and self._inter_want_delay > 0:
                await asyncio.sleep(self._inter_want_delay)

        summary = WantedSweepSummary(
            enrolled=enrolled, checked=checked, dispatched=dispatched,
            fulfilled=fulfilled, errors=errors,
        )
        if enrolled or checked:
            logger.info(
                "wanted.sweep_complete",
                extra={
                    "enrolled": enrolled, "checked": checked,
                    "dispatched": dispatched, "fulfilled": fulfilled, "errors": errors,
                },
            )
        return summary

    # -- API surface (Phase 2): ownership-scoped watch management --

    async def list_watches_for(self, user_id: str, user_role: str) -> list[WantedWatch]:
        """The caller's watches; admins see everyone's (D4/Phase 3)."""
        return await self._store.list_watches(None if user_role == "admin" else user_id)

    async def stop(self, release_group_mbid: str, user_id: str, user_role: str) -> WantedWatch:
        """Per-want Stop (D1): the human opts this album out of watching."""
        await self._owned_watch(release_group_mbid, user_id, user_role)
        if await self._store.stop_watch(release_group_mbid):
            logger.info("wanted.stopped", extra={"release_group_mbid": release_group_mbid})
        return await self._owned_watch(release_group_mbid, user_id, user_role)

    async def resume(self, release_group_mbid: str, user_id: str, user_role: str) -> WantedWatch:
        """Dormant/stopped -> watching, due immediately; on an already-watching
        want this doubles as "check now" (plan §6 Phase 2 - no separate endpoint)."""
        watch = await self._owned_watch(release_group_mbid, user_id, user_role)
        if watch.state in ("dormant", "stopped"):
            await self._store.resume_watch(release_group_mbid)
            logger.info("wanted.resumed", extra={"release_group_mbid": release_group_mbid})
        elif watch.state == "watching":
            await self._store.reschedule(release_group_mbid, time.time())
        else:
            raise ValidationError(
                "This watch is already fulfilled - re-request the album to watch it again"
            )
        return await self._owned_watch(release_group_mbid, user_id, user_role)

    async def mark_seen(self, release_group_mbid: str, user_id: str, user_role: str) -> WantedWatch:
        """The user visited the candidates - clear the new-candidates badge."""
        await self._owned_watch(release_group_mbid, user_id, user_role)
        await self._store.clear_new_candidates(release_group_mbid)
        return await self._owned_watch(release_group_mbid, user_id, user_role)

    async def list_retrying_for(self, user_id: str, user_role: str) -> list[WantedRetrying]:
        """Requests still inside their auto-retry ladder, for the Wanted tab's
        read-only 'still hunting' rows (owner decision 2026-07-06). Reads the
        SAME rows the enrolment classifier does - a failed/incomplete request
        whose linked task has a pending ``next_retry_at`` - so a row graduates
        into a real watch the sweep after its ladder exhausts, with no gap."""
        download_service = self._get_download_service()
        max_attempts = download_service.auto_retry_max
        items: list[WantedRetrying] = []
        for status in ("failed", "incomplete"):
            page = 1
            while True:
                records, total = await self._requests.async_get_history(
                    page=page, page_size=_ENROL_PAGE_SIZE, status_filter=status
                )
                if not records:
                    break
                for record in records:
                    if user_role != "admin" and record.user_id != user_id:
                        continue
                    if not record.download_task_id:
                        continue
                    task = await self._download_store.get_task(record.download_task_id)
                    if task is None:
                        continue
                    next_retry_at = download_service.next_retry_at(task)
                    if next_retry_at is None:
                        continue
                    items.append(
                        WantedRetrying(
                            release_group_mbid=record.musicbrainz_id,
                            artist_name=record.artist_name,
                            album_title=record.album_title,
                            retry_count=task.retry_count,
                            max_attempts=max_attempts,
                            next_retry_at=next_retry_at,
                            artist_mbid=record.artist_mbid,
                            year=record.year,
                            cover_url=record.cover_url,
                            user_id=record.user_id,
                        )
                    )
                if page * _ENROL_PAGE_SIZE >= total:
                    break
                page += 1
        items.sort(key=lambda item: item.next_retry_at)
        return items

    async def _owned_watch(
        self, release_group_mbid: str, user_id: str, user_role: str
    ) -> WantedWatch:
        watch = await self._store.get_watch(release_group_mbid)
        if watch is None:
            raise ResourceNotFoundError("Watch not found")
        if user_role != "admin" and watch.user_id != user_id:
            raise PermissionDeniedError("Cannot manage another user's watch")
        return watch

    async def dismiss_review(self, job_id: str, user_id: str, user_role: str) -> WantedWatch:
        """"None of these - keep watching" (owner decision 2026-07-06): the human
        rejected every parked candidate, which is a human-confirmed availability
        failure. Ends the parked attempt through the normal cancel path, remembers
        every rejected candidate in the seen set (the watcher never badges those
        exact copies again), and puts the album on the watchlist directly - the
        explicit ask overrides D1's "cancelled never AUTO-watches"."""
        parked = await self._download_store.get_parked_task_for_search_job(job_id)
        if parked is None:
            raise ResourceNotFoundError("No download is awaiting review on this search")
        if user_role != "admin" and parked.user_id != user_id:
            raise PermissionDeniedError("Cannot dismiss another user's review")
        if not parked.release_group_mbid:
            raise ValidationError("This search isn't tied to an album, so it can't be watched")

        candidates = await self._download_store.get_search_job_candidates(job_id)
        seen = [(c.source, self._candidate_identity(c)) for c in candidates]

        # cancel first so the watch never coexists with an active task (the
        # active-work guard would otherwise sit on it); flips the request too
        await self._get_download_service().cancel_task(parked.id, user_id, user_role)

        watch = await self._ensure_watch_for_task(parked)
        if seen:
            await self._store.add_seen(watch.release_group_mbid, seen)
        logger.info(
            "wanted.review_dismissed",
            extra={
                "release_group_mbid": parked.release_group_mbid,
                "task_id": parked.id,
                "candidates_seen": len(seen),
            },
        )
        return watch

    async def _ensure_watch_for_task(self, task) -> WantedWatch:  # noqa: ANN001 - DownloadTask
        """A watch for the task's album, acting for the task's owner: create it,
        or revive an existing terminal/paused one (the human just asked)."""
        mbid = task.release_group_mbid
        now = time.time()
        kind = "partial" if task.download_type == "track" else "missing"
        existing = await self._store.get_watch(mbid)
        if existing is not None:
            if existing.state == "fulfilled":
                await self._store.rearm_watch(
                    mbid, user_id=task.user_id, kind=kind,
                    next_check_at=now + self._interval_seconds(
                        existing.first_release_date, quiet_streak=0, now=now
                    ),
                    now=now,
                )
            elif existing.state in ("dormant", "stopped"):
                await self._store.resume_watch(mbid, now=now)
            watch = await self._store.get_watch(mbid)
            if watch is not None:
                return watch

        record = await self._requests.async_get_record(mbid)
        first_release_date = (
            await self._first_release_date(record)
            if record is not None
            else await self._first_release_date_for_mbid(mbid, task.year)
        )
        await self._store.create_watch(
            release_group_mbid=mbid,
            user_id=task.user_id,
            artist_name=task.artist_name or (record.artist_name if record else "Unknown Artist"),
            album_title=task.album_title or (record.album_title if record else "Unknown Album"),
            kind=kind,
            next_check_at=now + self._interval_seconds(first_release_date, quiet_streak=0, now=now),
            artist_mbid=task.artist_mbid or (record.artist_mbid if record else None),
            year=task.year or (record.year if record else None),
            cover_url=record.cover_url if record else None,
            first_release_date=first_release_date,
            created_at=now,
        )
        watch = await self._store.get_watch(mbid)
        if watch is None:  # pragma: no cover - create_watch just inserted it
            raise ResourceNotFoundError("Watch not found")
        return watch

    # -- enrolment (§5.2.1) --

    async def _enrol(self, settings) -> int:  # noqa: ANN001 - WantedWatcherSettings
        enrolled = 0
        statuses = ["failed"]
        if settings.watch_partial_albums:
            statuses.append("incomplete")
        for status in statuses:
            page = 1
            while True:
                records, total = await self._requests.async_get_history(
                    page=page, page_size=_ENROL_PAGE_SIZE, status_filter=status
                )
                if not records:
                    break
                for record in records:
                    try:
                        if await self._maybe_enrol(record):
                            enrolled += 1
                    except Exception:  # noqa: BLE001 - one bad row must not stop enrolment
                        logger.warning(
                            "wanted.enrol_failed",
                            extra={"release_group_mbid": record.musicbrainz_id},
                            exc_info=True,
                        )
                if page * _ENROL_PAGE_SIZE >= total:
                    break
                page += 1
        return enrolled

    async def _maybe_enrol(self, record: "RequestHistoryRecord") -> bool:
        existing = await self._store.get_watch(record.musicbrainz_id)
        if existing is not None and existing.state != "fulfilled":
            # watching/dormant/stopped: never auto-revive - the human's choice
            # (or the running watch) stands (§5.2.1)
            return False

        kind = self._classify_status(record.status)
        if kind is None:
            return False
        if not await self._task_says_enrol(record):
            return False

        now = time.time()
        if existing is not None:
            # fulfilled: re-arm only when the user re-requested AFTER fulfilment
            # and that fresh request failed again (§5.2.1)
            if self._requested_at_epoch(record) <= (existing.last_checked_at or 0.0):
                return False
            next_check = now + self._interval_seconds(
                existing.first_release_date, quiet_streak=0, now=now
            )
            rearmed = await self._store.rearm_watch(
                record.musicbrainz_id,
                user_id=record.user_id or existing.user_id,
                kind=kind,
                next_check_at=next_check,
                now=now,
            )
            if rearmed:
                self._log_enrolled(record, kind, rearmed=True)
            return rearmed

        if not record.user_id:
            return False  # no requester to act for (D7)

        first_release_date = await self._first_release_date(record)
        next_check = now + self._interval_seconds(first_release_date, quiet_streak=0, now=now)
        inserted = await self._store.create_watch(
            release_group_mbid=record.musicbrainz_id,
            user_id=record.user_id,
            artist_name=record.artist_name,
            album_title=record.album_title,
            kind=kind,
            next_check_at=next_check,
            artist_mbid=record.artist_mbid,
            year=record.year,
            cover_url=record.cover_url,
            first_release_date=first_release_date,
            created_at=now,
        )
        if inserted:
            self._log_enrolled(record, kind, rearmed=False)
        return inserted

    @staticmethod
    def _classify_status(status: str) -> str | None:
        if status == "failed":
            return "missing"
        if status == "incomplete":
            return "partial"
        return None

    async def _task_says_enrol(self, record: "RequestHistoryRecord") -> bool:
        """§4.5: the availability signal lives on the linked TASK, not the request.
        No linked task (a dispatch-time failure) cannot be classified and never
        enrols; a task still awaiting its auto-retry isn't dead yet; a 'failed'
        task enrols only when its message prefix-matches an availability constant
        (local faults - mount/import errors - must NOT enrol)."""
        if not record.download_task_id:
            return False
        task = await self._download_store.get_task(record.download_task_id)
        if task is None or not is_terminal(task.status):
            return False
        if self._get_download_service().next_retry_at(task) is not None:
            return False
        if record.status == "incomplete":
            return True
        message = task.error_message or ""
        return message.startswith(_NO_SOURCE_MSG) or message.startswith(_NO_MATCH_MSG)

    async def _first_release_date(self, record: "RequestHistoryRecord") -> str | None:
        return await self._first_release_date_for_mbid(record.musicbrainz_id, record.year)

    async def _first_release_date_for_mbid(self, mbid: str, year: int | None) -> str | None:
        rg = await self._mb.get_release_group_by_id(
            mbid, priority=RequestPriority.BACKGROUND_SYNC
        )
        first_release_date = (rg or {}).get("first-release-date")
        if first_release_date:
            return str(first_release_date)
        # degraded fetch: the row's year gives a coarse age bucket
        return str(year) if year else None

    def _log_enrolled(self, record: "RequestHistoryRecord", kind: str, *, rearmed: bool) -> None:
        logger.info(
            "wanted.enrolled",
            extra={
                "release_group_mbid": record.musicbrainz_id,
                "kind": kind,
                "user_id": record.user_id,
                "rearmed": rearmed,
            },
        )

    # -- per-want check cycle (§5.2.3) --

    async def _check_want(self, want: WantedWatch, settings) -> str:  # noqa: ANN001
        now = time.time()
        mbid = want.release_group_mbid

        if want.kind == "partial" and not settings.watch_partial_albums:
            # partial watching toggled off since enrolment: no search, just wait
            await self._store.reschedule(
                mbid, now + self._interval_seconds(want.first_release_date, want.quiet_streak, now)
            )
            return "skipped"

        # (a) satisfaction re-check FIRST (D6 edge case: an edition-pin change can
        # retroactively satisfy a want) - a satisfied want never searches
        tracks = await self._tracklist(mbid)
        missing_tracks: list = []
        if tracks:
            rows = await self._file_rows(mbid)
            covered, _orphans, _matched = match_rows_to_tracks(rows, tracks)
            if covered >= len(tracks):
                await self._fulfil(want)
                return "satisfied"
            missing_tracks = uncovered_tracks(rows, tracks)
        elif want.kind == "missing":
            # tracklist unavailable: the library-presence semantic the requests
            # page uses (§5.2.3.a) - never raw file-row counts
            if await self._in_library(mbid):
                await self._fulfil(want)
                return "satisfied"
        else:
            # a partial want without a tracklist can't be measured: fail open,
            # never search-spam on missing data (§5.2.3.a)
            await self._store.reschedule(
                mbid, now + self._interval_seconds(want.first_release_date, want.quiet_streak, now)
            )
            return "skipped"

        # (b) active-work guard (§4.9): someone/something is already on it
        if await self._has_active_work(want):
            await self._store.reschedule(
                mbid, now + _SHORT_RESCHEDULE_DAYS * _DAY * random.uniform(0.8, 1.2)
            )
            return "guarded"

        # (c) scout: no job row, no task (D10)
        download_service = self._get_download_service()
        try:
            candidates = await download_service.scout_album(
                artist_name=want.artist_name,
                album_title=want.album_title,
                year=want.year,
                release_group_mbid=mbid,
            )
        except ConfigurationError:
            # no download source is enabled - nothing to scout against
            await self._store.reschedule(
                mbid, now + self._interval_seconds(want.first_release_date, want.quiet_streak, now)
            )
            return "skipped"

        identities = [(c.source, self._candidate_identity(c)) for c in candidates]

        # (d) auto-tier hit -> silent dispatch through the normal gated pipeline
        if settings.auto_download_on_find and self._has_auto_hit(candidates):
            if await self._dispatch(want, missing_tracks, download_service) == "satisfied":
                return "satisfied"
            await self._store.record_cycle(
                mbid,
                outcome="auto_dispatched",
                next_check_at=now + _SHORT_RESCHEDULE_DAYS * _DAY * random.uniform(0.8, 1.2),
                quiet=False,
                seen=identities,
                now=now,
            )
            self._log_checked(want, "auto_dispatched", len(candidates), 0)
            return "auto_dispatched"

        # (e)/(f) manual-tier badge diff / nothing at all
        actionable = [
            (candidate, identity)
            for candidate, identity in zip(candidates, identities)
            if candidate.tier in ("auto", "manual")
        ]
        new_count: int | None = None
        if actionable:
            seen = await self._store.seen_identities(mbid)
            fresh = [identity for _c, identity in actionable if identity not in seen]
            if fresh:
                outcome = "new_manual"
                new_count = len(fresh)
                quiet = False
                await self._publish(
                    want, "wanted_new_candidates", {"new_candidates": new_count}
                )
            else:
                outcome = "seen_only"
                quiet = True
        else:
            outcome = "no_results"
            quiet = True

        # (g) reschedule + dormancy
        new_streak = want.quiet_streak + 1 if quiet else 0
        go_dormant = (now - want.created_at) > settings.dormant_after_days * _DAY
        await self._store.record_cycle(
            mbid,
            outcome=outcome,
            next_check_at=now + self._interval_seconds(want.first_release_date, new_streak, now),
            quiet=quiet,
            go_dormant=go_dormant,
            new_candidate_count=new_count,
            seen=identities,
            now=now,
        )
        self._log_checked(want, outcome, len(candidates), new_count or 0)
        return outcome

    async def _record_error_cycle(self, want: WantedWatch, settings) -> None:  # noqa: ANN001
        """A per-want failure reschedules normally with last_outcome='error' -
        one bad want never kills the sweep (§5.2.3)."""
        try:
            now = time.time()
            await self._store.record_cycle(
                want.release_group_mbid,
                outcome="error",
                next_check_at=now + self._interval_seconds(want.first_release_date, 0, now),
                quiet=False,
                go_dormant=(now - want.created_at) > settings.dormant_after_days * _DAY,
                now=now,
            )
        except Exception:  # noqa: BLE001 - bookkeeping failure must not escalate
            logger.warning(
                "wanted.error_cycle_record_failed",
                extra={"release_group_mbid": want.release_group_mbid},
            )

    # -- dispatch (§5.2.d) --

    async def _dispatch(
        self, want: WantedWatch, missing_tracks: list, download_service: "DownloadService"
    ) -> str:
        if want.kind == "missing":
            return await self._dispatch_album(want, download_service)
        await self._dispatch_tracks(want, missing_tracks, download_service)
        return "dispatched"

    async def _dispatch_album(
        self, want: WantedWatch, download_service: "DownloadService"
    ) -> str:
        mbid = want.release_group_mbid
        # §4.8: the same two request writes the requests-page retry does, or the
        # found download never flips the request. Pending first (mirrors
        # retry_request), restored on a dispatch failure.
        await self._requests.async_update_status(mbid, "pending")
        try:
            task_id = await download_service.request_album(
                user_id=want.user_id,
                release_group_mbid=mbid,
                artist_name=want.artist_name,
                album_title=want.album_title,
                year=want.year,
                artist_mbid=want.artist_mbid,
                origin="wanted",
            )
        except Exception:
            await self._requests.async_update_status(mbid, "failed")
            raise
        if task_id == ALREADY_IN_LIBRARY:
            # the pipeline holds a copy this request can't improve on - the
            # watch has nothing left to do (satisfied by other means)
            await self._fulfil(want)
            return "satisfied"
        await self._requests.async_update_download_task_id(mbid, task_id)
        logger.info(
            "wanted.auto_dispatched",
            extra={"release_group_mbid": mbid, "task_id": task_id, "kind": "missing"},
        )
        await self._publish(want, "wanted_auto_dispatched", {"task_id": task_id})
        return "dispatched"

    async def _dispatch_tracks(
        self, want: WantedWatch, missing_tracks: list, download_service: "DownloadService"
    ) -> None:
        """Per-track dispatch for a partial want (D9), capped per cycle with the
        drop logged. Deliberately does NOT touch the album request's task link
        (§4.8); duplicate protection is request_track's per-recording dedup."""
        mbid = want.release_group_mbid
        dispatchable = [t for t in missing_tracks if t.recording_id]
        dropped = max(0, len(dispatchable) - _MAX_TRACK_DISPATCH_PER_CYCLE)
        dispatched = 0
        for track in dispatchable[:_MAX_TRACK_DISPATCH_PER_CYCLE]:
            duration = round(track.length / 1000) if track.length else None
            try:
                result = await download_service.request_track(
                    user_id=want.user_id,
                    recording_mbid=track.recording_id,
                    artist_name=want.artist_name,
                    track_title=track.title,
                    album_title=want.album_title,
                    duration_seconds=duration,
                    release_group_mbid=mbid,
                    artist_mbid=want.artist_mbid,
                    origin="wanted",
                )
            except (ValidationError, ConfigurationError) as exc:
                logger.warning(
                    "wanted.track_dispatch_failed",
                    extra={"release_group_mbid": mbid, "recording_mbid": track.recording_id,
                           "error": str(exc)},
                )
                continue
            if result != ALREADY_IN_LIBRARY:
                dispatched += 1
        if dropped:
            logger.info(
                "wanted.track_dispatch_capped",
                extra={"release_group_mbid": mbid, "dispatched": dispatched, "dropped": dropped},
            )
        logger.info(
            "wanted.auto_dispatched",
            extra={"release_group_mbid": mbid, "kind": "partial", "tracks": dispatched},
        )
        await self._publish(want, "wanted_auto_dispatched", {"tracks": dispatched})

    # -- collaborator wrappers (all fail-open: the gates downstream stay armed) --

    async def _tracklist(self, mbid: str) -> list | None:
        try:
            info = await self._album_service.get_album_tracks_info(
                mbid, priority=RequestPriority.BACKGROUND_SYNC
            )
        except Exception:  # noqa: BLE001 - coverage is fail-open (§5.2.3.a)
            return None
        tracks = list(info.tracks or [])
        return tracks or None

    async def _file_rows(self, mbid: str) -> list[dict]:
        try:
            return await self._library.get_file_rows_for_album(mbid)
        except Exception:  # noqa: BLE001 - a rows failure reads as nothing held
            return []

    async def _in_library(self, mbid: str) -> bool:
        try:
            mbids = await self._library.get_library_mbids(include_release_ids=False)
        except Exception:  # noqa: BLE001 - presence check is best-effort
            return False
        return mbid.lower() in {str(m).lower() for m in mbids}

    async def _has_active_work(self, want: WantedWatch) -> bool:
        mbid = want.release_group_mbid
        if await self._download_store.get_active_task_for_album_any_user(mbid) is not None:
            return True
        record = await self._requests.async_get_record(mbid)
        if record is None or not record.download_task_id:
            return False
        task = await self._download_store.get_task(record.download_task_id)
        if task is None:
            return False
        if self._get_download_service().next_retry_at(task) is not None:
            return True
        return await self._download_store.has_unresolved_held_for_task(task.id)

    async def _fulfil(self, want: WantedWatch) -> None:
        mbid = want.release_group_mbid
        await self._store.mark_fulfilled(mbid, "satisfied")
        try:
            await self._requests.async_update_status(
                mbid, "imported", completed_at=datetime.now(timezone.utc).isoformat()
            )
        except Exception:  # noqa: BLE001 - the watch is settled; the flip is best-effort
            logger.warning(
                "wanted.request_flip_failed", extra={"release_group_mbid": mbid}
            )
        logger.info("wanted.fulfilled", extra={"release_group_mbid": mbid})
        await self._publish(want, "wanted_fulfilled", {})

    async def _publish(self, want: WantedWatch, event: str, extra: dict) -> None:
        # event_id lets the frontend de-dupe the snapshot the SSE bus replays on
        # every reconnect (same convention as personal_mix_refreshed)
        try:
            await self._sse.publish(
                f"user:{want.user_id}",
                event,
                {
                    "event_id": uuid.uuid4().hex,
                    "release_group_mbid": want.release_group_mbid,
                    "artist_name": want.artist_name,
                    "album_title": want.album_title,
                    **extra,
                },
            )
        except Exception as exc:  # noqa: BLE001 - notification is best-effort
            logger.debug("wanted SSE publish failed: %s", exc)

    # -- cadence helpers --

    def _interval_seconds(
        self, first_release_date: str | None, quiet_streak: int, now: float
    ) -> float:
        return (
            _interval_days(first_release_date, quiet_streak, now)
            * _DAY
            * random.uniform(0.8, 1.2)
        )

    @staticmethod
    def _has_auto_hit(candidates: "list[ScoredCandidate]") -> bool:
        """Auto = a source's TOP candidate scored auto tier - the same rule as
        the orchestrator's ``_search_score_autopick``. Each source's group is
        ranked, so the first candidate seen per source is its top."""
        tops: dict[str, str] = {}
        for candidate in candidates:
            tops.setdefault(candidate.source, candidate.tier)
        return any(tier == "auto" for tier in tops.values())

    @staticmethod
    def _candidate_identity(candidate: "ScoredCandidate") -> str:
        """The seen-set identity (§4.3): the standard source encoding, folder-
        level for soulseek (a scout candidate is a (username, directory) group)."""
        if candidate.source == "usenet" and candidate.usenet_release is not None:
            return usenet_identity(
                candidate.usenet_release.title, candidate.usenet_release.size_bytes
            )
        return soulseek_identity(candidate.username, candidate.parent_directory)

    @staticmethod
    def _requested_at_epoch(record: "RequestHistoryRecord") -> float:
        try:
            return datetime.fromisoformat(record.requested_at).timestamp()
        except (TypeError, ValueError):
            return 0.0

    def _log_checked(
        self, want: WantedWatch, outcome: str, candidates: int, new_candidates: int
    ) -> None:
        logger.info(
            "wanted.checked",
            extra={
                "release_group_mbid": want.release_group_mbid,
                "kind": want.kind,
                "outcome": outcome,
                "candidates": candidates,
                "new_candidates": new_candidates,
            },
        )
