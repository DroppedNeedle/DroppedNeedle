"""DropImportStore: DDL idempotency, CRUD roundtrips, scoping, stale sweep."""

import sqlite3
import threading

import pytest

from infrastructure.persistence.drop_import_store import DropImportStore
from models.drop_import import ItemStatus, JobStatus


@pytest.fixture()
def store(tmp_path):
    return DropImportStore(tmp_path / "library.db", threading.Lock())


def test_ensure_tables_is_idempotent(tmp_path):
    lock = threading.Lock()
    DropImportStore(tmp_path / "library.db", lock)
    DropImportStore(tmp_path / "library.db", lock)  # second construct must not raise


@pytest.mark.asyncio
async def test_job_and_item_roundtrip(store):
    await store.create_job("job-1", "user-1", "Harvey", "album.zip", "/staging/job-1")
    item_id = await store.add_item("job-1", "Artist - Album", ["/staging/a.flac"], 1)

    job = await store.get_job("job-1")
    assert job is not None
    assert job.status == JobStatus.PROCESSING
    assert job.user_name == "Harvey"
    assert [i.id for i in job.items] == [item_id]
    assert job.items[0].staging_paths == ["/staging/a.flac"]

    await store.update_item(
        item_id,
        status=ItemStatus.IMPORTED,
        release_group_mbid="rg-1",
        album_title="Album",
        artist_name="Artist",
        files_imported=1,
        detail="Imported 1",
        staging_paths=[],
    )
    await store.set_job_status("job-1", JobStatus.COMPLETED)

    job = await store.get_job("job-1")
    assert job.status == JobStatus.COMPLETED
    item = job.items[0]
    assert item.status == ItemStatus.IMPORTED
    assert item.release_group_mbid == "rg-1"
    assert item.staging_paths == []


@pytest.mark.asyncio
async def test_list_jobs_scopes_to_user_unless_all(store):
    await store.create_job("job-a", "user-1", "A", "a.zip", "/s/a")
    await store.create_job("job-b", "user-2", "B", "b.zip", "/s/b")

    mine = await store.list_jobs(user_id="user-1")
    assert [j.id for j in mine] == ["job-a"]

    everyone = await store.list_jobs(user_id=None)
    assert {j.id for j in everyone} == {"job-a", "job-b"}


@pytest.mark.asyncio
async def test_get_job_missing_returns_none(store):
    assert await store.get_job("nope") is None
    assert await store.get_item(999) is None


@pytest.mark.asyncio
async def test_fail_stale_processing_marks_job_and_items(store):
    await store.create_job("job-1", "user-1", "A", "a.zip", "/s/a")
    item_id = await store.add_item("job-1", "Folder", [], 0)

    changed = await store.fail_stale_processing("Interrupted by a restart")
    assert changed == 1

    job = await store.get_job("job-1")
    assert job.status == JobStatus.FAILED
    assert job.error == "Interrupted by a restart"
    item = await store.get_item(item_id)
    assert item.status == ItemStatus.FAILED


@pytest.mark.asyncio
async def test_item_cascade_on_job_delete(store, tmp_path):
    await store.create_job("job-1", "user-1", "A", "a.zip", "/s/a")
    item_id = await store.add_item("job-1", "Folder", [], 0)

    conn = sqlite3.connect(tmp_path / "library.db")
    conn.execute("PRAGMA foreign_keys=ON")
    conn.execute("DELETE FROM drop_import_jobs WHERE id = 'job-1'")
    conn.commit()
    conn.close()

    assert await store.get_item(item_id) is None
