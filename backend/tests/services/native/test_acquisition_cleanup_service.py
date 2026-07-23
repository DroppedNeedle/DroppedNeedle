import os
import sqlite3
import threading
import hashlib
from pathlib import Path
from types import SimpleNamespace

import pytest

from infrastructure.persistence.download_store import DownloadStore
from infrastructure.service_health import service_health
from repositories.protocols.download_client import (
    DownloadMaterialization,
    TaskHandle,
)
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native import acquisition_cleanup_service as cleanup_module


class _LibraryStore:
    def __init__(self) -> None:
        self.bundles: dict[str, str] = {}
        self.task_bundles: dict[str, list[SimpleNamespace]] = {}

    async def get_library_management_import_bundle(self, bundle_id: str):
        state = self.bundles.get(bundle_id)
        return SimpleNamespace(state=state) if state else None

    async def list_acquisition_import_bundles_for_download_task(self, task_id: str):
        return self.task_bundles.get(task_id, [])


class _Client:
    def __init__(self, materialization: DownloadMaterialization) -> None:
        self.materialization = materialization
        self.aborted = 0
        self.discarded = 0
        self.discard_error = False

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        return self.materialization

    async def abort(self, handle: TaskHandle) -> bool:
        self.aborted += 1
        return True

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        self.discarded += 1
        if self.discard_error:
            raise RuntimeError("history unavailable")
        return True


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


async def _attempt(
    store: DownloadStore,
    root: Path,
    *,
    task_id: str = "a" * 32,
    source: str = "usenet",
    workspace: Path | None = None,
    paths: list[Path] | None = None,
    bundle_ids: list[str] | None = None,
):
    job_name = f"droppedneedle-{task_id}-0" if source == "usenet" else ""
    attempt = await store.create_download_attempt(
        task_id=task_id,
        source=source,
        candidate_index=0,
        job_name=job_name,
        handle=TaskHandle(
            source=source,
            job_name=job_name,
            username="peer" if source == "soulseek" else "",
            filenames=[path.name for path in paths or []],
        ),
        now=1.0,
    )
    return await store.schedule_download_attempt_cleanup(
        attempt.id,
        disposition="discard",
        publisher_bundle_ids=bundle_ids or [],
        now=2.0,
    )


@pytest.fixture(autouse=True)
def _clear_health():
    service_health.clear()
    yield
    service_health.clear()


@pytest.mark.asyncio
async def test_completed_workspace_is_removed_after_barriers_clear(tmp_path: Path):
    root = tmp_path / "sab"
    workspace = root / "audio" / f"droppedneedle-{'a' * 32}-0"
    nested = workspace / "Disc 1"
    nested.mkdir(parents=True)
    for name in ("01.flac", "02-unmatched.flac", "cover.jpg", "album.nfo", "list.m3u"):
        (nested / name).write_bytes(b"source")
    library_copy = tmp_path / "library" / "01.flac"
    library_copy.parent.mkdir()
    library_copy.write_bytes(b"library")

    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace, bundle_ids=["bundle"])
    library = _LibraryStore()
    library.bundles["bundle"] = "completed"
    client = _Client(
        DownloadMaterialization(
            state="completed",
            nzo_id="nzo-1",
            remote_storage=f"/remote/audio/{workspace.name}",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, library, lambda source: client, lambda: root
    )

    assert await service.cleanup_now(attempt.id, worker_id="test") is True

    assert not workspace.exists()
    assert library_copy.read_bytes() == b"library"
    assert (await store.get_download_attempt(attempt.id)).state == "complete"
    assert client.discarded == 1


@pytest.mark.asyncio
async def test_history_failure_resumes_from_workspace_removed(tmp_path: Path):
    now = [10.0]
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    workspace.mkdir(parents=True)
    (workspace / "track.flac").write_bytes(b"x")
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            remote_storage=f"/remote/{workspace.name}",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=True,
        )
    )
    client.discard_error = True
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: root,
        clock=lambda: now[0],
    )

    await service.cleanup_now(attempt.id, worker_id="first")
    failed = await store.get_download_attempt(attempt.id)
    assert not workspace.exists()
    assert failed.state == "workspace_removed"
    assert failed.cleanup_failures == 1

    client.discard_error = False
    now[0] = failed.next_retry_at
    await service.cleanup_now(attempt.id, worker_id="second")
    assert (await store.get_download_attempt(attempt.id)).state == "complete"


@pytest.mark.asyncio
@pytest.mark.parametrize("case", ["root", "outside", "symlink"])
async def test_unsafe_workspace_evidence_is_preserved(tmp_path: Path, case: str):
    root = tmp_path / "sab"
    root.mkdir()
    job_name = f"droppedneedle-{'a' * 32}-0"
    outside = tmp_path / job_name
    outside.mkdir()
    if case == "root":
        workspace = root
    elif case == "outside":
        workspace = outside
    else:
        workspace = root / job_name
        workspace.symlink_to(outside, target_is_directory=True)
    (outside / "keep.flac").write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            remote_storage=f"/remote/{job_name}",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )

    await service.cleanup_now(attempt.id, worker_id="test")

    assert (await store.get_download_attempt(attempt.id)).state == "needs_attention"
    assert (outside / "keep.flac").exists()
    assert service_health.is_degraded("acquisition_cleanup", "source files")


@pytest.mark.asyncio
async def test_symlinked_mount_root_is_refused(tmp_path: Path):
    real_root = tmp_path / "real-sab"
    job_name = f"droppedneedle-{'a' * 32}-0"
    workspace = real_root / job_name
    workspace.mkdir(parents=True)
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    root = tmp_path / "sab"
    root.symlink_to(real_root, target_is_directory=True)
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=root / job_name)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            workspace_path=str(root / job_name),
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )

    await service.cleanup_now(attempt.id, worker_id="test")

    assert source.read_bytes() == b"keep"
    assert (await store.get_download_attempt(attempt.id)).state == "needs_attention"


@pytest.mark.asyncio
async def test_unhealthy_mount_retries_and_missing_workspace_is_idempotent(
    tmp_path: Path,
):
    root = tmp_path / "sab"
    root.mkdir()
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            remote_storage=f"/remote/{workspace.name}",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=False,
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )
    await service.cleanup_now(attempt.id, worker_id="unhealthy")
    pending = await store.get_download_attempt(attempt.id)
    assert pending.state == "cleanup_pending"
    assert pending.cleanup_failures == 1

    client.materialization.mount_healthy = True
    await store.transition_download_attempt(
        attempt.id,
        expected_row_revision=pending.row_revision,
        new_state="cleanup_pending",
        next_retry_at=0.0,
        now=20.0,
    )
    await service.cleanup_now(attempt.id, worker_id="healthy")
    assert (await store.get_download_attempt(attempt.id)).state == "complete"


@pytest.mark.asyncio
async def test_missing_history_cannot_authorize_a_persisted_workspace(tmp_path: Path):
    now = [10.0]
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    workspace.mkdir(parents=True)
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            remote_storage=f"/remote/{workspace.name}",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=False,
        )
    )
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: root,
        clock=lambda: now[0],
    )

    await service.cleanup_now(attempt.id, worker_id="record-evidence")
    pending = await store.get_download_attempt(attempt.id)
    client.materialization = DownloadMaterialization(
        state="missing", mount_root=str(root), mount_healthy=True
    )
    now[0] = pending.next_retry_at
    await service.cleanup_now(attempt.id, worker_id="history-gone")

    assert source.read_bytes() == b"keep"
    assert (await store.get_download_attempt(attempt.id)).state == "needs_attention"


@pytest.mark.asyncio
async def test_unresolved_enqueue_identity_waits_before_declaring_absence(
    tmp_path: Path,
):
    now = [10.0]
    root = tmp_path / "sab"
    root.mkdir()
    store = _store(tmp_path)
    attempt = await _attempt(store, root)
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: root,
        clock=lambda: now[0],
    )

    for index in range(4):
        await service.cleanup_now(attempt.id, worker_id=f"missing-{index}")
        current = await store.get_download_attempt(attempt.id)
        assert current.state == "cleanup_pending"
        now[0] = current.next_retry_at

    await service.cleanup_now(attempt.id, worker_id="stabilized")
    assert (await store.get_download_attempt(attempt.id)).state == "complete"


@pytest.mark.asyncio
async def test_slskd_cleanup_unlinks_only_exact_files(tmp_path: Path):
    root = tmp_path / "slskd"
    album = root / "shared-album"
    album.mkdir(parents=True)
    source = album / "requested.flac"
    sibling = album / "other.flac"
    source.write_bytes(b"source")
    sibling.write_bytes(b"other")
    store = _store(tmp_path)
    attempt = await _attempt(store, root, source="soulseek", paths=[source])
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            file_paths=[str(source)],
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )

    await service.cleanup_now(attempt.id, worker_id="test")

    assert not source.exists()
    assert sibling.read_bytes() == b"other"
    assert album.is_dir()


@pytest.mark.asyncio
async def test_slskd_retry_accepts_remaining_subset_after_partial_unlink(
    tmp_path: Path, monkeypatch
):
    now = [10.0]
    root = tmp_path / "slskd"
    root.mkdir()
    first = root / "first.flac"
    second = root / "second.flac"
    first.write_bytes(b"first")
    second.write_bytes(b"second")
    store = _store(tmp_path)
    attempt = await _attempt(store, root, source="soulseek", paths=[first, second])
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            file_paths=[str(first), str(second)],
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: root,
        clock=lambda: now[0],
    )
    original_unlink = cleanup_module._unlink_file_safely

    def fail_on_second(mount: Path, source: Path) -> None:
        if source == second:
            raise OSError("temporary failure")
        original_unlink(mount, source)

    monkeypatch.setattr(cleanup_module, "_unlink_file_safely", fail_on_second)
    await service.cleanup_now(attempt.id, worker_id="partial")
    pending = await store.get_download_attempt(attempt.id)
    assert not first.exists()
    assert second.exists()
    assert pending.state == "cleanup_pending"

    monkeypatch.setattr(cleanup_module, "_unlink_file_safely", original_unlink)
    client.materialization.file_paths = [str(second)]
    now[0] = pending.next_retry_at
    await service.cleanup_now(attempt.id, worker_id="retry")

    assert not second.exists()
    assert (await store.get_download_attempt(attempt.id)).state == "complete"


@pytest.mark.asyncio
async def test_publisher_cleanup_pending_defers_without_counting_a_failure(
    tmp_path: Path,
):
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    workspace.mkdir(parents=True)
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace, bundle_ids=["bundle"])
    library = _LibraryStore()
    library.bundles["bundle"] = "cleanup_pending"
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, library, lambda source: client, lambda: root
    )

    await service.cleanup_now(attempt.id, worker_id="test")

    deferred = await store.get_download_attempt(attempt.id)
    assert deferred.state == "cleanup_pending"
    assert deferred.cleanup_failures == 0
    assert deferred.error_code == "publisher_cleanup_pending"
    assert workspace.exists()
    assert client.discarded == 0


@pytest.mark.asyncio
async def test_repaired_publisher_attention_returns_to_cleanup(tmp_path: Path):
    now = [10.0]
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    workspace.mkdir(parents=True)
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace, bundle_ids=["bundle"])
    library = _LibraryStore()
    library.bundles["bundle"] = "needs_attention"
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, library, lambda source: client, lambda: root, clock=lambda: now[0]
    )
    await service.cleanup_now(attempt.id, worker_id="blocked")
    attention = await store.get_download_attempt(attempt.id)
    assert attention.state == "needs_attention"

    library.bundles["bundle"] = "completed"
    now[0] = attention.next_retry_at
    await service.run_once("barrier-repaired")
    pending = await store.get_download_attempt(attempt.id)
    assert pending.state == "cleanup_pending"

    await service.run_once("cleanup")
    assert (await store.get_download_attempt(attempt.id)).state == "complete"
    assert not workspace.exists()


@pytest.mark.asyncio
async def test_health_warning_starts_after_three_failures_and_auto_heals(
    tmp_path: Path,
):
    now = [10.0]
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    workspace.mkdir(parents=True)
    (workspace / "track.flac").write_bytes(b"source")
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=False,
        )
    )
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: root,
        clock=lambda: now[0],
    )

    for index in range(3):
        await service.cleanup_now(attempt.id, worker_id=f"failure-{index}")
        current = await store.get_download_attempt(attempt.id)
        now[0] = current.next_retry_at

    assert service_health.is_degraded("acquisition_cleanup", "source files")

    client.materialization.mount_healthy = True
    await service.cleanup_now(attempt.id, worker_id="recovered")

    assert (await store.get_download_attempt(attempt.id)).state == "complete"
    assert not service_health.is_degraded("acquisition_cleanup", "source files")


@pytest.mark.asyncio
async def test_attention_debt_heals_after_unsafe_workspace_is_removed(tmp_path: Path):
    now = [10.0]
    root = tmp_path / "sab"
    outside = tmp_path / "outside"
    outside.mkdir()
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    root.mkdir()
    workspace.symlink_to(outside, target_is_directory=True)
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: root,
        clock=lambda: now[0],
    )
    await service.cleanup_now(attempt.id, worker_id="unsafe")
    attention = await store.get_download_attempt(attempt.id)
    assert attention.state == "needs_attention"

    workspace.unlink()
    now[0] = attention.next_retry_at
    await service.run_once("recheck")

    assert (await store.get_download_attempt(attempt.id)).state == "complete"
    assert not service_health.is_degraded("acquisition_cleanup", "source files")


@pytest.mark.asyncio
async def test_read_only_workspace_retries_without_deleting_source(tmp_path: Path):
    if os.geteuid() == 0:
        pytest.skip("root can unlink from a read-only test directory")
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    workspace.mkdir(parents=True)
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    workspace.chmod(0o500)
    store = _store(tmp_path)
    attempt = await _attempt(store, root, workspace=workspace)
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            workspace_path=str(workspace),
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )
    try:
        await service.cleanup_now(attempt.id, worker_id="test")
    finally:
        workspace.chmod(0o700)
    assert source.exists()
    assert (await store.get_download_attempt(attempt.id)).state == "cleanup_pending"


@pytest.mark.asyncio
async def test_legacy_reconciliation_cleans_only_unambiguous_terminal_tasks(
    tmp_path: Path,
):
    root = tmp_path / "sab"
    category = root / "audio"
    category.mkdir(parents=True)
    store = _store(tmp_path)

    cleanable = []
    for index, status in enumerate(("completed", "partial", "cancelled"), start=1):
        task = await store.create_task(
            user_id="user-a",
            release_group_mbid=f"rg-{index}",
            artist_name="A",
            album_title=status,
        )
        fields = (
            {"cancelled_at": 10.0} if status == "cancelled" else {"completed_at": 10.0}
        )
        await store.update_status(task.id, status, **fields)
        cleanable.append(task)
    failed = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-4",
        artist_name="A",
        album_title="Failed",
    )
    active = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-5",
        artist_name="A",
        album_title="Active",
    )
    publisher_attention = await store.create_task(
        user_id="user-a", release_group_mbid="rg-6", artist_name="A", album_title="Held"
    )
    await store.update_status(failed.id, "failed", completed_at=10.0)
    await store.update_status(active.id, "downloading", started_at=10.0)
    await store.update_status(publisher_attention.id, "completed", completed_at=10.0)

    cleanable_workspaces = [
        category / f"droppedneedle-{task.id}-0" for task in cleanable
    ]
    failed_workspace = category / f"droppedneedle-{failed.id}-0"
    active_workspace = category / f"droppedneedle-{active.id}-0"
    attention_workspace = category / f"droppedneedle-{publisher_attention.id}-0"
    unknown_workspace = category / f"droppedneedle-{'f' * 32}-0"
    for workspace in (
        *cleanable_workspaces,
        failed_workspace,
        active_workspace,
        attention_workspace,
        unknown_workspace,
    ):
        workspace.mkdir()
        (workspace / "keep.flac").write_bytes(b"x")
    symlink_name = f"droppedneedle-{'e' * 32}-0"
    (category / symlink_name).symlink_to(unknown_workspace, target_is_directory=True)

    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    library = _LibraryStore()
    library.task_bundles[publisher_attention.id] = [
        SimpleNamespace(id="attention-bundle", state="needs_attention")
    ]
    library.bundles["attention-bundle"] = "needs_attention"
    service = AcquisitionCleanupService(
        store, library, lambda source: client, lambda: root
    )

    await service.recover_startup()
    await service.reconcile_legacy_mount(limit=2)

    assert all(not workspace.exists() for workspace in cleanable_workspaces)
    assert failed_workspace.exists()
    assert active_workspace.exists()
    assert attention_workspace.exists()
    assert unknown_workspace.exists()
    assert (
        await store.get_download_attempt_for_job(
            "usenet", f"droppedneedle-{failed.id}-0"
        )
    ).state == "needs_attention"
    assert (
        await store.get_download_attempt_for_job(
            "usenet", f"droppedneedle-{active.id}-0"
        )
    ).state == "in_use"
    assert (
        await store.get_download_attempt_for_job(
            "usenet", f"droppedneedle-{publisher_attention.id}-0"
        )
    ).state == "needs_attention"
    assert (
        await store.get_download_attempt_for_job(
            "usenet", f"droppedneedle-{'f' * 32}-0"
        )
    ).state == "needs_attention"
    assert (
        await store.get_download_attempt_for_job("usenet", symlink_name)
    ).state == "needs_attention"
    assert service_health.is_degraded("acquisition_cleanup", "source files")


@pytest.mark.asyncio
async def test_reconciliation_read_error_preserves_durable_cursor(
    tmp_path: Path, monkeypatch
):
    root = tmp_path / "sab"
    (root / "category").mkdir(parents=True)
    store = _store(tmp_path)
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )
    original_entries = cleanup_module._directory_entries

    def fail_read(path: Path):
        raise OSError("temporary read failure")

    monkeypatch.setattr(cleanup_module, "_directory_entries", fail_read)
    with pytest.raises(OSError, match="temporary read failure"):
        await service.reconcile_legacy_mount()

    mount_key = hashlib.sha256(str(root.resolve()).encode()).hexdigest()
    progress = await store.ensure_cleanup_reconciliation(mount_key, str(root))
    assert progress.pending_directories == ["."]
    assert progress.current_directory is None
    assert progress.completed is False

    monkeypatch.setattr(cleanup_module, "_directory_entries", original_entries)
    assert await service.reconcile_legacy_mount() > 0


@pytest.mark.asyncio
@pytest.mark.parametrize("mount", [Path("."), Path("/")])
async def test_reconciliation_refuses_unsafe_mounts(tmp_path: Path, mount: Path):
    store = _store(tmp_path)
    client = _Client(DownloadMaterialization(state="missing"))
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: mount
    )

    assert await service.reconcile_legacy_mount() == 0
