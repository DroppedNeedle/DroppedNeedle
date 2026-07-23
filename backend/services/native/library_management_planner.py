"""Durable, read-only planning for immutable Library Management previews."""

from __future__ import annotations

import asyncio
import base64
from collections.abc import Callable, Sequence
from dataclasses import dataclass
import fnmatch
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import secrets
import shutil
import stat
import time
import unicodedata
import uuid

import msgspec

from api.v1.schemas.library_management import (
    LibraryManagementProfile,
    LibraryManagementSettings,
    NamingScriptSettings,
    TaggingScriptSettings,
    profile_revision,
    settings_revision,
)
from core.exceptions import (
    AudioFormatError,
    ConfigurationError,
    ExternalServiceError,
    ProviderIdentityRequiredError,
    RateLimitedError,
    ResourceNotFoundError,
    ScriptValidationError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.audio.metadata_engine import (
    AUDIO_EXTENSION_FORMATS,
    AudioMetadataEngine,
)
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import (
    MANAGEMENT_PERSISTENCE_BATCH_SIZE,
    NativeLibraryStore,
)
from infrastructure.queue.priority_queue import RequestPriority
from models.audio_metadata import (
    AudioMetadataDocument,
    AudioSemanticField,
    DesiredAudioDocument,
    DesiredAudioField,
    EmbeddedArtworkDescriptor,
    ReadAudioDocument,
)
from models.library_management import (
    FIELD_UNSUPPORTED_BY_FORMAT,
    FILE_CHANGED,
    FILE_UNREADABLE,
    FORMAT_UNSUPPORTED,
    IDENTITY_NOT_ACCEPTED,
    INSUFFICIENT_SPACE,
    METADATA_UNAVAILABLE,
    OPTIONAL_ENRICHMENT_DEFERRED,
    OUT_OF_ROOT,
    PATH_COLLISION_DIFFERENT,
    PATH_COLLISION_IDENTICAL,
    PATH_TOO_LONG,
    RELEASE_NOT_SELECTED,
    ROOT_READ_ONLY,
    ROOT_UNAVAILABLE,
    SIDECAR_COLLISION,
    SYMLINK_UNSUPPORTED,
    TRACK_NOT_MAPPED,
    LibraryManagementBlobReference,
    LibraryManagementJobSnapshot,
    LibraryManagementPlanItem,
    LibraryManagementTagEditIntent,
    ManagementOrigin,
)
from models.library_management_artwork import ExistingArtworkDescriptor, ArtworkOutput
from models.library_management_enrichment import (
    LyricsProjection,
    ReplayGainAnalysis,
    ReplayGainTrackResult,
)
from models.library_management_planning import (
    LibraryManagementPreviewHandle,
    LibraryManagementRootScope,
    LibraryManagementSelection,
    LibraryManagementSelectionCursor,
    LibraryManagementSelectionSubject,
    NormalizedLibraryManagementSelection,
    PinnedLibraryManagementProfile,
)
from models.library_work import OperationJob
from services.native.artwork_projection_service import ArtworkProjectionService
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.canonical_release_metadata_service import (
    CanonicalReleaseMetadataService,
)
from services.native.effective_metadata_projection_service import (
    EffectiveMetadataProjectionService,
)
from services.native.genre_projection_service import GenreProjectionService
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.lyrics_projection_service import LyricsProjectionService
from services.native.managed_field_registry import canonical_track_values
from services.native.naming import NamingTemplateEngine
from services.native.replaygain_analysis_service import ReplayGainAnalysisService
from services.native.tagging_scripts import TaggingScriptEngine
from services.preferences_service import PreferencesService

MAX_EXPLICIT_SELECTION_IDS = 10_000
MAX_SIDECAR_ENTRIES = 10_000
DISK_SAFETY_BYTES = 64 * 1024 * 1024
_PREVIEW_NAMESPACE = uuid.UUID("8e6fd30e-412f-50c0-9e82-3019dc602f70")


def _json(value: object) -> str:
    return json.dumps(
        msgspec.to_builtins(value),
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )


def _sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _preview_token(job_id: str, idempotency_key: str | None) -> str:
    material = (
        f"{job_id}\x00{idempotency_key}".encode("utf-8")
        if idempotency_key is not None
        else secrets.token_bytes(32)
    )
    digest = hashlib.sha256(material).digest()
    return base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")


def _path_collision_key(value: str) -> str:
    return unicodedata.normalize("NFC", value).casefold()


@dataclass(frozen=True, slots=True)
class _SourceInspection:
    subject: LibraryManagementSelectionSubject
    path: Path | None
    document: ReadAudioDocument | None
    fingerprint: str
    reason_code: str | None


class LibraryManagementPlanner:
    def __init__(
        self,
        store: NativeLibraryStore,
        preferences: PreferencesService,
        canonical: CanonicalReleaseMetadataService,
        effective: EffectiveMetadataProjectionService,
        genres: GenreProjectionService,
        artwork: ArtworkProjectionService,
        audio: AudioMetadataEngine,
        write_planner: AudioWritePlanningService,
        naming: NamingTemplateEngine,
        tagging: TaggingScriptEngine,
        blobs: LibraryManagementBlobStore,
        workload_gate: BackgroundWorkloadGate | None = None,
        lyrics: LyricsProjectionService | None = None,
        replaygain: ReplayGainAnalysisService | None = None,
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._preferences = preferences
        self._canonical = canonical
        self._effective = effective
        self._genres = genres
        self._artwork = artwork
        self._audio = audio
        self._write_planner = write_planner
        self._naming = naming
        self._tagging = tagging
        self._blobs = blobs
        self._workload_gate = workload_gate
        self._lyrics = lyrics
        self._replaygain = replaygain
        self._clock = clock

    async def create_preview(
        self,
        *,
        selection: LibraryManagementSelection,
        profile_id: str,
        expected_settings_revision: str,
        expected_policy_revision: str,
        actor_user_id: str | None,
        idempotency_key: str | None,
        target_root_id: str | None = None,
        origin: ManagementOrigin = "manual",
        settings_snapshot: LibraryManagementSettings | None = None,
        effective_profile: LibraryManagementProfile | None = None,
        tag_edit_intent: LibraryManagementTagEditIntent | None = None,
        force_expand_album_bundles: bool = False,
    ) -> LibraryManagementPreviewHandle:
        current_settings = self._preferences.get_library_management_settings_raw()
        current_settings_revision = settings_revision(current_settings)
        if current_settings_revision != expected_settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed before preview."
            )
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        if resolver.policy_revision != expected_policy_revision:
            raise StaleRevisionError("Library policy changed before preview.")
        settings = settings_snapshot or current_settings
        profile = effective_profile or self._find_profile(settings, profile_id)
        if profile.id != profile_id:
            raise ConfigurationError(
                "The effective Library Management profile does not match the request."
            )
        normalized = self.normalize_selection(selection, profile, resolver)
        if force_expand_album_bundles:
            normalized = msgspec.structs.replace(normalized, expand_album_bundles=True)
        roots = {root.id: root for root in resolver.settings.library_roots}
        if target_root_id is not None:
            target = roots.get(target_root_id)
            if target is None:
                raise ConfigurationError("The destination root is not configured.")
            if not profile.organization.move_enabled:
                raise ConfigurationError(
                    "Cross-root organization requires file moving to be enabled."
                )
            if selection.kind == "roots" and any(
                scope.root_id == target_root_id for scope in normalized.root_scopes
            ):
                raise ConfigurationError(
                    "A cross-root destination must differ from every selected root."
                )
        pinned = self.pin_profile(settings, profile)
        naming_script = pinned.naming_script
        now = self._clock()
        preview_ttl_seconds = settings.preview_retention_hours * 60 * 60
        job_id = (
            str(uuid.uuid5(_PREVIEW_NAMESPACE, idempotency_key))
            if idempotency_key is not None
            else str(uuid.uuid4())
        )
        token = _preview_token(job_id, idempotency_key)
        token_hash = _sha256_text(token)
        catalog_revision = await self._store.get_catalog_revision()
        snapshot = LibraryManagementJobSnapshot(
            job_id=job_id,
            mode="preview",
            origin=origin,
            phase="planning",
            selection_json=_json(normalized),
            profile_revision=profile_revision(profile),
            settings_revision=current_settings_revision,
            proposed_settings_revision=(
                settings_revision(settings) if settings_snapshot is not None else None
            ),
            naming_revision=naming_script.revision,
            policy_revision=resolver.policy_revision,
            catalog_revision=catalog_revision,
            profile_snapshot_json=_json(pinned),
            preview_token_hash=token_hash,
            preview_created_at=now,
            preview_expires_at=now + preview_ttl_seconds,
            target_root_id=target_root_id,
            intent_json=_json(tag_edit_intent) if tag_edit_intent is not None else "{}",
            summary_json=_json(
                {
                    "expanded_track_count": normalized.expanded_track_count,
                }
            ),
            created_at=now,
            updated_at=now,
        )
        existing_id, created = await self._store.create_library_management_job(
            OperationJob(
                id=job_id,
                kind="library_management",
                requested_by_user_id=actor_user_id,
                input_catalog_revision=catalog_revision,
                idempotency_key=idempotency_key,
                created_at=now,
            ),
            snapshot,
        )
        if not created:
            existing = await self._store.get_library_management_job_snapshot(
                existing_id
            )
            operation = await self._store.get_operation_job(existing_id)
            request_matches = (
                existing is not None
                and operation is not None
                and existing.preview_token_hash == token_hash
                and existing.mode == snapshot.mode
                and existing.origin == snapshot.origin
                and existing.selection_json == snapshot.selection_json
                and existing.profile_revision == snapshot.profile_revision
                and existing.settings_revision == snapshot.settings_revision
                and existing.proposed_settings_revision
                == snapshot.proposed_settings_revision
                and existing.naming_revision == snapshot.naming_revision
                and existing.policy_revision == snapshot.policy_revision
                and existing.profile_snapshot_json == snapshot.profile_snapshot_json
                and existing.target_root_id == snapshot.target_root_id
                and existing.intent_json == snapshot.intent_json
                and operation["requested_by_user_id"] == actor_user_id
            )
            if not request_matches:
                raise StaleRevisionError(
                    "The idempotency key belongs to a different preview request."
                )
            assert existing is not None
            return LibraryManagementPreviewHandle(
                job_id=existing_id,
                preview_token=token,
                created_at=existing.preview_created_at or existing.created_at,
                expires_at=existing.preview_expires_at or existing.created_at,
                existing=True,
            )
        return LibraryManagementPreviewHandle(
            job_id=job_id,
            preview_token=token,
            created_at=now,
            expires_at=now + preview_ttl_seconds,
        )

    @classmethod
    def pin_profile(
        cls,
        settings: LibraryManagementSettings,
        profile: LibraryManagementProfile,
    ) -> PinnedLibraryManagementProfile:
        return PinnedLibraryManagementProfile(
            profile=profile,
            naming_script=cls._find_naming_script(
                settings, profile.organization.naming_script_id
            ),
            external_artwork_naming_script=(
                cls._find_naming_script(
                    settings, profile.artwork.external_naming_script_id
                )
                if profile.artwork.external_naming_script_id is not None
                else None
            ),
            tagging_scripts=tuple(
                cls._find_tagging_script(settings, script_id)
                for script_id in profile.metadata.tagging_script_ids
            ),
            recycle_bin_path=settings.recycle_bin_path,
        )

    @staticmethod
    def normalize_selection(
        selection: LibraryManagementSelection,
        profile: LibraryManagementProfile,
        resolver: LibraryPolicyResolver,
    ) -> NormalizedLibraryManagementSelection:
        ids = tuple(
            dict.fromkeys(value.strip() for value in selection.ids if value.strip())
        )
        if len(ids) > MAX_EXPLICIT_SELECTION_IDS:
            raise ValidationError(
                "Use a catalog filter for selections larger than 10,000 subjects."
            )
        if any(len(value) > 255 or "\x00" in value for value in ids):
            raise ValidationError("A management selection identifier is invalid.")
        if selection.kind == "filter":
            if ids or selection.catalog_filter is None:
                raise ValidationError(
                    "A filter selection requires one immutable catalog filter and no IDs."
                )
            value = selection.catalog_filter
            if len(value.artist_ids) > MAX_EXPLICIT_SELECTION_IDS:
                raise ValidationError(
                    "A catalog filter cannot contain more than 10,000 artist IDs."
                )
            if any(
                len(identifier) > 255 or "\x00" in identifier
                for identifier in value.artist_ids
            ):
                raise ValidationError("A catalog filter artist identifier is invalid.")
            if value.search is not None and len(value.search) > 500:
                raise ValidationError("A catalog filter search is too long.")
            if value.genre is not None and len(value.genre) > 255:
                raise ValidationError("A catalog filter genre is too long.")
            if (
                value.from_year is not None
                and value.to_year is not None
                and value.from_year > value.to_year
            ):
                raise ValidationError("The catalog year range is invalid.")
        elif not ids or selection.catalog_filter is not None:
            raise ValidationError(
                "A management selection requires IDs or one catalog filter, not both."
            )

        root_scopes: list[LibraryManagementRootScope] = []
        if selection.kind == "roots":
            roots = {root.id: root for root in resolver.settings.library_roots}
            rules = {
                rule.id: (root.id, rule.relative_path)
                for root in resolver.settings.library_roots
                for rule in root.rules
            }
            for scope_id in ids:
                if scope_id in roots:
                    root_scopes.append(LibraryManagementRootScope(root_id=scope_id))
                elif scope_id in rules:
                    root_id, prefix = rules[scope_id]
                    root_scopes.append(
                        LibraryManagementRootScope(
                            root_id=root_id,
                            relative_prefix=prefix,
                        )
                    )
                else:
                    raise ConfigurationError(
                        "A selected library root or rule is no longer configured."
                    )
        expand = bool(
            profile.organization.rename_enabled
            or profile.organization.move_enabled
            or profile.organization.move_sidecars
            or (
                profile.enrichment.replaygain.enabled
                and profile.enrichment.replaygain.album_aware
                and profile.enrichment.replaygain.mode != "preserve"
            )
        )
        return NormalizedLibraryManagementSelection(
            kind=selection.kind,
            ids=ids,
            root_scopes=tuple(root_scopes),
            catalog_filter=selection.catalog_filter,
            expand_album_bundles=expand,
            requested_track_ids=ids if selection.kind == "tracks" else (),
        )

    async def run_claimed_preview(
        self,
        job: dict,
        worker_id: str,
    ) -> LibraryManagementJobSnapshot:
        job_id = str(job["id"])
        snapshot = await self._store.get_library_management_job_snapshot(job_id)
        if snapshot is None or snapshot.mode != "preview":
            raise ValidationError("The claimed management operation is not a preview.")
        pinned = msgspec.json.decode(
            snapshot.profile_snapshot_json.encode("utf-8"),
            type=PinnedLibraryManagementProfile,
        )
        selection = msgspec.json.decode(
            snapshot.selection_json.encode("utf-8"),
            type=NormalizedLibraryManagementSelection,
        )
        tag_edit_intent = (
            msgspec.json.decode(
                snapshot.intent_json.encode("utf-8"),
                type=LibraryManagementTagEditIntent,
            )
            if snapshot.intent_json != "{}"
            else None
        )
        cursor = (
            msgspec.json.decode(
                snapshot.staging_cursor.encode("utf-8"),
                type=LibraryManagementSelectionCursor,
            )
            if snapshot.staging_cursor and snapshot.staging_cursor.startswith("{")
            else None
        )
        roots = await self._current_roots(snapshot)
        snapshot_revision = snapshot.row_revision
        while True:
            if self._workload_gate is not None and self._workload_gate.scan_active:
                await self._store.defer_library_management_preview_for_scan(
                    job_id, worker_id, now=self._clock()
                )
                refreshed = await self._store.get_library_management_job_snapshot(
                    job_id
                )
                if refreshed is None:
                    raise ResourceNotFoundError("Library management job not found.")
                return refreshed
            controlled = await self._store.checkpoint_operation_control(
                job_id, worker_id, now=self._clock()
            )
            if controlled is not None and controlled["state"] != "running":
                refreshed = await self._store.get_library_management_job_snapshot(
                    job_id
                )
                if refreshed is None:
                    raise ResourceNotFoundError("Library management job not found.")
                return refreshed
            await self._validate_pinned_revisions(snapshot)
            page = await self._store.list_library_management_selection_page(
                selection,
                cursor=cursor,
                limit=MANAGEMENT_PERSISTENCE_BATCH_SIZE,
            )
            if not page.subjects:
                return await self._seal_preview(
                    snapshot,
                    worker_id,
                    expected_snapshot_revision=snapshot_revision,
                    roots=roots,
                )
            items: list[LibraryManagementPlanItem] = []
            metadata_snapshot_ids: list[str] = []
            groups: dict[str, list[LibraryManagementSelectionSubject]] = {}
            for subject in page.subjects:
                groups.setdefault(subject.local_album_id, []).append(subject)
            for subjects in groups.values():
                renewed = await self._store.heartbeat_operation_job(
                    job_id,
                    worker_id,
                    now=self._clock(),
                    lease_seconds=60.0,
                )
                if not renewed:
                    raise StaleRevisionError(
                        "The management preview lease changed during planning."
                    )
                planned, pinned_ids = await self._plan_album_page(
                    snapshot,
                    pinned,
                    tuple(subjects),
                    roots,
                    tag_edit_intent=tag_edit_intent,
                )
                items.extend(planned)
                metadata_snapshot_ids.extend(pinned_ids)
                renewed = await self._store.heartbeat_operation_job(
                    job_id,
                    worker_id,
                    now=self._clock(),
                    lease_seconds=60.0,
                )
                if not renewed:
                    raise StaleRevisionError(
                        "The management preview lease changed during planning."
                    )
            if metadata_snapshot_ids:
                await self._store.pin_library_management_metadata_snapshots(
                    job_id, metadata_snapshot_ids
                )
            cursor = page.next_cursor
            cursor_json = _json(cursor) if cursor is not None else None
            snapshot_revision = await self._store.append_library_management_plan_items(
                job_id,
                items,
                expected_snapshot_revision=snapshot_revision,
                staging_cursor=cursor_json,
            )
            if page.complete:
                return await self._seal_preview(
                    snapshot,
                    worker_id,
                    expected_snapshot_revision=snapshot_revision,
                    roots=roots,
                )

    async def _current_roots(
        self, snapshot: LibraryManagementJobSnapshot
    ) -> dict[str, object]:
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        if resolver.policy_revision != snapshot.policy_revision:
            raise StaleRevisionError("Library policy changed during preview.")
        return {root.id: root for root in resolver.settings.library_roots}

    async def _validate_pinned_revisions(
        self, snapshot: LibraryManagementJobSnapshot
    ) -> None:
        settings = self._preferences.get_library_management_settings_raw()
        if settings_revision(settings) != snapshot.settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed during preview."
            )
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        if resolver.policy_revision != snapshot.policy_revision:
            raise StaleRevisionError("Library policy changed during preview.")

    async def _plan_album_page(
        self,
        snapshot: LibraryManagementJobSnapshot,
        pinned: PinnedLibraryManagementProfile,
        subjects: tuple[LibraryManagementSelectionSubject, ...],
        roots: dict[str, object],
        *,
        tag_edit_intent: LibraryManagementTagEditIntent | None = None,
    ) -> tuple[list[LibraryManagementPlanItem], list[str]]:
        if (
            tag_edit_intent is not None
            and tag_edit_intent.local_album_id != subjects[0].local_album_id
        ):
            raise ValidationError("The tag edit selection changed before planning.")
        inspected = [
            await asyncio.to_thread(self._inspect_source, subject, roots)
            for subject in subjects
        ]
        readable = [value for value in inspected if value.document is not None]
        if not readable:
            return (
                [
                    self._blocked_item(
                        snapshot, value, value.reason_code or FILE_UNREADABLE
                    )
                    for value in inspected
                ],
                [],
            )
        identity = await self._store.get_accepted_library_management_identity(
            subjects[0].local_album_id,
            local_track_ids=tuple(value.subject.local_track_id for value in readable),
        )
        identity_reason = self._identity_reason(identity, readable)
        if identity_reason is not None:
            return (
                [
                    self._blocked_item(
                        snapshot,
                        value,
                        value.reason_code or identity_reason,
                    )
                    for value in inspected
                ],
                [],
            )
        try:
            canonical = await self._canonical.build(
                local_album_id=subjects[0].local_album_id,
                profile=pinned.profile,
                local_track_ids=tuple(
                    value.subject.local_track_id for value in readable
                ),
                priority=RequestPriority.BACKGROUND_SYNC,
            )
        except ProviderIdentityRequiredError:
            return (
                [
                    self._blocked_item(
                        snapshot,
                        value,
                        value.reason_code or TRACK_NOT_MAPPED,
                    )
                    for value in inspected
                ],
                [],
            )
        except (ExternalServiceError, RateLimitedError):
            return (
                [
                    self._blocked_item(
                        snapshot,
                        value,
                        value.reason_code or METADATA_UNAVAILABLE,
                    )
                    for value in inspected
                ],
                [],
            )
        canonical_tracks = {
            track.local_track_id: track
            for medium in canonical.document.media
            for track in medium.tracks
        }
        replaygain_analysis: ReplayGainAnalysis | None = None
        replaygain_by_path: dict[str, ReplayGainTrackResult] = {}
        replaygain_settings = pinned.profile.enrichment.replaygain
        if replaygain_settings.enabled and replaygain_settings.mode != "preserve":
            if len(readable) != len(inspected):
                replaygain_analysis = ReplayGainAnalysis(
                    status="deferred",
                    reason="The complete album is not readable for ReplayGain analysis.",
                )
            elif self._replaygain is None:
                replaygain_analysis = ReplayGainAnalysis(
                    status="deferred",
                    reason="The ReplayGain analyzer is unavailable.",
                )
            else:
                replaygain_analysis = await self._replaygain.analyze(
                    tuple(value.path for value in readable if value.path is not None),
                    album_aware=replaygain_settings.album_aware,
                )
            if replaygain_analysis.status == "available":
                replaygain_by_path = {
                    value.source_path: value for value in replaygain_analysis.tracks
                }
            elif replaygain_settings.required:
                return (
                    [
                        self._blocked_item(
                            snapshot,
                            value,
                            value.reason_code or METADATA_UNAVAILABLE,
                        )
                        for value in inspected
                    ],
                    [canonical.metadata_snapshot_id],
                )
        (
            album_overrides,
            album_override_revision,
        ) = await self._store.list_management_overrides(
            subject_kind="album", subject_id=subjects[0].local_album_id
        )
        planned: list[LibraryManagementPlanItem] = []
        for value in inspected:
            if value.document is None or value.path is None:
                planned.append(
                    self._blocked_item(
                        snapshot, value, value.reason_code or FILE_UNREADABLE
                    )
                )
                continue
            track = canonical_tracks.get(value.subject.local_track_id)
            if track is None:
                planned.append(self._blocked_item(snapshot, value, TRACK_NOT_MAPPED))
                continue
            (
                track_overrides,
                track_override_revision,
            ) = await self._store.list_management_overrides(
                subject_kind="track", subject_id=value.subject.local_track_id
            )
            try:
                item = await self._plan_track(
                    snapshot=snapshot,
                    pinned=pinned,
                    source=value,
                    canonical_release=canonical.document,
                    canonical_track=track,
                    album_overrides=album_overrides,
                    track_overrides=track_overrides,
                    override_revision=_sha256_text(
                        f"{album_override_revision}\x00{track_override_revision}"
                    ),
                    identity=identity,
                    roots=roots,
                    replaygain_result=(
                        replaygain_by_path.get(str(value.path))
                        if value.path is not None
                        else None
                    ),
                    replaygain_analysis=replaygain_analysis,
                    tag_edit_intent=tag_edit_intent,
                )
            except ScriptValidationError:
                item = self._blocked_item(snapshot, value, PATH_TOO_LONG)
            except AudioFormatError:
                item = self._blocked_item(snapshot, value, FORMAT_UNSUPPORTED)
            except ValidationError:
                item = self._blocked_item(snapshot, value, FIELD_UNSUPPORTED_BY_FORMAT)
            planned.append(item)
        return planned, [canonical.metadata_snapshot_id]

    async def _plan_track(
        self,
        *,
        snapshot: LibraryManagementJobSnapshot,
        pinned: PinnedLibraryManagementProfile,
        source: _SourceInspection,
        canonical_release,
        canonical_track,
        album_overrides,
        track_overrides,
        override_revision: str,
        identity,
        roots: dict[str, object],
        replaygain_result: ReplayGainTrackResult | None = None,
        replaygain_analysis: ReplayGainAnalysis | None = None,
        tag_edit_intent: LibraryManagementTagEditIntent | None = None,
    ) -> LibraryManagementPlanItem:
        assert source.document is not None and source.path is not None
        profile = pinned.profile
        existing = {
            field.name: field.value for field in source.document.metadata.fields
        }
        canonical_values = canonical_track_values(canonical_release, canonical_track)
        genre_projection = await self._genres.project(
            settings=profile.genres,
            canonical_release=canonical_release,
            existing_genres=source.document.metadata.strings_for("genre"),
        )
        if profile.enrichment.lyrics.enabled and self._lyrics is not None:
            lyrics_projection = await self._lyrics.project(
                settings=profile.enrichment.lyrics,
                canonical_release=canonical_release,
                canonical_track=canonical_track,
                duration_seconds=source.document.technical.duration_seconds,
            )
        elif profile.enrichment.lyrics.enabled:
            lyrics_projection = LyricsProjection(
                status="deferred",
                reason="The lyrics provider is not available.",
            )
        else:
            lyrics_projection = LyricsProjection(status="disabled")
        selected_lyrics = tuple(
            value
            for enabled, value in (
                (
                    profile.enrichment.lyrics.write_plain,
                    lyrics_projection.plain_lyrics,
                ),
                (
                    profile.enrichment.lyrics.write_synced,
                    lyrics_projection.synced_lyrics,
                ),
            )
            if enabled and value
        )
        if (
            profile.enrichment.lyrics.enabled
            and profile.enrichment.lyrics.required
            and (lyrics_projection.status != "available" or not selected_lyrics)
        ):
            return self._blocked_item(snapshot, source, METADATA_UNAVAILABLE)
        replaygain_settings = profile.enrichment.replaygain
        replaygain_values: tuple[tuple[str, float | None], ...] = (
            (
                "replaygain_track_gain",
                replaygain_result.track_gain_db if replaygain_result else None,
            ),
            (
                "replaygain_track_peak",
                replaygain_result.track_peak if replaygain_result else None,
            ),
            (
                "replaygain_album_gain",
                replaygain_result.album_gain_db if replaygain_result else None,
            ),
            (
                "replaygain_album_peak",
                replaygain_result.album_peak if replaygain_result else None,
            ),
        )
        required_replaygain_names = {
            "replaygain_track_gain",
            "replaygain_track_peak",
            *(
                ("replaygain_album_gain", "replaygain_album_peak")
                if replaygain_settings.album_aware
                else ()
            ),
        }
        if replaygain_settings.enabled and replaygain_settings.required:
            available_replaygain = {
                name for name, value in replaygain_values if value is not None
            } | {
                name
                for name in required_replaygain_names
                if isinstance(existing.get(name), float)
            }
            if not required_replaygain_names <= available_replaygain:
                return self._blocked_item(snapshot, source, METADATA_UNAVAILABLE)
        enriched = {
            "genre": tuple(value.display_name for value in genre_projection.genres)
        }
        active_tag_edits = tuple(
            value
            for value in (tag_edit_intent.fields if tag_edit_intent is not None else ())
            if value.subject_kind == "album"
            or source.subject.local_track_id == tag_edit_intent.local_track_id
        )
        reset_fields = {
            value.field_name
            for value in active_tag_edits
            if tag_edit_intent is not None and tag_edit_intent.mode == "reset_canonical"
        }
        effective_album_overrides = tuple(
            value for value in album_overrides if value.field_name not in reset_fields
        )
        effective_track_overrides = tuple(
            value for value in track_overrides if value.field_name not in reset_fields
        )
        manual_overrides = (
            {value.field_name: value.value for value in active_tag_edits}
            if tag_edit_intent is not None
            and tag_edit_intent.mode in {"save_override", "write_once"}
            else None
        )
        preliminary = self._effective.project(
            profile=profile,
            canonical_values=canonical_values,
            existing_values=existing,
            enriched_values=enriched,
            canonical_available=True,
        )
        preliminary_document = self._metadata_document(preliminary)
        current_custom = self._write_planner.custom_tags(
            current=source.document, profile=profile
        )
        protected = frozenset(
            value.field_name
            for value in (*effective_album_overrides, *effective_track_overrides)
        )
        transformed = self._tagging.apply(
            preliminary_document,
            pinned.tagging_scripts,
            custom_tags=current_custom,
            protected_fields=protected,
        )
        effective = self._effective.project(
            profile=profile,
            canonical_values=canonical_values,
            existing_values=existing,
            enriched_values=enriched,
            transformed_values=self._tagging.transformed_values(transformed),
            album_overrides=effective_album_overrides,
            track_overrides=effective_track_overrides,
            manual_overrides=manual_overrides,
            canonical_available=True,
        )
        desired_metadata = self._metadata_document(effective)
        artwork_settings = (
            profile.artwork
            if source.subject.bundle_first
            else msgspec.structs.replace(profile.artwork, external_enabled=False)
        )
        existing_external = (
            await self._artwork.inspect_existing_external(
                artwork_settings, source.path.parent
            )
            if source.subject.bundle_first
            else ()
        )
        artwork_projection = await self._artwork.project(
            settings=artwork_settings,
            release_mbid=canonical_release.identifiers.release_mbid,
            release_group_mbid=canonical_release.identifiers.release_group_mbid,
            album_directory=source.path.parent,
            existing_embedded=tuple(
                ExistingArtworkDescriptor(
                    image_type=value.image_type,
                    mime_type=value.mime_type or "application/octet-stream",
                    width=value.width,
                    height=value.height,
                    byte_size=value.byte_size,
                    sha256=value.sha256,
                )
                for value in source.document.artwork
            ),
            existing_external=existing_external,
            priority=RequestPriority.BACKGROUND_SYNC,
        )
        desired_artwork = (
            tuple(
                self._embedded_descriptor(value)
                for value in artwork_projection.embedded
            )
            if profile.artwork.embedded_enabled
            else None
        )
        desired_fields = list(
            self._desired_fields(
                profile,
                source.document.metadata,
                desired_metadata,
                enabled_names=(
                    {value.field_name for value in active_tag_edits}
                    if tag_edit_intent is not None
                    else None
                ),
            )
        )
        if lyrics_projection.status == "available":
            for name, enabled, value in (
                (
                    "lyrics_plain",
                    profile.enrichment.lyrics.write_plain,
                    lyrics_projection.plain_lyrics,
                ),
                (
                    "lyrics_synced",
                    profile.enrichment.lyrics.write_synced,
                    lyrics_projection.synced_lyrics,
                ),
            ):
                if enabled and value:
                    desired_fields.append(
                        DesiredAudioField(name=name, action="set", value=value)
                    )
        if replaygain_settings.enabled and replaygain_settings.mode != "preserve":
            for name, value in replaygain_values:
                if value is None or (
                    not replaygain_settings.album_aware
                    and name.startswith("replaygain_album_")
                ):
                    continue
                if replaygain_settings.mode == "fill_missing" and isinstance(
                    existing.get(name), float
                ):
                    continue
                desired_fields.append(
                    DesiredAudioField(name=name, action="set", value=value)
                )
        desired = DesiredAudioDocument(
            fields=tuple(desired_fields),
            custom_tags=self._tagging.desired_custom_tags(current_custom, transformed),
            artwork=desired_artwork,
            artist_display=desired_metadata.artist_display,
            album_artist_display=desired_metadata.album_artist_display,
        )
        write_plan = self._write_planner.plan(
            current=source.document,
            desired=desired,
            profile=profile,
        )
        destination_root_id = snapshot.target_root_id or source.subject.root_id
        destination_root = roots.get(destination_root_id)
        if destination_root is None:
            return self._blocked_item(snapshot, source, ROOT_UNAVAILABLE)
        destination_reason = await asyncio.to_thread(
            self._destination_root_reason, Path(destination_root.path)
        )
        if destination_reason is not None:
            return self._blocked_item(snapshot, source, destination_reason)
        destination_relative, collision_key = self._destination_relative(
            pinned.naming_script,
            profile,
            source,
            desired_metadata,
            Path(destination_root.path),
        )
        destination_path = Path(destination_root.path) / PurePosixPath(
            destination_relative
        )
        destination_path_reason = await asyncio.to_thread(
            self._destination_path_reason,
            Path(destination_root.path),
            destination_path,
        )
        catalog_collision = await self._store.get_target_track_by_path(
            str(destination_path)
        )
        catalog_directory = PurePosixPath(destination_relative).parent
        catalog_siblings = await self._store.list_target_tracks_in_directory(
            destination_root_id,
            ""
            if catalog_directory == PurePosixPath(".")
            else catalog_directory.as_posix(),
        )
        collisions, collision_reason = await asyncio.to_thread(
            self._destination_collisions,
            source.path,
            source.fingerprint,
            destination_path,
            Path(destination_root.path),
        )
        for collision in collisions:
            collision["existing_root_id"] = destination_root_id
        if destination_path_reason is not None:
            collisions.append({"classification": "unsafe_destination_parent"})
            collision_reason = destination_path_reason
        if (
            catalog_collision is not None
            and str(catalog_collision["id"]) != source.subject.local_track_id
        ):
            existing_evidence = next(
                (
                    value
                    for value in collisions
                    if value.get("classification")
                    in {"same_path_same_content", "same_path_different_content"}
                ),
                None,
            )
            catalog_evidence = {
                "classification": "same_path_different_content",
                "existing_root_id": destination_root_id,
                "existing_relative_path": destination_relative,
                "existing_local_track_id": str(catalog_collision["id"]),
            }
            if existing_evidence is None:
                collisions.append(catalog_evidence)
            else:
                existing_evidence.update(catalog_evidence)
            collision_reason = PATH_COLLISION_DIFFERENT
        for sibling in catalog_siblings:
            if (
                str(sibling["id"]) != source.subject.local_track_id
                and _path_collision_key(str(sibling["relative_path"])) == collision_key
            ):
                collisions.append(
                    {
                        "classification": "normalized_catalog_path_collision",
                        "catalog_track_id": str(sibling["id"]),
                        "existing_root_id": destination_root_id,
                        "existing_relative_path": str(sibling["relative_path"]),
                    }
                )
                collision_reason = PATH_COLLISION_DIFFERENT
                break
        if len(catalog_siblings) > MAX_SIDECAR_ENTRIES:
            collisions.append({"classification": "catalog_directory_limit"})
            collision_reason = PATH_COLLISION_DIFFERENT
        sidecars, sidecar_reason = await asyncio.to_thread(
            self._sidecars,
            source.path.parent,
            destination_path.parent,
            profile,
            source.subject.bundle_first,
        )
        if sidecar_reason is not None:
            destination_parent = PurePosixPath(destination_relative).parent
            collisions.extend(
                {
                    "classification": "sidecar_path_collision",
                    "existing_root_id": destination_root_id,
                    "destination_relative_path": (
                        destination_parent / str(value["destination_relative_path"])
                    ).as_posix(),
                }
                for value in sidecars
                if value.get("destination_collision")
            )
        (
            external_artwork,
            artwork_collisions,
            external_artwork_reason,
        ) = await asyncio.to_thread(
            self._external_artwork,
            pinned,
            source,
            desired_metadata,
            Path(destination_root.path),
            destination_relative,
            artwork_projection.external,
        )
        collisions.extend(artwork_collisions)
        for collision in artwork_collisions:
            collision["existing_root_id"] = destination_root_id
        external_keys = {
            _path_collision_key(relative) for _, relative in external_artwork
        }
        destination_parent = PurePosixPath(destination_relative).parent
        if any(
            _path_collision_key(
                (destination_parent / value["destination_relative_path"]).as_posix()
            )
            in external_keys
            for value in sidecars
        ):
            sidecar_reason = SIDECAR_COLLISION
            collisions.append({"classification": "sidecar_external_artwork_collision"})
        artwork_choices = await self._store_artwork_choices(
            snapshot.job_id,
            source.subject.ordinal,
            (
                *((value, None) for value in artwork_projection.embedded),
                *external_artwork,
            ),
        )
        stored_desired = msgspec.structs.replace(desired, artwork=None)
        desired_json = _json(stored_desired)
        path_changed = (
            destination_root_id != source.subject.root_id
            or destination_relative != source.subject.relative_path
        )
        artwork_changed = bool(artwork_projection.embedded or external_artwork)
        sidecars_changed = bool(sidecars)
        requires_write = bool(
            write_plan.requires_write
            or path_changed
            or artwork_changed
            or sidecars_changed
        )
        blockers = list(write_plan.blockers)
        warnings = [*write_plan.warnings, *source.document.warnings]
        if any(
            PurePosixPath(value["source_relative_path"]).suffix.casefold()
            in {".cue", ".m3u", ".m3u8", ".pls"}
            for value in sidecars
        ):
            warnings.append("moved path-bearing sidecars are not rewritten")
        reason_code = source.reason_code
        eligibility = "eligible"
        if blockers:
            eligibility = "blocked"
            reason_code = FIELD_UNSUPPORTED_BY_FORMAT
        elif collision_reason is not None:
            eligibility = "blocked"
            reason_code = collision_reason
        elif sidecar_reason is not None or external_artwork_reason is not None:
            eligibility = "blocked"
            reason_code = sidecar_reason or external_artwork_reason
        elif (
            genre_projection.deferred_sources
            or artwork_projection.deferred_sources
            or lyrics_projection.status in {"deferred", "mismatch", "not_found"}
            or (
                replaygain_analysis is not None
                and replaygain_analysis.status == "deferred"
            )
        ):
            eligibility = "warning"
            reason_code = OPTIONAL_ENRICHMENT_DEFERRED
        elif warnings:
            eligibility = "warning"
        identity_track = next(
            value
            for value in identity.tracks
            if value.local_track_id == source.subject.local_track_id
        )
        estimated = (
            (
                source.subject.file_size_bytes
                if write_plan.requires_write or path_changed
                else 0
            )
            + sum(value.byte_size for value, _ in external_artwork)
            + sum(int(value["byte_size"]) for value in sidecars)
        )
        capability = {
            "audio_format": write_plan.audio_format,
            "adapter": write_plan.adapter_name,
            "blockers": blockers,
            "warnings": warnings,
            "representation_losses": [
                value.representation_loss
                for value in write_plan.mutations
                if value.representation_loss is not None
            ],
        }
        diff = {
            "requires_write": requires_write,
            "tags_changed": any(
                value.operation != "unchanged" for value in write_plan.mutations
            )
            or any(
                value.operation != "preserve"
                for value in write_plan.custom_tag_mutations
            ),
            "artwork_changed": artwork_changed,
            "path_changed": path_changed,
            "sidecars_changed": sidecars_changed,
            "field_mutations": msgspec.to_builtins(write_plan.mutations),
            "custom_tag_mutations": msgspec.to_builtins(
                write_plan.custom_tag_mutations
            ),
            "transformations": msgspec.to_builtins(transformed.transformations),
            "artwork_decisions": msgspec.to_builtins(artwork_projection.decisions),
            "lyrics_projection": {
                "status": lyrics_projection.status,
                "provider_id": lyrics_projection.provider_id,
                "provider_revision": lyrics_projection.provider_revision,
                "reason": lyrics_projection.reason,
                "plain_available": lyrics_projection.plain_lyrics is not None,
                "synced_available": lyrics_projection.synced_lyrics is not None,
            },
            "replaygain_analysis": (
                {
                    "status": replaygain_analysis.status,
                    "analyzer": replaygain_analysis.analyzer,
                    "analyzer_version": replaygain_analysis.analyzer_version,
                    "reason": replaygain_analysis.reason,
                }
                if replaygain_analysis is not None
                else {
                    "status": (
                        "preserved"
                        if replaygain_settings.enabled
                        and replaygain_settings.mode == "preserve"
                        else "disabled"
                    )
                }
            ),
            "source_relative_path": source.subject.relative_path,
            "destination_relative_path": destination_relative,
            "sidecars": sidecars,
            "manual_tag_edit_mode": (
                tag_edit_intent.mode if tag_edit_intent is not None else None
            ),
        }
        return LibraryManagementPlanItem(
            job_id=snapshot.job_id,
            ordinal=source.subject.ordinal,
            bundle_ordinal=source.subject.bundle_ordinal,
            local_album_id=source.subject.local_album_id,
            local_track_id=source.subject.local_track_id,
            expected_album_revision=source.subject.album_revision,
            expected_track_revision=source.subject.track_revision,
            expected_identity_revision=identity_track.identity_revision,
            expected_album_identity_revision=identity.identity_revision,
            expected_override_revision=override_revision,
            expected_catalog_revision=snapshot.catalog_revision,
            expected_policy_revision=snapshot.policy_revision,
            expected_profile_revision=snapshot.profile_revision,
            expected_root_id=source.subject.root_id,
            expected_relative_path=source.subject.relative_path,
            expected_stat_revision=source.subject.stat_revision,
            expected_tag_revision=source.subject.tag_revision,
            expected_file_fingerprint=source.fingerprint,
            source_path_identity=_sha256_text(
                f"{source.subject.root_id}\x00{source.subject.relative_path}"
            ),
            destination_root_id=destination_root_id,
            destination_relative_path=destination_relative,
            destination_collision_key=collision_key,
            desired_document_json=desired_json,
            desired_document_hash=_sha256_text(desired_json),
            artwork_choices_json=_json(artwork_choices),
            diff_json=_json(diff),
            capability_json=_json(capability),
            collision_json=_json(collisions),
            eligibility=eligibility,
            reason_code=reason_code,
            estimated_temporary_bytes=estimated,
            created_at=self._clock(),
        )

    def _inspect_source(
        self,
        subject: LibraryManagementSelectionSubject,
        roots: dict[str, object],
    ) -> _SourceInspection:
        root = roots.get(subject.root_id)
        if root is None:
            return _SourceInspection(subject, None, None, "", ROOT_UNAVAILABLE)
        root_path = Path(root.path)
        relative = PurePosixPath(subject.relative_path)
        if relative.is_absolute() or any(
            part in {"", ".", ".."} for part in relative.parts
        ):
            return _SourceInspection(subject, None, None, "", OUT_OF_ROOT)
        path = root_path.joinpath(*relative.parts)
        expected = Path(os.path.normpath(subject.file_path))
        if not expected.is_absolute() or expected != Path(os.path.normpath(path)):
            return _SourceInspection(subject, path, None, "", OUT_OF_ROOT)
        try:
            root_stat = root_path.lstat()
            if stat.S_ISLNK(root_stat.st_mode):
                return _SourceInspection(subject, path, None, "", SYMLINK_UNSUPPORTED)
            if not stat.S_ISDIR(root_stat.st_mode):
                return _SourceInspection(subject, path, None, "", ROOT_UNAVAILABLE)
            current = root_path
            for part in relative.parts:
                current = current / part
                if stat.S_ISLNK(current.lstat().st_mode):
                    return _SourceInspection(
                        subject, path, None, "", SYMLINK_UNSUPPORTED
                    )
            file_stat = path.stat()
        except (FileNotFoundError, NotADirectoryError):
            return _SourceInspection(subject, path, None, "", FILE_UNREADABLE)
        except PermissionError:
            return _SourceInspection(subject, path, None, "", FILE_UNREADABLE)
        if not stat.S_ISREG(file_stat.st_mode):
            return _SourceInspection(subject, path, None, "", FILE_UNREADABLE)
        if (
            file_stat.st_size != subject.file_size_bytes
            or file_stat.st_mtime_ns != subject.file_mtime_ns
        ):
            return _SourceInspection(subject, path, None, "", FILE_CHANGED)
        parent_mode = path.parent.stat().st_mode
        if not parent_mode & (stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH):
            return _SourceInspection(subject, path, None, "", ROOT_READ_ONLY)
        if not file_stat.st_mode & (stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH):
            return _SourceInspection(subject, path, None, "", FILE_UNREADABLE)
        try:
            fingerprint = self._hash_file(path)
            if stat.S_ISLNK(path.lstat().st_mode):
                return _SourceInspection(subject, path, None, "", SYMLINK_UNSUPPORTED)
            document = self._audio.read(path)
            final_stat = path.stat()
        except AudioFormatError:
            return _SourceInspection(subject, path, None, "", FORMAT_UNSUPPORTED)
        except OSError:
            return _SourceInspection(subject, path, None, "", FILE_UNREADABLE)
        if (
            final_stat.st_size != file_stat.st_size
            or final_stat.st_mtime_ns != file_stat.st_mtime_ns
        ):
            return _SourceInspection(subject, path, None, "", FILE_CHANGED)
        return _SourceInspection(subject, path, document, fingerprint, None)

    def _destination_relative(
        self,
        naming_script: NamingScriptSettings,
        profile: LibraryManagementProfile,
        source: _SourceInspection,
        metadata: AudioMetadataDocument,
        destination_root: Path,
    ) -> tuple[str, str]:
        assert source.document is not None
        organization = profile.organization
        if not organization.rename_enabled and not organization.move_enabled:
            relative = source.subject.relative_path
            return relative, _path_collision_key(relative)
        named_document = msgspec.structs.replace(source.document, metadata=metadata)
        rendered = self._naming.format_management_path(
            naming_script.source,
            named_document,
            organization.compatibility,
            script_name=naming_script.name,
            root=destination_root,
        )
        rendered_path = PurePosixPath(rendered.relative_path)
        source_path = PurePosixPath(source.subject.relative_path)
        parent = (
            rendered_path.parent if organization.move_enabled else source_path.parent
        )
        filename = (
            rendered_path.name if organization.rename_enabled else source_path.name
        )
        relative = (parent / filename).as_posix()
        return relative, _path_collision_key(relative)

    @staticmethod
    def _destination_collisions(
        source: Path,
        source_fingerprint: str,
        destination: Path,
        destination_root: Path,
    ) -> tuple[list[dict[str, object]], str | None]:
        if destination == source:
            return [], None
        evidence: list[dict[str, object]] = []
        try:
            destination_stat = destination.lstat()
        except (FileNotFoundError, NotADirectoryError):
            destination_stat = None
        if destination_stat is not None:
            identical = False
            if stat.S_ISREG(destination_stat.st_mode) and not stat.S_ISLNK(
                destination_stat.st_mode
            ):
                try:
                    identical = (
                        LibraryManagementPlanner._hash_file(destination)
                        == source_fingerprint
                    )
                except OSError:
                    identical = False
            classification = (
                "same_path_same_content" if identical else "same_path_different_content"
            )
            evidence.append(
                {
                    "classification": classification,
                    "existing_relative_path": destination.relative_to(
                        destination_root
                    ).as_posix(),
                }
            )
            return (
                evidence,
                PATH_COLLISION_IDENTICAL if identical else PATH_COLLISION_DIFFERENT,
            )
        parent = destination.parent
        if parent.is_dir():
            wanted = _path_collision_key(destination.name)
            examined = 0
            with os.scandir(parent) as entries:
                for entry in entries:
                    examined += 1
                    if examined > MAX_SIDECAR_ENTRIES:
                        evidence.append({"classification": "normalized_path_collision"})
                        return evidence, PATH_COLLISION_DIFFERENT
                    if (
                        entry.name != destination.name
                        and _path_collision_key(entry.name) == wanted
                    ):
                        evidence.append(
                            {
                                "classification": "normalized_path_collision",
                                "existing_relative_path": (
                                    destination.parent / entry.name
                                )
                                .relative_to(destination_root)
                                .as_posix(),
                            }
                        )
                        return evidence, PATH_COLLISION_DIFFERENT
        return evidence, None

    @staticmethod
    def _sidecars(
        source_directory: Path,
        destination_directory: Path,
        profile: LibraryManagementProfile,
        bundle_first: bool,
    ) -> tuple[list[dict[str, object]], str | None]:
        organization = profile.organization
        if (
            not organization.move_sidecars
            or not bundle_first
            or source_directory == destination_directory
            or not organization.sidecar_patterns
        ):
            return [], None
        matches: list[dict[str, object]] = []
        destination_keys: set[str] = set()
        collision_reason: str | None = None
        examined = 0
        patterns = tuple(value.casefold() for value in organization.sidecar_patterns)
        for current_root, directories, files in os.walk(
            source_directory, followlinks=False
        ):
            current = Path(current_root)
            directories[:] = sorted(
                value for value in directories if not (current / value).is_symlink()
            )
            for name in sorted(files):
                examined += 1
                if examined > MAX_SIDECAR_ENTRIES:
                    return matches, SIDECAR_COLLISION
                path = current / name
                relative = path.relative_to(source_directory).as_posix()
                folded_relative = relative.casefold()
                matched = any(
                    fnmatch.fnmatchcase(folded_relative, pattern)
                    if "/" in pattern
                    else "/" not in folded_relative
                    and fnmatch.fnmatchcase(folded_relative, pattern)
                    for pattern in patterns
                )
                if path.is_symlink():
                    if matched:
                        return matches, SIDECAR_COLLISION
                    continue
                if not matched:
                    continue
                if path.suffix.casefold() in AUDIO_EXTENSION_FORMATS:
                    continue
                target = destination_directory / PurePosixPath(relative)
                destination_collision = bool(
                    target.exists()
                    or target.is_symlink()
                    or LibraryManagementPlanner._normalized_sibling_exists(target)
                )
                if destination_collision:
                    collision_reason = SIDECAR_COLLISION
                destination_key = _path_collision_key(str(target))
                if destination_key in destination_keys:
                    collision_reason = SIDECAR_COLLISION
                    destination_collision = True
                destination_keys.add(destination_key)
                try:
                    metadata = path.stat()
                    fingerprint = LibraryManagementPlanner._hash_file(path)
                    final_metadata = path.lstat()
                except OSError:
                    return matches, SIDECAR_COLLISION
                if (
                    stat.S_ISLNK(final_metadata.st_mode)
                    or final_metadata.st_size != metadata.st_size
                    or final_metadata.st_mtime_ns != metadata.st_mtime_ns
                ):
                    return matches, SIDECAR_COLLISION
                matches.append(
                    {
                        "source_relative_path": relative,
                        "destination_relative_path": relative,
                        "byte_size": metadata.st_size,
                        "mtime_ns": metadata.st_mtime_ns,
                        "sha256": fingerprint,
                        "destination_collision": destination_collision,
                    }
                )
        return matches, collision_reason

    @staticmethod
    def _normalized_sibling_exists(path: Path) -> bool:
        parent = path.parent
        if not parent.is_dir():
            return False
        wanted = _path_collision_key(path.name)
        examined = 0
        with os.scandir(parent) as entries:
            for entry in entries:
                examined += 1
                if examined > MAX_SIDECAR_ENTRIES:
                    return True
                if (
                    entry.name != path.name
                    and _path_collision_key(entry.name) == wanted
                ):
                    return True
        return False

    def _external_artwork(
        self,
        pinned: PinnedLibraryManagementProfile,
        source: _SourceInspection,
        metadata: AudioMetadataDocument,
        destination_root: Path,
        destination_relative: str,
        outputs: Sequence[ArtworkOutput],
    ) -> tuple[
        list[tuple[ArtworkOutput, str]],
        list[dict[str, object]],
        str | None,
    ]:
        assert source.document is not None
        planned: list[tuple[ArtworkOutput, str]] = []
        evidence: list[dict[str, object]] = []
        used_keys: set[str] = set()
        named_document = msgspec.structs.replace(source.document, metadata=metadata)
        destination_parent = PurePosixPath(destination_relative).parent
        for output in outputs:
            extension = "jpg" if output.format == "jpeg" else output.format
            if pinned.external_artwork_naming_script is not None:
                script = pinned.external_artwork_naming_script
                rendered = self._naming.format_management_path(
                    script.source,
                    named_document,
                    pinned.profile.organization.compatibility,
                    script_name=script.name,
                    root=destination_root,
                    artwork_type=output.image_type,
                    artwork_comment=output.description,
                    artwork_extension=extension,
                    artwork_format=output.format,
                )
                relative = rendered.relative_path
                collision_key = rendered.collision_key
            else:
                stem = "cover" if output.image_type == "front" else output.image_type
                raw_relative = (destination_parent / f"{stem}.{extension}").as_posix()
                rendered = self._naming.format_management_path(
                    raw_relative,
                    named_document,
                    pinned.profile.organization.compatibility,
                    script_name="Default external artwork naming",
                    root=destination_root,
                    artwork_type=output.image_type,
                    artwork_comment=output.description,
                    artwork_extension=extension,
                    artwork_format=output.format,
                )
                relative = rendered.relative_path
                collision_key = rendered.collision_key
            if collision_key in used_keys:
                evidence.append(
                    {
                        "classification": "external_artwork_path_collision",
                        "destination_relative_path": relative,
                    }
                )
                return planned, evidence, SIDECAR_COLLISION
            used_keys.add(collision_key)
            destination = destination_root / PurePosixPath(relative)
            collision, identical, existing_fingerprint = (
                self._external_artwork_collision(
                    destination,
                    output.sha256,
                    overwrite=pinned.profile.artwork.overwrite_external_files,
                )
            )
            if collision is not None:
                evidence.append(
                    {
                        "classification": collision,
                        "destination_relative_path": relative,
                        "existing_file_fingerprint": existing_fingerprint,
                    }
                )
                if identical:
                    continue
                if collision != "configured_external_artwork_replacement":
                    return planned, evidence, SIDECAR_COLLISION
            planned.append((output, relative))
        return planned, evidence, None

    @staticmethod
    def _external_artwork_collision(
        destination: Path,
        output_sha256: str,
        *,
        overwrite: bool,
    ) -> tuple[str | None, bool, str | None]:
        try:
            metadata = destination.lstat()
        except FileNotFoundError:
            metadata = None
        if metadata is not None:
            if stat.S_ISREG(metadata.st_mode) and not stat.S_ISLNK(metadata.st_mode):
                try:
                    existing_fingerprint = LibraryManagementPlanner._hash_file(
                        destination
                    )
                    if existing_fingerprint == output_sha256:
                        return (
                            "same_external_artwork_content",
                            True,
                            existing_fingerprint,
                        )
                except OSError:
                    return "external_artwork_destination_occupied", False, None
                if overwrite:
                    return (
                        "configured_external_artwork_replacement",
                        False,
                        existing_fingerprint,
                    )
            return "external_artwork_destination_occupied", False, None
        parent = destination.parent
        if parent.is_dir():
            wanted = _path_collision_key(destination.name)
            examined = 0
            with os.scandir(parent) as entries:
                for entry in entries:
                    examined += 1
                    if examined > MAX_SIDECAR_ENTRIES:
                        return "external_artwork_directory_limit", False, None
                    if (
                        entry.name != destination.name
                        and _path_collision_key(entry.name) == wanted
                    ):
                        return "normalized_external_artwork_collision", False, None
        return None, False, None

    @staticmethod
    def _destination_root_reason(root: Path) -> str | None:
        try:
            metadata = root.lstat()
        except (FileNotFoundError, NotADirectoryError, PermissionError):
            return ROOT_UNAVAILABLE
        if stat.S_ISLNK(metadata.st_mode):
            return SYMLINK_UNSUPPORTED
        if not stat.S_ISDIR(metadata.st_mode):
            return ROOT_UNAVAILABLE
        if not metadata.st_mode & (stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH):
            return ROOT_READ_ONLY
        return None

    @staticmethod
    def _destination_path_reason(root: Path, destination: Path) -> str | None:
        try:
            relative_parent = destination.parent.relative_to(root)
        except ValueError:
            return OUT_OF_ROOT
        current = root
        paths = [root]
        for part in relative_parent.parts:
            current = current / part
            paths.append(current)
        for current in paths:
            try:
                metadata = current.lstat()
            except FileNotFoundError:
                return ROOT_UNAVAILABLE if current == root else None
            except (NotADirectoryError, PermissionError):
                return ROOT_UNAVAILABLE
            if stat.S_ISLNK(metadata.st_mode):
                return SYMLINK_UNSUPPORTED
            if not stat.S_ISDIR(metadata.st_mode):
                return PATH_COLLISION_DIFFERENT
            if not metadata.st_mode & (stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH):
                return ROOT_READ_ONLY
        return None

    async def _store_artwork_choices(
        self,
        job_id: str,
        ordinal: int,
        outputs: Sequence[tuple[ArtworkOutput, str | None]],
    ) -> list[dict[str, object]]:
        choices: list[dict[str, object]] = []
        for index, (output, destination_relative_path) in enumerate(outputs):
            blob = await self._blobs.add_bytes(
                output.content,
                kind="image",
                created_at=self._clock(),
                media_metadata_json=_json(
                    {
                        "mime_type": output.mime_type,
                        "width": output.width,
                        "height": output.height,
                        "image_type": output.image_type,
                    }
                ),
            )
            await self._store.add_management_blob_reference(
                LibraryManagementBlobReference(
                    blob_sha256=blob.sha256,
                    reference_kind="artwork",
                    reference_id=f"{job_id}:{ordinal}:{index}",
                    created_at=self._clock(),
                )
            )
            choices.append(
                {
                    "output_kind": output.output_kind,
                    "image_type": output.image_type,
                    "blob_sha256": blob.sha256,
                    "mime_type": output.mime_type,
                    "format": output.format,
                    "width": output.width,
                    "height": output.height,
                    "byte_size": output.byte_size,
                    "source": output.source,
                    "source_candidate_id": output.source_candidate_id,
                    "source_is_exact_release": output.source_is_exact_release,
                    "description": output.description,
                    "destination_relative_path": destination_relative_path,
                }
            )
        return choices

    async def _seal_preview(
        self,
        snapshot: LibraryManagementJobSnapshot,
        worker_id: str,
        *,
        expected_snapshot_revision: int,
        roots: dict[str, object],
    ) -> LibraryManagementJobSnapshot:
        await self._validate_pinned_revisions(snapshot)
        totals = await self._store.get_library_management_plan_disk_totals(
            snapshot.job_id
        )
        root_reasons: dict[str, str] = {}
        for root_id, required in totals.items():
            root = roots.get(root_id)
            if root is None:
                root_reasons[root_id] = ROOT_UNAVAILABLE
                continue
            root_path = Path(root.path)
            root_reason = await asyncio.to_thread(
                self._destination_root_reason, root_path
            )
            if root_reason is not None:
                root_reasons[root_id] = root_reason
                continue
            try:
                usage = await asyncio.to_thread(shutil.disk_usage, root_path)
            except OSError:
                root_reasons[root_id] = ROOT_UNAVAILABLE
                continue
            if required + DISK_SAFETY_BYTES > usage.free:
                root_reasons[root_id] = INSUFFICIENT_SPACE
        if root_reasons:
            await self._store.block_library_management_plan_roots(
                snapshot.job_id,
                root_reasons,
                expected_snapshot_revision=expected_snapshot_revision,
            )
        return await self._store.finalize_library_management_preview(
            snapshot.job_id,
            worker_id,
            expected_snapshot_revision=expected_snapshot_revision,
            now=self._clock(),
        )

    @staticmethod
    def _identity_reason(
        identity, inspected: Sequence[_SourceInspection]
    ) -> str | None:
        if identity is None or identity.identity_revision is None:
            return IDENTITY_NOT_ACCEPTED
        if not identity.release_group_mbid or not identity.release_mbid:
            return RELEASE_NOT_SELECTED
        by_id = {value.local_track_id: value for value in identity.tracks}
        release_track_ids: set[str] = set()
        for source in inspected:
            track = by_id.get(source.subject.local_track_id)
            if (
                track is None
                or track.identity_revision is None
                or not track.recording_mbid
                or not track.release_track_mbid
                or track.release_mbid != identity.release_mbid
            ):
                return TRACK_NOT_MAPPED
            if track.release_track_mbid in release_track_ids:
                return TRACK_NOT_MAPPED
            release_track_ids.add(track.release_track_mbid)
        return None

    @staticmethod
    def _metadata_document(projection) -> AudioMetadataDocument:
        fields = tuple(
            AudioSemanticField(name=value.name, value=value.value)
            for value in projection.fields
            if value.value is not None and value.value != ()
        )
        artists = next(
            (value.value for value in projection.fields if value.name == "artist"), ()
        )
        album_artists = next(
            (
                value.value
                for value in projection.fields
                if value.name == "album_artist"
            ),
            (),
        )
        return AudioMetadataDocument(
            fields=fields,
            artist_display="; ".join(artists) if isinstance(artists, tuple) else None,
            album_artist_display=(
                "; ".join(album_artists) if isinstance(album_artists, tuple) else None
            ),
        )

    @staticmethod
    def _desired_fields(
        profile: LibraryManagementProfile,
        current: AudioMetadataDocument,
        desired: AudioMetadataDocument,
        enabled_names: set[str] | None = None,
    ) -> tuple[DesiredAudioField, ...]:
        fields: list[DesiredAudioField] = []
        selected_names = enabled_names
        if selected_names is None:
            selected_names = {
                value.field
                for value in profile.metadata.fields
                if profile.metadata.enabled
                and value.mode not in {"disabled", "preserve"}
            }
            if profile.genres.enabled:
                selected_names.add("genre")
        for name in sorted(selected_names):
            before = current.value_for(name)
            after = desired.value_for(name)
            if before == after:
                action = "unchanged"
            elif after is None or after == ():
                action = "clear"
            else:
                action = "set"
            fields.append(DesiredAudioField(name=name, action=action, value=after))
        return tuple(fields)

    @staticmethod
    def _embedded_descriptor(output: ArtworkOutput) -> EmbeddedArtworkDescriptor:
        return EmbeddedArtworkDescriptor(
            image_type=output.image_type,
            mime_type=output.mime_type,
            description=output.description,
            width=output.width,
            height=output.height,
            byte_size=output.byte_size,
            sha256=output.sha256,
            content=output.content,
            format_supported=True,
        )

    def _blocked_item(
        self,
        snapshot: LibraryManagementJobSnapshot,
        source: _SourceInspection,
        reason_code: str,
    ) -> LibraryManagementPlanItem:
        desired_json = "{}"
        return LibraryManagementPlanItem(
            job_id=snapshot.job_id,
            ordinal=source.subject.ordinal,
            bundle_ordinal=source.subject.bundle_ordinal,
            local_album_id=source.subject.local_album_id,
            local_track_id=source.subject.local_track_id,
            expected_album_revision=source.subject.album_revision,
            expected_track_revision=source.subject.track_revision,
            expected_catalog_revision=snapshot.catalog_revision,
            expected_policy_revision=snapshot.policy_revision,
            expected_profile_revision=snapshot.profile_revision,
            expected_root_id=source.subject.root_id,
            expected_relative_path=source.subject.relative_path,
            expected_stat_revision=source.subject.stat_revision,
            expected_tag_revision=source.subject.tag_revision,
            expected_file_fingerprint=source.fingerprint,
            source_path_identity=_sha256_text(
                f"{source.subject.root_id}\x00{source.subject.relative_path}"
            ),
            desired_document_json=desired_json,
            desired_document_hash=_sha256_text(desired_json),
            diff_json=_json({"requires_write": False}),
            capability_json=_json({"audio_format": source.subject.file_format}),
            eligibility="blocked",
            reason_code=reason_code,
            created_at=self._clock(),
        )

    @staticmethod
    def _hash_file(path: Path) -> str:
        digest = hashlib.sha256()
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as input_file:
            if not stat.S_ISREG(os.fstat(input_file.fileno()).st_mode):
                raise OSError("Management source is not a regular file.")
            while chunk := input_file.read(1024 * 1024):
                digest.update(chunk)
        return digest.hexdigest()

    @staticmethod
    def _find_profile(
        settings: LibraryManagementSettings, profile_id: str
    ) -> LibraryManagementProfile:
        for profile in settings.profiles:
            if profile.id == profile_id:
                return profile
        raise ResourceNotFoundError("Library Management profile not found.")

    @staticmethod
    def _find_naming_script(
        settings: LibraryManagementSettings, script_id: str
    ) -> NamingScriptSettings:
        for script in settings.naming_scripts:
            if script.id == script_id:
                return script
        raise ConfigurationError("The profile's naming script does not exist.")

    @staticmethod
    def _find_tagging_script(
        settings: LibraryManagementSettings, script_id: str
    ) -> TaggingScriptSettings:
        for script in settings.tagging_scripts:
            if script.id == script_id:
                return script
        raise ConfigurationError("The profile's tagging script does not exist.")
