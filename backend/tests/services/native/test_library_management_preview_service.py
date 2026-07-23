import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import msgspec
import pytest

from api.v1.schemas.library_management import (
    PICARD_ORGANIZER_PROFILE_ID,
    LibraryManagementRootAssignment,
    LibraryManagementRootOverrides,
    settings_revision,
)
from api.v1.schemas.library_management_preview import (
    LibraryManagementActivationConfirmRequest,
    LibraryManagementActivationProof,
    LibraryManagementApplyRequest,
    LibraryManagementDiscardRequest,
    LibraryManagementPreviewCreateRequest,
    LibraryManagementSelectionRequest,
    LibraryManagementTagEditFieldRequest,
    LibraryManagementTagEditPreviewRequest,
)
from core.config import Settings
from core.exceptions import ResourceNotFoundError, StaleRevisionError, ValidationError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_management import (
    LibraryManagementExternalRefreshDelivery,
    LibraryManagementJobSnapshot,
    LibraryManagementOverride,
    LibraryManagementPlanItem,
)
from models.audio_metadata import AudioMetadataDocument, AudioSemanticField
from models.library_management_canonical import (
    AcceptedAlbumManagementIdentity,
    AcceptedTrackManagementIdentity,
)
from models.library_management_planning import (
    LibraryManagementPreviewHandle,
    LibraryManagementRootScope,
    NormalizedLibraryManagementSelection,
)
from services.native.library_management_planner import LibraryManagementPlanner
from services.native.library_management_preview_service import (
    LibraryManagementPreviewService,
)
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.preferences_service import PreferencesService


def _json(value: object) -> str:
    return json.dumps(
        msgspec.to_builtins(value),
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )


def _preferences(tmp_path: Path) -> tuple[PreferencesService, str]:
    root = tmp_path / "Music"
    root.mkdir()
    second_root = tmp_path / "Music Two"
    second_root.mkdir()
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    settings.config_file_path.write_text(
        json.dumps(
            {
                "library_settings": {
                    "library_roots": [
                        {
                            "id": "root-1",
                            "path": str(root),
                            "label": "Music",
                            "policy": "automatic",
                            "rules": [],
                        },
                        {
                            "id": "root-2",
                            "path": str(second_root),
                            "label": "Music Two",
                            "policy": "automatic",
                            "rules": [],
                        },
                    ]
                }
            }
        ),
        encoding="utf-8",
    )
    preferences = PreferencesService(settings)
    return preferences, "root-1"


def _operation(*, state: str = "ready") -> dict:
    return {
        "id": "job-1",
        "kind": "library_management",
        "state": state,
        "row_revision": 2,
        "event_revision": 1,
        "terminal_code": None,
        "expected_work_count": 0,
        "completed_count": 0,
        "succeeded_count": 0,
        "failed_count": 0,
        "skipped_count": 0,
        "control_request": "none",
        "created_at": 90.0,
        "updated_at": 100.0,
    }


def _service_fixture(
    tmp_path: Path,
    *,
    clock: float = 100.0,
    proposed_settings_revision: str | None = None,
) -> tuple[
    LibraryManagementPreviewService,
    AsyncMock,
    PreferencesService,
    LibraryManagementJobSnapshot,
]:
    preferences, _root_id = _preferences(tmp_path)
    profiles = LibraryManagementProfileService(preferences)
    current = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in current.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    policy_revision = LibraryPolicyResolver(
        preferences.get_typed_library_settings_raw()
    ).policy_revision
    selection = NormalizedLibraryManagementSelection(
        kind="roots",
        ids=("root-1",),
        root_scopes=(LibraryManagementRootScope(root_id="root-1"),),
        expand_album_bundles=True,
    )
    token = "activation-token"
    snapshot = LibraryManagementJobSnapshot(
        job_id="job-1",
        mode="preview",
        origin="manual",
        phase="ready",
        selection_json=_json(selection),
        profile_revision=profile.revision,
        settings_revision=settings_revision(current),
        proposed_settings_revision=proposed_settings_revision,
        naming_revision=current.naming_scripts[0].revision,
        policy_revision=policy_revision,
        catalog_revision=0,
        profile_snapshot_json=_json(
            LibraryManagementPlanner.pin_profile(current, profile)
        ),
        preview_token_hash=hashlib.sha256(token.encode()).hexdigest(),
        preview_created_at=90.0,
        preview_expires_at=200.0,
        summary_json="{}",
        created_at=90.0,
        updated_at=100.0,
    )
    store = AsyncMock(spec=NativeLibraryStore)
    store.get_library_management_job_snapshot.return_value = snapshot
    store.get_operation_job.return_value = _operation()
    store.get_management_operation_recovery_availability.return_value = {
        "snapshot_count": 2,
        "undo_available_count": 2,
        "undo_expired_count": 0,
        "undo_expires_at": 300.0,
        "baseline_available_count": 2,
    }
    store.get_catalog_revision.return_value = 0
    store.list_library_management_plan_items.return_value = []
    store.list_library_management_external_refreshes.return_value = []
    planner = AsyncMock(spec=LibraryManagementPlanner)
    planner.pin_profile.side_effect = LibraryManagementPlanner.pin_profile
    service = LibraryManagementPreviewService(
        store,
        preferences,
        profiles,
        planner,
        clock=lambda: clock,
    )
    return service, store, preferences, snapshot


@pytest.mark.asyncio
async def test_manual_preview_pins_temporary_profile_overrides(
    tmp_path: Path,
) -> None:
    service, _store, preferences, _snapshot = _service_fixture(tmp_path)
    planner = service._planner
    planner.create_preview.return_value = LibraryManagementPreviewHandle(
        job_id="manual-preview",
        preview_token="manual-token",
        created_at=100.0,
        expires_at=200.0,
    )
    settings = preferences.get_library_management_settings()
    policy_revision = LibraryPolicyResolver(
        preferences.get_typed_library_settings_raw()
    ).policy_revision

    response = await service.create_manual(
        LibraryManagementPreviewCreateRequest(
            selection=LibraryManagementSelectionRequest(kind="tracks", ids=["track-1"]),
            profile_id=PICARD_ORGANIZER_PROFILE_ID,
            expected_settings_revision=settings.settings_revision,
            expected_policy_revision=policy_revision,
            overrides=LibraryManagementRootOverrides(
                metadata_enabled=False,
                move_enabled=False,
                move_sidecars=False,
            ),
        ),
        "admin",
    )

    assert response.job_id == "manual-preview"
    effective = planner.create_preview.await_args.kwargs["effective_profile"]
    assert effective.metadata.enabled is False
    assert effective.organization.move_enabled is False
    assert effective.organization.move_sidecars is False
    original = next(
        profile
        for profile in preferences.get_library_management_settings_raw().profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    assert original.metadata.enabled is True
    assert original.organization.move_enabled is True


@pytest.mark.asyncio
async def test_tag_edit_preview_normalizes_lists_and_expands_album_scope(
    tmp_path: Path,
) -> None:
    service, store, preferences, _snapshot = _service_fixture(tmp_path)
    root = Path(preferences.get_typed_library_settings_raw().library_roots[0].path)
    store.get_library_management_tag_editor_subject.return_value = {
        "id": "track-1",
        "local_album_id": "album-1",
        "root_id": "root-1",
        "relative_path": "track.flac",
        "file_path": str(root / "track.flac"),
        "availability": "indexed",
        "track_revision": 3,
        "album_revision": 5,
    }
    existing = LibraryManagementOverride(
        id="override-1",
        subject_kind="album",
        local_album_id="album-1",
        field_name="album",
        value_json='"Old album"',
        mode="replace",
        subject_revision=5,
        row_revision=2,
    )
    store.list_management_overrides.side_effect = [
        ([existing], "album-rev"),
        ([], "track-rev"),
    ]
    service._planner.create_preview.return_value = LibraryManagementPreviewHandle(
        job_id="tag-preview",
        preview_token="tag-token",
        created_at=100.0,
        expires_at=200.0,
    )
    settings = preferences.get_library_management_settings()
    policy_revision = LibraryPolicyResolver(
        preferences.get_typed_library_settings_raw()
    ).policy_revision

    response = await service.create_tag_edit(
        LibraryManagementTagEditPreviewRequest(
            local_track_id="track-1",
            mode="save_override",
            expected_settings_revision=settings.settings_revision,
            expected_policy_revision=policy_revision,
            fields=[
                LibraryManagementTagEditFieldRequest(
                    field_name="artist", value=["Alpha", "Beta", "alpha"]
                ),
                LibraryManagementTagEditFieldRequest(
                    field_name="album", value="New album"
                ),
            ],
        ),
        "admin",
    )

    assert response.job_id == "tag-preview"
    call = service._planner.create_preview.await_args.kwargs
    assert call["force_expand_album_bundles"] is True
    assert call["effective_profile"].organization.move_enabled is False
    assert call["effective_profile"].artwork.embedded_enabled is False
    intent = call["tag_edit_intent"]
    assert intent.fields[0].value == ("Alpha", "Beta")
    assert intent.fields[1].override_id == "override-1"
    assert intent.fields[1].expected_override_row_revision == 2


@pytest.mark.asyncio
async def test_tag_edit_reset_requires_an_existing_override(tmp_path: Path) -> None:
    service, store, preferences, _snapshot = _service_fixture(tmp_path)
    root = Path(preferences.get_typed_library_settings_raw().library_roots[0].path)
    store.get_library_management_tag_editor_subject.return_value = {
        "id": "track-1",
        "local_album_id": "album-1",
        "root_id": "root-1",
        "relative_path": "track.flac",
        "file_path": str(root / "track.flac"),
        "availability": "indexed",
        "track_revision": 3,
        "album_revision": 5,
    }
    store.list_management_overrides.return_value = ([], "revision")
    settings = preferences.get_library_management_settings()
    policy_revision = LibraryPolicyResolver(
        preferences.get_typed_library_settings_raw()
    ).policy_revision

    with pytest.raises(ValidationError, match="has no local override"):
        await service.create_tag_edit(
            LibraryManagementTagEditPreviewRequest(
                local_track_id="track-1",
                mode="reset_canonical",
                expected_settings_revision=settings.settings_revision,
                expected_policy_revision=policy_revision,
                fields=[LibraryManagementTagEditFieldRequest(field_name="genre")],
            ),
            "admin",
        )


@pytest.mark.asyncio
async def test_tag_editor_context_exposes_lists_and_mapping_gate(
    tmp_path: Path,
) -> None:
    preferences, _root_id = _preferences(tmp_path)
    profiles = LibraryManagementProfileService(preferences)
    root = Path(preferences.get_typed_library_settings_raw().library_roots[0].path)
    path = root / "track.flac"
    path.write_bytes(b"fixture")
    store = AsyncMock(spec=NativeLibraryStore)
    store.get_library_management_tag_editor_subject.return_value = {
        "id": "track-1",
        "local_album_id": "album-1",
        "root_id": "root-1",
        "relative_path": "track.flac",
        "file_path": str(path),
        "availability": "indexed",
        "track_revision": 3,
        "album_revision": 5,
    }
    store.list_management_overrides.return_value = ([], "revision")
    store.get_accepted_library_management_identity.return_value = (
        AcceptedAlbumManagementIdentity(
            local_album_id="album-1",
            album_revision=5,
            identity_revision=2,
            release_group_mbid="rg-1",
            release_mbid="release-1",
            tracks=(
                AcceptedTrackManagementIdentity(
                    local_track_id="track-1",
                    track_revision=3,
                    identity_revision=4,
                    recording_mbid="recording-1",
                    release_mbid="release-1",
                    release_track_mbid="release-track-1",
                    medium_position=1,
                    release_track_position=1,
                ),
            ),
        )
    )
    audio = MagicMock()
    audio.read.return_value = SimpleNamespace(
        metadata=AudioMetadataDocument(
            fields=(
                AudioSemanticField(name="artist", value=("Alpha", "Beta")),
                AudioSemanticField(name="genre", value=("Rock", "Art Rock")),
            ),
            artist_display="Alpha & Beta",
        )
    )
    service = LibraryManagementPreviewService(
        store,
        preferences,
        profiles,
        AsyncMock(spec=LibraryManagementPlanner),
        audio,
    )

    response = await service.tag_editor_context("track-1")

    assert response.accepted_identity is True
    by_name = {value.field_name: value for value in response.fields}
    assert by_name["artist"].current_value == ["Alpha", "Beta"]
    assert by_name["genre"].current_value == ["Rock", "Art Rock"]


@pytest.mark.asyncio
async def test_detail_reports_expiry_and_current_staleness(tmp_path: Path) -> None:
    service, store, preferences, snapshot = _service_fixture(tmp_path)
    store.list_library_management_external_refreshes.return_value = [
        LibraryManagementExternalRefreshDelivery(
            id="delivery-1",
            operation_job_id="job-1",
            target="jellyfin",
            state="succeeded",
            attempts=1,
            max_attempts=4,
            retry_delay_seconds=30,
            created_at=90,
            updated_at=100,
            completed_at=100,
        )
    ]

    detail = await service.detail("job-1")

    assert detail.ready_for_confirmation is True
    assert detail.expired is False
    assert detail.stale_reasons == []
    assert detail.external_refreshes[0].target == "jellyfin"
    assert detail.external_refreshes[0].state == "succeeded"
    assert detail.undo_available_count == 2
    assert detail.undo_expired_count == 0
    assert detail.undo_expires_at == 300.0
    assert detail.baseline_available_count == 2
    store.get_management_operation_recovery_availability.assert_awaited_with(
        "job-1", now=100.0
    )
    assert not hasattr(detail, "preview_token_hash")

    changed = preferences.get_library_management_settings_raw()
    changed.undo_retention_days += 1
    current = preferences.get_library_management_settings()
    preferences.save_library_management_settings_if_current(
        changed, expected_settings_revision=current.settings_revision
    )
    stale = await service.detail("job-1")
    assert stale.stale is True
    assert stale.stale_reasons == ["PROFILE_CHANGED"]

    snapshot.settings_revision = settings_revision(
        preferences.get_library_management_settings_raw()
    )
    store.get_catalog_revision.return_value = 1
    changed_catalog = await service.detail("job-1")
    assert changed_catalog.stale_reasons == ["FILE_CHANGED"]

    store.get_catalog_revision.return_value = 0
    snapshot.preview_expires_at = 99.0
    store.get_library_management_job_snapshot.return_value = snapshot
    expired = await service.detail("job-1")
    assert expired.expired is True
    assert expired.ready_for_confirmation is False


@pytest.mark.asyncio
async def test_item_page_never_exposes_absolute_source_identity(tmp_path: Path) -> None:
    service, store, _preferences, _snapshot = _service_fixture(tmp_path)
    store.list_library_management_plan_items.return_value = [
        LibraryManagementPlanItem(
            job_id="job-1",
            ordinal=0,
            bundle_ordinal=0,
            expected_catalog_revision=0,
            expected_policy_revision="policy",
            expected_profile_revision="profile",
            expected_root_id="root-1",
            expected_relative_path="Album/track.flac",
            expected_stat_revision="1:2",
            expected_tag_revision="tag",
            expected_file_fingerprint="fingerprint",
            source_path_identity="/secret/library/Album/track.flac",
            desired_document_json='{"fields":[]}',
            desired_document_hash=hashlib.sha256(b"{}").hexdigest(),
            eligibility="eligible",
            created_at=1.0,
            destination_root_id="root-1",
            destination_relative_path="Artist/Album/track.flac",
        )
    ]

    page = await service.items(
        "job-1",
        after_ordinal=-1,
        limit=10,
        eligibility=None,
        reason_code=None,
        root_id=None,
        artist_id=None,
        album_id=None,
        audio_format=None,
        collision_class=None,
        has_preserved_value=None,
        has_representation_loss=None,
        change_kind=None,
    )
    payload = msgspec.json.encode(page).decode()

    assert page.items[0].source_relative_path == "Album/track.flac"
    assert "/secret/library" not in payload
    assert "source_path_identity" not in payload
    store.list_library_management_plan_items.assert_awaited_once_with(
        "job-1",
        after_ordinal=-1,
        limit=11,
        eligibility=None,
        reason_code=None,
        root_id=None,
        artist_id=None,
        album_id=None,
        audio_format=None,
        collision_class=None,
        has_preserved_value=None,
        has_representation_loss=None,
        change_kind=None,
    )


@pytest.mark.asyncio
async def test_artwork_preview_serves_only_a_blob_referenced_by_the_item(
    tmp_path: Path,
) -> None:
    service, store, _preferences, _snapshot = _service_fixture(tmp_path)
    sha256 = hashlib.sha256(b"pinned-artwork").hexdigest()
    store.get_library_management_plan_item.return_value = LibraryManagementPlanItem(
        job_id="job-1",
        ordinal=2,
        bundle_ordinal=0,
        expected_catalog_revision=0,
        expected_policy_revision="policy",
        expected_profile_revision="profile",
        expected_root_id="root-1",
        expected_relative_path="Album/track.flac",
        expected_stat_revision="1:2",
        expected_tag_revision="tag",
        expected_file_fingerprint="fingerprint",
        source_path_identity="source-identity",
        desired_document_json='{"fields":[]}',
        desired_document_hash=hashlib.sha256(b"{}").hexdigest(),
        eligibility="eligible",
        created_at=1.0,
        artwork_choices_json=json.dumps(
            [{"blob_sha256": sha256, "mime_type": "image/png"}]
        ),
    )
    blobs = AsyncMock()
    blobs.read_bytes.return_value = b"pinned-artwork"
    service._blobs = blobs

    content, mime_type = await service.artwork_preview("job-1", 2, sha256)

    assert content == b"pinned-artwork"
    assert mime_type == "image/png"
    blobs.read_bytes.assert_awaited_once_with(sha256)

    with pytest.raises(ResourceNotFoundError):
        await service.artwork_preview("job-1", 2, "0" * 64)
    store.get_library_management_plan_item.return_value.artwork_choices_json = (
        json.dumps([{"blob_sha256": sha256, "mime_type": "image/svg+xml"}])
    )
    with pytest.raises(ResourceNotFoundError):
        await service.artwork_preview("job-1", 2, sha256)
    blobs.read_bytes.assert_awaited_once()


@pytest.mark.asyncio
async def test_apply_requires_exact_current_token_and_is_idempotent(
    tmp_path: Path,
) -> None:
    service, store, _preferences, snapshot = _service_fixture(tmp_path)
    store.begin_library_management_apply.return_value = _operation(state="queued")
    request = LibraryManagementApplyRequest(
        preview_token="activation-token",
        expected_operation_row_revision=2,
        idempotency_key="apply-once",
        confirmation=True,
    )

    started = await service.apply("job-1", request)

    assert started.state == "queued"
    store.begin_library_management_apply.assert_awaited_once_with(
        "job-1",
        preview_token_hash=snapshot.preview_token_hash,
        expected_job_revision=2,
        idempotency_key="apply-once",
        now=100.0,
    )

    snapshot.mode = "apply"
    snapshot.phase = "applying"
    snapshot.apply_idempotency_key = "apply-once"
    store.begin_library_management_apply.reset_mock()
    repeated = await service.apply("job-1", request)
    assert repeated.state == "queued"
    store.begin_library_management_apply.assert_awaited_once()


@pytest.mark.asyncio
async def test_apply_rejects_missing_confirmation_and_wrong_token(
    tmp_path: Path,
) -> None:
    service, store, _preferences, _snapshot = _service_fixture(tmp_path)
    with pytest.raises(ValidationError, match="Confirm Apply"):
        await service.apply(
            "job-1",
            LibraryManagementApplyRequest(
                preview_token="activation-token",
                expected_operation_row_revision=2,
                idempotency_key="apply-once",
            ),
        )
    with pytest.raises(ValidationError, match="token is invalid"):
        await service.apply(
            "job-1",
            LibraryManagementApplyRequest(
                preview_token="wrong",
                expected_operation_row_revision=2,
                idempotency_key="apply-once",
                confirmation=True,
            ),
        )
    store.begin_library_management_apply.assert_not_awaited()


@pytest.mark.asyncio
async def test_discard_cancels_exact_ready_preview_and_returns_its_audit_detail(
    tmp_path: Path,
) -> None:
    service, store, _preferences, _snapshot = _service_fixture(tmp_path)
    store.get_operation_job.return_value = {
        **_operation(state="cancelled"),
        "row_revision": 3,
        "event_revision": 2,
        "terminal_code": "PREVIEW_DISCARDED",
    }

    detail = await service.discard(
        "job-1",
        LibraryManagementDiscardRequest(expected_operation_row_revision=2),
    )

    store.discard_library_management_preview.assert_awaited_once_with(
        "job-1", expected_job_revision=2, now=100.0
    )
    assert detail.state == "cancelled"
    assert detail.terminal_code == "PREVIEW_DISCARDED"
    assert detail.ready_for_confirmation is False


@pytest.mark.asyncio
async def test_activation_confirmation_binds_exact_proposed_settings_and_token(
    tmp_path: Path,
) -> None:
    service, store, preferences, snapshot = _service_fixture(tmp_path)
    proposed = preferences.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(
            root_id="root-1", enabled=True, automatic_acquisitions=True
        )
    ]
    normalized, _assignment, effective, _policy = service._profiles.prepare_activation(
        proposed,
        root_id="root-1",
        expected_settings_revision=snapshot.settings_revision,
    )
    snapshot.proposed_settings_revision = settings_revision(normalized)
    snapshot.profile_revision = effective.revision
    snapshot.profile_snapshot_json = _json(
        LibraryManagementPlanner.pin_profile(normalized, effective)
    )
    store.get_library_management_job_snapshot.return_value = snapshot
    request = LibraryManagementActivationConfirmRequest(
        settings=proposed,
        proofs=[
            LibraryManagementActivationProof(
                root_id="root-1",
                job_id="job-1",
                preview_token="activation-token",
            )
        ],
        expected_settings_revision=snapshot.settings_revision,
        confirmation=True,
    )

    saved = await service.confirm_activation(request)

    assignment = saved.root_assignments[0]
    assert assignment.enabled is True
    assert assignment.activation_preview_token == "activation-token"
    assert assignment.activation_preview_hash == snapshot.preview_token_hash
    assert assignment.activation_settings_revision == snapshot.settings_revision


@pytest.mark.asyncio
async def test_activation_confirmation_rejects_changed_proposal_and_bad_token(
    tmp_path: Path,
) -> None:
    service, store, preferences, snapshot = _service_fixture(tmp_path)
    proposed = preferences.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(
            root_id="root-1", enabled=True, automatic_acquisitions=True
        )
    ]
    normalized, _assignment, effective, _policy = service._profiles.prepare_activation(
        proposed,
        root_id="root-1",
        expected_settings_revision=snapshot.settings_revision,
    )
    snapshot.proposed_settings_revision = settings_revision(normalized)
    snapshot.profile_revision = effective.revision
    snapshot.profile_snapshot_json = _json(
        LibraryManagementPlanner.pin_profile(normalized, effective)
    )
    store.get_library_management_job_snapshot.return_value = snapshot

    bad_token = LibraryManagementActivationConfirmRequest(
        settings=proposed,
        proofs=[
            LibraryManagementActivationProof(
                root_id="root-1", job_id="job-1", preview_token="wrong"
            )
        ],
        expected_settings_revision=snapshot.settings_revision,
        confirmation=True,
    )
    with pytest.raises(ValidationError, match="token is invalid"):
        await service.confirm_activation(bad_token)

    changed = msgspec.convert(msgspec.to_builtins(proposed), type=type(proposed))
    changed.preview_retention_hours += 1
    changed_request = LibraryManagementActivationConfirmRequest(
        settings=changed,
        proofs=[
            LibraryManagementActivationProof(
                root_id="root-1",
                job_id="job-1",
                preview_token="activation-token",
            )
        ],
        expected_settings_revision=snapshot.settings_revision,
        confirmation=True,
    )
    with pytest.raises(StaleRevisionError, match="proposed profile"):
        await service.confirm_activation(changed_request)


@pytest.mark.asyncio
async def test_activation_confirmation_validates_all_roots_before_saving(
    tmp_path: Path,
) -> None:
    service, store, preferences, first = _service_fixture(tmp_path)
    proposed = preferences.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(
            root_id=root_id, enabled=True, automatic_acquisitions=True
        )
        for root_id in ("root-1", "root-2")
    ]
    normalized, _assignment, effective, _policy = service._profiles.prepare_activation(
        proposed,
        root_id="root-1",
        expected_settings_revision=first.settings_revision,
    )
    proposed_revision = settings_revision(normalized)
    pinned = _json(LibraryManagementPlanner.pin_profile(normalized, effective))
    first.proposed_settings_revision = proposed_revision
    first.profile_revision = effective.revision
    first.profile_snapshot_json = pinned
    second = msgspec.convert(
        msgspec.to_builtins(first), type=LibraryManagementJobSnapshot
    )
    second.job_id = "job-2"
    second.selection_json = _json(
        NormalizedLibraryManagementSelection(
            kind="roots",
            ids=("root-2",),
            root_scopes=(LibraryManagementRootScope(root_id="root-2"),),
            expand_album_bundles=True,
        )
    )
    second.preview_token_hash = hashlib.sha256(b"second-token").hexdigest()
    snapshots = {"job-1": first, "job-2": second}
    store.get_library_management_job_snapshot.side_effect = snapshots.get
    store.get_operation_job.side_effect = lambda job_id: {
        **_operation(),
        "id": job_id,
    }

    saved = await service.confirm_activation(
        LibraryManagementActivationConfirmRequest(
            settings=proposed,
            proofs=[
                LibraryManagementActivationProof(
                    root_id="root-1",
                    job_id="job-1",
                    preview_token="activation-token",
                ),
                LibraryManagementActivationProof(
                    root_id="root-2",
                    job_id="job-2",
                    preview_token="second-token",
                ),
            ],
            expected_settings_revision=first.settings_revision,
            confirmation=True,
        )
    )

    assert [value.root_id for value in saved.root_assignments] == ["root-1", "root-2"]
    assert all(
        value.activation_confirmed_at == 100.0 for value in saved.root_assignments
    )
