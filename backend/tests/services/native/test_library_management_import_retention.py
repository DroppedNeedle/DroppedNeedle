"""Finished import bundles expire, and the artwork only they held is freed (#517)."""

import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace

import pytest

from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.library_management import (
    LibraryManagementBlobReference,
    LibraryManagementImportJournal,
)
from repositories.protocols.download_client import TaskHandle
from services.native.library_management_import_retention import (
    LibraryManagementImportRetention,
)
from services.native.library_management_publisher import import_bundle_id
from tests.infrastructure.test_library_management_import_bundle_lookups import (
    _record,
)

DAY = 86400.0
NOW = 400 * DAY
OLD = 1.0
RECENT = NOW - 10 * DAY


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


async def _bundle(
    store: NativeLibraryStore,
    db_path: Path,
    name: str,
    state: str,
    updated_at: float = OLD,
    *,
    conversion_job_id: str | None = None,
) -> str:
    await store.ensure_library_management_import_bundle(
        _record(name, (f"task-{name}",), conversion_job_id=conversion_job_id)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_management_import_bundles SET state=?,updated_at=? "
            "WHERE id=?",
            (state, updated_at, name),
        )
    return name


async def _attempt(
    db_path: Path, bundle_id: str, state: str, downloads: DownloadStore
) -> None:
    attempt = await downloads.create_download_attempt(
        task_id=f"task-{bundle_id}",
        source="soulseek",
        candidate_index=0,
        job_name=f"job-{bundle_id}",
        handle=TaskHandle(source="soulseek"),
        now=1.0,
    )
    await downloads.schedule_download_attempt_cleanup(
        attempt.id, disposition="discard", publisher_bundle_ids=[bundle_id], now=1.0
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE download_attempts SET state=? WHERE id=?", (state, attempt.id)
        )


def _ids(db_path: Path, table: str, column: str = "id") -> set[str]:
    with sqlite3.connect(db_path) as connection:
        return {str(row[0]) for row in connection.execute(f"SELECT {column} FROM {table}")}


@pytest.mark.asyncio
async def test_prune_removes_only_bundles_nothing_needs(
    store: NativeLibraryStore, db_path: Path
) -> None:
    downloads = DownloadStore(db_path, threading.Lock())
    for name, state, updated_at in (
        ("completed-old", "completed", OLD),
        ("resolved-old", "resolved", OLD),
        ("rolled-back-free", "rolled_back", OLD),
        ("completed-recent", "completed", RECENT),
        ("rolled-back-held", "rolled_back", OLD),
        ("completed-held", "completed", OLD),
        ("completed-done-attempt", "completed", OLD),
        ("attention", "needs_attention", OLD),
        ("publishing", "publishing", OLD),
        ("preparing", "preparing", OLD),
    ):
        await _bundle(store, db_path, name, state, updated_at)
    await _attempt(db_path, "rolled-back-held", "cleanup_pending", downloads)
    await _attempt(db_path, "completed-held", "needs_attention", downloads)
    await _attempt(db_path, "completed-done-attempt", "complete", downloads)
    await store.ensure_library_management_import_journal(
        LibraryManagementImportJournal(
            bundle_id="completed-old",
            ordinal=0,
            state="completed",
            source_fingerprint="f" * 64,
            source_size=1,
            source_mtime_ns=1,
            temporary_relative_path="tmp",
            destination_root_id="root",
            destination_relative_path="a.flac",
        )
    )
    await store.complete_import_bundle_lookup_backfill(completed_at=1.0)

    removed, more = await store.prune_terminal_import_bundles(
        updated_before=NOW - 90 * DAY
    )

    assert (removed, more) == (2, False)
    assert _ids(db_path, "library_management_import_bundles") == {
        "resolved-old",
        "rolled-back-free",
        "completed-recent",
        "rolled-back-held",
        "completed-held",
        "attention",
        "publishing",
        "preparing",
    }
    assert _ids(db_path, "library_management_import_journal", "bundle_id") == set()
    assert _ids(
        db_path, "library_management_import_bundle_tasks", "bundle_id"
    ) == {
        "resolved-old",
        "rolled-back-free",
        "completed-recent",
        "rolled-back-held",
        "completed-held",
        "attention",
        "publishing",
        "preparing",
    }
    assert await store.prune_terminal_import_bundles(updated_before=NOW - 90 * DAY) == (
        0,
        False,
    )


@pytest.mark.asyncio
async def test_prune_waits_for_the_lookup_backfill(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _bundle(store, db_path, "completed-old", "completed")

    assert await store.prune_terminal_import_bundles(updated_before=NOW) == (0, False)
    assert _ids(db_path, "library_management_import_bundles") == {"completed-old"}


@pytest.mark.asyncio
async def test_prune_keeps_bundles_of_unfinished_edition_conversions(
    store: NativeLibraryStore, db_path: Path
) -> None:
    with sqlite3.connect(db_path) as connection:
        for job_id, state in (("job-ready", "ready"), ("job-applied", "applied")):
            connection.execute(
                "INSERT INTO library_edition_conversion_jobs "
                "(id,local_album_id,target_release_group_mbid,target_release_mbid,"
                "target_album_title,target_artist_name,state,expected_album_revision,"
                "expected_input_revision,expected_identity_revision,"
                "preflight_token_hash,download_source_ready,requested_by_user_id,"
                "created_at,updated_at) "
                "VALUES (?,'album-1','rg','release','Album','Artist',?,1,"
                "'input','identity','token',1,'admin',1,1)",
                (job_id, state),
            )
    await _bundle(
        store, db_path, "bundle-ready", "completed", conversion_job_id="job-ready"
    )
    await _bundle(
        store, db_path, "bundle-applied", "completed", conversion_job_id="job-applied"
    )
    await store.complete_import_bundle_lookup_backfill(completed_at=1.0)

    assert await store.prune_terminal_import_bundles(updated_before=NOW) == (1, False)
    assert _ids(db_path, "library_management_import_bundles") == {"bundle-ready"}


@pytest.mark.asyncio
async def test_prune_never_removes_rolled_back_bundles(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _bundle(store, db_path, "rolled-back-old", "rolled_back")
    await store.complete_import_bundle_lookup_backfill(completed_at=1.0)

    assert await store.prune_terminal_import_bundles(updated_before=NOW) == (0, False)
    assert _ids(db_path, "library_management_import_bundles") == {"rolled-back-old"}


@pytest.mark.asyncio
async def test_prune_never_removes_resolved_bundles(
    store: NativeLibraryStore, db_path: Path
) -> None:
    # The orphan workspace reconcile keeps a task's folder while any of its
    # bundles is not completed, so pruning a resolved one would free the folder.
    await _bundle(store, db_path, "resolved-old", "resolved")
    await store.complete_import_bundle_lookup_backfill(completed_at=1.0)

    assert await store.prune_terminal_import_bundles(updated_before=NOW) == (0, False)
    assert _ids(db_path, "library_management_import_bundles") == {"resolved-old"}


@pytest.mark.asyncio
async def test_prune_runs_in_bounded_batches(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for index in range(5):
        await _bundle(store, db_path, f"bundle-{index}", "completed", OLD + index)
    await store.complete_import_bundle_lookup_backfill(completed_at=1.0)

    assert await store.prune_terminal_import_bundles(updated_before=NOW, limit=2) == (
        2,
        True,
    )
    assert await store.prune_terminal_import_bundles(updated_before=NOW, limit=2) == (
        2,
        True,
    )
    assert await store.prune_terminal_import_bundles(updated_before=NOW, limit=2) == (
        1,
        False,
    )


async def _reference(
    store: NativeLibraryStore,
    blobs: LibraryManagementBlobStore,
    content: bytes,
    reference_id: str,
    created_at: float = OLD,
) -> str:
    blob = await blobs.add_bytes(content, kind="image", created_at=OLD)
    await store.add_management_blob_reference(
        LibraryManagementBlobReference(
            blob_sha256=blob.sha256,
            reference_kind="artwork",
            reference_id=reference_id,
            created_at=created_at,
        )
    )
    return blob.sha256


def _blob_exists(blobs: LibraryManagementBlobStore, sha256: str) -> bool:
    return (blobs.root / "objects" / sha256[:2] / sha256[2:4] / f"{sha256}.blob").exists()


@pytest.mark.asyncio
async def test_retention_frees_only_artwork_that_nothing_references(
    store: NativeLibraryStore, db_path: Path, tmp_path: Path
) -> None:
    downloads = DownloadStore(db_path, threading.Lock())
    blobs = LibraryManagementBlobStore(tmp_path / "blobs", store)
    sealed_id = import_bundle_id("edition-conversion:job-sealed")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_edition_conversion_jobs "
            "(id,local_album_id,target_release_group_mbid,target_release_mbid,"
            "target_album_title,target_artist_name,state,expected_album_revision,"
            "expected_input_revision,expected_identity_revision,"
            "preflight_token_hash,download_source_ready,final_bundle_json,"
            "requested_by_user_id,created_at,updated_at) "
            "VALUES ('job-sealed','album-1','rg','release','Album','Artist','ready',"
            "1,'input','identity','token',1,'{}','admin',1,1)"
        )
    await _bundle(store, db_path, "expired", "completed")
    await _bundle(store, db_path, "held", "completed")
    await _bundle(store, db_path, "recent", "completed", RECENT)
    await _attempt(db_path, "held", "cleanup_pending", downloads)
    await store.complete_import_bundle_lookup_backfill(completed_at=1.0)
    freed = await _reference(store, blobs, b"expired-only", "import-bundle:expired")
    shared = await _reference(store, blobs, b"shared", "import-bundle:expired")
    await store.add_management_blob_reference(
        LibraryManagementBlobReference(
            blob_sha256=shared,
            reference_kind="artwork",
            reference_id="job-1:0:0",
            created_at=OLD,
        )
    )
    held = await _reference(store, blobs, b"held", "import-bundle:held")
    recent = await _reference(store, blobs, b"recent", "import-bundle:recent")
    orphaned = await _reference(store, blobs, b"orphaned", "import-bundle:failed-publish")
    in_flight = await _reference(
        store, blobs, b"in-flight", "import-bundle:publishing", NOW - 60
    )
    sealed = await _reference(
        store, blobs, b"sealed", f"import-bundle:{sealed_id}", OLD
    )
    retention = LibraryManagementImportRetention(
        store,
        blobs,
        SimpleNamespace(
            get_library_management_settings_raw=lambda: SimpleNamespace(
                undo_retention_days=90
            )
        ),
        clock=lambda: NOW,
    )

    result = await retention.prune()

    assert result.bundles_removed == 1
    assert result.references_removed == 1
    assert result.blobs_removed == 2
    assert _ids(db_path, "library_management_import_bundles") == {"held", "recent"}
    assert not _blob_exists(blobs, freed)
    assert not _blob_exists(blobs, orphaned)
    for survivor in (shared, held, recent, in_flight, sealed):
        assert _blob_exists(blobs, survivor)
        assert await blobs.read_bytes(survivor)
    assert _ids(db_path, "library_management_blob_references", "reference_id") == {
        "job-1:0:0",
        "import-bundle:held",
        "import-bundle:recent",
        "import-bundle:publishing",
        f"import-bundle:{sealed_id}",
    }

    again = await retention.prune()

    assert (again.bundles_removed, again.references_removed, again.blobs_removed) == (
        0,
        0,
        0,
    )


@pytest.mark.asyncio
async def test_readding_a_reference_restarts_its_orphan_grace_period(
    store: NativeLibraryStore, db_path: Path, tmp_path: Path
) -> None:
    blobs = LibraryManagementBlobStore(tmp_path / "blobs", store)
    await _reference(store, blobs, b"retried", "import-bundle:retried")
    await _reference(store, blobs, b"stale", "import-bundle:stale")

    await _reference(store, blobs, b"retried", "import-bundle:retried", NOW)

    assert await store.remove_orphaned_import_bundle_references(
        created_before=NOW - DAY
    ) == 1
    assert _ids(db_path, "library_management_blob_references", "reference_id") == {
        "import-bundle:retried"
    }


@pytest.mark.asyncio
async def test_blobs_held_by_the_import_journal_do_not_block_cleanup(
    store: NativeLibraryStore, db_path: Path, tmp_path: Path
) -> None:
    blobs = LibraryManagementBlobStore(tmp_path / "blobs", store)
    await _bundle(store, db_path, "journaled", "completed")
    held = [
        (await blobs.add_bytes(content, kind="image", created_at=OLD)).sha256
        for content in (b"held-1", b"held-2")
    ]
    free = (await blobs.add_bytes(b"free", kind="image", created_at=OLD + 1)).sha256
    for ordinal, sha256 in enumerate(held):
        await store.ensure_library_management_import_journal(
            LibraryManagementImportJournal(
                bundle_id="journaled",
                ordinal=ordinal,
                state="completed",
                source_fingerprint="f" * 64,
                source_size=1,
                source_mtime_ns=1,
                temporary_relative_path=f"tmp-{ordinal}",
                destination_root_id="root",
                destination_relative_path=f"{ordinal}.flac",
                baseline_blob_sha256=sha256,
            )
        )

    # A batch no larger than the held blobs would otherwise return only them.
    unreferenced = await store.list_unreferenced_management_blobs(
        older_than=NOW, limit=2
    )

    assert [blob.sha256 for blob in unreferenced] == [free]
    result = await blobs.cleanup(older_than=NOW, limit=2)
    assert result.unreferenced_blobs_removed == 1
    for sha256 in held:
        assert _blob_exists(blobs, sha256)
