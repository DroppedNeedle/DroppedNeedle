"""Source strategies (ArrRebuild step 4).

Each acquisition source (Soulseek via slskd, Usenet via Newznab+SABnzbd) differs in
search, identity, enqueue, poll→status, completed-file enumeration and cleanup. Lidarr
sprinkles ``if protocol == usenet`` across its download flow; we collapse those branches
behind a ``SourceStrategy`` so the orchestrator never branches on source. Source
enablement stays on the orchestrator (it reads the live enable toggles).
"""

import asyncio
from contextlib import suppress
import logging
import time
from pathlib import Path
from typing import Protocol, runtime_checkable

from models.acquisition_quality import AcquisitionQualitySnapshot
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
    _TAG_TITLE_WEAK,
)
from services.native.title_match import title_containment_score

logger = logging.getLogger(__name__)

# Re-poll budget for an unpacked Usenet job folder that EXISTS but is empty,
# tolerating slow mount-visibility lag (NFS/SMB attribute-cache delays can
# exceed the old 20s window). A missing folder never settles - it short-
# circuits to the remap-fault path in import_files.
_USENET_SETTLE_SECONDS = 60.0

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


async def _expected_tracks_for_task(  # noqa: ANN001, ANN201
    task, album_service, task_store=None
):
    """Return ``(release_mbid, exact tracks)`` for one durable task identity."""

    if task.download_type == "track" or (
        task.track_count == 1
        and task.release_mbid
        and task.release_track_mbid
        and task.recording_mbid
        and task.track_number
    ):
        if (
            not task.release_mbid
            or not task.release_track_mbid
            or not task.recording_mbid
            or not task.track_number
        ):
            return task.release_mbid, []
        return task.release_mbid, [
            ExpectedTrack(
                track_number=task.track_number,
                disc_number=task.disc_number or 1,
                duration_seconds=task.track_duration_seconds,
                recording_mbid=task.recording_mbid,
                title=task.track_title,
                release_track_mbid=task.release_track_mbid,
            )
        ]
    if album_service is None or not task.release_group_mbid:
        return task.release_mbid, []
    try:
        if task.release_mbid:
            info = await album_service.get_exact_edition_tracks_info(
                task.release_group_mbid,
                task.release_mbid,
            )
        else:
            # Upgrade compatibility for a queued pre-identity task: resolve once at
            # manifest creation. New tasks always arrive with release_mbid pinned.
            info = await album_service.get_album_tracks_info(task.release_group_mbid)
    except Exception as error:  # noqa: BLE001 - no exact proof means no enqueue
        raise OrchestrationError("could not verify the exact album edition") from error
    expected = [
        ExpectedTrack(
            track_number=track.position,
            disc_number=track.disc_number or 1,
            duration_seconds=(track.length / 1000.0) if track.length else None,
            recording_mbid=track.recording_id,
            title=track.title,
            release_track_mbid=track.release_track_id,
        )
        for track in info.tracks
    ]
    if (
        not expected
        or any(
            not value.recording_mbid
            or not value.release_track_mbid
            or value.track_number < 1
            or value.disc_number < 1
            for value in expected
        )
        or len({(value.disc_number, value.track_number) for value in expected})
        != len(expected)
        or len({str(value.release_track_mbid).casefold() for value in expected})
        != len(expected)
    ):
        raise OrchestrationError("the exact album edition has an incomplete track map")
    selected_release = task.release_mbid or info.selected_release_mbid
    if not task.release_mbid and selected_release and task_store is not None:
        try:
            selected_release = await task_store.pin_task_release_mbid(
                task.id, task.release_group_mbid, selected_release
            )
        except Exception as error:  # noqa: BLE001 - edition drift must fail closed
            raise OrchestrationError(
                "could not pin the exact album edition to this download"
            ) from error
    return selected_release, expected


def _file_serves_expected(value, tracks) -> bool:  # noqa: ANN001
    """Whether one search-result file plausibly serves any of ``tracks`` - the
    per-file failover filter (#292). Search-side evidence only (filename stem +
    advertised duration), judged by the SAME thresholds the post-download matcher
    applies (coverage's ``row_covers_track`` rules): containment-strong title,
    duration within max(15s, 10%). Evidence is tri-state per track: a disagreeing
    title only excludes when duration cannot rescue it (peer paths like ``02.flac``
    carry no title signal), a hard duration miss always excludes, and a track with
    no usable signal cannot be discriminated - its files pass rather than strand
    the position on every candidate."""
    stem = value.filename.replace("\\", "/").rsplit("/", 1)[-1]
    base, dot, _ext = stem.rpartition(".")
    if dot and base:
        stem = base
    for track in tracks:
        title_ok = None
        if track.title and stem:
            title_ok = title_containment_score(track.title, stem) >= _TAG_TITLE_WEAK
        duration_ok = None
        if track.duration_seconds and value.duration:
            duration_ok = abs(value.duration - track.duration_seconds) <= max(
                15.0, 0.10 * track.duration_seconds
            )
        if duration_ok is False:
            continue
        if title_ok is False and duration_ok is not True:
            continue
        return True
    return False


def pre_publication_quality_check(
    task,
    candidate,
    files,
    tagger,
):  # noqa: ANN001
    """UNCONDITIONAL local verification before publication (Acquisition plan):
    probe the downloaded bytes through the codec-aware tagger and re-run the
    task's STORED snapshot evaluation on measured facts. Returns a mismatch
    dict, or None when the quality is acceptable / cannot be judged (no stored
    snapshot, no tagger wired - tests - or unreadable files)."""
    if tagger is None or not files:
        return None
    raw = getattr(task, "quality_snapshot_json", None)
    if raw is None:
        return None

    from services.native.acquisition import quality as acq_quality

    try:
        snapshot = acq_quality.decode_snapshot(raw)
    except acq_quality.SnapshotValidationError:
        return {
            "reason": "post_download_quality_mismatch",
            "detail": "Stored quality policy snapshot is invalid.",
        }

    from services.native.acquisition.local_probe import (
        expected_vs_actual_copy,
        probe_files_sync,
        quality_mismatch,
    )

    try:
        probed = probe_files_sync(files, tagger)
    except Exception:  # noqa: BLE001 - an unreadable byte never blocks publication
        return None
    decision = getattr(candidate, "quality_decision", None)
    if quality_mismatch(snapshot, decision, probed):
        return {
            "reason": "post_download_quality_mismatch",
            "detail": expected_vs_actual_copy(decision, probed),
        }
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

    def local_fault_message(self, attempt_mount: bool) -> str:
        """The user-facing 'we hit a local/environment fault' message for this source."""
        ...

    async def maybe_blocklist_on_failure(
        self,
        task,
        status,
        *,
        completed: bool,
        enumerated_any: bool,  # noqa: ANN001
    ) -> None:
        """Blocklist a dead/under-delivering release before failover, the source's way
        (Usenet: age-guarded title+size identity; Soulseek: a no-op - its per-file
        quarantine already ran at import). The caller has already excluded local faults."""
        ...

    async def search_and_score(
        self,
        task,
        *,
        timeout: float,
        auto: float,
        manual: float,  # noqa: ANN001
        snapshot: AcquisitionQualitySnapshot,
    ) -> list[ScoredCandidate]:
        """Search this source for ``task`` and return its scored candidates
        ranked under the task's immutable quality snapshot (best first)."""
        ...

    async def enqueue(
        self,
        task,
        candidate,
        *,
        strict_track_duration: bool,
        hold_on_wrong_track: bool = False,  # noqa: ANN001
        remaining_positions: "frozenset[tuple[int, int]] | None" = None,
    ) -> None:
        """Build + persist the crash-recovery manifest, then hand the pick to the client.
        ``hold_on_wrong_track`` (the last-resort track re-pull, D9): a canonical-duration
        failure at import then holds the file for review instead of failing it.
        ``remaining_positions`` (per-file failover, #292): when given, ask ONLY for the
        still-missing (disc, track) positions instead of the whole album; Usenet ignores
        it (an NZB is the smallest addressable unit)."""
        ...

    async def import_files(
        self,
        task,
        manifest,
        *,
        only_filenames=None,
        completed: bool = False,  # noqa: ANN001
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
        self,
        *,
        indexer,
        scorer,
        track_matcher,
        client,
        store,
        file_processor,
        staging,
        manifest_codec,
        naming_template,
        library=None,
        album_service=None,
        policy_extras=None,  # Callable[[], SpecPolicy | None]: live NON-quality gates
        probe_tagger=None,  # AudioTagger for the pre-publication quality probe
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
        self._album_service = album_service
        self._policy_extras = policy_extras
        self._probe_tagger = probe_tagger

    @property
    def client(self):  # noqa: ANN201
        return self._client

    def candidate_identity(self, candidate) -> str:  # noqa: ANN001
        return candidate.username

    def _extras(self):
        return self._policy_extras() if self._policy_extras is not None else None

    def local_fault_message(self, attempt_mount: bool) -> str:  # noqa: ARG002
        # slskd's only local fault is an unreachable downloads mount (attempt_mount is True here).
        return DOWNLOADS_MOUNT_UNAVAILABLE

    async def maybe_blocklist_on_failure(
        self, task, status, *, completed, enumerated_any
    ):  # noqa: ANN001, ANN201, ARG002
        # No-op: a failed slskd peer is quarantined per-file at IMPORT (see import_files);
        # there's no release-level blocklist to apply at failover time.
        return

    async def search_and_score(self, task, *, timeout, auto, manual, snapshot):  # noqa: ANN001, ANN201
        held_tier = await _upgrade_held_tier(self._library, task)
        extras = self._extras()
        if task.download_type == "track":
            target = TargetTrack(
                artist_name=task.artist_name,
                track_title=task.track_title or "",
                album_title=task.album_title,
                duration_seconds=task.track_duration_seconds,
                recording_mbid=task.recording_mbid,
            )
            indexer_results = await self._indexer.search_track(
                task.artist_name,
                task.track_title or "",
                task.album_title,
                timeout=timeout,
            )
            results = [r.soulseek for r in indexer_results if r.soulseek is not None]
            return await self._track_matcher.rank(
                target,
                results,
                snapshot=snapshot,
                auto_accept_threshold=auto,
                manual_threshold=manual,
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
                artist_name=task.artist_name,
                track_title=task.track_title,
                album_title=task.album_title,
                duration_seconds=task.track_duration_seconds,
                recording_mbid=task.recording_mbid,
            )
            indexer_results = await self._indexer.search_album(
                task.artist_name,
                task.album_title,
                task.year,
                task.track_count,
                timeout=timeout,
            )
            results = [r.soulseek for r in indexer_results if r.soulseek is not None]
            return await self._track_matcher.rank(
                target,
                results,
                snapshot=snapshot,
                auto_accept_threshold=auto,
                manual_threshold=manual,
                held_tier=held_tier,
            )
        target = TargetAlbum(
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
        )
        indexer_results = await self._indexer.search_album(
            task.artist_name,
            task.album_title,
            task.year,
            task.track_count,
            timeout=timeout,
        )
        results = [r.soulseek for r in indexer_results if r.soulseek is not None]
        return await self._scorer.rank(
            target,
            results,
            snapshot=snapshot,
            spec_extras=extras,
            auto_accept_threshold=auto,
            manual_threshold=manual,
            held_tier=held_tier,
        )

    async def enqueue(
        self,
        task,
        candidate,
        *,
        strict_track_duration,
        hold_on_wrong_track=False,
        remaining_positions=None,
    ):  # noqa: ANN001, ANN201
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
        release_mbid, expected_tracks = await _expected_tracks_for_task(
            task, self._album_service, self._store
        )
        if not expected_tracks and (
            self._album_service is not None or task.release_mbid is not None
        ):
            raise OrchestrationError("could not resolve the exact album tracklist")

        # Per-file failover (#292): when the orchestrator says only some (disc, track)
        # positions are still missing, ask this peer for JUST those - expected_tracks
        # shrinks to them, and only files that plausibly serve one get enqueued. slskd
        # keeps partial bytes per file, so re-requesting a missing track RESUMES its
        # earlier errored attempt instead of starting over.
        serving = candidate.files
        if remaining_positions is not None:
            expected_tracks = [
                value
                for value in expected_tracks
                if (value.disc_number or 1, value.track_number) in remaining_positions
            ]
            if not expected_tracks:
                raise OrchestrationError("nothing left to acquire from this source")
            serving = [
                value
                for value in candidate.files
                if _file_serves_expected(value, expected_tracks)
            ]

        files = [
            DownloadFileRef(
                username=candidate.username, filename=f.filename, size=f.size
            )
            for f in serving
        ]
        total_size = sum(f.size for f in serving)
        await self._store.update_status(
            task.id,
            "downloading",
            files_total=len(files),
            total_size_bytes=total_size,
            started_at=time.time(),
        )
        # No 'downloading' SSE status here: the UI reads the polled task.status for the
        # in-flight view, and not re-publishing it lets a 'retrying' status (set when we fail
        # over) persist through the next attempt instead of being clobbered.

        # Persist the manifest BEFORE enqueueing: it carries the correlation handle
        # (source + username + the enqueued filenames) so a restart can re-correlate.
        initial_handle = TaskHandle(
            source="soulseek",
            username=candidate.username,
            filenames=[f.filename for f in serving],
        )
        attempt = await self._store.create_download_attempt(
            task_id=task.id,
            source="soulseek",
            candidate_index=task.candidate_index or 0,
            job_name="",
            handle=initial_handle,
        )
        manifest = DownloadManifest(
            task_id=task.id,
            source_username=candidate.username,
            handle=initial_handle,
            origin=task.origin,
            release_group_mbid=task.release_group_mbid,
            release_mbid=release_mbid,
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
                    duration=task.track_duration_seconds
                    if use_canonical
                    else f.duration,
                )
                for f in serving
            ],
            # Complete selected-edition map. Transfer filenames remain the correlation
            # keys; import maps each audio file to one exact release position and then
            # verifies its recording/title/duration evidence.
            expected_tracks=expected_tracks,
            attempt_id=attempt.id,
        )
        self._staging.joinpath(task.id).mkdir(parents=True, exist_ok=True)
        (self._staging / task.id / "manifest.json").write_bytes(
            self._manifest_codec.encode(manifest)
        )

        try:
            handle = await self._client.enqueue(
                EnqueueRequest(task_id=task.id, source="soulseek", files=files)
            )
        except Exception as exc:  # noqa: BLE001 - any client error -> task failed
            # Per review-triage: do NOT quarantine on enqueue failure (nothing was
            # downloaded). The safe runner / process_task persists the sanitized msg.
            logger.exception("Enqueue failed for task %s", task.id)
            raise OrchestrationError("enqueue failed") from exc

        await self._store.update_download_attempt_handle(attempt.id, handle)
        manifest.handle = handle
        (self._staging / task.id / "manifest.json").write_bytes(
            self._manifest_codec.encode(manifest)
        )

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

    async def import_files(
        self, task, manifest, *, only_filenames=None, completed=False
    ):  # noqa: ANN001, ANN201, ARG002
        # Per-file import: slskd wrote the exact files we enqueued; verify + place each.
        logger.info(
            "download.processing",
            extra={"task_id": task.id, "files_total": len(manifest.target_files)},
        )
        # UNCONDITIONAL local quality probe before publication handoff.
        if self._probe_tagger is not None:
            local_paths: list = []
            for target_file in manifest.target_files:
                with suppress(Exception):
                    resolved = await self._client.get_file_path(
                        getattr(manifest, "handle", None), target_file.filename
                    )
                    if resolved is not None:
                        local_paths.append(resolved)
            candidate = await self._current_candidate(task)
            mismatch = pre_publication_quality_check(
                task, candidate, local_paths, self._probe_tagger
            )
            if mismatch is not None:
                logger.info(
                    "download.quality_mismatch",
                    extra={
                        "task_id": task.id,
                        **{k: v for k, v in mismatch.items() if k != "probed"},
                    },
                )
                failed = [
                    FileFailure(filename=f.filename, reason=mismatch["reason"])
                    for f in manifest.target_files
                ]
                return ProcessResult(succeeded=[], failed=failed), len(failed)

        result = await self._file_processor.process_downloaded(
            manifest, only_filenames=only_filenames
        )
        for failure in result.failed:
            # ``tag_mismatch`` is intentionally kept on the returned ProcessResult so
            # callers can report the truthful content-verification outcome. The
            # quarantine table's existing CHECK vocabulary predates this reason, so
            # persist it as the equivalent ``verify_failed`` source exclusion without
            # changing the failure surfaced to the orchestrator or held-import UI.
            quarantine_reason = (
                "verify_failed" if failure.reason == "tag_mismatch" else failure.reason
            )
            if quarantine_reason in QUARANTINE_REASONS:
                await self._store.record_quarantine(
                    source="soulseek",
                    identity=soulseek_identity(
                        task.source_username or "", failure.filename
                    ),
                    reason=quarantine_reason,
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
            await self._store.set_final_path(
                task.id, str(Path(result.succeeded[0]).parent)
            )
        return result, len(result.succeeded) + len(result.failed)

    async def _current_candidate(self, task):  # noqa: ANN001
        """The selected candidate blob for this task (None when unlinked)."""
        try:
            if task.search_job_id is None or task.candidate_index is None:
                return None
            candidates = await self._store.get_search_job_candidates(task.search_job_id)
            if 0 <= task.candidate_index < len(candidates):
                return candidates[task.candidate_index]
        except Exception:  # noqa: BLE001 - probe path must fail open
            return None
        return None


class UsenetStrategy:
    """Newznab search + SABnzbd download. Always searches the ALBUM (a per-track grab
    fetches the album NZB, D4); imports the unpacked job folder against the MB tracklist."""

    name = "usenet"
    # SABnzbd Queued/Paused/post-processing jobs move 0 bytes legitimately, so they must NOT
    # accrue the queued-peer clock (the 6h deadline is the only backstop for a paused job).
    applies_queued_timeout = False
    has_local_disk_faults = True  # SABnzbd reports disk/write/permission errors

    def __init__(  # noqa: ANN001
        self,
        *,
        indexer,
        scorer,
        client,
        store,
        file_processor,
        import_settle_seconds,
        staging,
        manifest_codec,
        naming_template,
        album_service,
        category,
        priority,
        post_processing,
        min_release_age_seconds,
        library=None,
        policy_extras=None,
        probe_tagger=None,
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
        self._policy_extras = policy_extras
        self._probe_tagger = probe_tagger

    @property
    def client(self):  # noqa: ANN201
        return self._client

    def candidate_identity(self, candidate) -> str:  # noqa: ANN001
        if candidate.usenet_release is not None:
            return usenet_identity(
                candidate.usenet_release.title, candidate.usenet_release.size_bytes
            )
        return candidate.username

    def local_fault_message(self, attempt_mount: bool) -> str:
        return (
            "downloads directory not accessible - check the SABnzbd downloads mount"
            if attempt_mount
            else "SABnzbd reported a local disk/write error - will retry when it clears"
        )

    async def maybe_blocklist_on_failure(
        self, task, status, *, completed, enumerated_any
    ):  # noqa: ANN001, ANN201
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
            age = (
                (time.time() - release.usenet_date)
                if release.usenet_date is not None
                else None
            )
            if age is None or age < self._min_release_age:
                logger.info(
                    "download.usenet_propagation_skip",
                    extra={
                        "task_id": task.id,
                        "age_seconds": int(age) if age is not None else None,
                    },
                )
                return  # too young / undated - let the auto-retry try it again later
        # Honest reason: a Completed job whose files didn't satisfy the tracklist (a wrong or
        # short album - the Led Zeppelin debut matching every other LZ album) FAILED VERIFICATION
        # against the requested tracks; it is NOT a SABnzbd download failure. ``reason`` is
        # CHECK-constrained in the DB AND shown in the Quarantine panel, so it must stay in the
        # allowed vocabulary - "verify_failed" is the existing term for "downloaded but didn't
        # match", reusing the soulseek import-verify reasons.
        if completed and not enumerated_any and await self._completed_folder_missing(task):
            # A Completed job whose folder is missing on the mount is a
            # storage-remap fault (Windows backslashes, category-subfolder
            # mounts), not a dead release: never blocklist it. The workspace is
            # preserved by the import path, so a later reimport can still
            # recover the files (#245).
            logger.info(
                "download.usenet_remap_skip",
                extra={"task_id": task.id},
            )
            return
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

    async def search_and_score(self, task, *, timeout, auto, manual, snapshot):  # noqa: ANN001, ANN201
        # A track upgrade still fetches the album NZB (D4), but its floor is the
        # RECORDING's held tier - _upgrade_held_tier scopes by download_type.
        held_tier = await _upgrade_held_tier(self._library, task)
        extras = self._policy_extras() if self._policy_extras else None
        target = TargetAlbum(
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
        )
        indexer_results = await self._indexer.search_album(
            task.artist_name,
            task.album_title,
            task.year,
            task.track_count,
            timeout=timeout,
        )
        releases = [r.usenet for r in indexer_results if r.usenet is not None]
        return await self._scorer.rank(
            target,
            releases,
            snapshot=snapshot,
            spec_extras=extras,
            auto_accept_threshold=auto,
            manual_threshold=manual,
            track_count=task.track_count,
            held_tier=held_tier,
        )

    async def enqueue(
        self,
        task,
        candidate,
        *,
        strict_track_duration,
        hold_on_wrong_track=False,
        remaining_positions=None,
    ):  # noqa: ANN001, ANN201, ARG002
        # Hand the chosen album NZB to SABnzbd. The manifest carries the expected MB
        # tracklist (not pre-known filenames) - the folder import matches the unpacked files
        # to it (D18). For a per-track grab (D4) the tracklist is the single track.
        # hold_on_wrong_track is a slskd re-pull concern; the folder import has its own
        # per-track matcher, so it is accepted for protocol conformance and unused.
        # remaining_positions is likewise accepted-and-ignored (#292): an NZB is the
        # smallest addressable Usenet unit, so failover stays whole-album here; the
        # release-level blocklist above already stops re-grabbing an under-delivering
        # release.
        release = candidate.usenet_release
        if release is None:
            raise OrchestrationError("usenet candidate has no release")
        use_canonical = (
            task.download_type == "track"
            and strict_track_duration
            and bool(task.track_duration_seconds)
        )
        release_mbid, expected_tracks = await _expected_tracks_for_task(
            task, self._album_service, self._store
        )
        if not expected_tracks:
            raise OrchestrationError("could not resolve the album tracklist")
        # Unique per failover candidate: failover reuses the same task object (only
        # candidate_index advances), so a constant name collides with the prior attempt's
        # not-yet-deleted SABnzbd job and SAB appends .1/.2, orphaning unpacked folders on the
        # mount. The index makes each attempt individually addressable + cleanable.
        job_name = f"droppedneedle-{task.id}-{task.candidate_index or 0}"
        await self._store.update_status(
            task.id,
            "downloading",
            files_total=len(expected_tracks),
            total_size_bytes=release.size_bytes,
            started_at=time.time(),
        )
        initial_handle = TaskHandle(source="usenet", job_name=job_name)
        attempt = await self._store.create_download_attempt(
            task_id=task.id,
            source="usenet",
            candidate_index=task.candidate_index or 0,
            job_name=job_name,
            handle=initial_handle,
        )
        manifest = DownloadManifest(
            task_id=task.id,
            handle=initial_handle,
            origin=task.origin,
            release_group_mbid=task.release_group_mbid,
            release_mbid=release_mbid,
            artist_mbid=task.artist_mbid,
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            is_track=use_canonical,
            naming_template=self._naming_template,
            target_files=[],
            expected_tracks=expected_tracks,
            attempt_id=attempt.id,
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
                    job_name=job_name,
                    category=self._category,
                    priority=self._priority,
                    post_processing=self._post_processing,
                )
            )
        except Exception as exc:  # noqa: BLE001 - any client error -> task failed
            logger.exception("Usenet enqueue failed for task %s", task.id)
            raise OrchestrationError("enqueue failed") from exc

        # SQLite first: the journal closes a crash before the manifest rewrite.
        await self._store.update_download_attempt_handle(attempt.id, handle)
        # Re-persist the manifest with the nzo_id filled in (the post-enqueue batch id).
        manifest.handle = handle
        manifest_path.write_bytes(self._manifest_codec.encode(manifest))
        logger.info(
            "download.enqueued",
            extra={
                "task_id": task.id,
                "source": "usenet",
                "release_group_mbid": task.release_group_mbid,
                "job_name": job_name,
                "nzo_id": handle.nzo_id,
                "tracklist": len(expected_tracks),
                "total_size_bytes": release.size_bytes,
                "via_album_nzb": task.download_type == "track",
            },
        )

    async def import_files(
        self, task, manifest, *, only_filenames=None, completed=False
    ):  # noqa: ANN001, ANN201, ARG002
        # Folder-based import (D18): enumerate the unpacked job folder and match the files
        # to the expected MB tracklist by tags/duration. A Usenet dead release is blocklisted
        # by identity in the failover loop (it can be a zero-file Failed that never reaches here).
        files = await self._client.list_completed_files(manifest.handle)
        if not files and completed:
            # An empty first enumeration is either mount-visibility lag or a
            # storage-remap fault. A missing folder short-circuits to the
            # remap-fault path (no settle can fix a path that resolves
            # nowhere); an existing-but-empty folder settles first (#245).
            if await self._completed_folder_missing_handle(manifest.handle):
                logger.warning(
                    "download.usenet_folder_missing",
                    extra={"task_id": task.id},
                )
                return ProcessResult(
                    succeeded=[],
                    failed=[],
                    workspace_disposition="preserve",
                ), 0
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
                logger.warning(
                    "download.usenet_mount_unhealthy", extra={"task_id": task.id}
                )
                return ProcessResult(
                    succeeded=[],
                    failed=[
                        FileFailure(filename="", reason=DOWNLOADS_MOUNT_UNAVAILABLE)
                    ],
                ), enumerated
        # UNCONDITIONAL local quality probe before publication handoff.
        if self._probe_tagger is not None:
            candidate = None
            try:
                if task.search_job_id is not None and task.candidate_index is not None:
                    candidates = await self._store.get_search_job_candidates(
                        task.search_job_id
                    )
                    if 0 <= task.candidate_index < len(candidates):
                        candidate = candidates[task.candidate_index]
            except Exception:  # noqa: BLE001 - probe fails open
                candidate = None
            mismatch = pre_publication_quality_check(
                task, candidate, list(files), self._probe_tagger
            )
            if mismatch is not None:
                logger.info(
                    "download.quality_mismatch",
                    extra={
                        "task_id": task.id,
                        "detail": mismatch.get("detail", "")[:200],
                    },
                )
                return (
                    ProcessResult(
                        succeeded=[],
                        failed=[
                            FileFailure(
                                filename=str(f),
                                reason=mismatch["reason"],
                            )
                            for f in files
                        ],
                    ),
                    enumerated,
                )

        result = await self._file_processor.process_downloaded_folder(manifest, files)
        if result.succeeded:
            await self._store.set_final_path(
                task.id, str(Path(result.succeeded[0]).parent)
            )
        if not files and completed:
            # Ambiguous empty after settle on a reachable mount (remap fault or
            # slow visibility): never discard the user's completed folder -
            # preserve it so a later reimport can still find the files. The 6h
            # orphan reconcile is the terminal state for genuinely dead folders
            # (#245). (The mount-unreachable path returned above.)
            result = ProcessResult(
                succeeded=list(result.succeeded),
                failed=list(result.failed),
                publisher_bundle_ids=list(result.publisher_bundle_ids),
                workspace_disposition="preserve",
            )
        return result, enumerated

    async def _completed_folder_missing(self, task) -> bool:  # noqa: ANN001
        """Whether the Completed job's folder is missing on the mount (or its
        storage unresolvable) - the remap-fault signal (#245)."""
        try:
            attempt = await self._store.get_download_attempt_for_candidate(
                task.id, "usenet", task.candidate_index
            )
        except Exception:  # noqa: BLE001 - journal trouble reads as missing (safe path)
            return True
        handle = attempt.handle if attempt is not None else None
        return await self._completed_folder_missing_handle(handle)

    async def _completed_folder_missing_handle(self, handle) -> bool:  # noqa: ANN001
        """Handle-level half of :meth:`_completed_folder_missing`, shared with the
        import path (which already holds the manifest handle).

        Resolved through the exact-path evidence: when the exact path is
        unresolvable but the hardened remap could still apply, this reads
        missing and the caller takes the safe path (preserve, no blocklist) -
        the fail-safe direction. An inspect failure also reads missing.
        """
        if handle is None or not (handle.job_name or handle.nzo_id):
            return True
        try:
            material = await self._client.inspect_materialization(handle)
        except Exception:  # noqa: BLE001 - diagnostic failure reads as missing
            return True
        workspace = material.workspace_path or ""
        if not workspace:
            return True
        try:
            return not await asyncio.to_thread(Path(workspace).is_dir)
        except OSError:
            return True

    async def _settle_files(self, handle):  # noqa: ANN001, ANN201
        """Re-poll the completed job's existing-but-empty folder for audio,
        tolerating slow mount-visibility lag. Returns once the enumerated set is
        stable across two polls (a partially-materialised folder must not import
        short), else whatever the last poll saw after polling for up to
        ``_USENET_SETTLE_SECONDS``."""
        interval = self._import_settle
        tries = max(2, int(_USENET_SETTLE_SECONDS / interval)) if interval > 0 else 5
        previous: list[str] = []
        files: list[Path] = []
        for _ in range(tries):
            if interval > 0:
                await asyncio.sleep(interval)
            files = await self._client.list_completed_files(handle)
            current = sorted(str(p) for p in files)
            if current and current == previous:
                return files
            previous = current
        return files


def _basename(filename: str) -> str:
    """Last path segment (slskd filenames use backslashes); log basenames not full peer
    paths to keep log lines free of identifying directory structure."""
    return filename.replace("\\", "/").rsplit("/", 1)[-1]
