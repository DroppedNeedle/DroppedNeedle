import asyncio
import os
import shutil
import sqlite3
from pathlib import Path
from unittest.mock import AsyncMock

import hashlib
import msgspec
import pytest

from services.native.library_management_recovery_service import (
    LibraryManagementRecoveryService,
    _JournalPaths,
)
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from tests.services.native.test_library_management_publisher import (
    _ArtworkRepository,
    _add_second_album_track,
    _add_second_canonical_track,
    _external_artwork_configuration,
    _keep_source_configuration,
    _ready_apply_operation,
    _same_path_configuration,
    _sidecar_configuration,
    _update_profile,
    _import_file,
    _import_publication_fixture,
)
from models.library_management_planning import LibraryManagementSelection
from models.library_management import (
    LibraryFileMutationJournal,
    LibraryManagementImportBundle,
)


def _recovery(publisher, store) -> LibraryManagementRecoveryService:
    return LibraryManagementRecoveryService(
        store,
        publisher,
        publisher._filesystem,
        clock=lambda: 120.0,
    )


async def _prepare_bundle(publisher, store, job_id: str):
    snapshot = await store.get_library_management_job_snapshot(job_id)
    assert snapshot is not None
    pinned, roots = publisher.recovery_configuration(snapshot)
    items = await store.get_library_management_bundle_plan_items(job_id, 0)
    prepared = []
    for item in items:
        prepared.extend(
            await publisher._prepare_plan_item(snapshot, pinned, item, roots, 0)
        )
    return prepared


@pytest.mark.asyncio
async def test_startup_recovery_rolls_back_import_interrupted_after_publish(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, publisher, _service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "interrupted-import.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Interrupted.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:startup-recovery:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    class SimulatedProcessStop(BaseException):
        pass

    async def stop_after_replace(value):
        await asyncio.to_thread(os.replace, value.temporary, value.destination)
        raise SimulatedProcessStop

    rollback = publisher._rollback_import_bundle
    publisher._publish_import_file = stop_after_replace
    publisher._rollback_import_bundle = AsyncMock(side_effect=SimulatedProcessStop)
    with pytest.raises(SimulatedProcessStop):
        await publisher.publish_import_bundle(bundle, AsyncMock())

    publisher._rollback_import_bundle = rollback
    result = await _recovery(publisher, store).recover_startup()

    remaining = await store.list_recoverable_library_management_import_bundles(
        limit=10, include_committed_cleanup=False
    )
    with sqlite3.connect(store.db_path) as connection:
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    journals = await store.list_library_management_import_journals(bundle_id)
    assert result.rolled_back_bundles == 1
    assert incoming.is_file()
    assert not (root / request.destination_relative_path).exists()
    assert remaining == []
    assert [journal.state for journal in journals] == ["rolled_back"]


@pytest.mark.parametrize("journal_state", ["validated", "source_backed_up"])
@pytest.mark.asyncio
async def test_recovery_never_republishes_an_interrupted_delete(
    tmp_path: Path, journal_state: str
) -> None:
    content = b"generated artwork"
    fingerprint = hashlib.sha256(content).hexdigest()
    source = tmp_path / "cover.jpg"
    temporary = tmp_path / ".delete-temp.jpg"
    backup = tmp_path / ".delete-backup.jpg"
    temporary.write_bytes(content)
    backup.write_bytes(content)
    journal = LibraryFileMutationJournal(
        id="delete-journal",
        job_id="undo-job",
        plan_item_ordinal=0,
        subject_kind="external_art",
        subject_key="delete:cover.jpg",
        source_root_id="root-1",
        source_relative_path="cover.jpg",
        temporary_root_id="root-1",
        temporary_relative_path=temporary.name,
        backup_root_id="root-1",
        backup_relative_path=backup.name,
        destination_root_id="root-1",
        destination_relative_path="cover.jpg",
        source_fingerprint=fingerprint,
        staged_fingerprint=fingerprint,
        recovery_evidence_json='{"mutation":"delete"}',
        state=journal_state,
        created_at=1.0,
        updated_at=1.0,
    )
    store = AsyncMock()

    async def transition(_journal_id: str, **values):
        nonlocal journal
        journal = msgspec.structs.replace(
            journal,
            state=values["new_state"],
            row_revision=journal.row_revision + 1,
        )
        return journal

    store.transition_file_mutation_journal.side_effect = transition
    service = LibraryManagementRecoveryService(
        store,
        AsyncMock(),
        LibraryFilesystemCoordinator(),
        clock=lambda: 2.0,
    )

    recovered = await service._publish_remaining(
        [
            _JournalPaths(
                journal=journal,
                source=source,
                temporary=temporary,
                backup=backup,
                destination=source,
            )
        ]
    )

    assert recovered[0].journal.state == "published"
    assert source.exists() is False
    assert temporary.read_bytes() == content
    assert backup.read_bytes() == content


@pytest.mark.asyncio
async def test_recovery_resumes_validated_bundle_and_second_run_is_noop(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    await _prepare_bundle(publisher, store, job_id)
    service = _recovery(publisher, store)

    first = await service.recover_startup()
    second = await service.recover_startup()

    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert first.recovered_bundles == 1
    assert second.examined_bundles == 0
    assert source.exists() is False
    assert row is not None and (root / str(row["relative_path"])).is_file()
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.parametrize(
    ("journal_state", "corrupt_temp"),
    [("planned", True), ("snapshot_saved", True), ("staged", False)],
)
@pytest.mark.asyncio
async def test_recovery_restages_owned_prepublication_temp(
    tmp_path: Path, journal_state: str, corrupt_temp: bool
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    value = prepared[0]
    if corrupt_temp:
        value.temporary.write_bytes(b"interrupted staged write")
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_file_mutation_journal SET state=? WHERE id=?",
            (journal_state, value.journal.id),
        )

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.exists() is False
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_recovery_finishes_same_path_after_source_backup(tmp_path: Path) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, configure=_same_path_configuration
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    value = prepared[0]
    assert value.backup is not None
    os.replace(value.source, value.backup)
    await store.transition_file_mutation_journal(
        value.journal.id,
        expected_state="validated",
        new_state="source_backed_up",
        expected_row_revision=value.journal.row_revision,
        updated_at=111,
    )

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.is_file()
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_recovery_finishes_move_after_publish_before_journal_transition(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    value = prepared[0]
    os.replace(value.temporary, value.destination)

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.exists() is False
    assert value.destination.is_file()
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_recovery_defers_repeated_cancellation_until_commit_is_durable() -> None:
    started = asyncio.Event()
    release = asyncio.Event()

    async def critical():
        started.set()
        await release.wait()
        return "recovered"

    critical_task = asyncio.create_task(critical())
    task = asyncio.create_task(
        LibraryManagementRecoveryService._finish_critical_task(critical_task)
    )
    await started.wait()
    task.cancel()
    await asyncio.sleep(0)
    task.cancel()
    await asyncio.sleep(0)
    release.set()

    result, cancelled = await task

    assert result == "recovered"
    assert cancelled is True


@pytest.mark.asyncio
async def test_recovery_finishes_same_path_after_backup_before_journal_transition(
    tmp_path: Path,
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, configure=_same_path_configuration
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    value = prepared[0]
    assert value.backup is not None
    os.replace(value.source, value.backup)

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.is_file()
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_recovery_finishes_partially_published_album_bundle(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        prepare_store=_add_second_album_track,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    await publisher._publish_one(prepared[0])

    result = await _recovery(publisher, store).recover_startup()

    first = await store.get_target_track("track-1")
    second = await store.get_target_track("track-2")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.exists() is False
    assert first is not None and (root / str(first["relative_path"])).is_file()
    assert second is not None and (root / str(second["relative_path"])).is_file()
    assert all(journal.state == "completed" for journal in journals)


@pytest.mark.asyncio
async def test_recovery_finishes_partial_audio_artwork_and_sidecar_bundle(
    tmp_path: Path,
) -> None:
    artwork = _ArtworkRepository()

    def configure(root, preferences, store) -> None:
        _sidecar_configuration(root, preferences, store)
        _external_artwork_configuration(root, preferences, store)

    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        configure=configure,
        artwork_repository=artwork,
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    audio = next(value for value in prepared if value.journal.subject_kind == "audio")
    await publisher._publish_one(audio)

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.exists() is False
    assert (root / "disc.cue").exists() is False
    assert {journal.subject_kind for journal in journals} == {
        "audio",
        "external_art",
        "sidecar",
    }
    assert all(journal.state == "completed" for journal in journals)


@pytest.mark.asyncio
async def test_recovery_marks_changed_published_destination_attention_without_deletion(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    await publisher._publish_one(prepared[0])
    destination = prepared[0].destination
    destination.write_bytes(b"third-party replacement")

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.needs_attention_bundles == 1
    assert source.is_file()
    assert destination.read_bytes() == b"third-party replacement"
    assert journals[0].state == "needs_attention"
    assert journals[0].failure_code == "RECOVERY_DESTINATION_CHANGED"
    assert not list(root.rglob("*.deleted"))


@pytest.mark.asyncio
async def test_recovery_marks_duplicate_staged_fingerprint_attention(
    tmp_path: Path,
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    value = prepared[0]
    value.destination.parent.mkdir(parents=True, exist_ok=True)
    value.destination.write_bytes(value.temporary.read_bytes())

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.needs_attention_bundles == 1
    assert source.is_file()
    assert value.temporary.is_file()
    assert value.destination.is_file()
    assert journals[0].failure_code == "RECOVERY_PUBLISH_FINGERPRINT_AMBIGUOUS"


@pytest.mark.asyncio
async def test_recovery_rolls_back_when_configuration_changed(tmp_path: Path) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)

    def update(settings, _profile) -> None:
        settings.undo_retention_days += 1

    _update_profile(publisher._preferences, update)
    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.rolled_back_bundles == 1
    assert source.is_file()
    assert prepared[0].temporary.exists() is False
    assert prepared[0].destination.exists() is False
    assert [journal.state for journal in journals] == ["rolled_back"]
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_recovery_resumes_rollback_pending_and_is_idempotent(
    tmp_path: Path,
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    journal = prepared[0].journal
    await store.transition_file_mutation_journal(
        journal.id,
        expected_state="validated",
        new_state="rollback_pending",
        expected_row_revision=journal.row_revision,
        updated_at=111,
    )
    service = _recovery(publisher, store)

    first = await service.recover_startup()
    second = await service.recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert first.rolled_back_bundles == 1
    assert second.examined_bundles == 0
    assert source.is_file()
    assert [journal.state for journal in journals] == ["rolled_back"]


@pytest.mark.asyncio
async def test_periodic_recovery_skips_live_operation_lease(tmp_path: Path) -> None:
    _root, _source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    await _prepare_bundle(publisher, store, job_id)

    result = await _recovery(publisher, store).recover_once()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.skipped_bundles == 1
    assert [journal.state for journal in journals] == ["validated"]


@pytest.mark.asyncio
async def test_recovery_marks_missing_committed_destination_and_catalog_missing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _root, _source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )

    def fail_cleanup(_value) -> None:
        raise OSError("injected cleanup failure")

    monkeypatch.setattr(publisher, "_cleanup_committed_filesystem", fail_cleanup)
    await publisher.publish_bundle(job_id, 0, "apply-worker")
    row = await store.get_target_track("track-1")
    assert row is not None
    destination = Path(str(row["file_path"]))
    destination.unlink()

    result = await _recovery(publisher, store).recover_startup()

    updated = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.needs_attention_bundles == 1
    assert updated is not None and updated["availability"] == "missing"
    assert journals[0].state == "needs_attention"


@pytest.mark.asyncio
async def test_recovery_retries_committed_cleanup_after_settings_change(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )

    def fail_cleanup(_value) -> None:
        raise OSError("injected cleanup failure")

    monkeypatch.setattr(publisher, "_cleanup_committed_filesystem", fail_cleanup)
    await publisher.publish_bundle(job_id, 0, "apply-worker")

    def update(settings, _profile) -> None:
        settings.undo_retention_days += 1

    _update_profile(publisher._preferences, update)
    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.exists() is False
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_recovery_restores_missing_committed_destination_from_exact_copy(
    tmp_path: Path,
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, configure=_keep_source_configuration
    )
    await publisher.publish_bundle(job_id, 0, "apply-worker")
    row = await store.get_target_track("track-1")
    assert row is not None
    destination = Path(str(row["file_path"]))
    source.write_bytes(destination.read_bytes())
    destination.unlink()
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_file_mutation_journal SET state='catalog_committed' "
            "WHERE job_id=?",
            (job_id,),
        )

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert source.is_file()
    assert destination.is_file()
    assert source.read_bytes() == destination.read_bytes()
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_recovery_catalog_destination_mismatch_preserves_all_files(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )

    def fail_cleanup(_value) -> None:
        raise OSError("injected cleanup failure")

    monkeypatch.setattr(publisher, "_cleanup_committed_filesystem", fail_cleanup)
    await publisher.publish_bundle(job_id, 0, "apply-worker")
    row = await store.get_target_track("track-1")
    assert row is not None
    destination = Path(str(row["file_path"]))
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET relative_path='catalog-disagrees.flac' "
            "WHERE id='track-1'"
        )

    result = await _recovery(publisher, store).recover_startup()

    assert result.needs_attention_bundles == 1
    assert source.is_file()
    assert destination.is_file()
