"""Indexed task and conversion lookups for stored import bundles (#517)."""

from __future__ import annotations

import hashlib
import sqlite3
import threading
from pathlib import Path

import msgspec
import pytest

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioInfo, AudioTag
from models.library_management import (
    LibraryManagementImportBundle,
    LibraryManagementImportBundleRecord,
    LibraryManagementImportFile,
)
from services.native.target_application_lifecycle import (
    backfill_import_bundle_lookups,
)


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users(id) VALUES ('admin')")
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    return NativeLibraryStore(db_path, threading.Lock())


def _file(ordinal: int, task_id: str | None) -> LibraryManagementImportFile:
    return LibraryManagementImportFile(
        ordinal=ordinal,
        input_path=f"/incoming/{ordinal:02d}.flac",
        destination_root_id="root",
        destination_relative_path=f"Artist/Album/{ordinal:02d}.flac",
        tag=AudioTag(
            title=f"Track {ordinal}",
            artist="Artist",
            album="Album",
            track_number=ordinal + 1,
        ),
        info=AudioInfo(
            duration_seconds=180.0,
            bitrate=800,
            sample_rate=44100,
            channels=2,
            file_format="flac",
            file_size_bytes=100,
        ),
        release_group_mbid=None,
        release_mbid=None,
        recording_mbid=None,
        confidence=0.0,
        source="test",
        download_task_id=task_id,
    )


def _record(
    bundle_id: str,
    task_ids: tuple[str | None, ...],
    *,
    origin: str = "acquisition",
    created_at: float = 1.0,
    conversion_job_id: str | None = None,
) -> LibraryManagementImportBundleRecord:
    bundle = LibraryManagementImportBundle(
        idempotency_key=f"key-{bundle_id}",
        origin="edition_conversion" if conversion_job_id else origin,  # type: ignore[arg-type]
        policy_revision="policy-1",
        files=tuple(_file(index, task_id) for index, task_id in enumerate(task_ids)),
        conversion_job_id=conversion_job_id,
    )
    request_json = msgspec.json.encode(bundle).decode()
    return LibraryManagementImportBundleRecord(
        id=bundle_id,
        idempotency_key=bundle.idempotency_key,
        origin=origin,  # type: ignore[arg-type]
        policy_revision="policy-1",
        request_json=request_json,
        request_hash=hashlib.sha256(request_json.encode()).hexdigest(),
        state="preparing",
        created_at=created_at,
        updated_at=created_at,
    )


async def _seed_bundles(store: NativeLibraryStore) -> None:
    # `shared` is on three bundles (one of another origin) to exercise ordering.
    await store.ensure_library_management_import_bundle(
        _record("bundle-b", ("task-1", "shared"), created_at=2.0)
    )
    await store.ensure_library_management_import_bundle(
        _record("bundle-a", ("shared", "shared", None), created_at=2.0)
    )
    await store.ensure_library_management_import_bundle(
        _record("bundle-c", ("shared",), origin="drop_import", created_at=1.0)
    )
    await store.ensure_library_management_import_bundle(
        _record("bundle-d", ("shared", "task-2"), created_at=1.5)
    )
    await store.ensure_library_management_import_bundle(_record("bundle-e", (None,)))


def _clear_lookups(db_path: Path) -> None:
    """Return the database to its shape before the lookup tables existed."""
    with sqlite3.connect(db_path) as connection:
        connection.execute("DELETE FROM library_management_import_bundle_tasks")
        connection.execute("DELETE FROM library_management_import_bundle_conversions")
        connection.execute("DELETE FROM library_migration_markers")


def _lookup_rows(db_path: Path) -> set[tuple[str, str]]:
    with sqlite3.connect(db_path) as connection:
        return {
            (str(row[0]), str(row[1]))
            for row in connection.execute(
                "SELECT download_task_id,bundle_id "
                "FROM library_management_import_bundle_tasks"
            )
        }


async def _task_bundle_ids(store: NativeLibraryStore, task_id: str) -> list[str]:
    return [
        bundle.id
        for bundle in await store.list_acquisition_import_bundles_for_download_task(
            task_id
        )
    ]


@pytest.mark.asyncio
async def test_write_path_fills_lookup_tables(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)
    await store.ensure_library_management_import_bundle(
        _record("bundle-f", ("task-3",), conversion_job_id="job-1")
    )

    assert _lookup_rows(db_path) == {
        ("task-1", "bundle-b"),
        ("shared", "bundle-b"),
        ("shared", "bundle-a"),
        ("shared", "bundle-c"),
        ("shared", "bundle-d"),
        ("task-2", "bundle-d"),
        ("task-3", "bundle-f"),
    }
    with sqlite3.connect(db_path) as connection:
        conversions = connection.execute(
            "SELECT conversion_job_id,bundle_id "
            "FROM library_management_import_bundle_conversions"
        ).fetchall()
    assert conversions == [("job-1", "bundle-f")]


@pytest.mark.asyncio
async def test_idempotent_retry_does_not_duplicate_lookup_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    record = _record("bundle-a", ("task-1", "task-1"))
    await store.ensure_library_management_import_bundle(record)
    _saved, created = await store.ensure_library_management_import_bundle(record)

    assert created is False
    assert _lookup_rows(db_path) == {("task-1", "bundle-a")}


@pytest.mark.asyncio
async def test_indexed_lookup_matches_json_scan(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)
    task_ids = ("shared", "task-1", "task-2", "missing")

    # No completion marker yet, so this is the JSON scan the index replaces.
    scanned = {task_id: await _task_bundle_ids(store, task_id) for task_id in task_ids}
    await store.complete_import_bundle_lookup_backfill(completed_at=3.0)
    indexed = {task_id: await _task_bundle_ids(store, task_id) for task_id in task_ids}

    assert indexed == scanned
    # created_at then id, acquisition only (bundle-c is a drop import).
    assert indexed["shared"] == ["bundle-d", "bundle-a", "bundle-b"]
    assert indexed["missing"] == []


@pytest.mark.asyncio
async def test_lookup_is_correct_before_and_after_backfill(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)
    _clear_lookups(db_path)

    assert await _task_bundle_ids(store, "task-2") == ["bundle-d"]
    assert await _task_bundle_ids(store, "shared") == [
        "bundle-d",
        "bundle-a",
        "bundle-b",
    ]

    await backfill_import_bundle_lookups(store)

    assert await store.has_completed_import_bundle_lookup_backfill()
    assert await _task_bundle_ids(store, "task-2") == ["bundle-d"]
    assert await _task_bundle_ids(store, "shared") == [
        "bundle-d",
        "bundle-a",
        "bundle-b",
    ]
    # With the marker present the answer comes from the lookup table.
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "DELETE FROM library_management_import_bundle_tasks "
            "WHERE download_task_id='task-2'"
        )
    assert await _task_bundle_ids(store, "task-2") == []


@pytest.mark.asyncio
async def test_backfill_is_idempotent(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)
    expected = _lookup_rows(db_path)
    _clear_lookups(db_path)

    await backfill_import_bundle_lookups(store)
    first = _lookup_rows(db_path)
    with sqlite3.connect(db_path) as connection:
        marker_count = connection.execute(
            "SELECT COUNT(*) FROM library_migration_markers"
        ).fetchone()[0]
    await backfill_import_bundle_lookups(store)
    await store.complete_import_bundle_lookup_backfill(completed_at=9.0)

    assert first == expected
    assert _lookup_rows(db_path) == expected
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute("SELECT COUNT(*) FROM library_migration_markers")
            .fetchone()[0]
            == marker_count
            == 1
        )


@pytest.mark.asyncio
async def test_backfill_resumes_after_interruption(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)
    expected = _lookup_rows(db_path)
    _clear_lookups(db_path)

    # One bounded batch, then the process "dies" before the marker is written.
    last_id = await store.backfill_import_bundle_lookups(limit=2)

    assert last_id == "bundle-b"
    partial = _lookup_rows(db_path)
    assert partial and partial < expected
    assert not await store.has_completed_import_bundle_lookup_backfill()
    # Bundles stored before the interruption still resolve through the scan.
    assert await _task_bundle_ids(store, "task-1") == ["bundle-b"]

    # A restart begins from the top but skips what is already indexed.
    batches: list[str] = []
    original = store.backfill_import_bundle_lookups

    async def spy(**kwargs: object) -> str | None:
        last = await original(**kwargs)  # type: ignore[arg-type]
        if last is not None:
            batches.append(last)
        return last

    store.backfill_import_bundle_lookups = spy  # type: ignore[method-assign]
    await backfill_import_bundle_lookups(store)

    assert _lookup_rows(db_path) == expected
    assert await store.has_completed_import_bundle_lookup_backfill()
    assert batches == ["bundle-e"]


@pytest.mark.asyncio
async def test_backfill_indexes_conversion_job(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.ensure_library_management_import_bundle(
        _record("bundle-f", (None,), conversion_job_id="job-1")
    )
    _clear_lookups(db_path)

    await backfill_import_bundle_lookups(store)

    with sqlite3.connect(db_path) as connection:
        assert connection.execute(
            "SELECT conversion_job_id,bundle_id "
            "FROM library_management_import_bundle_conversions"
        ).fetchall() == [("job-1", "bundle-f")]


@pytest.mark.asyncio
async def test_backfill_skips_unreadable_bundle(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)
    _clear_lookups(db_path)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_management_import_bundles "
            "SET request_json='not json' WHERE id='bundle-a'"
        )

    await backfill_import_bundle_lookups(store)

    assert await store.has_completed_import_bundle_lookup_backfill()
    assert ("task-1", "bundle-b") in _lookup_rows(db_path)
    assert not any(bundle_id == "bundle-a" for _task, bundle_id in _lookup_rows(db_path))


@pytest.mark.asyncio
async def test_zero_bundles(store: NativeLibraryStore, db_path: Path) -> None:
    assert await _task_bundle_ids(store, "task-1") == []

    assert await store.backfill_import_bundle_lookups() is None
    await backfill_import_bundle_lookups(store)

    assert await store.has_completed_import_bundle_lookup_backfill()
    assert await _task_bundle_ids(store, "task-1") == []
    assert _lookup_rows(db_path) == set()


def _seed_conversion(db_path: Path) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_operation_jobs "
            "(id,kind,state,started_at,created_at,updated_at) "
            "VALUES ('preview','library_management','running',1,1,1)"
        )
        connection.execute(
            "INSERT INTO library_edition_conversion_jobs "
            "(id,local_album_id,target_release_group_mbid,target_release_mbid,"
            "target_album_title,target_artist_name,state,expected_album_revision,"
            "expected_input_revision,expected_identity_revision,"
            "preflight_token_hash,download_source_ready,requested_by_user_id,"
            "created_at,updated_at) "
            "VALUES ('job-1','album-1','rg','release','Album','Artist','ready',1,"
            "'input','identity','token',1,'admin',1,1)"
        )


async def _preview_state(db_path: Path) -> str:
    with sqlite3.connect(db_path) as connection:
        return str(
            connection.execute(
                "SELECT state FROM library_operation_jobs WHERE id='preview'"
            ).fetchone()[0]
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("backfilled", [False, True])
async def test_conversion_lookup_guards_reset_for_committed_bundle(
    store: NativeLibraryStore, db_path: Path, backfilled: bool
) -> None:
    _seed_conversion(db_path)
    committed = _record("bundle-f", (None,), conversion_job_id="job-1")
    await store.ensure_library_management_import_bundle(committed)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_management_import_bundles SET state='completed'"
        )
    if backfilled:
        await backfill_import_bundle_lookups(store)

    await store.reset_edition_conversion_apply(
        "job-1", preview_job_id="preview", now=5.0
    )

    assert await _preview_state(db_path) == "running"


@pytest.mark.asyncio
@pytest.mark.parametrize("backfilled", [False, True])
async def test_conversion_lookup_resets_without_committed_bundle(
    store: NativeLibraryStore, db_path: Path, backfilled: bool
) -> None:
    _seed_conversion(db_path)
    await store.ensure_library_management_import_bundle(
        _record("bundle-f", (None,), conversion_job_id="job-1")
    )
    if backfilled:
        await backfill_import_bundle_lookups(store)

    await store.reset_edition_conversion_apply(
        "job-1", preview_job_id="preview", now=5.0
    )

    assert await _preview_state(db_path) == "ready"


@pytest.mark.asyncio
async def test_conversion_reset_without_any_bundle(
    store: NativeLibraryStore, db_path: Path
) -> None:
    _seed_conversion(db_path)
    await backfill_import_bundle_lookups(store)

    await store.reset_edition_conversion_apply(
        "job-1", preview_job_id="preview", now=5.0
    )

    assert await _preview_state(db_path) == "ready"


@pytest.mark.asyncio
async def test_state_index_serves_recovery_and_diagnostics_scans(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)

    await store.ensure_import_bundle_state_index()
    await store.ensure_import_bundle_state_index()

    with sqlite3.connect(db_path) as connection:
        recovery_plan = " ".join(
            str(row[3])
            for row in connection.execute(
                "EXPLAIN QUERY PLAN SELECT * FROM library_management_import_bundles "
                "WHERE state IN ('preparing','publishing') "
                "ORDER BY updated_at,id LIMIT 100"
            )
        )
        diagnostics_plan = " ".join(
            str(row[3])
            for row in connection.execute(
                "EXPLAIN QUERY PLAN SELECT state,COUNT(*),MIN(updated_at) "
                "FROM library_management_import_bundles "
                "WHERE state NOT IN ('completed','rolled_back','resolved') "
                "GROUP BY state"
            )
        )
    assert "idx_import_bundles_state" in recovery_plan
    assert "idx_import_bundles_state" in diagnostics_plan
    assert [
        bundle.id
        for bundle in await store.list_recoverable_library_management_import_bundles()
    ] == ["bundle-c", "bundle-e", "bundle-d", "bundle-a", "bundle-b"]


@pytest.mark.asyncio
async def test_lookup_rows_are_cascaded_with_their_bundle(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_bundles(store)

    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "DELETE FROM library_management_import_bundles WHERE id='bundle-d'"
        )

    assert all(bundle_id != "bundle-d" for _task, bundle_id in _lookup_rows(db_path))
