"""Preview first-management baseline restoration through the shared publisher."""

from __future__ import annotations

import asyncio
import base64
from collections.abc import Callable
import hashlib
import json
import logging
import os
from pathlib import Path, PurePosixPath
import stat
import time
import uuid

import msgspec

from api.v1.schemas.library_management import (
    PICARD_ORGANIZER_PROFILE_ID,
    settings_revision,
)
from api.v1.schemas.library_management_preview import (
    LibraryManagementBaselinePurgeImpactResponse,
    LibraryManagementBaselinePurgeRequest,
    LibraryManagementBaselinePurgeResponse,
    LibraryManagementBaselineRestorePreviewRequest,
    LibraryManagementPreviewCreatedResponse,
)
from core.exceptions import (
    ConflictError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.audio.metadata_engine import AudioMetadataEngine
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import (
    MANAGEMENT_PERSISTENCE_BATCH_SIZE,
    NativeLibraryStore,
)
from models.audio_metadata import (
    DesiredAudioDocument,
    DesiredAudioField,
    SemanticTagSnapshot,
)
from models.library_management import (
    BASELINE_SNAPSHOT_CORRUPT,
    BASELINE_SNAPSHOT_MISSING,
    BASELINE_UNAVAILABLE,
    FILE_CHANGED,
    IDENTITY_NOT_ACCEPTED,
    PATH_COLLISION_DIFFERENT,
    PATH_COLLISION_IDENTICAL,
    ROOT_UNAVAILABLE,
    MANAGEMENT_RECYCLE_ROOT_ID,
    LibraryManagementBaseline,
    LibraryManagementJobSnapshot,
    LibraryManagementPlanItem,
)
from models.library_management_planning import (
    LibraryManagementCatalogFilter,
    LibraryManagementSelection,
    LibraryManagementSelectionCursor,
    LibraryManagementSelectionSubject,
    NormalizedLibraryManagementSelection,
    naming_policy_revision,
)
from models.library_work import OperationJob
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.library_management_planner import LibraryManagementPlanner
from services.native.library_management_undo_service import (
    LibraryManagementUndoService,
    _restoration_audio_diff,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.preferences_service import PreferencesService

_BASELINE_RESTORE_NAMESPACE = uuid.UUID("42ad30b8-fb3b-4edb-998a-cbfc4896a43e")
logger = logging.getLogger(__name__)


def _json(value: object) -> str:
    return json.dumps(
        msgspec.to_builtins(value),
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )


def _token(job_id: str, idempotency_key: str) -> str:
    digest = hashlib.sha256(f"{job_id}\x00{idempotency_key}".encode()).digest()
    return base64.urlsafe_b64encode(digest).rstrip(b"=").decode()


def _display_document(
    subject: LibraryManagementSelectionSubject,
    restore_snapshot: SemanticTagSnapshot | None,
) -> DesiredAudioDocument:
    values = (
        {field.name: field.value for field in restore_snapshot.metadata.fields}
        if restore_snapshot is not None
        else {}
    )
    for name, value in (
        ("title", subject.track_title),
        ("artist", subject.artist_name),
        ("album", subject.album_title),
        ("album_artist", subject.album_artist_name),
        ("disc_number", subject.disc_number),
        ("track_number", subject.track_number),
        ("date", str(subject.year) if subject.year is not None else None),
    ):
        if value is None:
            continue
        current = values.get(name)
        missing = current is None or current == "" or current == () or current == []
        if name in {"disc_number", "track_number"}:
            missing = (
                not isinstance(current, int)
                or isinstance(current, bool)
                or current <= 0
            )
        if missing:
            values[name] = value
    return DesiredAudioDocument(
        fields=tuple(
            DesiredAudioField(name=name, action="unchanged", value=value)
            for name, value in values.items()
        ),
        artist_display=(
            (restore_snapshot.metadata.artist_display or subject.artist_name)
            if restore_snapshot is not None
            else subject.artist_name
        ),
        album_artist_display=(
            (
                restore_snapshot.metadata.album_artist_display
                or subject.album_artist_name
            )
            if restore_snapshot is not None
            else subject.album_artist_name
        ),
    )


def _baseline_catalog_document(
    baseline: LibraryManagementBaseline | None,
    subject: LibraryManagementSelectionSubject,
    restore_snapshot: SemanticTagSnapshot | None,
) -> DesiredAudioDocument:
    if baseline is None or baseline.catalog_document_json is None:
        return _display_document(subject, restore_snapshot)
    expected_hash = baseline.catalog_document_hash
    if (
        expected_hash is None
        or hashlib.sha256(baseline.catalog_document_json.encode()).hexdigest()
        != expected_hash
    ):
        raise ValidationError("The first-management catalog snapshot is invalid.")
    try:
        return msgspec.json.decode(
            baseline.catalog_document_json.encode(), type=DesiredAudioDocument
        )
    except msgspec.DecodeError as error:
        raise ValidationError(
            "The first-management catalog snapshot is invalid."
        ) from error


class LibraryManagementBaselineService:
    def __init__(
        self,
        store: NativeLibraryStore,
        preferences: PreferencesService,
        audio: AudioMetadataEngine,
        blobs: LibraryManagementBlobStore,
        filesystem: LibraryFilesystemCoordinator,
        undo: LibraryManagementUndoService,
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._preferences = preferences
        self._audio = audio
        self._blobs = blobs
        self._filesystem = filesystem
        self._undo = undo
        self._clock = clock

    async def purge_impact(self) -> LibraryManagementBaselinePurgeImpactResponse:
        return LibraryManagementBaselinePurgeImpactResponse(
            **await self._store.management_baseline_purge_impact()
        )

    async def purge(
        self,
        request: LibraryManagementBaselinePurgeRequest,
        actor_user_id: str,
    ) -> LibraryManagementBaselinePurgeResponse:
        if request.typed_confirmation != "CONFIRM":
            raise ValidationError(
                "Type CONFIRM exactly to permanently purge baselines."
            )
        if not request.idempotency_key.strip():
            raise ValidationError("A baseline purge idempotency key is required.")
        result = await self._store.purge_management_baselines(
            impact_token=request.impact_token,
            expected_catalog_revision=request.expected_catalog_revision,
            idempotency_key=request.idempotency_key,
            actor_user_id=actor_user_id,
            now=self._clock(),
        )
        cleaned = 0
        while True:
            cleanup = await self._blobs.cleanup(
                older_than=self._clock() + 1.0,
                limit=MANAGEMENT_PERSISTENCE_BATCH_SIZE,
            )
            removed = (
                cleanup.unreferenced_blobs_removed + cleanup.unledgered_files_removed
            )
            cleaned += removed
            if removed + cleanup.temporary_files_removed == 0:
                break
        return LibraryManagementBaselinePurgeResponse(
            purged_baseline_count=int(result["purged_baseline_count"]),
            detached_reference_count=int(result["detached_reference_count"]),
            cleaned_blob_count=cleaned,
            existing=bool(result["existing"]),
        )

    async def create_restore_preview(
        self,
        request: LibraryManagementBaselineRestorePreviewRequest,
        actor_user_id: str,
    ) -> LibraryManagementPreviewCreatedResponse:
        if not request.idempotency_key.strip():
            raise ValidationError("A baseline restore idempotency key is required.")
        settings = self._preferences.get_library_management_settings_raw()
        management_revision = settings_revision(settings)
        if management_revision != request.expected_settings_revision:
            raise StaleRevisionError(
                "Library management settings changed before baseline restore preview."
            )
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        if resolver.policy_revision != request.expected_policy_revision:
            raise StaleRevisionError(
                "Library policy changed before baseline restore preview."
            )
        profile = next(
            (
                value
                for value in settings.profiles
                if value.id == PICARD_ORGANIZER_PROFILE_ID
            ),
            settings.profiles[0],
        )
        pinned = LibraryManagementPlanner.pin_profile(settings, profile)
        selection = LibraryManagementPlanner.normalize_selection(
            self._selection(request), profile, resolver
        )
        selection = msgspec.structs.replace(selection, expand_album_bundles=True)
        now = self._clock()
        job_id = str(uuid.uuid5(_BASELINE_RESTORE_NAMESPACE, request.idempotency_key))
        preview_token = _token(job_id, request.idempotency_key)
        catalog_revision = await self._store.get_catalog_revision()
        snapshot = LibraryManagementJobSnapshot(
            job_id=job_id,
            mode="baseline_restore",
            origin="manual",
            phase="planning",
            selection_json=msgspec.json.encode(selection).decode(),
            profile_revision=profile.revision,
            settings_revision=management_revision,
            naming_revision=naming_policy_revision(pinned),
            policy_revision=resolver.policy_revision,
            catalog_revision=catalog_revision,
            profile_snapshot_json=msgspec.json.encode(pinned).decode(),
            preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
            preview_created_at=now,
            preview_expires_at=now + settings.preview_retention_hours * 60 * 60,
            created_at=now,
            updated_at=now,
        )
        existing_id, created = await self._store.create_library_management_job(
            OperationJob(
                id=job_id,
                kind="library_management",
                requested_by_user_id=actor_user_id,
                input_catalog_revision=catalog_revision,
                idempotency_key=request.idempotency_key,
                created_at=now,
            ),
            snapshot,
        )
        if not created:
            existing = await self._store.get_library_management_job_snapshot(
                existing_id
            )
            existing_job = await self._store.get_operation_job(existing_id)
            if (
                existing is None
                or existing_job is None
                or existing.mode != "baseline_restore"
                or existing.selection_json != snapshot.selection_json
                or existing.preview_token_hash != snapshot.preview_token_hash
                or existing_job["requested_by_user_id"] != actor_user_id
            ):
                raise ConflictError(
                    "The idempotency key belongs to another baseline restore request."
                )
            return LibraryManagementPreviewCreatedResponse(
                job_id=existing_id,
                preview_token=preview_token,
                created_at=existing.preview_created_at or existing.created_at,
                expires_at=existing.preview_expires_at or existing.created_at,
                existing=True,
            )
        return LibraryManagementPreviewCreatedResponse(
            job_id=job_id,
            preview_token=preview_token,
            created_at=now,
            expires_at=now + settings.preview_retention_hours * 60 * 60,
        )

    async def run_claimed_preview(self, job: dict, worker_id: str) -> None:
        job_id = str(job["id"])
        snapshot = await self._store.get_library_management_job_snapshot(job_id)
        if (
            snapshot is None
            or snapshot.mode != "baseline_restore"
            or snapshot.phase != "planning"
        ):
            raise ValidationError(
                "The claimed operation is not a baseline restore preview."
            )
        selection = msgspec.json.decode(
            snapshot.selection_json.encode(),
            type=NormalizedLibraryManagementSelection,
        )
        cursor = (
            msgspec.json.decode(
                snapshot.staging_cursor.encode(),
                type=LibraryManagementSelectionCursor,
            )
            if snapshot.staging_cursor
            else None
        )
        snapshot_revision = snapshot.row_revision
        while True:
            controlled = await self._store.checkpoint_operation_control(
                job_id, worker_id, now=self._clock()
            )
            if controlled is not None and controlled["state"] != "running":
                return
            page = await self._store.list_library_management_selection_page(
                selection,
                cursor=cursor,
                limit=MANAGEMENT_PERSISTENCE_BATCH_SIZE,
            )
            if page.subjects:
                items = [
                    await self._plan_item(snapshot, subject)
                    for subject in page.subjects
                ]
                snapshot_revision = (
                    await self._store.append_library_management_plan_items(
                        job_id,
                        items,
                        expected_snapshot_revision=snapshot_revision,
                        staging_cursor=(
                            msgspec.json.encode(page.next_cursor).decode()
                            if page.next_cursor is not None
                            else None
                        ),
                    )
                )
            if page.complete:
                await self._store.finalize_library_management_preview(
                    job_id,
                    worker_id,
                    expected_snapshot_revision=snapshot_revision,
                    now=self._clock(),
                )
                return
            cursor = page.next_cursor
            renewed = await self._store.heartbeat_operation_job(
                job_id,
                worker_id,
                now=self._clock(),
                lease_seconds=60.0,
            )
            if not renewed:
                raise StaleRevisionError(
                    "The baseline restore preview lease changed during planning."
                )

    async def _plan_item(
        self,
        snapshot: LibraryManagementJobSnapshot,
        subject: LibraryManagementSelectionSubject,
    ) -> LibraryManagementPlanItem:
        now = self._clock()
        baseline = await self._store.get_management_baseline(subject.local_track_id)
        management_settings = self._preferences.get_library_management_settings_raw()
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        roots = {
            value.id: Path(value.path) for value in resolver.settings.library_roots
        }
        if management_settings.recycle_bin_path:
            roots[MANAGEMENT_RECYCLE_ROOT_ID] = Path(
                management_settings.recycle_bin_path
            )
        source_root = roots.get(subject.root_id)
        destination_root = (
            roots.get(baseline.original_root_id) if baseline is not None else None
        )
        reason = BASELINE_UNAVAILABLE if baseline is None else None
        if source_root is None or (baseline is not None and destination_root is None):
            reason = reason or ROOT_UNAVAILABLE
        source = (
            self._safe_path(source_root, subject.relative_path)
            if source_root is not None
            else Path(subject.file_path)
        )
        destination = (
            self._safe_path(destination_root, baseline.original_relative_path)
            if baseline is not None and destination_root is not None
            else source
        )
        source_fingerprint = "missing"
        collisions: list[dict] = []
        ancillary: list[dict] = []
        current = None
        restore_snapshot: SemanticTagSnapshot | None = None
        if reason is None and baseline is not None:
            try:
                async with self._filesystem.read_many(
                    {subject.root_id, baseline.original_root_id}
                ):
                    source_fingerprint = await asyncio.to_thread(
                        self._hash_file, source
                    )
                    current = await asyncio.to_thread(self._audio.read, source)
                    try:
                        restore_bytes = await self._blobs.read_bytes(
                            baseline.semantic_snapshot_blob_sha256
                        )
                        restore_snapshot = msgspec.json.decode(
                            restore_bytes, type=SemanticTagSnapshot
                        )
                    except ValidationError as error:
                        # F-177: a lost snapshot blob is baseline-data loss,
                        # not an edited audio file; report it honestly.
                        reason = reason or BASELINE_SNAPSHOT_MISSING
                        logger.warning(
                            "Baseline restore preview lost its semantic "
                            "snapshot blob: baseline_id=%s sha256=%s",
                            baseline.id,
                            baseline.semantic_snapshot_blob_sha256,
                        )
                        raise
                    except (
                        ConflictError,
                        msgspec.DecodeError,
                        msgspec.ValidationError,
                    ) as error:
                        reason = reason or BASELINE_SNAPSHOT_CORRUPT
                        logger.warning(
                            "Baseline restore preview found a corrupt "
                            "semantic snapshot blob: baseline_id=%s sha256=%s",
                            baseline.id,
                            baseline.semantic_snapshot_blob_sha256,
                        )
                        raise
                    if (
                        restore_snapshot.probe.detected_format
                        != current.probe.detected_format
                    ):
                        reason = FILE_CHANGED
                    elif destination != source and (
                        destination.exists() or destination.is_symlink()
                    ):
                        destination_hash = await asyncio.to_thread(
                            self._hash_file, destination
                        )
                        reason = (
                            PATH_COLLISION_IDENTICAL
                            if destination_hash == source_fingerprint
                            else PATH_COLLISION_DIFFERENT
                        )
                        collisions.append(
                            {
                                "classification": "same_path_same_content"
                                if reason == PATH_COLLISION_IDENTICAL
                                else "same_path_different_content",
                                "destination_root_id": baseline.original_root_id,
                                "destination_relative_path": (
                                    baseline.original_relative_path
                                ),
                            }
                        )
                    if reason is None:
                        ancillary = await self._undo.plan_ancillary_restore(
                            baseline.ancillary_snapshot_json,
                            roots,
                            source,
                            destination,
                        )
            except (
                OSError,
                ValidationError,
                ConflictError,
                StaleRevisionError,
                msgspec.DecodeError,
                msgspec.ValidationError,
            ):
                reason = reason or FILE_CHANGED

        identity = await self._store.get_accepted_library_management_identity(
            subject.local_album_id,
            local_track_ids=(subject.local_track_id,),
        )
        mapped = (
            identity.tracks[0] if identity is not None and identity.tracks else None
        )
        if (
            identity is None
            or identity.identity_revision is None
            or identity.release_mbid is None
            or mapped is None
            or mapped.identity_revision is None
            or mapped.recording_mbid is None
            or mapped.release_track_mbid is None
        ):
            reason = reason or IDENTITY_NOT_ACCEPTED
        _album_overrides, album_revision = await self._store.list_management_overrides(
            subject_kind="album", subject_id=subject.local_album_id
        )
        _track_overrides, track_revision = await self._store.list_management_overrides(
            subject_kind="track", subject_id=subject.local_track_id
        )
        override_revision = hashlib.sha256(
            f"{album_revision}\x00{track_revision}".encode()
        ).hexdigest()
        artwork_choices = [
            value["artwork_choice"]
            for value in ancillary
            if value.get("artwork_choice") is not None
        ]
        collisions.extend(
            value["collision"]
            for value in ancillary
            if value.get("collision") is not None
        )
        tags_changed, embedded_artwork_changed, field_mutations, restoration = (
            _restoration_audio_diff(
                current,
                restore_snapshot,
                scope="first_management_baseline",
            )
        )
        artwork_changed = (
            any(value.get("kind") == "external_art" for value in ancillary)
            or embedded_artwork_changed
        )
        diff = {
            "requires_write": True,
            "tags_changed": tags_changed,
            "artwork_changed": artwork_changed,
            "path_changed": destination != source,
            "sidecars_changed": any(
                value.get("kind") == "sidecar" for value in ancillary
            ),
            "restore_snapshot_blob_sha256": (
                baseline.semantic_snapshot_blob_sha256 if baseline is not None else None
            ),
            "field_mutations": field_mutations,
            "restoration": restoration,
            "restore_management_state": {
                "baseline_id": baseline.id if baseline is not None else None,
                "applied_profile_id": None,
                "applied_profile_revision": None,
                "applied_projection_hash": None,
                "applied_naming_script_revision": None,
                "applied_override_revision": None,
                "managed_root_id": (
                    baseline.original_root_id
                    if baseline is not None
                    else subject.root_id
                ),
                "last_outcome": "restored",
                "last_reason_code": None,
            },
            "baseline_id": baseline.id if baseline is not None else None,
            "sidecars": [
                value["sidecar"]
                for value in ancillary
                if value.get("sidecar") is not None
            ],
            "delete_outputs": [
                value["delete_output"]
                for value in ancillary
                if value.get("delete_output") is not None
            ],
        }
        desired = _baseline_catalog_document(baseline, subject, restore_snapshot)
        desired_json = msgspec.json.encode(desired).decode()
        destination_relative = (
            baseline.original_relative_path if baseline is not None else None
        )
        return LibraryManagementPlanItem(
            job_id=snapshot.job_id,
            ordinal=subject.ordinal,
            bundle_ordinal=subject.bundle_ordinal,
            local_album_id=subject.local_album_id,
            local_track_id=subject.local_track_id,
            expected_album_revision=subject.album_revision,
            expected_track_revision=subject.track_revision,
            expected_identity_revision=(mapped.identity_revision if mapped else None),
            expected_album_identity_revision=(
                identity.identity_revision if identity else None
            ),
            expected_override_revision=override_revision,
            expected_catalog_revision=snapshot.catalog_revision,
            expected_policy_revision=snapshot.policy_revision,
            expected_profile_revision=snapshot.profile_revision,
            expected_root_id=subject.root_id,
            expected_relative_path=subject.relative_path,
            expected_stat_revision=subject.stat_revision,
            expected_tag_revision=subject.tag_revision,
            expected_file_fingerprint=source_fingerprint,
            source_path_identity=str(source),
            destination_root_id=(
                baseline.original_root_id if baseline is not None else None
            ),
            destination_relative_path=destination_relative,
            destination_collision_key=(
                self._collision_key(destination_relative)
                if destination_relative is not None
                else None
            ),
            desired_document_json=desired_json,
            desired_document_hash=hashlib.sha256(desired_json.encode()).hexdigest(),
            artwork_choices_json=_json(artwork_choices),
            diff_json=_json(diff),
            capability_json=_json(
                {
                    "audio_format": subject.file_format,
                    "restoration": restore_snapshot is not None,
                    "album_artwork_version": subject.album_artwork_version,
                }
            ),
            collision_json=_json(collisions),
            eligibility="blocked" if reason else "eligible",
            reason_code=reason,
            estimated_temporary_bytes=subject.file_size_bytes,
            created_at=now,
        )

    @staticmethod
    def _selection(
        request: LibraryManagementBaselineRestorePreviewRequest,
    ) -> LibraryManagementSelection:
        value = request.selection
        catalog_filter = (
            LibraryManagementCatalogFilter(
                search=value.catalog_filter.search,
                genre=value.catalog_filter.genre,
                from_year=value.catalog_filter.from_year,
                to_year=value.catalog_filter.to_year,
                artist_ids=tuple(value.catalog_filter.artist_ids),
                album_artist_only=value.catalog_filter.album_artist_only,
            )
            if value.catalog_filter is not None
            else None
        )
        return LibraryManagementSelection(
            kind=value.kind,
            ids=tuple(value.ids),
            catalog_filter=catalog_filter,
        )

    @staticmethod
    def _safe_path(root: Path, relative: str) -> Path:
        pure = PurePosixPath(relative)
        if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
            raise ValidationError("A baseline restore path is unsafe.")
        if not root.exists() or stat.S_ISLNK(root.lstat().st_mode):
            raise ValidationError("A baseline restore root is unsafe.")
        current = root
        for part in pure.parts[:-1]:
            current = current / part
            if current.exists():
                metadata = current.lstat()
                if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                    raise ValidationError("A baseline restore path contains a symlink.")
        return root.joinpath(*pure.parts)

    @staticmethod
    def _hash_file(path: Path) -> str:
        digest = hashlib.sha256()
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                raise OSError("The baseline restore path is not a regular file.")
            while chunk := handle.read(1024 * 1024):
                digest.update(chunk)
        return digest.hexdigest()

    @staticmethod
    def _collision_key(value: str) -> str:
        import unicodedata

        return unicodedata.normalize("NFC", value).casefold()
