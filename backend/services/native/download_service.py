"""``DownloadService`` - the user-facing search/pick/cancel service.

Checks the library, runs a background slskd search, ranks candidates, and on a
pick creates a queued ``download_tasks`` row linked to the search job and
dispatches the orchestrator.
"""

import asyncio
import logging
import os
from pathlib import Path
from typing import TYPE_CHECKING

from core.exceptions import (
    ConfigurationError,
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from core.task_registry import TaskRegistry
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.sse_publisher import SSEPublisher
from models.download import DownloadsMountStatus, ScoredCandidate, SearchJob, TargetAlbum
from repositories.protocols.download_client import DownloadClientProtocol
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.library_manager import LibraryManager

if TYPE_CHECKING:
    from repositories.protocols.musicbrainz import MusicBrainzRepository
    from services.album_service import AlbumService
    from services.native.musicbrainz_matcher import MusicBrainzMatcher

logger = logging.getLogger(__name__)

ALREADY_IN_LIBRARY = "already_in_library"

_LOSSLESS = {"flac", "alac", "wav", "ape", "wv"}


def check_downloads_mount(
    downloads_path: Path | str | None, library_paths: list[Path]
) -> DownloadsMountStatus:
    """Verify slskd's downloads dir is set -> exists -> writable -> on the same
    filesystem as a library path (so the import ``os.rename`` won't fail with EXDEV).
    Returns a structured per-condition reason; never raises."""
    if not downloads_path:
        return DownloadsMountStatus(ok=False, reason="not_set", path="")
    path = Path(downloads_path)
    path_str = str(path)
    if not path.exists():
        return DownloadsMountStatus(ok=False, reason="missing", path=path_str)
    if not os.access(path, os.W_OK):
        return DownloadsMountStatus(ok=False, reason="not_writable", path=path_str)
    try:
        dev = path.stat().st_dev
        existing = [lib for lib in library_paths if lib.exists()]
        same_fs = any(lib.stat().st_dev == dev for lib in existing)
    except OSError as exc:
        return DownloadsMountStatus(ok=False, reason=f"stat_error: {exc}", path=path_str)
    if existing and not same_fs:
        return DownloadsMountStatus(ok=False, reason="different_filesystem", path=path_str)
    return DownloadsMountStatus(ok=True, reason="ok", path=path_str)


class DownloadService:
    def __init__(
        self,
        download_client: DownloadClientProtocol,
        scorer: AlbumPreflightScorer,
        library_manager: LibraryManager,
        download_store: DownloadStore,
        event_bus: SSEPublisher,
        orchestrator: DownloadOrchestrator,
        *,
        matcher: "MusicBrainzMatcher | None" = None,
        musicbrainz: "MusicBrainzRepository | None" = None,
        album_service: "AlbumService | None" = None,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        enabled: bool = True,
    ):
        self._client = download_client
        self._scorer = scorer
        self._library = library_manager
        self._store = download_store
        self._bus = event_bus
        self._orchestrator = orchestrator
        self._matcher = matcher
        self._mb = musicbrainz
        self._album_service = album_service
        self._auto = auto_accept_threshold
        self._manual = manual_threshold
        self._enabled = enabled

    def _ensure_enabled(self) -> None:
        # flag captured at construction; the config-save PUT clears the
        # DownloadService singleton to pick up changes
        if not self._enabled:
            raise ConfigurationError(
                "The download client is disabled. Enable it in Settings to start downloads."
            )

    async def _ensure_track_count(
        self, release_group_mbid: str | None, track_count: int | None
    ) -> int | None:
        """Backfill an album's track count from MusicBrainz when the request omitted
        it (every request/auto-download path does today). Without it the preflight
        scorer can't down-rank a partial folder and the orchestrator's completeness
        gate accepts a 2-of-12 source as 'complete'. Best-effort: a MusicBrainz failure
        must never block the download. Reuses the album page's resolver so the gate's
        'expected' matches the track count the user sees on the album."""
        if (
            track_count is not None
            or not release_group_mbid
            or self._album_service is None
        ):
            return track_count
        try:
            info = await self._album_service.get_album_tracks_info(release_group_mbid)
        except Exception:  # noqa: BLE001 - track count is best-effort, never block
            logger.warning(
                "Track-count backfill failed for %s; downloading without a "
                "completeness target",
                release_group_mbid,
            )
            return None
        return info.total_tracks or None

    async def search_album(
        self,
        user_id: str,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        release_group_mbid: str | None = None,
    ) -> str:
        """Returns the new search job id, or the ``already_in_library`` sentinel."""
        self._ensure_enabled()
        if release_group_mbid and await self._library.has_album(release_group_mbid):
            return ALREADY_IN_LIBRARY

        track_count = await self._ensure_track_count(release_group_mbid, track_count)
        job = await self._store.create_search_job(
            user_id=user_id,
            artist_name=artist_name,
            album_title=album_title,
            year=year,
            track_count=track_count,
            release_group_mbid=release_group_mbid,
            search_query=f"{artist_name} - {album_title}",
        )
        task = asyncio.create_task(
            self._run_search(job.id, artist_name, album_title, year, track_count)
        )
        task.add_done_callback(self._log_task_exception)
        TaskRegistry.get_instance().register(f"search-{job.id}", task)
        return job.id

    async def _run_search(
        self,
        job_id: str,
        artist: str,
        album: str,
        year: int | None,
        track_count: int | None,
    ) -> None:
        await self._bus.publish(f"search:{job_id}", "status", {"status": "searching"})
        try:
            results = await self._client.search_album(artist, album, year, track_count)
        except Exception:
            logger.exception("slskd album search failed for job %s", job_id)
            await self._store.update_search_job_status(job_id, "failed", error="search failed")
            await self._bus.publish(f"search:{job_id}", "complete", {"status": "failed"})
            return

        target = TargetAlbum(
            artist_name=artist, album_title=album, year=year, track_count=track_count
        )
        candidates = await self._scorer.rank(
            target, results, auto_accept_threshold=self._auto, manual_threshold=self._manual
        )
        await self._store.set_search_job_candidates(job_id, candidates)
        await self._store.update_search_job_status(job_id, "completed")
        await self._bus.publish(
            f"search:{job_id}",
            "complete",
            {
                "status": "completed",
                "candidate_count": len(candidates),
                "top_score": candidates[0].final_score if candidates else 0.0,
            },
        )

    async def get_search_job(
        self, user_id: str, job_id: str
    ) -> tuple[SearchJob, list[ScoredCandidate]]:
        job = await self._store.get_search_job(job_id)
        if job is None:
            raise ResourceNotFoundError("Search job not found")
        if job.user_id != user_id:
            raise PermissionDeniedError("Cannot view another user's search job")
        candidates = await self._store.get_search_job_candidates(job_id)
        return job, candidates

    async def pick_candidate(self, user_id: str, job_id: str, candidate_index: int) -> str:
        """User picked a manual-tier candidate -> create the linked queued task and
        dispatch the orchestrator."""
        self._ensure_enabled()
        job = await self._store.get_search_job(job_id)
        if job is None:
            raise ResourceNotFoundError("Search job not found")
        if job.user_id != user_id:
            raise PermissionDeniedError("Cannot pick on another user's search job")
        candidates = await self._store.get_search_job_candidates(job_id)
        if candidate_index < 0 or candidate_index >= len(candidates):
            raise ValidationError("Invalid candidate index")
        candidate = candidates[candidate_index]

        task = await self._store.create_task(
            user_id=user_id,
            download_type="album",
            release_group_mbid=job.release_group_mbid or "",
            artist_name=job.artist_name,
            album_title=job.album_title,
            year=job.year,
            track_count=job.track_count,
            source_username=candidate.username,
            source_directory=candidate.parent_directory,
            preflight_score=candidate.final_score,
            search_job_id=job_id,
            candidate_index=candidate_index,
            status="queued",
        )
        await self._store.update_search_job_status(job_id, "matched")
        # orchestrator skips search (candidate already linked) and goes straight to
        # enqueue -> poll -> import
        self._orchestrator.dispatch(task.id)
        return task.id

    async def request_album(
        self,
        user_id: str,
        release_group_mbid: str,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        recording_mbid: str | None = None,
        track_title: str | None = None,
        track_duration_seconds: float | None = None,
        download_type: str = "album",
    ) -> str:
        """Create a download task and dispatch the orchestrator. Returns the new
        task id, the existing active task id (dedup), or the ``already_in_library``
        sentinel. The orchestrator runs search -> score -> auto-pick internally."""
        self._ensure_enabled()
        # skipped for orphan-track requests, which download a track whose album
        # isn't in the library yet
        if download_type == "album" and await self._library.has_album(release_group_mbid):
            return ALREADY_IN_LIBRARY

        # track tasks dedup on the recording (not the album) so a different track of
        # the same album runs concurrently
        if download_type == "track" and recording_mbid:
            existing = await self._store.get_active_task_for_track(recording_mbid, user_id)
        else:
            existing = await self._store.get_active_task_for_album(release_group_mbid, user_id)
        if existing:
            return existing.id

        # Folder naming uses the request's year ({album} ({year})); compact request
        # buttons don't always supply it. Backfill from the release group when missing,
        # or the folder is created as "Album ()". After dedup, so it runs once per new
        # request; best-effort, since a MusicBrainz failure must not fail the download
        # over a missing year.
        if year is None and release_group_mbid and self._mb is not None:
            try:
                album_meta = await self._mb.get_release_group(release_group_mbid)
            except Exception:  # noqa: BLE001 - year is best-effort, never block the request
                logger.warning(
                    "Year backfill failed for %s; requesting without a year",
                    release_group_mbid,
                )
                album_meta = None
            if album_meta is not None:
                year = album_meta.year
                artist_name = artist_name or album_meta.artist_name
                album_title = album_title or album_meta.title

        # Backfill the album track count (best-effort) so the completeness gate and
        # scorer can tell a partial source from a full one. Skipped for per-track
        # downloads, which already carry track_count=1.
        track_count = await self._ensure_track_count(release_group_mbid, track_count)

        task = await self._store.create_task(
            user_id=user_id,
            download_type=download_type,
            release_group_mbid=release_group_mbid,
            recording_mbid=recording_mbid,
            artist_name=artist_name,
            album_title=album_title,
            track_title=track_title,
            year=year,
            track_count=track_count,
            track_duration_seconds=track_duration_seconds,
        )
        self._orchestrator.dispatch(task.id)
        return task.id

    async def request_track(
        self,
        user_id: str,
        recording_mbid: str,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        release_group_mbid: str | None = None,
    ) -> str:
        """Request a single track. Orphan tracks (album not in the library) resolve
        the release group via MusicBrainz, auto-create the album folder, and download
        the one track; the album appears partially present."""
        self._ensure_enabled()
        if recording_mbid and await self._library.has_track(recording_mbid):
            return ALREADY_IN_LIBRARY

        if not release_group_mbid:
            if self._matcher is None:
                raise ValidationError("Per-track download is unavailable (no MusicBrainz resolver)")
            release_group_mbid = await self._matcher.resolve_recording_to_release_group(
                recording_mbid
            )
            if not release_group_mbid:
                raise ValidationError(
                    f"Recording {recording_mbid} has no resolvable release group; "
                    "per-track download requires an album."
                )

        year: int | None = None
        if (not album_title or not artist_name) and self._mb is not None:
            album_meta = await self._mb.get_release_group(release_group_mbid)
            if album_meta is not None:
                album_title = album_title or album_meta.title
                artist_name = artist_name or album_meta.artist_name
                year = album_meta.year

        return await self.request_album(
            user_id=user_id,
            release_group_mbid=release_group_mbid,
            artist_name=artist_name or "Unknown Artist",
            album_title=album_title or "Unknown Album",
            year=year,
            track_count=1,
            recording_mbid=recording_mbid,
            track_title=track_title,
            track_duration_seconds=duration_seconds,
            download_type="track",
        )

    async def get_task(self, task_id: str, user_id: str, user_role: str):
        """One task, ownership-scoped: 404 if missing, 403 if not owner (non-admin)."""
        task = await self._store.get_task(task_id)
        if task is None:
            raise ResourceNotFoundError("Download task not found")
        if user_role != "admin" and task.user_id != user_id:
            raise PermissionDeniedError("Cannot view another user's download")
        return task

    async def list_tasks(
        self,
        user_id: str,
        user_role: str,
        status: str | None = None,
        release_group_mbid: str | None = None,
        page: int = 1,
        page_size: int = 20,
    ) -> list:
        """User-scoped task list (admins see all). Paginated, optional status +
        release-group filters."""
        return await self._store.list_tasks(
            user_id=user_id,
            user_role=user_role,
            status=status,
            release_group_mbid=release_group_mbid,
            page=page,
            page_size=page_size,
        )

    async def get_task_files(self, task_id: str, user_id: str, user_role: str):
        """The files of a task (from the linked candidate) + the task's aggregate
        counts. Returns ``(task, files)``. Per-transfer live detail beyond the
        aggregate isn't exposed by the client protocol (deferred)."""
        task = await self.get_task(task_id, user_id, user_role)
        files: list = []
        if task.search_job_id and task.candidate_index is not None:
            candidates = await self._store.get_search_job_candidates(task.search_job_id)
            if 0 <= task.candidate_index < len(candidates):
                files = candidates[task.candidate_index].files
        return task, files

    async def cancel_task(self, task_id: str, user_id: str, user_role: str) -> None:
        """Cancel a download (ownership-enforced in the orchestrator)."""
        await self._orchestrator.cancel_task(task_id, user_id, user_role)

    async def retry_task(self, task_id: str, user_id: str, user_role: str) -> str:
        """Retry a failed/cancelled/partial download; returns the new task id."""
        self._ensure_enabled()
        return await self._orchestrator.retry_task(task_id, user_id, user_role)

    @property
    def auto_retry_max(self) -> int:
        """Configured max auto-retry attempts, for the queue UI's attempt counter."""
        return self._orchestrator.auto_retry_max

    def next_retry_at(self, task) -> float | None:  # noqa: ANN001 - DownloadTask
        """When a failed/partial task's next auto-retry is due (None if it won't)."""
        return self._orchestrator.next_retry_at(task)

    async def cancel_search(self, user_id: str, job_id: str) -> bool:
        job = await self._store.get_search_job(job_id)
        if job is None:
            raise ResourceNotFoundError("Search job not found")
        if job.user_id != user_id:
            raise PermissionDeniedError("Cannot cancel another user's search job")
        await self._store.update_search_job_status(job_id, "cancelled")
        return True

    @staticmethod
    def _log_task_exception(task: asyncio.Task) -> None:
        if task.cancelled():
            return
        exc = task.exception()
        if exc:
            logger.error("Background search task failed: %s", exc, exc_info=exc)
