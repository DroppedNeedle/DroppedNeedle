"""Durable staged publication for immutable Library Management bundles."""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import shutil
import stat
import time
import unicodedata
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

import msgspec

from core.exceptions import ConflictError, StaleRevisionError, ValidationError
from infrastructure.audio.metadata_engine import (
    AudioMetadataEngine,
    legacy_audio_projection,
)
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio_metadata import (
    DesiredAudioDocument,
    DesiredAudioField,
    EmbeddedArtworkDescriptor,
)
from models.audio_metadata import SemanticTagSnapshot
from models.library_management import (
    MANAGEMENT_RECYCLE_ROOT_ID,
    LibraryFileMutationJournal,
    LibraryManagementBaseline,
    LibraryManagementBlobReference,
    LibraryManagementBundleCommitResult,
    LibraryManagementCatalogMutation,
    LibraryManagementCollisionEvidence,
    LibraryManagementImportBundle,
    LibraryManagementImportBundleRecord,
    LibraryManagementImportFile,
    LibraryManagementImportJournal,
    LibraryManagementImportResult,
    LibraryManagementOperationSnapshot,
    LibraryManagementPlanItem,
    LibraryManagementPublishedImportFile,
    ManagementBlobKind,
)
from models.library_management_planning import PinnedLibraryManagementProfile
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.file_revision import revision_from_stat
from services.native.library_filesystem_coordinator import (
    MANAGEMENT_ARTIFACT_PREFIX,
    LibraryFilesystemCoordinator,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.recycle_bin import recycle
from services.preferences_service import PreferencesService
from api.v1.schemas.library_management import picard_style_organizer_profile

logger = logging.getLogger(__name__)
_JOURNAL_NAMESPACE = uuid.UUID("c646c2dd-f0cc-4c9d-8b2c-feb0a8a660c9")
_SNAPSHOT_NAMESPACE = uuid.UUID("77d7be20-4ff2-475a-941f-e0a575806d78")
_BASELINE_NAMESPACE = uuid.UUID("bf48a4f8-5968-41f6-9c82-f95a976a8f21")
_IMPORT_BUNDLE_NAMESPACE = uuid.UUID("4ac147e4-7371-4eb4-89d3-5cc30f394277")
_MAX_DIRECTORY_COLLISION_ENTRIES = 10_000

CommitCallback = Callable[[set[str], set[str]], Awaitable[None]]
ImportCommitCallback = Callable[
    [str, tuple[LibraryManagementPublishedImportFile, ...]],
    Awaitable[tuple[str, ...]],
]


@dataclass
class _PreparedMutation:
    journal: LibraryFileMutationJournal
    plan_item: LibraryManagementPlanItem
    source: Path | None
    temporary: Path
    destination: Path
    backup: Path | None
    source_fingerprint: str | None
    staged_fingerprint: str
    catalog_mutation: LibraryManagementCatalogMutation | None = None
    published: bool = False
    source_backed_up: bool = False
    remove_source: bool = True
    delete_only: bool = False
    recycle_move: bool = False


@dataclass
class _PreparedImportMutation:
    request: LibraryManagementImportFile
    journal: LibraryManagementImportJournal
    source: Path
    temporary: Path
    destination: Path
    replacement: Path | None
    replacement_backup: Path | None
    artifacts: list[_PreparedImportArtifact]


@dataclass
class _PreparedImportArtifact:
    kind: str
    source: Path | None
    temporary: Path
    destination: Path
    fingerprint: str


class LibraryManagementPublisher:
    def __init__(
        self,
        store: NativeLibraryStore,
        preferences: PreferencesService,
        audio: AudioMetadataEngine,
        write_planner: AudioWritePlanningService,
        blobs: LibraryManagementBlobStore,
        filesystem: LibraryFilesystemCoordinator,
        *,
        on_commit: CommitCallback | None = None,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._preferences = preferences
        self._audio = audio
        self._write_planner = write_planner
        self._blobs = blobs
        self._filesystem = filesystem
        self._on_commit = on_commit
        self._clock = clock

    @staticmethod
    async def _finish_critical_task[ResultT](
        task: asyncio.Task[ResultT],
    ) -> tuple[ResultT, bool]:
        cancelled = False
        while not task.done():
            try:
                await asyncio.shield(task)
            except asyncio.CancelledError:
                cancelled = True
        return task.result(), cancelled

    async def publish_import_bundle(
        self,
        bundle: LibraryManagementImportBundle,
        catalog_commit: ImportCommitCallback,
    ) -> LibraryManagementImportResult:
        """Publish one verified acquisition/drop unit through a durable journal.

        Incoming files do not exist in the catalog yet, so they cannot use the
        foreign-keyed manual-management plan table. This import lane shares the
        staged writer, root leases, safe path checks, publish/rollback rules, and
        NativeLibraryStore transaction owner without inventing another work queue.
        """

        self._validate_import_bundle(bundle)
        request_json = msgspec.json.encode(bundle).decode()
        request_hash = hashlib.sha256(request_json.encode()).hexdigest()
        bundle_id = str(uuid.uuid5(_IMPORT_BUNDLE_NAMESPACE, bundle.idempotency_key))
        now = self._clock()
        record, created = await self._store.ensure_library_management_import_bundle(
            LibraryManagementImportBundleRecord(
                id=bundle_id,
                idempotency_key=bundle.idempotency_key,
                origin=bundle.origin,
                policy_revision=bundle.policy_revision,
                request_json=request_json,
                request_hash=request_hash,
                state="preparing",
                created_at=now,
                updated_at=now,
            )
        )
        if record.state in {"completed", "cleanup_pending", "catalog_committed"}:
            repeated = record.state == "completed"
            if record.state != "completed":
                record = await self._resume_import_cleanup(record, bundle)
            return self._import_result(record, repeated=repeated)
        if record.state == "needs_attention":
            raise ConflictError("The import publication needs administrator attention.")
        if record.state in {"preparing", "rolled_back"}:
            record = await self._store.mark_library_management_import_publishing(
                bundle_id,
                expected_row_revision=record.row_revision,
                updated_at=self._clock(),
            )
        elif record.state != "publishing":
            raise StaleRevisionError("The import publication state is invalid.")

        roots = self._root_paths(bundle.policy_revision)
        existing = {
            value.ordinal: value
            for value in await self._store.list_library_management_import_journals(
                bundle_id
            )
        }
        prepared: list[_PreparedImportMutation] = []
        try:
            for request in sorted(bundle.files, key=lambda value: value.ordinal):
                current = existing.get(request.ordinal)
                if current is not None and current.state == "rolled_back":
                    current = (
                        await self._store.transition_library_management_import_journal(
                            bundle_id,
                            request.ordinal,
                            expected_state="rolled_back",
                            new_state="planned",
                            expected_row_revision=current.row_revision,
                            updated_at=self._clock(),
                        )
                    )
                prepared.append(
                    await self._prepare_import_file(bundle_id, request, roots, current)
                )
        except BaseException:
            rollback_task = asyncio.create_task(
                self._rollback_import_preparation(record, bundle, roots, prepared)
            )
            await self._finish_critical_task(rollback_task)
            raise

        root_ids = (
            {value.request.destination_root_id for value in prepared}
            | {
                value.request.replacement_root_id
                for value in prepared
                if value.request.replacement_root_id is not None
            }
            | {
                artifact.destination_root_id
                for value in prepared
                for artifact in value.request.artifacts
            }
        )
        critical_cancelled = False
        try:
            async with self._filesystem.write_many(root_ids):

                async def critical() -> tuple[str, ...]:
                    self._root_paths(bundle.policy_revision)
                    for value in prepared:
                        await self._recover_import_publish_boundary(value)
                    await asyncio.to_thread(self._recheck_import_bundle, prepared)
                    for value in prepared:
                        await self._publish_import_file(value)
                    await asyncio.to_thread(self._fsync_import_directories, prepared)
                    published = tuple(
                        [await self._published_import_file(value) for value in prepared]
                    )
                    committed = await catalog_commit(bundle_id, published)
                    if len(committed) != len(prepared):
                        raise ValidationError(
                            "The import catalog commit returned an incomplete result."
                        )
                    return committed

                critical_task = asyncio.create_task(critical())
                local_track_ids, critical_cancelled = await self._finish_critical_task(
                    critical_task
                )
        except BaseException:
            refreshed = await self._store.get_library_management_import_bundle(
                bundle_id
            )
            if refreshed is not None and refreshed.state in {
                "catalog_committed",
                "cleanup_pending",
                "completed",
            }:
                record = refreshed
            else:
                rollback_task = asyncio.create_task(
                    self._rollback_import_bundle(record, prepared)
                )
                await self._finish_critical_task(rollback_task)
                raise
        else:
            record = await self._store.get_library_management_import_bundle(bundle_id)
            if record is None:
                raise ValidationError("The committed import publication disappeared.")

        record = await self._resume_import_cleanup(record, bundle)
        result = self._import_result(record, repeated=not created)
        if self._on_commit is not None:
            try:
                await self._on_commit(set(result.local_track_ids), set())
            except Exception:  # noqa: BLE001 - post-commit invalidation is retryable
                logger.warning("Import publication invalidation failed")
        if critical_cancelled:
            raise asyncio.CancelledError
        return result

    async def recover_import_bundle(
        self, record: LibraryManagementImportBundleRecord
    ) -> str:
        try:
            request_bytes = record.request_json.encode()
            if hashlib.sha256(request_bytes).hexdigest() != record.request_hash:
                raise ValidationError("The import recovery request changed.")
            bundle = msgspec.json.decode(
                request_bytes, type=LibraryManagementImportBundle
            )
            self._validate_import_bundle(bundle)
            expected_id = str(
                uuid.uuid5(_IMPORT_BUNDLE_NAMESPACE, bundle.idempotency_key)
            )
            if (
                expected_id != record.id
                or bundle.origin != record.origin
                or bundle.policy_revision != record.policy_revision
            ):
                raise ValidationError("The import recovery identity changed.")
            if record.state in {"completed", "rolled_back", "needs_attention"}:
                return "skipped"
            if record.state in {"catalog_committed", "cleanup_pending"}:
                recovered = await self._resume_import_cleanup(record, bundle)
                return "recovered" if recovered.state == "completed" else "skipped"
            journals = await self._store.list_library_management_import_journals(
                record.id
            )
            if record.state == "preparing":
                if journals:
                    raise ConflictError(
                        "A preparing import unexpectedly has filesystem journals."
                    )
                await self._store.finish_library_management_import_rollback(
                    record.id,
                    needs_attention=False,
                    updated_at=self._clock(),
                )
                return "rolled_back"
            if record.state != "publishing":
                raise ValidationError("The import recovery state is invalid.")
            roots = self._root_paths(bundle.policy_revision)
            requests = {value.ordinal: value for value in bundle.files}
            if any(journal.ordinal not in requests for journal in journals):
                raise ValidationError("The import recovery journal is inconsistent.")
            prepared = [
                self._prepared_import_from_journal(
                    record.id, requests[journal.ordinal], journal, roots
                )
                for journal in journals
            ]
            await self._rollback_import_bundle(record, prepared)
            recovered = await self._store.get_library_management_import_bundle(
                record.id
            )
            if recovered is None:
                raise ValidationError("The import recovery record disappeared.")
            if recovered.state == "rolled_back":
                return "rolled_back"
            if recovered.state == "needs_attention":
                return "needs_attention"
            raise ValidationError("The import recovery did not reach a terminal state.")
        except asyncio.CancelledError:
            raise
        except Exception:  # noqa: BLE001 - recovery must durably surface corrupt intent
            await self._store.mark_library_management_import_needs_attention(
                record.id,
                failure_code="RECOVERY_NEEDS_ATTENTION",
                updated_at=self._clock(),
            )
            return "needs_attention"

    @staticmethod
    def _validate_import_bundle(bundle: LibraryManagementImportBundle) -> None:
        if not bundle.idempotency_key.strip():
            raise ValidationError("An import publication idempotency key is required.")
        if not 1 <= len(bundle.files) <= 500:
            raise ValidationError("An import publication must contain 1 to 500 files.")
        ordinals = [value.ordinal for value in bundle.files]
        destinations = [
            (value.destination_root_id, value.destination_relative_path)
            for value in bundle.files
        ]
        normalized_destinations = [
            (
                root_id,
                unicodedata.normalize("NFC", relative_path).casefold(),
            )
            for root_id, relative_path in destinations
        ]
        artifact_destinations = [
            (
                artifact.destination_root_id,
                unicodedata.normalize(
                    "NFC", artifact.destination_relative_path
                ).casefold(),
            )
            for value in bundle.files
            for artifact in value.artifacts
        ]
        normalized_sources = [
            os.path.normcase(str(Path(value.input_path).resolve(strict=False)))
            for value in bundle.files
        ]
        if len(set(ordinals)) != len(ordinals) or min(ordinals) < 0:
            raise ValidationError("Import publication ordinals must be unique.")
        all_destinations = [*normalized_destinations, *artifact_destinations]
        if len(set(all_destinations)) != len(all_destinations):
            raise ValidationError("An import publication repeats a destination.")
        if len(set(normalized_sources)) != len(normalized_sources):
            raise ValidationError("An import publication repeats a source file.")
        for value in bundle.files:
            if not Path(value.input_path).is_absolute():
                raise ValidationError("An import source path must be absolute.")
            replacement_values = (
                value.replacement_local_track_id,
                value.replacement_root_id,
                value.replacement_relative_path,
                value.recycle_bin_path,
            )
            if any(item is not None for item in replacement_values) and not all(
                item is not None for item in replacement_values
            ):
                raise ValidationError(
                    "An import replacement needs complete catalog and recycle evidence."
                )
            if (
                value.recycle_bin_path is not None
                and not Path(value.recycle_bin_path).is_absolute()
            ):
                raise ValidationError("An import recycle path must be absolute.")
            automatic_values = (
                value.desired_document,
                value.pinned_profile,
                value.metadata_snapshot_id,
                value.projection_hash,
                value.settings_revision,
                value.undo_retention_days,
                value.baseline_relative_path,
                value.release_track_mbid,
                value.medium_position,
                value.release_track_position,
            )
            if any(item is not None for item in automatic_values) and not all(
                item is not None for item in automatic_values
            ):
                raise ValidationError(
                    "An automatic import needs one complete immutable projection."
                )
            if value.pinned_profile is not None and not value.authoritative_mapping:
                raise ValidationError(
                    "An automatic import needs an authoritative release-track mapping."
                )
            for artifact in value.artifacts:
                if (artifact.source_path is None) == (artifact.content is None):
                    raise ValidationError(
                        "An import artifact needs exactly one immutable source."
                    )
                if (
                    artifact.source_fingerprint is None
                    or len(artifact.source_fingerprint) != 64
                    or any(
                        character not in "0123456789abcdef"
                        for character in artifact.source_fingerprint
                    )
                ):
                    raise ValidationError(
                        "An import artifact needs an immutable SHA-256 fingerprint."
                    )
                if (
                    artifact.source_path is not None
                    and not Path(artifact.source_path).is_absolute()
                ):
                    raise ValidationError("An import artifact source must be absolute.")

    @staticmethod
    def _minimal_import_profile():
        profile = picard_style_organizer_profile()
        profile.metadata.scrub_unmanaged_tags = False
        profile.metadata.tagging_script_ids = []
        profile.artwork.embedded_enabled = False
        profile.artwork.external_enabled = False
        profile.organization.rename_enabled = False
        profile.organization.move_enabled = False
        profile.organization.move_sidecars = False
        profile.enrichment.lyrics.enabled = False
        profile.enrichment.replaygain.enabled = False
        profile.file_behavior.preserve_timestamps = False
        return profile

    @staticmethod
    def _minimal_import_document(tag) -> DesiredAudioDocument:  # noqa: ANN001
        fields = [DesiredAudioField(name="album", action="set", value=tag.album)]
        if tag.album_artist is not None:
            fields.append(
                DesiredAudioField(
                    name="album_artist", action="set", value=(tag.album_artist,)
                )
            )
        if tag.year is not None:
            fields.append(
                DesiredAudioField(name="date", action="set", value=str(tag.year))
            )
        for name, value in (
            ("musicbrainz_release_group_id", tag.musicbrainz_release_group_id),
            ("musicbrainz_release_id", tag.musicbrainz_release_id),
        ):
            if value:
                fields.append(DesiredAudioField(name=name, action="set", value=value))
        album_artist_ids = tuple(
            tag.musicbrainz_album_artist_ids
            or (
                [tag.musicbrainz_album_artist_id]
                if tag.musicbrainz_album_artist_id
                else []
            )
        )
        if album_artist_ids:
            fields.append(
                DesiredAudioField(
                    name="musicbrainz_album_artist_id",
                    action="set",
                    value=album_artist_ids,
                )
            )
        return DesiredAudioDocument(fields=tuple(fields))

    async def _prepare_import_file(
        self,
        bundle_id: str,
        request: LibraryManagementImportFile,
        roots: dict[str, Path],
        current: LibraryManagementImportJournal | None,
    ) -> _PreparedImportMutation:
        root = roots.get(request.destination_root_id)
        if root is None:
            raise StaleRevisionError("An import destination root changed.")
        destination = self._safe_path(
            root, request.destination_relative_path, create_parent=True
        )
        source = Path(request.input_path)
        temporary = self._artifact_path(
            destination, bundle_id, request.ordinal, "import-temp", destination.suffix
        )
        replacement = None
        replacement_backup = None
        replacement_fingerprint = None
        if request.replacement_root_id is not None:
            replacement_root = roots.get(request.replacement_root_id)
            if replacement_root is None:
                raise StaleRevisionError("An import replacement root changed.")
            replacement = self._safe_path(
                replacement_root, str(request.replacement_relative_path)
            )
            replacement_backup = self._artifact_path(
                replacement,
                bundle_id,
                request.ordinal,
                "import-replacement-backup",
                replacement.suffix,
            )

        if current is None:
            source_stat = await asyncio.to_thread(source.stat)
            source_fingerprint = await asyncio.to_thread(self._hash_file, source)
            if replacement is not None:
                replacement_fingerprint = await asyncio.to_thread(
                    self._hash_file, replacement
                )
            current = await self._store.ensure_library_management_import_journal(
                LibraryManagementImportJournal(
                    bundle_id=bundle_id,
                    ordinal=request.ordinal,
                    state="planned",
                    source_fingerprint=source_fingerprint,
                    source_size=source_stat.st_size,
                    source_mtime_ns=source_stat.st_mtime_ns,
                    temporary_relative_path=temporary.relative_to(root).as_posix(),
                    destination_root_id=request.destination_root_id,
                    destination_relative_path=request.destination_relative_path,
                    replacement_fingerprint=replacement_fingerprint,
                    replacement_backup_relative_path=(
                        replacement_backup.relative_to(
                            roots[str(request.replacement_root_id)]
                        ).as_posix()
                        if replacement_backup is not None
                        else None
                    ),
                    created_at=self._clock(),
                    updated_at=self._clock(),
                )
            )
        prepared = _PreparedImportMutation(
            request=request,
            journal=current,
            source=source,
            temporary=temporary,
            destination=destination,
            replacement=replacement,
            replacement_backup=replacement_backup,
            artifacts=[],
        )
        if current.state == "planned":
            source_stat = await asyncio.to_thread(source.stat)
            if (
                source_stat.st_size != current.source_size
                or source_stat.st_mtime_ns != current.source_mtime_ns
                or await asyncio.to_thread(self._hash_file, source)
                != current.source_fingerprint
            ):
                raise StaleRevisionError("An import source changed before staging.")
            read = await asyncio.to_thread(self._audio.read, source)
            profile = (
                request.pinned_profile.profile
                if request.pinned_profile is not None
                else self._minimal_import_profile()
            )
            desired = request.desired_document or self._minimal_import_document(
                request.tag
            )
            plan = self._write_planner.plan(
                current=read,
                desired=desired,
                profile=profile,
            )
            if plan.blockers:
                raise ValidationError(
                    "The import file does not pass the staged writer capability gate."
                )
            prepared.artifacts = await self._prepare_import_artifacts(
                bundle_id, request, roots, stage=True
            )
            if request.pinned_profile is not None:
                before_stat = source_stat
                before_snapshot = plan.snapshot
                if replacement is not None:
                    before_stat = await asyncio.to_thread(replacement.stat)
                    if (
                        await asyncio.to_thread(self._hash_file, replacement)
                        != current.replacement_fingerprint
                    ):
                        raise StaleRevisionError(
                            "An import replacement changed before snapshotting."
                        )
                    before_snapshot = await asyncio.to_thread(
                        self._audio.snapshot, replacement
                    )
                before_format = before_snapshot.probe.detected_format
                if before_format is None:
                    raise ValidationError(
                        "An automatic import before-state has no admitted format."
                    )
                snapshot_blob = await self._blobs.add_bytes(
                    msgspec.json.encode(before_snapshot),
                    kind="tag_snapshot",
                    created_at=self._clock(),
                )
                reference_id = self._import_snapshot_reference_id(
                    bundle_id, request.ordinal
                )
                await self._store.add_management_blob_reference(
                    LibraryManagementBlobReference(
                        blob_sha256=snapshot_blob.sha256,
                        reference_kind="operation_snapshot",
                        reference_id=reference_id,
                        created_at=self._clock(),
                    )
                )
                ancillary_snapshot_json = await self._capture_import_ancillary(
                    bundle_id,
                    request,
                    prepared.artifacts,
                    roots,
                )
                current = await self._store.set_library_management_import_baseline(
                    bundle_id,
                    request.ordinal,
                    expected_row_revision=current.row_revision,
                    baseline_blob_sha256=snapshot_blob.sha256,
                    baseline_format=before_format,
                    baseline_adapter_version=before_snapshot.adapter_version,
                    baseline_stat_revision=revision_from_stat(before_stat),
                    baseline_tag_revision=hashlib.sha256(
                        msgspec.json.encode(before_snapshot)
                    ).hexdigest(),
                    baseline_image_snapshot_json=json.dumps(
                        msgspec.to_builtins(before_snapshot.artwork),
                        separators=(",", ":"),
                        sort_keys=True,
                    ),
                    baseline_ancillary_snapshot_json=ancillary_snapshot_json,
                    baseline_file_mtime_ns=before_snapshot.file_attributes.mtime_ns,
                    baseline_file_mode=before_snapshot.file_attributes.permission_bits,
                    updated_at=self._clock(),
                )
                prepared.journal = current
            await asyncio.to_thread(self._stage_audio, source, temporary, plan)
            staged_fingerprint = await asyncio.to_thread(self._hash_file, temporary)
            current = await self._store.transition_library_management_import_journal(
                bundle_id,
                request.ordinal,
                expected_state="planned",
                new_state="staged",
                expected_row_revision=current.row_revision,
                updated_at=self._clock(),
                staged_fingerprint=staged_fingerprint,
            )
            prepared.journal = current
        if current.state == "staged":
            if (
                current.staged_fingerprint is None
                or await asyncio.to_thread(self._hash_file, temporary)
                != current.staged_fingerprint
            ):
                raise ConflictError("The staged import file changed.")
            current = await self._store.transition_library_management_import_journal(
                bundle_id,
                request.ordinal,
                expected_state="staged",
                new_state="validated",
                expected_row_revision=current.row_revision,
                updated_at=self._clock(),
            )
            prepared.journal = current
        if current.state not in {
            "validated",
            "replacement_backed_up",
            "published",
        }:
            raise StaleRevisionError("The import journal cannot be published.")
        if not prepared.artifacts and request.artifacts:
            prepared.artifacts = await self._prepare_import_artifacts(
                bundle_id, request, roots, stage=False
            )
        return prepared

    async def _capture_import_ancillary(
        self,
        bundle_id: str,
        request: LibraryManagementImportFile,
        artifacts: list[_PreparedImportArtifact],
        roots: dict[str, Path],
    ) -> str:
        reference_id = self._import_snapshot_reference_id(bundle_id, request.ordinal)
        baseline_parent = PurePosixPath(
            request.baseline_relative_path or request.destination_relative_path
        ).parent
        values: list[dict[str, object]] = []
        for artifact in artifacts:
            destination_root_id, destination_root = next(
                (root_id, root)
                for root_id, root in roots.items()
                if artifact.destination.is_relative_to(root)
            )
            if artifact.source is None:
                blob = await self._blobs.add_file(
                    artifact.temporary,
                    kind="image",
                    created_at=self._clock(),
                )
                values.append(
                    {
                        "kind": "external_art",
                        "before_exists": False,
                        "after_exists": True,
                        "after_root_id": destination_root_id,
                        "after_relative_path": artifact.destination.relative_to(
                            destination_root
                        ).as_posix(),
                        "after_blob_sha256": blob.sha256,
                    }
                )
            else:
                if request.replacement_local_track_id is not None:
                    values.append(
                        {
                            "kind": "sidecar",
                            "before_exists": False,
                            "after_exists": True,
                            "after_root_id": destination_root_id,
                            "after_relative_path": artifact.destination.relative_to(
                                destination_root
                            ).as_posix(),
                            "after_blob_sha256": artifact.fingerprint,
                        }
                    )
                    continue
                else:
                    blob = await self._blobs.add_file(
                        artifact.source,
                        kind="sidecar_manifest",
                        created_at=self._clock(),
                    )
                    relative_source = artifact.source.relative_to(
                        Path(request.input_path).parent
                    ).as_posix()
                    values.append(
                        {
                            "kind": "sidecar",
                            "before_exists": True,
                            "before_root_id": request.destination_root_id,
                            "before_relative_path": (
                                baseline_parent / PurePosixPath(relative_source)
                            ).as_posix(),
                            "after_root_id": destination_root_id,
                            "after_relative_path": artifact.destination.relative_to(
                                destination_root
                            ).as_posix(),
                            "blob_sha256": blob.sha256,
                            "after_blob_sha256": artifact.fingerprint,
                        }
                    )
            await self._store.add_management_blob_reference(
                LibraryManagementBlobReference(
                    blob_sha256=blob.sha256,
                    reference_kind="operation_snapshot",
                    reference_id=reference_id,
                    created_at=self._clock(),
                )
            )
        return json.dumps(values, separators=(",", ":"), sort_keys=True)

    async def _prepare_import_artifacts(
        self,
        bundle_id: str,
        request: LibraryManagementImportFile,
        roots: dict[str, Path],
        *,
        stage: bool,
    ) -> list[_PreparedImportArtifact]:
        prepared: list[_PreparedImportArtifact] = []
        for index, artifact in enumerate(request.artifacts):
            root = roots.get(artifact.destination_root_id)
            if root is None:
                raise StaleRevisionError("An import artifact root changed.")
            destination = self._safe_path(
                root, artifact.destination_relative_path, create_parent=True
            )
            temporary = self._artifact_path(
                destination,
                bundle_id,
                request.ordinal,
                f"import-{artifact.kind}-{index}",
                destination.suffix,
            )
            if artifact.content is not None:
                fingerprint = hashlib.sha256(artifact.content).hexdigest()
                if artifact.source_fingerprint not in {None, fingerprint}:
                    raise ConflictError("An import artwork payload changed.")
                if stage:
                    await asyncio.to_thread(
                        self._write_temp_bytes, temporary, artifact.content
                    )
                source = None
            else:
                assert artifact.source_path is not None
                source = Path(artifact.source_path)
                fingerprint = str(artifact.source_fingerprint)
                if stage:
                    if await asyncio.to_thread(self._hash_file, source) != fingerprint:
                        raise StaleRevisionError(
                            "An import sidecar changed before staging."
                        )
                    await asyncio.to_thread(self._copy_temp, source, temporary)
            if (
                stage
                and await asyncio.to_thread(self._hash_file, temporary) != fingerprint
            ):
                raise ConflictError("A staged import artifact failed validation.")
            prepared.append(
                _PreparedImportArtifact(
                    kind=artifact.kind,
                    source=source,
                    temporary=temporary,
                    destination=destination,
                    fingerprint=fingerprint,
                )
            )
        return prepared

    @classmethod
    def _recheck_import_bundle(cls, prepared: list[_PreparedImportMutation]) -> None:
        for value in prepared:
            journal = value.journal
            if journal.state == "published":
                if (
                    journal.staged_fingerprint is None
                    or cls._hash_file(value.destination) != journal.staged_fingerprint
                ):
                    raise ConflictError("A published import file changed.")
                for artifact in value.artifacts:
                    if artifact.temporary.exists():
                        if cls._hash_file(artifact.temporary) != artifact.fingerprint:
                            raise ConflictError(
                                "A redundant staged import artifact changed."
                            )
                        artifact.temporary.unlink()
                    if (
                        not artifact.destination.exists()
                        or cls._hash_file(artifact.destination) != artifact.fingerprint
                    ):
                        raise ConflictError("A published import artifact changed.")
                continue
            source_stat = value.source.stat()
            if (
                source_stat.st_size != journal.source_size
                or source_stat.st_mtime_ns != journal.source_mtime_ns
                or cls._hash_file(value.source) != journal.source_fingerprint
            ):
                raise StaleRevisionError("An import source changed before publish.")
            if (
                journal.staged_fingerprint is None
                or cls._hash_file(value.temporary) != journal.staged_fingerprint
            ):
                raise ConflictError("A staged import file changed before publish.")
            same_path_replacement = value.replacement == value.destination
            if journal.state == "replacement_backed_up":
                if (
                    not same_path_replacement
                    or value.replacement_backup is None
                    or cls._hash_file(value.replacement_backup)
                    != journal.replacement_fingerprint
                ):
                    raise ConflictError("An import replacement backup changed.")
            elif value.destination.exists():
                if (
                    not same_path_replacement
                    or cls._hash_file(value.destination)
                    != journal.replacement_fingerprint
                ):
                    raise ConflictError(
                        "An import destination was created after planning."
                    )
            elif cls._has_normalized_destination_sibling(value.destination):
                raise ConflictError(
                    "An import destination collides after normalization."
                )
            if value.replacement is not None and not same_path_replacement:
                if cls._hash_file(value.replacement) != journal.replacement_fingerprint:
                    raise ConflictError("An import replacement changed before publish.")
            for artifact in value.artifacts:
                if artifact.temporary.exists():
                    if cls._hash_file(artifact.temporary) != artifact.fingerprint:
                        raise ConflictError("A staged import artifact changed.")
                    if artifact.destination.exists():
                        raise ConflictError(
                            "An import artifact destination is occupied."
                        )
                    if cls._has_normalized_destination_sibling(artifact.destination):
                        raise ConflictError(
                            "An import artifact collides after normalization."
                        )
                elif (
                    not artifact.destination.exists()
                    or cls._hash_file(artifact.destination) != artifact.fingerprint
                ):
                    raise ConflictError("An import artifact is missing after publish.")

    async def _recover_import_publish_boundary(
        self, value: _PreparedImportMutation
    ) -> None:
        journal = value.journal
        if journal.state not in {"validated", "replacement_backed_up"}:
            return
        if (
            not value.temporary.exists()
            and value.destination.exists()
            and journal.staged_fingerprint is not None
            and await asyncio.to_thread(self._hash_file, value.destination)
            == journal.staged_fingerprint
        ):
            await self._publish_import_artifacts(value)
            value.journal = (
                await self._store.transition_library_management_import_journal(
                    journal.bundle_id,
                    journal.ordinal,
                    expected_state=journal.state,
                    new_state="published",
                    expected_row_revision=journal.row_revision,
                    updated_at=self._clock(),
                )
            )
            return
        if (
            journal.state == "validated"
            and value.replacement == value.destination
            and not value.destination.exists()
            and value.replacement_backup is not None
            and value.replacement_backup.exists()
            and await asyncio.to_thread(self._hash_file, value.replacement_backup)
            == journal.replacement_fingerprint
        ):
            value.journal = (
                await self._store.transition_library_management_import_journal(
                    journal.bundle_id,
                    journal.ordinal,
                    expected_state="validated",
                    new_state="replacement_backed_up",
                    expected_row_revision=journal.row_revision,
                    updated_at=self._clock(),
                )
            )

    async def _publish_import_file(self, value: _PreparedImportMutation) -> None:
        journal = value.journal
        if journal.state == "published":
            return
        if value.replacement == value.destination and journal.state == "validated":
            assert value.replacement_backup is not None
            await asyncio.to_thread(
                os.replace, value.destination, value.replacement_backup
            )
            journal = await self._store.transition_library_management_import_journal(
                journal.bundle_id,
                journal.ordinal,
                expected_state="validated",
                new_state="replacement_backed_up",
                expected_row_revision=journal.row_revision,
                updated_at=self._clock(),
            )
            value.journal = journal
        await asyncio.to_thread(os.replace, value.temporary, value.destination)
        await self._publish_import_artifacts(value)
        value.journal = await self._store.transition_library_management_import_journal(
            journal.bundle_id,
            journal.ordinal,
            expected_state=journal.state,
            new_state="published",
            expected_row_revision=journal.row_revision,
            updated_at=self._clock(),
        )

    async def _publish_import_artifacts(self, value: _PreparedImportMutation) -> None:
        for artifact in value.artifacts:
            if artifact.temporary.exists():
                if artifact.destination.exists():
                    raise ConflictError("An import artifact destination is occupied.")
                await asyncio.to_thread(
                    os.replace, artifact.temporary, artifact.destination
                )
            elif (
                not artifact.destination.exists()
                or await asyncio.to_thread(self._hash_file, artifact.destination)
                != artifact.fingerprint
            ):
                raise ConflictError("An import artifact publish cannot be recovered.")

    async def _published_import_file(
        self, value: _PreparedImportMutation
    ) -> LibraryManagementPublishedImportFile:
        document = await asyncio.to_thread(self._audio.read, value.destination)
        tag, info = legacy_audio_projection(document)
        return LibraryManagementPublishedImportFile(
            request=value.request,
            destination_path=str(value.destination),
            staged_fingerprint=str(value.journal.staged_fingerprint),
            tag=tag,
            info=info,
        )

    async def _rollback_import_bundle(
        self,
        record: LibraryManagementImportBundleRecord,
        prepared: list[_PreparedImportMutation],
    ) -> None:
        if not prepared:
            refreshed = await self._store.get_library_management_import_bundle(
                record.id
            )
            if refreshed is not None and refreshed.state == "publishing":
                await self._store.finish_library_management_import_rollback(
                    record.id,
                    needs_attention=False,
                    updated_at=self._clock(),
                )
            return
        root_ids = (
            {value.request.destination_root_id for value in prepared}
            | {
                value.request.replacement_root_id
                for value in prepared
                if value.request.replacement_root_id is not None
            }
            | {
                artifact.destination_root_id
                for value in prepared
                for artifact in value.request.artifacts
            }
        )
        async with self._filesystem.write_many(root_ids):
            await self._rollback_import_bundle_locked(record, prepared)

    async def _rollback_import_preparation(
        self,
        record: LibraryManagementImportBundleRecord,
        bundle: LibraryManagementImportBundle,
        roots: dict[str, Path],
        prepared: list[_PreparedImportMutation],
    ) -> None:
        """Include a file whose journal was created before preparation failed."""

        known = {value.request.ordinal for value in prepared}
        requests = {value.ordinal: value for value in bundle.files}
        for journal in await self._store.list_library_management_import_journals(
            record.id
        ):
            request = requests.get(journal.ordinal)
            if request is None or journal.ordinal in known:
                continue
            prepared.append(
                self._prepared_import_from_journal(record.id, request, journal, roots)
            )
        await self._rollback_import_bundle(record, prepared)

    def _prepared_import_from_journal(
        self,
        bundle_id: str,
        request: LibraryManagementImportFile,
        journal: LibraryManagementImportJournal,
        roots: dict[str, Path],
    ) -> _PreparedImportMutation:
        root = roots[request.destination_root_id]
        destination = self._safe_path(root, request.destination_relative_path)
        replacement = None
        replacement_backup = None
        if request.replacement_root_id is not None:
            replacement_root = roots[request.replacement_root_id]
            replacement = self._safe_path(
                replacement_root, str(request.replacement_relative_path)
            )
            if journal.replacement_backup_relative_path is not None:
                replacement_backup = self._safe_path(
                    replacement_root, journal.replacement_backup_relative_path
                )
        artifacts = []
        for index, artifact in enumerate(request.artifacts):
            artifact_root = roots[artifact.destination_root_id]
            artifact_destination = self._safe_path(
                artifact_root, artifact.destination_relative_path
            )
            artifacts.append(
                _PreparedImportArtifact(
                    kind=artifact.kind,
                    source=(
                        Path(artifact.source_path)
                        if artifact.source_path is not None
                        else None
                    ),
                    temporary=self._artifact_path(
                        artifact_destination,
                        bundle_id,
                        request.ordinal,
                        f"import-{artifact.kind}-{index}",
                        artifact_destination.suffix,
                    ),
                    destination=artifact_destination,
                    fingerprint=artifact.source_fingerprint or "",
                )
            )
        return _PreparedImportMutation(
            request=request,
            journal=journal,
            source=Path(request.input_path),
            temporary=self._safe_path(root, journal.temporary_relative_path),
            destination=destination,
            replacement=replacement,
            replacement_backup=replacement_backup,
            artifacts=artifacts,
        )

    async def _rollback_import_bundle_locked(
        self,
        record: LibraryManagementImportBundleRecord,
        prepared: list[_PreparedImportMutation],
    ) -> None:
        needs_attention = False
        for value in reversed(prepared):
            journal = value.journal
            if journal.state not in {
                "planned",
                "staged",
                "validated",
                "replacement_backed_up",
                "published",
                "rollback_pending",
            }:
                continue
            try:
                was_published = journal.state == "published" or (
                    not value.temporary.exists()
                    and value.destination.exists()
                    and journal.staged_fingerprint is not None
                    and await asyncio.to_thread(self._hash_file, value.destination)
                    == journal.staged_fingerprint
                )
                if journal.state != "rollback_pending":
                    journal = (
                        await self._store.transition_library_management_import_journal(
                            journal.bundle_id,
                            journal.ordinal,
                            expected_state=journal.state,
                            new_state="rollback_pending",
                            expected_row_revision=journal.row_revision,
                            updated_at=self._clock(),
                        )
                    )
                await asyncio.to_thread(
                    self._restore_import_filesystem,
                    value,
                    was_published=was_published,
                )
                await self._store.remove_management_blob_references(
                    reference_kind="operation_snapshot",
                    reference_id=self._import_snapshot_reference_id(
                        journal.bundle_id, journal.ordinal
                    ),
                )
                value.journal = (
                    await self._store.transition_library_management_import_journal(
                        journal.bundle_id,
                        journal.ordinal,
                        expected_state="rollback_pending",
                        new_state="rolled_back",
                        expected_row_revision=journal.row_revision,
                        updated_at=self._clock(),
                    )
                )
            except (OSError, ConflictError, StaleRevisionError, ValidationError):
                needs_attention = True
                try:
                    await self._store.transition_library_management_import_journal(
                        journal.bundle_id,
                        journal.ordinal,
                        expected_state=journal.state,
                        new_state="needs_attention",
                        expected_row_revision=journal.row_revision,
                        updated_at=self._clock(),
                        failure_code="RECOVERY_NEEDS_ATTENTION",
                    )
                except (StaleRevisionError, ValidationError):
                    pass
        refreshed = await self._store.get_library_management_import_bundle(record.id)
        if refreshed is not None and refreshed.state == "publishing":
            await self._store.finish_library_management_import_rollback(
                record.id,
                needs_attention=needs_attention,
                updated_at=self._clock(),
            )

    @classmethod
    def _restore_import_filesystem(
        cls,
        value: _PreparedImportMutation,
        *,
        was_published: bool,
    ) -> None:
        journal = value.journal
        if (
            was_published
            and value.destination.exists()
            and journal.staged_fingerprint is not None
        ):
            if cls._hash_file(value.destination) != journal.staged_fingerprint:
                raise ConflictError("A published import changed during rollback.")
            value.destination.unlink()
        if was_published:
            for artifact in value.artifacts:
                if artifact.destination.exists():
                    if cls._hash_file(artifact.destination) != artifact.fingerprint:
                        raise ConflictError(
                            "A published import artifact changed during rollback."
                        )
                    artifact.destination.unlink()
        for artifact in value.artifacts:
            artifact.temporary.unlink(missing_ok=True)
        if value.replacement_backup is not None and value.replacement_backup.exists():
            if (
                cls._hash_file(value.replacement_backup)
                != journal.replacement_fingerprint
            ):
                raise ConflictError("An import replacement backup changed.")
            os.replace(value.replacement_backup, value.destination)
        value.temporary.unlink(missing_ok=True)

    async def _resume_import_cleanup(
        self,
        record: LibraryManagementImportBundleRecord,
        bundle: LibraryManagementImportBundle,
    ) -> LibraryManagementImportBundleRecord:
        if record.state == "completed":
            return record
        roots = self._root_paths(bundle.policy_revision)
        root_ids = (
            {value.destination_root_id for value in bundle.files}
            | {
                value.replacement_root_id
                for value in bundle.files
                if value.replacement_root_id is not None
            }
            | {
                artifact.destination_root_id
                for value in bundle.files
                for artifact in value.artifacts
            }
        )
        async with self._filesystem.write_many(root_ids):
            return await self._resume_import_cleanup_locked(record, bundle, roots)

    async def _resume_import_cleanup_locked(
        self,
        record: LibraryManagementImportBundleRecord,
        bundle: LibraryManagementImportBundle,
        roots: dict[str, Path],
    ) -> LibraryManagementImportBundleRecord:
        journals = {
            value.ordinal: value
            for value in await self._store.list_library_management_import_journals(
                record.id
            )
        }
        completed: list[int] = []
        failed: list[int] = []
        for request in bundle.files:
            journal = journals.get(request.ordinal)
            if journal is None or journal.state == "completed":
                if journal is not None:
                    completed.append(request.ordinal)
                continue
            try:
                source = Path(request.input_path)
                if source.exists():
                    if (
                        await asyncio.to_thread(self._hash_file, source)
                        != journal.source_fingerprint
                    ):
                        raise ConflictError("An import source changed before cleanup.")
                    await asyncio.to_thread(source.unlink)
                if request.replacement_root_id is not None:
                    replacement_root = roots[str(request.replacement_root_id)]
                    replacement = self._safe_path(
                        replacement_root, str(request.replacement_relative_path)
                    )
                    destination = self._safe_path(
                        roots[request.destination_root_id],
                        request.destination_relative_path,
                    )
                    if replacement == destination:
                        backup_relative = journal.replacement_backup_relative_path
                        replacement = self._safe_path(
                            replacement_root, str(backup_relative)
                        )
                    if replacement.exists():
                        if (
                            await asyncio.to_thread(self._hash_file, replacement)
                            != journal.replacement_fingerprint
                        ):
                            raise ConflictError(
                                "An import replacement changed before recycle."
                            )
                        await asyncio.to_thread(
                            recycle, replacement, Path(str(request.recycle_bin_path))
                        )
                for artifact in request.artifacts:
                    if artifact.source_path is None:
                        continue
                    artifact_source = Path(artifact.source_path)
                    if artifact_source.exists():
                        if (
                            await asyncio.to_thread(self._hash_file, artifact_source)
                            != artifact.source_fingerprint
                        ):
                            raise ConflictError(
                                "An import sidecar changed before cleanup."
                            )
                        await asyncio.to_thread(artifact_source.unlink)
                completed.append(request.ordinal)
            except (OSError, ConflictError, StaleRevisionError, ValidationError):
                failed.append(request.ordinal)
        return await self._store.finish_library_management_import_cleanup(
            record.id,
            completed_ordinals=completed,
            failed_ordinals=failed,
            updated_at=self._clock(),
        )

    @staticmethod
    def _import_result(
        record: LibraryManagementImportBundleRecord, *, repeated: bool
    ) -> LibraryManagementImportResult:
        try:
            result = json.loads(record.result_json)
            paths = tuple(str(value) for value in result["paths"])
            track_ids = tuple(str(value) for value in result["local_track_ids"])
        except (json.JSONDecodeError, KeyError, TypeError) as error:
            raise ValidationError(
                "The import publication result is invalid."
            ) from error
        return LibraryManagementImportResult(
            bundle_id=record.id,
            paths=paths,
            local_track_ids=track_ids,
            repeated=repeated,
        )

    @staticmethod
    def _fsync_import_directories(
        prepared: list[_PreparedImportMutation],
    ) -> None:
        directories = {
            path.parent
            for value in prepared
            for path in (
                value.destination,
                value.temporary,
                value.replacement_backup,
                *(artifact.temporary for artifact in value.artifacts),
                *(artifact.destination for artifact in value.artifacts),
            )
            if path is not None
        }
        for directory in directories:
            descriptor = os.open(directory, os.O_RDONLY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)

    @staticmethod
    def _import_snapshot_reference_id(bundle_id: str, ordinal: int) -> str:
        return f"import:{bundle_id}:{ordinal}"

    def recovery_configuration(
        self, snapshot
    ) -> tuple[PinnedLibraryManagementProfile, dict[str, Path]]:
        """Validate the immutable configuration shared by publish and recovery."""

        pinned = msgspec.json.decode(
            snapshot.profile_snapshot_json.encode(),
            type=PinnedLibraryManagementProfile,
        )
        self._validate_pinned_configuration(snapshot, pinned)
        return pinned, self._root_paths(snapshot.policy_revision, pinned)

    def recovery_filesystem_configuration(
        self, snapshot
    ) -> tuple[PinnedLibraryManagementProfile, dict[str, Path]]:
        """Load pinned cleanup policy while still requiring unchanged root policy."""

        pinned = msgspec.json.decode(
            snapshot.profile_snapshot_json.encode(),
            type=PinnedLibraryManagementProfile,
        )
        return pinned, self._root_paths(snapshot.policy_revision, pinned)

    async def publish_bundle(
        self, job_id: str, bundle_ordinal: int, worker_id: str
    ) -> LibraryManagementBundleCommitResult:
        snapshot = await self._store.get_library_management_job_snapshot(job_id)
        if snapshot is None or snapshot.phase not in {
            "applying",
            "undoing",
            "restoring",
        }:
            raise StaleRevisionError("The management operation is not applying.")
        pinned = msgspec.json.decode(
            snapshot.profile_snapshot_json.encode(),
            type=PinnedLibraryManagementProfile,
        )
        items = await self._store.get_library_management_bundle_plan_items(
            job_id, bundle_ordinal
        )
        if not items:
            raise ValidationError("The management bundle has no plan items.")
        if any(item.eligibility not in {"eligible", "warning"} for item in items):
            raise ValidationError("A blocked management bundle cannot be published.")

        existing = await self._store.list_file_mutation_journals_for_bundle(
            job_id, bundle_ordinal
        )
        if existing and all(
            value.state in {"catalog_committed", "cleanup_pending", "completed"}
            for value in existing
        ):
            return LibraryManagementBundleCommitResult(
                catalog_revision=snapshot.catalog_revision,
                snapshot_revision=snapshot.row_revision,
                committed_journal_ids=tuple(sorted(value.id for value in existing)),
            )

        self._validate_pinned_configuration(snapshot, pinned)
        roots = self._root_paths(snapshot.policy_revision, pinned)
        prepared: list[_PreparedMutation] = []
        try:
            for item in items:
                prepared.extend(
                    await self._prepare_plan_item(
                        snapshot, pinned, item, roots, bundle_ordinal
                    )
                )
            affected_roots = {
                root_id
                for value in prepared
                for root_id in (
                    value.journal.source_root_id,
                    value.journal.destination_root_id,
                )
                if root_id is not None
            }
            critical_cancelled = False
            async with self._filesystem.write_many(affected_roots):
                critical = asyncio.create_task(
                    self._publish_critical_section(
                        snapshot,
                        pinned,
                        prepared,
                        job_id,
                        bundle_ordinal,
                        worker_id,
                        roots,
                    )
                )
                (
                    (result, mutations),
                    critical_cancelled,
                ) = await self._finish_critical_task(critical)
        except BaseException:

            async def rollback() -> None:
                await self._rollback(prepared)
                await asyncio.to_thread(self._remove_unpublished_temporaries, prepared)

            rollback_task = asyncio.create_task(rollback())
            await self._finish_critical_task(rollback_task)
            raise

        if self._on_commit is not None:
            try:
                await self._on_commit(
                    {value.local_track_id for value in mutations},
                    {value.local_album_id for value in mutations},
                )
            except Exception:  # noqa: BLE001 - post-commit invalidation is retryable
                logger.warning("Library management post-commit invalidation failed")
        if critical_cancelled:
            raise asyncio.CancelledError
        return result

    async def _publish_critical_section(
        self,
        snapshot,
        pinned: PinnedLibraryManagementProfile,
        prepared: list[_PreparedMutation],
        job_id: str,
        bundle_ordinal: int,
        worker_id: str,
        roots: dict[str, Path],
    ) -> tuple[
        LibraryManagementBundleCommitResult, list[LibraryManagementCatalogMutation]
    ]:
        try:
            self._validate_pinned_configuration(snapshot, pinned)
            self._root_paths(snapshot.policy_revision, pinned)
            try:
                await asyncio.to_thread(self._recheck_prepared, prepared, roots)
            except ConflictError:
                await self._record_late_collisions(prepared)
                raise
            for value in prepared:
                await self._publish_one(value)
            await asyncio.to_thread(self._fsync_directories, prepared)
            self._validate_pinned_configuration(snapshot, pinned)
            self._root_paths(snapshot.policy_revision, pinned)
            mutations = [
                value.catalog_mutation
                for value in prepared
                if value.catalog_mutation is not None
            ]
            result = await self._store.commit_library_management_bundle(
                job_id,
                bundle_ordinal,
                worker_id,
                mutations,
                now=self._clock(),
            )
            await self._cleanup_committed(prepared)
            if (
                pinned.profile.organization.remove_empty_directories
                and pinned.profile.organization.source_cleanup
                == "remove_after_confirmed_move"
                and all(value.journal.state == "completed" for value in prepared)
            ):
                await asyncio.to_thread(
                    self._remove_empty_source_directories, prepared, roots
                )
            return result, mutations
        except BaseException:
            rollback_task = asyncio.create_task(self._rollback(prepared))
            await self._finish_critical_task(rollback_task)
            raise

    def _root_paths(
        self,
        policy_revision: str,
        pinned: PinnedLibraryManagementProfile | None = None,
    ) -> dict[str, Path]:
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        if resolver.policy_revision != policy_revision:
            raise StaleRevisionError("Library policy changed after preview.")
        roots = {root.id: Path(root.path) for root in resolver.settings.library_roots}
        if pinned is not None and pinned.recycle_bin_path:
            roots[MANAGEMENT_RECYCLE_ROOT_ID] = Path(pinned.recycle_bin_path)
        return roots

    async def _prepare_plan_item(
        self,
        snapshot,
        pinned: PinnedLibraryManagementProfile,
        item: LibraryManagementPlanItem,
        roots: dict[str, Path],
        bundle_ordinal: int,
    ) -> list[_PreparedMutation]:
        if item.local_track_id is None or item.local_album_id is None:
            raise ValidationError("A management audio item has no local track.")
        if item.expected_track_revision is None or item.expected_album_revision is None:
            raise ValidationError("A management audio item has no catalog revision.")
        source_root = roots.get(item.expected_root_id)
        destination_root_id = item.destination_root_id or item.expected_root_id
        destination_root = roots.get(destination_root_id)
        if source_root is None or destination_root is None:
            raise StaleRevisionError("A library root changed after preview.")
        source = self._safe_path(source_root, item.expected_relative_path)
        destination_relative = (
            item.destination_relative_path or item.expected_relative_path
        )
        destination = self._safe_path(
            destination_root, destination_relative, create_parent=True
        )
        row = await self._store.get_target_track(item.local_track_id)
        if row is None or (
            int(row["row_revision"]) != item.expected_track_revision
            or str(row["root_id"]) != item.expected_root_id
            or str(row["relative_path"]) != item.expected_relative_path
            or str(row["stat_revision"]) != item.expected_stat_revision
            or str(row["tag_revision"]) != item.expected_tag_revision
        ):
            raise StaleRevisionError("A managed track changed after preview.")
        source_fingerprint = await asyncio.to_thread(self._hash_file, source)
        if source_fingerprint != item.expected_file_fingerprint:
            raise StaleRevisionError("A managed file changed after preview.")
        current = await asyncio.to_thread(self._audio.read, source)
        desired = await self._desired_document(item)
        identity_values = await self._validate_subject_revision(item, desired)
        diff = json.loads(item.diff_json)
        restore_blob_sha256 = diff.get("restore_snapshot_blob_sha256")
        write_plan = self._write_planner.plan(
            current=current, desired=desired, profile=pinned.profile
        )
        if write_plan.blockers:
            raise ValidationError(
                "The staged write no longer passes capability checks."
            )

        now = self._clock()
        baseline_id = str(uuid.uuid5(_BASELINE_NAMESPACE, item.local_track_id))
        operation_snapshot_id = str(
            uuid.uuid5(_SNAPSHOT_NAMESPACE, f"{snapshot.job_id}:{item.ordinal}")
        )
        operation_snapshot = await self._store.get_management_operation_snapshot(
            snapshot.job_id, bundle_ordinal, item.local_track_id
        )
        if operation_snapshot is None:
            ancillary_snapshot_json = await self._capture_ancillary_snapshot(
                item,
                roots,
                source,
                destination,
                created_at=now,
            )
            management_state = await self._store.get_track_management_state(
                item.local_track_id
            )
            snapshot_blob = await self._blobs.add_bytes(
                msgspec.json.encode(write_plan.snapshot),
                kind="tag_snapshot",
                created_at=now,
            )
            baseline, _created = await self._store.ensure_management_baseline(
                LibraryManagementBaseline(
                    id=baseline_id,
                    local_track_id=item.local_track_id,
                    original_root_id=item.expected_root_id,
                    original_relative_path=item.expected_relative_path,
                    format=write_plan.audio_format,
                    adapter_version=write_plan.snapshot.adapter_version,
                    semantic_snapshot_blob_sha256=snapshot_blob.sha256,
                    stat_revision=item.expected_stat_revision,
                    tag_revision=item.expected_tag_revision,
                    image_snapshot_json=json.dumps(
                        msgspec.to_builtins(write_plan.snapshot.artwork),
                        separators=(",", ":"),
                        sort_keys=True,
                    ),
                    ancillary_snapshot_json=ancillary_snapshot_json,
                    file_mtime_ns=write_plan.snapshot.file_attributes.mtime_ns,
                    file_mode=write_plan.snapshot.file_attributes.permission_bits,
                    identity_revision=item.expected_identity_revision,
                    created_at=now,
                )
            )
            operation_snapshot = await self._store.ensure_management_operation_snapshot(
                LibraryManagementOperationSnapshot(
                    id=operation_snapshot_id,
                    job_id=snapshot.job_id,
                    work_ordinal=bundle_ordinal,
                    local_track_id=item.local_track_id,
                    before_root_id=item.expected_root_id,
                    before_relative_path=item.expected_relative_path,
                    after_root_id=destination_root_id,
                    after_relative_path=destination_relative,
                    format=write_plan.audio_format,
                    adapter_version=write_plan.snapshot.adapter_version,
                    semantic_snapshot_blob_sha256=snapshot_blob.sha256,
                    image_snapshot_json=baseline.image_snapshot_json,
                    ancillary_snapshot_json=ancillary_snapshot_json,
                    before_management_state_json=(
                        json.dumps(
                            msgspec.to_builtins(management_state),
                            separators=(",", ":"),
                            sort_keys=True,
                        )
                        if management_state is not None
                        else "{}"
                    ),
                    file_mtime_ns=write_plan.snapshot.file_attributes.mtime_ns,
                    file_mode=write_plan.snapshot.file_attributes.permission_bits,
                    source_fingerprint=source_fingerprint,
                    created_at=now,
                    expires_at=now
                    + self._preferences.get_library_management_settings().undo_retention_days
                    * 24
                    * 60
                    * 60,
                )
            )
        else:
            baseline = await self._store.get_management_baseline(item.local_track_id)
            if (
                baseline is None
                or baseline.id != baseline_id
                or operation_snapshot.id != operation_snapshot_id
                or operation_snapshot.source_fingerprint != source_fingerprint
                or operation_snapshot.before_root_id != item.expected_root_id
                or operation_snapshot.before_relative_path
                != item.expected_relative_path
                or operation_snapshot.after_root_id != destination_root_id
                or operation_snapshot.after_relative_path != destination_relative
            ):
                raise ConflictError(
                    "The durable management snapshot does not match its retry."
                )
        temporary = self._artifact_path(
            destination,
            snapshot.job_id,
            item.ordinal,
            "audio-temp",
            destination.suffix,
        )
        backup = self._artifact_path(
            source, snapshot.job_id, item.ordinal, "audio-backup", source.suffix
        )
        journal = await self._ensure_journal(
            LibraryFileMutationJournal(
                id=self._journal_id(
                    snapshot.job_id, item.ordinal, "audio", item.local_track_id
                ),
                job_id=snapshot.job_id,
                plan_item_ordinal=item.ordinal,
                subject_kind="audio",
                subject_key=item.local_track_id,
                local_track_id=item.local_track_id,
                source_root_id=item.expected_root_id,
                source_relative_path=item.expected_relative_path,
                temporary_root_id=destination_root_id,
                temporary_relative_path=temporary.relative_to(
                    destination_root
                ).as_posix(),
                backup_root_id=item.expected_root_id,
                backup_relative_path=backup.relative_to(source_root).as_posix(),
                destination_root_id=destination_root_id,
                destination_relative_path=destination_relative,
                source_fingerprint=source_fingerprint,
                baseline_id=baseline.id,
                operation_snapshot_id=operation_snapshot_id,
                recovery_evidence_json=(
                    '{"mutation":"recycle","cataloged":true}'
                    if diff.get("duplicate_recycle_only")
                    else "{}"
                ),
                state="planned",
                created_at=now,
                updated_at=now,
            )
        )
        audio_mutation = _PreparedMutation(
            journal=journal,
            plan_item=item,
            source=source,
            temporary=temporary,
            destination=destination,
            backup=backup,
            source_fingerprint=source_fingerprint,
            staged_fingerprint=journal.staged_fingerprint or "",
            remove_source=(
                snapshot.mode in {"undo", "baseline_restore", "duplicate_resolution"}
                or pinned.profile.organization.source_cleanup
                == "remove_after_confirmed_move"
            ),
            recycle_move=bool(diff.get("duplicate_recycle_only")),
        )
        prepared = [audio_mutation]
        try:
            if journal.state not in {"staged", "validated"}:
                if diff.get("duplicate_recycle_only"):
                    await asyncio.to_thread(self._copy_temp, source, temporary)
                elif restore_blob_sha256 is not None:
                    restore_bytes = await self._blobs.read_bytes(
                        str(restore_blob_sha256)
                    )
                    try:
                        restore_snapshot = msgspec.json.decode(
                            restore_bytes, type=SemanticTagSnapshot
                        )
                    except (msgspec.DecodeError, msgspec.ValidationError) as error:
                        raise ValidationError(
                            "The restoration snapshot is invalid."
                        ) from error
                    if (
                        restore_snapshot.probe.detected_format
                        != current.probe.detected_format
                    ):
                        raise ValidationError(
                            "The restoration snapshot format no longer matches."
                        )
                    await asyncio.to_thread(
                        self._stage_restore,
                        source,
                        temporary,
                        restore_snapshot,
                    )
                else:
                    await asyncio.to_thread(
                        self._stage_audio, source, temporary, write_plan
                    )
                staged_fingerprint = await asyncio.to_thread(self._hash_file, temporary)
                journal = await self._advance_to_validated(journal, staged_fingerprint)
            else:
                staged_fingerprint = await asyncio.to_thread(self._hash_file, temporary)
                if staged_fingerprint != journal.staged_fingerprint:
                    raise ConflictError(
                        "The staged management audio no longer matches its journal."
                    )
                journal = await self._advance_to_validated(journal, staged_fingerprint)
            audio_mutation.journal = journal
            audio_mutation.staged_fingerprint = staged_fingerprint
            audio_mutation.catalog_mutation = await self._build_catalog_mutation(
                item,
                journal,
                pinned,
                temporary,
                destination,
                snapshot.profile_revision,
                snapshot.naming_revision,
                identity_values=identity_values,
            )
            await self._prepare_external_artwork(snapshot.job_id, item, roots, prepared)
            await self._prepare_sidecars(
                snapshot.job_id,
                item,
                source,
                destination,
                roots,
                prepared,
                remove_source=audio_mutation.remove_source,
            )
            await self._prepare_deletions(snapshot.job_id, item, roots, prepared)
            untracked_recycle = diff.get("recycle_untracked_collision")
            if untracked_recycle is not None:
                auxiliary = await self._prepare_untracked_recycle(
                    snapshot.job_id,
                    item,
                    roots,
                    untracked_recycle,
                )
                prepared.insert(0, auxiliary)
            return prepared
        except BaseException:

            async def rollback() -> None:
                await self._rollback(prepared)
                await asyncio.to_thread(self._remove_unpublished_temporaries, prepared)

            rollback_task = asyncio.create_task(rollback())
            await self._finish_critical_task(rollback_task)
            raise

    async def _capture_ancillary_snapshot(
        self,
        item: LibraryManagementPlanItem,
        roots: dict[str, Path],
        source_audio: Path,
        destination_audio: Path,
        *,
        created_at: float,
    ) -> str:
        """Capture exact sidecar/art before bytes needed by per-operation undo."""

        values: list[dict[str, object]] = []
        replacements = {
            str(value.get("destination_relative_path")): str(
                value.get("existing_file_fingerprint")
            )
            for value in json.loads(item.collision_json)
            if value.get("classification") == "configured_external_artwork_replacement"
            and value.get("destination_relative_path")
            and value.get("existing_file_fingerprint")
        }
        for choice in json.loads(item.artwork_choices_json):
            relative = choice.get("destination_relative_path")
            if choice.get("output_kind") != "external" or not relative:
                continue
            root_id = item.destination_root_id or item.expected_root_id
            entry: dict[str, object] = {
                "kind": "external_art",
                "before_exists": False,
                "after_exists": True,
                "after_root_id": root_id,
                "after_relative_path": str(relative),
                "after_blob_sha256": str(choice["blob_sha256"]),
            }
            expected = replacements.get(str(relative))
            if expected is not None:
                path = self._safe_path(roots[root_id], str(relative))
                fingerprint = await asyncio.to_thread(self._hash_file, path)
                if fingerprint != expected:
                    raise StaleRevisionError(
                        "Planned external artwork changed after preview."
                    )
                blob_sha256 = await self._capture_snapshot_file(
                    path, kind="image", created_at=created_at
                )
                entry.update(
                    {
                        "before_exists": True,
                        "before_root_id": root_id,
                        "before_relative_path": str(relative),
                        "blob_sha256": blob_sha256,
                    }
                )
            values.append(entry)
        for sidecar in json.loads(item.diff_json).get("sidecars", []):
            source = self._safe_child(
                source_audio.parent, str(sidecar["source_relative_path"])
            )
            source_fingerprint = await asyncio.to_thread(self._hash_file, source)
            if source_fingerprint != sidecar["sha256"]:
                raise StaleRevisionError("A planned sidecar changed after preview.")
            blob_sha256 = await self._capture_snapshot_file(
                source, kind="sidecar_manifest", created_at=created_at
            )
            destination = self._safe_child(
                destination_audio.parent,
                str(sidecar["destination_relative_path"]),
            )
            destination_root_id = item.destination_root_id or item.expected_root_id
            values.append(
                {
                    "kind": "sidecar",
                    "before_exists": True,
                    "before_root_id": item.expected_root_id,
                    "before_relative_path": source.relative_to(
                        roots[item.expected_root_id]
                    ).as_posix(),
                    "after_root_id": destination_root_id,
                    "after_relative_path": destination.relative_to(
                        roots[destination_root_id]
                    ).as_posix(),
                    "blob_sha256": blob_sha256,
                    "after_blob_sha256": source_fingerprint,
                }
            )
        for deletion in json.loads(item.diff_json).get("delete_outputs", []):
            root_id = str(deletion["root_id"])
            root = roots.get(root_id)
            if root is None:
                raise StaleRevisionError("A deletion root changed after preview.")
            relative = str(deletion["relative_path"])
            source = self._safe_path(root, relative)
            source_fingerprint = await asyncio.to_thread(self._hash_file, source)
            if source_fingerprint != deletion["sha256"]:
                raise StaleRevisionError("A planned deletion changed after preview.")
            blob_sha256 = await self._capture_snapshot_file(
                source, kind="image", created_at=created_at
            )
            values.append(
                {
                    "kind": "external_art",
                    "before_exists": True,
                    "before_root_id": root_id,
                    "before_relative_path": relative,
                    "blob_sha256": blob_sha256,
                    "after_exists": False,
                    "after_root_id": root_id,
                    "after_relative_path": relative,
                }
            )
        return json.dumps(values, separators=(",", ":"), sort_keys=True)

    async def _capture_snapshot_file(
        self,
        path: Path,
        *,
        kind: ManagementBlobKind,
        created_at: float,
    ) -> str:
        fingerprint = await asyncio.to_thread(self._hash_file, path)
        existing = await self._store.get_management_blob(fingerprint)
        if existing is not None:
            await self._blobs.read_bytes(fingerprint)
            return fingerprint
        blob = await self._blobs.add_file(path, kind=kind, created_at=created_at)
        return blob.sha256

    async def build_recovery_catalog_mutation(
        self,
        item: LibraryManagementPlanItem,
        journal: LibraryFileMutationJournal,
        pinned: PinnedLibraryManagementProfile,
        destination: Path,
        *,
        profile_revision: str,
        naming_revision: str,
    ) -> LibraryManagementCatalogMutation:
        """Rebuild the deterministic catalog payload from exact published bytes."""

        desired = msgspec.json.decode(
            item.desired_document_json.encode(), type=DesiredAudioDocument
        )
        identity_values = await self._validate_subject_revision(item, desired)
        return await self._build_catalog_mutation(
            item,
            journal,
            pinned,
            destination,
            destination,
            profile_revision,
            naming_revision,
            identity_values=identity_values,
        )

    async def _build_catalog_mutation(
        self,
        item: LibraryManagementPlanItem,
        journal: LibraryFileMutationJournal,
        pinned: PinnedLibraryManagementProfile,
        content_path: Path,
        destination: Path,
        profile_revision: str,
        naming_revision: str,
        *,
        identity_values: tuple[str, str, str],
    ) -> LibraryManagementCatalogMutation:
        required = (
            item.local_track_id,
            item.local_album_id,
            item.expected_album_revision,
            item.expected_track_revision,
            item.expected_identity_revision,
            item.expected_album_identity_revision,
            item.expected_override_revision,
            journal.destination_root_id,
            journal.destination_relative_path,
            journal.staged_fingerprint,
            journal.baseline_id,
            journal.operation_snapshot_id,
        )
        if any(value is None for value in required):
            raise ValidationError(
                "A published management journal has incomplete catalog evidence."
            )
        fingerprint = await asyncio.to_thread(self._hash_file, content_path)
        if fingerprint != journal.staged_fingerprint:
            raise ConflictError(
                "Published management audio no longer matches its journal."
            )
        result_document = await asyncio.to_thread(self._audio.read, content_path)
        tag, info = legacy_audio_projection(result_document)
        content_stat = await asyncio.to_thread(content_path.stat)
        destination_relative = str(journal.destination_relative_path)
        diff = json.loads(item.diff_json)
        return LibraryManagementCatalogMutation(
            journal_id=journal.id,
            plan_item_ordinal=item.ordinal,
            local_track_id=str(item.local_track_id),
            local_album_id=str(item.local_album_id),
            expected_album_revision=int(item.expected_album_revision),
            expected_track_revision=int(item.expected_track_revision),
            expected_root_id=item.expected_root_id,
            expected_relative_path=item.expected_relative_path,
            expected_stat_revision=item.expected_stat_revision,
            expected_tag_revision=item.expected_tag_revision,
            expected_identity_revision=int(item.expected_identity_revision),
            expected_album_identity_revision=int(item.expected_album_identity_revision),
            expected_override_revision=str(item.expected_override_revision),
            expected_release_mbid=identity_values[0],
            expected_recording_mbid=identity_values[1],
            expected_release_track_mbid=identity_values[2],
            destination_root_id=str(journal.destination_root_id),
            destination_relative_path=destination_relative,
            destination_file_path=str(destination),
            destination_path_hash=hashlib.sha256(
                destination_relative.encode()
            ).hexdigest(),
            file_size_bytes=content_stat.st_size,
            file_mtime_ns=content_stat.st_mtime_ns,
            stat_revision=revision_from_stat(content_stat),
            tag_revision=hashlib.sha256(msgspec.json.encode(tag)).hexdigest(),
            file_fingerprint=fingerprint,
            tag=tag,
            info=info,
            baseline_id=str(journal.baseline_id),
            operation_snapshot_id=str(journal.operation_snapshot_id),
            applied_profile_id=pinned.profile.id,
            applied_profile_revision=profile_revision,
            applied_projection_hash=item.desired_document_hash,
            applied_naming_script_revision=naming_revision,
            applied_override_revision=item.expected_override_revision,
            restored_management_state_json=(
                json.dumps(
                    diff["restore_management_state"],
                    separators=(",", ":"),
                    sort_keys=True,
                )
                if "restore_management_state" in diff
                else None
            ),
            recycle_only=bool(diff.get("duplicate_recycle_only")),
        )

    def _validate_pinned_configuration(
        self, snapshot, pinned: PinnedLibraryManagementProfile
    ) -> None:
        current = self._preferences.get_library_management_settings()
        if current.settings_revision != snapshot.settings_revision:
            raise StaleRevisionError(
                "Library management settings changed after preview."
            )
        if snapshot.mode in {"undo", "baseline_restore"}:
            return
        profile = next(
            (value for value in current.profiles if value.id == pinned.profile.id), None
        )
        naming = next(
            (
                value
                for value in current.naming_scripts
                if value.id == pinned.naming_script.id
            ),
            None,
        )
        if (
            profile is None
            or profile.revision != snapshot.profile_revision
            or naming is None
            or naming.revision != snapshot.naming_revision
        ):
            raise StaleRevisionError("The management profile changed after preview.")

    async def _validate_subject_revision(
        self,
        item: LibraryManagementPlanItem,
        desired: DesiredAudioDocument,
    ) -> tuple[str, str, str]:
        if (
            item.local_album_id is None
            or item.local_track_id is None
            or item.expected_identity_revision is None
            or item.expected_album_identity_revision is None
            or item.expected_override_revision is None
        ):
            raise ValidationError(
                "A management item has no accepted identity revision."
            )
        identity = await self._store.get_accepted_library_management_identity(
            item.local_album_id, local_track_ids=(item.local_track_id,)
        )
        track = identity.tracks[0] if identity is not None and identity.tracks else None
        release_mbid = identity.release_mbid if identity is not None else None
        release_group_mbid = (
            identity.release_group_mbid if identity is not None else None
        )
        recording_mbid = track.recording_mbid if track is not None else None
        release_track_mbid = track.release_track_mbid if track is not None else None
        if (
            identity is None
            or identity.identity_revision != item.expected_album_identity_revision
            or release_mbid is None
            or track is None
            or track.identity_revision != item.expected_identity_revision
            or track.release_mbid != release_mbid
            or recording_mbid is None
            or release_track_mbid is None
        ):
            raise StaleRevisionError(
                "The accepted MusicBrainz mapping changed after preview."
            )
        for name, expected in (
            ("musicbrainz_release_group_id", release_group_mbid),
            ("musicbrainz_release_id", release_mbid),
            ("musicbrainz_recording_id", recording_mbid),
            ("musicbrainz_release_track_id", release_track_mbid),
        ):
            field = next(
                (value for value in desired.fields if value.name == name), None
            )
            if field is not None and field.action == "set" and field.value != expected:
                raise StaleRevisionError(
                    "The accepted MusicBrainz mapping changed after preview."
                )
        _album_overrides, album_revision = await self._store.list_management_overrides(
            subject_kind="album", subject_id=item.local_album_id
        )
        _track_overrides, track_revision = await self._store.list_management_overrides(
            subject_kind="track", subject_id=item.local_track_id
        )
        override_revision = hashlib.sha256(
            f"{album_revision}\x00{track_revision}".encode()
        ).hexdigest()
        if override_revision != item.expected_override_revision:
            raise StaleRevisionError("Management overrides changed after preview.")
        return release_mbid, recording_mbid, release_track_mbid

    async def _desired_document(
        self, item: LibraryManagementPlanItem
    ) -> DesiredAudioDocument:
        desired = msgspec.json.decode(
            item.desired_document_json.encode(), type=DesiredAudioDocument
        )
        embedded: list[EmbeddedArtworkDescriptor] = []
        for choice in json.loads(item.artwork_choices_json):
            if choice.get("output_kind") != "embedded":
                continue
            content = await self._blobs.read_bytes(str(choice["blob_sha256"]))
            if hashlib.sha256(content).hexdigest() != choice["blob_sha256"]:
                raise ConflictError("Pinned embedded artwork changed after preview.")
            embedded.append(
                EmbeddedArtworkDescriptor(
                    image_type=choice["image_type"],
                    mime_type=choice["mime_type"],
                    description=choice.get("description", ""),
                    width=choice.get("width"),
                    height=choice.get("height"),
                    byte_size=len(content),
                    sha256=choice["blob_sha256"],
                    content=content,
                    format_supported=True,
                )
            )
        return msgspec.structs.replace(
            desired, artwork=tuple(embedded) if embedded else desired.artwork
        )

    async def _prepare_external_artwork(
        self,
        job_id: str,
        item: LibraryManagementPlanItem,
        roots: dict[str, Path],
        prepared: list[_PreparedMutation],
    ) -> None:
        replacements = {
            str(value.get("destination_relative_path")): str(
                value.get("existing_file_fingerprint")
            )
            for value in json.loads(item.collision_json)
            if value.get("classification") == "configured_external_artwork_replacement"
            and value.get("destination_relative_path")
            and value.get("existing_file_fingerprint")
        }
        for index, choice in enumerate(json.loads(item.artwork_choices_json)):
            relative = choice.get("destination_relative_path")
            if choice.get("output_kind") != "external" or not relative:
                continue
            root_id = item.destination_root_id or item.expected_root_id
            root = roots[root_id]
            destination = self._safe_path(root, relative, create_parent=True)
            temporary = self._artifact_path(
                destination,
                job_id,
                item.ordinal,
                f"art-{index}-temp",
                destination.suffix,
            )
            backup = self._artifact_path(
                destination,
                job_id,
                item.ordinal,
                f"art-{index}-backup",
                destination.suffix,
            )
            content = await self._blobs.read_bytes(str(choice["blob_sha256"]))
            fingerprint = hashlib.sha256(content).hexdigest()
            expected_existing_fingerprint = replacements.get(str(relative))
            existing_fingerprint = None
            source = None
            if expected_existing_fingerprint is not None:
                try:
                    existing_fingerprint = await asyncio.to_thread(
                        self._hash_file, destination
                    )
                except OSError as error:
                    raise StaleRevisionError(
                        "Planned external artwork changed after preview."
                    ) from error
                if existing_fingerprint != expected_existing_fingerprint:
                    raise StaleRevisionError(
                        "Planned external artwork changed after preview."
                    )
                source = destination
            journal = await self._ensure_journal(
                LibraryFileMutationJournal(
                    id=self._journal_id(
                        job_id, item.ordinal, "external_art", str(relative)
                    ),
                    job_id=job_id,
                    plan_item_ordinal=item.ordinal,
                    subject_kind="external_art",
                    subject_key=str(relative),
                    source_root_id=(
                        root_id if expected_existing_fingerprint is not None else None
                    ),
                    source_relative_path=(
                        str(relative)
                        if expected_existing_fingerprint is not None
                        else None
                    ),
                    temporary_root_id=root_id,
                    temporary_relative_path=temporary.relative_to(root).as_posix(),
                    backup_root_id=root_id,
                    backup_relative_path=backup.relative_to(root).as_posix(),
                    destination_root_id=root_id,
                    destination_relative_path=str(relative),
                    source_fingerprint=existing_fingerprint,
                    staged_fingerprint=fingerprint,
                    state="planned",
                    created_at=self._clock(),
                    updated_at=self._clock(),
                )
            )
            mutation = _PreparedMutation(
                journal=journal,
                plan_item=item,
                source=source,
                temporary=temporary,
                destination=destination,
                backup=backup,
                source_fingerprint=existing_fingerprint,
                staged_fingerprint=fingerprint,
            )
            prepared.append(mutation)
            await asyncio.to_thread(self._write_temp_bytes, temporary, content)
            mutation.journal = await self._advance_to_validated(journal, fingerprint)

    async def _prepare_sidecars(
        self,
        job_id: str,
        item: LibraryManagementPlanItem,
        source_audio: Path,
        destination_audio: Path,
        roots: dict[str, Path],
        prepared: list[_PreparedMutation],
        *,
        remove_source: bool,
    ) -> None:
        diff = json.loads(item.diff_json)
        for index, sidecar in enumerate(diff.get("sidecars", [])):
            source = self._safe_child(
                source_audio.parent, str(sidecar["source_relative_path"])
            )
            destination = self._safe_child(
                destination_audio.parent,
                str(sidecar["destination_relative_path"]),
                create_parent=True,
            )
            source_fingerprint = await asyncio.to_thread(self._hash_file, source)
            source_stat = await asyncio.to_thread(source.stat)
            if source_fingerprint != sidecar[
                "sha256"
            ] or source_stat.st_mtime_ns != int(sidecar["mtime_ns"]):
                raise StaleRevisionError("A planned sidecar changed after preview.")
            root_id = item.destination_root_id or item.expected_root_id
            root = roots[root_id]
            temporary = self._artifact_path(
                destination,
                job_id,
                item.ordinal,
                f"sidecar-{index}-temp",
                destination.suffix,
            )
            staged_fingerprint = source_fingerprint
            journal = await self._ensure_journal(
                LibraryFileMutationJournal(
                    id=self._journal_id(job_id, item.ordinal, "sidecar", str(index)),
                    job_id=job_id,
                    plan_item_ordinal=item.ordinal,
                    subject_kind="sidecar",
                    subject_key=str(sidecar["source_relative_path"]),
                    source_root_id=item.expected_root_id,
                    source_relative_path=(
                        source.relative_to(roots[item.expected_root_id]).as_posix()
                    ),
                    temporary_root_id=root_id,
                    temporary_relative_path=temporary.relative_to(root).as_posix(),
                    destination_root_id=root_id,
                    destination_relative_path=destination.relative_to(root).as_posix(),
                    source_fingerprint=source_fingerprint,
                    staged_fingerprint=staged_fingerprint,
                    state="planned",
                    created_at=self._clock(),
                    updated_at=self._clock(),
                )
            )
            mutation = _PreparedMutation(
                journal=journal,
                plan_item=item,
                source=source,
                temporary=temporary,
                destination=destination,
                backup=None,
                source_fingerprint=source_fingerprint,
                staged_fingerprint=staged_fingerprint,
                remove_source=remove_source,
            )
            prepared.append(mutation)
            await asyncio.to_thread(self._copy_temp, source, temporary)
            mutation.journal = await self._advance_to_validated(
                journal, staged_fingerprint
            )

    async def _prepare_deletions(
        self,
        job_id: str,
        item: LibraryManagementPlanItem,
        roots: dict[str, Path],
        prepared: list[_PreparedMutation],
    ) -> None:
        for index, deletion in enumerate(
            json.loads(item.diff_json).get("delete_outputs", [])
        ):
            root_id = str(deletion["root_id"])
            root = roots.get(root_id)
            if root is None:
                raise StaleRevisionError("A deletion root changed after preview.")
            relative = str(deletion["relative_path"])
            source = self._safe_path(root, relative)
            source_fingerprint = await asyncio.to_thread(self._hash_file, source)
            if source_fingerprint != deletion["sha256"]:
                raise StaleRevisionError("A planned deletion changed after preview.")
            temporary = self._artifact_path(
                source, job_id, item.ordinal, f"delete-{index}-temp", source.suffix
            )
            backup = self._artifact_path(
                source, job_id, item.ordinal, f"delete-{index}-backup", source.suffix
            )
            journal = await self._ensure_journal(
                LibraryFileMutationJournal(
                    id=self._journal_id(job_id, item.ordinal, "delete", relative),
                    job_id=job_id,
                    plan_item_ordinal=item.ordinal,
                    subject_kind="external_art",
                    subject_key=f"delete:{relative}",
                    source_root_id=root_id,
                    source_relative_path=relative,
                    temporary_root_id=root_id,
                    temporary_relative_path=temporary.relative_to(root).as_posix(),
                    backup_root_id=root_id,
                    backup_relative_path=backup.relative_to(root).as_posix(),
                    destination_root_id=root_id,
                    destination_relative_path=relative,
                    source_fingerprint=source_fingerprint,
                    staged_fingerprint=source_fingerprint,
                    recovery_evidence_json='{"mutation":"delete"}',
                    state="planned",
                    created_at=self._clock(),
                    updated_at=self._clock(),
                )
            )
            mutation = _PreparedMutation(
                journal=journal,
                plan_item=item,
                source=source,
                temporary=temporary,
                destination=source,
                backup=backup,
                source_fingerprint=source_fingerprint,
                staged_fingerprint=source_fingerprint,
                delete_only=True,
            )
            prepared.append(mutation)
            await asyncio.to_thread(self._copy_temp, source, temporary)
            mutation.journal = await self._advance_to_validated(
                journal, source_fingerprint
            )

    async def _prepare_untracked_recycle(
        self,
        job_id: str,
        item: LibraryManagementPlanItem,
        roots: dict[str, Path],
        evidence: dict,
    ) -> _PreparedMutation:
        source_root_id = str(evidence.get("existing_root_id", ""))
        source_relative = str(evidence.get("existing_relative_path", ""))
        source_fingerprint = str(evidence.get("existing_file_fingerprint", ""))
        recycle_relative = str(evidence.get("recycle_relative_path", ""))
        source_root = roots.get(source_root_id)
        recycle_root = roots.get(MANAGEMENT_RECYCLE_ROOT_ID)
        if source_root is None or recycle_root is None:
            raise StaleRevisionError("The duplicate recycle configuration changed.")
        source = self._safe_path(source_root, source_relative)
        destination = self._safe_path(
            recycle_root, recycle_relative, create_parent=True
        )
        if await asyncio.to_thread(self._hash_file, source) != source_fingerprint:
            raise StaleRevisionError("The duplicate destination changed after preview.")
        temporary = self._artifact_path(
            destination,
            job_id,
            item.ordinal,
            "untracked-recycle-temp",
            destination.suffix,
        )
        backup = self._artifact_path(
            source,
            job_id,
            item.ordinal,
            "untracked-recycle-backup",
            source.suffix,
        )
        journal = await self._ensure_journal(
            LibraryFileMutationJournal(
                id=self._journal_id(
                    job_id, item.ordinal, "untracked-recycle", source_relative
                ),
                job_id=job_id,
                plan_item_ordinal=item.ordinal,
                subject_kind="external_art",
                subject_key=f"recycle:{source_root_id}:{source_relative}",
                source_root_id=source_root_id,
                source_relative_path=source_relative,
                temporary_root_id=MANAGEMENT_RECYCLE_ROOT_ID,
                temporary_relative_path=temporary.relative_to(recycle_root).as_posix(),
                backup_root_id=source_root_id,
                backup_relative_path=backup.relative_to(source_root).as_posix(),
                destination_root_id=MANAGEMENT_RECYCLE_ROOT_ID,
                destination_relative_path=recycle_relative,
                source_fingerprint=source_fingerprint,
                staged_fingerprint=source_fingerprint,
                recovery_evidence_json=json.dumps(
                    {"mutation": "recycle", "cataloged": False},
                    separators=(",", ":"),
                    sort_keys=True,
                ),
                state="planned",
                created_at=self._clock(),
                updated_at=self._clock(),
            )
        )
        mutation = _PreparedMutation(
            journal=journal,
            plan_item=item,
            source=source,
            temporary=temporary,
            destination=destination,
            backup=backup,
            source_fingerprint=source_fingerprint,
            staged_fingerprint=source_fingerprint,
            remove_source=True,
            recycle_move=True,
        )
        await asyncio.to_thread(self._copy_temp, source, temporary)
        mutation.journal = await self._advance_to_validated(journal, source_fingerprint)
        return mutation

    async def _ensure_journal(
        self, journal: LibraryFileMutationJournal
    ) -> LibraryFileMutationJournal:
        return await self._store.ensure_file_mutation_journal(journal)

    async def _advance_to_validated(
        self, journal: LibraryFileMutationJournal, fingerprint: str
    ) -> LibraryFileMutationJournal:
        transitions = {
            "planned": "snapshot_saved",
            "snapshot_saved": "staged",
            "staged": "validated",
        }
        current = journal
        while current.state in transitions:
            current = await self._store.transition_file_mutation_journal(
                current.id,
                expected_state=current.state,
                new_state=transitions[current.state],
                expected_row_revision=current.row_revision,
                updated_at=self._clock(),
                staged_fingerprint=fingerprint,
            )
        if current.state != "validated":
            raise StaleRevisionError("The mutation journal is not ready to publish.")
        return current

    def _recheck_prepared(
        self, prepared: list[_PreparedMutation], roots: dict[str, Path]
    ) -> None:
        recycled_sources = {
            value.source: value.source_fingerprint
            for value in prepared
            if value.recycle_move and value.source is not None
        }
        for value in prepared:
            journal = value.journal
            for root_id, relative, expected in (
                (
                    journal.source_root_id,
                    journal.source_relative_path,
                    value.source,
                ),
                (
                    journal.temporary_root_id,
                    journal.temporary_relative_path,
                    value.temporary,
                ),
                (
                    journal.destination_root_id,
                    journal.destination_relative_path,
                    value.destination,
                ),
                (
                    journal.backup_root_id,
                    journal.backup_relative_path,
                    value.backup,
                ),
            ):
                if root_id is None or relative is None or expected is None:
                    continue
                root = roots.get(root_id)
                if root is None or self._safe_path(root, relative) != expected:
                    raise StaleRevisionError(
                        "A management path changed before publication."
                    )
            if (
                value.backup is not None
                and (
                    value.source == value.destination
                    or value.recycle_move
                    or (
                        journal.subject_kind == "external_art"
                        and value.source is not None
                    )
                )
                and (value.backup.exists() or value.backup.is_symlink())
            ):
                raise ConflictError("A management backup path is occupied.")
            if self._hash_file(value.temporary) != value.staged_fingerprint:
                raise StaleRevisionError("A staged output changed before publication.")
            if value.source is not None and value.source != value.destination:
                if self._hash_file(value.source) != value.source_fingerprint:
                    raise StaleRevisionError("A source changed before publication.")
            if value.journal.subject_kind in {"audio", "sidecar"}:
                if (
                    value.destination != value.source
                    and (value.destination.exists() or value.destination.is_symlink())
                    and value.destination not in recycled_sources
                ):
                    raise ConflictError("A destination was created after preview.")
            elif value.source is not None:
                if self._hash_file(value.source) != value.source_fingerprint:
                    raise StaleRevisionError("External artwork changed after preview.")
                if value.recycle_move and (
                    value.destination.exists() or value.destination.is_symlink()
                ):
                    raise ConflictError(
                        "A recycle destination was created after preview."
                    )
            elif value.destination.exists() or value.destination.is_symlink():
                raise ConflictError("An artwork destination was created after preview.")
            if value.destination != value.source and value.destination.parent.is_dir():
                wanted = unicodedata.normalize("NFC", value.destination.name).casefold()
                with os.scandir(value.destination.parent) as entries:
                    for index, entry in enumerate(entries, start=1):
                        if index > _MAX_DIRECTORY_COLLISION_ENTRIES:
                            raise ConflictError(
                                "A destination directory exceeds the collision limit."
                            )
                        if entry.name != value.destination.name and (
                            unicodedata.normalize("NFC", entry.name).casefold()
                            == wanted
                        ):
                            sibling = value.destination.parent / entry.name
                            if sibling in recycled_sources:
                                continue
                            raise ConflictError(
                                "A normalized destination was created after preview."
                            )

    async def _record_late_collisions(self, prepared: list[_PreparedMutation]) -> None:
        collisions = []
        now = self._clock()
        for value in prepared:
            if value.destination == value.source or not (
                value.destination.exists()
                or value.destination.is_symlink()
                or self._has_normalized_destination_sibling(value.destination)
            ):
                continue
            collisions.append(
                LibraryManagementCollisionEvidence(
                    id=str(
                        uuid.uuid5(
                            _JOURNAL_NAMESPACE,
                            f"late-collision:{value.journal.id}",
                        )
                    ),
                    job_id=value.journal.job_id,
                    plan_item_ordinal=value.journal.plan_item_ordinal,
                    classification="destination_created_after_preview",
                    destination_root_id=value.journal.destination_root_id or "",
                    destination_relative_path=(
                        value.journal.destination_relative_path or ""
                    ),
                    evidence_json=json.dumps(
                        {
                            "reason": "DESTINATION_CREATED_AFTER_PREVIEW",
                            "is_symlink": value.destination.is_symlink(),
                        },
                        separators=(",", ":"),
                        sort_keys=True,
                    ),
                    created_at=now,
                )
            )
        if collisions:
            await self._store.add_management_collision_evidence(collisions)

    @staticmethod
    def _has_normalized_destination_sibling(destination: Path) -> bool:
        if not destination.parent.is_dir():
            return False
        wanted = unicodedata.normalize("NFC", destination.name).casefold()
        with os.scandir(destination.parent) as entries:
            return any(
                entry.name != destination.name
                and unicodedata.normalize("NFC", entry.name).casefold() == wanted
                for entry in entries
            )

    async def _publish_one(self, value: _PreparedMutation) -> None:
        journal = value.journal
        if value.recycle_move:
            assert value.backup is not None and value.source is not None
            await asyncio.to_thread(os.replace, value.source, value.backup)
            value.source_backed_up = True
            journal = await self._store.transition_file_mutation_journal(
                journal.id,
                expected_state="validated",
                new_state="source_backed_up",
                expected_row_revision=journal.row_revision,
                updated_at=self._clock(),
            )
            value.journal = journal
        elif value.source == value.destination or (
            journal.subject_kind == "external_art" and value.source is not None
        ):
            assert value.backup is not None and value.source is not None
            await asyncio.to_thread(os.replace, value.source, value.backup)
            value.source_backed_up = True
            journal = await self._store.transition_file_mutation_journal(
                journal.id,
                expected_state="validated",
                new_state="source_backed_up",
                expected_row_revision=journal.row_revision,
                updated_at=self._clock(),
            )
            value.journal = journal
        if value.delete_only:
            value.published = True
            value.journal = await self._store.transition_file_mutation_journal(
                journal.id,
                expected_state=journal.state,
                new_state="published",
                expected_row_revision=journal.row_revision,
                updated_at=self._clock(),
            )
            return
        await asyncio.to_thread(os.replace, value.temporary, value.destination)
        value.published = True
        value.journal = await self._store.transition_file_mutation_journal(
            journal.id,
            expected_state=journal.state,
            new_state="published",
            expected_row_revision=journal.row_revision,
            updated_at=self._clock(),
        )
        if value.catalog_mutation is not None:
            published_stat = await asyncio.to_thread(value.destination.stat)
            value.catalog_mutation = msgspec.structs.replace(
                value.catalog_mutation,
                file_size_bytes=published_stat.st_size,
                file_mtime_ns=published_stat.st_mtime_ns,
                stat_revision=revision_from_stat(published_stat),
            )

    async def _rollback(self, prepared: list[_PreparedMutation]) -> None:
        for value in reversed(prepared):
            journal = value.journal
            if journal.state in {
                "published",
                "source_backed_up",
                "validated",
                "staged",
                "snapshot_saved",
                "planned",
            }:
                try:
                    journal = await self._store.transition_file_mutation_journal(
                        journal.id,
                        expected_state=journal.state,
                        new_state="rollback_pending",
                        expected_row_revision=journal.row_revision,
                        updated_at=self._clock(),
                    )
                    await asyncio.to_thread(self._restore_prepared_filesystem, value)
                    value.journal = await self._store.transition_file_mutation_journal(
                        journal.id,
                        expected_state="rollback_pending",
                        new_state="rolled_back",
                        expected_row_revision=journal.row_revision,
                        updated_at=self._clock(),
                    )
                except (OSError, ConflictError, StaleRevisionError, ValidationError):
                    try:
                        await self._store.transition_file_mutation_journal(
                            journal.id,
                            expected_state=journal.state,
                            new_state="needs_attention",
                            expected_row_revision=journal.row_revision,
                            updated_at=self._clock(),
                            failure_code="RECOVERY_NEEDS_ATTENTION",
                        )
                    except (StaleRevisionError, ValidationError):
                        pass

    async def _cleanup_committed(self, prepared: list[_PreparedMutation]) -> None:
        for value in prepared:
            journal = value.journal
            try:
                await asyncio.to_thread(self._cleanup_committed_filesystem, value)
                value.journal = await self._store.transition_file_mutation_journal(
                    journal.id,
                    expected_state="catalog_committed",
                    new_state="completed",
                    expected_row_revision=journal.row_revision + 1,
                    updated_at=self._clock(),
                )
            except (OSError, ConflictError, StaleRevisionError):
                value.journal = await self._store.transition_file_mutation_journal(
                    journal.id,
                    expected_state="catalog_committed",
                    new_state="cleanup_pending",
                    expected_row_revision=journal.row_revision + 1,
                    updated_at=self._clock(),
                    failure_code="SOURCE_CLEANUP_FAILED",
                )

    @classmethod
    def _restore_prepared_filesystem(cls, value: _PreparedMutation) -> None:
        if value.published and value.destination.exists():
            if (
                value.destination.is_symlink()
                or not value.destination.is_file()
                or value.journal.staged_fingerprint is None
                or cls._hash_file(value.destination) != value.journal.staged_fingerprint
            ):
                raise ConflictError(
                    "A published management destination changed during rollback."
                )
            value.destination.unlink()
        if (
            value.source_backed_up
            and value.backup is not None
            and value.source is not None
        ):
            os.replace(value.backup, value.source)
        value.temporary.unlink(missing_ok=True)

    def _cleanup_committed_filesystem(self, value: _PreparedMutation) -> None:
        if value.source_backed_up and value.backup is not None:
            if self._hash_file(value.backup) != value.source_fingerprint:
                raise ConflictError("A management backup changed before cleanup.")
            value.backup.unlink()
        elif (
            value.remove_source
            and value.source is not None
            and value.source != value.destination
        ):
            if self._hash_file(value.source) != value.source_fingerprint:
                raise ConflictError("A management source changed before cleanup.")
            value.source.unlink()
        if value.temporary != value.destination:
            value.temporary.unlink(missing_ok=True)

    @staticmethod
    def _remove_empty_source_directories(
        prepared: list[_PreparedMutation], roots: dict[str, Path]
    ) -> None:
        candidates: set[tuple[Path, Path]] = set()
        for value in prepared:
            root_id = value.journal.source_root_id
            if (
                not value.remove_source
                or value.source is None
                or value.source == value.destination
                or root_id is None
                or root_id not in roots
            ):
                continue
            candidates.add((value.source.parent, roots[root_id]))
        for directory, root in sorted(
            candidates, key=lambda value: len(value[0].parts), reverse=True
        ):
            current = directory
            while current != root and root in current.parents:
                try:
                    current.rmdir()
                except OSError:
                    break
                current = current.parent

    @staticmethod
    def _journal_id(job_id: str, ordinal: int, kind: str, key: str) -> str:
        return str(uuid.uuid5(_JOURNAL_NAMESPACE, f"{job_id}:{ordinal}:{kind}:{key}"))

    @staticmethod
    def _artifact_path(
        subject: Path, job_id: str, ordinal: int, kind: str, suffix: str
    ) -> Path:
        token = hashlib.sha256(f"{job_id}:{ordinal}:{kind}".encode()).hexdigest()[:16]
        return subject.parent / f"{MANAGEMENT_ARTIFACT_PREFIX}{token}-{kind}{suffix}"

    @staticmethod
    def _safe_path(root: Path, relative: str, *, create_parent: bool = False) -> Path:
        pure = PurePosixPath(relative)
        if (
            pure.is_absolute()
            or not pure.parts
            or any(part in {"", ".", ".."} for part in pure.parts)
        ):
            raise ValidationError("A management path is not a safe relative path.")
        current = root
        if stat.S_ISLNK(current.lstat().st_mode):
            raise ValidationError("A library root cannot be a symlink.")
        for part in pure.parts[:-1]:
            current = current / part
            try:
                metadata = current.lstat()
            except FileNotFoundError:
                if not create_parent:
                    raise
                current.mkdir()
                metadata = current.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise ValidationError("A management path contains a symlink.")
        return root.joinpath(*pure.parts)

    @staticmethod
    def _safe_child(
        parent: Path, relative: str, *, create_parent: bool = False
    ) -> Path:
        pure = PurePosixPath(relative)
        if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
            raise ValidationError("A sidecar path is unsafe.")
        current = parent
        for part in pure.parts[:-1]:
            current = current / part
            try:
                metadata = current.lstat()
            except FileNotFoundError:
                if not create_parent:
                    raise
                current.mkdir()
                metadata = current.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise ValidationError("A sidecar path contains a symlink.")
        return parent.joinpath(*pure.parts)

    def _stage_audio(self, source: Path, temporary: Path, plan) -> None:
        self._copy_temp(source, temporary)
        self._audio.apply(temporary, plan)

    def _stage_restore(
        self, source: Path, temporary: Path, snapshot: SemanticTagSnapshot
    ) -> None:
        self._copy_temp(source, temporary)
        self._audio.restore(temporary, snapshot)

    @staticmethod
    def _copy_temp(source: Path, temporary: Path) -> None:
        temporary.parent.mkdir(parents=True, exist_ok=True)
        temporary.unlink(missing_ok=True)
        shutil.copyfile(source, temporary)
        shutil.copystat(source, temporary)
        with temporary.open("rb") as handle:
            os.fsync(handle.fileno())

    @staticmethod
    def _write_temp_bytes(temporary: Path, content: bytes) -> None:
        temporary.parent.mkdir(parents=True, exist_ok=True)
        temporary.unlink(missing_ok=True)
        with temporary.open("xb") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())

    @staticmethod
    def _hash_file(path: Path) -> str:
        digest = hashlib.sha256()
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                raise OSError("The management path is not a regular file.")
            while chunk := handle.read(1024 * 1024):
                digest.update(chunk)
        return digest.hexdigest()

    @staticmethod
    def _fsync_directories(prepared: list[_PreparedMutation]) -> None:
        directories = {
            path.parent
            for value in prepared
            for path in (value.destination, value.temporary, value.backup)
            if path is not None
        }
        for directory in directories:
            try:
                descriptor = os.open(directory, os.O_RDONLY)
                try:
                    os.fsync(descriptor)
                finally:
                    os.close(descriptor)
            except OSError:
                continue

    @staticmethod
    def _remove_unpublished_temporaries(prepared: list[_PreparedMutation]) -> None:
        for value in prepared:
            if not value.published:
                value.temporary.unlink(missing_ok=True)
