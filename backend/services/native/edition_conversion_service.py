"""Acquire a complete exact release without changing library files before Apply."""

from __future__ import annotations

import asyncio
import hashlib
import hmac
from pathlib import Path
import secrets
import shutil
import time
from typing import TYPE_CHECKING, Callable
import unicodedata
import uuid

import msgspec

from api.v1.schemas.edition_conversion import (
    EditionConversionLocalFileResponse,
    EditionConversionPreviewResponse,
    EditionConversionStatusResponse,
    EditionConversionTargetResponse,
)
from api.v1.schemas.library_management_preview import LibraryManagementApplyRequest
from api.v1.schemas.library_operations import OperationResponse
from core.exceptions import (
    ConflictError,
    ResourceNotFoundError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.validators import is_valid_mbid
from infrastructure.audio.metadata_engine import (
    AudioMetadataEngine,
    legacy_audio_projection,
)
from models.edition_management import (
    EditionConversionArtifact,
    EditionConversionJob,
    EditionConversionLocalFile,
    EditionConversionTarget,
)
from models.library_management import (
    MANAGEMENT_RECYCLE_ROOT_ID,
    LibraryManagementImportBundle,
    LibraryManagementImportFile,
    LibraryManagementJobSnapshot,
    LibraryManagementPlanItem,
)
from models.library_work import OperationJob
from api.v1.schemas.library_management import settings_revision
from services.native.library_management_naming_policy import naming_policy_revision
from services.native.file_processor import _fingerprint_recording_proof
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.identification_revisions import (
    album_identity_revision,
    album_input_revisions,
)

if TYPE_CHECKING:
    from infrastructure.persistence.download_store import DownloadStore
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from services.acquisition_dispatcher import AcquisitionDispatcher
    from services.album_service import AlbumService
    from services.native.download_service import DownloadService
    from services.native.free_music_service import FreeMusicService
    from services.native.automatic_import_management_service import (
        AutomaticImportManagementService,
    )
    from services.native.target_import_library_service import TargetImportLibraryService
    from services.preferences_service import PreferencesService
    from infrastructure.audio.fingerprinter import AudioFingerprinter


_ACQUISITION_ESTIMATE_FLOOR = 8 * 1024 * 1024
_DISK_SAFETY_BYTES = 64 * 1024 * 1024
_TERMINAL_DOWNLOAD_STATES = frozenset(
    {"completed", "partial", "failed", "cancelled", "held"}
)


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class EditionConversionService:
    def __init__(
        self,
        *,
        store: "NativeLibraryStore",
        album_service: "AlbumService",
        preferences: "PreferencesService",
        acquisition: "AcquisitionDispatcher",
        download_store: "DownloadStore",
        get_download_service: "Callable[[], DownloadService]",
        get_free_music_service: "Callable[[], FreeMusicService]",
        automatic_management: "AutomaticImportManagementService",
        fingerprinter: "AudioFingerprinter",
        held_dir: Path,
        import_library: "TargetImportLibraryService",
        audio: AudioMetadataEngine | None = None,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._albums = album_service
        self._preferences = preferences
        self._acquisition = acquisition
        self._downloads = download_store
        self._get_download_service = get_download_service
        self._get_free_music_service = get_free_music_service
        self._automatic_management = automatic_management
        self._fingerprinter = fingerprinter
        self._held_dir = held_dir
        self._audio = audio or AudioMetadataEngine()
        self._import_library = import_library
        self._clock = clock

    async def create_preflight(
        self,
        *,
        local_album_id: str,
        release_group_mbid: str,
        release_mbid: str,
        actor_user_id: str,
    ) -> EditionConversionStatusResponse:
        if not is_valid_mbid(release_group_mbid) or not is_valid_mbid(release_mbid):
            raise ValidationError("Select a valid MusicBrainz release.")
        if await self._store.get_active_edition_conversion(local_album_id) is not None:
            raise ConflictError(
                "This album already has active edition-conversion work."
            )
        context, album_info, edition = await asyncio.gather(
            self._store.get_album_identification_context(local_album_id),
            self._albums.get_album_info(release_group_mbid),
            self._albums.get_exact_edition_tracks_info(
                release_group_mbid, release_mbid
            ),
        )
        if context is None:
            raise ResourceNotFoundError("Library album not found.")
        identity = context["identity"]
        if (
            identity is not None
            and identity.get("release_group_mbid")
            and str(identity["release_group_mbid"]).casefold()
            != release_group_mbid.casefold()
        ):
            raise ConflictError(
                "The selected release belongs to a different accepted album identity."
            )
        tracks = [
            value for value in context["tracks"] if value["availability"] == "indexed"
        ]
        if not tracks:
            raise ValidationError("This album has no indexed audio files to convert.")
        targets = self._targets(edition.tracks)
        local_files = self._match_local_files(
            job_id="pending", tracks=tracks, targets=targets, release_mbid=release_mbid
        )
        kept_by_ordinal = {
            int(value.target_ordinal): value.local_track_id
            for value in local_files
            if value.action == "keep" and value.target_ordinal is not None
        }
        job_id = str(uuid.uuid4())
        targets = tuple(
            EditionConversionTarget(
                job_id=job_id,
                ordinal=value.ordinal,
                disc_number=value.disc_number,
                track_number=value.track_number,
                release_track_mbid=value.release_track_mbid,
                recording_mbid=value.recording_mbid,
                title=value.title,
                duration_seconds=value.duration_seconds,
                state="kept" if value.ordinal in kept_by_ordinal else "pending",
                kept_local_track_id=kept_by_ordinal.get(value.ordinal),
            )
            for value in targets
        )
        local_files = tuple(
            EditionConversionLocalFile(
                job_id=job_id,
                local_track_id=value.local_track_id,
                action=value.action,
                target_ordinal=value.target_ordinal,
                evidence_kind=value.evidence_kind,
                expected_track_revision=value.expected_track_revision,
                expected_identity_revision=value.expected_identity_revision,
                expected_stat_revision=value.expected_stat_revision,
            )
            for value in local_files
        )
        kept_size = sum(
            int(track["file_size_bytes"])
            for track in tracks
            if any(
                value.local_track_id == str(track["id"]) and value.action == "keep"
                for value in local_files
            )
        )
        acquisition_estimate = sum(
            max(
                _ACQUISITION_ESTIMATE_FLOOR,
                int((value.duration_seconds or 240.0) * 32_000),
            )
            for value in targets
            if value.state == "pending"
        )
        token = secrets.token_urlsafe(32)
        now = self._clock()
        input_revision = ":".join(album_input_revisions(tracks))
        identity_revision = album_identity_revision(identity, tracks)
        job = EditionConversionJob(
            id=job_id,
            local_album_id=local_album_id,
            target_release_group_mbid=release_group_mbid,
            target_release_mbid=release_mbid,
            target_album_title=album_info.title,
            target_artist_name=album_info.artist_name,
            state="preflight",
            expected_album_revision=int(context["album"]["row_revision"]),
            expected_input_revision=input_revision,
            expected_identity_revision=identity_revision,
            preflight_token_hash=hashlib.sha256(token.encode()).hexdigest(),
            download_source_ready=self._preferences.is_download_source_ready(),
            required_temporary_bytes=(
                sum(int(track["file_size_bytes"]) for track in tracks)
                + kept_size
                + acquisition_estimate
                + _DISK_SAFETY_BYTES
            ),
            kept_count=len(kept_by_ordinal),
            acquire_count=sum(value.state == "pending" for value in targets),
            recycle_count=sum(value.action != "keep" for value in local_files),
            staged_count=0,
            failed_count=0,
            final_preview_job_id=None,
            final_preview_token_hash=None,
            final_bundle_json=None,
            final_bundle_hash=None,
            requested_by_user_id=actor_user_id,
            error_code=None,
            created_at=now,
            updated_at=now,
        )
        created = await self._store.create_edition_conversion(job, targets, local_files)
        return self._response(created, preflight_token=token)

    async def start(
        self,
        job_id: str,
        *,
        preflight_token: str,
        expected_row_revision: int,
        confirmation: bool,
    ) -> EditionConversionStatusResponse:
        if not confirmation:
            raise ValidationError(
                "Confirm matching this exact edition before starting."
            )
        job = await self._require(job_id)
        token_hash = hashlib.sha256(preflight_token.encode()).hexdigest()
        if not hmac.compare_digest(token_hash, job.preflight_token_hash):
            raise ValidationError("The edition-conversion preflight token is invalid.")
        await self._assert_current(job)
        if job.acquire_count and not self._preferences.is_download_source_ready():
            raise ValidationError(
                "Set up a music acquisition source before matching this edition."
            )
        await self._ensure_temporary_space(job)
        job = await self._store.start_edition_conversion(
            job_id,
            expected_row_revision=expected_row_revision,
            preflight_token_hash=token_hash,
            now=self._clock(),
        )
        await self._dispatch_pending(job)
        return await self.status(job_id)

    async def status(self, job_id: str) -> EditionConversionStatusResponse:
        job = await self._require(job_id)
        if job.state == "acquiring":
            job = await self._refresh_acquisition(job)
        return self._response(job)

    async def create_final_preview(
        self, job_id: str, *, expected_row_revision: int
    ) -> EditionConversionPreviewResponse:
        job = await self._require(job_id)
        if job.state != "ready" or job.row_revision != expected_row_revision:
            raise StaleRevisionError(
                "The edition conversion changed before its final preview."
            )
        preview_token = secrets.token_urlsafe(32)
        if job.final_preview_job_id is None:
            job = await self._ensure_final_preview(job, preview_token=preview_token)
        else:
            settings = self._preferences.get_library_management_settings_raw()
            now = self._clock()
            job = await self._store.rotate_edition_conversion_preview_capability(
                job.id,
                expected_row_revision=job.row_revision,
                preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
                preview_expires_at=(now + settings.preview_retention_hours * 60 * 60),
                now=now,
            )
        return EditionConversionPreviewResponse(
            status=self._response(job), preview_token=preview_token
        )

    async def retry(
        self,
        job_id: str,
        *,
        target_ordinals: list[int],
        expected_row_revision: int,
    ) -> EditionConversionStatusResponse:
        if not self._preferences.is_download_source_ready():
            raise ValidationError(
                "Set up a music acquisition source before retrying these tracks."
            )
        job = await self._store.reset_edition_conversion_targets(
            job_id,
            tuple(target_ordinals),
            expected_row_revision=expected_row_revision,
            now=self._clock(),
        )
        await self._assert_current(job)
        await self._dispatch_pending(job, selected=set(target_ordinals))
        return await self.status(job_id)

    async def cancel(
        self,
        job_id: str,
        *,
        expected_row_revision: int,
        confirmation: bool,
    ) -> EditionConversionStatusResponse:
        if not confirmation:
            raise ValidationError("Confirm cancelling this edition conversion.")
        job = await self._require(job_id)
        cancelled = await self._store.cancel_edition_conversion(
            job_id,
            expected_row_revision=expected_row_revision,
            now=self._clock(),
        )
        for association in await self._store.list_edition_conversion_downloads(job_id):
            if str(association["state"]) not in {
                "active",
                "downloading",
                "cancelled",
            }:
                continue
            task_id = str(association["task_id"])
            try:
                if association["source_kind"] == "free_music":
                    await self._get_free_music_service().cancel(
                        task_id, user_id=job.requested_by_user_id, is_admin=True
                    )
                else:
                    await self._get_download_service().cancel_task(
                        task_id, job.requested_by_user_id, "admin"
                    )
            except (ResourceNotFoundError, ValidationError):
                pass
        held_ids: list[int] = []
        for association in await self._store.list_edition_conversion_downloads(job_id):
            held = await self._downloads.list_held_imports(
                job.requested_by_user_id,
                "admin",
                source_task_id=str(association["task_id"]),
            )
            held_ids.extend(
                value.id for value in held if value.origin == "edition_conversion"
            )
        if held_ids:
            await self._downloads.resolve_held_imports(held_ids, "discarded")
        for artifact in job.artifacts:
            await asyncio.to_thread(Path(artifact.held_path).unlink, missing_ok=True)
        return self._response(cancelled)

    async def recheck(
        self, job_id: str, *, expected_row_revision: int
    ) -> EditionConversionStatusResponse:
        job = await self._require(job_id)
        if job.state != "needs_recheck":
            raise ValidationError("Only stale conversion work can be rechecked.")
        if job.row_revision != expected_row_revision:
            raise StaleRevisionError("The edition conversion changed before recheck.")
        context = await self._store.get_album_identification_context(job.local_album_id)
        if context is None:
            raise StaleRevisionError("The album is no longer available.")
        identity = context["identity"]
        if (
            identity is not None
            and identity.get("release_group_mbid")
            and str(identity["release_group_mbid"]).casefold()
            != job.target_release_group_mbid.casefold()
        ):
            raise ConflictError(
                "The album now has a conflicting MusicBrainz release-group identity."
            )
        tracks = [
            value for value in context["tracks"] if value["availability"] == "indexed"
        ]
        if not tracks:
            raise ValidationError("This album no longer has indexed audio files.")
        local_files = self._match_local_files(
            job_id=job.id,
            tracks=tracks,
            targets=job.targets,
            release_mbid=job.target_release_mbid,
        )
        target_by_ordinal = {value.ordinal: value for value in job.targets}
        kept_by_ordinal = {
            int(value.target_ordinal): value.local_track_id
            for value in local_files
            if value.action == "keep" and value.target_ordinal is not None
        }
        reusable: set[int] = set()
        for artifact in job.artifacts:
            target = target_by_ordinal.get(artifact.target_ordinal)
            path = Path(artifact.held_path)
            if (
                target is None
                or artifact.release_track_mbid != target.release_track_mbid
                or artifact.recording_mbid != target.recording_mbid
                or path.is_symlink()
                or not path.is_file()
                or (
                    artifact.source_kind == "retained_copy"
                    and target.kept_local_track_id
                    != kept_by_ordinal.get(target.ordinal)
                )
            ):
                continue
            try:
                digest = await asyncio.to_thread(_sha256_file, path)
            except OSError:
                continue
            if hmac.compare_digest(digest, artifact.file_sha256):
                reusable.add(artifact.target_ordinal)
        associations = await self._store.list_edition_conversion_downloads(job.id)
        active_downloads = {
            int(value["target_ordinal"])
            for value in associations
            if str(value["state"]) in {"active", "downloading"}
        }
        uncovered = set(target_by_ordinal) - reusable - set(kept_by_ordinal)
        if (
            uncovered - active_downloads
            and not self._preferences.is_download_source_ready()
        ):
            raise ValidationError(
                "Set up a music acquisition source before continuing this conversion."
            )
        refreshed, cleanup_paths = await self._store.recheck_edition_conversion(
            job.id,
            local_files,
            reusable_artifact_ordinals=frozenset(reusable),
            expected_row_revision=expected_row_revision,
            expected_album_revision=int(context["album"]["row_revision"]),
            expected_input_revision=":".join(album_input_revisions(tracks)),
            expected_identity_revision=album_identity_revision(identity, tracks),
            now=self._clock(),
        )
        for raw_path in cleanup_paths:
            await asyncio.to_thread(Path(raw_path).unlink, missing_ok=True)
        if any(value.state == "pending" for value in refreshed.targets):
            await self._dispatch_pending(refreshed)
        return await self.status(refreshed.id)

    async def apply_preview(
        self, preview_job_id: str, request: LibraryManagementApplyRequest
    ) -> OperationResponse | None:
        job = await self._store.get_edition_conversion_for_preview(preview_job_id)
        if job is None:
            return None
        if not request.confirmation:
            raise ValidationError("Confirm Apply Library Management before starting.")
        if not request.idempotency_key.strip():
            raise ValidationError("An apply idempotency key is required.")
        token_hash = hashlib.sha256(request.preview_token.encode()).hexdigest()
        if not hmac.compare_digest(token_hash, job.final_preview_token_hash or ""):
            raise ValidationError("The final conversion preview token is invalid.")
        if job.state == "applied":
            snapshot = await self._store.get_library_management_job_snapshot(
                preview_job_id
            )
            row = await self._store.get_operation_job(preview_job_id)
            if (
                snapshot is not None
                and row is not None
                and snapshot.apply_idempotency_key == request.idempotency_key
                and str(row["state"]) == "succeeded"
            ):
                from services.native.library_operation_service import (
                    LibraryOperationService,
                )

                return LibraryOperationService._response(row)
            raise StaleRevisionError(
                "The final conversion preview has already been applied."
            )
        if (
            job.state != "ready"
            or job.final_bundle_json is None
            or job.final_bundle_hash is None
            or job.final_preview_token_hash is None
        ):
            raise StaleRevisionError("The final conversion preview is no longer ready.")
        if (
            hashlib.sha256(job.final_bundle_json.encode()).hexdigest()
            != job.final_bundle_hash
        ):
            raise ConflictError("The sealed conversion bundle changed.")
        bundle = msgspec.json.decode(
            job.final_bundle_json.encode(), type=LibraryManagementImportBundle
        )
        if (
            bundle.conversion_job_id != job.id
            or bundle.conversion_preview_job_id != preview_job_id
            or bundle.conversion_expected_row_revision != job.row_revision
        ):
            raise StaleRevisionError("The sealed conversion correlation changed.")
        await self._assert_current(job)
        await self._store.begin_edition_conversion_apply(
            job.id,
            preview_job_id=preview_job_id,
            preview_token_hash=token_hash,
            expected_operation_row_revision=request.expected_operation_row_revision,
            apply_idempotency_key=request.idempotency_key,
            now=self._clock(),
        )
        try:
            await self._import_library.publish_import_bundle(bundle)
        except BaseException:
            await self._store.reset_edition_conversion_apply(
                job.id, preview_job_id=preview_job_id, now=self._clock()
            )
            raise
        row = await self._store.get_operation_job(preview_job_id)
        if row is None:
            raise ResourceNotFoundError(
                "The completed conversion operation disappeared."
            )
        from services.native.library_operation_service import LibraryOperationService

        return LibraryOperationService._response(row)

    async def _require(self, job_id: str) -> EditionConversionJob:
        job = await self._store.get_edition_conversion(job_id)
        if job is None:
            raise ResourceNotFoundError("Edition conversion not found.")
        return job

    async def _assert_current(self, job: EditionConversionJob) -> None:
        context = await self._store.get_album_identification_context(job.local_album_id)
        if context is None:
            raise StaleRevisionError("The album is no longer available.")
        tracks = [
            value for value in context["tracks"] if value["availability"] == "indexed"
        ]
        if (
            int(context["album"]["row_revision"]) != job.expected_album_revision
            or ":".join(album_input_revisions(tracks)) != job.expected_input_revision
            or album_identity_revision(context["identity"], tracks)
            != job.expected_identity_revision
        ):
            if job.state in {"acquiring", "ready"}:
                await self._store.set_edition_conversion_state(
                    job.id,
                    expected_row_revision=job.row_revision,
                    expected_states=(job.state,),
                    state="needs_recheck",
                    now=self._clock(),
                    error_code="STALE_REVISION",
                )
            raise StaleRevisionError(
                "The album or its provider identity changed; recheck this conversion."
            )

    async def _dispatch_pending(
        self, job: EditionConversionJob, selected: set[int] | None = None
    ) -> None:
        album = await self._albums.get_album_info(job.target_release_group_mbid)
        source_kind = (
            "download"
            if self._preferences.is_builtin_download_ready()
            else "free_music"
        )
        for target in job.targets:
            if target.state != "pending" or (
                selected is not None and target.ordinal not in selected
            ):
                continue
            task_id: str | None = None
            try:
                task_id = await self._acquisition.request_track(
                    user_id=job.requested_by_user_id,
                    recording_mbid=target.recording_mbid,
                    artist_name=job.target_artist_name,
                    track_title=target.title,
                    album_title=job.target_album_title,
                    duration_seconds=(
                        int(target.duration_seconds)
                        if target.duration_seconds is not None
                        else None
                    ),
                    release_group_mbid=job.target_release_group_mbid,
                    artist_mbid=album.artist_id,
                    origin="edition_conversion",
                    release_mbid=job.target_release_mbid,
                    release_track_mbid=target.release_track_mbid,
                    track_number=target.track_number,
                    disc_number=target.disc_number,
                )
                await self._store.associate_edition_conversion_download(
                    job.id,
                    target.ordinal,
                    source_kind=source_kind,
                    task_id=task_id,
                    now=self._clock(),
                )
            except Exception:  # noqa: BLE001 - each target has an independent retry
                if task_id is not None:
                    try:
                        if source_kind == "free_music":
                            await self._get_free_music_service().cancel(
                                task_id,
                                user_id=job.requested_by_user_id,
                                is_admin=True,
                            )
                        else:
                            await self._get_download_service().cancel_task(
                                task_id, job.requested_by_user_id, "admin"
                            )
                    except Exception:  # noqa: BLE001 - compensation cannot stop other targets
                        pass
                await self._store.fail_edition_conversion_target(
                    job.id,
                    target.ordinal,
                    code="ACQUISITION_FAILED",
                    now=self._clock(),
                )

    async def _refresh_acquisition(
        self, job: EditionConversionJob
    ) -> EditionConversionJob:
        associations = await self._store.list_edition_conversion_downloads(job.id)
        target_by_ordinal = {value.ordinal: value for value in job.targets}
        for association in associations:
            if str(association["state"]) not in {"active", "downloading"}:
                continue
            target = target_by_ordinal.get(int(association["target_ordinal"]))
            if target is None or target.state in {"kept", "staged", "failed"}:
                continue
            task_id = str(association["task_id"])
            held = await self._downloads.list_held_imports(
                job.requested_by_user_id, "admin", source_task_id=task_id
            )
            matching = [
                value
                for value in held
                if value.origin == "edition_conversion"
                and value.release_track_mbid == target.release_track_mbid
                and value.recording_mbid == target.recording_mbid
            ]
            if len(matching) == 1:
                path = Path(matching[0].held_path)
                try:
                    stat_result, digest = await asyncio.gather(
                        asyncio.to_thread(path.stat),
                        asyncio.to_thread(_sha256_file, path),
                    )
                except OSError:
                    await self._store.fail_edition_conversion_target(
                        job.id,
                        target.ordinal,
                        code="STAGED_ARTIFACT_MISSING",
                        now=self._clock(),
                    )
                    continue
                await self._store.stage_edition_conversion_artifact(
                    EditionConversionArtifact(
                        id=str(uuid.uuid4()),
                        job_id=job.id,
                        target_ordinal=target.ordinal,
                        held_path=str(path),
                        file_sha256=digest,
                        fingerprint=None,
                        release_track_mbid=target.release_track_mbid,
                        recording_mbid=target.recording_mbid,
                        source_kind=str(association["source_kind"]),
                        source_task_id=task_id,
                        file_size_bytes=stat_result.st_size,
                        created_at=self._clock(),
                    ),
                    task_id=task_id,
                    now=self._clock(),
                )
                continue
            if association["source_kind"] == "free_music":
                try:
                    task = await self._get_free_music_service().get_task(
                        task_id,
                        user_id=job.requested_by_user_id,
                        is_admin=True,
                    )
                    terminal = task.status in {"completed", "failed", "cancelled"}
                except ResourceNotFoundError:
                    terminal = True
            else:
                task = await self._downloads.get_task(task_id)
                terminal = task is None or task.status in _TERMINAL_DOWNLOAD_STATES
            if terminal:
                await self._store.fail_edition_conversion_target(
                    job.id,
                    target.ordinal,
                    code="ACQUISITION_UNRESOLVED",
                    now=self._clock(),
                )
        refreshed = await self._require(job.id)
        if all(value.state in {"kept", "staged"} for value in refreshed.targets):
            refreshed = await self._store.set_edition_conversion_state(
                refreshed.id,
                expected_row_revision=refreshed.row_revision,
                expected_states=("acquiring",),
                state="ready",
                now=self._clock(),
            )
        return refreshed

    async def _ensure_final_preview(
        self, job: EditionConversionJob, *, preview_token: str
    ) -> EditionConversionJob:
        await self._assert_current(job)
        context = await self._store.get_album_identification_context(job.local_album_id)
        if context is None:
            raise ResourceNotFoundError("Library album not found.")
        indexed = [
            value for value in context["tracks"] if value["availability"] == "indexed"
        ]
        tracks_by_id = {str(value["id"]): value for value in indexed}
        artifacts = {value.target_ordinal: value for value in job.artifacts}
        await asyncio.to_thread(self._held_dir.mkdir, parents=True, exist_ok=True)
        for target in job.targets:
            if target.ordinal in artifacts:
                continue
            if target.state != "kept" or target.kept_local_track_id is None:
                raise StaleRevisionError(
                    "The final conversion preview is missing a verified track artifact."
                )
            local = tracks_by_id.get(target.kept_local_track_id)
            if local is None:
                raise StaleRevisionError("A retained conversion track changed.")
            source = Path(str(local["file_path"]))
            if source.is_symlink():
                raise StaleRevisionError(
                    "A retained conversion source is a symbolic link."
                )
            held = self._held_dir / (
                f"conversion-{job.id}-{target.ordinal}-{source.name}"
            )
            try:
                await asyncio.to_thread(shutil.copy2, source, held)
                if held.is_symlink():
                    raise StaleRevisionError(
                        "A retained conversion artifact is a symbolic link."
                    )
                stat_result, digest = await asyncio.gather(
                    asyncio.to_thread(held.stat),
                    asyncio.to_thread(_sha256_file, held),
                )
                fingerprint = await self._fingerprinter.fingerprint(held)
                if (
                    _fingerprint_recording_proof(fingerprint, target.recording_mbid)
                    is not True
                ):
                    raise ValidationError(
                        "A retained track could not be verified as the requested recording."
                    )
                artifact = EditionConversionArtifact(
                    id=str(uuid.uuid4()),
                    job_id=job.id,
                    target_ordinal=target.ordinal,
                    held_path=str(held),
                    file_sha256=digest,
                    fingerprint=None,
                    release_track_mbid=target.release_track_mbid,
                    recording_mbid=target.recording_mbid,
                    source_kind="retained_copy",
                    source_task_id=None,
                    file_size_bytes=stat_result.st_size,
                    created_at=self._clock(),
                )
                await self._store.stage_retained_edition_conversion_artifact(
                    artifact, now=self._clock()
                )
                artifacts[target.ordinal] = artifact
            except BaseException:
                await asyncio.to_thread(held.unlink, missing_ok=True)
                raise
        job = await self._require(job.id)
        artifacts = {value.target_ordinal: value for value in job.artifacts}
        settings = self._preferences.get_library_management_settings_raw()
        recycle_path = settings.recycle_bin_path.strip()
        if not recycle_path:
            raise ValidationError(
                "Configure the Library Management recycle bin before converting an edition."
            )
        policy = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        local_by_target: dict[int, list[EditionConversionLocalFile]] = {}
        for value in job.local_files:
            if value.action != "keep" and value.target_ordinal is not None:
                local_by_target.setdefault(value.target_ordinal, []).append(value)
        used_local_ids: set[str] = set()
        requests: list[LibraryManagementImportFile] = []
        first = indexed[0]
        first_parent = Path(str(first["relative_path"])).parent
        for target in job.targets:
            artifact = artifacts.get(target.ordinal)
            if artifact is None:
                raise StaleRevisionError(
                    "The final conversion preview lost a verified artifact."
                )
            replacement_id = target.kept_local_track_id
            if replacement_id is None:
                candidate = next(
                    (
                        value
                        for value in local_by_target.get(target.ordinal, [])
                        if value.local_track_id not in used_local_ids
                    ),
                    None,
                )
                replacement_id = (
                    candidate.local_track_id if candidate is not None else None
                )
            replacement = (
                tracks_by_id.get(replacement_id) if replacement_id is not None else None
            )
            if replacement_id is not None:
                used_local_ids.add(replacement_id)
            document = await asyncio.to_thread(
                self._audio.read, Path(artifact.held_path)
            )
            tag, info = legacy_audio_projection(document)
            suffix = Path(artifact.held_path).suffix
            destination_root_id = str(
                replacement["root_id"] if replacement is not None else first["root_id"]
            )
            destination_relative = str(
                replacement["relative_path"]
                if replacement is not None
                else (
                    first_parent
                    / f"conversion-{target.disc_number}-{target.track_number}{suffix}"
                ).as_posix()
            )
            requests.append(
                LibraryManagementImportFile(
                    ordinal=len(requests),
                    input_path=artifact.held_path,
                    destination_root_id=destination_root_id,
                    destination_relative_path=destination_relative,
                    tag=tag,
                    info=info,
                    release_group_mbid=job.target_release_group_mbid,
                    release_mbid=job.target_release_mbid,
                    recording_mbid=target.recording_mbid,
                    confidence=1.0,
                    source="edition_conversion",
                    source_path=artifact.held_path,
                    download_task_id=artifact.source_task_id,
                    replacement_local_track_id=replacement_id,
                    replacement_root_id=(
                        str(replacement["root_id"]) if replacement is not None else None
                    ),
                    replacement_relative_path=(
                        str(replacement["relative_path"])
                        if replacement is not None
                        else None
                    ),
                    recycle_bin_path=recycle_path if replacement is not None else None,
                    authoritative_mapping=True,
                    release_track_mbid=target.release_track_mbid,
                    medium_position=target.disc_number,
                    release_track_position=target.track_number,
                )
            )
        for local in job.local_files:
            if local.local_track_id in used_local_ids or local.action == "keep":
                continue
            row = tracks_by_id.get(local.local_track_id)
            if row is None:
                raise StaleRevisionError("A file selected for recycling changed.")
            source = Path(str(row["file_path"]))
            document = await asyncio.to_thread(self._audio.read, source)
            tag, info = legacy_audio_projection(document)
            requests.append(
                LibraryManagementImportFile(
                    ordinal=len(requests),
                    input_path=str(source),
                    destination_root_id=MANAGEMENT_RECYCLE_ROOT_ID,
                    destination_relative_path=(
                        f"{job.id}/{local.local_track_id}-{source.name}"
                    ),
                    tag=tag,
                    info=info,
                    release_group_mbid=None,
                    release_mbid=None,
                    recording_mbid=None,
                    confidence=1.0,
                    source="edition_conversion",
                    source_path=str(source),
                    replacement_local_track_id=local.local_track_id,
                    replacement_root_id=str(row["root_id"]),
                    replacement_relative_path=str(row["relative_path"]),
                    recycle_bin_path=recycle_path,
                    conversion_recycle_only=True,
                )
            )
        preview_job_id = str(
            uuid.uuid5(
                uuid.UUID("93faab95-7805-47d3-a7f0-c0ecce38940b"),
                f"{job.id}:{job.row_revision}",
            )
        )
        bundle = LibraryManagementImportBundle(
            idempotency_key=f"edition-conversion:{job.id}",
            origin="edition_conversion",
            policy_revision=policy.policy_revision,
            files=tuple(requests),
            conversion_job_id=job.id,
            conversion_expected_row_revision=job.row_revision + 1,
            conversion_local_album_id=job.local_album_id,
            conversion_preview_job_id=preview_job_id,
            conversion_recycle_bin_path=recycle_path,
        )
        prepared = await self._automatic_management.prepare(bundle)
        pinned = next(
            (
                value.pinned_profile
                for value in prepared.files
                if not value.conversion_recycle_only
                and value.pinned_profile is not None
            ),
            None,
        )
        if pinned is None:
            raise ValidationError(
                "The final conversion preview has no current management profile."
            )
        bundle_json = msgspec.json.encode(prepared).decode()
        bundle_hash = hashlib.sha256(bundle_json.encode()).hexdigest()
        now = self._clock()
        catalog_revision = await self._store.get_catalog_revision()
        snapshot = LibraryManagementJobSnapshot(
            job_id=preview_job_id,
            mode="preview",
            origin="manual",
            phase="ready",
            selection_json=('{"kind":"albums","ids":["' + job.local_album_id + '"]}'),
            profile_revision=pinned.profile.revision,
            settings_revision=settings_revision(settings),
            naming_revision=naming_policy_revision(pinned),
            policy_revision=policy.policy_revision,
            catalog_revision=catalog_revision,
            profile_snapshot_json=msgspec.json.encode(pinned).decode(),
            preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
            preview_created_at=now,
            preview_expires_at=(now + settings.preview_retention_hours * 60 * 60),
            target_root_id=str(first["root_id"]),
            intent_json=('{"edition_conversion_job_id":"' + job.id + '"}'),
            summary_json=msgspec.json.encode(
                {
                    "selected_item_count": len(requests),
                    "item_count": len(requests),
                    "bundle_count": 1,
                    "eligible_count": len(requests),
                    "tag_change_count": len(job.targets),
                    "path_change_count": len(job.targets),
                    "estimated_temporary_bytes": job.required_temporary_bytes,
                    "roots": {
                        str(
                            value.replacement_root_id or value.destination_root_id
                        ): sum(
                            1
                            for item in prepared.files
                            if str(item.replacement_root_id or item.destination_root_id)
                            == str(
                                value.replacement_root_id or value.destination_root_id
                            )
                        )
                        for value in prepared.files
                    },
                    "formats": {
                        value.info.file_format: sum(
                            1
                            for item in prepared.files
                            if item.info.file_format == value.info.file_format
                        )
                        for value in prepared.files
                    },
                    "metadata_snapshot_ids": list(
                        dict.fromkeys(
                            value.metadata_snapshot_id
                            for value in prepared.files
                            if value.metadata_snapshot_id is not None
                        )
                    ),
                    "reasons": {
                        "edition_conversion_keep": job.kept_count,
                        "edition_conversion_acquire": job.acquire_count,
                        "edition_conversion_recycle": job.recycle_count,
                    },
                }
            ).decode(),
            created_at=now,
            updated_at=now,
        )
        plan_items = list(
            await asyncio.gather(
                *(
                    asyncio.to_thread(
                        self._preview_plan_item,
                        preview_job_id,
                        job,
                        request,
                        catalog_revision=catalog_revision,
                        policy_revision=policy.policy_revision,
                        profile_revision=pinned.profile.revision,
                        created_at=now,
                    )
                    for request in prepared.files
                )
            )
        )
        existing_id, _created = await self._store.create_library_management_job(
            OperationJob(
                id=preview_job_id,
                kind="library_management",
                state="ready",
                requested_by_user_id=job.requested_by_user_id,
                input_catalog_revision=catalog_revision,
                expected_work_count=1,
                idempotency_key=f"edition-conversion-preview:{job.id}",
                created_at=now,
            ),
            snapshot,
            metadata_snapshot_ids=list(
                dict.fromkeys(
                    value.metadata_snapshot_id
                    for value in prepared.files
                    if value.metadata_snapshot_id is not None
                )
            ),
            plan_items=plan_items,
        )
        if existing_id != preview_job_id:
            raise ConflictError(
                "The edition-conversion preview key belongs to another operation."
            )
        return await self._store.seal_edition_conversion_preview(
            job.id,
            expected_row_revision=job.row_revision,
            preview_job_id=preview_job_id,
            preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
            bundle_json=bundle_json,
            bundle_hash=bundle_hash,
            now=now,
        )

    @staticmethod
    def _preview_plan_item(
        preview_job_id: str,
        job: EditionConversionJob,
        request: LibraryManagementImportFile,
        *,
        catalog_revision: int,
        policy_revision: str,
        profile_revision: str,
        created_at: float,
    ) -> LibraryManagementPlanItem:
        desired_json = msgspec.json.encode(request.desired_document).decode()
        source_root_id = request.replacement_root_id or request.destination_root_id
        source_relative = (
            request.replacement_relative_path
            or request.baseline_relative_path
            or Path(request.input_path).name
        )
        capability = {
            "audio_format": request.info.file_format,
            "automatic": False,
            "catalog_track_title": request.tag.title,
            "catalog_artist_name": request.tag.artist,
            "catalog_album_title": request.tag.album,
            "catalog_album_artist_name": request.tag.album_artist,
            "catalog_disc_number": request.tag.disc_number,
            "catalog_track_number": request.tag.track_number,
            "edition_conversion_action": (
                "recycle" if request.conversion_recycle_only else "publish"
            ),
        }
        diff = {
            "requires_write": True,
            "tags_changed": not request.conversion_recycle_only,
            "artwork_changed": bool(request.artifacts),
            "path_changed": (
                source_root_id != request.destination_root_id
                or source_relative != request.destination_relative_path
            ),
            "sidecars_changed": any(
                artifact.kind == "sidecar" for artifact in request.artifacts
            ),
            "edition_conversion_recycle": request.conversion_recycle_only,
        }
        artwork = [
            {
                "output_kind": artifact.kind,
                "destination_relative_path": artifact.destination_relative_path,
            }
            for artifact in request.artifacts
        ]
        fingerprint = _sha256_file(Path(request.input_path))
        return LibraryManagementPlanItem(
            job_id=preview_job_id,
            ordinal=request.ordinal,
            bundle_ordinal=0,
            local_album_id=job.local_album_id,
            local_track_id=request.replacement_local_track_id,
            expected_catalog_revision=catalog_revision,
            expected_policy_revision=policy_revision,
            expected_profile_revision=profile_revision,
            expected_root_id=source_root_id,
            expected_relative_path=source_relative,
            expected_stat_revision=fingerprint,
            expected_tag_revision=fingerprint,
            expected_file_fingerprint=fingerprint,
            source_path_identity=hashlib.sha256(
                f"{source_root_id}\x00{source_relative}".encode()
            ).hexdigest(),
            destination_root_id=request.destination_root_id,
            destination_relative_path=request.destination_relative_path,
            destination_collision_key=unicodedata.normalize(
                "NFC", request.destination_relative_path
            ).casefold(),
            desired_document_json=desired_json,
            desired_document_hash=hashlib.sha256(desired_json.encode()).hexdigest(),
            catalog_document_json=desired_json,
            catalog_document_hash=hashlib.sha256(desired_json.encode()).hexdigest(),
            artwork_choices_json=msgspec.json.encode(artwork).decode(),
            diff_json=msgspec.json.encode(diff).decode(),
            capability_json=msgspec.json.encode(capability).decode(),
            collision_json="[]",
            eligibility="eligible",
            estimated_temporary_bytes=request.info.file_size_bytes,
            created_at=created_at,
        )

    async def _ensure_temporary_space(self, job: EditionConversionJob) -> None:
        path = self._held_dir
        while not path.exists() and path != path.parent:
            path = path.parent
        usage = await asyncio.to_thread(shutil.disk_usage, path)
        if usage.free < job.required_temporary_bytes:
            raise ValidationError(
                "There is not enough temporary space to match this edition safely."
            )

    @staticmethod
    def _targets(values) -> tuple[EditionConversionTarget, ...]:  # noqa: ANN001
        targets: list[EditionConversionTarget] = []
        positions: set[tuple[int, int]] = set()
        release_tracks: set[str] = set()
        for ordinal, value in enumerate(values):
            disc = int(value.disc_number or 1)
            position = int(value.position)
            if (
                disc < 1
                or position < 1
                or not value.release_track_id
                or not value.recording_id
                or (disc, position) in positions
                or value.release_track_id in release_tracks
            ):
                raise ValidationError(
                    "The selected exact edition does not have a unique, verifiable track list."
                )
            positions.add((disc, position))
            release_tracks.add(value.release_track_id)
            targets.append(
                EditionConversionTarget(
                    job_id="pending",
                    ordinal=ordinal,
                    disc_number=disc,
                    track_number=position,
                    release_track_mbid=value.release_track_id,
                    recording_mbid=value.recording_id,
                    title=value.title,
                    duration_seconds=(
                        value.length / 1000.0 if value.length is not None else None
                    ),
                    state="pending",
                )
            )
        if not targets:
            raise ValidationError("The selected exact edition has no track list.")
        return tuple(targets)

    @classmethod
    def _match_local_files(
        cls,
        *,
        job_id: str,
        tracks: list[dict],
        targets: tuple[EditionConversionTarget, ...],
        release_mbid: str,
    ) -> tuple[EditionConversionLocalFile, ...]:
        recording_counts: dict[str, int] = {}
        for target in targets:
            key = target.recording_mbid.casefold()
            recording_counts[key] = recording_counts.get(key, 0) + 1
        candidates: dict[int, list[tuple[int, str, dict]]] = {
            target.ordinal: [] for target in targets
        }
        target_by_position = {
            (value.disc_number, value.track_number): value for value in targets
        }
        target_by_release_track = {
            value.release_track_mbid.casefold(): value for value in targets
        }
        for track in tracks:
            recordings = {
                str(value).casefold()
                for value in (
                    track.get("recording_mbid"),
                    track.get("embedded_recording_mbid"),
                    track.get("fingerprint_recording_mbid"),
                )
                if value
            }
            release_tracks = {
                str(value).casefold()
                for value in (
                    track.get("release_track_mbid"),
                    track.get("embedded_release_track_mbid"),
                )
                if value
            }
            exact_target = next(
                (
                    target_by_release_track[value]
                    for value in release_tracks
                    if value in target_by_release_track
                ),
                None,
            )
            if exact_target is not None and (
                not recordings or recordings == {exact_target.recording_mbid.casefold()}
            ):
                release_matches = {
                    str(track.get("identity_release_mbid") or "").casefold(),
                    str(track.get("embedded_release_mbid") or "").casefold(),
                }
                strength = 4 if release_mbid.casefold() in release_matches else 3
                candidates[exact_target.ordinal].append(
                    (strength, "release_track", track)
                )
                continue
            if len(recordings) != 1:
                continue
            recording = next(iter(recordings))
            recording_targets = [
                value
                for value in targets
                if value.recording_mbid.casefold() == recording
            ]
            if len(recording_targets) == 1:
                candidates[recording_targets[0].ordinal].append((2, "recording", track))
            elif recording_targets:
                positional = target_by_position.get(
                    (int(track["disc_number"]), int(track["track_number"]))
                )
                if (
                    positional is not None
                    and positional.recording_mbid.casefold() == recording
                ):
                    candidates[positional.ordinal].append(
                        (2, "recording_and_position", track)
                    )
        used: set[str] = set()
        winners: dict[str, tuple[int, str]] = {}
        for target in targets:
            ordered = sorted(
                candidates[target.ordinal],
                key=lambda value: (
                    -value[0],
                    abs(int(value[2]["disc_number"]) - target.disc_number),
                    abs(int(value[2]["track_number"]) - target.track_number),
                    str(value[2]["id"]),
                ),
            )
            winner = next(
                (value for value in ordered if str(value[2]["id"]) not in used), None
            )
            if winner is not None:
                used.add(str(winner[2]["id"]))
                winners[str(winner[2]["id"])] = (target.ordinal, winner[1])
        result: list[EditionConversionLocalFile] = []
        for track in tracks:
            track_id = str(track["id"])
            winner = winners.get(track_id)
            target_at_position = target_by_position.get(
                (int(track["disc_number"]), int(track["track_number"]))
            )
            recordings = {
                str(value).casefold()
                for value in (
                    track.get("recording_mbid"),
                    track.get("embedded_recording_mbid"),
                    track.get("fingerprint_recording_mbid"),
                )
                if value
            }
            if winner is not None:
                action = "keep"
                target_ordinal = winner[0]
                evidence = winner[1]
            elif (
                target_at_position is not None
                and recordings
                and (target_at_position.recording_mbid.casefold() not in recordings)
            ):
                action = "recycle_conflict"
                target_ordinal = target_at_position.ordinal
                evidence = "recording_conflict"
            elif any(
                target.recording_mbid.casefold() in recordings for target in targets
            ):
                action = "recycle_duplicate"
                target_ordinal = next(
                    target.ordinal
                    for target in targets
                    if target.recording_mbid.casefold() in recordings
                )
                evidence = "duplicate_recording"
            else:
                action = "recycle_extra"
                target_ordinal = None
                evidence = "no_target_identity"
            result.append(
                EditionConversionLocalFile(
                    job_id=job_id,
                    local_track_id=track_id,
                    action=action,
                    target_ordinal=target_ordinal,
                    evidence_kind=evidence,
                    expected_track_revision=int(track["row_revision"]),
                    expected_identity_revision=(
                        int(track["identity_row_revision"])
                        if track.get("identity_row_revision") is not None
                        else None
                    ),
                    expected_stat_revision=str(track["stat_revision"]),
                )
            )
        return tuple(result)

    def _response(
        self, job: EditionConversionJob, *, preflight_token: str | None = None
    ) -> EditionConversionStatusResponse:
        return EditionConversionStatusResponse(
            job_id=job.id,
            local_album_id=job.local_album_id,
            release_group_mbid=job.target_release_group_mbid,
            release_mbid=job.target_release_mbid,
            album_title=job.target_album_title,
            artist_name=job.target_artist_name,
            state=job.state,
            download_source_ready=self._preferences.is_download_source_ready(),
            required_temporary_bytes=job.required_temporary_bytes,
            kept_count=job.kept_count,
            acquire_count=job.acquire_count,
            recycle_count=job.recycle_count,
            staged_count=job.staged_count,
            failed_count=job.failed_count,
            row_revision=job.row_revision,
            created_at=job.created_at,
            updated_at=job.updated_at,
            targets=[
                EditionConversionTargetResponse(
                    ordinal=value.ordinal,
                    disc_number=value.disc_number,
                    track_number=value.track_number,
                    release_track_mbid=value.release_track_mbid,
                    recording_mbid=value.recording_mbid,
                    title=value.title,
                    duration_seconds=value.duration_seconds,
                    state=value.state,
                    kept_local_track_id=value.kept_local_track_id,
                    failure_code=value.failure_code,
                )
                for value in job.targets
            ],
            local_files=[
                EditionConversionLocalFileResponse(
                    local_track_id=value.local_track_id,
                    action=value.action,
                    target_ordinal=value.target_ordinal,
                    evidence_kind=value.evidence_kind,
                )
                for value in job.local_files
            ],
            final_preview_job_id=job.final_preview_job_id,
            preflight_token=preflight_token,
            error_code=job.error_code,
        )
