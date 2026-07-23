"""E2E Usenet pipeline: Soulseek finds nothing acceptable -> Usenet fallback (D3) ->
SABnzbd enqueue -> poll -> FOLDER import (D18, duration+filename match) into
library_files. Plus the per-track-from-album-NZB extract (D4): exactly one track
imported, the rest discarded.

Uses fakes for the indexer + SABnzbd download client; the folder import runs for real
against a fixture folder of re-tagged copies of the test FLAC."""

import shutil
import sqlite3
import threading
from pathlib import Path
from types import SimpleNamespace

import pytest
from mutagen.flac import FLAC

from infrastructure.persistence._database import PersistenceBase  # noqa: F401
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.sse_publisher import SSEPublisher
from models.common import ServiceStatus
from models.download_manifest import ManifestCodec
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadTaskStatus,
    MountDiagnosis,
    TaskHandle,
)
from repositories.protocols.indexer import IndexerResult, UsenetRelease
from services.native.album_preflight_scorer import AlbumPreflightScorer
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.file_processor import FileProcessor
from services.native.library_manager import LibraryManager
from services.native.naming import NamingTemplateEngine
from services.native.newznab_release_scorer import NewznabReleaseScorer
from services.native.track_matcher import TrackMatcher
from infrastructure.audio.tagger import AudioTagger
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


def _place_track(folder: Path, filename: str, *, title: str, track: int) -> None:
    """Copy the fixture FLAC and re-tag it as a distinct track (the unpacked rip)."""
    dest = folder / filename
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURE_FLAC, dest)
    audio = FLAC(dest)
    audio["title"] = title
    audio["tracknumber"] = str(track)
    audio["album"] = "OK Computer"
    audio.save()


class _EmptySlskdIndexer:
    @property
    def indexer_name(self):
        return "soulseek"

    def is_configured(self):
        return True

    async def health_check(self):
        return ServiceStatus(status="ok")

    async def search_album(self, *a, **k):
        return []  # Soulseek finds nothing -> forces the Usenet fallback (D3)

    async def search_track(self, *a, **k):
        return []


class _FakeUsenetIndexer:
    def __init__(self, release: UsenetRelease):
        self._release = release

    @property
    def indexer_name(self):
        return "usenet"

    def is_configured(self):
        return True

    async def health_check(self):
        return ServiceStatus(status="ok")

    async def search_album(self, *a, **k):
        return [IndexerResult(source="usenet", usenet=self._release)]

    async def search_track(self, *a, **k):
        return [IndexerResult(source="usenet", usenet=self._release)]


class _FakeSabnzbd:
    """Completes (or fails) immediately; serves the unpacked folder from a temp dir."""

    def __init__(
        self,
        completed_folder: Path,
        *,
        status: str = "completed",
        fail_message: str = "",
        mount_healthy: bool = True,
        files_visible_after: int = 0,
    ):
        self._folder = completed_folder
        self._status = status
        self._fail_message = fail_message
        self._mount_healthy = mount_healthy
        self._files_visible_after = (
            files_visible_after  # hide files for the first N calls
        )
        self._list_calls = 0
        self.discarded: list[TaskHandle] = []
        self.aborted: list[TaskHandle] = []

    @property
    def client_name(self):
        return "sabnzbd"

    def is_configured(self):
        return True

    async def health_check(self):
        return ServiceStatus(status="ok")

    async def enqueue(self, request):
        target = self._folder.parent / request.job_name
        if self._folder != target and self._folder.exists():
            self._folder.rename(target)
            self._folder = target
        return TaskHandle(source="usenet", job_name=request.job_name, nzo_id="nzo-1")

    async def get_status(self, handle):
        if self._status == "failed":
            return DownloadTaskStatus(
                task_id="",
                status="failed",
                error=self._fail_message,
                matched_transfers=1,
            )
        if self._status == "stuck":
            # Never materialises a transfer (e.g. a globally-paused SABnzbd) -> the poll is
            # interrupted, NOT a terminal SABnzbd outcome.
            return DownloadTaskStatus(task_id="", status="queued", matched_transfers=0)
        return DownloadTaskStatus(
            task_id="",
            status="completed",
            files_total=1,
            files_completed=1,
            progress_percent=100.0,
            matched_transfers=1,
        )

    async def abort(self, handle):
        self.aborted.append(handle)
        return True

    async def inspect_materialization(self, handle):
        if self._status == "failed":
            state = "failed"
        elif self._status == "stuck":
            state = "active"
        else:
            state = "completed"
        return DownloadMaterialization(
            state=state,
            nzo_id=handle.nzo_id,
            remote_storage=f"/data/Downloads/complete/{self._folder.name}",
            mount_root=str(self._folder.parent),
            workspace_path=str(self._folder),
            mount_healthy=self._mount_healthy,
        )

    async def discard_client_artifacts(self, handle):
        self.discarded.append(handle)
        return True

    async def list_completed_files(self, handle):
        self._list_calls += 1
        if self._list_calls <= self._files_visible_after:
            return []  # simulate SABnzbd reporting Completed before the move is visible
        return [p for p in sorted(self._folder.glob("*.flac"))]

    async def downloads_mount_healthy(self):
        return self._mount_healthy

    async def get_file_path(self, handle, remote_filename, size=None):
        return self._folder / remote_filename

    async def diagnose_downloads_mount(self):
        return MountDiagnosis(supported=False)


class _CleanupLibrary:
    async def get_library_management_import_bundle(self, bundle_id: str):
        return SimpleNamespace(state="completed")

    async def list_acquisition_import_bundles_for_download_task(self, task_id: str):
        return []


def _album_service(tracks):
    svc = SimpleNamespace()

    async def get_album_tracks_info(rg):
        return SimpleNamespace(tracks=tracks, total_tracks=len(tracks))

    svc.get_album_tracks_info = get_album_tracks_info
    return svc


def _track(position, title, length_ms):
    return SimpleNamespace(
        position=position,
        title=title,
        disc_number=1,
        length=length_ms,
        recording_id=None,
    )


def _build(
    tmp_path: Path,
    *,
    album_tracks,
    completed_folder,
    sab_status="completed",
    fail_message="",
    release_usenet_date=None,
    mount_healthy=True,
    files_visible_after=0,
):
    library = tmp_path / "library"
    staging = tmp_path / "staging"
    for p in (library, staging):
        p.mkdir(parents=True, exist_ok=True)
    db_path = tmp_path / "library.db"
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    manager = LibraryManager(
        LibraryDB(db_path=tmp_path / "library_files.db", write_lock=threading.Lock())
    )
    fp = FileProcessor(
        AudioTagger(),
        naming_engine=NamingTemplateEngine(),
        library_manager=manager,
        library_paths=[library],
        client=_FakeSabnzbd(completed_folder),
        slskd_downloads_path=tmp_path / "slskd",
        fingerprinter=None,
        verify_downloads=False,
        library_root_ids=["root-a"],
        publish_import_bundle=make_test_import_publisher(manager, {"root-a": library}),
        policy_revision_getter=lambda: "test-policy",
    )
    release = UsenetRelease(
        indexer_id="ds",
        indexer_name="DS",
        guid="g",
        title="Radiohead - OK Computer [FLAC]",
        nzb_url="https://idx/nzb",
        size_bytes=600_000_000,
        category_ids=[3040],
        grabs=200,
        usenet_date=release_usenet_date,
    )
    sab = _FakeSabnzbd(
        completed_folder,
        status=sab_status,
        fail_message=fail_message,
        mount_healthy=mount_healthy,
        files_visible_after=files_visible_after,
    )
    cleanup = AcquisitionCleanupService(
        store,
        _CleanupLibrary(),
        lambda source: sab,
        lambda: completed_folder.parent,
    )
    orch = DownloadOrchestrator(
        client=_EmptySlskdIndexer(),  # placeholder; download-side not used for usenet
        indexer=_EmptySlskdIndexer(),
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
        usenet_indexer=_FakeUsenetIndexer(release),
        usenet_client=sab,
        usenet_scorer=NewznabReleaseScorer(
            store, quality_min="low", flac_mp3_only=False
        ),
        usenet_enabled=True,
        album_service=_album_service(album_tracks),
        source_priority=["soulseek", "usenet"],
        usenet_import_settle_seconds=0.0,
        cleanup_service=cleanup,
    )
    return store, manager, orch, sab, library


@pytest.mark.asyncio
async def test_usenet_fallback_album_imports_via_folder_match(tmp_path: Path):
    completed = tmp_path / "complete" / "droppedneedle-job"
    _place_track(completed, "01 Airbag.flac", title="Airbag", track=1)
    _place_track(
        completed, "02 Paranoid Android.flac", title="Paranoid Android", track=2
    )
    tracks = [_track(1, "Airbag", 300), _track(2, "Paranoid Android", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path, album_tracks=tracks, completed_folder=completed
    )

    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=2,
    )
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert final.source == "usenet"  # routed to Usenet after Soulseek found nothing
    assert final.download_client == "sabnzbd"
    flacs = list(library.rglob("*.flac"))
    assert len(flacs) == 2  # both tracks matched by duration + filename and imported
    assert len(sab.discarded) == 1
    assert not any(completed.parent.glob("droppedneedle-*"))


@pytest.mark.asyncio
async def test_usenet_per_track_imports_exactly_one(tmp_path: Path):
    # D4: a per-track request on Usenet grabs the album NZB but imports ONLY the one
    # requested track; the siblings are discarded.
    completed = tmp_path / "complete" / "droppedneedle-job"
    _place_track(completed, "01 Airbag.flac", title="Airbag", track=1)
    _place_track(
        completed, "02 Paranoid Android.flac", title="Paranoid Android", track=2
    )
    # The manifest's tracklist is the SINGLE requested track (D4) -> only it matches.
    tracks = [_track(2, "Paranoid Android", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path, album_tracks=tracks, completed_folder=completed
    )

    task = await store.create_task(
        user_id="user-a",
        download_type="track",
        release_group_mbid="rg-okc",
        recording_mbid="rec-pa",
        artist_name="Radiohead",
        album_title="OK Computer",
        track_title="Paranoid Android",
        track_number=2,
        disc_number=1,
        track_duration_seconds=0.3,
        year=1997,
        track_count=12,
    )
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    flacs = list(library.rglob("*.flac"))
    assert len(flacs) == 1  # exactly one track imported (the requested one)
    assert flacs[0].name.endswith("Paranoid Android.flac")
    assert not any(completed.parent.glob("droppedneedle-*"))


@pytest.mark.asyncio
async def test_usenet_failed_job_quarantines_release_identity(tmp_path: Path):
    import time

    from models.download_identity import usenet_identity

    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)
    tracks = [_track(1, "Airbag", 300)]
    # An OLD release (well past the propagation window) that SABnzbd reports Failed.
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="failed",
        fail_message="Unpacking failed",
        release_usenet_date=time.time() - 86400,
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)

    quarantine = await store.load_quarantine_set()
    ident = usenet_identity("Radiohead - OK Computer [FLAC]", 600_000_000)
    assert ("usenet", ident) in quarantine  # dead release blocklisted by identity


@pytest.mark.asyncio
async def test_usenet_young_failed_release_not_blocklisted(tmp_path: Path):
    import time

    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)
    tracks = [_track(1, "Airbag", 300)]
    # A release younger than usenet_min_release_age (default 30 min) -> propagation guard
    # leaves it un-blocklisted so the auto-retry can try again once it propagates (Q2).
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="failed",
        fail_message="Unpacking failed",
        release_usenet_date=time.time() - 60,
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)
    assert await store.load_quarantine_set() == set()  # NOT blocklisted (too young)


@pytest.mark.asyncio
async def test_usenet_disk_full_failure_not_blocklisted(tmp_path: Path):
    import time

    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)
    tracks = [_track(1, "Airbag", 300)]
    # A disk-full unpack failure is a transient LOCAL fault - the release must NOT be
    # permanently blocklisted (03-… §Errors), even though it's old enough to pass the guard.
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="failed",
        fail_message="Unpacking failed, write error or disk is full?",
        release_usenet_date=time.time() - 86400,
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)
    assert await store.load_quarantine_set() == set()  # disk-full → retryable, not dead


@pytest.mark.asyncio
async def test_usenet_completed_but_incomplete_album_is_blocklisted(tmp_path: Path):
    import time

    from models.download_identity import usenet_identity

    # The Led Zeppelin case: SABnzbd COMPLETES the job but the NZB only yields 1 of the
    # album's tracks (corrupt/incomplete release). We keep the track we got, but the
    # release is blocklisted so failover/retry finds a COMPLETE one instead of re-grabbing
    # this dead release (Lidarr's failed-import handling). Use a YOUNG release to prove the
    # propagation age guard does NOT spare it: the files ARE present, so the shortfall is
    # confirmed under-delivery, not propagation (review M1).
    completed = tmp_path / "complete" / "droppedneedle-job"
    _place_track(completed, "01 Airbag.flac", title="Airbag", track=1)  # only 1 of 2
    tracks = [_track(1, "Airbag", 300), _track(2, "Paranoid Android", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        release_usenet_date=time.time()
        - 60,  # YOUNG - but enumerated-short still blocklists
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=2,
    )
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "partial"  # incomplete - didn't get the whole album
    assert len(list(library.rglob("*.flac"))) == 1  # the one track we got is kept
    ident = usenet_identity("Radiohead - OK Computer [FLAC]", 600_000_000)
    assert (
        "usenet",
        ident,
    ) in await store.load_quarantine_set()  # bad release blocklisted


@pytest.mark.asyncio
async def test_usenet_empty_completed_young_is_not_blocklisted(tmp_path: Path):
    import time

    # SABnzbd Completed but the folder is EMPTY (no files enumerated) on a YOUNG release.
    # That's ambiguous - propagation / a transient empty - so do NOT permanently blocklist
    # (asymmetry: never kill a possibly-good release). It fails, to be auto-retried.
    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)  # exists but empty
    tracks = [_track(1, "Airbag", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="completed",
        release_usenet_date=time.time() - 60,  # young
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)
    assert await store.load_quarantine_set() == set()  # young empty -> NOT blocklisted


@pytest.mark.asyncio
async def test_usenet_empty_completed_old_is_blocklisted(tmp_path: Path):
    import time

    from models.download_identity import usenet_identity

    # SABnzbd Completed, folder EMPTY, mount healthy, OLD release: a genuine garbage NZB
    # (well past propagation) -> blocklist + fail over (Lidarr's Redownload Failed).
    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)
    tracks = [_track(1, "Airbag", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="completed",
        release_usenet_date=time.time() - 86400,  # old
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)
    ident = usenet_identity("Radiohead - OK Computer [FLAC]", 600_000_000)
    assert (
        "usenet",
        ident,
    ) in await store.load_quarantine_set()  # old garbage -> blocklisted


@pytest.mark.asyncio
async def test_usenet_import_fault_does_not_blocklist(tmp_path: Path):
    import time

    from services.native.file_processor import IMPORT_FAILED, FileFailure, ProcessResult

    # SABnzbd delivered the files (enumerated > 0) but writing them into the library failed
    # locally (perms / cross-mount-copy reject - the TrueNAS-ACL fault). That's OUR fault,
    # not the release's - it must NOT be blocklisted (review H3).
    completed = tmp_path / "complete" / "droppedneedle-job"
    _place_track(completed, "01 Airbag.flac", title="Airbag", track=1)
    tracks = [_track(1, "Airbag", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        release_usenet_date=time.time() - 86400,
    )

    async def _import_fails(manifest, files):
        return ProcessResult(
            succeeded=[],
            failed=[FileFailure(filename="01 Airbag.flac", reason=IMPORT_FAILED)],
        )

    orch._strategies["usenet"]._file_processor.process_downloaded_folder = _import_fails
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)
    assert (
        await store.load_quarantine_set() == set()
    )  # local import fault -> NOT blocklisted


@pytest.mark.asyncio
async def test_usenet_sabnzbd_local_fault_stops_without_blocklist_or_delete(
    tmp_path: Path,
):
    import time

    # A SABnzbd "Failed moving ..." (a local disk/move fault, not in the old 2-word
    # whitelist) must NOT blocklist, NOT fail over, and NOT delete data (review M2 + H1).
    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)
    tracks = [_track(1, "Airbag", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="failed",
        fail_message="Failed moving /incomplete to /complete",
        release_usenet_date=time.time() - 86400,
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)
    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert await store.load_quarantine_set() == set()  # local fault -> not blocklisted
    assert sab.discarded == []
    assert (await store.list_download_attempts(task.id))[-1].state == "preserved"


@pytest.mark.asyncio
async def test_usenet_unreachable_mount_does_not_blocklist_or_delete(tmp_path: Path):
    import time

    # The downloads MOUNT ROOT is unreachable (a genuine environment fault). That must NOT
    # blocklist the release, NOT fail over, and crucially NOT delete the SABnzbd data
    # (cancel/del_files would erase a download we simply couldn't read - review H1).
    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)  # exists but EMPTY -> 0 files enumerated
    tracks = [_track(1, "Airbag", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="completed",
        mount_healthy=False,
        release_usenet_date=time.time() - 86400,
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert "SABnzbd" in (final.error_message or "")  # names the right client, not slskd
    assert await store.load_quarantine_set() == set()  # good release NOT blocklisted
    assert sab.discarded == []
    assert (await store.list_download_attempts(task.id))[-1].state == "preserved"


@pytest.mark.asyncio
async def test_usenet_completed_files_appear_after_settle(tmp_path: Path):
    # SABnzbd reports Completed a beat before the unpacked files are visible on our mount.
    # The importer must re-poll (settle) and import them, NOT misread the empty first
    # enumeration as a garbage NZB / mount fault and blocklist+delete a good download.
    completed = tmp_path / "complete" / "droppedneedle-job"
    _place_track(completed, "01 Airbag.flac", title="Airbag", track=1)
    tracks = [_track(1, "Airbag", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        files_visible_after=1,  # first enumeration empty, then the file appears
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert len(list(library.rglob("*.flac"))) == 1  # imported after the settle retry
    assert await store.load_quarantine_set() == set()  # not blocklisted


@pytest.mark.asyncio
async def test_usenet_interrupted_poll_does_not_blocklist(tmp_path: Path, monkeypatch):
    import time

    from services.native import download_orchestrator as orch_mod

    # A job that never materialises a transfer (e.g. a globally-paused SABnzbd) is an
    # INTERRUPTED poll, not a terminal SABnzbd outcome - it must not blocklist the release.
    monkeypatch.setattr(orch_mod, "_TRANSFER_MATERIALIZE_SECONDS", 0.0)
    completed = tmp_path / "complete" / "droppedneedle-job"
    completed.mkdir(parents=True)
    tracks = [_track(1, "Airbag", 300)]
    store, manager, orch, sab, library = _build(
        tmp_path,
        album_tracks=tracks,
        completed_folder=completed,
        sab_status="stuck",
        release_usenet_date=time.time() - 86400,
    )
    task = await store.create_task(
        user_id="user-a",
        download_type="album",
        release_group_mbid="rg-okc",
        artist_name="Radiohead",
        album_title="OK Computer",
        year=1997,
        track_count=1,
    )
    await orch.process_task(task.id)
    assert (
        await store.load_quarantine_set() == set()
    )  # never completed -> not blocklisted
