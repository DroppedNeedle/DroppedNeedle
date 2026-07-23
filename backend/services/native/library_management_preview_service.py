"""Admin-facing orchestration for immutable management previews and activation."""

from __future__ import annotations

import asyncio
from collections.abc import Callable
import hashlib
import hmac
import json
from pathlib import Path, PurePosixPath
import re
import stat
import time

import msgspec

from api.v1.schemas.library_management import (
    LibraryManagementProfile,
    LibraryManagementSettings,
    LibraryManagementSettingsResponse,
    profile_revision,
    settings_revision,
)
from api.v1.schemas.library_management_preview import (
    LibraryManagementActivationConfirmRequest,
    LibraryManagementActivationProof,
    LibraryManagementActivationPreviewRequest,
    LibraryManagementApplyRequest,
    LibraryManagementDiscardRequest,
    LibraryManagementOperationHistoryItemResponse,
    LibraryManagementOperationHistoryResponse,
    LibraryManagementPlanItemPageResponse,
    LibraryManagementPlanItemResponse,
    LibraryManagementPreviewCreateRequest,
    LibraryManagementPreviewCreatedResponse,
    LibraryManagementPreviewDetailResponse,
    LibraryManagementExternalRefreshResponse,
    LibraryManagementPreviewSummaryResponse,
    LibraryManagementResultItemResponse,
    LibraryManagementResultPageResponse,
    LibraryManagementTagEditPreviewRequest,
    LibraryManagementTagEditorContextResponse,
    LibraryManagementTagEditorFieldResponse,
)
from api.v1.schemas.library_operations import OperationResponse
from core.exceptions import (
    ConfigurationError,
    ResourceNotFoundError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.audio.metadata_engine import AudioMetadataEngine
from models.library_management import FILE_CHANGED, POLICY_CHANGED, PROFILE_CHANGED
from models.library_management import (
    LibraryManagementJobSnapshot,
    LibraryManagementPlanItem,
    LibraryManagementTagEditFieldIntent,
    LibraryManagementTagEditIntent,
)
from models.library_management_planning import (
    LibraryManagementCatalogFilter,
    LibraryManagementSelection,
    NormalizedLibraryManagementSelection,
    PinnedLibraryManagementProfile,
)
from services.native.library_management_planner import LibraryManagementPlanner
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.library_operation_service import LibraryOperationService
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.effective_metadata_projection_service import (
    normalize_managed_field_value,
)
from services.native.managed_field_registry import MANAGED_FIELD_REGISTRY
from services.preferences_service import PreferencesService


def _stable_json(value: object) -> str:
    return json.dumps(
        msgspec.to_builtins(value),
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )


_ARTWORK_PREVIEW_MIME_TYPES = frozenset(
    {"image/gif", "image/jpeg", "image/png", "image/webp"}
)


class LibraryManagementPreviewService:
    def __init__(
        self,
        store: NativeLibraryStore,
        preferences: PreferencesService,
        profiles: LibraryManagementProfileService,
        planner: LibraryManagementPlanner,
        audio: AudioMetadataEngine | None = None,
        blobs: LibraryManagementBlobStore | None = None,
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._preferences = preferences
        self._profiles = profiles
        self._planner = planner
        self._audio = audio or AudioMetadataEngine()
        self._blobs = blobs
        self._clock = clock

    async def create_manual(
        self,
        request: LibraryManagementPreviewCreateRequest,
        actor_user_id: str,
    ) -> LibraryManagementPreviewCreatedResponse:
        _settings, effective = self._profiles.prepare_manual_profile(
            request.profile_id, request.overrides
        )
        handle = await self._planner.create_preview(
            selection=self._selection(request.selection),
            profile_id=request.profile_id,
            expected_settings_revision=request.expected_settings_revision,
            expected_policy_revision=request.expected_policy_revision,
            actor_user_id=actor_user_id,
            idempotency_key=request.idempotency_key,
            target_root_id=request.target_root_id,
            effective_profile=effective,
        )
        return msgspec.convert(
            msgspec.to_builtins(handle),
            type=LibraryManagementPreviewCreatedResponse,
        )

    async def tag_editor_context(
        self, local_track_id: str
    ) -> LibraryManagementTagEditorContextResponse:
        subject = await self._tag_editor_subject(local_track_id)
        resolver = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        roots = {value.id: value for value in resolver.settings.library_roots}
        root = roots.get(str(subject["root_id"]))
        if root is None:
            raise ConfigurationError("The track's library root is not configured.")
        path = Path(root.path) / PurePosixPath(str(subject["relative_path"]))
        stored_path = Path(str(subject["file_path"]))
        if path != stored_path or not path.exists():
            raise ValidationError("The audio file is no longer present on disk.")
        try:
            if stat.S_ISLNK(path.lstat().st_mode):
                raise ValidationError("Symbolic-link audio files cannot be managed.")
            document = await asyncio.to_thread(self._audio.read, path)
        except OSError as error:
            raise ValidationError("Could not read the audio file.") from error

        settings, profile = self._profiles.prepare_tag_editor_profile(
            root_id=str(subject["root_id"]),
            field_names=(),
            reset_canonical=False,
        )
        album_overrides, _ = await self._store.list_management_overrides(
            subject_kind="album", subject_id=str(subject["local_album_id"])
        )
        track_overrides, _ = await self._store.list_management_overrides(
            subject_kind="track", subject_id=str(subject["id"])
        )
        overrides = {
            value.field_name: value for value in (*album_overrides, *track_overrides)
        }
        current_values = {value.name: value.value for value in document.metadata.fields}
        identity = await self._store.get_accepted_library_management_identity(
            str(subject["local_album_id"]),
            local_track_ids=(str(subject["id"]),),
        )
        accepted, identity_reason = self._tag_editor_identity(identity)
        return LibraryManagementTagEditorContextResponse(
            local_track_id=str(subject["id"]),
            local_album_id=str(subject["local_album_id"]),
            root_id=str(subject["root_id"]),
            profile_id=profile.id,
            profile_name=profile.name,
            settings_revision=settings_revision(settings),
            policy_revision=resolver.policy_revision,
            track_revision=int(subject["track_revision"]),
            album_revision=int(subject["album_revision"]),
            accepted_identity=accepted,
            identity_reason=identity_reason,
            fields=[
                LibraryManagementTagEditorFieldResponse(
                    field_name=name,
                    scope=definition.scope,
                    cardinality=definition.cardinality,
                    current_value=self._http_field_value(current_values.get(name)),
                    override_id=(overrides[name].id if name in overrides else None),
                    override_mode=(overrides[name].mode if name in overrides else None),
                    override_row_revision=(
                        overrides[name].row_revision if name in overrides else None
                    ),
                )
                for name, definition in MANAGED_FIELD_REGISTRY.items()
                if definition.allow_override
            ],
        )

    async def create_tag_edit(
        self,
        request: LibraryManagementTagEditPreviewRequest,
        actor_user_id: str,
    ) -> LibraryManagementPreviewCreatedResponse:
        subject = await self._tag_editor_subject(request.local_track_id)
        if not request.fields or len(request.fields) > len(MANAGED_FIELD_REGISTRY):
            raise ValidationError("Select at least one valid field to edit.")
        names = [value.field_name for value in request.fields]
        if len(set(names)) != len(names):
            raise ValidationError("A tag field can be edited only once per preview.")

        album_overrides, _ = await self._store.list_management_overrides(
            subject_kind="album", subject_id=str(subject["local_album_id"])
        )
        track_overrides, _ = await self._store.list_management_overrides(
            subject_kind="track", subject_id=str(subject["id"])
        )
        existing = {
            (value.subject_kind, value.field_name): value
            for value in (*album_overrides, *track_overrides)
        }
        fields: list[LibraryManagementTagEditFieldIntent] = []
        for requested in request.fields:
            definition = MANAGED_FIELD_REGISTRY.get(requested.field_name)
            if definition is None or not definition.allow_override:
                raise ValidationError("That tag field cannot be edited.")
            override = existing.get((definition.scope, requested.field_name))
            if request.mode == "reset_canonical":
                if override is None:
                    raise ValidationError(
                        f"{requested.field_name} has no local override to reset."
                    )
                value = None
            else:
                value = normalize_managed_field_value(definition, requested.value)
                if value == "":
                    value = None
            fields.append(
                LibraryManagementTagEditFieldIntent(
                    field_name=requested.field_name,
                    subject_kind=definition.scope,
                    value=value,
                    override_id=override.id if override is not None else None,
                    expected_override_row_revision=(
                        override.row_revision if override is not None else None
                    ),
                )
            )

        settings, profile = self._profiles.prepare_tag_editor_profile(
            root_id=str(subject["root_id"]),
            field_names=tuple(names),
            reset_canonical=request.mode == "reset_canonical",
        )
        if settings_revision(settings) != request.expected_settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed before tag preview."
            )
        intent = LibraryManagementTagEditIntent(
            local_track_id=str(subject["id"]),
            local_album_id=str(subject["local_album_id"]),
            mode=request.mode,
            fields=fields,
        )
        handle = await self._planner.create_preview(
            selection=LibraryManagementSelection(
                kind="tracks", ids=(str(subject["id"]),)
            ),
            profile_id=profile.id,
            expected_settings_revision=request.expected_settings_revision,
            expected_policy_revision=request.expected_policy_revision,
            actor_user_id=actor_user_id,
            idempotency_key=request.idempotency_key,
            effective_profile=profile,
            tag_edit_intent=intent,
            force_expand_album_bundles=any(
                value.subject_kind == "album" for value in fields
            ),
        )
        return msgspec.convert(
            msgspec.to_builtins(handle),
            type=LibraryManagementPreviewCreatedResponse,
        )

    async def _tag_editor_subject(self, track_id: str) -> dict:
        subject = await self._store.get_library_management_tag_editor_subject(track_id)
        if subject is None or subject["availability"] != "indexed":
            raise ResourceNotFoundError("Library track not found.")
        return subject

    @staticmethod
    def _http_field_value(value: object):
        return list(value) if isinstance(value, tuple) else value

    @staticmethod
    def _tag_editor_identity(identity) -> tuple[bool, str | None]:
        if identity is None or identity.identity_revision is None:
            return False, "IDENTITY_NOT_ACCEPTED"
        if not identity.release_group_mbid or not identity.release_mbid:
            return False, "RELEASE_NOT_SELECTED"
        track = identity.tracks[0] if identity.tracks else None
        if (
            track is None
            or track.identity_revision is None
            or not track.recording_mbid
            or not track.release_track_mbid
            or track.release_mbid != identity.release_mbid
        ):
            return False, "TRACK_NOT_MAPPED"
        return True, None

    async def create_activation(
        self,
        request: LibraryManagementActivationPreviewRequest,
        actor_user_id: str,
    ) -> LibraryManagementPreviewCreatedResponse:
        normalized, _assignment, effective, policy = self._profiles.prepare_activation(
            request.settings,
            root_id=request.root_id,
            expected_settings_revision=request.expected_settings_revision,
        )
        if policy.policy_revision != request.expected_policy_revision:
            raise StaleRevisionError(
                "Library policy changed before activation preview."
            )
        handle = await self._planner.create_preview(
            selection=LibraryManagementSelection(kind="roots", ids=(request.root_id,)),
            profile_id=effective.id,
            expected_settings_revision=request.expected_settings_revision,
            expected_policy_revision=request.expected_policy_revision,
            actor_user_id=actor_user_id,
            idempotency_key=request.idempotency_key,
            settings_snapshot=normalized,
            effective_profile=effective,
        )
        return msgspec.convert(
            msgspec.to_builtins(handle),
            type=LibraryManagementPreviewCreatedResponse,
        )

    async def detail(self, job_id: str) -> LibraryManagementPreviewDetailResponse:
        snapshot = await self._store.get_library_management_job_snapshot(job_id)
        operation = await self._store.get_operation_job(job_id)
        if (
            snapshot is None
            or operation is None
            or operation["kind"] != "library_management"
        ):
            raise ResourceNotFoundError("Library Management preview not found.")
        try:
            pinned = msgspec.json.decode(
                snapshot.profile_snapshot_json.encode("utf-8"),
                type=PinnedLibraryManagementProfile,
            )
            selection = json.loads(snapshot.selection_json)
            summary_payload = json.loads(snapshot.summary_json)
            summary = msgspec.convert(
                summary_payload,
                type=LibraryManagementPreviewSummaryResponse,
                strict=False,
            )
        except (
            msgspec.DecodeError,
            msgspec.ValidationError,
            json.JSONDecodeError,
        ) as error:
            raise ValidationError(
                "The stored Library Management preview is invalid."
            ) from error
        stale_reasons = await self._stale_reasons(snapshot)
        external_refreshes = (
            await self._store.list_library_management_external_refreshes(job_id)
        )
        now = self._clock()
        recovery = await self._store.get_management_operation_recovery_availability(
            job_id, now=now
        )
        expired = (
            snapshot.preview_expires_at is None or snapshot.preview_expires_at <= now
        )
        ready = (
            operation["state"] == "ready"
            and snapshot.phase == "ready"
            and not expired
            and not stale_reasons
        )
        return LibraryManagementPreviewDetailResponse(
            job_id=job_id,
            state=str(operation["state"]),
            phase=snapshot.phase,
            mode=snapshot.mode,
            origin=snapshot.origin,
            profile_id=pinned.profile.id,
            profile_name=pinned.profile.name,
            profile_revision=snapshot.profile_revision,
            settings_revision=snapshot.settings_revision,
            proposed_settings_revision=snapshot.proposed_settings_revision,
            policy_revision=snapshot.policy_revision,
            catalog_revision=snapshot.catalog_revision,
            target_root_id=snapshot.target_root_id,
            selection=selection,
            summary=summary,
            created_at=snapshot.created_at,
            updated_at=snapshot.updated_at,
            expires_at=snapshot.preview_expires_at,
            expired=expired,
            stale=bool(stale_reasons),
            stale_reasons=stale_reasons,
            ready_for_confirmation=ready,
            operation_row_revision=int(operation["row_revision"]),
            operation_event_revision=int(operation["event_revision"]),
            terminal_code=operation["terminal_code"],
            expected_work_count=int(operation.get("expected_work_count", 0)),
            completed_count=int(operation.get("completed_count", 0)),
            succeeded_count=int(operation.get("succeeded_count", 0)),
            failed_count=int(operation.get("failed_count", 0)),
            skipped_count=int(operation.get("skipped_count", 0)),
            control_request=str(operation.get("control_request", "none")),
            undo_available_count=int(recovery["undo_available_count"] or 0),
            undo_expired_count=int(recovery["undo_expired_count"] or 0),
            undo_expires_at=(
                float(recovery["undo_expires_at"])
                if recovery["undo_expires_at"] is not None
                else None
            ),
            baseline_available_count=int(recovery["baseline_available_count"] or 0),
            external_refreshes=[
                LibraryManagementExternalRefreshResponse(
                    target=value.target,
                    state=value.state,
                    attempts=value.attempts,
                    max_attempts=value.max_attempts,
                    failure_code=value.failure_code,
                    updated_at=value.updated_at,
                    completed_at=value.completed_at,
                )
                for value in external_refreshes
            ],
        )

    async def apply(
        self, job_id: str, request: LibraryManagementApplyRequest
    ) -> OperationResponse:
        if not request.confirmation:
            raise ValidationError("Confirm Apply Library Management before starting.")
        if not request.idempotency_key.strip():
            raise ValidationError("An apply idempotency key is required.")
        snapshot = await self._store.get_library_management_job_snapshot(job_id)
        if snapshot is None:
            raise ResourceNotFoundError("Library Management preview not found.")
        token_hash = hashlib.sha256(request.preview_token.encode("utf-8")).hexdigest()
        if not hmac.compare_digest(token_hash, snapshot.preview_token_hash or ""):
            raise ValidationError("The Library Management preview token is invalid.")
        if snapshot.proposed_settings_revision is not None:
            raise ValidationError("An activation preview cannot be applied.")
        if snapshot.phase == "ready":
            detail = await self.detail(job_id)
            if not detail.ready_for_confirmation:
                raise StaleRevisionError(
                    "The Library Management preview is not current and ready to apply."
                )
        row = await self._store.begin_library_management_apply(
            job_id,
            preview_token_hash=token_hash,
            expected_job_revision=request.expected_operation_row_revision,
            idempotency_key=request.idempotency_key,
            now=self._clock(),
        )
        return LibraryOperationService._response(row)

    async def discard(
        self, job_id: str, request: LibraryManagementDiscardRequest
    ) -> LibraryManagementPreviewDetailResponse:
        await self._store.discard_library_management_preview(
            job_id,
            expected_job_revision=request.expected_operation_row_revision,
            now=self._clock(),
        )
        return await self.detail(job_id)

    async def history(
        self,
        *,
        limit: int,
        cursor: str | None,
        origin: str | None,
        profile_id: str | None,
        root_id: str | None,
        state: str | None,
        mode: str | None,
        created_from: float | None,
        created_to: float | None,
    ) -> LibraryManagementOperationHistoryResponse:
        if limit < 1 or limit > 50:
            raise ValidationError(
                "Library Management history page size must be between 1 and 50."
            )
        if origin is not None and origin not in {
            "manual",
            "acquisition",
            "drop_import",
            "scan_discovered",
        }:
            raise ValidationError("Unknown Library Management origin filter.")
        if mode is not None and mode not in {
            "preview",
            "apply",
            "automatic_apply",
            "undo",
            "baseline_restore",
            "duplicate_resolution",
        }:
            raise ValidationError("Unknown Library Management mode filter.")
        if state is not None and state not in {
            "queued",
            "running",
            "paused",
            "ready",
            "succeeded",
            "failed",
            "cancelled",
            "stopped",
        }:
            raise ValidationError("Unknown Library Management state filter.")
        if (
            created_from is not None
            and created_to is not None
            and created_from > created_to
        ):
            raise ValidationError("The history date range is invalid.")
        before_created_at: float | None = None
        before_id: str | None = None
        if cursor is not None:
            try:
                created, before_id = cursor.split(":", 1)
                before_created_at = float(created)
            except (TypeError, ValueError) as error:
                raise ValidationError(
                    "The Library Management history cursor is invalid."
                ) from error
        rows = await self._store.list_library_management_operations(
            limit=limit + 1,
            before_created_at=before_created_at,
            before_id=before_id,
            origin=origin,
            profile_id=profile_id,
            root_id=root_id,
            state=state,
            mode=mode,
            created_from=created_from,
            created_to=created_to,
        )
        visible = rows[:limit]
        return LibraryManagementOperationHistoryResponse(
            items=[self._history_item(row) for row in visible],
            next_cursor=(
                f"{visible[-1]['created_at']}:{visible[-1]['id']}"
                if len(rows) > limit and visible
                else None
            ),
        )

    async def results(
        self, job_id: str, *, after_ordinal: int, limit: int
    ) -> LibraryManagementResultPageResponse:
        if await self._store.get_library_management_job_snapshot(job_id) is None:
            raise ResourceNotFoundError("Library Management operation not found.")
        rows = await self._store.list_library_management_result_items(
            job_id, after_ordinal=after_ordinal, limit=limit + 1
        )
        has_more = len(rows) > limit
        visible = rows[:limit]
        items: list[LibraryManagementResultItemResponse] = []
        for row in visible:
            plan = msgspec.convert(
                {
                    field: row[field]
                    for field in LibraryManagementPlanItem.__struct_fields__
                },
                type=LibraryManagementPlanItem,
                strict=False,
            )
            try:
                result = (
                    json.loads(str(row["result_json"])) if row["result_json"] else {}
                )
            except json.JSONDecodeError as error:
                raise ValidationError(
                    "The stored Library Management result is invalid."
                ) from error
            items.append(
                LibraryManagementResultItemResponse(
                    plan=self._item(plan),
                    work_state=str(row["work_state"] or "not_scheduled"),
                    failure_code=row["failure_code"] or plan.reason_code,
                    result=result,
                    journal_states=sorted(
                        str(row["journal_states"] or "").split(",")
                        if row["journal_states"]
                        else []
                    ),
                )
            )
        return LibraryManagementResultPageResponse(
            items=items,
            next_after_ordinal=(
                int(visible[-1]["ordinal"]) if has_more and visible else None
            ),
            has_more=has_more,
        )

    async def items(
        self,
        job_id: str,
        *,
        after_ordinal: int,
        limit: int,
        eligibility: str | None,
        reason_code: str | None,
        root_id: str | None,
        artist_id: str | None,
        album_id: str | None,
        audio_format: str | None,
        collision_class: str | None,
        has_preserved_value: bool | None,
        has_representation_loss: bool | None,
        change_kind: str | None,
    ) -> LibraryManagementPlanItemPageResponse:
        if await self._store.get_library_management_job_snapshot(job_id) is None:
            raise ResourceNotFoundError("Library Management preview not found.")
        rows = await self._store.list_library_management_plan_items(
            job_id,
            after_ordinal=after_ordinal,
            limit=limit + 1,
            eligibility=eligibility,
            reason_code=reason_code,
            root_id=root_id,
            artist_id=artist_id,
            album_id=album_id,
            audio_format=audio_format,
            collision_class=collision_class,
            has_preserved_value=has_preserved_value,
            has_representation_loss=has_representation_loss,
            change_kind=change_kind,
        )
        has_more = len(rows) > limit
        visible = rows[:limit]
        return LibraryManagementPlanItemPageResponse(
            items=[self._item(row) for row in visible],
            next_after_ordinal=(visible[-1].ordinal if has_more and visible else None),
            has_more=has_more,
        )

    async def artwork_preview(
        self, job_id: str, ordinal: int, sha256: str
    ) -> tuple[bytes, str]:
        if re.fullmatch(r"[0-9a-f]{64}", sha256) is None:
            raise ResourceNotFoundError("Library Management artwork not found.")
        item = await self._store.get_library_management_plan_item(job_id, ordinal)
        if item is None:
            raise ResourceNotFoundError("Library Management artwork not found.")
        try:
            choices = json.loads(item.artwork_choices_json)
        except json.JSONDecodeError as error:
            raise ValidationError(
                "The stored Library Management artwork preview is invalid."
            ) from error
        choice = next(
            (
                value
                for value in choices
                if isinstance(value, dict) and value.get("blob_sha256") == sha256
            ),
            None,
        )
        if choice is None or self._blobs is None:
            raise ResourceNotFoundError("Library Management artwork not found.")
        mime_type = choice.get("mime_type")
        if mime_type not in _ARTWORK_PREVIEW_MIME_TYPES:
            raise ResourceNotFoundError("Library Management artwork not found.")
        return await self._blobs.read_bytes(sha256), mime_type

    async def confirm_activation(
        self,
        request: LibraryManagementActivationConfirmRequest,
    ) -> LibraryManagementSettingsResponse:
        if not request.confirmation:
            raise ValidationError("Confirm Enable Library Management before saving.")
        if not request.proofs:
            raise ValidationError("At least one activation preview proof is required.")
        proof_roots = [proof.root_id for proof in request.proofs]
        if len(set(proof_roots)) != len(proof_roots):
            raise ValidationError("An activation root can be confirmed only once.")

        normalized: LibraryManagementSettings | None = None
        assignments_by_root = {}
        validated_roots: set[str] = set()
        validated: list[
            tuple[
                LibraryManagementActivationProof,
                LibraryManagementProfile,
                LibraryManagementJobSnapshot,
            ]
        ] = []
        for proof in request.proofs:
            prepared, _assignment, effective, _policy = (
                self._profiles.prepare_activation(
                    request.settings,
                    root_id=proof.root_id,
                    expected_settings_revision=request.expected_settings_revision,
                )
            )
            if normalized is None:
                normalized = prepared
                assignments_by_root = {
                    value.root_id: value for value in normalized.root_assignments
                }
            await self._validate_activation_proof(
                proof.root_id,
                proof.job_id,
                proof.preview_token,
                prepared,
                effective,
            )
            snapshot = await self._store.get_library_management_job_snapshot(
                proof.job_id
            )
            assert snapshot is not None
            validated.append((proof, effective, snapshot))

        assert normalized is not None
        for proof, effective, snapshot in validated:
            assignment = assignments_by_root[proof.root_id]
            assignment.activation_profile_revision = profile_revision(effective)
            assignment.activation_policy_revision = snapshot.policy_revision
            assignment.activation_settings_revision = request.expected_settings_revision
            assignment.activation_preview_token = proof.preview_token
            assignment.activation_preview_hash = snapshot.preview_token_hash
            assignment.activation_confirmed_at = self._clock()
            validated_roots.add(proof.root_id)

        return self._profiles.save_settings(
            normalized,
            expected_settings_revision=request.expected_settings_revision,
            validated_activation_root_ids=frozenset(validated_roots),
        )

    async def _validate_activation_proof(
        self,
        root_id: str,
        job_id: str,
        preview_token: str,
        settings: LibraryManagementSettings,
        effective_profile: LibraryManagementProfile,
    ) -> None:
        snapshot = await self._store.get_library_management_job_snapshot(job_id)
        detail = await self.detail(job_id)
        if snapshot is None:
            raise ResourceNotFoundError("Library Management preview not found.")
        token_hash = hashlib.sha256(preview_token.encode("utf-8")).hexdigest()
        if not hmac.compare_digest(token_hash, snapshot.preview_token_hash or ""):
            raise ValidationError("The activation preview token is invalid.")
        if not detail.ready_for_confirmation:
            raise StaleRevisionError(
                "The activation preview is not current and ready for confirmation."
            )
        try:
            selection = msgspec.json.decode(
                snapshot.selection_json.encode("utf-8"),
                type=NormalizedLibraryManagementSelection,
            )
        except (msgspec.DecodeError, msgspec.ValidationError) as error:
            raise ValidationError(
                "The activation preview selection is invalid."
            ) from error
        if (
            selection.kind != "roots"
            or selection.ids != (root_id,)
            or len(selection.root_scopes) != 1
            or selection.root_scopes[0].root_id != root_id
            or selection.root_scopes[0].relative_prefix is not None
            or snapshot.target_root_id is not None
        ):
            raise ValidationError(
                "The activation preview does not cover exactly the selected root."
            )
        expected_pinned = self._planner.pin_profile(settings, effective_profile)
        if (
            snapshot.profile_revision != profile_revision(effective_profile)
            or snapshot.proposed_settings_revision != settings_revision(settings)
            or snapshot.profile_snapshot_json != _stable_json(expected_pinned)
            or snapshot.settings_revision
            != settings_revision(
                self._preferences.get_library_management_settings_raw()
            )
        ):
            raise StaleRevisionError(
                "The activation preview does not match the proposed profile."
            )

    async def _stale_reasons(self, snapshot: LibraryManagementJobSnapshot) -> list[str]:
        reasons: list[str] = []
        current_settings_revision = settings_revision(
            self._preferences.get_library_management_settings_raw()
        )
        if current_settings_revision != snapshot.settings_revision:
            reasons.append(PROFILE_CHANGED)
        policy = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        if policy.policy_revision != snapshot.policy_revision:
            reasons.append(POLICY_CHANGED)
        if await self._store.get_catalog_revision() != snapshot.catalog_revision:
            reasons.append(FILE_CHANGED)
        return reasons

    @staticmethod
    def _selection(value) -> LibraryManagementSelection:
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
    def _item(row: LibraryManagementPlanItem) -> LibraryManagementPlanItemResponse:
        try:
            desired_document = json.loads(row.desired_document_json)
            artwork_choices = json.loads(row.artwork_choices_json)
            diff = json.loads(row.diff_json)
            capability = json.loads(row.capability_json)
            collisions = json.loads(row.collision_json)
        except json.JSONDecodeError as error:
            raise ValidationError(
                "The stored Library Management plan item is invalid."
            ) from error
        return LibraryManagementPlanItemResponse(
            ordinal=row.ordinal,
            bundle_ordinal=row.bundle_ordinal,
            local_album_id=row.local_album_id,
            local_track_id=row.local_track_id,
            source_root_id=row.expected_root_id,
            source_relative_path=row.expected_relative_path,
            destination_root_id=row.destination_root_id,
            destination_relative_path=row.destination_relative_path,
            eligibility=row.eligibility,
            reason_code=row.reason_code,
            estimated_temporary_bytes=row.estimated_temporary_bytes,
            desired_document=desired_document,
            artwork_choices=artwork_choices,
            diff=diff,
            capability=capability,
            collisions=collisions,
        )

    @staticmethod
    def _history_item(row: dict) -> LibraryManagementOperationHistoryItemResponse:
        try:
            pinned = msgspec.json.decode(
                str(row["management_profile_snapshot_json"]).encode(),
                type=PinnedLibraryManagementProfile,
            )
            selection = json.loads(str(row["management_selection_json"]))
        except (
            msgspec.DecodeError,
            msgspec.ValidationError,
            json.JSONDecodeError,
        ) as error:
            raise ValidationError(
                "The stored Library Management history is invalid."
            ) from error
        return LibraryManagementOperationHistoryItemResponse(
            operation=LibraryOperationService._response(row),
            mode=str(row["management_mode"]),
            origin=str(row["management_origin"]),
            phase=str(row["management_phase"]),
            profile_id=pinned.profile.id,
            profile_name=pinned.profile.name,
            profile_revision=str(row["management_profile_revision"]),
            target_root_id=row["management_target_root_id"],
            selection=selection,
        )
