"""Source strategies (ArrRebuild step 4).

Each acquisition source (Soulseek via slskd, Usenet via Newznab+SABnzbd) differs in
search, identity, enqueue, poll→status, completed-file enumeration and cleanup. Lidarr
sprinkles ``if protocol == usenet`` across its download flow; we collapse those branches
behind a ``SourceStrategy`` so the orchestrator never branches on source.

Extracted in behaviour-preserving slices (each verbatim + suite-green):
- slice 1: ``search_and_score`` (find candidates for this source).
- slice 2a: ``import_files`` (per-file slskd import vs unpacked-folder Usenet import).
- slice 2b: ``enqueue`` (build + persist the manifest, hand off to the client).
Source-enablement stays on the orchestrator (it reads the live enable toggles). Later
slices fold in identity + blocklist-on-failure and the failover-loop source branches.
"""

import asyncio
import logging
import time
from pathlib import Path
from typing import Protocol, runtime_checkable

from models.download import ScoredCandidate, TargetAlbum, TargetTrack
from models.download_identity import soulseek_identity, usenet_identity
from models.download_manifest import DownloadManifest, ExpectedFile, ExpectedTrack
from repositories.protocols.download_client import (
    DownloadFileRef,
    EnqueueRequest,
    TaskHandle,
)
from services.native.acquisition.errors import OrchestrationError
from services.native.file_processor import (
    DOWNLOADS_MOUNT_UNAVAILABLE,
    QUARANTINE_REASONS,
    FileFailure,
    ProcessResult,
)

logger = logging.getLogger(__name__)

# Re-poll budget for an unpacked Usenet job folder, tolerating a separate-NFS-client
# directory-attribute cache lag (a shared client sees the move instantly).
_USENET_SETTLE_SECONDS = 20.0

# A SABnzbd failure mentioning one of these is a password-protected NZB - a non-retryable
# skip (blocklisted regardless of age, since propagation can't fix encryption).
_PASSWORD_MARKERS = ("password", "passworded", "encrypt")


async def _upgrade_held_tier(library, task) -> "str | None":  # noqa: ANN001
    """The held library tier an ``origin='upgrade'`` task must strictly beat, resolved
    with the right scope (D12): the recording's BEST copy for a per-track upgrade, the
    album's WORST tier otherwise. ``None`` for every non-upgrade origin - a retry of a
    partially-imported user download must NOT inherit a floor from the partial files,
    or the retry would reject the very candidates that complete the album."""
    if library is None or task.origin != "upgrade":
        return None
    if task.download_type == "track" and task.recording_mbid:
        return await library.recording_quality_tier(task.recording_mbid)
    if task.release_group_mbid:
        return await library.album_quality_tier(task.release_group_mbid)
    return None


@runtime_checkable
class SourceStrategy(Protocol):
    """One acquisition source's behaviour. The orchestrator holds a ``{name: strategy}``
    map and dispatches to it instead of branching on ``source``."""

    name: str
    # slskd applies the queued-peer timeout; SABnzbd queued/paused jobs move 0 bytes
    # legitimately, so Usenet sets this False (the deadline is its only backstop).
    applies_queued_timeout: bool
    # Whether this source can report a LOCAL disk/write fault on a terminal outcome (SABnzbd
    # does; slskd's only local fault is the downloads mount, handled via attempt_mount).
    has_local_disk_faults: bool

    @property
    def client(self):  # noqa: ANN201
        """The download client that owns this source's transfers."""
        ...

    def candidate_identity(self, candidate) -> str:  # noqa: ANN001
        """The failover-skip identity of a candidate (slskd peer username / Usenet
        title+size release identity)."""
        ...

    def is_cancelable(self, task, manifest) -> bool:  # noqa: ANN001
        """Whether this task has a stop-/cleanup-able handle."""
        ...

    def local_fault_message(self, attempt_mount: bool) -> str:
        """The user-facing 'we hit a local/environment fault' message for this source."""
        ...

    async def maybe_blocklist_on_failure(
        self, task, status, *, completed: bool, enumerated_any: bool  # noqa: ANN001
    ) -> None:
        """Blocklist a dead/under-delivering release before failover, the source's way
        (Usenet: age-guarded title+size identity; Soulseek: a no-op - its per-file
        quarantine already ran at import). The caller has already excluded local faults."""
        ...

    async def search_and_score(
        self, task, *, timeout: float, auto: float, manual: float  # noqa: ANN001
    ) -> list[ScoredCandidate]:
        """Search this source for ``task`` and return its scored candidates (best first)."""
        ...

    async def enqueue(
        self, task, candidate, *, strict_track_duration: bool, hold_on_wrong_track: bool = False  # noqa: ANN001
    ) -> None:
        """Build + persist the crash-recovery manifest, then hand the pick to the client.
        ``hold_on_wrong_track`` (the last-resort track re-pull, D9): a canonical-duration
        failure at import then holds the file for review instead of failing it."""
        ...

    async def import_files(
        self, task, manifest, *, only_filenames=None, completed: bool = False  # noqa: ANN001
    ) -> "tuple[ProcessResult, int]":
        """Import this task's downloaded files into the library; quarantine only files that
        arrived but failed verification. Returns ``(ProcessResult, audio_files_enumerated)``."""
        ...


class SoulseekStrategy:
    """slskd / Soulseek. Per-track grabs match a single track; albums match the folder."""

    name = "soulseek"
    applies_queued_timeout = True
    has_local_disk_faults = False  # slskd's only local fault is the downloads mount

    def __init__(  # noqa: ANN001
        self, *, indexer, scorer, track_matcher, client, store, file_processor,
        staging, manifest_codec, naming_template, library=None,
    ):
        self._indexer = indexer
        self._scorer = scorer
        self._track_matcher = track_matcher
        self._client = client
        self._store = store
        self._file_processor = file_processor
        self._staging = Path(staging)
        self._manifest_codec = manifest_codec
        self._naming_template = naming_template
        # Resolves the held tier an origin='upgrade' run must beat (upgrade-floor, D12).
        self._library = library

    @property
    def client(self):  # noqa: ANN201
        return self._client

    def candidate_identity(self, candidate) -> str:  # noqa: ANN001
        return candidate.username

    def is_cancelable(self, task, manifest) -> bool:  # noqa: ANN001
        # slskd is correlated by source_username + the manifest filenames, so a cancel needs both.
        return manifest.handle is not None and bool(task.source_username)

    def local_fault_message(self, attempt_mount: bool) -> str:  # noqa: ARG002
        # slskd's only local fault is an unreachable downloads mount (attempt_mount is True here).
        return DOWNLOADS_MOUNT_UNAVAILABLE

    async def maybe_blocklist_on_failure(self, task, status, *, completed, enumerated_any):  # noqa: ANN001, ANN201, ARG002
        # No-op: a failed slskd peer is quarantined per-file at IMPORT (see import_files);
        # there's no release-level blocklist to apply at failover time.
        return

    async def search_and_score(self, task, *, timeout, auto, manual):  # noqa: ANN001, ANN201
        held_tier = await _upgrade_held_tier(self._library, task)
        if task.download_type == "track":
            target = TargetTrack(
                artist_name=task.artist_name, track_title=task.track_title or "",
                album_title=task.album_title, duration_seconds=task.track_duration_seconds,
                recording_mbid=task.recording_mbid,
            )
            indexer_results = await self._indexer.search_track(
                task.artist_name, task.track_title or "", task.album_title, timeout=timeout
            )
            results = [r.soulseek for r in indexer_results if r.soulseek is not None]
            return await self._track_matcher.rank(
                target, results, auto_accept_threshold=auto, manual_threshold=manual,
                held_tier=held_tier,
            )
        # A 1-track release (a single requested as an album) scores per-file via the
        # track matcher, not the folder scorer: folder coherence hands a lone
        # fuzzy-matched file a perfect count_ratio, and only the per-file path carries
        # the canonical duration + the artist-evidence auto gate (2026-07-05
        # wrong-single incident). The SEARCH stays search_album (the album query
        # ladder) - its per-file results are exactly the track matcher's input shape.
        # Falls back to the folder scorer when identity threading failed (track_title
        # is None - MusicBrainz was down at request time).
        if task.track_count == 1 and task.track_title:
            target = TargetTrack(
                artist_name=task.artist_name, track_title=task.track_title,
                album_title=task.album_title, duration_seconds=task.track_duration_seconds,
                recording_mbid=task.recording_mbid,
            )
            indexer_results = await self._indexer.search_album(
                task.artist_name, task.album_title, task.year, task.track_count,
                timeout=timeout,
            )
            results = [r.soulseek for r in indexer_results if r.soulseek is not None]
            return await self._track_matcher.rank(
                target, results, auto_accept_threshold=auto, manual_threshold=manual,
                held_tier=held_tier,
            )
        target = TargetAlbum(
            artist_name=task.artist_name, album_title=task.album_title,
            year=task.year, track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
        )
        indexer_results = await self._indexer.search_album(
            task.artist_name, task.album_title, task.year, task.track_count, timeout=timeout
        )
        results = [r.soulseek for r in indexer_results if r.soulseek is not None]
        return await self._scorer.rank(
            target, results, auto_accept_threshold=auto, manual_threshold=manual,
            held_tier=held_tier,
        )

    async def enqueue(self, task, candidate, *, strict_track_duration, hold_on_wrong_track=False):  # noqa: ANN001, ANN201
        # For a per-track download - or a 1-track album (a single, whose identity was
        # threaded at request time) - verify the imported file against the CANONICAL
        # track length so a wrong-length recording fails over instead of being imported
        # and mislabelled (2026-07-05 wrong-single incident). The last-resort track
        # fallback keeps the gate ON but sets hold_on_wrong_track, so the closest match
        # is captured for human review rather than imported unverified (D9).
        is_single = task.download_type == "album" and task.track_count == 1
        use_canonical = (
            (task.download_type == "track" or is_single)
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
        # in-flight view, and not re-publishing it lets a 'retrying' status (set when we fail
        # over) persist through the next attempt instead of being clobbered.

        # Persist the manifest BEFORE enqueueing: it carries the correlation handle
        # (source + username + the enqueued filenames) so a restart can re-correlate.
        manifest = DownloadManifest(
            task_id=task.id,
            source_username=candidate.username,
            handle=TaskHandle(
                source="soulseek",
                username=candidate.username,
                filenames=[f.filename for f in candidate.files],
            ),
            origin=task.origin,
            release_group_mbid=task.release_group_mbid,
            release_mbid=task.release_mbid,
            artist_mbid=task.artist_mbid,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            is_track=use_canonical,
            hold_on_wrong_track=hold_on_wrong_track,
            naming_template=self._naming_template,
            target_files=[
                ExpectedFile(
                    filename=f.filename,
                    size=f.size,
                    duration=task.track_duration_seconds if use_canonical else f.duration,
                )
                for f in candidate.files
            ],
            # The expected track identity, when this download targets exactly one
            # known track (a track download or a 1-track single): arms the AcoustID
            # TITLE check and the import-time tag verification, which the per-file
            # slskd path otherwise runs artist-only (2026-07-05 wrong-single incident).
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
        self._staging.joinpath(task.id).mkdir(parents=True, exist_ok=True)
        (self._staging / task.id / "manifest.json").write_bytes(
            self._manifest_codec.encode(manifest)
        )

        try:
            await self._client.enqueue(
                EnqueueRequest(task_id=task.id, source="soulseek", files=files)
            )
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

    async def import_files(self, task, manifest, *, only_filenames=None, completed=False):  # noqa: ANN001, ANN201, ARG002
        # Per-file import: slskd wrote the exact files we enqueued; verify + place each.
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
                    source="soulseek",
                    identity=soulseek_identity(task.source_username or "", failure.filename),
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
        return result, len(result.succeeded) + len(result.failed)


class UsenetStrategy:
    """Newznab search + SABnzbd download. Always searches the ALBUM (a per-track grab
    fetches the album NZB, D4); imports the unpacked job folder against the MB tracklist."""

    name = "usenet"
    # SABnzbd Queued/Paused/post-processing jobs move 0 bytes legitimately, so they must NOT
    # accrue the queued-peer clock (the 6h deadline is the only backstop for a paused job).
    applies_queued_timeout = False
    has_local_disk_faults = True  # SABnzbd reports disk/write/permission errors

    def __init__(  # noqa: ANN001
        self, *, indexer, scorer, client, store, file_processor, import_settle_seconds,
        staging, manifest_codec, naming_template, album_service,
        category, priority, post_processing, min_release_age_seconds, library=None,
    ):
        self._indexer = indexer
        self._scorer = scorer
        self._client = client
        self._store = store
        self._file_processor = file_processor
        self._import_settle = import_settle_seconds
        self._staging = Path(staging)
        self._manifest_codec = manifest_codec
        self._naming_template = naming_template
        self._album_service = album_service
        # Resolves the held tier an origin='upgrade' run must beat (upgrade-floor, D12).
        self._library = library
        self._category = category
        self._priority = priority
        self._post_processing = post_processing
        self._min_release_age = min_release_age_seconds

    @property
    def client(self):  # noqa: ANN201
        return self._client

    def candidate_identity(self, candidate) -> str:  # noqa: ANN001
        if candidate.usenet_release is not None:
            return usenet_identity(candidate.usenet_release.title, candidate.usenet_release.size_bytes)
        return candidate.username

    def is_cancelable(self, task, manifest) -> bool:  # noqa: ANN001, ARG002
        # The client is correlated by the identifiers in the generic handle alone.
        return manifest.handle is not None

    def local_fault_message(self, attempt_mount: bool) -> str:
        return (
            "downloads directory not accessible - check the SABnzbd downloads mount"
            if attempt_mount
            else "SABnzbd reported a local disk/write error - will retry when it clears"
        )

    async def maybe_blocklist_on_failure(self, task, status, *, completed, enumerated_any):  # noqa: ANN001, ANN201
        """Blocklist a dead/under-delivering Usenet release by its title+size identity before
        failover (D11), mirroring Lidarr's blocklist-on-failed-import. Local faults are already
        filtered out by the caller. A password/encrypted release is a non-retryable skip.
        Propagation leniency (don't permanently blocklist a too-young release that may not have
        fully propagated) applies ONLY when the outcome is ambiguous - i.e. NOT a Completed job
        that enumerated files: such a job's content is present, so a shortfall is genuine
        under-delivery and propagation can't add more (review M1/H2)."""
        if task.search_job_id is None or task.candidate_index is None:
            return
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if not (0 <= task.candidate_index < len(candidates)):
            return
        release = candidates[task.candidate_index].usenet_release
        if release is None:
            return
        fail_message = ((status.error if status else "") or "").lower()
        is_password = any(m in fail_message for m in _PASSWORD_MARKERS)
        # Under-delivery is CONFIRMED only when SABnzbd completed AND files were present but
        # short. Otherwise (a failure, or a completed-but-empty folder) the cause is ambiguous
        # - propagation, a transient empty - so spare a too-young or undated release and let the
        # backoff'd auto-retry settle it (asymmetry favours not permanently killing a good
        # release; a missed dead one just costs one retry cycle).
        confirms_underdelivery = completed and enumerated_any
        if not is_password and not confirms_underdelivery:
            age = (time.time() - release.usenet_date) if release.usenet_date is not None else None
            if age is None or age < self._min_release_age:
                logger.info(
                    "download.usenet_propagation_skip",
                    extra={"task_id": task.id, "age_seconds": int(age) if age is not None else None},
                )
                return  # too young / undated - let the auto-retry try it again later
        # Honest reason: a Completed job whose files didn't satisfy the tracklist (a wrong or
        # short album - the Led Zeppelin debut matching every other LZ album) FAILED VERIFICATION
        # against the requested tracks; it is NOT a SABnzbd download failure. ``reason`` is
        # CHECK-constrained in the DB AND shown in the Quarantine panel, so it must stay in the
        # allowed vocabulary - "verify_failed" is the existing term for "downloaded but didn't
        # match", reusing the soulseek import-verify reasons.
        stored_reason = "verify_failed" if confirms_underdelivery else "download_failed"
        await self._store.record_quarantine(
            source="usenet",
            identity=usenet_identity(release.title, release.size_bytes),
            reason=stored_reason,
            release_group_mbid=task.release_group_mbid,
        )
        logger.info(
            "download.quarantined",
            extra={
                "task_id": task.id,
                "source": "usenet",
                "reason": "password" if is_password else stored_reason,
                "identity": usenet_identity(release.title, release.size_bytes),
            },
        )

    async def search_and_score(self, task, *, timeout, auto, manual):  # noqa: ANN001, ANN201
        # A track upgrade still fetches the album NZB (D4), but its floor is the
        # RECORDING's held tier - _upgrade_held_tier scopes by download_type.
        held_tier = await _upgrade_held_tier(self._library, task)
        target = TargetAlbum(
            artist_name=task.artist_name, album_title=task.album_title,
            year=task.year, track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
        )
        indexer_results = await self._indexer.search_album(
            task.artist_name, task.album_title, task.year, task.track_count, timeout=timeout
        )
        releases = [r.usenet for r in indexer_results if r.usenet is not None]
        return await self._scorer.rank(
            target, releases, auto_accept_threshold=auto,
            manual_threshold=manual, track_count=task.track_count,
            held_tier=held_tier,
        )

    async def enqueue(self, task, candidate, *, strict_track_duration, hold_on_wrong_track=False):  # noqa: ANN001, ANN201, ARG002
        # Hand the chosen album NZB to SABnzbd. The manifest carries the expected MB
        # tracklist (not pre-known filenames) - the folder import matches the unpacked files
        # to it (D18). For a per-track grab (D4) the tracklist is the single track.
        # hold_on_wrong_track is a slskd re-pull concern; the folder import has its own
        # per-track matcher, so it is accepted for protocol conformance and unused.
        release = candidate.usenet_release
        if release is None:
            raise OrchestrationError("usenet candidate has no release")
        use_canonical = (
            task.download_type == "track"
            and strict_track_duration
            and bool(task.track_duration_seconds)
        )
        expected_tracks = await self._expected_tracks(task)
        if not expected_tracks:
            raise OrchestrationError("could not resolve the album tracklist")
        # Unique per failover candidate: failover reuses the same task object (only
        # candidate_index advances), so a constant name collides with the prior attempt's
        # not-yet-deleted SABnzbd job and SAB appends .1/.2, orphaning unpacked folders on the
        # mount. The index makes each attempt individually addressable + cleanable.
        job_name = f"droppedneedle-{task.id}-{task.candidate_index or 0}"
        await self._store.update_status(
            task.id, "downloading", files_total=len(expected_tracks),
            total_size_bytes=release.size_bytes, started_at=time.time(),
        )
        manifest = DownloadManifest(
            task_id=task.id,
            handle=TaskHandle(source="usenet", job_name=job_name),
            origin=task.origin,
            release_group_mbid=task.release_group_mbid,
            release_mbid=task.release_mbid,
            artist_mbid=task.artist_mbid,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            is_track=use_canonical,
            naming_template=self._naming_template,
            target_files=[],
            expected_tracks=expected_tracks,
        )
        self._staging.joinpath(task.id).mkdir(parents=True, exist_ok=True)
        manifest_path = self._staging / task.id / "manifest.json"
        manifest_path.write_bytes(self._manifest_codec.encode(manifest))

        try:
            handle = await self._client.enqueue(
                EnqueueRequest(
                    task_id=task.id,
                    source="usenet",
                    nzb_url=release.nzb_url,
                    category=self._category,
                    job_name=job_name,
                    priority=self._priority,
                    post_processing=self._post_processing,
                )
            )
        except Exception as exc:  # noqa: BLE001 - any client error -> task failed
            logger.exception("Usenet enqueue failed for task %s", task.id)
            raise OrchestrationError("enqueue failed") from exc

        # Re-persist the manifest with the post-enqueue external identifier.
        manifest.handle = handle
        manifest_path.write_bytes(self._manifest_codec.encode(manifest))
        logger.info(
            "download.enqueued",
            extra={
                "task_id": task.id,
                "source": "usenet",
                "release_group_mbid": task.release_group_mbid,
                "job_name": job_name,
                "external_id": handle.external_id,
                "tracklist": len(expected_tracks),
                "total_size_bytes": release.size_bytes,
                "via_album_nzb": task.download_type == "track",
            },
        )

    async def import_files(self, task, manifest, *, only_filenames=None, completed=False):  # noqa: ANN001, ANN201, ARG002
        # Folder-based import (D18): enumerate the unpacked job folder and match the files
        # to the expected MB tracklist by tags/duration. A Usenet dead release is blocklisted
        # by identity in the failover loop (it can be a zero-file Failed that never reaches here).
        files = await self._client.list_completed_files(manifest.handle)
        if not files and completed:
            # A good release's files are present at Completed; re-poll briefly only to cover a
            # separate-NFS-client visibility lag before concluding empty. A failed job is
            # terminal (waiting won't add files), so don't settle.
            files = await self._settle_files(manifest.handle)
        enumerated = len(files)
        logger.info(
            "download.processing",
            extra={"task_id": task.id, "source": "usenet", "enumerated": enumerated},
        )
        if not files and completed:
            # No audio after settling on a Completed job. If the downloads MOUNT itself is
            # unreachable this is an ENVIRONMENT fault: don't blocklist, don't fail over. A
            # HEALTHY mount with an empty folder is a bad/garbage release -> fall through to the
            # empty import so the caller blocklists it.
            if not await self._client.downloads_mount_healthy():
                logger.warning("download.usenet_mount_unhealthy", extra={"task_id": task.id})
                return ProcessResult(
                    succeeded=[],
                    failed=[FileFailure(filename="", reason=DOWNLOADS_MOUNT_UNAVAILABLE)],
                ), enumerated
        result = await self._file_processor.process_downloaded_folder(manifest, files)
        if result.succeeded:
            await self._store.set_final_path(task.id, str(Path(result.succeeded[0]).parent))
        return result, enumerated

    async def _expected_tracks(self, task):  # noqa: ANN001, ANN201
        """The MB tracklist the folder-import matches files against (D18). For a per-track
        download (D4) it's the single requested track; for an album it's the full MB
        tracklist (durations in seconds, from MB milliseconds)."""
        if task.download_type == "track":
            return [
                ExpectedTrack(
                    track_number=task.track_number or 1,
                    disc_number=task.disc_number or 1,
                    duration_seconds=task.track_duration_seconds,
                    recording_mbid=task.recording_mbid,
                    title=task.track_title,
                )
            ]
        if self._album_service is None or not task.release_group_mbid:
            return []
        try:
            info = await self._album_service.get_album_tracks_info(task.release_group_mbid)
        except Exception:  # noqa: BLE001 - tracklist resolution must not crash the task
            logger.warning("Could not resolve MB tracklist for %s", task.release_group_mbid)
            return []
        return [
            ExpectedTrack(
                track_number=track.position,
                disc_number=track.disc_number or 1,
                duration_seconds=(track.length / 1000.0) if track.length else None,
                recording_mbid=track.recording_id,
                title=track.title,
            )
            for track in info.tracks
        ]

    async def _settle_files(self, handle):  # noqa: ANN001, ANN201
        """Re-poll the completed job's folder for audio, tolerating the window where a
        separate NFS client's directory-attribute cache delays visibility. Returns as soon
        as any file appears, else [] after polling for up to ``_USENET_SETTLE_SECONDS``."""
        interval = self._import_settle
        tries = max(1, int(_USENET_SETTLE_SECONDS / interval)) if interval > 0 else 5
        for _ in range(tries):
            if interval > 0:
                await asyncio.sleep(interval)
            files = await self._client.list_completed_files(handle)
            if files:
                return files
        return []


class TorrentStrategy:
    """Prowlarr (Torznab) search + a torrent-capable download client. Always searches the album (a
    per-track grab fetches the album torrent, mirroring Usenet D4); imports the completed
    torrent's folder against the MB tracklist.

    **Private-tracker seeding rule:** the torrent's payload must keep seeding after
    import, but ``FileProcessor`` MOVES its input files into the library. So
    ``import_files`` first COPIES the completed files into a per-task scratch dir under
    staging and hands the COPIES to the folder import - the torrent client's files are never
    touched, and the client adapter policy never deletes a completed torrent.
    """

    name = "torrent"
    # A 0-seeder/stalled torrent moves 0 bytes and nothing will change that (unlike a
    # queued SABnzbd job) - let the queued watchdog give up on it instead of the 6h ceiling.
    applies_queued_timeout = True
    has_local_disk_faults = False  # delegated clients report mount health separately

    def __init__(  # noqa: ANN001
        self, *, indexer, scorer, client, store, file_processor, import_settle_seconds,
        staging, manifest_codec, naming_template, album_service, library=None,
    ):
        self._indexer = indexer
        self._scorer = scorer
        self._client = client
        self._store = store
        self._file_processor = file_processor
        self._import_settle = import_settle_seconds
        self._staging = Path(staging)
        self._manifest_codec = manifest_codec
        self._naming_template = naming_template
        self._album_service = album_service
        self._library = library

    @property
    def client(self):  # noqa: ANN201
        return self._client

    def candidate_identity(self, candidate) -> str:  # noqa: ANN001
        if candidate.torrent_release is not None:
            return usenet_identity(
                candidate.torrent_release.title, candidate.torrent_release.size_bytes
            )
        return candidate.username

    def is_cancelable(self, task, manifest) -> bool:  # noqa: ANN001, ARG002
        # The concrete client owns correlation details inside the generic handle.
        return manifest.handle is not None

    def local_fault_message(self, attempt_mount: bool) -> str:  # noqa: ARG002
        return "downloads directory not accessible - check the torrent client downloads mount"

    async def maybe_blocklist_on_failure(self, task, status, *, completed, enumerated_any):  # noqa: ANN001, ANN201, ARG002
        """Blocklist a dead/under-delivering torrent by its title+size identity before
        failover. No propagation leniency (torrents don't propagate - a dead torrent
        stays dead), so both a hard failure and a confirmed under-delivery blocklist."""
        if task.search_job_id is None or task.candidate_index is None:
            return
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if not (0 <= task.candidate_index < len(candidates)):
            return
        release = candidates[task.candidate_index].torrent_release
        if release is None:
            return
        stored_reason = "verify_failed" if (completed and enumerated_any) else "download_failed"
        await self._store.record_quarantine(
            source="torrent",
            identity=usenet_identity(release.title, release.size_bytes),
            reason=stored_reason,
            release_group_mbid=task.release_group_mbid,
        )
        logger.info(
            "download.quarantined",
            extra={
                "task_id": task.id,
                "source": "torrent",
                "reason": stored_reason,
                "identity": usenet_identity(release.title, release.size_bytes),
            },
        )

    async def search_and_score(self, task, *, timeout, auto, manual):  # noqa: ANN001, ANN201
        held_tier = await _upgrade_held_tier(self._library, task)
        target = TargetAlbum(
            artist_name=task.artist_name, album_title=task.album_title,
            year=task.year, track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
        )
        indexer_results = await self._indexer.search_album(
            task.artist_name, task.album_title, task.year, task.track_count, timeout=timeout
        )
        releases = [r.torrent for r in indexer_results if r.torrent is not None]
        return await self._scorer.rank(
            target, releases, auto_accept_threshold=auto,
            manual_threshold=manual, track_count=task.track_count,
            held_tier=held_tier,
        )

    async def enqueue(self, task, candidate, *, strict_track_duration, hold_on_wrong_track=False):  # noqa: ANN001, ANN201, ARG002
        release = candidate.torrent_release
        if release is None:
            raise OrchestrationError("torrent candidate has no release")
        use_canonical = (
            task.download_type == "track"
            and strict_track_duration
            and bool(task.track_duration_seconds)
        )
        expected_tracks = await self._expected_tracks(task)
        if not expected_tracks:
            raise OrchestrationError("could not resolve the album tracklist")
        # Unique per failover candidate (same reasoning as the Usenet job_name).
        job_name = f"droppedneedle-{task.id}-{task.candidate_index or 0}"
        await self._store.update_status(
            task.id, "downloading", files_total=len(expected_tracks),
            total_size_bytes=release.size_bytes, started_at=time.time(),
        )
        manifest = DownloadManifest(
            task_id=task.id,
            handle=TaskHandle(source="torrent", job_name=job_name),
            origin=task.origin,
            release_group_mbid=task.release_group_mbid,
            release_mbid=task.release_mbid,
            artist_mbid=task.artist_mbid,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            is_track=use_canonical,
            naming_template=self._naming_template,
            target_files=[],
            expected_tracks=expected_tracks,
        )
        self._staging.joinpath(task.id).mkdir(parents=True, exist_ok=True)
        manifest_path = self._staging / task.id / "manifest.json"
        manifest_path.write_bytes(self._manifest_codec.encode(manifest))

        try:
            handle = await self._client.enqueue(
                EnqueueRequest(
                    task_id=task.id,
                    source="torrent",
                    magnet_uri=release.magnet_url or None,
                    torrent_url=release.download_url or None,
                    content_id=release.info_hash or None,
                    job_name=job_name,
                )
            )
        except Exception as exc:  # noqa: BLE001 - any client error -> task failed
            logger.exception("Torrent enqueue failed for task %s", task.id)
            raise OrchestrationError("enqueue failed") from exc

        # Re-persist the manifest with the client-owned external id filled in.
        manifest.handle = handle
        manifest_path.write_bytes(self._manifest_codec.encode(manifest))
        logger.info(
            "download.enqueued",
            extra={
                "task_id": task.id,
                "source": "torrent",
                "release_group_mbid": task.release_group_mbid,
                "job_name": job_name,
                "external_id": handle.external_id,
                "tracklist": len(expected_tracks),
                "total_size_bytes": release.size_bytes,
                "via_album_torrent": task.download_type == "track",
            },
        )

    async def import_files(self, task, manifest, *, only_filenames=None, completed=False):  # noqa: ANN001, ANN201, ARG002
        # Folder-based import (D18) with the seeding-safe COPY step: the file processor
        # MOVES its inputs into the library, so hand it copies, never the torrent client payload.
        files = await self._client.list_completed_files(manifest.handle)
        if not files and completed:
            files = await self._settle_files(manifest.handle)
        enumerated = len(files)
        logger.info(
            "download.processing",
            extra={"task_id": task.id, "source": "torrent", "enumerated": enumerated},
        )
        if not files and completed:
            if not await self._client.downloads_mount_healthy():
                logger.warning("download.torrent_mount_unhealthy", extra={"task_id": task.id})
                await asyncio.to_thread(self._cleanup_scratch, task.id)
                return ProcessResult(
                    succeeded=[],
                    failed=[FileFailure(filename="", reason=DOWNLOADS_MOUNT_UNAVAILABLE)],
                ), enumerated
        try:
            copies = await asyncio.to_thread(self._copy_for_import, task.id, files)
            result = await self._file_processor.process_downloaded_folder(manifest, copies)
            if result.succeeded:
                await self._store.set_final_path(
                    task.id, str(Path(result.succeeded[0]).parent)
                )
            return result, enumerated
        finally:
            await asyncio.to_thread(self._cleanup_scratch, task.id)

    def _copy_for_import(self, task_id: str, files: "list[Path]") -> "list[Path]":
        """Copy the torrent's completed audio files into a per-task scratch dir under
        staging, preserving structure relative to their common parent (multi-disc albums
        reuse filenames across disc folders). Sync I/O - the caller offloads it."""
        import os
        import shutil

        if not files:
            return []
        scratch = self._staging / task_id / "torrent-import"
        common = Path(os.path.commonpath([str(f.parent) for f in files]))
        out: list[Path] = []
        for src in files:
            try:
                rel = src.relative_to(common)
            except ValueError:
                rel = Path(src.name)
            dest = scratch / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            try:
                shutil.copy2(src, dest)
            except OSError as exc:
                logger.warning("torrent import copy failed for %s: %s", src.name, exc)
                continue
            out.append(dest)
        return out

    def _cleanup_scratch(self, task_id: str) -> None:
        """Best-effort removal of whatever the import left in the scratch dir (imported
        files were MOVED out of it; failures may leave copies behind)."""
        import shutil

        scratch = self._staging / task_id / "torrent-import"
        shutil.rmtree(scratch, ignore_errors=True)

    async def _expected_tracks(self, task):  # noqa: ANN001, ANN201
        """The MB tracklist the folder-import matches files against (same shape as
        ``UsenetStrategy._expected_tracks``)."""
        if task.download_type == "track":
            return [
                ExpectedTrack(
                    track_number=task.track_number or 1,
                    disc_number=task.disc_number or 1,
                    duration_seconds=task.track_duration_seconds,
                    recording_mbid=task.recording_mbid,
                    title=task.track_title,
                )
            ]
        if self._album_service is None or not task.release_group_mbid:
            return []
        try:
            info = await self._album_service.get_album_tracks_info(task.release_group_mbid)
        except Exception:  # noqa: BLE001 - tracklist resolution must not crash the task
            logger.warning("Could not resolve MB tracklist for %s", task.release_group_mbid)
            return []
        return [
            ExpectedTrack(
                track_number=track.position,
                disc_number=track.disc_number or 1,
                duration_seconds=(track.length / 1000.0) if track.length else None,
                recording_mbid=track.recording_id,
                title=track.title,
            )
            for track in info.tracks
        ]

    async def _settle_files(self, handle):  # noqa: ANN001, ANN201
        """Re-poll the completed torrent's folder for audio (NFS attribute-cache lag
        cover, same budget as the Usenet settle)."""
        interval = self._import_settle
        tries = max(1, int(_USENET_SETTLE_SECONDS / interval)) if interval > 0 else 5
        for _ in range(tries):
            if interval > 0:
                await asyncio.sleep(interval)
            files = await self._client.list_completed_files(handle)
            if files:
                return files
        return []


def _basename(filename: str) -> str:
    """Last path segment (slskd filenames use backslashes); log basenames not full peer
    paths to keep log lines free of identifying directory structure."""
    return filename.replace("\\", "/").rsplit("/", 1)[-1]
