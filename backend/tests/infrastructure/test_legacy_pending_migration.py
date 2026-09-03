import asyncio
import logging
import shutil
import sqlite3
import threading
import time
from pathlib import Path

import pytest

from core.exceptions import StaleRevisionError, ValidationError

from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_work import MigrationProvenance
from services.native.bounded_legacy_catalog_migrator import (
    BoundedLegacyCatalogMigrator,
)
from services.native.legacy_pending_migration_service import (
    LegacyPendingMigrationService,
    pending_run_id,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from tests.infrastructure.test_legacy_catalog_importer import (
    TRACK_1,
    TRACK_2,
    _create_source,
)
from tests.infrastructure.test_bounded_legacy_catalog_migrator import (
    _insert_legacy_library_file,
)


def _store(database: Path) -> NativeLibraryStore:
    return NativeLibraryStore(database, threading.Lock())


def _resolver(*roots: tuple[str, Path]) -> LibraryPolicyResolver:
    return LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id=root_id,
                    path=str(path),
                    label=f"Library {index}",
                    policy="automatic",
                )
                for index, (root_id, path) in enumerate(roots, start=1)
            ]
        )
    )


def _write_catalog_files(root: Path) -> None:
    compilation = root / "Compilation"
    compilation.mkdir(parents=True)
    (compilation / "01.flac").write_bytes(b"a" * 100)
    (compilation / "02.flac").write_bytes(b"b" * 200)


@pytest.mark.asyncio
async def test_lenient_migration_skips_unmappable_paths_and_completes(
    tmp_path: Path,
) -> None:
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)
    resolver = _resolver(("root", tmp_path / "Missing" / "Music"))

    outcome = await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    assert outcome.blocker_count == 0
    assert outcome.skipped_counts["library_file"] == 2
    assert outcome.skipped_counts["review_row"] == 4
    assert outcome.report.state == "applied"
    review_counts = next(
        count for count in outcome.report.reference_counts if count.kind == "review_row"
    )
    assert review_counts.unresolved == 4
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone() == (
            0,
        )
        assert connection.execute(
            "SELECT COUNT(*) FROM library_migration_markers "
            "WHERE marker = 'legacy_catalog_import_complete'"
        ).fetchone() == (1,)


@pytest.mark.asyncio
async def test_pending_migration_imports_newly_resolvable_rows(tmp_path: Path) -> None:
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)

    first = await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Missing" / "Music")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)
    assert first.skipped_counts["library_file"] == 2

    resolver = _resolver(("root", historical_root))
    pending = await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate_pending(f"legacy-pending-{resolver.policy_revision}", now=200)

    assert pending.blocker_count == 0
    assert "library_file" not in pending.skipped_counts
    assert "review_row" not in pending.skipped_counts
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone() == (
            4,
        )
        assert connection.execute(
            "SELECT COUNT(*) FROM library_migration_provenance "
            "WHERE source_kind = 'library_file'"
        ).fetchone() == (2,)
        assert connection.execute(
            "SELECT COUNT(*) FROM library_migration_provenance "
            "WHERE source_kind = 'review_row'"
        ).fetchone() == (4,)

    repeat = await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate_pending(f"legacy-pending-{resolver.policy_revision}", now=300)

    assert repeat.blocker_count == 0
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone() == (
            4,
        )


@pytest.mark.asyncio
async def test_pending_migration_skips_rows_already_owned_by_tracks(
    tmp_path: Path,
) -> None:
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)

    outcome = await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", historical_root)),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("initial-migration", now=100)
    assert outcome.blocker_count == 0

    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="99999999-9999-4999-8999-000000000001",
            path=historical_root / "Compilation" / "01.flac",
            title="Duplicate",
            track_number=1,
            release_group_mbid=None,
        )

    resolver = _resolver(("root", historical_root))
    pending = await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate_pending(f"legacy-pending-{resolver.policy_revision}", now=200)

    assert pending.blocker_count == 0
    assert pending.skipped_counts["scan_owned_library_file"] == 1
    with sqlite3.connect(database) as connection:
        paths = [
            str(row[0])
            for row in connection.execute(
                "SELECT file_path FROM local_tracks ORDER BY file_path"
            ).fetchall()
        ]
    assert paths.count(str(historical_root / "Compilation" / "01.flac")) == 1


@pytest.mark.asyncio
async def test_pending_service_gates_scheduling(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)

    service = LegacyPendingMigrationService(
        store, lambda: _resolver(("root", tmp_path / "Missing" / "Music"))
    )
    runs: list[str] = []

    async def fake_run(run_id: str) -> None:
        runs.append(run_id)
        service._running = False

    monkeypatch.setattr(service, "_run", fake_run)

    assert await service.schedule() is False

    await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Missing" / "Music")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    assert await service.schedule() is True
    await asyncio.sleep(0)
    assert len(runs) == 1
    assert runs[0].startswith("legacy-pending-")

    # NEW-MIG-01: mark the launched run completed for ITS input revision.
    # Repeated schedules with unchanged pending input stay idempotently skipped.
    source_revision = await store.get_bounded_legacy_source_revision()
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO library_migration_runs "
            "(id, source_revision, root_revision, state, report_json, "
            "started_at, updated_at) VALUES (?, ?, '', 'completed', '', 100, 100)",
            (runs[0], source_revision),
        )

    assert await service.schedule() is False

    # A new legacy row arrives under the SAME policy revision: the pending
    # input revision changes, so the completed policy-only gate must not
    # suppress the next schedule.
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="99999999-9999-4999-8999-000000000004",
            path=historical_root / "Compilation" / "04.flac",
            title="Late Arrival",
            track_number=4,
            release_group_mbid=None,
        )

    service = LegacyPendingMigrationService(
        store, lambda: _resolver(("root", historical_root))
    )
    monkeypatch.setattr(service, "_run", fake_run)
    assert await service.schedule() is True
    await asyncio.sleep(0)
    assert runs[-1] != runs[0]
    assert runs[-1] == pending_run_id(
        _resolver(("root", historical_root)).policy_revision,
        await store.get_bounded_legacy_source_revision(),
    )


@pytest.mark.asyncio
async def test_lenient_migration_skips_unmappable_local_only_rows(
    tmp_path: Path,
) -> None:
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="99999999-9999-4999-8999-000000000002",
            path=historical_root / "Compilation" / "03.flac",
            title="Local Only",
            track_number=3,
            release_group_mbid=None,
        )
    store = _store(database)

    outcome = await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Missing" / "Music")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    assert outcome.blocker_count == 0
    assert outcome.skipped_counts["library_file"] == 3
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone() == (
            0,
        )


@pytest.mark.asyncio
async def test_pending_migration_ignores_already_migrated_local_only_rows(
    tmp_path: Path,
) -> None:
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="99999999-9999-4999-8999-000000000003",
            path=historical_root / "Compilation" / "03.flac",
            title="Local Only",
            track_number=3,
            release_group_mbid=None,
        )
    store = _store(database)
    resolver = _resolver(("root", historical_root))

    first = await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("initial-migration", now=100)
    assert first.blocker_count == 0
    with sqlite3.connect(database) as connection:
        migrated_tracks = connection.execute(
            "SELECT COUNT(*) FROM local_tracks"
        ).fetchone()[0]

    pending = await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate_pending(f"legacy-pending-{resolver.policy_revision}", now=200)

    assert pending.blocker_count == 0
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone() == (
            migrated_tracks,
        )


@pytest.mark.asyncio
async def test_pending_service_rejects_concurrent_schedule(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)
    await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Missing" / "Music")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    service = LegacyPendingMigrationService(
        store, lambda: _resolver(("root", tmp_path / "Missing" / "Music"))
    )

    async def fake_run(run_id: str) -> None:
        service._running = False

    monkeypatch.setattr(service, "_run", fake_run)

    first, second = await asyncio.gather(service.schedule(), service.schedule())

    assert sorted([first, second]) == [False, True]


@pytest.mark.asyncio
async def test_pending_run_id_binds_policy_and_input_revision() -> None:
    """NEW-MIG-01: the run identity changes when either the policy revision or
    the pending input revision changes, and is stable when both are unchanged."""
    assert (
        pending_run_id("policy-a", "rev-1") == "legacy-pending-policy-a-rev-1"
    )
    assert pending_run_id("policy-a", "rev-1") != pending_run_id("policy-b", "rev-1")
    assert pending_run_id("policy-a", "rev-1") != pending_run_id("policy-a", "rev-2")


@pytest.mark.asyncio
async def test_completed_input_a_does_not_suppress_new_pending_input_b(
    tmp_path: Path,
) -> None:
    """NEW-MIG-01 core regression, real SQLite: complete pending input A, then
    a new legacy row (input B) arrives under the SAME policy revision. The
    service must schedule a fresh run keyed on B's input revision instead of
    being suppressed by A's completed policy-only-style ID."""
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)
    missing_resolver = _resolver(("root", tmp_path / "Missing" / "Music"))
    resolvable_resolver = _resolver(("root", historical_root))
    service = LegacyPendingMigrationService(store, lambda: missing_resolver)
    runs: list[str] = []

    async def fake_run(run_id: str) -> None:
        runs.append(run_id)
        service._running = False

    monkeypatch_runs = True

    # Input A: two skippable-under-missing-root rows make pending counts
    # nonzero; schedule and complete the launched run for input A.
    await BoundedLegacyCatalogMigrator(
        store,
        missing_resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    import asyncio as _asyncio

    original_run = service._run

    async def real_then_record(run_id: str) -> None:
        runs.append(run_id)
        try:
            await original_run(run_id)
        finally:
            # the real _run resets ITS OWN instance's flag; this wrapper's
            # owner needs the same guarantee or scheduling deadlocks
            service._running = False

    service._run = real_then_record
    assert await service.schedule() is True
    while service._running:
        await _asyncio.sleep(0.01)
    revision_a = await store.get_bounded_legacy_source_revision()
    assert len(runs) == 1
    revision_a = await store.get_bounded_legacy_source_revision()
    assert runs[0] == pending_run_id(missing_resolver.policy_revision, revision_a)
    with sqlite3.connect(database) as connection:
        state = connection.execute(
            "SELECT state FROM library_migration_runs WHERE id = ?", (runs[0],)
        ).fetchone()[0]
    assert state == "completed"

    # Same input again: idempotently skipped.
    service._run = real_then_record
    skipped = await service.schedule()
    assert skipped is False

    # Input B: one new legacy row arrives; policy revision unchanged.
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="99999999-9999-4999-8999-000000000005",
            path=historical_root / "Compilation" / "05.flac",
            title="Late Arrival",
            track_number=5,
            release_group_mbid=None,
        )

    counts = await store.get_pending_legacy_counts()
    assert any(value > 0 for value in counts.values())
    revision_b = await store.get_bounded_legacy_source_revision()
    assert revision_b != revision_a
    candidate = pending_run_id(resolvable_resolver.policy_revision, revision_b)
    assert await store.get_migration_run_state(candidate) != "completed"

    service = LegacyPendingMigrationService(store, lambda: resolvable_resolver)
    service._run = real_then_record
    launched = await service.schedule()
    while service._running:
        await _asyncio.sleep(0.01)
    assert runs[-1] == candidate


@pytest.mark.asyncio
async def test_schedule_without_marker_or_pending_launches_nothing(
    tmp_path: Path,
) -> None:
    """Fresh install (no marker) and completed-marker-with-zero-pending are
    no-op paths that launch no migration task."""
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)
    resolver = _resolver(("root", tmp_path / "Missing" / "Music"))
    service = LegacyPendingMigrationService(store, lambda: resolver)

    # Fresh install: no completion marker.
    assert await service.schedule() is False

    # Marker present but zero pending rows: an empty legacy source migrates
    # to a completed marker with nothing left pending, so the gate skips.
    empty_database = tmp_path / "empty.db"
    _create_source(empty_database, historical_root)
    with sqlite3.connect(empty_database) as connection:
        for table in (
            "library_files",
            "manual_review_queue",
            "library_albums",
            "library_artists",
            "library_album_meta",
            "user_favorites",
            "play_history",
            "playlists",
            "playlist_tracks",
            "album_release_pins",
            "compat_bookmarks",
            "compat_play_queues",
            "compat_play_queue_items",
            "compat_id_map",
        ):
            connection.execute(f"DELETE FROM {table}")
    empty_store = _store(empty_database)
    await BoundedLegacyCatalogMigrator(
        empty_store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("empty-migration", now=300)
    empty_service = LegacyPendingMigrationService(empty_store, lambda: resolver)
    counts = await empty_store.get_pending_legacy_counts()
    assert all(value == 0 for value in counts.values())
    assert await empty_service.schedule() is False


@pytest.mark.asyncio
async def test_runtime_smoke_real_migration_through_schedule_for_input_b(
    tmp_path: Path,
) -> None:
    """NEW-MIG-01 disposable SQLite runtime smoke: complete pending input A
    through the service, then a NEW legacy row (input B) arrives under the same
    policy revision. The gate must launch a fresh run for B, the real migrator
    must give B provenance, and scheduling B again must be a no-op."""
    historical_root = tmp_path / "Historical" / "Music"
    historical_root.mkdir(parents=True)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)
    resolver = _resolver(("root", tmp_path / "Missing" / "Music"))
    service = LegacyPendingMigrationService(store, lambda: resolver)
    original_run = service._run

    async def run_and_reset(run_id: str) -> None:
        try:
            await original_run(run_id)
        finally:
            service._running = False

    async def run_and_reset(run_id: str) -> None:
        try:
            await original_run(run_id)
        finally:
            service._running = False

    service._run = run_and_reset

    async def wait_done() -> None:
        while service._running:
            await asyncio.sleep(0.01)

    def insert_row(file_id: str, name: str, title: str, track: int) -> None:
        with sqlite3.connect(database) as connection:
            _insert_legacy_library_file(
                connection,
                file_id=file_id,
                path=historical_root / name,
                title=title,
                track_number=track,
                release_group_mbid=None,
            )

    # The lenient upgrade completes first and creates the durable marker,
    # skipping every row because its root is gone.
    await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    # Input A arrives after the upgrade: the gate launches a pending run whose
    # real migration resolves the row against the restored root.
    insert_row(
        "99999999-9999-4999-8999-000000000008",
        "Late A/08.flac",
        "Late A",
        8,
    )
    Path(historical_root / "Late A").mkdir(parents=True)
    (historical_root / "Late A" / "08.flac").write_bytes(b"a" * 100)
    assert await service.schedule() is True
    await wait_done()
    with sqlite3.connect(database) as connection:
        provenance_a = connection.execute(
            "SELECT COUNT(*) FROM library_migration_provenance"
        ).fetchone()[0]
        states_a = dict(
            connection.execute(
                "SELECT id, state FROM library_migration_runs "
                "WHERE id LIKE 'legacy-pending-%'"
            ).fetchall()
        )
    revision_a = await store.get_bounded_legacy_source_revision()
    assert states_a
    assert all(state == "completed" for state in states_a.values())
    assert list(states_a) == [
        pending_run_id(resolver.policy_revision, revision_a)
    ]
    with sqlite3.connect(database) as connection:
        rows_a = connection.execute(
            "SELECT source_kind, source_key FROM library_migration_provenance"
        ).fetchall()
    print("SMOKE provenance A:", rows_a, flush=True)

    # Same input again: idempotently skipped.
    assert await service.schedule() is False

    # Input B arrives under the SAME policy revision.
    insert_row(
        "99999999-9999-4999-8999-000000000009",
        "Late B/09.flac",
        "Late B",
        9,
    )
    Path(historical_root / "Late B").mkdir(parents=True)
    (historical_root / "Late B" / "09.flac").write_bytes(b"b" * 100)

    revision_b = await store.get_bounded_legacy_source_revision()
    assert revision_b != revision_a
    candidate = pending_run_id(resolver.policy_revision, revision_b)
    assert candidate not in states_a
    assert await store.get_migration_run_state(candidate) != "completed"

    assert await service.schedule() is True
    await wait_done()

    # The durable run row is keyed to B's exact input revision.
    with sqlite3.connect(database) as connection:
        stored = connection.execute(
            "SELECT source_revision, state FROM library_migration_runs "
            "WHERE id = ?",
            (candidate,),
        ).fetchone()
    assert stored is not None
    assert stored[0] == revision_b
    assert stored[1] == "completed"
    with sqlite3.connect(database) as connection:
        provenance_b = connection.execute(
            "SELECT COUNT(*) FROM library_migration_provenance"
        ).fetchone()[0]
        states_b = dict(
            connection.execute(
                "SELECT id, state FROM library_migration_runs "
                "WHERE id LIKE 'legacy-pending-%'"
            ).fetchall()
        )
    assert candidate in states_b
    assert states_b[candidate] == "completed"
    assert len(states_b) == len(states_a) + 1
    # Idempotency: scheduling the same input again adds nothing.
    assert await service.schedule() is False


@pytest.mark.asyncio
async def test_pending_migration_projects_moved_root_after_restoration(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """NEW-MIG-03: rows left pending because their mount was absent at cutover
    become migratable after the user configures the CURRENT mount. The service
    re-proves the move with the same rules as cutover and passes the verified
    projector into the bounded migration."""
    old_root = tmp_path / "Old" / "Music"
    current_root = tmp_path / "Current" / "Music"
    database = tmp_path / "library.db"
    _create_source(database, old_root)
    store = _store(database)

    # The library MOVED: the same relative files now live under Current with
    # the exact byte sizes recorded in the legacy rows.
    current_compilation = current_root / "Compilation"
    current_compilation.mkdir(parents=True)
    (current_compilation / "01.flac").write_bytes(b"a" * 100)
    (current_compilation / "02.flac").write_bytes(b"b" * 200)

    # /tmp is a blocked probe prefix in production; relax it exactly like the
    # reconciler tests so the moved-root proof can run against tmp_path.
    import services.native.legacy_path_reconciler as lpr_module

    monkeypatch.setattr(lpr_module, "_BLOCKED_ROOTS", (Path("/"),))

    # Cutover with an UNRELATED root configured: every legacy path is outside
    # the configured roots -> lenient skip leaves them pending, and the
    # durable marker is created.
    await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Unrelated")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)
    counts = await store.get_pending_legacy_counts()
    assert counts["library_file"] == 2

    # Root restoration: the user points the root at the CURRENT mount.
    resolver = _resolver(("root", current_root))
    service = LegacyPendingMigrationService(store, lambda: resolver)
    original_run = service._run

    async def run_and_reset(run_id: str) -> None:
        try:
            await original_run(run_id)
        finally:
            service._running = False

    service._run = run_and_reset

    assert await service.schedule() is True
    while service._running:
        await asyncio.sleep(0.01)

    with sqlite3.connect(database) as connection:
        tracks = connection.execute(
            "SELECT relative_path, file_path FROM local_tracks ORDER BY relative_path"
        ).fetchall()
        provenance = connection.execute(
            "SELECT COUNT(*) FROM library_migration_provenance "
            "WHERE source_kind = 'library_file'"
        ).fetchone()[0]
    # All four seeded rows (2 identified + 2 review-derived) follow the same
    # projector onto the CURRENT mount, not the dead old mount.
    relative_paths = {row[0] for row in tracks}
    assert relative_paths == {
        "Compilation/01.flac",
        "Compilation/02.flac",
        "Local Album/01.flac",
        "Rejected/01.flac",
    }
    assert all(row[1].startswith(str(current_root)) for row in tracks)
    assert provenance == 2
    # Idempotency: the same input again is a no-op.
    assert await service.schedule() is False


@pytest.mark.asyncio
async def test_pending_migration_keeps_wrong_size_destination_pending(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """NEW-MIG-03 negative boundary: a destination whose size does not match the
    legacy evidence makes the all-row proof block the remap - nothing is
    guessed, and the rows stay pending and retryable."""
    old_root = tmp_path / "Old" / "Music"
    current_root = tmp_path / "Current" / "Music"
    database = tmp_path / "library.db"
    _create_source(database, old_root)
    store = _store(database)

    current_compilation = current_root / "Compilation"
    current_compilation.mkdir(parents=True)
    (current_compilation / "01.flac").write_bytes(b"a" * 100)
    # Wrong size for one destination: the all-row proof must reject the remap.
    (current_compilation / "02.flac").write_bytes(b"b" * 12345)

    monkeypatch.setattr(
        "services.native.legacy_path_reconciler._BLOCKED_ROOTS", (Path("/"),)
    )

    await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Unrelated")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    resolver = _resolver(("root", current_root))
    service = LegacyPendingMigrationService(store, lambda: resolver)
    original_run = service._run

    async def run_and_reset(run_id: str) -> None:
        try:
            await original_run(run_id)
        finally:
            service._running = False

    service._run = run_and_reset

    assert await service.schedule() is True
    while service._running:
        await asyncio.sleep(0.01)

    with sqlite3.connect(database) as connection:
        track_count = connection.execute(
            "SELECT COUNT(*) FROM local_tracks"
        ).fetchone()[0]
    assert track_count == 0  # nothing was guessed
    counts = await store.get_pending_legacy_counts()
    assert counts["library_file"] == 2  # rows remain pending and retryable


@pytest.mark.asyncio
async def test_policy_revision_change_during_pending_run_fails_closed(
    tmp_path: Path,
) -> None:
    """F1/H1: a roots/policy save racing an in-flight pending run must abort
    the run non-completed instead of silently completing under the captured
    projector. The untouched rows stay pending and the next schedule imports
    them exactly once under the new revision."""
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)

    # Lenient cutover with an unrelated root: durable marker exists and every
    # legacy row stays pending (established A→B seeding pattern).
    await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Unrelated")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)
    counts = await store.get_pending_legacy_counts()
    assert counts["library_file"] == 2
    assert counts["review_row"] == 4

    resolver = _resolver(("root", historical_root))
    policy_a = resolver.policy_revision
    revision = await store.get_bounded_legacy_source_revision()
    stale_run_id = pending_run_id(policy_a, revision)

    original_apply = store.apply_reference_provenance_batch
    applied = {"batches": 0}

    async def spying_apply(rows, **kwargs):
        result = await original_apply(rows, **kwargs)
        applied["batches"] += 1
        if applied["batches"] == 1:
            # The roots save lands right after the first applied bundle.
            resolver.policy_revision = "policy-b"
        return result

    store.apply_reference_provenance_batch = spying_apply  # type: ignore[method-assign]

    migrator = BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    )
    with pytest.raises(StaleRevisionError):
        await migrator.migrate_pending(stale_run_id)

    with sqlite3.connect(database) as connection:
        state = connection.execute(
            "SELECT state FROM library_migration_runs WHERE id = ?",
            (stale_run_id,),
        ).fetchone()[0]
    assert state != "completed"
    counts = await store.get_pending_legacy_counts()
    assert counts["library_file"] == 0  # imported before the policy flip
    assert counts["review_row"] == 4  # never processed: still retryable

    # Follow-up schedule runs under the new revision and converges.
    service = LegacyPendingMigrationService(store, lambda: resolver)
    original_run = service._run

    async def run_and_reset(run_id: str) -> None:
        try:
            await original_run(run_id)
        finally:
            service._running = False

    service._run = run_and_reset
    candidate = pending_run_id(resolver.policy_revision, revision)
    assert candidate != stale_run_id
    assert await store.get_migration_run_state(candidate) != "completed"
    assert await service.schedule() is True
    while service._running:
        await asyncio.sleep(0.01)

    with sqlite3.connect(database) as connection:
        final_state = connection.execute(
            "SELECT state FROM library_migration_runs WHERE id = ?",
            (candidate,),
        ).fetchone()[0]
        review_provenance = connection.execute(
            "SELECT COUNT(*) FROM library_migration_provenance "
            "WHERE source_kind = 'review_row'"
        ).fetchone()[0]
    counts = await store.get_pending_legacy_counts()
    assert counts["review_row"] == 0
    assert counts["library_file"] == 0
    assert review_provenance == 4  # imported exactly once, under P2
    assert await service.schedule() is False


async def _seed_skip_run(
    store: NativeLibraryStore, run_id: str, revision: str = "rev1"
) -> None:
    await store.save_migration_dry_run(
        run_id,
        source_revision=revision,
        root_revision="root1",
        report_json="{}",
        created_at=100.0,
    )


def _provenance(
    kind: str, key: str, target_kind: str, target_id: str, revision: str = "rev1"
) -> MigrationProvenance:
    return MigrationProvenance(
        source_kind=kind,
        source_key=key,
        target_kind=target_kind,
        target_id=target_id,
        source_revision=revision,
        imported_at=100.0,
    )


@pytest.mark.asyncio
async def test_reference_batch_skips_poison_row_and_commits_healthy(
    tmp_path: Path,
) -> None:
    """GH-367: one unmaterializable reference must not abort its batch.

    The healthy compat queue row commits with provenance while the favorite
    whose target vanished is reported as skipped with no residue.
    """
    database = tmp_path / "library.db"
    _create_source(database, tmp_path / "Historical" / "Music")
    store = _store(database)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO user_favorites VALUES ('alice','track','ghost-track-1',1.0)"
        )
        connection.execute(
            "INSERT INTO library_compat_play_queues "
            "(user_id, current_index, position_ms, updated_at, changed_by_client) "
            "VALUES ('alice',0,0,1.0,'t')"
        )
        connection.commit()
    run_id = "test-skip-run"
    await _seed_skip_run(store, run_id)
    good = _provenance("compat_play_queue", "alice", "compat_play_queue", "alice")
    bad = _provenance("favorite", "alice:track:ghost-track-1", "local_track", "ghost-track-1")
    result = await store.apply_reference_provenance_batch(
        [good, bad],
        migration_run_id=run_id,
        source_revision="rev1",
        copy_playlists=False,
    )
    assert result.inserted == 1
    assert int(result) == 1
    assert [(skip.source_kind, skip.source_key, skip.reason) for skip in result.skipped] == [
        ("favorite", "alice:track:ghost-track-1", "not_materialized")
    ]
    with sqlite3.connect(database) as connection:
        provenance = {
            (row[0], row[1])
            for row in connection.execute(
                "SELECT source_kind, source_key FROM library_migration_provenance"
            ).fetchall()
        }
        assert provenance == {("compat_play_queue", "alice")}
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_user_favorites WHERE item_id = ?",
                ("ghost-track-1",),
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_compat_play_queues WHERE user_id = 'alice'"
            ).fetchone()[0]
            == 1
        )


@pytest.mark.asyncio
async def test_reference_batch_skips_poison_shapes(tmp_path: Path) -> None:
    """GH-367/F2: every poison shape skips instead of aborting the batch.

    Malformed favorite keys raise before any write (``invalid_key``); rows
    whose inserts violate target constraints raise from the insert
    (``integrity_error``); rows gated on a vanished target insert nothing
    and fail verification (``not_materialized``).
    """
    database = tmp_path / "library.db"
    _create_source(database, tmp_path / "Historical" / "Music")
    store = _store(database)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO play_history VALUES "
            "('hist-null','alice',NULL,'Ghost Artist','Ghost Album',"
            "NULL,NULL,180000,'local','2026-07-01T12:00:00Z')"
        )
        connection.executemany(
            "INSERT INTO playlist_tracks VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    "pt-poison",
                    "ghost-playlist",
                    0,
                    "Ghost",
                    "Ghost Artist",
                    "Ghost Album",
                    None,
                    None,
                    None,
                    None,
                    "local",
                    None,
                    None,
                    None,
                    None,
                    None,
                    "a",
                    None,
                    None,
                ),
            ],
        )
        connection.commit()
    run_id = "test-skip-shapes"
    await _seed_skip_run(store, run_id)
    malformed = _provenance("favorite", "nocolon", "local_track", "ghost-track-1")
    null_column = _provenance("history", "hist-null", "local_track", "ghost-track-1")
    missing_parent = _provenance(
        "playlist_track", "pt-poison", "local_track", "ghost-track-9"
    )
    missing_queue = _provenance(
        "compat_play_queue_item", "alice:9", "local_track", "ghost-track-9"
    )
    result = await store.apply_reference_provenance_batch(
        [malformed, null_column, missing_parent, missing_queue],
        migration_run_id=run_id,
        source_revision="rev1",
        copy_playlists=False,
    )
    assert result.inserted == 0
    assert [(skip.source_kind, skip.reason) for skip in result.skipped] == [
        ("favorite", "invalid_key"),
        ("history", "integrity_error"),
        ("playlist_track", "integrity_error"),
        ("compat_play_queue_item", "not_materialized"),
    ]
    with sqlite3.connect(database) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_migration_provenance"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_play_history WHERE id = 'hist-null'"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_playlist_tracks WHERE id = 'pt-poison'"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_compat_play_queue_items "
                "WHERE user_id = 'alice' AND item_index = 9"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_reference_batch_skip_leaves_no_library_residue(tmp_path: Path) -> None:
    """GH-367/F3: a skipped row's partial materialization rolls back."""
    database = tmp_path / "library.db"
    _create_source(database, tmp_path / "Historical" / "Music")
    store = _store(database)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO play_history VALUES "
            "('hist-poison','alice','Ghost','Ghost Artist','Ghost Album',"
            "NULL,NULL,180000,'local','2026-07-01T12:00:00Z')"
        )
        connection.execute(
            "INSERT INTO album_release_pins VALUES ('rg-poison','rel-poison','a','t')"
        )
        connection.execute(
            "INSERT INTO library_playlists "
            "(id, name, cover_image_path, created_at, updated_at, source_ref, "
            "user_id, is_public) VALUES ('playlist-1','Mix',NULL,'a','b',NULL,NULL,0)"
        )
        connection.executemany(
            "INSERT INTO playlist_tracks VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    "pt-orphan",
                    "playlist-1",
                    0,
                    "Ghost",
                    "Ghost Artist",
                    "Ghost Album",
                    None,
                    None,
                    None,
                    None,
                    "local",
                    None,
                    None,
                    None,
                    None,
                    None,
                    "a",
                    None,
                    None,
                ),
            ],
        )
        connection.commit()
    run_id = "test-skip-residue"
    await _seed_skip_run(store, run_id)
    result = await store.apply_reference_provenance_batch(
        [
            _provenance("history", "hist-poison", "local_track", "ghost-track-1"),
            _provenance("album_release_pin", "rg-poison", "local_album", "ghost-album"),
            _provenance("playlist_track", "pt-orphan", "local_track", "ghost-track-1"),
        ],
        migration_run_id=run_id,
        source_revision="rev1",
        copy_playlists=False,
    )
    assert result.inserted == 0
    assert len(result.skipped) == 3
    with sqlite3.connect(database) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_play_history WHERE id = 'hist-poison'"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_album_release_pins "
                "WHERE release_group_mbid = 'rg-poison'"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_playlist_tracks WHERE id = 'pt-orphan'"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_pending_migration_completes_with_poison_reference_and_reports_skip(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    """GH-367 end to end: a poison reference completes the run as a skip.

    Counts reconcile so reference validation passes, the skip surfaces in
    ``skipped_counts``, and the completed run id suppresses rescheduling
    under the same revision (terminal skip, F4).
    """
    historical_root = tmp_path / "Historical" / "Music"
    (historical_root / "Compilation").mkdir(parents=True)
    (historical_root / "Compilation" / "01.flac").write_bytes(b"a" * 100)
    (historical_root / "Compilation" / "02.flac").write_bytes(b"b" * 200)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)
    await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Missing" / "Music")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)

    resolver = _resolver(("root", historical_root))
    revision = await store.get_bounded_legacy_source_revision()
    run_id = pending_run_id(resolver.policy_revision, revision)

    original_resolve = store.resolve_bounded_legacy_references
    poisoned = {"done": False}

    async def poisoned_resolve(
        kind: str, rows: list[dict]
    ) -> list[tuple[str, str] | None]:
        targets = await original_resolve(kind, rows)
        if kind == "favorite" and not poisoned["done"]:
            poisoned_targets = list(targets)
            for index, target in enumerate(poisoned_targets):
                if target is not None:
                    poisoned_targets[index] = ("local_track", "ghost-track-1")
                    poisoned["done"] = True
                    break
            return poisoned_targets
        return targets

    store.resolve_bounded_legacy_references = poisoned_resolve  # type: ignore[method-assign]
    with caplog.at_level(logging.WARNING):
        outcome = await BoundedLegacyCatalogMigrator(
            store,
            resolver,
            emit_progress=lambda _message: None,
            batch_size=1,
            skip_unmappable_paths=True,
        ).migrate_pending(run_id, now=200)
    assert poisoned["done"] is True
    assert outcome.blocker_count == 0
    assert outcome.skipped_counts.get("favorite", 0) == 1
    assert outcome.report.state == "applied"
    skip_records = [
        record for record in caplog.records if "legacy reference skipped" in record.getMessage()
    ]
    assert len(skip_records) == 1
    assert "kind=favorite" in skip_records[0].getMessage()
    assert "alice" not in skip_records[0].getMessage()
    with sqlite3.connect(database) as connection:
        assert (
            connection.execute(
                "SELECT state FROM library_migration_runs WHERE id = ?", (run_id,)
            ).fetchone()[0]
            == "completed"
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_user_favorites WHERE item_id = ?",
                ("ghost-track-1",),
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute("SELECT COUNT(*) FROM library_user_favorites").fetchone()[0]
            == 2
        )
    counts = await store.get_pending_legacy_counts()
    assert counts["favorite"] > 0
    service = LegacyPendingMigrationService(store, lambda: resolver)
    assert await service.schedule() is False


@pytest.mark.asyncio
async def test_reference_batch_skips_gated_branches_with_live_target(
    tmp_path: Path,
) -> None:
    """GH-367/F2: gated branches skip on live targets too.

    A compat queue item whose parent queue vanished raises from its insert
    (``integrity_error``); a compat bookmark with a malformed key raises
    from key parsing (``invalid_key``). Both need a real target track, so
    this runs a genuine cutover first instead of hand-seeding catalog rows.
    """
    historical_root = tmp_path / "Historical" / "Music"
    (historical_root / "Compilation").mkdir(parents=True)
    (historical_root / "Compilation" / "01.flac").write_bytes(b"a" * 100)
    (historical_root / "Compilation" / "02.flac").write_bytes(b"b" * 200)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)
    outcome = await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", historical_root)),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("cutover-live-target", now=100)
    assert outcome.blocker_count == 0
    with sqlite3.connect(database) as connection:
        track_id = connection.execute("SELECT id FROM local_tracks LIMIT 1").fetchone()[0]
        connection.execute("DELETE FROM library_compat_play_queues WHERE user_id='alice'")
        connection.commit()
    run_id = "test-live-target"
    await _seed_skip_run(store, run_id)
    result = await store.apply_reference_provenance_batch(
        [
            _provenance("compat_play_queue_item", "alice:7", "local_track", track_id),
            _provenance("compat_bookmark", "nocolon", "local_track", track_id),
        ],
        migration_run_id=run_id,
        source_revision="rev1",
        copy_playlists=False,
    )
    assert result.inserted == 0
    assert [(skip.source_kind, skip.reason) for skip in result.skipped] == [
        ("compat_play_queue_item", "integrity_error"),
        ("compat_bookmark", "invalid_key"),
    ]
    with sqlite3.connect(database) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_compat_play_queue_items "
                "WHERE user_id = 'alice' AND item_index = 7"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_compat_bookmarks "
                "WHERE user_id = 'nocolon'"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_reference_batch_skips_verification_parse_error(tmp_path: Path) -> None:
    """GH-367 re-review: verification-time key parsing must skip, not abort.

    A compat queue item whose target exists but is not a consumable track
    no-ops in materialize (track_id gate) and reaches verification, where
    the malformed item_index key raises. That ValueError must record an
    invalid_key skip while co-batched healthy rows commit; a malformed key
    with a missing track target fails verification as not_materialized.
    """
    database = tmp_path / "library.db"
    _create_source(database, tmp_path / "Historical" / "Music")
    store = _store(database)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO library_compat_play_queues "
            "(user_id, current_index, position_ms, updated_at, changed_by_client) "
            "VALUES ('alice',0,0,1.0,'t')"
        )
        connection.execute(
            "INSERT INTO library_playlists "
            "(id, name, cover_image_path, created_at, updated_at, source_ref, "
            "user_id, is_public) VALUES ('playlist-1','Mix',NULL,'a','b',NULL,NULL,0)"
        )
        connection.commit()
    run_id = "test-skip-verify-parse"
    await _seed_skip_run(store, run_id)
    good = _provenance("compat_play_queue", "alice", "compat_play_queue", "alice")
    verify_parse = _provenance(
        "compat_play_queue_item", "alice:xyz", "playlist", "playlist-1"
    )
    dead_target = _provenance(
        "compat_play_queue_item", "nocolon", "local_track", "ghost-track-1"
    )
    result = await store.apply_reference_provenance_batch(
        [good, verify_parse, dead_target],
        migration_run_id=run_id,
        source_revision="rev1",
        copy_playlists=False,
    )
    assert result.inserted == 1
    assert [
        (skip.source_kind, skip.source_key, skip.reason) for skip in result.skipped
    ] == [
        ("compat_play_queue_item", "alice:xyz", "invalid_key"),
        ("compat_play_queue_item", "nocolon", "not_materialized"),
    ]
    with sqlite3.connect(database) as connection:
        provenance = {
            (row[0], row[1])
            for row in connection.execute(
                "SELECT source_kind, source_key FROM library_migration_provenance"
            ).fetchall()
        }
        assert provenance == {("compat_play_queue", "alice")}
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_compat_play_queue_items"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_pending_migration_resume_counts_prior_provenance(
    tmp_path: Path,
) -> None:
    """GH-308: a resumed pending run validates prior plus new provenance.

    A retried pending run skips already-provenanced keys, so the in-memory
    expectation must be seeded with this run's durable counts; otherwise
    the final reference-count validation sees prior rows as
    durable-but-uncounted and fails.
    """
    historical_root = tmp_path / "Historical" / "Music"
    _write_catalog_files(historical_root)
    database = tmp_path / "library.db"
    _create_source(database, historical_root)
    store = _store(database)

    first = await BoundedLegacyCatalogMigrator(
        store,
        _resolver(("root", tmp_path / "Missing" / "Music")),
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate("lenient-migration", now=100)
    assert first.report.state == "applied"

    resolver = _resolver(("root", historical_root))
    pending_run_id = f"legacy-pending-{resolver.policy_revision}"
    # Simulate a pending run that wrote one provenance row then stopped
    # before completion: attribute it to the pending run id so the resume
    # skips it while validation still expects it.
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_migration_provenance SET migration_run_id = ? "
            "WHERE source_kind = 'compat_play_queue' AND source_key = 'alice'",
            (pending_run_id,),
        )
        connection.commit()

    resumed = await BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    ).migrate_pending(pending_run_id, now=200)

    assert resumed.blocker_count == 0
    assert resumed.report.state == "applied"
    durable = await store.get_migration_provenance_counts(pending_run_id)
    assert durable["compat_play_queue"] == 1
    by_kind = {
        count.kind: count
        for count in resumed.report.reference_counts
        if count.user_id is None
    }
    for kind, total in durable.items():
        assert by_kind[kind].mapped == total
    assert by_kind["compat_play_queue"].source == durable["compat_play_queue"]
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone() == (
            4,
        )
