import sqlite3
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from api.v1.schemas.library_management import (
    PICARD_ORGANIZER_PROFILE_ID,
    LibraryManagementRootAssignment,
    profile_revision,
)
from infrastructure.audio.metadata_engine import AudioMetadataEngine
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.automatic_scan_management_service import (
    AutomaticScanManagementService,
)
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.identification_revisions import album_input_revisions
from services.native.library_management_baseline_service import (
    LibraryManagementBaselineService,
)
from services.native.library_management_duplicate_service import (
    LibraryManagementDuplicateService,
)
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.library_management_publisher import LibraryManagementPublisher
from services.native.library_management_undo_service import LibraryManagementUndoService
from services.native.library_management_worker import LibraryManagementWorker
from tests.services.native.test_library_management_planner import _configured, _planner


def _activate_scan(preferences, policy_revision: str) -> None:  # noqa: ANN001
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    settings.root_assignments = [
        LibraryManagementRootAssignment(
            root_id="root-1",
            profile_id=profile.id,
            enabled=True,
            automatic_scan_discovered=True,
            activation_profile_revision=profile_revision(profile),
            activation_policy_revision=policy_revision,
            activation_settings_revision=current.settings_revision,
            activation_preview_token="confirmed",
            activation_preview_hash="confirmed-hash",
            activation_confirmed_at=100.0,
        )
    ]
    preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )


def _record_applied_policy(database: Path, policy_revision: str) -> None:
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE local_tracks SET applied_policy_revision=?, "
            "applied_policy='automatic'",
            (policy_revision,),
        )


@pytest.mark.asyncio
async def test_scan_trigger_is_independent_and_deduplicates_exact_input(
    tmp_path: Path,
) -> None:
    _root, _source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _record_applied_policy(tmp_path / "library.db", policy_revision)
    planner = _planner(tmp_path, store, preferences)
    service = AutomaticScanManagementService(
        store, LibraryManagementProfileService(preferences), planner
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    input_policy_revision = album_input_revisions(context["tracks"])[2]

    assert (
        await service.schedule_identified_album("album-1", input_policy_revision)
        is None
    )

    _activate_scan(preferences, policy_revision)
    assert await service.schedule_identified_album("album-1", "stale-input") is None
    first = await service.schedule_identified_album("album-1", input_policy_revision)
    second = await service.schedule_identified_album("album-1", input_policy_revision)

    assert first is not None and second == first
    operation = await store.get_operation_job(first)
    snapshot = await store.get_library_management_job_snapshot(first)
    assert operation is not None and operation["requested_by_user_id"] is None
    assert snapshot is not None
    assert snapshot.origin == "scan_discovered"
    assert snapshot.mode == "preview"
    assert snapshot.phase == "planning"


@pytest.mark.asyncio
async def test_scan_trigger_waits_for_every_release_track_mapping(
    tmp_path: Path,
) -> None:
    _root, _source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _record_applied_policy(tmp_path / "library.db", policy_revision)
    _activate_scan(preferences, policy_revision)
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET release_track_mbid=NULL "
            "WHERE local_track_id='track-1'"
        )
    service = AutomaticScanManagementService(
        store,
        LibraryManagementProfileService(preferences),
        _planner(tmp_path, store, preferences),
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    input_policy_revision = album_input_revisions(context["tracks"])[2]

    assert (
        await service.schedule_identified_album("album-1", input_policy_revision)
        is None
    )


@pytest.mark.asyncio
async def test_baseline_restore_suppresses_scan_until_fresh_activation(
    tmp_path: Path,
) -> None:
    _root, _source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _record_applied_policy(tmp_path / "library.db", policy_revision)
    _activate_scan(preferences, policy_revision)
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "INSERT INTO library_track_management_state "
            "(local_track_id,last_managed_at,last_outcome,row_revision) "
            "VALUES ('track-1',110.0,'restored',1)"
        )
    service = AutomaticScanManagementService(
        store,
        LibraryManagementProfileService(preferences),
        _planner(tmp_path, store, preferences),
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    input_policy_revision = album_input_revisions(context["tracks"])[2]

    assert (
        await service.schedule_identified_album("album-1", input_policy_revision)
        is None
    )

    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    settings.root_assignments[0].activation_confirmed_at = 120.0
    settings.root_assignments[
        0
    ].activation_settings_revision = current.settings_revision
    preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )

    assert (
        await service.schedule_identified_album("album-1", input_policy_revision)
        is not None
    )


@pytest.mark.asyncio
async def test_scan_preview_seals_directly_into_durable_automatic_apply(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "services.native.library_management_worker.time.time", lambda: 100.0
    )
    _root, _source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _record_applied_policy(tmp_path / "library.db", policy_revision)
    _activate_scan(preferences, policy_revision)
    planner = _planner(tmp_path, store, preferences)
    service = AutomaticScanManagementService(
        store, LibraryManagementProfileService(preferences), planner
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    input_policy_revision = album_input_revisions(context["tracks"])[2]
    job_id = await service.schedule_identified_album("album-1", input_policy_revision)
    assert job_id is not None
    claimed = await store.claim_operation_job(
        "management-worker", now=100.0, lease_seconds=60.0, kind="library_management"
    )
    assert claimed is not None
    worker = LibraryManagementWorker(
        store,
        planner,
        AsyncMock(spec=LibraryManagementPublisher),
        AsyncMock(spec=LibraryManagementUndoService),
        AsyncMock(spec=LibraryManagementBaselineService),
        AsyncMock(spec=LibraryManagementDuplicateService),
    )

    operation = await worker.run_claimed(claimed, "management-worker")
    snapshot = await store.get_library_management_job_snapshot(job_id)

    assert operation["state"] == "queued"
    assert snapshot is not None
    assert snapshot.mode == "automatic_apply"
    assert snapshot.origin == "scan_discovered"
    assert snapshot.phase == "applying"

    audio = AudioMetadataEngine()
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "scan-blobs", store),
        LibraryFilesystemCoordinator(),
        clock=lambda: 100.0,
    )
    apply_worker = LibraryManagementWorker(
        store,
        planner,
        publisher,
        AsyncMock(spec=LibraryManagementUndoService),
        AsyncMock(spec=LibraryManagementBaselineService),
        AsyncMock(spec=LibraryManagementDuplicateService),
    )
    claimed_apply = await store.claim_operation_job(
        "management-worker", now=101.0, lease_seconds=60.0, kind="library_management"
    )
    assert claimed_apply is not None
    completed = await apply_worker.run_claimed(claimed_apply, "management-worker")
    assert completed["state"] == "succeeded"

    managed_context = await store.get_album_identification_context("album-1")
    assert managed_context is not None
    managed_input_revision = album_input_revisions(managed_context["tracks"])[2]
    assert (
        await service.schedule_identified_album("album-1", managed_input_revision)
        is None
    )

    managed_path = Path(managed_context["tracks"][0]["file_path"])
    with managed_path.open("ab") as output:
        output.write(b"externally-changed")
    assert (
        await service.schedule_identified_album("album-1", managed_input_revision)
        is not None
    )
