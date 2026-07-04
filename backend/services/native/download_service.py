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
from repositories.protocols.indexer import IndexerProtocol
from services.native.acquisition.status import DownloadStatus
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.library_manager import LibraryManager
from services.native.quality_tiers import should_acquire, tier_for, tier_rank

if TYPE_CHECKING:
    from models.held_import import HeldImport
    from repositories.protocols.musicbrainz import MusicBrainzRepository
    from services.album_service import AlbumService
    from services.native.file_processor import FileProcessor
    from services.native.musicbrainz_matcher import MusicBrainzMatcher

logger = logging.getLogger(__name__)

# Fixed v1 source -> client_type map (the DownloadTask.download_client value).
_CLIENT_FOR_SOURCE = {"soulseek": "slskd", "usenet": "sabnzbd"}

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
        indexer: IndexerProtocol,
        scorer: AlbumPreflightScorer,
        library_manager: LibraryManager,
        download_store: DownloadStore,
        event_bus: SSEPublisher,
        orchestrator: DownloadOrchestrator,
        *,
        file_processor: "FileProcessor | None" = None,
        matcher: "MusicBrainzMatcher | None" = None,
        musicbrainz: "MusicBrainzRepository | None" = None,
        album_service: "AlbumService | None" = None,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        enabled: bool = True,
        usenet_indexer=None,  # IndexerProtocol | None
        usenet_scorer=None,  # NewznabReleaseScorer | None
        usenet_enabled: bool = False,
        soulseek_enabled: bool = True,  # the slskd enable toggle (separate from is_configured)
        upgrade_allowed: bool = False,
        quality_cutoff: str = "lossless",
        quota_service=None,  # QuotaService | None - byte-cap admission (Feature C layer 2)
        release_pin_store=None,  # AlbumReleasePinStore | None - edition pins (Feature E)
    ):
        self._client = download_client
        self._indexer = indexer
        self._usenet_indexer = usenet_indexer
        self._usenet_scorer = usenet_scorer
        self._usenet_enabled = usenet_enabled and usenet_indexer is not None
        self._soulseek_enabled = soulseek_enabled
        # Cutoff/upgrade (step 8). Default upgrade_allowed=False -> the album gate is the prior
        # binary "have it -> skip"; opt in to re-acquire a sub-cutoff album as an upgrade.
        self._upgrade_allowed = upgrade_allowed
        self._quality_cutoff = quality_cutoff
        self._scorer = scorer
        self._library = library_manager
        self._store = download_store
        self._bus = event_bus
        self._orchestrator = orchestrator
        self._file_processor = file_processor
        self._matcher = matcher
        self._mb = musicbrainz
        self._album_service = album_service
        self._auto = auto_accept_threshold
        self._manual = manual_threshold
        self._enabled = enabled
        self._quota = quota_service
        self._pins = release_pin_store

    def _ensure_enabled(self) -> None:
        # flag captured at construction; the config-save PUT clears the
        # DownloadService singleton to pick up changes
        if not self._enabled:
            raise ConfigurationError(
                "The download client is disabled. Enable it in Settings to start downloads."
            )

    async def _already_satisfied(self, release_group_mbid: str, origin: str = "user") -> bool:
        """True when the library already holds this album at a quality this request won't
        improve on. Origin-aware (D18): only an ``origin='upgrade'`` request may treat a
        below-cutoff held album as not-satisfied - replace-on-import fires only for
        upgrades, so re-fetching for any other origin would download bytes that are then
        skipped at placement. Every non-upgrade origin sees any held copy as satisfied."""
        held = await self._library.album_quality_tier(release_group_mbid)
        if origin != "upgrade":
            return held is not None
        if held is None:
            # An upgrade of nothing is not an upgrade: an un-held album must go
            # through the normal (quota/cap-checked) request path, never the
            # exempt upgrade path - and with upgrades off nothing may pass either.
            return True
        return not should_acquire(held, self._quality_cutoff, self._upgrade_allowed)

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
        if release_group_mbid and await self._already_satisfied(release_group_mbid):
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
        target = TargetAlbum(
            artist_name=artist, album_title=album, year=year, track_count=track_count
        )
        # Manual search fans out to ALL enabled sources at once (D15) and pools the
        # results source-grouped (D16) - Soulseek first, then Usenet. A disabled source is
        # skipped entirely; a source erroring only drops its group; the whole search fails
        # only if the primary is enabled, errors, and nothing else produced candidates.
        candidates: list[ScoredCandidate] = []
        soulseek_ok = True
        if self._soulseek_enabled:
            try:
                candidates.extend(await self._search_soulseek(target))
            except Exception:
                logger.exception("soulseek album search failed for job %s", job_id)
                soulseek_ok = False
        if self._usenet_enabled:
            try:
                candidates.extend(await self._search_usenet(target))
            except Exception:
                logger.exception("usenet album search failed for job %s", job_id)

        if not candidates and not soulseek_ok:
            await self._store.update_search_job_status(job_id, "failed", error="search failed")
            await self._bus.publish(f"search:{job_id}", "complete", {"status": "failed"})
            return
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

    async def _search_soulseek(self, target: TargetAlbum) -> list[ScoredCandidate]:
        indexer_results = await self._indexer.search_album(
            target.artist_name, target.album_title, target.year, target.track_count
        )
        results = [r.soulseek for r in indexer_results if r.soulseek is not None]
        return await self._scorer.rank(
            target, results, auto_accept_threshold=self._auto, manual_threshold=self._manual
        )

    async def _search_usenet(self, target: TargetAlbum) -> list[ScoredCandidate]:
        indexer_results = await self._usenet_indexer.search_album(
            target.artist_name, target.album_title, target.year, target.track_count
        )
        releases = [r.usenet for r in indexer_results if r.usenet is not None]
        return await self._usenet_scorer.rank(
            target, releases, auto_accept_threshold=self._auto,
            manual_threshold=self._manual, track_count=target.track_count,
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

        # Byte-cap admission (Feature C layer 2): the manual-pick path creates a
        # task directly, bypassing request_album, so it needs its own gate.
        if self._quota is not None:
            await self._quota.check_storage_admission(user_id, "user")

        # Route a picked Usenet candidate to SABnzbd, not the slskd default (D2/D16).
        task = await self._store.create_task(
            user_id=user_id,
            download_type="album",
            release_group_mbid=job.release_group_mbid or "",
            artist_mbid=job.artist_mbid,
            artist_name=job.artist_name,
            album_title=job.album_title,
            year=job.year,
            track_count=job.track_count,
            origin="user",
            source=candidate.source,
            download_client=_CLIENT_FOR_SOURCE.get(candidate.source, "slskd"),
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
        artist_mbid: str | None = None,
        origin: str = "user",
        release_mbid: str | None = None,
    ) -> str:
        """Create a download task and dispatch the orchestrator. Returns the new
        task id, the existing active task id (dedup), or the ``already_in_library``
        sentinel. The orchestrator runs search -> score -> auto-pick internally."""
        self._ensure_enabled()
        # skipped for orphan-track requests, which download a track whose album
        # isn't in the library yet
        if download_type == "album" and await self._already_satisfied(release_group_mbid, origin):
            return ALREADY_IN_LIBRARY

        # track tasks dedup on the recording (not the album) so a different track of
        # the same album runs concurrently
        if download_type == "track" and recording_mbid:
            existing = await self._store.get_active_task_for_track(recording_mbid, user_id)
        else:
            existing = await self._store.get_active_task_for_album(release_group_mbid, user_id)
        if existing:
            return existing.id

        # Byte-cap admission (Feature C layer 2, D11): global cap + per-user storage
        # quota before any bytes are committed. After dedup (re-asking for an active
        # task must keep returning it), before the MusicBrainz backfills (don't spend
        # external calls on a rejected request). Upgrades are exempt (size-neutral).
        if self._quota is not None:
            await self._quota.check_storage_admission(user_id, origin)

        # Edition pin (Feature E, D14): when no explicit release was asked for, the
        # album's pinned edition becomes the task's release_mbid - a SOFT target
        # (scoring hint + tag stamp), never a hard filter. Resolving it here covers
        # every path (requests, upgrades, auto-download) without threading it through
        # each caller.
        if release_mbid is None and self._pins is not None and release_group_mbid:
            try:
                release_mbid = await self._pins.get(release_group_mbid)
            except Exception:  # noqa: BLE001 - the pin is best-effort, never block a request
                logger.warning("Edition-pin lookup failed for %s", release_group_mbid)

        # A manual re-request is an explicit "try again" - clear this album's blocklist so
        # releases quarantined by an earlier failed attempt are reconsidered (otherwise the
        # scorer keeps filtering them and the re-request finds nothing). Album-scoped only;
        # a per-track retry must not wipe the whole album's blocklist.
        if download_type == "album" and release_group_mbid:
            cleared = await self._store.delete_quarantine_for_album(release_group_mbid)
            if cleared:
                logger.info(
                    "download.blocklist_cleared_on_request",
                    extra={"release_group_mbid": release_group_mbid, "cleared": cleared},
                )

        # Folder naming uses the request's year ({album} ({year})); compact request
        # buttons don't always supply it. Backfill (year, and the artist mbid for the
        # queue UI's artist link) from the release group when missing, or the folder
        # is created as "Album ()". After dedup, so it runs once per new request;
        # best-effort, since a MusicBrainz failure must not fail the download.
        if (year is None or artist_mbid is None) and release_group_mbid and self._mb is not None:
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
                artist_mbid = artist_mbid or album_meta.artist_id

        # Backfill the album track count (best-effort) so the completeness gate and
        # scorer can tell a partial source from a full one. Skipped for per-track
        # downloads, which already carry track_count=1.
        track_count = await self._ensure_track_count(release_group_mbid, track_count)

        task = await self._store.create_task(
            user_id=user_id,
            download_type=download_type,
            release_group_mbid=release_group_mbid,
            release_mbid=release_mbid,
            recording_mbid=recording_mbid,
            artist_mbid=artist_mbid,
            artist_name=artist_name,
            album_title=album_title,
            track_title=track_title,
            year=year,
            track_count=track_count,
            track_duration_seconds=track_duration_seconds,
            origin=origin,
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
        artist_mbid: str | None = None,
        origin: str = "user",
        release_mbid: str | None = None,
    ) -> str:
        """Request a single track. Orphan tracks (album not in the library) resolve
        the release group via MusicBrainz, auto-create the album folder, and download
        the one track; the album appears partially present."""
        self._ensure_enabled()
        if recording_mbid:
            if origin == "upgrade":
                # per-recording floor (D12): a track upgrade must beat the BEST held
                # copy of that recording, and only while upgrades are on + below cutoff.
                # An un-held recording is no upgrade target (see _already_satisfied).
                held = await self._library.recording_quality_tier(recording_mbid)
                if held is None or not should_acquire(
                    held, self._quality_cutoff, self._upgrade_allowed
                ):
                    return ALREADY_IN_LIBRARY
            elif await self._library.has_track(recording_mbid):
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
        if (not album_title or not artist_name or not artist_mbid) and self._mb is not None:
            album_meta = await self._mb.get_release_group(release_group_mbid)
            if album_meta is not None:
                album_title = album_title or album_meta.title
                artist_name = artist_name or album_meta.artist_name
                artist_mbid = artist_mbid or album_meta.artist_id
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
            artist_mbid=artist_mbid,
            origin=origin,
            release_mbid=release_mbid,
        )

    @property
    def upgrade_allowed(self) -> bool:
        return self._upgrade_allowed

    @property
    def quality_cutoff(self) -> str:
        return self._quality_cutoff

    async def list_cutoff_unmet(self) -> list[dict]:
        """The admin/trusted upgrade worklist (D7): albums whose worst held tier is
        below the cutoff. Empty when upgrades are off - the worklist is an upgrade
        surface, not a general quality report."""
        if not self._upgrade_allowed:
            return []
        return await self._library.list_cutoff_unmet(self._quality_cutoff)

    async def request_upgrade_album(
        self,
        user_id: str,
        release_group_mbid: str,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        artist_mbid: str | None = None,
    ) -> str:
        """Enqueue an album quality upgrade (origin='upgrade', D18). The route guards
        the admin/trusted role; the origin-aware gate returns ALREADY_IN_LIBRARY when
        the album is at/above cutoff or upgrades are off."""
        return await self.request_album(
            user_id=user_id,
            release_group_mbid=release_group_mbid,
            artist_name=artist_name,
            album_title=album_title,
            year=year,
            artist_mbid=artist_mbid,
            origin="upgrade",
        )

    async def request_upgrade_track(
        self,
        user_id: str,
        recording_mbid: str,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        release_group_mbid: str | None = None,
        artist_mbid: str | None = None,
    ) -> str:
        """Enqueue a per-track quality upgrade (origin='upgrade', per-recording floor D12)."""
        return await self.request_track(
            user_id=user_id,
            recording_mbid=recording_mbid,
            artist_name=artist_name,
            track_title=track_title,
            album_title=album_title,
            duration_seconds=duration_seconds,
            release_group_mbid=release_group_mbid,
            artist_mbid=artist_mbid,
            origin="upgrade",
        )

    async def acquire_edition(self, user_id: str, release_group_mbid: str) -> dict:
        """'Acquire this edition' (Feature E, D13; the route guards admin/trusted):
        for the pinned (else owned) edition's tracklist, request the MISSING tracks
        (origin='user', release as soft target D14) and upgrade the owned tracks that
        sit below the cutoff (origin='upgrade', per-recording floor). Scoped strictly
        to the edition's tracklist - a low-tier bonus track outside it never triggers
        anything. Existing files are never retagged (D15)."""
        self._ensure_enabled()
        if self._album_service is None or self._mb is None:
            raise ValidationError("Edition acquisition is unavailable")
        release_id = await self._album_service.resolve_edition(release_group_mbid)
        if not release_id:
            raise ValidationError("No edition is pinned or owned for this album")
        try:
            release = await self._mb.get_release_by_id(release_id, includes=["recordings"])
        except Exception as exc:  # noqa: BLE001 - MB hiccup -> clean 400, not a 500
            raise ValidationError("Could not load that edition from MusicBrainz") from exc
        if not release:
            raise ValidationError("Could not load that edition from MusicBrainz")
        from services.album_utils import extract_tracks

        tracks, _total_length = extract_tracks(release)
        if not tracks:
            raise ValidationError("That edition has no tracklist")

        artist_name = ""
        artist_mbid: str | None = None
        album_title = ""
        try:
            meta = await self._mb.get_release_group(release_group_mbid)
        except Exception:  # noqa: BLE001 - names are labels only, never block the acquire
            meta = None
        if meta is not None:
            artist_name = meta.artist_name
            artist_mbid = meta.artist_id
            album_title = meta.title

        rows = await self._library.get_file_rows_for_album(release_group_mbid)
        by_recording = {r["recording_mbid"]: r for r in rows if r.get("recording_mbid")}
        # positional fallback is best-effort and lossy across editions (a deluxe's
        # position N != the standard's), so recording matching is preferred
        by_position = {
            (int(r.get("disc_number") or 1), int(r.get("track_number") or 0)): r for r in rows
        }

        requested = upgraded = skipped = 0
        for track in tracks:
            row = by_recording.get(track.recording_id) or by_position.get(
                (track.disc_number or 1, track.position)
            )
            duration = round(track.length / 1000) if track.length else None
            if row is None:
                if not track.recording_id:
                    skipped += 1  # can't request a track MB gave no recording for
                    continue
                result = await self.request_track(
                    user_id=user_id,
                    recording_mbid=track.recording_id,
                    artist_name=artist_name,
                    track_title=track.title,
                    album_title=album_title,
                    duration_seconds=duration,
                    release_group_mbid=release_group_mbid,
                    artist_mbid=artist_mbid,
                    release_mbid=release_id,
                )
                if result == ALREADY_IN_LIBRARY:
                    skipped += 1
                else:
                    requested += 1
                continue
            if not self._upgrade_allowed:
                continue
            held_tier = tier_for(row.get("file_format") or "", row.get("bit_rate"))
            if tier_rank(held_tier) >= tier_rank(self._quality_cutoff):
                continue
            recording = row.get("recording_mbid") or track.recording_id
            if not recording:
                skipped += 1
                continue
            result = await self.request_upgrade_track(
                user_id=user_id,
                recording_mbid=recording,
                artist_name=artist_name,
                track_title=track.title,
                album_title=album_title,
                duration_seconds=duration,
                release_group_mbid=release_group_mbid,
                artist_mbid=artist_mbid,
            )
            if result == ALREADY_IN_LIBRARY:
                skipped += 1
            else:
                upgraded += 1
        return {
            "release_mbid": release_id,
            "total_tracks": len(tracks),
            "requested": requested,
            "upgrades": upgraded,
            "skipped": skipped,
        }

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

    async def cancel_album_retries(self, release_group_mbid: str) -> int:
        """Cancel an album's pending auto-retries (its ``failed``/``partial`` tasks) so
        removing the album from the library also stops the "retry N/M in ..." loop.
        Returns the number of tasks cancelled. No source needs to be configured - this
        is a pure status update, so it deliberately skips ``_ensure_enabled``."""
        cancelled = await self._store.cancel_album_auto_retries(release_group_mbid)
        if cancelled:
            logger.info(
                "download.album_retries_cancelled",
                extra={"release_group_mbid": release_group_mbid, "count": len(cancelled)},
            )
        return len(cancelled)

    async def purge_album_downloads(self, release_group_mbid: str) -> None:
        """Full download-side cleanup when an album is removed from the library: cancel its
        pending auto-retries (so it can't re-download), then drop its held 'Couldn't verify'
        tracks (rows + their files) and its blocklist entries - none of which should outlive
        the album. Best-effort per artifact; a stray file that won't unlink is logged, not
        raised, so it never fails the removal the user already confirmed."""
        await self.cancel_album_retries(release_group_mbid)
        held_paths = await self._store.purge_album_artifacts(release_group_mbid)
        for path in held_paths:
            try:
                Path(path).unlink(missing_ok=True)
            except OSError as exc:
                logger.warning(
                    "Could not delete held file %s on album removal: %s", path, exc
                )
        if held_paths:
            logger.info(
                "download.album_held_purged",
                extra={
                    "release_group_mbid": release_group_mbid,
                    "held_files": len(held_paths),
                },
            )

    async def reimport_task(self, task_id: str):
        self._ensure_enabled()
        return await self._orchestrator.reimport_task(task_id)

    @property
    def auto_retry_max(self) -> int:
        """Configured max auto-retry attempts, for the queue UI's attempt counter."""
        return self._orchestrator.auto_retry_max

    def next_retry_at(self, task) -> float | None:  # noqa: ANN001 - DownloadTask
        """When a failed/partial task's next auto-retry is due (None if it won't)."""
        return self._orchestrator.next_retry_at(task)

    def retry_ladder_minutes(self) -> list[int]:
        """The full auto-retry backoff schedule (minutes) for the queue UI's ladder."""
        return self._orchestrator.retry_ladder_minutes()

    # -- held imports ("import anyway" review) --

    async def held_task_ids(self, user_id: str, user_role: str) -> set[str]:
        """Task ids paused for a held-track review, so the queue shows them as needing a
        decision rather than a retry countdown that will never fire."""
        return await self._store.task_ids_with_unresolved_held(user_id, user_role)

    async def list_held(
        self, user_id: str, user_role: str, release_group_mbid: str | None = None
    ) -> list["HeldImport"]:
        """Tracks held for review, optionally scoped to one album (the album page)."""
        return await self._store.list_held_imports(user_id, user_role, release_group_mbid)

    async def get_held(
        self, held_id: int, user_id: str, user_role: str
    ) -> "HeldImport | None":
        """One held track (ownership-checked) - for the in-review audio preview."""
        return await self._store.get_held_import(held_id, user_id, user_role)

    async def import_held(self, held_id: int, user_id: str, user_role: str) -> str:
        """Force-import a held track, bypassing the AcoustID identity check (a human has
        judged it correct), and mark it resolved. Returns the library path it landed at."""
        held = await self._store.get_held_import(held_id, user_id, user_role)
        if held is None:
            raise ResourceNotFoundError("Held track not found")
        if self._file_processor is None:
            raise ConfigurationError("Import is unavailable right now")
        try:
            target = await self._file_processor.place_held_file(held)
        except FileNotFoundError as exc:
            # its copy is gone (shouldn't happen - it lives in our held area); tidy the row
            await self._store.resolve_held_import(held_id, "discarded")
            raise ValidationError(
                "The held file is no longer available - discard it and re-download the album"
            ) from exc
        await self._store.resolve_held_import(held_id, "imported")
        try:
            await self._library.reconcile_with_filesystem(targets=[target.parent])
        except Exception:  # noqa: BLE001 - reconcile is best-effort
            logger.warning("post-held-import reconcile failed for %s", target)
        # the import may have completed the album - settle the source task so a finished
        # album stops showing a phantom retry (best-effort; the import itself already stuck)
        try:
            await self._orchestrator.settle_after_manual_import(held.source_task_id)
        except Exception:  # noqa: BLE001
            logger.warning("post-held-import task settle failed for %s", held.source_task_id)
        logger.info(
            "download.held_imported",
            extra={"held_id": held_id, "release_group_mbid": held.release_group_mbid,
                   "track": held.track_title},
        )
        return str(target)

    async def discard_held(self, held_id: int, user_id: str, user_role: str) -> None:
        """Delete a held track's file and mark it discarded, re-enabling the album's
        auto-retry. The file is always removed - a rejected candidate never lingers on disk."""
        held = await self._store.get_held_import(held_id, user_id, user_role)
        if held is None:
            raise ResourceNotFoundError("Held track not found")
        try:
            Path(held.held_path).unlink(missing_ok=True)
        except OSError as exc:
            logger.warning("Could not delete held file %s: %s", held.held_path, exc)
        await self._store.resolve_held_import(held_id, "discarded")
        logger.info(
            "download.held_discarded",
            extra={"held_id": held_id, "release_group_mbid": held.release_group_mbid},
        )

    async def clear_finished(self, user_id: str, user_role: str) -> int:
        """Hard-delete the user's terminal completed + cancelled tasks (the queue's
        "Clear" bulk action). Active/failed/partial/queued rows are left untouched. A
        pure status delete - no source needs configuring, so it skips ``_ensure_enabled``
        like ``cancel_album_retries`` does. Admins clear across all users, mirroring the
        list endpoint's ownership."""
        cleared = await self._store.delete_tasks_by_status(
            user_id, user_role, [DownloadStatus.COMPLETED, DownloadStatus.CANCELLED]
        )
        if cleared:
            logger.info(
                "download.cleared_finished", extra={"user_id": user_id, "count": cleared}
            )
        return cleared

    async def stop_all_retries(self, user_id: str, user_role: str) -> int:
        """Stop every still-scheduled auto-retry the user has (the "Stop all retries"
        bulk action). A ``failed``/``partial`` task with a PENDING ``next_retry_at`` is
        "wanted"; cancelling it the same way the per-task stop does (-> status
        ``cancelled``) drops it from the retry sweep. Exhausted failures (no pending
        retry) are left for ``retry_all_failed``. Returns the number stopped."""
        tasks = await self._store.list_tasks_by_status(
            user_id, user_role, [DownloadStatus.FAILED, DownloadStatus.PARTIAL]
        )
        stopped = 0
        for task in tasks:
            if self.next_retry_at(task) is None:
                continue
            await self.cancel_task(task.id, user_id, user_role)
            stopped += 1
        return stopped

    async def retry_all_failed(self, user_id: str, user_role: str) -> int:
        """Re-dispatch every terminally-failed task the user has that will NOT auto-retry
        (the "Retry all failed" bulk action): ``status == failed`` AND no pending
        ``next_retry_at`` (auto-retry off, or attempts exhausted). Tasks still scheduled
        to auto-retry are "wanted" and left for ``stop_all_retries``. Each is retried via
        the same path as the per-task retry. Returns the number retried."""
        tasks = await self._store.list_tasks_by_status(
            user_id, user_role, [DownloadStatus.FAILED]
        )
        retried = 0
        for task in tasks:
            if self.next_retry_at(task) is not None:
                continue
            await self.retry_task(task.id, user_id, user_role)
            retried += 1
        return retried

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
