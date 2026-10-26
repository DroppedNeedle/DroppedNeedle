"""Offline compaction moves artwork out of finished import rows (#517)."""

import hashlib
import json
from pathlib import Path
import sqlite3
import threading
from types import SimpleNamespace

import msgspec
import pytest

from infrastructure.audio.metadata_engine import AudioMetadataEngine
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.native_library_store import (
    NativeLibraryStore,
    _import_plan_documents,
)
from maintenance.library_management_artwork_compaction import (
    ArtworkCompactionError,
    compact_artwork_copy,
    main,
)
from models.audio_metadata import (
    DesiredAudioDocument,
    DesiredAudioField,
    EmbeddedArtworkDescriptor,
)
from models.library_management import (
    LibraryManagementImportArtifact,
    LibraryManagementImportBundle,
    LibraryManagementImportBundleRecord,
    LibraryManagementImportFile,
    LibraryManagementImportJournal,
)
from models.library_management_stored_artwork import (
    persisted_import_bundle,
    stored_artwork_descriptor,
)
from models.library_work import OperationJob
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native.library_management_publisher import (
    LibraryManagementPublisher,
    import_bundle_id,
)
from tests.infrastructure.test_library_management_store import (
    _job_snapshot,
    _plan_item,
    _seed_auth,
    _seed_catalog,
)
from tests.services.native.test_library_management_publisher import _import_file, _png

FIXTURE = Path(__file__).parents[1] / "fixtures/library/management_full.flac"
COVER = _png(b"\x10\x80\xf0")
EXTERNAL = _png(b"\xf0\x10\x20")


def _sha(value: bytes | str) -> str:
    return hashlib.sha256(value if isinstance(value, bytes) else value.encode()).hexdigest()


def _descriptor(content: bytes) -> EmbeddedArtworkDescriptor:
    return EmbeddedArtworkDescriptor(
        image_type="front",
        mime_type="image/png",
        description="Front",
        width=4,
        height=4,
        byte_size=len(content),
        sha256=_sha(content),
        content=content,
        format_supported=True,
    )


def _legacy_file(ordinal: int = 0) -> LibraryManagementImportFile:
    """A file as it was stored before the fix: artwork bytes inline."""

    return msgspec.structs.replace(
        _import_file(
            AudioMetadataEngine(),
            FIXTURE,
            ordinal=ordinal,
            relative_path=f"Artist/Album/{ordinal + 1:02d}.flac",
        ),
        desired_document=DesiredAudioDocument(
            fields=(DesiredAudioField(name="title", action="set", value="Managed"),),
            artwork=(_descriptor(COVER),),
        ),
        artifacts=(
            LibraryManagementImportArtifact(
                kind="external_art",
                destination_root_id="root-1",
                destination_relative_path="Artist/Album/cover.png",
                content=EXTERNAL,
                source_fingerprint=_sha(EXTERNAL),
            ),
        ),
    )


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    _seed_auth(path)
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    value = NativeLibraryStore(db_path, threading.Lock())
    _seed_catalog(db_path)
    return value


@pytest.fixture
def blob_root(tmp_path: Path) -> Path:
    path = tmp_path / "blobs"
    path.mkdir()
    return path


async def _add_bundle(
    store: NativeLibraryStore, key: str, state: str, *, ordinal: int = 0
) -> str:
    bundle = LibraryManagementImportBundle(
        idempotency_key=key,
        origin="acquisition",
        policy_revision="policy-1",
        files=(_legacy_file(ordinal),),
    )
    request_json = msgspec.json.encode(bundle).decode()
    record = LibraryManagementImportBundleRecord(
        id=import_bundle_id(key),
        idempotency_key=key,
        origin="acquisition",
        policy_revision="policy-1",
        request_json=request_json,
        request_hash=_sha(request_json),
        state="preparing",
        created_at=10.0,
        updated_at=20.0,
    )
    await store.ensure_library_management_import_bundle(record)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_management_import_bundles SET state=? WHERE id=?",
            (state, record.id),
        )
    return record.id


async def _add_plan_item(
    store: NativeLibraryStore, job_id: str, phase: str, *, catalog_differs: bool = False
) -> None:
    await store.create_library_management_job(
        OperationJob(
            id=job_id, kind="library_management", input_catalog_revision=0, created_at=10
        ),
        _job_snapshot(job_id),
    )
    desired_json = msgspec.json.encode(
        DesiredAudioDocument(
            fields=(DesiredAudioField(name="title", action="set", value="Managed"),),
            artwork=(_descriptor(COVER),),
        )
    ).decode()
    catalog_json = (
        '{"fields":[],"custom_tags":[]}' if catalog_differs else desired_json
    )
    await store.append_library_management_plan_items(
        job_id,
        [
            msgspec.structs.replace(
                _plan_item(job_id, 0),
                desired_document_json=desired_json,
                desired_document_hash=_sha(desired_json),
                catalog_document_json=catalog_json,
                catalog_document_hash=_sha(catalog_json),
            )
        ],
        expected_snapshot_revision=1,
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_management_job_snapshots SET phase=? WHERE job_id=?",
            (phase, job_id),
        )


def _rows(db_path: Path, table: str) -> list[dict]:
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        return [dict(row) for row in connection.execute(f"SELECT * FROM {table}")]


def _blob_files(blob_root: Path) -> list[Path]:
    return sorted(blob_root.glob("objects/*/*/*.blob"))


@pytest.mark.asyncio
async def test_dry_run_reports_and_changes_nothing(
    store: NativeLibraryStore, db_path: Path, blob_root: Path
) -> None:
    await _add_bundle(store, "acquisition:done", "completed")
    await _add_bundle(store, "acquisition:running", "publishing")
    await _add_plan_item(store, "job-complete", "complete")
    await _add_plan_item(store, "job-ready", "ready")
    before = {
        table: _rows(db_path, table)
        for table in (
            "library_management_import_bundles",
            "library_management_plan_items",
            "library_management_blobs",
            "library_management_blob_references",
        )
    }

    result = compact_artwork_copy(db_path, blob_root)

    assert result.dry_run is True
    assert result.bundles_compactable == 1
    assert result.plan_items_compactable == 1
    assert result.bundles_compacted == result.plan_items_compacted == 0
    assert result.skipped_non_terminal == 2
    assert result.skipped_unverifiable == 0
    assert result.logical_bytes_reclaimable > 0
    assert result.blobs_written == 0
    assert not _blob_files(blob_root)
    for table, rows in before.items():
        assert _rows(db_path, table) == rows


@pytest.mark.asyncio
async def test_apply_rewrites_terminal_rows_so_they_decode_and_hydrate_identically(
    store: NativeLibraryStore, db_path: Path, blob_root: Path
) -> None:
    legacy_file = _legacy_file()
    bundle_id = await _add_bundle(store, "acquisition:done", "completed")
    await _add_plan_item(store, "job-complete", "complete")
    legacy_bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:done",
        origin="acquisition",
        policy_revision="policy-1",
        files=(legacy_file,),
    )
    lookups = _rows(db_path, "library_management_import_bundle_tasks")

    result = compact_artwork_copy(db_path, blob_root, apply=True)

    assert (result.bundles_compacted, result.plan_items_compacted) == (1, 1)
    assert result.complete is True
    assert result.foreign_key_check_passed is True
    assert result.integrity_check_passed is True
    assert result.blobs_written == 2

    (bundle_row,) = _rows(db_path, "library_management_import_bundles")
    expected = msgspec.json.encode(persisted_import_bundle(legacy_bundle)).decode()
    assert bundle_row["request_json"] == expected
    assert bundle_row["request_hash"] == _sha(expected)
    assert bundle_row["state"] == "completed"
    assert bundle_row["updated_at"] == 20.0
    assert _rows(db_path, "library_management_import_bundle_tasks") == lookups
    # The cleanup barrier still reads the compacted bundle and its hash.
    record = await store.get_library_management_import_bundle(bundle_id)
    assert record is not None and _sha(record.request_json) == record.request_hash
    journal = LibraryManagementImportJournal(
        bundle_id=bundle_id,
        ordinal=0,
        state="completed",
        source_fingerprint="f" * 64,
        source_size=1,
        source_mtime_ns=1,
        temporary_relative_path="tmp",
        destination_root_id="root-1",
        destination_relative_path="a.flac",
    )
    await store.ensure_library_management_import_journal(journal)
    fingerprints = await AcquisitionCleanupService(
        store, store, lambda _source: None, lambda: db_path.parent
    )._publisher_source_fingerprints(
        SimpleNamespace(source="soulseek", publisher_bundle_ids=[bundle_id])
    )
    assert list(fingerprints.values()) == ["f" * 64]

    # The bytes come back from the blob store exactly as they were stored inline.
    blobs = LibraryManagementBlobStore(blob_root, store)
    stored = msgspec.json.decode(
        bundle_row["request_json"].encode(), type=LibraryManagementImportBundle
    ).files[0]
    assert stored.desired_artwork is not None
    assert (
        stored_artwork_descriptor(
            stored.desired_artwork[0],
            await blobs.read_bytes(stored.desired_artwork[0].sha256),
        )
        == legacy_file.desired_document.artwork[0]
    )
    assert await blobs.read_bytes(stored.artifacts[0].content_blob_sha256) == EXTERNAL
    references = {
        (row["blob_sha256"], row["reference_id"])
        for row in _rows(db_path, "library_management_blob_references")
    }
    assert (_sha(COVER), f"import-bundle:{bundle_id}") in references
    assert (_sha(EXTERNAL), f"import-bundle:{bundle_id}") in references

    # The plan item has the form a freshly written one has.
    item = await store.get_library_management_plan_item("job-complete", 0)
    assert item is not None
    desired_json, desired_hash, choices = _import_plan_documents(legacy_file)
    assert item.desired_document_json == desired_json
    assert item.desired_document_hash == desired_hash == _sha(desired_json)
    assert item.catalog_document_json is None and item.catalog_document_hash is None
    assert json.loads(item.artwork_choices_json) == choices
    assert LibraryManagementPublisher._catalog_document(item) == msgspec.json.decode(
        desired_json.encode(), type=DesiredAudioDocument
    )
    assert await blobs.read_bytes(choices[0]["blob_sha256"]) == COVER
    assert (_sha(COVER), "job-complete:0:0") in {
        (row["blob_sha256"], row["reference_id"])
        for row in _rows(db_path, "library_management_blob_references")
    }

    # A second run finds nothing left to do.
    again = compact_artwork_copy(db_path, blob_root, apply=True)
    assert again.bundles_scanned == again.plan_items_scanned == 0
    assert again.bundles_compacted == again.plan_items_compacted == 0
    assert again.blobs_written == 0
    assert _rows(db_path, "library_management_import_bundles") == [bundle_row]


@pytest.mark.asyncio
async def test_non_terminal_rows_are_skipped_and_counted(
    store: NativeLibraryStore, db_path: Path, blob_root: Path
) -> None:
    await _add_bundle(store, "acquisition:preparing", "preparing", ordinal=0)
    await _add_bundle(store, "acquisition:attention", "needs_attention", ordinal=1)
    await _add_plan_item(store, "job-planning", "planning")
    before = _rows(db_path, "library_management_import_bundles")
    before_items = _rows(db_path, "library_management_plan_items")

    result = compact_artwork_copy(db_path, blob_root, apply=True)

    assert (result.bundles_compacted, result.plan_items_compacted) == (0, 0)
    assert result.skipped_non_terminal == 3
    assert _rows(db_path, "library_management_import_bundles") == before
    assert _rows(db_path, "library_management_plan_items") == before_items
    assert not _blob_files(blob_root)


@pytest.mark.asyncio
async def test_rows_that_cannot_be_proven_are_left_whole(
    store: NativeLibraryStore, db_path: Path, blob_root: Path
) -> None:
    tampered = await _add_bundle(store, "acquisition:tampered", "completed", ordinal=0)
    await _add_bundle(store, "acquisition:good", "resolved", ordinal=1)
    await _add_plan_item(store, "job-different-catalog", "complete", catalog_differs=True)
    await _add_plan_item(store, "job-bad-hash", "complete")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_management_import_bundles SET request_hash=? WHERE id=?",
            ("0" * 64, tampered),
        )
        connection.execute(
            "UPDATE library_management_plan_items SET desired_document_hash=? "
            "WHERE job_id='job-bad-hash'",
            ("1" * 64,),
        )
    untouched = [
        row
        for row in _rows(db_path, "library_management_import_bundles")
        if row["id"] == tampered
    ]

    result = compact_artwork_copy(db_path, blob_root, apply=True, batch_size=1)

    # The unproven bundle and the unproven item stay; the other two are rewritten.
    assert result.bundles_compacted == 1
    assert result.plan_items_compacted == 1
    assert result.skipped_unverifiable == 2
    assert [
        row
        for row in _rows(db_path, "library_management_import_bundles")
        if row["id"] == tampered
    ] == untouched
    items = {row["job_id"]: row for row in _rows(db_path, "library_management_plan_items")}
    assert '"content":"' in items["job-bad-hash"]["desired_document_json"]
    # A catalog document of its own keeps its hash; only the desired one changed.
    different = items["job-different-catalog"]
    assert '"content":"' not in different["desired_document_json"]
    assert different["catalog_document_json"] == '{"fields":[],"custom_tags":[]}'
    assert different["catalog_document_hash"] == _sha(different["catalog_document_json"])
    assert different["desired_document_hash"] == _sha(different["desired_document_json"])


@pytest.mark.asyncio
async def test_batches_are_bounded_and_resumable(
    store: NativeLibraryStore, db_path: Path, blob_root: Path
) -> None:
    for index in range(3):
        await _add_bundle(store, f"acquisition:{index}", "completed", ordinal=index)

    first = compact_artwork_copy(
        db_path, blob_root, apply=True, batch_size=1, max_batches=2
    )
    second = compact_artwork_copy(db_path, blob_root, apply=True, batch_size=1)

    assert (first.bundles_compacted, first.complete) == (2, False)
    assert first.integrity_check_passed is None
    assert (second.bundles_compacted, second.complete) == (1, True)
    assert second.integrity_check_passed is True
    assert all(
        '"content":"' not in row["request_json"]
        for row in _rows(db_path, "library_management_import_bundles")
    )


def test_unsafe_arguments_and_paths_are_refused(tmp_path: Path, blob_root: Path) -> None:
    database = tmp_path / "library.db"
    database.write_bytes(b"")

    with pytest.raises(ArtworkCompactionError):
        compact_artwork_copy(database, blob_root, batch_size=0)
    with pytest.raises(ArtworkCompactionError):
        compact_artwork_copy(tmp_path / "missing.db", blob_root)
    with pytest.raises(ArtworkCompactionError):
        compact_artwork_copy(database, tmp_path / "missing")


def test_cli_requires_explicit_isolated_copy_confirmation(
    tmp_path: Path, blob_root: Path
) -> None:
    database = tmp_path / "library.db"
    database.write_bytes(b"")

    with pytest.raises(SystemExit):
        main(["--database", str(database), "--blob-root", str(blob_root), "--apply"])
