"""Preview immediate per-operation undo from durable before-state snapshots."""

from __future__ import annotations

import asyncio
import base64
from collections.abc import Callable
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat
import time
import uuid

import msgspec

from api.v1.schemas.library_management import settings_revision
from api.v1.schemas.library_management_preview import (
    LibraryManagementPreviewCreatedResponse,
    LibraryManagementUndoPreviewRequest,
)
from core.exceptions import (
    ConflictError,
    ResourceNotFoundError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.audio.metadata_engine import AudioMetadataEngine
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import (
    MANAGEMENT_PERSISTENCE_BATCH_SIZE,
    NativeLibraryStore,
)
from models.audio_metadata import SemanticTagSnapshot
from models.library_management import (
    FILE_CHANGED,
    IDENTITY_NOT_ACCEPTED,
    PATH_COLLISION_DIFFERENT,
    ROOT_UNAVAILABLE,
    UNDO_EXPIRED,
    MANAGEMENT_RECYCLE_ROOT_ID,
    LibraryManagementJobSnapshot,
    LibraryManagementOperationSnapshot,
    LibraryManagementPlanItem,
)
from models.library_work import OperationJob
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.preferences_service import PreferencesService

_UNDO_NAMESPACE = uuid.UUID("339c4fcf-a060-44e3-9e0f-e78751e24169")


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


class LibraryManagementUndoService:
    def __init__(
        self,
        store: NativeLibraryStore,
        preferences: PreferencesService,
        audio: AudioMetadataEngine,
        blobs: LibraryManagementBlobStore,
        filesystem: LibraryFilesystemCoordinator,
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._preferences = preferences
        self._audio = audio
        self._blobs = blobs
        self._filesystem = filesystem
        self._clock = clock

    async def create_preview(
        self,
        source_job_id: str,
        request: LibraryManagementUndoPreviewRequest,
        actor_user_id: str,
    ) -> LibraryManagementPreviewCreatedResponse:
        if not request.idempotency_key.strip():
            raise ValidationError("An undo idempotency key is required.")
        source_job = await self._store.get_operation_job(source_job_id)
        source_snapshot = await self._store.get_library_management_job_snapshot(
            source_job_id
        )
        if source_job is None or source_snapshot is None:
            raise ResourceNotFoundError("Library Management operation not found.")
        if source_job["state"] not in {"succeeded", "stopped"}:
            raise ValidationError(
                "Only a completed or stopped operation can be undone."
            )
        if int(source_job["succeeded_count"]) == 0:
            raise ValidationError("The operation has no completed changes to undo.")
        if int(source_job["row_revision"]) != request.expected_operation_row_revision:
            raise StaleRevisionError("The operation changed before undo preview.")

        settings = self._preferences.get_library_management_settings_raw()
        management_revision = settings_revision(settings)
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        now = self._clock()
        job_id = str(uuid.uuid5(_UNDO_NAMESPACE, request.idempotency_key))
        preview_token = _token(job_id, request.idempotency_key)
        catalog_revision = await self._store.get_catalog_revision()
        selection = {
            "source_operation_job_id": source_job_id,
            "source_operation_row_revision": request.expected_operation_row_revision,
        }
        snapshot = LibraryManagementJobSnapshot(
            job_id=job_id,
            mode="undo",
            origin="manual",
            phase="planning",
            selection_json=_json(selection),
            profile_revision=source_snapshot.profile_revision,
            settings_revision=management_revision,
            naming_revision=source_snapshot.naming_revision,
            policy_revision=resolver.policy_revision,
            catalog_revision=catalog_revision,
            profile_snapshot_json=source_snapshot.profile_snapshot_json,
            preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
            preview_created_at=now,
            preview_expires_at=now + settings.preview_retention_hours * 60 * 60,
            linked_operation_job_id=source_job_id,
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
                or existing.mode != "undo"
                or existing.linked_operation_job_id != source_job_id
                or existing.preview_token_hash != snapshot.preview_token_hash
                or existing_job["requested_by_user_id"] != actor_user_id
            ):
                raise ConflictError(
                    "The idempotency key belongs to another undo request."
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
        if snapshot is None or snapshot.mode != "undo" or snapshot.phase != "planning":
            raise ValidationError("The claimed operation is not an undo preview.")
        selection = json.loads(snapshot.selection_json)
        source_job_id = str(selection["source_operation_job_id"])
        expected_source_revision = int(selection["source_operation_row_revision"])
        cursor = json.loads(snapshot.staging_cursor) if snapshot.staging_cursor else {}
        after_work = int(cursor.get("work_ordinal", -1))
        after_track = str(cursor.get("track_id", ""))
        snapshot_revision = snapshot.row_revision
        while True:
            controlled = await self._store.checkpoint_operation_control(
                job_id, worker_id, now=self._clock()
            )
            if controlled is not None and controlled["state"] != "running":
                return
            source_job = await self._store.get_operation_job(source_job_id)
            if (
                source_job is None
                or source_job["state"] not in {"succeeded", "stopped"}
                or int(source_job["row_revision"]) != expected_source_revision
            ):
                raise StaleRevisionError(
                    "The source operation changed during undo planning."
                )
            rows = await self._store.list_management_operation_snapshots(
                source_job_id,
                after_work_ordinal=after_work,
                after_track_id=after_track,
                limit=MANAGEMENT_PERSISTENCE_BATCH_SIZE,
            )
            if not rows:
                await self._store.finalize_library_management_preview(
                    job_id,
                    worker_id,
                    expected_snapshot_revision=snapshot_revision,
                    now=self._clock(),
                )
                return
            items = [
                await self._plan_item(snapshot, source_job_id, value) for value in rows
            ]
            after_work = rows[-1].work_ordinal
            after_track = rows[-1].local_track_id
            snapshot_revision = await self._store.append_library_management_plan_items(
                job_id,
                items,
                expected_snapshot_revision=snapshot_revision,
                staging_cursor=json.dumps(
                    {"work_ordinal": after_work, "track_id": after_track},
                    separators=(",", ":"),
                    sort_keys=True,
                ),
            )
            renewed = await self._store.heartbeat_operation_job(
                job_id,
                worker_id,
                now=self._clock(),
                lease_seconds=60.0,
            )
            if not renewed:
                raise StaleRevisionError(
                    "The undo preview lease changed during planning."
                )

    async def _plan_item(
        self,
        snapshot: LibraryManagementJobSnapshot,
        source_job_id: str,
        before: LibraryManagementOperationSnapshot,
    ) -> LibraryManagementPlanItem:
        original = await self._store.get_library_management_plan_item_for_track(
            source_job_id, before.local_track_id
        )
        journal = await self._store.get_management_audio_journal(
            source_job_id, before.local_track_id
        )
        track = await self._store.get_target_track(before.local_track_id)
        now = self._clock()
        if original is None or journal is None or track is None:
            return self._stale_item(snapshot, before, original, FILE_CHANGED, now)
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
        source_root_id = str(track["root_id"])
        destination_root_id = before.before_root_id
        source_root = roots.get(source_root_id)
        destination_root = roots.get(destination_root_id)
        if source_root is None or destination_root is None:
            return self._stale_item(snapshot, before, original, ROOT_UNAVAILABLE, now)
        source = self._safe_path(source_root, str(track["relative_path"]))
        destination = self._safe_path(destination_root, before.before_relative_path)
        reason: str | None = None
        collision: list[dict] = []
        ancillary: list[dict] = []
        try:
            async with self._filesystem.read_many(
                {source_root_id, destination_root_id}
            ):
                fingerprint = await asyncio.to_thread(self._hash_file, source)
                if (
                    before.expires_at <= now
                    or journal.staged_fingerprint is None
                    or fingerprint != journal.staged_fingerprint
                    or before.after_root_id != source_root_id
                    or before.after_relative_path != str(track["relative_path"])
                ):
                    reason = UNDO_EXPIRED if before.expires_at <= now else FILE_CHANGED
                elif destination != source and destination.exists():
                    reason = PATH_COLLISION_DIFFERENT
                    collision.append(
                        {
                            "classification": "destination_created_after_preview",
                            "destination_root_id": destination_root_id,
                            "destination_relative_path": before.before_relative_path,
                        }
                    )
                ancillary = await self.plan_ancillary_restore(
                    before.ancillary_snapshot_json, roots, source, destination
                )
        except (OSError, ValidationError, ConflictError):
            reason = reason or FILE_CHANGED

        identity = await self._store.get_accepted_library_management_identity(
            str(track["local_album_id"]),
            local_track_ids=(before.local_track_id,),
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
        album_overrides, album_revision = await self._store.list_management_overrides(
            subject_kind="album", subject_id=str(track["local_album_id"])
        )
        track_overrides, track_revision = await self._store.list_management_overrides(
            subject_kind="track", subject_id=before.local_track_id
        )
        del album_overrides, track_overrides
        override_revision = hashlib.sha256(
            f"{album_revision}\x00{track_revision}".encode()
        ).hexdigest()
        try:
            restore_bytes = await self._blobs.read_bytes(
                before.semantic_snapshot_blob_sha256
            )
            restore_snapshot = msgspec.json.decode(
                restore_bytes, type=SemanticTagSnapshot
            )
            if restore_snapshot.probe.detected_format != str(track["file_format"]):
                reason = reason or FILE_CHANGED
        except (
            ValidationError,
            ConflictError,
            msgspec.DecodeError,
            msgspec.ValidationError,
        ):
            reason = reason or FILE_CHANGED

        diff = {
            "requires_write": True,
            "tags_changed": True,
            "artwork_changed": bool(ancillary),
            "path_changed": (
                source_root_id != destination_root_id
                or str(track["relative_path"]) != before.before_relative_path
            ),
            "sidecars_changed": any(
                value.get("kind") == "sidecar" for value in ancillary
            ),
            "restore_snapshot_blob_sha256": before.semantic_snapshot_blob_sha256,
            "restore_management_state": json.loads(before.before_management_state_json),
            "undo_source_job_id": source_job_id,
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
        artwork_choices = [
            value["artwork_choice"]
            for value in ancillary
            if value.get("artwork_choice") is not None
        ]
        collision.extend(
            value["collision"]
            for value in ancillary
            if value.get("collision") is not None
        )
        return LibraryManagementPlanItem(
            job_id=snapshot.job_id,
            ordinal=original.ordinal,
            bundle_ordinal=before.work_ordinal,
            local_album_id=str(track["local_album_id"]),
            local_track_id=before.local_track_id,
            expected_album_revision=(identity.album_revision if identity else None),
            expected_track_revision=int(track["row_revision"]),
            expected_identity_revision=(mapped.identity_revision if mapped else None),
            expected_album_identity_revision=(
                identity.identity_revision if identity else None
            ),
            expected_override_revision=override_revision,
            expected_catalog_revision=snapshot.catalog_revision,
            expected_policy_revision=snapshot.policy_revision,
            expected_profile_revision=snapshot.profile_revision,
            expected_root_id=source_root_id,
            expected_relative_path=str(track["relative_path"]),
            expected_stat_revision=str(track["stat_revision"]),
            expected_tag_revision=str(track["tag_revision"]),
            expected_file_fingerprint=(journal.staged_fingerprint or "missing"),
            source_path_identity=str(source),
            destination_root_id=destination_root_id,
            destination_relative_path=before.before_relative_path,
            destination_collision_key=self._collision_key(before.before_relative_path),
            desired_document_json=original.desired_document_json,
            desired_document_hash=hashlib.sha256(_json(diff).encode()).hexdigest(),
            artwork_choices_json=_json(artwork_choices),
            diff_json=_json(diff),
            capability_json=_json(
                {"audio_format": str(track["file_format"]), "restoration": True}
            ),
            collision_json=_json(collision),
            eligibility="stale" if reason else "eligible",
            reason_code=reason,
            estimated_temporary_bytes=int(track["file_size_bytes"] or 0),
            created_at=now,
        )

    async def plan_ancillary_restore(
        self,
        ancillary_snapshot_json: str,
        roots: dict[str, Path],
        source_audio: Path,
        destination_audio: Path,
    ) -> list[dict]:
        try:
            manifest = json.loads(ancillary_snapshot_json)
        except json.JSONDecodeError as error:
            raise ValidationError("The undo ancillary snapshot is invalid.") from error
        if not isinstance(manifest, list):
            raise ValidationError("The undo ancillary snapshot is invalid.")
        values: list[dict] = []
        for entry in manifest:
            if not isinstance(entry, dict):
                raise ValidationError("The undo ancillary snapshot is invalid.")
            after_root = roots.get(str(entry.get("after_root_id")))
            if after_root is None:
                raise ValidationError("An undo ancillary root is unavailable.")
            current = self._safe_path(after_root, str(entry.get("after_relative_path")))
            after_exists = bool(entry.get("after_exists", True))
            if not after_exists:
                if current.exists() or current.is_symlink():
                    raise StaleRevisionError("An undo ancillary destination changed.")
                before_root = roots.get(str(entry.get("before_root_id")))
                if before_root is None:
                    raise ValidationError("An undo artwork root is unavailable.")
                target_relative = str(entry.get("before_relative_path"))
                blob_sha256 = str(entry.get("blob_sha256"))
                await self._blobs.read_bytes(blob_sha256)
                values.append(
                    {
                        "kind": "external_art",
                        "artwork_choice": {
                            "output_kind": "external",
                            "blob_sha256": blob_sha256,
                            "destination_relative_path": target_relative,
                        },
                    }
                )
                continue
            current_hash = await asyncio.to_thread(self._hash_file, current)
            if current_hash != entry.get("after_blob_sha256"):
                raise StaleRevisionError("An undo ancillary file changed.")
            if not bool(entry.get("before_exists", False)):
                values.append(
                    {
                        "kind": str(entry.get("kind")),
                        "delete_output": {
                            "root_id": str(entry["after_root_id"]),
                            "relative_path": str(entry["after_relative_path"]),
                            "sha256": current_hash,
                        },
                    }
                )
                continue
            if entry.get("kind") == "sidecar":
                before_root = roots.get(str(entry.get("before_root_id")))
                if before_root is None:
                    raise ValidationError("An undo sidecar root is unavailable.")
                target = self._safe_path(
                    before_root, str(entry.get("before_relative_path"))
                )
                if target != current and target.exists():
                    raise ConflictError("An undo sidecar destination is occupied.")
                values.append(
                    {
                        "kind": "sidecar",
                        "sidecar": {
                            "source_relative_path": current.relative_to(
                                source_audio.parent
                            ).as_posix(),
                            "destination_relative_path": target.relative_to(
                                destination_audio.parent
                            ).as_posix(),
                            "sha256": current_hash,
                            "mtime_ns": current.stat().st_mtime_ns,
                        },
                    }
                )
            elif entry.get("kind") == "external_art" and entry.get("before_exists"):
                before_root = roots.get(str(entry.get("before_root_id")))
                if before_root is None:
                    raise ValidationError("An undo artwork root is unavailable.")
                target_relative = str(entry.get("before_relative_path"))
                target = self._safe_path(before_root, target_relative)
                if target != current and target.exists():
                    raise ConflictError("An undo artwork destination is occupied.")
                blob_sha256 = str(entry.get("blob_sha256"))
                await self._blobs.read_bytes(blob_sha256)
                values.append(
                    {
                        "kind": "external_art",
                        "artwork_choice": {
                            "output_kind": "external",
                            "blob_sha256": blob_sha256,
                            "destination_relative_path": target_relative,
                        },
                        "collision": (
                            {
                                "classification": "configured_external_artwork_replacement",
                                "destination_relative_path": target_relative,
                                "existing_file_fingerprint": current_hash,
                            }
                            if target == current
                            else None
                        ),
                        "delete_output": (
                            {
                                "root_id": str(entry["after_root_id"]),
                                "relative_path": str(entry["after_relative_path"]),
                                "sha256": current_hash,
                            }
                            if target != current
                            else None
                        ),
                    }
                )
            elif entry.get("kind") == "external_art":
                values.append(
                    {
                        "kind": "external_art",
                        "delete_output": {
                            "root_id": str(entry["after_root_id"]),
                            "relative_path": str(entry["after_relative_path"]),
                            "sha256": current_hash,
                        },
                    }
                )
        return values

    @staticmethod
    def _stale_item(
        snapshot: LibraryManagementJobSnapshot,
        before: LibraryManagementOperationSnapshot,
        original: LibraryManagementPlanItem | None,
        reason: str,
        now: float,
    ) -> LibraryManagementPlanItem:
        placeholder = original.desired_document_json if original is not None else "{}"
        return LibraryManagementPlanItem(
            job_id=snapshot.job_id,
            ordinal=original.ordinal if original is not None else before.work_ordinal,
            bundle_ordinal=before.work_ordinal,
            local_album_id=original.local_album_id if original is not None else None,
            local_track_id=before.local_track_id,
            expected_catalog_revision=snapshot.catalog_revision,
            expected_policy_revision=snapshot.policy_revision,
            expected_profile_revision=snapshot.profile_revision,
            expected_root_id=before.after_root_id or before.before_root_id,
            expected_relative_path=(
                before.after_relative_path or before.before_relative_path
            ),
            expected_stat_revision="stale",
            expected_tag_revision="stale",
            expected_file_fingerprint="stale",
            source_path_identity="stale",
            desired_document_json=placeholder,
            desired_document_hash=hashlib.sha256(placeholder.encode()).hexdigest(),
            eligibility="stale",
            reason_code=reason,
            created_at=now,
        )

    @staticmethod
    def _safe_path(root: Path, relative: str) -> Path:
        pure = PurePosixPath(relative)
        if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
            raise ValidationError("An undo path is unsafe.")
        if not root.exists() or stat.S_ISLNK(root.lstat().st_mode):
            raise ValidationError("An undo root is unsafe.")
        current = root
        for part in pure.parts[:-1]:
            current = current / part
            if current.exists():
                metadata = current.lstat()
                if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                    raise ValidationError("An undo path contains a symlink.")
        return root.joinpath(*pure.parts)

    @staticmethod
    def _hash_file(path: Path) -> str:
        digest = hashlib.sha256()
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                raise OSError("The undo path is not a regular file.")
            while chunk := handle.read(1024 * 1024):
                digest.update(chunk)
        return digest.hexdigest()

    @staticmethod
    def _collision_key(value: str) -> str:
        import unicodedata

        return unicodedata.normalize("NFC", value).casefold()
