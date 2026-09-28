"""Plugin source strategy (v1 acquisition API).

One instance per plugin source key (``plugin:<name>``). Collapses the
Soulseek/Usenet branches behind ``SourceStrategy`` for a plugin backend:
album search + release scoring through :class:`PluginReleaseScorer`, per-file
track/single scoring through the shared ``TrackMatcher``, exact-file enqueue
(files mode) or whole-release enqueue (folder mode), and the matching import
path per mode.
"""

import asyncio
import logging
import re
import time
from contextlib import suppress
from pathlib import Path

from models.download import ScoredCandidate, TargetAlbum, TargetTrack
from models.download_manifest import DownloadManifest, ExpectedFile
from repositories.protocols.download_client import (
    DownloadFileRef,
    DownloadSearchResult,
    EnqueueRequest,
    TaskHandle,
)
from repositories.protocols.indexer import PluginSearchResult
from services.native.acquisition.errors import OrchestrationError
from services.native.acquisition.strategy import (
    _expected_tracks_for_task,
    _upgrade_held_tier,
    pre_publication_quality_check,
)
from services.native.file_processor import (
    DOWNLOADS_MOUNT_UNAVAILABLE,
    QUARANTINE_REASONS,
    FileFailure,
    ProcessResult,
)

logger = logging.getLogger(__name__)

try:
    from models.download_identity import plugin_identity as _plugin_identity
except Exception:  # noqa: BLE001 - ModelsCheck lands the helper alongside this file
    _plugin_identity = None  # type: ignore[assignment]


def _plugin_key(result: PluginSearchResult) -> str:
    """Quarantine key for one plugin result: the opaque payload when set,
    else the normalised-title + size-MB fallback."""
    payload = (result.payload or "").strip()
    if payload:
        return payload
    norm = re.sub(r"\s+", " ", (result.title or "").strip().lower())
    size_mb = (result.size_bytes or 0) // (1024 * 1024)
    return f"{norm}\x1f{size_mb}"


def _plugin_result_identity(source_key: str, result: PluginSearchResult) -> str:
    if _plugin_identity is not None:
        return _plugin_identity(source_key, _plugin_key(result))
    return f"{source_key}\x1f{_plugin_key(result)}"


def _shim_search_result(
    source_key: str, release: PluginSearchResult, ref: DownloadFileRef
) -> DownloadSearchResult:
    """Project one plugin file ref onto the track matcher's input shape."""
    return DownloadSearchResult(
        username=source_key,
        filename=ref.filename,
        parent_directory=release.title,
        size=ref.size,
        extension=_ext_from_filename(ref.filename),
    )


def _ext_from_filename(filename: str) -> str:
    base = (filename or "").replace("\\", "/").rsplit("/", 1)[-1]
    stem, dot, ext = base.rpartition(".")
    return ext.lower() if dot and stem else ""


class PluginSourceStrategy:
    """One v1 plugin backend behind the ``SourceStrategy`` protocol.

    ``name`` is the instance's source key (``plugin:<name>``); manifests,
    attempts, handles and quarantine rows all carry it, so bundled
    soulseek/usenet rows are never touched.
    """

    applies_queued_timeout = True
    has_local_disk_faults = False

    def __init__(
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
        album_service=None,
        library=None,
        policy_extras=None,
        probe_tagger=None,
        source_key: str,
        display_name: str | None = None,
    ):  # noqa: ANN001, ANN204
        self._indexer = indexer
        self._scorer = scorer
        self._track_matcher = track_matcher
        self._client = client
        self._store = store
        self._file_processor = file_processor
        self._staging = Path(staging)
        self._manifest_codec = manifest_codec
        self._naming_template = naming_template
        self._album_service = album_service
        self._library = library
        self._policy_extras = policy_extras
        self._probe_tagger = probe_tagger
        self.name = source_key
        self._display_name = display_name or source_key

    @property
    def client(self):  # noqa: ANN201
        return self._client

    def candidate_identity(self, candidate) -> str:  # noqa: ANN001
        release = getattr(candidate, "plugin_release", None)
        if release is None:
            return getattr(candidate, "username", "") or ""
        return _plugin_result_identity(self.name, release)

    def local_fault_message(self, attempt_mount: bool) -> str:
        if attempt_mount:
            return (
                "downloads directory not accessible - check the "
                f"{self._display_name} downloads mount"
            )
        return (
            f"{self._display_name} reported a local disk/write error - "
            "will retry when it clears"
        )

    async def maybe_blocklist_on_failure(
        self, task, status, *, completed, enumerated_any
    ):  # noqa: ANN001, ANN201
        """Quarantine a plugin release only on a CONFIRMED error. Anything else
        (a failure without an error message, an ambiguous empty folder) fails
        over without blocklisting - a missed dead release costs one retry
        cycle, a wrongly-killed good one costs the source."""
        if task.search_job_id is None or task.candidate_index is None:
            return
        candidates = await self._store.get_search_job_candidates(task.search_job_id)
        if not (0 <= task.candidate_index < len(candidates)):
            return
        release = candidates[task.candidate_index].plugin_release
        if release is None:
            return
        if not ((status.error if status else "") or ""):
            return
        stored_reason = "verify_failed" if completed and enumerated_any else "download_failed"
        await self._store.record_quarantine(
            source=self.name,
            identity=_plugin_result_identity(self.name, release),
            reason=stored_reason,
            release_group_mbid=task.release_group_mbid,
        )
        logger.info(
            "download.quarantined",
            extra={
                "task_id": task.id,
                "source": self.name,
                "reason": stored_reason,
                "identity": _plugin_result_identity(self.name, release),
            },
        )

    async def search_and_score(self, task, *, timeout, auto, manual, snapshot):  # noqa: ANN001, ANN201
        held_tier = await _upgrade_held_tier(self._library, task)
        extras = self._policy_extras() if self._policy_extras else None
        indexer_results = await self._indexer.search_album(
            task.artist_name,
            task.album_title,
            task.year,
            task.track_count,
            timeout=timeout,
        )
        releases = [r.plugin for r in indexer_results if r.plugin is not None]
        if task.download_type == "track" or (
            task.track_count == 1 and task.track_title
        ):
            return await self._search_track_files(
                task,
                releases,
                snapshot=snapshot,
                auto=auto,
                manual=manual,
                held_tier=held_tier,
            )
        target = TargetAlbum(
            artist_name=task.artist_name,
            album_title=task.album_title,
            year=task.year,
            track_count=task.track_count,
            release_group_mbid=task.release_group_mbid,
        )
        return await self._scorer.rank(
            target,
            releases,
            snapshot=snapshot,
            spec_extras=extras,
            auto_accept_threshold=auto,
            manual_threshold=manual,
            track_count=task.track_count,
            held_tier=held_tier,
            source_key=self.name,
        )

    async def _search_track_files(
        self, task, releases, *, snapshot, auto, manual, held_tier
    ):  # noqa: ANN001, ANN201
        """Per-file branch for track/single tasks: releases that name exact
        files score through the shared ``TrackMatcher``; folder-mode releases
        (no files) fall back to the album scorer so they stay pickable."""
        from services.native.quality_tiers import is_audio

        files_mode = [r for r in releases if r.files]
        folder_mode = [r for r in releases if not r.files]
        scored: list[ScoredCandidate] = []
        if files_mode:
            quarantined = await self._store.load_quarantine_set()
            shims: list[DownloadSearchResult] = []
            owners: dict[int, tuple[PluginSearchResult, DownloadFileRef]] = {}
            for release in files_mode:
                if (self.name, _plugin_result_identity(self.name, release)) in quarantined:
                    continue
                for ref in release.files:
                    if not _ext_from_filename(ref.filename):
                        continue
                    shim = _shim_search_result(self.name, release, ref)
                    if not is_audio(shim):
                        continue
                    owners[id(shim)] = (release, ref)
                    shims.append(shim)
            if shims:
                target = TargetTrack(
                    artist_name=task.artist_name,
                    track_title=task.track_title or "",
                    album_title=task.album_title,
                    duration_seconds=task.track_duration_seconds,
                    recording_mbid=task.recording_mbid,
                )
                matched = await self._track_matcher.rank(
                    target,
                    shims,
                    snapshot=snapshot,
                    auto_accept_threshold=auto,
                    manual_threshold=manual,
                    held_tier=held_tier,
                )
                for cand in matched:
                    owned = (
                        owners.get(id(cand.files[0]))
                        if cand.files
                        else None
                    )
                    if owned is None:
                        continue
                    release, ref = owned
                    scored.append(
                        ScoredCandidate(
                            source=self.name,
                            plugin_release=release,
                            files=[ref],
                            coherence=cand.coherence,
                            file_confidence=cand.file_confidence,
                            final_score=cand.final_score,
                            tier=cand.tier,
                            quality_evidence=cand.quality_evidence,
                            quality_decision=cand.quality_decision,
                        )
                    )
        if folder_mode:
            target = TargetAlbum(
                artist_name=task.artist_name,
                album_title=task.album_title,
                year=task.year,
                track_count=task.track_count,
                release_group_mbid=task.release_group_mbid,
            )
            extras = self._policy_extras() if self._policy_extras else None
            scored.extend(
                await self._scorer.rank(
                    target,
                    folder_mode,
                    snapshot=snapshot,
                    spec_extras=extras,
                    auto_accept_threshold=auto,
                    manual_threshold=manual,
                    track_count=task.track_count,
                    held_tier=held_tier,
                    source_key=self.name,
                )
            )
        return scored

    async def enqueue(
        self,
        task,
        candidate,
        *,
        strict_track_duration,
        hold_on_wrong_track=False,
        remaining_positions=None,
    ):  # noqa: ANN001, ANN201, ARG002
        # remaining_positions (#292) is accepted-and-ignored: the plugin payload
        # is the smallest addressable unit we can re-request, so failover stays
        # whole-release here; the release-level blocklist stops re-grabs.
        release = candidate.plugin_release
        if release is None:
            raise OrchestrationError("plugin candidate has no release")
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

        files_mode = bool(release.files)
        if files_mode:
            serving = list(candidate.files) or list(release.files)
            files = [
                DownloadFileRef(username=f.username, filename=f.filename, size=f.size)
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
            initial_handle = TaskHandle(
                source=self.name,
                filenames=[f.filename for f in serving],
            )
            attempt = await self._store.create_download_attempt(
                task_id=task.id,
                source=self.name,
                candidate_index=task.candidate_index or 0,
                job_name="",
                handle=initial_handle,
            )
            manifest = DownloadManifest(
                task_id=task.id,
                handle=initial_handle,
                origin=task.origin,
                download_type=task.download_type,
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
                        duration=task.track_duration_seconds if use_canonical else None,
                    )
                    for f in serving
                ],
                expected_tracks=expected_tracks,
                attempt_id=attempt.id,
            )
        else:
            job_name = f"droppedneedle-{task.id}-{task.candidate_index or 0}"
            await self._store.update_status(
                task.id,
                "downloading",
                files_total=len(expected_tracks),
                total_size_bytes=release.size_bytes,
                started_at=time.time(),
            )
            initial_handle = TaskHandle(source=self.name, job_name=job_name)
            attempt = await self._store.create_download_attempt(
                task_id=task.id,
                source=self.name,
                candidate_index=task.candidate_index or 0,
                job_name=job_name,
                handle=initial_handle,
            )
            manifest = DownloadManifest(
                task_id=task.id,
                handle=initial_handle,
                origin=task.origin,
                download_type=task.download_type,
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
            if files_mode:
                handle = await self._client.enqueue(
                    EnqueueRequest(
                        task_id=task.id,
                        source=self.name,
                        files=[
                            DownloadFileRef(
                                username=f.username, filename=f.filename, size=f.size
                            )
                            for f in serving
                        ],
                        payload=release.payload,
                    )
                )
            else:
                handle = await self._client.enqueue(
                    EnqueueRequest(
                        task_id=task.id,
                        source=self.name,
                        job_name=job_name,
                        payload=release.payload,
                    )
                )
        except Exception as exc:  # noqa: BLE001 - any client error -> task failed
            logger.exception("Plugin enqueue failed for task %s", task.id)
            raise OrchestrationError("enqueue failed") from exc

        await self._store.update_download_attempt_handle(attempt.id, handle)
        manifest.handle = handle
        manifest_path.write_bytes(self._manifest_codec.encode(manifest))
        logger.info(
            "download.enqueued",
            extra={
                "task_id": task.id,
                "source": self.name,
                "release_group_mbid": task.release_group_mbid,
                "files_total": len(files) if files_mode else len(expected_tracks),
                "total_size_bytes": total_size if files_mode else release.size_bytes,
            },
        )

    async def import_files(
        self, task, manifest, *, only_filenames=None, completed=False
    ):  # noqa: ANN001, ANN201
        if manifest.target_files:
            return await self._import_files_mode(
                task, manifest, only_filenames=only_filenames
            )
        return await self._import_folder_mode(task, manifest, completed=completed)

    async def _import_files_mode(
        self, task, manifest, *, only_filenames=None
    ):  # noqa: ANN001, ANN201
        # Per-file import: the plugin client wrote the exact files we enqueued;
        # verify + place each, quarantining the RELEASE per failed file.
        logger.info(
            "download.processing",
            extra={
                "task_id": task.id,
                "source": self.name,
                "files_total": len(manifest.target_files),
            },
        )
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
        release_identity = await self._current_release_identity(task)
        for failure in result.failed:
            quarantine_reason = (
                "verify_failed" if failure.reason == "tag_mismatch" else failure.reason
            )
            if quarantine_reason in QUARANTINE_REASONS and release_identity is not None:
                await self._store.record_quarantine(
                    source=self.name,
                    identity=release_identity,
                    reason=quarantine_reason,
                    release_group_mbid=task.release_group_mbid,
                )
                logger.info(
                    "download.quarantined",
                    extra={
                        "task_id": task.id,
                        "source": self.name,
                        "file": failure.filename.replace("\\", "/").rsplit("/", 1)[-1],
                        "reason": failure.reason,
                    },
                )
        if result.succeeded:
            await self._store.set_final_path(
                task.id, str(Path(result.succeeded[0]).parent)
            )
        return result, len(result.succeeded) + len(result.failed)

    async def _import_folder_mode(self, task, manifest, *, completed):  # noqa: ANN001, ANN201
        # Folder-based import: enumerate the plugin job's completed files and
        # match them to the expected MB tracklist. No NFS settle re-poll: a
        # missing folder short-circuits to the preserve path, an
        # existing-but-empty one is judged once.
        files = await self._client.list_completed_files(manifest.handle)
        if not files and completed:
            if await self._completed_folder_missing_handle(manifest.handle):
                logger.warning(
                    "download.plugin_folder_missing",
                    extra={"task_id": task.id, "source": self.name},
                )
                return ProcessResult(
                    succeeded=[],
                    failed=[],
                    workspace_disposition="preserve",
                ), 0
        enumerated = len(files)
        logger.info(
            "download.processing",
            extra={"task_id": task.id, "source": self.name, "enumerated": enumerated},
        )
        if not files and completed:
            if not await self._mount_healthy():
                logger.warning(
                    "download.plugin_mount_unhealthy",
                    extra={"task_id": task.id, "source": self.name},
                )
                return ProcessResult(
                    succeeded=[],
                    failed=[
                        FileFailure(filename="", reason=DOWNLOADS_MOUNT_UNAVAILABLE)
                    ],
                ), enumerated
        if self._probe_tagger is not None and files:
            candidate = await self._current_candidate(task)
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
        release_identity = await self._current_release_identity(task)
        for failure in result.failed:
            quarantine_reason = (
                "verify_failed" if failure.reason == "tag_mismatch" else failure.reason
            )
            if quarantine_reason in QUARANTINE_REASONS and release_identity is not None:
                await self._store.record_quarantine(
                    source=self.name,
                    identity=release_identity,
                    reason=quarantine_reason,
                    release_group_mbid=task.release_group_mbid,
                )
                logger.info(
                    "download.quarantined",
                    extra={
                        "task_id": task.id,
                        "source": self.name,
                        "file": failure.filename.replace("\\", "/").rsplit("/", 1)[-1],
                        "reason": failure.reason,
                    },
                )
        if result.succeeded:
            await self._store.set_final_path(
                task.id, str(Path(result.succeeded[0]).parent)
            )
        if not files and completed:
            result = ProcessResult(
                succeeded=list(result.succeeded),
                failed=list(result.failed),
                publisher_bundle_ids=list(result.publisher_bundle_ids),
                workspace_disposition="preserve",
            )
        return result, enumerated

    async def _mount_healthy(self) -> bool:
        fn = getattr(self._client, "downloads_mount_healthy", None)
        if fn is None:
            return True
        try:
            return bool(await fn())
        except Exception:  # noqa: BLE001 - a diagnostic failure reads healthy
            return True

    async def _completed_folder_missing_handle(self, handle) -> bool:  # noqa: ANN001
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

    async def _current_release_identity(self, task) -> str | None:  # noqa: ANN001
        candidate = await self._current_candidate(task)
        release = getattr(candidate, "plugin_release", None) if candidate else None
        if release is None:
            return None
        return _plugin_result_identity(self.name, release)


PluginStrategy = PluginSourceStrategy

__all__ = ["PluginSourceStrategy", "PluginStrategy"]
