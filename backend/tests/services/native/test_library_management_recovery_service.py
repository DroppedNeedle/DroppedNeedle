import asyncio
import os
import shutil
import sqlite3
import uuid
from pathlib import Path
from unittest.mock import AsyncMock

import hashlib
import msgspec
import pytest

from core.exceptions import ConflictError

from services.native.library_management_recovery_service import (
    LibraryManagementRecoveryRun,
    LibraryManagementRecoveryService,
    _JournalPaths,
    _RecoveryUncertainError,
)
from services.native.library_management_publisher import (
    LibraryManagementPublisher,
    _IMPORT_BUNDLE_NAMESPACE,
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
    LibraryManagementImportFile,
)
from models.audio import AudioTag
from infrastructure.audio.metadata_engine import legacy_audio_projection

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

    async def stop_after_replace(value, _roots):
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
        connection.row_factory = sqlite3.Row
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
        ],
        {"root-1": tmp_path},
    )

    assert recovered[0].journal.state == "published"
    assert source.exists() is False
    assert temporary.read_bytes() == content
    assert backup.read_bytes() == content


@pytest.mark.parametrize(
    ("backup_exists", "temporary_exists"),
    [(True, True), (False, True), (False, False)],
)
@pytest.mark.asyncio
async def test_committed_delete_cleanup_resumes_from_monotonic_substates(
    tmp_path: Path, backup_exists: bool, temporary_exists: bool
) -> None:
    (
        root,
        _source,
        _real_store,
        _audio,
        publisher,
        job_id,
    ) = await _ready_apply_operation(tmp_path)
    snapshot = await _real_store.get_library_management_job_snapshot(job_id)
    assert snapshot is not None
    pinned, roots = publisher.recovery_configuration(snapshot)
    content = b"generated artwork"
    fingerprint = hashlib.sha256(content).hexdigest()
    destination = root / "cover.jpg"
    temporary = root / ".delete-temp.jpg"
    backup = root / ".delete-backup.jpg"
    if temporary_exists:
        temporary.write_bytes(content)
    if backup_exists:
        backup.write_bytes(content)
    journal = LibraryFileMutationJournal(
        id="committed-delete-journal",
        job_id=job_id,
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
        state="cleanup_pending",
        created_at=1,
        updated_at=1,
    )
    store = AsyncMock()
    service = LibraryManagementRecoveryService(
        store,
        publisher,
        LibraryFilesystemCoordinator(),
        clock=lambda: 2,
    )

    await service._cleanup_committed_locked(
        snapshot,
        [
            _JournalPaths(
                journal=journal,
                source=destination,
                temporary=temporary,
                backup=backup,
                destination=destination,
            )
        ],
        pinned,
        roots,
    )

    assert not backup.exists()
    assert not temporary.exists()
    store.transition_file_mutation_journal.assert_awaited_once_with(
        journal.id,
        expected_state="cleanup_pending",
        new_state="completed",
        expected_row_revision=journal.row_revision,
        updated_at=2,
        increment_attempts=True,
    )


@pytest.mark.asyncio
async def test_recovery_resumes_validated_bundle_and_second_run_is_noop(
    tmp_path: Path,
) -> None:
    root, source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    original_artwork = audio.snapshot(source).artwork
    await _prepare_bundle(publisher, store, job_id)
    baseline = await store.get_management_baseline("track-1")
    before_snapshot = await store.get_management_operation_snapshot(
        job_id, 0, "track-1"
    )
    assert baseline is not None and baseline.image_snapshot_json == "[]"
    assert before_snapshot is not None and before_snapshot.image_snapshot_json == "[]"
    service = _recovery(publisher, store)

    first = await service.recover_startup()
    second = await service.recover_startup()

    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert first.recovered_bundles == 1
    assert second.examined_bundles == 0
    assert source.exists() is False
    assert row is not None and (root / str(row["relative_path"])).is_file()
    assert audio.snapshot(root / str(row["relative_path"])).artwork == original_artwork
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
        connection.row_factory = sqlite3.Row
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
    snapshot = await store.get_library_management_job_snapshot(job_id)
    assert snapshot is not None
    _pinned, roots = publisher.recovery_configuration(snapshot)
    await publisher._publish_one(prepared[0], roots)

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
    snapshot = await store.get_library_management_job_snapshot(job_id)
    assert snapshot is not None
    _pinned, roots = publisher.recovery_configuration(snapshot)
    await publisher._publish_one(audio, roots)

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
    snapshot = await store.get_library_management_job_snapshot(job_id)
    assert snapshot is not None
    _pinned, roots = publisher.recovery_configuration(snapshot)
    await publisher._publish_one(prepared[0], roots)
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
    monkeypatch: pytest.MonkeyPatch,
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
    original_inspect = service._inspect

    def reject_temporary_hash(path: Path | None):
        assert path != prepared[0].temporary
        return original_inspect(path)

    monkeypatch.setattr(service, "_inspect", reject_temporary_hash)

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

    def fail_cleanup(_value, _roots) -> None:
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

    def fail_cleanup(_value, _roots) -> None:
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
        connection.row_factory = sqlite3.Row
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

    def fail_cleanup(_value, _roots) -> None:
        raise OSError("injected cleanup failure")

    monkeypatch.setattr(publisher, "_cleanup_committed_filesystem", fail_cleanup)
    await publisher.publish_bundle(job_id, 0, "apply-worker")
    row = await store.get_target_track("track-1")
    assert row is not None
    destination = Path(str(row["file_path"]))
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.execute(
            "UPDATE local_tracks SET relative_path='catalog-disagrees.flac' "
            "WHERE id='track-1'"
        )

    result = await _recovery(publisher, store).recover_startup()

    assert result.needs_attention_bundles == 1
    assert source.is_file()
    assert destination.is_file()


def _add_second_album(root: Path, _preferences, store) -> None:
    """Seed a second single-track album so one operation spans two bundles."""
    source = root / "second.flac"
    shutil.copy2(root / "source.flac", source)
    metadata = source.stat()
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, grouping_source, created_at, "
            "updated_at) VALUES ('album-2', 'root-1', 'group-2', 'Second Album', "
            "'second album', 'Alpha', 'alpha', 'artist-1', 'automatic', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, "
            "album_artist_name_folded, disc_number, track_number, year, genre, "
            "genre_folded, file_format, ingest_source, imported_at, membership_source) "
            "VALUES ('track-2', 'album-2', 'root-1', ?, 'second.flac', ?, ?, ?, ?, "
            "'exact', 'tag-2', 'Second Track', 'second track', 'Alpha', 'alpha', "
            "'Second Album', 'second album', 'Alpha', 'alpha', 1, 1, 2024, "
            "'Electronic', 'electronic', 'flac', 'scan', 1, 'automatic')",
            (
                str(source),
                hashlib.sha256(b"second.flac").hexdigest(),
                metadata.st_size,
                metadata.st_mtime_ns,
                f"{metadata.st_size}:{metadata.st_mtime_ns}",
            ),
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at) VALUES ('album-2', 'musicbrainz', "
            "'dcff25f1-702d-3b5e-b0da-d48172e6e62a', "
            "'aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b', 'manual', 1)"
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, release_mbid, "
            "release_track_mbid, medium_position, release_track_position, "
            "decision_source, selected_at) VALUES ('track-2', 'musicbrainz', "
            "'33333333-3333-4333-8333-333333333333', "
            "'aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b', "
            "'22222222-2222-4222-8222-222222222222', 1, 1, 'manual', 1)"
        )


async def _prepare_bundle_ordinal(publisher, store, job_id: str, ordinal: int):
    snapshot = await store.get_library_management_job_snapshot(job_id)
    assert snapshot is not None
    pinned, roots = publisher.recovery_configuration(snapshot)
    items = await store.get_library_management_bundle_plan_items(job_id, ordinal)
    prepared = []
    for item in items:
        prepared.extend(
            await publisher._prepare_plan_item(snapshot, pinned, item, roots, ordinal)
        )
    return prepared


@pytest.mark.asyncio
async def test_recovery_failure_is_bundle_scoped_and_job_settles_after_last(
    tmp_path: Path,
) -> None:
    (
        _root,
        source,
        store,
        _audio,
        publisher,
        job_id,
    ) = await _ready_apply_operation(
        tmp_path,
        prepare_store=_add_second_album,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(
            kind="albums", ids=("album-1", "album-2")
        ),
    )
    settings_revision = (
        publisher._preferences.get_library_management_settings().settings_revision
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.execute(
            "UPDATE library_operation_jobs SET expected_work_count=2 WHERE id=?",
            (job_id,),
        )
        connection.execute(
            "INSERT INTO library_operation_work "
            "(job_id,ordinal,local_album_id,expected_subject_revision,"
            "expected_input_revision,action,idempotency_key,state,updated_at) "
            "VALUES (?,1,'album-2',1,?,'library_management',?,'running',110)",
            (job_id, settings_revision, f"{job_id}:bundle:1"),
        )
    prepared_zero = await _prepare_bundle_ordinal(publisher, store, job_id, 0)
    prepared_one = await _prepare_bundle_ordinal(publisher, store, job_id, 1)
    value = prepared_zero[0]
    value.destination.parent.mkdir(parents=True, exist_ok=True)
    value.destination.write_bytes(value.temporary.read_bytes())
    for item in prepared_one:
        os.replace(item.temporary, item.destination)

    service = _recovery(publisher, store)
    first = await service.recover_once(
        limit=1, force_expired_process_leases=True
    )

    journals_zero = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert first.needs_attention_bundles == 1
    assert all(journal.state == "needs_attention" for journal in journals_zero)
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        job = connection.execute(
            "SELECT state,terminal_code FROM library_operation_jobs WHERE id=?",
            (job_id,),
        ).fetchone()
        failed_work = connection.execute(
            "SELECT state,failure_code FROM library_operation_work "
            "WHERE job_id=? AND ordinal=0",
            (job_id,),
        ).fetchone()
        sibling_work = connection.execute(
            "SELECT state FROM library_operation_work WHERE job_id=? AND ordinal=1",
            (job_id,),
        ).fetchone()
    assert job["state"] == "running"
    assert job["terminal_code"] is None
    assert failed_work["state"] == "failed"
    assert failed_work["failure_code"] == "RECOVERY_NEEDS_ATTENTION"
    assert sibling_work["state"] == "running"
    assert await store.claim_operation_work(job_id, "apply-worker", now=130) is None

    second = await service.recover_once(force_expired_process_leases=True)

    journals_one = await store.list_file_mutation_journals_for_bundle(job_id, 1)
    assert second.recovered_bundles == 1
    assert all(journal.state == "completed" for journal in journals_one)
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        job = connection.execute(
            "SELECT state,terminal_code FROM library_operation_jobs WHERE id=?",
            (job_id,),
        ).fetchone()
    assert job["state"] == "failed"
    assert job["terminal_code"] == "RECOVERY_FAILED"
    assert source.is_file()
    assert value.temporary.is_file()


@pytest.mark.asyncio
async def test_recovery_attention_on_last_bundle_still_fails_the_job(
    tmp_path: Path,
) -> None:
    _root, _source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    value = prepared[0]
    value.destination.parent.mkdir(parents=True, exist_ok=True)
    value.destination.write_bytes(value.temporary.read_bytes())

    result = await _recovery(publisher, store).recover_startup()

    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        job = connection.execute(
            "SELECT state,terminal_code FROM library_operation_jobs WHERE id=?",
            (job_id,),
        ).fetchone()
    assert result.needs_attention_bundles == 1
    assert job["state"] == "failed"
    assert job["terminal_code"] == "RECOVERY_NEEDS_ATTENTION"



def _seed_stuck_cleanup_imports(
    store, policy_revision: str, source: Path, count: int
) -> None:
    """Seed cleanup_pending imports whose source no longer matches its journal.

    Each bundle keeps failing startup cleanup (source hash mismatch), so it
    stays recoverable forever and exercises the bounded drain/block behavior.
    """
    bundle_rows = []
    journal_rows = []
    for index in range(count):
        key = f"acquisition:stuck:{index}"
        bundle_id = str(uuid.uuid5(_IMPORT_BUNDLE_NAMESPACE, key))
        file_request = msgspec.structs.replace(
            _STUCK_IMPORT_TEMPLATE[0],
            destination_relative_path=f"Stuck {index}/01 Stuck.flac",
        )
        request_json = msgspec.json.encode(
            LibraryManagementImportBundle(
                idempotency_key=key,
                origin="acquisition",
                policy_revision=policy_revision,
                files=(file_request,),
            )
        ).decode()
        request_hash = hashlib.sha256(request_json.encode()).hexdigest()
        bundle_rows.append(
            (
                bundle_id,
                key,
                policy_revision,
                request_json,
                request_hash,
                110.0,
                110.0,
            )
        )
        journal_rows.append(
            (
                bundle_id,
                0,
                "catalog_committed",
                "ab" * 32,
                1,
                1,
                ".stuck-temp.flac",
                "root-1",
                f"Stuck {index}/01 Stuck.flac",
                110.0,
                110.0,
            )
        )
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.executemany(
            "INSERT INTO library_management_import_bundles "
            "(id,idempotency_key,origin,policy_revision,request_json,request_hash,"
            "state,result_json,created_at,updated_at,row_revision) "
            "VALUES (?,?,'acquisition',?,?,?,'cleanup_pending','{}',?,?,1)",
            bundle_rows,
        )
        connection.executemany(
            "INSERT INTO library_management_import_journal "
            "(bundle_id,ordinal,state,source_fingerprint,source_size,source_mtime_ns,"
            "temporary_relative_path,destination_root_id,destination_relative_path,"
            "created_at,updated_at,row_revision) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,1)",
            journal_rows,
        )


_STUCK_SOURCE_PATH: list[object] = [None]
_STUCK_IMPORT_TEMPLATE: list[object] = [None]

@pytest.mark.asyncio
async def test_startup_escalates_stuck_import_cleanup_to_needs_attention(
    tmp_path: Path,
) -> None:
    """Persistent import source-cleanup failures boot instead of bricking (#387)."""
    (
        _root,
        catalog_source,
        store,
        audio,
        publisher,
        _service,
        policy_revision,
    ) = _import_publication_fixture(tmp_path)
    stuck_source = tmp_path / "stuck-source.bin"
    stuck_source.write_bytes(b"not audio")
    _tag, _info = legacy_audio_projection(audio.read(catalog_source))
    del _tag
    _STUCK_IMPORT_TEMPLATE[0] = LibraryManagementImportFile(
        ordinal=0,
        input_path=str(stuck_source),
        destination_root_id="root-1",
        destination_relative_path="Stuck/01 Stuck.flac",
        tag=AudioTag(title="Stuck", artist="Artist", album="Album", track_number=1),
        info=_info,
        release_group_mbid=None,
        release_mbid=None,
        recording_mbid=None,
        confidence=0.0,
        source="drop",
    )
    _seed_stuck_cleanup_imports(store, policy_revision, stuck_source, count=3)

    result = await _recovery(publisher, store).recover_startup()

    remaining = await store.list_recoverable_library_management_import_bundles(
        limit=10, include_committed_cleanup=True
    )
    assert remaining == []
    assert result.needs_attention_bundles == 3
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        bundle_states = {
            row["state"]
            for row in connection.execute(
                "SELECT state FROM library_management_import_bundles "
                "WHERE idempotency_key LIKE 'acquisition:stuck:%'"
            ).fetchall()
        }
        journal_states = {
            (row["state"], row["failure_code"])
            for row in connection.execute(
                "SELECT state,failure_code FROM library_management_import_journal "
                "WHERE bundle_id IN (SELECT id FROM "
                "library_management_import_bundles WHERE idempotency_key "
                "LIKE 'acquisition:stuck:%')"
            ).fetchall()
        }
    assert bundle_states == {"needs_attention"}
    assert journal_states == {("needs_attention", "SOURCE_CLEANUP_FAILED")}
    # Escalation flags the bundles; it never deletes the retained sources.
    assert stuck_source.is_file()


@pytest.mark.asyncio
async def test_startup_still_blocks_when_manual_bundles_remain(
    tmp_path: Path,
) -> None:
    """The manual lane keeps the old refuse-to-boot contract (#387)."""
    (
        _root,
        _catalog_source,
        store,
        _audio,
        publisher,
        _service,
        _policy_revision,
    ) = _import_publication_fixture(tmp_path)
    service = _recovery(publisher, store)

    async def _no_work(**kwargs: object) -> LibraryManagementRecoveryRun:
        return LibraryManagementRecoveryRun()

    async def _manual_remaining(*, limit: int = 1) -> list[dict[str, object]]:
        return [{"job_id": "manual-stuck", "bundle_ordinal": 0}]

    service.recover_once = _no_work  # type: ignore[method-assign]
    store.list_recoverable_management_bundles = (  # type: ignore[method-assign]
        _manual_remaining
    )
    with pytest.raises(ConflictError, match="safe startup boundary"):
        await service.recover_startup()


@pytest.mark.asyncio
async def test_startup_escalates_a_stuck_import_backlog_past_the_bundle_cap(
    tmp_path: Path,
) -> None:
    """A huge stuck-cleanup backlog boots after one bounded drain pass (#387)."""
    (
        _root,
        catalog_source,
        store,
        audio,
        publisher,
        _service,
        policy_revision,
    ) = _import_publication_fixture(tmp_path)
    stuck_source = tmp_path / "stuck-source.bin"
    stuck_source.write_bytes(b"not audio")
    _tag, _info = legacy_audio_projection(audio.read(catalog_source))
    del _tag
    _STUCK_IMPORT_TEMPLATE[0] = LibraryManagementImportFile(
        ordinal=0,
        input_path=str(stuck_source),
        destination_root_id="root-1",
        destination_relative_path="Stuck/01 Stuck.flac",
        tag=AudioTag(title="Stuck", artist="Artist", album="Album", track_number=1),
        info=_info,
        release_group_mbid=None,
        release_mbid=None,
        recording_mbid=None,
        confidence=0.0,
        source="drop",
    )
    _seed_stuck_cleanup_imports(store, policy_revision, stuck_source, count=501)

    result = await _recovery(publisher, store).recover_startup()

    remaining = await store.list_recoverable_library_management_import_bundles(
        limit=10, include_committed_cleanup=True
    )
    assert remaining == []
    assert result.needs_attention_bundles == 501
    # One bounded drain pass ran before escalation; startup never spins to
    # the cap re-examining the same failing bundles.
    assert result.examined_bundles == 100


@pytest.mark.asyncio
async def test_startup_drains_import_committed_without_cleanup(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "committed-import.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Committed.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:startup-cleanup:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    class SimulatedProcessStop(BaseException):
        pass

    async def stop_before_cleanup(record, bundle_arg):
        del bundle_arg
        raise SimulatedProcessStop

    resume = publisher._resume_import_cleanup
    publisher._resume_import_cleanup = stop_before_cleanup
    with pytest.raises(SimulatedProcessStop):
        await service.publish_import_bundle(bundle)
    publisher._resume_import_cleanup = resume

    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.row_factory = sqlite3.Row
        state = connection.execute(
            "SELECT state FROM library_management_import_bundles "
            "WHERE idempotency_key=?",
            (bundle.idempotency_key,),
        ).fetchone()[0]
    assert state == "catalog_committed"

    result = await _recovery(publisher, store).recover_startup()
    second = await _recovery(publisher, store).recover_startup()

    assert result.recovered_bundles == 1
    assert incoming.is_file() is False
    assert (root / request.destination_relative_path).is_file()
    remaining = await store.list_recoverable_library_management_import_bundles(
        limit=10, include_committed_cleanup=True
    )
    assert remaining == []
    assert second.examined_bundles == 0


@pytest.mark.asyncio
async def test_periodic_recovery_reserves_manual_bundles_behind_import_backlog(
    tmp_path: Path,
) -> None:
    (
        _root,
        catalog_source,
        store,
        audio,
        publisher,
        _job_id,
        policy_revision,
    ) = await _ready_apply_operation_with_policy(tmp_path)
    await _prepare_bundle(publisher, store, _job_id)
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.row_factory = sqlite3.Row
        connection.execute(
            "UPDATE library_operation_jobs SET state='stopped',terminal_code='STOPPED' "
            "WHERE id=?",
            (_job_id,),
        )
    stuck_source = tmp_path / "stuck-source.bin"
    stuck_source.write_bytes(b"not audio")
    _tag, _info = legacy_audio_projection(audio.read(catalog_source))
    del _tag
    _STUCK_IMPORT_TEMPLATE[0] = LibraryManagementImportFile(
        ordinal=0,
        input_path=str(stuck_source),
        destination_root_id="root-1",
        destination_relative_path="Stuck/01 Stuck.flac",
        tag=AudioTag(title="Stuck", artist="Artist", album="Album", track_number=1),
        info=_info,
        release_group_mbid=None,
        release_mbid=None,
        recording_mbid=None,
        confidence=0.0,
        source="drop",
    )
    _seed_stuck_cleanup_imports(store, policy_revision, stuck_source, count=100)

    result = await _recovery(publisher, store).recover_once(limit=100)

    journals = await store.list_file_mutation_journals_for_bundle(_job_id, 0)
    assert [journal.state for journal in journals] == ["rolled_back"]
    assert result.skipped_bundles == 99


async def _ready_apply_operation_with_policy(tmp_path: Path):
    from services.native.library_policy_resolver import LibraryPolicyResolver

    root, source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    policy_revision = LibraryPolicyResolver(
        publisher._preferences.get_typed_library_settings_raw()
    ).policy_revision
    return root, source, store, audio, publisher, job_id, policy_revision


@pytest.mark.asyncio
async def test_recovery_blocks_on_mixed_commit_state(tmp_path: Path) -> None:
    _root, _source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        prepare_store=_add_second_album_track,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    first = prepared[0].journal
    published = await store.transition_file_mutation_journal(
        first.id,
        expected_state="validated",
        new_state="published",
        expected_row_revision=first.row_revision,
        updated_at=111,
    )
    await store.transition_file_mutation_journal(
        first.id,
        expected_state="published",
        new_state="catalog_committed",
        expected_row_revision=published.row_revision,
        updated_at=112,
    )

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.needs_attention_bundles == 1
    assert {
        journal.failure_code for journal in journals
    } == {"RECOVERY_MIXED_COMMIT_STATE"}


@pytest.mark.asyncio
async def test_recovery_blocks_on_unknown_journal_state(tmp_path: Path) -> None:
    _root, _source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        prepare_store=_add_second_album_track,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    await store.transition_file_mutation_journal(
        prepared[0].journal.id,
        expected_state="validated",
        new_state="needs_attention",
        expected_row_revision=prepared[0].journal.row_revision,
        updated_at=111,
        failure_code="RECOVERY_NEEDS_ATTENTION",
    )

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.needs_attention_bundles == 1
    codes = {journal.failure_code for journal in journals}
    # The already-attention journal keeps its original code; the surviving
    # validated journal is reclassified by the mixed-active classifier.
    assert codes == {"RECOVERY_NEEDS_ATTENTION", "RECOVERY_UNKNOWN_STATE"}


@pytest.mark.asyncio
async def test_recovery_marks_operation_state_mismatch_when_work_row_is_terminal(
    tmp_path: Path,
) -> None:
    _root, _source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )


    prepared = await _prepare_bundle(publisher, store, job_id)
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.row_factory = sqlite3.Row
        connection.execute(
            "UPDATE library_operation_work SET state='succeeded' "
            "WHERE job_id=? AND ordinal=0",
            (job_id,),
        )

    result = await _recovery(publisher, store).recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.needs_attention_bundles == 1
    assert journals[0].failure_code == "RECOVERY_OPERATION_STATE_MISMATCH"


@pytest.mark.asyncio
async def test_corrupt_delete_evidence_fails_closed_instead_of_move_rules(
    tmp_path: Path,
) -> None:
    content = b"generated artwork"
    fingerprint = hashlib.sha256(content).hexdigest()
    source = tmp_path / "cover.jpg"
    temporary = tmp_path / ".delete-temp.jpg"
    backup = tmp_path / ".delete-backup.jpg"
    temporary.write_bytes(content)
    backup.write_bytes(content)
    journal = LibraryFileMutationJournal(
        id="corrupt-delete-journal",
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
        recovery_evidence_json='{"mutation":"dele',
        state="validated",
        created_at=1.0,
        updated_at=1.0,
    )
    store = AsyncMock()
    service = LibraryManagementRecoveryService(
        store,
        AsyncMock(),
        LibraryFilesystemCoordinator(),
        clock=lambda: 2.0,
    )

    with pytest.raises(_RecoveryUncertainError) as caught:
        await service._publish_remaining(
            [
                _JournalPaths(
                    journal=journal,
                    source=source,
                    temporary=temporary,
                    backup=backup,
                    destination=source,
                )
            ],
            {"root-1": tmp_path},
        )

    assert caught.value.reason == "RECOVERY_JOURNAL_EVIDENCE_CORRUPT"
    store.transition_file_mutation_journal.assert_not_awaited()


@pytest.mark.asyncio
async def test_recover_import_bundle_skips_while_publication_lock_held(
    tmp_path: Path,
) -> None:
    (
        _root,
        catalog_source,
        store,
        audio,
        publisher,
        _service,
        policy_revision,
    ) = _import_publication_fixture(tmp_path)
    incoming = tmp_path / "locked-import.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Locked.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:lock-skip:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )
    commit_started = asyncio.Event()
    release = asyncio.Event()

    class SimulatedProcessStop(BaseException):
        pass

    async def gated_commit(bundle_id: str, published):
        del bundle_id, published
        commit_started.set()
        await release.wait()
        raise SimulatedProcessStop

    original_rollback = publisher._rollback_import_bundle
    publisher._rollback_import_bundle = AsyncMock(
        side_effect=SimulatedProcessStop
    )
    publication = asyncio.create_task(
        publisher.publish_import_bundle(bundle, gated_commit)
    )
    await commit_started.wait()

    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        connection.row_factory = sqlite3.Row
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    record = await store.get_library_management_import_bundle(bundle_id)
    assert record is not None
    assert await publisher.recover_import_bundle(record) == "skipped"

    release.set()
    with pytest.raises(SimulatedProcessStop):
        await publication
    publisher._rollback_import_bundle = original_rollback

    stuck_record = await store.get_library_management_import_bundle(bundle_id)
    assert stuck_record is not None
    assert stuck_record.state == "publishing"
    assert await publisher.recover_import_bundle(stuck_record) == "rolled_back"
@pytest.mark.asyncio
async def test_recovery_replays_post_commit_hook_after_completing_bundle(
    tmp_path: Path,
) -> None:
    """F-145: recovery finishing a published bundle must replay the guarded
    post-commit hook so crash-lost enqueues are recovered at startup."""
    _root, source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    prepared = await _prepare_bundle(publisher, store, job_id)
    value = prepared[0]
    os.replace(value.temporary, value.destination)

    hook = AsyncMock()
    from infrastructure.library_management_blob_store import LibraryManagementBlobStore
    from services.native.audio_write_planning_service import AudioWritePlanningService

    recovered_publisher = LibraryManagementPublisher(
        store,
        publisher._preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "blobs", store),
        publisher._filesystem,
        clock=lambda: 110.0,
        on_commit=hook,
    )
    service = _recovery(recovered_publisher, store)

    result = await service.recover_startup()

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.recovered_bundles == 1
    assert [journal.state for journal in journals] == ["completed"]
    hook.assert_awaited_once()
    track_ids, album_ids = hook.await_args.args
    assert track_ids == {"track-1"}
    assert album_ids >= {"album-1"}
    assert source.exists() is False
