"""End-to-end download pipeline (mock slskd): search -> score -> enqueue -> poll ->
process -> import, plus the error branches (no match, quarantine, cancel, retry) and
the boot-time manifest-orphan staging sweep.

Everything except the download client is real: DownloadStore, LibraryDB/LibraryManager,
AudioTagger, NamingTemplateEngine, the real AlbumPreflightScorer/TrackMatcher,
FileProcessor and DownloadOrchestrator. A real-slskd container variant is included but
skips unless ``testcontainers`` and a Docker daemon are available.
"""

import hashlib
import os
import shutil
import sqlite3
import threading
import time
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock

import msgspec
import pytest

from infrastructure.audio.tagger import AudioTagger
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.sse_publisher import SSEPublisher
from models.common import ServiceStatus
from models.download import DownloadSearchResult
from models.download_manifest import ManifestCodec
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadTaskStatus,
    MountDiagnosis,
    TaskHandle,
)
from repositories.protocols.indexer import IndexerResult
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.file_processor import FileProcessor
from services.native.library_manager import LibraryManager
from services.native.naming import NamingTemplateEngine
from services.native.track_matcher import TrackMatcher
from tests.helpers import make_test_import_publisher

FIXTURE_FLAC = (
    Path(__file__).resolve().parent.parent
    / "fixtures"
    / "library"
    / "flac_full_01.flac"
)
_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        conn.execute(
            "INSERT OR IGNORE INTO auth_users (id, username, role) VALUES (?, ?, ?)",
            ("user-a", "alice", "admin"),
        )
        conn.commit()
    finally:
        conn.close()


class _StubIndexer:
    """Search half of the split (D2): canned soulseek results."""

    def __init__(self, album: list) -> None:
        self._album = album

    @property
    def indexer_name(self) -> str:
        return "soulseek"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def search_album(
        self, artist, album, year=None, track_count=None, *, timeout=30.0
    ):
        return [IndexerResult(source="soulseek", soulseek=r) for r in self._album]

    async def search_track(
        self, artist, track, album=None, duration_seconds=None, *, timeout=30.0
    ):
        return []


class _StubClient:
    def __init__(self, downloads_root: Path, status: str = "completed") -> None:
        self._root = downloads_root
        self._status = status
        self.discarded: list[TaskHandle] = []

    @property
    def client_name(self) -> str:
        return "stub"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def enqueue(self, request) -> TaskHandle:
        files = request.files
        return TaskHandle(
            source="soulseek",
            username=files[0].username,
            filenames=[f.filename for f in files],
        )

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        n = len(handle.filenames)
        return DownloadTaskStatus(
            task_id="",
            status=self._status,
            files_total=n,
            files_completed=n,
            bytes_total=0,
            bytes_downloaded=0,
            progress_percent=100.0,
        )

    async def abort(self, handle: TaskHandle) -> bool:
        return True

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        return DownloadMaterialization(
            state="completed",
            mount_root=str(self._root),
            file_paths=[
                str(self._root / value.replace("\\", "/").lstrip("/"))
                for value in handle.filenames
            ],
            mount_healthy=True,
        )

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        self.discarded.append(handle)
        return True

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        return [self._root / f.replace("\\", "/").lstrip("/") for f in handle.filenames]

    async def get_file_path(
        self, handle: TaskHandle, remote_filename: str, size: int | None = None
    ):
        return self._root / remote_filename.replace("\\", "/").lstrip("/")

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return MountDiagnosis(supported=False)


class _CleanupLibrary:
    def __init__(self) -> None:
        self.record = None
        self.journals: list[SimpleNamespace] = []

    def capture_bundle(self, bundle) -> None:  # noqa: ANN001
        request_json = msgspec.json.encode(bundle).decode()
        self.record = SimpleNamespace(
            state="completed",
            request_json=request_json,
            request_hash=hashlib.sha256(request_json.encode()).hexdigest(),
        )
        self.journals = [
            SimpleNamespace(
                ordinal=request.ordinal,
                source_fingerprint=hashlib.sha256(
                    Path(request.input_path).read_bytes()
                ).hexdigest(),
            )
            for request in bundle.files
        ]

    async def get_library_management_import_bundle(self, bundle_id: str):
        return self.record

    async def list_library_management_import_journals(self, bundle_id: str):
        return self.journals

    async def list_acquisition_import_bundles_for_download_task(self, task_id: str):
        return []


def _place_fixture(
    downloads_root: Path, rel: str, *, duration: float | None = None
) -> DownloadSearchResult:
    dest = downloads_root / rel
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURE_FLAC, dest)
    return DownloadSearchResult(
        username="peer1",
        filename=rel,
        parent_directory=rel.rsplit("/", 1)[0],
        size=dest.stat().st_size,
        extension="flac",
        bitrate=1000,
        has_free_slot=True,
        upload_speed=5_000_000,
        duration=duration,
    )


def _build(tmp_path: Path, *, album=None, status="completed"):
    downloads = tmp_path / "slskd_downloads"
    library = tmp_path / "library"
    staging = tmp_path / "staging"
    for p in (downloads, library, staging):
        p.mkdir(parents=True, exist_ok=True)

    db_path = tmp_path / "library.db"
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    library_db = LibraryDB(
        db_path=tmp_path / "library_files.db", write_lock=threading.Lock()
    )
    manager = LibraryManager(library_db)
    client = _StubClient(downloads, status=status)
    indexer = _StubIndexer(album or [])
    cleanup_library = _CleanupLibrary()
    test_publisher = make_test_import_publisher(manager, {"root-a": library})

    async def publish(bundle):  # noqa: ANN001, ANN202
        cleanup_library.capture_bundle(bundle)
        return await test_publisher(bundle)

    fp = FileProcessor(
        AudioTagger(),
        naming_engine=NamingTemplateEngine(),
        library_manager=manager,
        library_paths=[library],
        client=client,
        slskd_downloads_path=downloads,
        fingerprinter=None,
        verify_downloads=False,
        library_root_ids=["root-a"],
        publish_import_bundle=publish,
        policy_revision_getter=lambda: "test-policy",
    )
    cleanup = AcquisitionCleanupService(
        store, cleanup_library, lambda source: client, lambda: downloads
    )
    orch = DownloadOrchestrator(
        client=client,
        indexer=indexer,
        download_store=store,
        file_processor=fp,
        library_manager=manager,
        scorer=AlbumPreflightScorer(store, quality_min="low", flac_mp3_only=False),
        track_matcher=TrackMatcher(store, quality_min="low", flac_mp3_only=False),
        manifest_codec=ManifestCodec(),
        event_bus=SSEPublisher(),
        staging_path=staging,
        naming_template=_TEMPLATE,
        poll_interval=0.0,
        auto_accept_threshold=0.5,
        manual_threshold=0.1,
        cleanup_service=cleanup,
    )
    return store, manager, orch, client, library, staging


async def _make_task(store, **overrides):
    base = dict(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    base.update(overrides)
    return await store.create_task(**base)


@pytest.mark.asyncio
async def test_happy_path_imports_into_library(tmp_path: Path):
    album = [
        _place_fixture(
            tmp_path / "slskd_downloads", "Radiohead - OK Computer/01 Airbag.flac"
        )
    ]
    store, manager, orch, client, library, _staging = _build(tmp_path, album=album)
    task = await _make_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert await manager.has_album("rg-okc") is True
    assert len(list(library.rglob("*.flac"))) == 1
    assert await store.list_quarantine(1, 50) == []  # nothing quarantined on success
    assert len(client.discarded) == 1


@pytest.mark.asyncio
async def test_no_match_fails_cleanly(tmp_path: Path):
    store, _manager, orch, _client, library, _staging = _build(tmp_path, album=[])
    task = await _make_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert final.error_message  # a sanitized, user-facing reason is set
    assert list(library.rglob("*.flac")) == []


@pytest.mark.asyncio
async def test_verification_failure_quarantines_source(tmp_path: Path):
    # A wildly-wrong duration trips the always-on duration check -> duration_mismatch.
    album = [
        _place_fixture(
            tmp_path / "slskd_downloads",
            "Radiohead - OK Computer/01 Airbag.flac",
            duration=9999.0,
        )
    ]
    store, _manager, orch, _client, library, _staging = _build(tmp_path, album=album)
    task = await _make_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert list(library.rglob("*.flac")) == []  # nothing imported
    quarantine = await store.list_quarantine(1, 50)
    assert len(quarantine) == 1
    assert quarantine[0]["reason"] == "duration_mismatch"


@pytest.mark.asyncio
async def test_cancel_marks_task_cancelled(tmp_path: Path):
    store, _manager, orch, _client, _library, _staging = _build(tmp_path)
    task = await _make_task(store)

    await orch.cancel_task(task.id, "user-a", "admin")

    final = await store.get_task(task.id)
    assert final.status == "cancelled"


@pytest.mark.asyncio
async def test_retry_creates_a_fresh_task(tmp_path: Path):
    store, _manager, orch, _client, _library, _staging = _build(tmp_path)
    task = await _make_task(store)
    await store.update_status(task.id, "failed", error_message="boom")
    orch.dispatch = MagicMock()  # don't spawn a real background download

    new_id = await orch.retry_task(task.id, "user-a", "admin")

    assert new_id != task.id
    new_task = await store.get_task(new_id)
    assert new_task.retry_count == task.retry_count + 1
    assert new_task.release_group_mbid == "rg-okc"
    orch.dispatch.assert_called_once_with(new_id)


@pytest.mark.asyncio
async def test_orphan_staging_swept_on_startup(tmp_path: Path):
    """main._cleanup_orphan_staging deletes only stale dirs with no download_tasks row."""
    from main import _cleanup_orphan_staging

    db_path = tmp_path / "library.db"
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    staging = tmp_path / "staging"
    staging.mkdir()

    old = time.time() - 8 * 24 * 3600
    # (a) stale dir, no task row -> swept
    orphan = staging / "orphan-task"
    orphan.mkdir()
    (orphan / "manifest.json").write_text("{}")
    os.utime(orphan, (old, old))
    # (b) stale dir whose task still exists -> kept
    live = await _make_task(store)
    live_dir = staging / live.id
    live_dir.mkdir()
    os.utime(live_dir, (old, old))
    # (c) fresh orphan -> kept (too young)
    fresh = staging / "fresh-task"
    fresh.mkdir()

    await _cleanup_orphan_staging(store, staging)

    assert not orphan.exists()
    assert live_dir.exists()
    assert fresh.exists()


@pytest.mark.e2e
@pytest.mark.asyncio
async def test_real_slskd_container_health(tmp_path: Path):
    """Optional smoke test against a real slskd 0.25.1 container.

    Skips (never fails the suite) when testcontainers, a Docker daemon, or a
    reachable/authenticated slskd is unavailable - the mock-based tests above are
    the actual gate. slskd's API key/auth must be provisioned for this to run; a
    default container rejects an arbitrary key with 401, which is a skip, not a
    failure.
    """
    testcontainers = pytest.importorskip("testcontainers.core.container")
    from repositories.slskd.slskd_client import SlskdClient
    import httpx

    try:
        container = (
            testcontainers.DockerContainer("slskd/slskd:0.25.1")
            .with_exposed_ports(5030)
            .with_env("SLSKD_REMOTE_CONFIGURATION", "true")
        )
        container.start()
    except Exception as exc:  # noqa: BLE001 - no Docker daemon in this environment
        pytest.skip(f"Docker/slskd container unavailable: {exc}")
    try:
        host = container.get_container_host_ip()
        port = container.get_exposed_port(5030)
        async with httpx.AsyncClient() as http:
            client = SlskdClient(http, f"http://{host}:{port}", "test-key")
            try:
                info = await client.health_check()
            except Exception as exc:  # noqa: BLE001 - unprovisioned slskd -> skip
                pytest.skip(f"slskd container not reachable/authenticated: {exc}")
            assert info["version"]["current"].startswith("0.25")
    finally:
        container.stop()
