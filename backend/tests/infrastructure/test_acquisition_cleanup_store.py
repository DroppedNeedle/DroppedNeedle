import sqlite3
import threading
from pathlib import Path

import pytest

from infrastructure.persistence.download_store import DownloadStore
from repositories.protocols.download_client import TaskHandle


def _store(tmp_path: Path) -> DownloadStore:
    path = tmp_path / "library.db"
    store = DownloadStore(path, threading.Lock())
    connection = sqlite3.connect(path)
    try:
        connection.execute(
            "CREATE TABLE IF NOT EXISTS auth_users "
            "(id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        connection.execute(
            "INSERT OR IGNORE INTO auth_users VALUES ('user-a','alice','user')"
        )
        connection.commit()
    finally:
        connection.close()
    return store


@pytest.mark.asyncio
async def test_attempt_schema_is_idempotent_and_survives_task_deletion(tmp_path: Path):
    store = _store(tmp_path)
    DownloadStore(store.db_path, threading.Lock())
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg", artist_name="A", album_title="B"
    )
    attempt = await store.create_download_attempt(
        task_id=task.id,
        source="usenet",
        candidate_index=0,
        job_name=f"droppedneedle-{task.id}-0",
        handle=TaskHandle(source="usenet", job_name=f"droppedneedle-{task.id}-0"),
        now=10.0,
    )

    await store.delete_tasks_by_status("user-a", "user", ["queued"])

    assert await store.get_task(task.id) is None
    assert (await store.get_download_attempt(attempt.id)).task_id == task.id


@pytest.mark.asyncio
async def test_attempt_cas_leases_and_retry_ladder_are_deterministic(tmp_path: Path):
    store = _store(tmp_path)
    attempt = await store.create_download_attempt(
        task_id="a" * 32,
        source="usenet",
        candidate_index=2,
        job_name=f"droppedneedle-{'a' * 32}-2",
        handle=TaskHandle(source="usenet", job_name=f"droppedneedle-{'a' * 32}-2"),
        now=10.0,
    )
    attempt = await store.schedule_download_attempt_cleanup(
        attempt.id, disposition="discard", now=20.0
    )
    claimed = await store.claim_download_cleanup_attempt(
        attempt.id, "worker-a", now=20.0
    )
    assert claimed.lease_expires_at == 320.0
    assert (
        await store.claim_download_cleanup_attempt(attempt.id, "worker-b", now=21.0)
        is None
    )

    expected = [81.0, 326.0, 1231.0, 4841.0, 8442.0]
    current = claimed
    for now, next_retry in zip((21.0, 26.0, 331.0, 1241.0, 4842.0), expected):
        current = await store.record_download_cleanup_failure(
            attempt.id,
            expected_row_revision=current.row_revision,
            error_code="workspace_remove_failed",
            now=now,
        )
        assert current.next_retry_at == next_retry

    stale = await store.transition_download_attempt(
        attempt.id,
        expected_row_revision=claimed.row_revision,
        new_state="complete",
        now=9000.0,
    )
    assert stale is None


@pytest.mark.asyncio
async def test_finalize_and_cleanup_state_precedence_are_atomic(tmp_path: Path):
    store = _store(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg", artist_name="A", album_title="B"
    )
    attempts = []
    for index in range(3):
        attempts.append(
            await store.create_download_attempt(
                task_id=task.id,
                source="usenet",
                candidate_index=index,
                job_name=f"droppedneedle-{task.id}-{index}",
                handle=TaskHandle(
                    source="usenet", job_name=f"droppedneedle-{task.id}-{index}"
                ),
                now=float(index + 1),
            )
        )
    await store.schedule_download_attempt_cleanup(
        attempts[0].id, disposition="discard", now=10.0
    )
    await store.schedule_download_attempt_cleanup(
        attempts[1].id, disposition="preserve", now=10.0
    )
    await store.finalize_task_and_attempt(
        task.id,
        "completed",
        task_fields={"completed_at": 12.0},
        attempt_id=attempts[2].id,
        disposition="discard",
        publisher_bundle_ids=["bundle-1"],
        now=12.0,
    )

    refreshed = await store.get_task(task.id)
    final_attempt = await store.get_download_attempt(attempts[2].id)
    states = await store.cleanup_states_for_tasks([task.id, "missing"])
    assert refreshed.status == "completed"
    assert final_attempt.state == "cleanup_pending"
    assert final_attempt.publisher_bundle_ids == ["bundle-1"]
    assert states == {task.id: "preserved"}

    attention = await store.transition_download_attempt(
        attempts[0].id,
        expected_row_revision=(
            await store.get_download_attempt(attempts[0].id)
        ).row_revision,
        new_state="needs_attention",
        disposition="preserve",
        now=13.0,
    )
    assert attention is not None
    assert (await store.cleanup_states_for_tasks([task.id]))[
        task.id
    ] == "needs_attention"


@pytest.mark.asyncio
async def test_only_completed_attempt_debt_is_prunable(tmp_path: Path):
    store = _store(tmp_path)
    complete = await store.create_download_attempt(
        task_id="a" * 32,
        source="soulseek",
        candidate_index=0,
        job_name="",
        handle=TaskHandle(source="soulseek", username="peer", filenames=["a.flac"]),
        now=1.0,
    )
    pending = await store.create_download_attempt(
        task_id="b" * 32,
        source="soulseek",
        candidate_index=0,
        job_name="",
        handle=TaskHandle(source="soulseek", username="peer", filenames=["b.flac"]),
        now=1.0,
    )
    complete = await store.transition_download_attempt(
        complete.id,
        expected_row_revision=complete.row_revision,
        new_state="complete",
        completed_at=2.0,
        now=2.0,
    )
    await store.schedule_download_attempt_cleanup(
        pending.id, disposition="discard", now=2.0
    )

    assert await store.prune_completed_download_attempts(older_than=3.0) == 1
    assert await store.get_download_attempt(complete.id) is None
    assert await store.get_download_attempt(pending.id) is not None


@pytest.mark.asyncio
async def test_reimport_cannot_acquire_an_active_cleanup_lease(tmp_path: Path):
    store = _store(tmp_path)
    attempt = await store.create_download_attempt(
        task_id="a" * 32,
        source="soulseek",
        candidate_index=0,
        job_name="",
        handle=TaskHandle(source="soulseek", username="peer", filenames=["a.flac"]),
        now=1.0,
    )
    await store.schedule_download_attempt_cleanup(
        attempt.id, disposition="discard", now=2.0
    )
    claimed = await store.claim_download_cleanup_attempt(
        attempt.id, "cleanup", now=2.0
    )
    assert claimed is not None

    assert (
        await store.acquire_download_attempt_for_reimport(attempt.id, now=3.0)
        is None
    )
    acquired = await store.acquire_download_attempt_for_reimport(
        attempt.id, now=303.0
    )
    assert acquired is not None
    assert acquired.state == "in_use"
    assert acquired.disposition == "undecided"


@pytest.mark.asyncio
async def test_cancellation_attaches_all_publisher_barriers_atomically(tmp_path: Path):
    store = _store(tmp_path)
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg", artist_name="A", album_title="B"
    )
    attempt = await store.create_download_attempt(
        task_id=task.id,
        source="usenet",
        candidate_index=0,
        job_name=f"droppedneedle-{task.id}-0",
        handle=TaskHandle(source="usenet", job_name=f"droppedneedle-{task.id}-0"),
        now=1.0,
    )

    ids = await store.cancel_task_and_schedule_attempts(
        task.id, publisher_bundle_ids=["bundle-a", "bundle-b"], cancelled_at=2.0
    )

    refreshed = await store.get_download_attempt(attempt.id)
    assert ids == [attempt.id]
    assert (await store.get_task(task.id)).status == "cancelled"
    assert refreshed.state == "cleanup_pending"
    assert refreshed.publisher_bundle_ids == ["bundle-a", "bundle-b"]
