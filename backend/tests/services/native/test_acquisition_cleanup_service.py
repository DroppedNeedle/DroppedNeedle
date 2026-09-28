import os
import sqlite3
import threading
import hashlib
from pathlib import Path
from types import SimpleNamespace

import msgspec
import pytest

from infrastructure.persistence.download_store import DownloadStore
from infrastructure.plugins.adapters import PluginClientAdapter
from infrastructure.service_health import service_health
from models.audio import AudioInfo, AudioTag
from models.library_management import (
    LibraryManagementImportBundle,
    LibraryManagementImportFile,
)
from repositories.protocols.download_client import (
    DownloadMaterialization,
    TaskHandle,
)
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native import acquisition_cleanup_service as cleanup_module


class _LibraryStore:
    def __init__(self) -> None:
        self.bundles: dict[str, str] = {}
        self.records: dict[str, SimpleNamespace] = {}
        self.journals: dict[str, list[SimpleNamespace]] = {}
        self.task_bundles: dict[str, list[SimpleNamespace]] = {}

    async def get_library_management_import_bundle(self, bundle_id: str):
        if bundle_id in self.records:
            return self.records[bundle_id]
        state = self.bundles.get(bundle_id)
        return SimpleNamespace(state=state) if state else None

    async def list_library_management_import_journals(self, bundle_id: str):
        return self.journals.get(bundle_id, [])

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


class _PluginClient(_Client):
    def __init__(self, workspace: Path, *, state: str = "completed") -> None:
        super().__init__(
            DownloadMaterialization(
                state=state,
                workspace_path=str(workspace),
                file_paths=[str(workspace / "track.flac")],
            )
        )
        self.workspace = workspace
        self.calls: list[str] = []
        self.abort_result = True
        self.abort_error = False
        self.discard_result = True
        self.inspection_error = False

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        self.calls.append("inspect")
        if self.inspection_error:
            raise RuntimeError("plugin inspection unavailable")
        return await super().inspect_materialization(handle)

    async def abort(self, handle: TaskHandle) -> bool:
        self.calls.append("abort")
        await super().abort(handle)
        if self.abort_error:
            raise RuntimeError("plugin abort unavailable")
        return self.abort_result

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        self.calls.append("discard")
        await super().discard_client_artifacts(handle)
        if not self.discard_result:
            return False
        (self.workspace / "track.flac").unlink(missing_ok=True)
        if self.workspace.exists():
            self.workspace.rmdir()
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
@pytest.mark.parametrize("state", ["completed", "active"])
async def test_plugin_removes_owned_workspace_without_mount_or_fingerprints(
    tmp_path: Path, state: str
):
    workspace = tmp_path / "plugin-private"
    workspace.mkdir()
    (workspace / "track.flac").write_bytes(b"source")
    library_copy = tmp_path / "library.flac"
    library_copy.write_bytes(b"published")
    store = _store(tmp_path)
    attempt = await _attempt(
        store, tmp_path, source="plugin:example", bundle_ids=["bundle"]
    )
    library = _LibraryStore()
    library.bundles["bundle"] = "completed"
    client = _PluginClient(workspace, state=state)
    service = AcquisitionCleanupService(
        store, library, lambda source: client, lambda: tmp_path
    )

    await service.cleanup_now(attempt.id, worker_id="test")

    cleaned = await store.get_download_attempt(attempt.id)
    assert cleaned.state == "complete"
    assert cleaned.mount_root is None
    assert cleaned.materialized_fingerprints == {}
    assert cleaned.workspace_path == str(workspace)
    assert cleaned.materialized_paths == [str(workspace / "track.flac")]
    assert client.calls == (
        ["inspect", "abort", "discard"] if state == "active" else ["inspect", "discard"]
    )
    assert not workspace.exists()
    assert library_copy.read_bytes() == b"published"
    assert await service.cleanup_now(attempt.id, worker_id="repeat") is False
    assert client.discarded == 1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "state", ["cleanup_pending", "needs_attention", "rolled_back", None]
)
@pytest.mark.parametrize("resolved_state", ["completed", "resolved"])
async def test_plugin_publisher_barriers_preserve_active_work_until_resolved(
    tmp_path: Path, state: str | None, resolved_state: str
):
    workspace = tmp_path / "plugin-private"
    workspace.mkdir()
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(
        store, tmp_path, source="plugin:example", bundle_ids=["bundle"]
    )
    library = _LibraryStore()
    if state is not None:
        library.bundles["bundle"] = state
    client = _PluginClient(workspace, state="active")
    now = [10.0]
    service = AcquisitionCleanupService(
        store, library, lambda source: client, lambda: tmp_path, clock=lambda: now[0]
    )

    await service.cleanup_now(attempt.id, worker_id="blocked")

    blocked = await store.get_download_attempt(attempt.id)
    assert blocked.state == (
        "cleanup_pending" if state == "cleanup_pending" else "needs_attention"
    )
    assert blocked.cleanup_failures == 0
    assert client.calls == []
    assert source.read_bytes() == b"keep"
    library.bundles["bundle"] = resolved_state
    now[0] = blocked.next_retry_at
    if state != "cleanup_pending":
        await service.cleanup_now(attempt.id, worker_id="attention-recheck")
        pending = await store.get_download_attempt(attempt.id)
        assert pending.state == "cleanup_pending"
        assert pending.disposition == "discard"
        assert pending.error_code is None
        assert client.calls == ["inspect"]
        assert client.aborted == 0
        assert client.discarded == 0
        assert source.read_bytes() == b"keep"
        client.calls.clear()

    await service.cleanup_now(attempt.id, worker_id="resolved")
    assert (await store.get_download_attempt(attempt.id)).state == "complete"
    assert client.calls == ["inspect", "abort", "discard"]
    assert not workspace.exists()


@pytest.mark.asyncio
@pytest.mark.parametrize("publisher_attention", [False, True])
async def test_plugin_adapter_inspection_failure_preserves_workspace_until_retry(
    tmp_path: Path, publisher_attention: bool
):
    workspace = tmp_path / "plugin-private"
    workspace.mkdir()
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(
        store, tmp_path, source="plugin:example", bundle_ids=["bundle"]
    )
    library = _LibraryStore()
    client = _PluginClient(workspace)
    adapter = PluginClientAdapter("example", client)
    now = [10.0]
    service = AcquisitionCleanupService(
        store, library, lambda source: adapter, lambda: tmp_path, clock=lambda: now[0]
    )
    if publisher_attention:
        await service.cleanup_now(attempt.id, worker_id="missing-publisher")
        blocked = await store.get_download_attempt(attempt.id)
        assert blocked.state == "needs_attention"
        assert blocked.error_code == "publisher_barrier_missing"
        now[0] = blocked.next_retry_at
    library.bundles["bundle"] = "completed"
    client.inspection_error = True

    await service.cleanup_now(attempt.id, worker_id="inspection-failed")

    failed = await store.get_download_attempt(attempt.id)
    assert failed.state == (
        "needs_attention" if publisher_attention else "cleanup_pending"
    )
    assert failed.error_code == (
        "publisher_barrier_missing"
        if publisher_attention
        else "materialization_inspection_failed"
    )
    assert failed.cleanup_failures == (0 if publisher_attention else 1)
    assert failed.next_retry_at > now[0]
    assert client.calls == ["inspect"]
    assert client.discarded == 0
    assert source.read_bytes() == b"keep"

    client.inspection_error = False
    now[0] = failed.next_retry_at
    if publisher_attention:
        await service.cleanup_now(attempt.id, worker_id="attention-recheck")
        assert (await store.get_download_attempt(attempt.id)).state == "cleanup_pending"
        assert client.discarded == 0
        assert source.read_bytes() == b"keep"
    await service.cleanup_now(attempt.id, worker_id="inspection-recovered")
    assert (await store.get_download_attempt(attempt.id)).state == "complete"
    assert client.discarded == 1
    assert not workspace.exists()


@pytest.mark.asyncio
@pytest.mark.parametrize("raises", [False, True])
async def test_plugin_abort_failure_preserves_workspace(tmp_path: Path, raises: bool):
    workspace = tmp_path / "plugin-private"
    workspace.mkdir()
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(store, tmp_path, source="plugin:example")
    client = _PluginClient(workspace, state="active")
    client.abort_result = False
    client.abort_error = raises
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: tmp_path
    )

    await service.cleanup_now(attempt.id, worker_id="test")

    failed = await store.get_download_attempt(attempt.id)
    assert failed.state == "cleanup_pending"
    assert failed.error_code == "active_abort_failed"
    assert failed.cleanup_failures == 1
    assert client.calls == ["inspect", "abort"]
    assert source.read_bytes() == b"keep"


@pytest.mark.asyncio
@pytest.mark.parametrize("raises", [False, True])
async def test_plugin_discard_failure_retries(tmp_path: Path, raises: bool):
    workspace = tmp_path / "plugin-private"
    workspace.mkdir()
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(store, tmp_path, source="plugin:example")
    client = _PluginClient(workspace)
    client.discard_error = raises
    client.discard_result = False
    now = [10.0]
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: tmp_path,
        clock=lambda: now[0],
    )

    await service.cleanup_now(attempt.id, worker_id="first")

    failed = await store.get_download_attempt(attempt.id)
    assert failed.state == "cleanup_pending"
    assert failed.error_code == "client_artifact_discard_failed"
    assert failed.cleanup_failures == 1
    assert source.read_bytes() == b"keep"
    client.discard_error = False
    client.discard_result = True
    now[0] = failed.next_retry_at
    await service.cleanup_now(attempt.id, worker_id="retry")
    assert (await store.get_download_attempt(attempt.id)).state == "complete"
    assert client.discarded == 2
    assert not workspace.exists()


@pytest.mark.asyncio
@pytest.mark.parametrize("field", ["workspace_path", "materialized_paths_json"])
@pytest.mark.parametrize("mount_healthy", [False, True])
async def test_plugin_conflicting_evidence_preserves_workspace(
    tmp_path: Path, field: str, mount_healthy: bool
):
    workspace = tmp_path / "plugin-private"
    workspace.mkdir()
    source = workspace / "track.flac"
    source.write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(store, tmp_path, source="plugin:example")
    other = str(tmp_path / "other")
    await store.transition_download_attempt(
        attempt.id,
        expected_row_revision=attempt.row_revision,
        new_state=attempt.state,
        now=3.0,
        **{
            field: msgspec.json.encode([other]).decode()
            if field.endswith("_json")
            else other
        },
    )
    client = _PluginClient(workspace, state="active")
    now = [10.0]
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: tmp_path,
        clock=lambda: now[0],
    )

    await service.cleanup_now(attempt.id, worker_id="test")

    blocked = await store.get_download_attempt(attempt.id)
    assert blocked.state == "needs_attention"
    assert blocked.disposition == "preserve"
    assert blocked.error_code == "materialization_evidence_conflict"
    assert client.calls == ["inspect"]
    assert source.read_bytes() == b"keep"

    # Missing plugin client records are not host-verifiable proof that its
    # private materialization vanished, even if it reports a healthy mount.
    client.materialization = DownloadMaterialization(
        state="missing", mount_healthy=mount_healthy
    )
    now[0] = blocked.next_retry_at
    await service.cleanup_now(attempt.id, worker_id="attention-recheck")
    deferred = await store.get_download_attempt(attempt.id)
    assert deferred.state == "needs_attention"
    assert deferred.disposition == "preserve"
    assert deferred.error_code == "materialization_evidence_conflict"
    assert deferred.next_retry_at > blocked.next_retry_at
    assert client.calls == ["inspect", "inspect"]
    assert client.aborted == 0
    assert client.discarded == 0
    assert source.read_bytes() == b"keep"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "source", ["torrent", "plugin:", "plugin:Example", "plugin:../other"]
)
@pytest.mark.parametrize(
    "state", ["cleanup_pending", "workspace_removed", "needs_attention"]
)
async def test_unsupported_cleanup_source_is_preserved(
    tmp_path: Path, source: str, state: str
):
    workspace = tmp_path / "private"
    workspace.mkdir()
    local_file = workspace / "track.flac"
    local_file.write_bytes(b"keep")
    store = _store(tmp_path)
    attempt = await _attempt(store, tmp_path, source="plugin:example")
    attempt = await store.claim_download_cleanup_attempt(
        attempt.id, "test", now=10.0, lease_seconds=300.0
    )
    # Exercise the worker's fail-closed boundary even for names rejected by the
    # current database CHECK constraint (e.g. a future client source).
    attempt = msgspec.structs.replace(
        attempt, source=source, state=state, handle=TaskHandle(source=source)
    )
    client = _PluginClient(workspace, state="active")
    requested_sources: list[str] = []

    def get_client(source: str):
        requested_sources.append(source)
        return client

    service = AcquisitionCleanupService(
        store, _LibraryStore(), get_client, lambda: tmp_path
    )
    await service._process_claimed(attempt)

    blocked = await store.get_download_attempt(attempt.id)
    assert blocked.state == "needs_attention"
    assert blocked.disposition == "preserve"
    assert blocked.error_code == "unsupported_cleanup_source"
    assert requested_sources == []
    assert client.calls == []
    assert local_file.read_bytes() == b"keep"


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

    def fail_on_second(mount: Path, source: Path, expected_sha256: str | None) -> None:
        if source == second:
            raise OSError("temporary failure")
        original_unlink(mount, source, expected_sha256)

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
async def test_slskd_retry_preserves_replacement_at_reused_source_path(
    tmp_path: Path,
):
    now = [10.0]
    root = tmp_path / "slskd"
    root.mkdir()
    source = root / "requested.flac"
    source.write_bytes(b"original transfer")
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
    client.discard_error = True
    service = AcquisitionCleanupService(
        store,
        _LibraryStore(),
        lambda source: client,
        lambda: root,
        clock=lambda: now[0],
    )

    await service.cleanup_now(attempt.id, worker_id="first")
    pending = await store.get_download_attempt(attempt.id)
    assert pending.state == "cleanup_pending"
    assert pending.materialized_fingerprints == {
        str(source.resolve()): hashlib.sha256(b"original transfer").hexdigest()
    }
    assert not source.exists()

    source.write_bytes(b"unrelated replacement")
    client.discard_error = False
    now[0] = pending.next_retry_at
    await service.cleanup_now(attempt.id, worker_id="retry")

    attention = await store.get_download_attempt(attempt.id)
    assert attention.state == "needs_attention"
    assert attention.error_code == "source_file_fingerprint_changed"
    assert source.read_bytes() == b"unrelated replacement"
    assert client.discarded == 1


@pytest.mark.asyncio
async def test_slskd_first_cleanup_uses_publisher_fingerprint_for_reused_path(
    tmp_path: Path,
):
    root = tmp_path / "slskd"
    root.mkdir()
    source = root / "requested.flac"
    source.write_bytes(b"unrelated replacement")
    original_fingerprint = hashlib.sha256(b"published source").hexdigest()
    bundle = LibraryManagementImportBundle(
        idempotency_key="cleanup-source-evidence",
        origin="acquisition",
        policy_revision="policy-1",
        files=(
            LibraryManagementImportFile(
                ordinal=0,
                input_path=str(source),
                destination_root_id="root-1",
                destination_relative_path="Artist/Album/01 Track.flac",
                tag=AudioTag(
                    title="Track",
                    artist="Artist",
                    album="Album",
                    track_number=1,
                ),
                info=AudioInfo(
                    duration_seconds=180.0,
                    bitrate=900,
                    sample_rate=44_100,
                    channels=2,
                    file_format="flac",
                    file_size_bytes=len(b"published source"),
                    bit_depth=16,
                ),
                release_group_mbid=None,
                release_mbid=None,
                recording_mbid=None,
                confidence=1.0,
                source="download",
            ),
        ),
    )
    request_json = msgspec.json.encode(bundle).decode()
    library = _LibraryStore()
    library.records["bundle"] = SimpleNamespace(
        state="completed",
        request_json=request_json,
        request_hash=hashlib.sha256(request_json.encode()).hexdigest(),
    )
    library.journals["bundle"] = [
        SimpleNamespace(ordinal=0, source_fingerprint=original_fingerprint)
    ]
    store = _store(tmp_path)
    attempt = await _attempt(
        store,
        root,
        source="soulseek",
        paths=[source],
        bundle_ids=["bundle"],
    )
    client = _Client(
        DownloadMaterialization(
            state="completed",
            mount_root=str(root),
            file_paths=[str(source)],
            mount_healthy=True,
        )
    )
    service = AcquisitionCleanupService(
        store, library, lambda source: client, lambda: root
    )

    await service.cleanup_now(attempt.id, worker_id="first")

    attention = await store.get_download_attempt(attempt.id)
    assert attention.state == "needs_attention"
    assert attention.error_code == "source_file_fingerprint_changed"
    assert attention.materialized_fingerprints == {
        str(source.resolve()): original_fingerprint
    }
    assert source.read_bytes() == b"unrelated replacement"
    assert client.discarded == 0


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
    (entry,) = [
        item
        for item in service_health.current()
        if item.service == "acquisition_cleanup"
    ]
    assert entry.message == (
        "Temporary files couldn't be removed for 1 download. "
        "Your library is safe. Retrying automatically."
    )

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
        store,
        library,
        lambda source: client,
        lambda: root,
        sab_category_getter=lambda: "audio",
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


@pytest.mark.asyncio
async def test_reconciliation_survives_directory_removed_between_passes(
    tmp_path: Path,
):
    root = tmp_path / "sab"
    vanished = root / "droppedneedle-staging"
    vanished.mkdir(parents=True)
    store = _store(tmp_path)
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )

    assert await service.reconcile_legacy_mount(limit=1) == 1
    vanished.rmdir()

    await service.reconcile_legacy_mount()

    mount_key = hashlib.sha256(str(root.resolve()).encode()).hexdigest()
    progress = await store.ensure_cleanup_reconciliation(mount_key, str(root))
    assert progress.completed is True


def test_directory_entries_skips_entry_removed_mid_scan(tmp_path: Path, monkeypatch):
    class _VanishingEntry:
        def __init__(self, name: str) -> None:
            self.name = name
            self.path = str(tmp_path / name)

        def is_dir(self, follow_symlinks: bool = False) -> bool:
            raise FileNotFoundError(self.name)

        def is_symlink(self) -> bool:
            return False

    class _FakeEntries:
        def __enter__(self):
            return iter([_VanishingEntry("gone")])

        def __exit__(self, *args: object) -> bool:
            return False

    monkeypatch.setattr(cleanup_module.os, "scandir", lambda path: _FakeEntries())
    assert cleanup_module._directory_entries(tmp_path) == []


def test_directory_entries_returns_empty_for_vanished_directory(tmp_path: Path):
    assert cleanup_module._directory_entries(tmp_path / "missing") == []


@pytest.mark.asyncio
async def test_reconciliation_default_category_scopes_to_droppedneedle_prefix(
    tmp_path: Path, monkeypatch
):
    root = tmp_path / "sab"
    (root / "droppedneedle-staging").mkdir(parents=True)
    foreign = root / "sonarr"
    foreign_job = foreign / f"droppedneedle-{'f' * 32}-0"
    foreign_job.mkdir(parents=True)
    store = _store(tmp_path)
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = AcquisitionCleanupService(
        store, _LibraryStore(), lambda source: client, lambda: root
    )
    visited: list[str] = []
    original_entries = cleanup_module._directory_entries

    def recording_entries(path: Path):
        visited.append(str(path))
        return original_entries(path)

    monkeypatch.setattr(cleanup_module, "_directory_entries", recording_entries)

    await service.reconcile_legacy_mount()

    assert str(root) in visited
    assert str(root / "droppedneedle-staging") in visited
    assert not any(str(foreign) in path for path in visited)
    assert (
        await store.get_download_attempt_for_job(
            "usenet", f"droppedneedle-{'f' * 32}-0"
        )
        is None
    )


@pytest.mark.asyncio
async def test_reconciliation_configured_category_descends_only_there(
    tmp_path: Path,
):
    root = tmp_path / "sab"
    category_job = root / "audio" / f"droppedneedle-{'f' * 32}-0"
    category_job.mkdir(parents=True)
    foreign_job = root / "movies" / f"droppedneedle-{'e' * 32}-0"
    foreign_job.mkdir(parents=True)
    store = _store(tmp_path)
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
        sab_category_getter=lambda: "Audio",
    )

    await service.reconcile_legacy_mount()

    assert (
        await store.get_download_attempt_for_job(
            "usenet", f"droppedneedle-{'f' * 32}-0"
        )
    ).state == "needs_attention"
    assert (
        await store.get_download_attempt_for_job(
            "usenet", f"droppedneedle-{'e' * 32}-0"
        )
        is None
    )


def _age_folder(path: Path) -> None:
    stale = 1_000_000_000.0  # 2001 - always older than ORPHAN_MIN_AGE_SECONDS
    os.utime(path, (stale, stale))


def _orphan_service(
    tmp_path: Path,
    root: Path,
    store: DownloadStore,
    client: _Client,
    library: _LibraryStore | None = None,
) -> AcquisitionCleanupService:
    return AcquisitionCleanupService(
        store,
        library or _LibraryStore(),
        lambda source: client,
        lambda: root,
    )


@pytest.mark.asyncio
async def test_orphan_reconcile_removes_unowned_stale_dn_folder(tmp_path: Path):
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'a' * 32}-0"
    workspace.mkdir(parents=True)
    (workspace / "album.flac").write_bytes(b"x")
    _age_folder(workspace)
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = _orphan_service(tmp_path, root, _store(tmp_path), client)

    assert await service.reconcile_orphan_folders() == 1
    assert not workspace.exists()
    assert client.discarded == 1


@pytest.mark.asyncio
async def test_orphan_reconcile_leaves_folder_owned_by_attention_journal(
    tmp_path: Path,
):
    root = tmp_path / "sab"
    task_id = "b" * 32
    job_name = f"droppedneedle-{task_id}-0"
    workspace = root / job_name
    workspace.mkdir(parents=True)
    _age_folder(workspace)
    store = _store(tmp_path)
    attempt = await store.create_download_attempt(
        task_id=task_id,
        source="usenet",
        candidate_index=0,
        job_name=job_name,
        handle=TaskHandle(source="usenet", job_name=job_name),
        now=1.0,
    )
    await store.transition_download_attempt(
        attempt.id,
        expected_row_revision=attempt.row_revision,
        new_state="needs_attention",
        now=2.0,
    )
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = _orphan_service(tmp_path, root, store, client)

    assert await service.reconcile_orphan_folders() == 0
    assert workspace.exists()
    assert client.discarded == 0


@pytest.mark.asyncio
async def test_orphan_reconcile_leaves_active_task_workspace_without_journal_row(
    tmp_path: Path,
):
    root = tmp_path / "sab"
    store = _store(tmp_path)
    active = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-1",
        artist_name="A",
        album_title="Active",
    )
    await store.update_status(active.id, "downloading", started_at=10.0)
    workspace = root / f"droppedneedle-{active.id}-0"
    workspace.mkdir(parents=True)
    _age_folder(workspace)
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = _orphan_service(tmp_path, root, store, client)

    assert await service.reconcile_orphan_folders() == 0
    assert workspace.exists()


@pytest.mark.asyncio
async def test_orphan_reconcile_leaves_young_folder_alone(tmp_path: Path):
    root = tmp_path / "sab"
    workspace = root / f"droppedneedle-{'c' * 32}-0"
    workspace.mkdir(parents=True)  # fresh mtime, below the age floor
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = _orphan_service(tmp_path, root, _store(tmp_path), client)

    assert await service.reconcile_orphan_folders() == 0
    assert workspace.exists()
    assert client.discarded == 0


@pytest.mark.asyncio
async def test_orphan_reconcile_never_touches_foreign_folder_names(tmp_path: Path):
    root = tmp_path / "sab"
    foreign = root / "sonarr.something"
    foreign.mkdir(parents=True)
    (foreign / "episode.mkv").write_bytes(b"x")
    _age_folder(foreign)
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = _orphan_service(tmp_path, root, _store(tmp_path), client)

    assert await service.reconcile_orphan_folders() == 0
    assert foreign.exists()


@pytest.mark.asyncio
async def test_orphan_reconcile_handles_sab_collision_suffixes(tmp_path: Path):
    root = tmp_path / "sab"
    owned_task = "d" * 32
    owned_job = f"droppedneedle-{owned_task}-0"
    orphan_suffixed = root / f"droppedneedle-{'c' * 32}-0.1"
    kept_suffixed = root / f"{owned_job}.2"
    orphan_suffixed.mkdir(parents=True)
    kept_suffixed.mkdir(parents=True)
    _age_folder(orphan_suffixed)
    _age_folder(kept_suffixed)
    store = _store(tmp_path)
    attempt = await store.create_download_attempt(
        task_id=owned_task,
        source="usenet",
        candidate_index=0,
        job_name=owned_job,
        handle=TaskHandle(source="usenet", job_name=owned_job),
        now=1.0,
    )
    await store.transition_download_attempt(
        attempt.id,
        expected_row_revision=attempt.row_revision,
        new_state="needs_attention",
        now=2.0,
    )
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = _orphan_service(tmp_path, root, store, client)

    assert await service.reconcile_orphan_folders() == 1
    assert not orphan_suffixed.exists()
    assert kept_suffixed.exists()


@pytest.mark.asyncio
async def test_orphan_reconcile_honours_publisher_barrier(tmp_path: Path):
    root = tmp_path / "sab"
    held_task = "e" * 32
    workspace = root / f"droppedneedle-{held_task}-0"
    workspace.mkdir(parents=True)
    _age_folder(workspace)
    library = _LibraryStore()
    library.task_bundles[held_task] = [
        SimpleNamespace(id="held-bundle", state="needs_attention")
    ]
    client = _Client(
        DownloadMaterialization(
            state="missing", mount_root=str(root), mount_healthy=True
        )
    )
    service = _orphan_service(
        tmp_path, root, _store(tmp_path), client, library=library
    )

    assert await service.reconcile_orphan_folders() == 0
    assert workspace.exists()
    assert client.discarded == 0
