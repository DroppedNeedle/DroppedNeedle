"""DownloadOrchestrator lifecycle tests.

Real DownloadStore (status transitions, search-job round-trip, quarantine) + real
SSEPublisher/ManifestCodec; the scorer/TrackMatcher/FileProcessor are mocked and a
small fake library tracks "what's been imported" so the failover loop's completeness
check is exercised deterministically. Covers auto-pick, manual park, no-match,
enqueue failure, partial/quarantine, the stall + queued watchdogs, safe partial
harvest, auto-failover to the next candidate, cancel, retry and startup_resume. The
full real import is covered by the E2E gate."""

import asyncio
import sqlite3
import threading
import time as _t
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.exceptions import (
    PermissionDeniedError,
    ResourceNotFoundError,
    ValidationError,
)
from core.task_registry import TaskRegistry
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.sse_publisher import SSEPublisher
from models.common import ServiceStatus
from models.download import DownloadTask, ScoredCandidate
from models.download_identity import soulseek_identity
from models.download_manifest import DownloadManifest, ExpectedFile, ManifestCodec
from repositories.protocols.download_client import (
    DownloadSearchResult,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
)
from services.native.download_orchestrator import (
    _OUT_COMPLETED,
    _OUT_NO_TRANSFER,
    _OUT_QUEUED,
    _OUT_STALLED,
    DownloadOrchestrator,
    _Cancelled,
)
from services.native.file_processor import WRONG_TRACK, FileFailure, ProcessResult

_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"


def _status(state, *, succeeded=(), active=False, bytes_=0, files_total=1, files_completed=0, matched=0):
    return DownloadTaskStatus(
        task_id="", status=state, files_total=files_total, files_completed=files_completed,
        bytes_total=0, bytes_downloaded=bytes_, progress_percent=0.0,
        succeeded_filenames=list(succeeded), has_active_transfer=active,
        matched_transfers=matched,
    )


def _write_manifest(orch, task_id, filenames, username="peer"):
    manifest = DownloadManifest(
        task_id=task_id, source_username=username, release_group_mbid="rg-1",
        artist_name="A", album_title="B", naming_template=_TEMPLATE,
        target_files=[ExpectedFile(filename=f, size=1) for f in filenames],
    )
    d = orch._staging / task_id
    d.mkdir(parents=True, exist_ok=True)
    (d / "manifest.json").write_bytes(orch._manifest_codec.encode(manifest))


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT)"
        )
        conn.executemany(
            "INSERT OR IGNORE INTO auth_users (id, username, role) VALUES (?, ?, ?)",
            [("user-a", "alice", "user"), ("user-b", "bob", "user")],
        )
        conn.commit()
    finally:
        conn.close()


def _candidate(score: float, *, files: int = 1, username: str = "peer") -> ScoredCandidate:
    results = [
        DownloadSearchResult(
            username=username, filename=f"{username}/{i:02d}.flac", parent_directory=username,
            size=100, extension="flac", duration=None,
        )
        for i in range(1, files + 1)
    ]
    return ScoredCandidate(
        username=username, parent_directory=username, files=results,
        coherence=score, file_confidence=score, final_score=score,
        tier="auto" if score >= 0.7 else "manual",
    )


class _FakeLibrary:
    """Tracks imported file rows so the orchestrator's completeness check has real
    state to read. Optionally fed by the FileProcessor mock (see ``_coupled_fp``)."""

    def __init__(self, rows=None):
        self.rows = list(rows or [])
        self.present_tracks: set[str] = set()

    async def get_file_rows_for_album(self, release_group_mbid):
        return list(self.rows)

    async def has_track(self, recording_mbid):
        return recording_mbid in self.present_tracks or bool(self.rows)

    async def album_quality_tier(self, release_group_mbid):
        # worst held tier for the upgrade floor; None = album not held
        from services.native.quality_tiers import tier_for, tier_rank

        tiers = [
            tier_for(r.get("file_format") or "", r.get("bit_rate")) for r in self.rows
        ]
        return min(tiers, key=tier_rank) if tiers else None

    async def recording_quality_tier(self, recording_mbid):
        return None


class _StubIndexer:
    """Search half of the split (D2). The orchestrator unwraps ``.soulseek`` then
    hands the (mocked) scorer the results, so returning ``[]`` is fine - the scorer
    is a MagicMock returning the test's ``scorer_result`` regardless."""

    @property
    def indexer_name(self) -> str:
        return "soulseek"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def search_album(self, *a, **k):
        return []

    async def search_track(self, *a, **k):
        return []


class _StubClient:
    def __init__(self, status=None) -> None:
        self.enqueue = AsyncMock(
            return_value=TaskHandle(source="soulseek", username="peer", filenames=["peer/01.flac"])
        )
        self.cancel = AsyncMock(return_value=True)
        self.get_status = AsyncMock(
            return_value=status or _status("completed", files_completed=1, succeeded=["peer/01.flac"])
        )

    @property
    def client_name(self) -> str:
        return "stub"

    def is_configured(self) -> bool:
        return True

    async def health_check(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def list_completed_files(self, handle):
        return [Path("/fake") / f for f in handle.filenames]

    async def get_file_path(self, handle, remote_filename, size=None):
        return Path("/fake") / remote_filename

    async def diagnose_downloads_mount(self):
        return MountDiagnosis(supported=False)


class _FakeRequestHistory:
    def __init__(self, record=None):
        self.record = record
        self.updates: list[tuple] = []
        self.relinks: list[tuple] = []

    async def async_get_record(self, mbid):
        if self.record is not None and self.record.musicbrainz_id == mbid:
            return self.record
        return None

    async def async_update_status(self, mbid, status, completed_at=None):
        self.updates.append((mbid, status, completed_at))
        if self.record is not None and self.record.musicbrainz_id == mbid:
            self.record.status = status

    async def async_update_download_task_id(self, mbid, task_id):
        self.relinks.append((mbid, task_id))
        if self.record is not None and self.record.musicbrainz_id == mbid:
            self.record.download_task_id = task_id


def _request_record(mbid="rg-1", *, download_task_id=None, status="downloading"):
    from types import SimpleNamespace
    return SimpleNamespace(
        musicbrainz_id=mbid, status=status, download_task_id=download_task_id,
        artist_mbid=None, artist_name="Artist", album_title="Album", year=2020, cover_url="",
    )


def _build(
    tmp_path: Path, *, client=None, indexer=None, scorer_result=None, track_result=None, fp_result=None,
    imported_rows=None, library=None, stall_minutes=30.0, queued_minutes=120.0, max_failover=3,
    max_concurrent=3, request_history=None, on_import=None,
    auto_retry_enabled=True, auto_retry_max_attempts=6, auto_retry_base_interval_minutes=15.0,
    soulseek_enabled=True,
):
    db_path = tmp_path / "library.db"
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)

    scorer = MagicMock()
    scorer.rank = AsyncMock(return_value=scorer_result if scorer_result is not None else [])
    track_matcher = MagicMock()
    track_matcher.match = AsyncMock(return_value=track_result)
    track_matcher.rank = AsyncMock(return_value=[track_result] if track_result else [])
    file_processor = MagicMock()
    file_processor.process_downloaded = AsyncMock(
        return_value=fp_result if fp_result is not None else ProcessResult(succeeded=[], failed=[])
    )

    if library is None:
        rows = imported_rows
        if rows is None:
            rows = [{"file_path": p} for p in (fp_result.succeeded if fp_result is not None else [])]
        library = _FakeLibrary(rows)

    orch = DownloadOrchestrator(
        client=client or _StubClient(),
        indexer=indexer or _StubIndexer(),
        download_store=store,
        file_processor=file_processor,
        library_manager=library,
        scorer=scorer,
        track_matcher=track_matcher,
        manifest_codec=ManifestCodec(),
        event_bus=SSEPublisher(),
        staging_path=tmp_path / "staging",
        naming_template=_TEMPLATE,
        poll_interval=0.0,
        auto_accept_threshold=0.7,
        manual_threshold=0.5,
        stall_timeout_minutes=stall_minutes,
        queued_timeout_minutes=queued_minutes,
        max_failover_attempts=max_failover,
        max_concurrent_downloads=max_concurrent,
        auto_retry_enabled=auto_retry_enabled,
        auto_retry_max_attempts=auto_retry_max_attempts,
        auto_retry_base_interval_minutes=auto_retry_base_interval_minutes,
        request_history=request_history,
        on_import_callback=on_import,
        soulseek_enabled=soulseek_enabled,
    )
    return store, orch, file_processor, library


def _coupled_fp(file_processor, library, *, fail=()):
    """Wire the FileProcessor mock so each import appends rows to the fake library,
    making the completeness check see what landed. ``fail`` filenames are reported as
    verification failures instead of imports."""
    fail = set(fail)

    async def _proc(manifest, only_filenames=None):
        targets = manifest.target_files
        if only_filenames is not None:
            targets = [f for f in targets if f.filename in only_filenames]
        succeeded, failed = [], []
        for f in targets:
            if f.filename in fail:
                failed.append(FileFailure(filename=f.filename, reason="duration_mismatch"))
            else:
                path = f"/lib/{f.filename}"
                succeeded.append(path)
                library.rows.append({"file_path": path})
        return ProcessResult(succeeded=succeeded, failed=failed)

    file_processor.process_downloaded = AsyncMock(side_effect=_proc)


async def _new_task(store, **overrides):
    kwargs = dict(
        user_id="user-a", download_type="album", release_group_mbid="rg-1",
        artist_name="Artist", album_title="Album", year=2020, track_count=1,
    )
    kwargs.update(overrides)
    return await store.create_task(**kwargs)


# ---------------------------------------------------------------------------
# Happy path + park/no-match/config
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_process_task_autopicks_and_completes(tmp_path: Path):
    client = _StubClient()
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9)],
        fp_result=ProcessResult(succeeded=[str(tmp_path / "lib" / "a.flac")], failed=[]),
        imported_rows=[{"file_path": "a"}],
    )
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    fp.process_downloaded.assert_awaited()
    client.enqueue.assert_awaited_once()
    client.cancel.assert_awaited()           # post-import transfer cleanup
    assert not (tmp_path / "staging" / task.id).exists()   # staging cleaned
    job = await store.get_search_job(final.search_job_id)
    assert job.status == "matched"           # (AUD-8) auto-pick matched the job


@pytest.mark.asyncio
async def test_process_task_parks_for_manual_review(tmp_path: Path):
    client = _StubClient()
    store, orch, *_ = _build(tmp_path, client=client, scorer_result=[_candidate(0.6)])
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "queued"           # parked, not downloading
    assert final.search_job_id is not None
    assert final.candidate_index is None      # nothing picked yet
    client.enqueue.assert_not_awaited()


@pytest.mark.asyncio
async def test_process_task_no_match_fails(tmp_path: Path):
    store, orch, *_ = _build(tmp_path, scorer_result=[])
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert "No matching release" in (final.error_message or "")


@pytest.mark.asyncio
async def test_upgrade_task_with_no_candidates_ends_non_failed(tmp_path: Path):
    """No candidate beat the upgrade floor: the library is intact, so the task ends
    quietly (cancelled + 'No better copy found'), never in the failed bucket
    (CollectionManagement Feature B §6)."""
    store, orch, *_ = _build(tmp_path, scorer_result=[])
    task = await _new_task(store, origin="upgrade")

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "cancelled"
    assert "No better copy found" in (final.error_message or "")


@pytest.mark.asyncio
async def test_process_task_unconfigured_client_fails_clearly(tmp_path: Path):
    client = _StubClient()
    client.is_configured = lambda: False
    store, orch, *_ = _build(tmp_path, client=client, scorer_result=[_candidate(0.9)])
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert "not configured" in (final.error_message or "")
    client.enqueue.assert_not_awaited()


@pytest.mark.asyncio
async def test_disabled_slskd_is_not_routed_even_when_configured(tmp_path: Path):
    # The user's bug: slskd disabled (but still has a URL/key, so is_configured() is True)
    # and no Usenet source. An auto-accept candidate must NOT be downloaded via slskd -
    # the disabled toggle has to win over "still configured".
    client = _StubClient()  # is_configured() == True
    store, orch, *_ = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9)], soulseek_enabled=False,
    )
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert "No download source is enabled" in (final.error_message or "")
    client.enqueue.assert_not_awaited()


# ---------------------------------------------------------------------------
# Partial + quarantine + harvest
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_partial_quarantines_failed_file_and_keeps_what_landed(tmp_path: Path):
    """A 2-track album where one file fails verification settles 'partial' (one
    candidate, nothing better to fail over to) and quarantines the bad source."""
    client = _StubClient(_status("completed", files_completed=2, succeeded=["peer/01.flac", "peer/02.flac"]))
    store, orch, fp, lib = _build(tmp_path, client=client, scorer_result=[_candidate(0.9, files=2)])
    _coupled_fp(fp, lib, fail={"peer/02.flac"})
    task = await _new_task(store, track_count=2)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "partial"
    assert ("soulseek", soulseek_identity("peer", "peer/02.flac")) in await store.load_quarantine_set()


@pytest.mark.asyncio
async def test_all_failed_marks_failed_and_quarantines(tmp_path: Path):
    client = _StubClient(_status("completed", files_completed=1, succeeded=["peer/01.flac"]))
    store, orch, _fp, _lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9)],
        fp_result=ProcessResult(succeeded=[], failed=[FileFailure(filename="peer/01.flac", reason="corrupt")]),
        imported_rows=[],
    )
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert ("soulseek", soulseek_identity("peer", "peer/01.flac")) in await store.load_quarantine_set()


@pytest.mark.asyncio
async def test_missing_on_mount_fails_with_mount_message_not_quarantine(tmp_path: Path):
    """slskd reported the transfer done but the importer couldn't find the files on the
    mount (SOURCE_FILE_MISSING). That's a local/config fault: the peer must NOT be
    quarantined, and the failure message must point at the mount rather than wrongly
    claiming Soulseek had no source."""
    from services.native.file_processor import SOURCE_FILE_MISSING

    client = _StubClient(_status("completed", files_completed=1, succeeded=["peer/01.flac"]))
    store, orch, _fp, _lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9)],
        fp_result=ProcessResult(
            succeeded=[], failed=[FileFailure(filename="peer/01.flac", reason=SOURCE_FILE_MISSING)]
        ),
        imported_rows=[],
    )
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert "slskd downloads" in final.error_message      # truthful, mount-pointing message
    assert "No working source" not in final.error_message
    assert await store.load_quarantine_set() == set()  # peer not blacklisted for a local fault


@pytest.mark.asyncio
async def test_import_failure_fails_with_library_message_not_soulseek(tmp_path: Path):
    """slskd delivered the files and we found them, but writing them into the library
    failed (IMPORT_FAILED - perms/disk/a rejected cross-mount copy). A local fault: the
    message must point at the library, not wrongly claim Soulseek had no source."""
    from services.native.file_processor import IMPORT_FAILED

    client = _StubClient(_status("completed", files_completed=1, succeeded=["peer/01.flac"]))
    store, orch, _fp, _lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9)],
        fp_result=ProcessResult(
            succeeded=[], failed=[FileFailure(filename="peer/01.flac", reason=IMPORT_FAILED)]
        ),
        imported_rows=[],
    )
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert "library" in final.error_message              # truthful, library-pointing message
    assert "No working source" not in final.error_message
    assert await store.load_quarantine_set() == set()  # peer not blacklisted for a local fault


def test_no_source_message_names_only_enabled_sources(tmp_path: Path):
    """The 'nothing usable' message must name the sources actually searched - a Usenet
    download must never read "No working source found on Soulseek"."""
    _store, orch, *_ = _build(tmp_path)  # default: soulseek on, usenet off
    assert orch._no_source_message() == "No working source found on Soulseek"

    orch._soulseek_enabled = False
    orch._usenet_enabled = True
    assert orch._no_source_message() == "No working source found on Usenet"

    orch._soulseek_enabled = True
    assert orch._no_source_message() == "No working source found on Soulseek or Usenet"

    orch._soulseek_enabled = False
    orch._usenet_enabled = False
    assert orch._no_source_message() == "No working source found"


def test_no_match_message_names_only_enabled_sources(tmp_path: Path):
    """'No matching release found' must name the sources actually searched - a Usenet-only
    setup reads "...on Usenet" (surfacing that Soulseek is off), never "...on any source"."""
    _store, orch, *_ = _build(tmp_path)  # default: soulseek on, usenet off
    assert orch._no_match_message() == "No matching release found on Soulseek"

    orch._soulseek_enabled = False
    orch._usenet_enabled = True
    assert orch._no_match_message() == "No matching release found on Usenet"

    orch._soulseek_enabled = True
    assert orch._no_match_message() == "No matching release found on Soulseek or Usenet"

    orch._soulseek_enabled = False
    orch._usenet_enabled = False
    assert orch._no_match_message() == "No matching release found on any source"


@pytest.mark.asyncio
async def test_enqueue_failure_fails_without_quarantine(tmp_path: Path):
    client = _StubClient()
    client.enqueue = AsyncMock(side_effect=RuntimeError("boom"))
    store, orch, *_ = _build(tmp_path, client=client, scorer_result=[_candidate(0.9)])
    task = await _new_task(store)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert await store.load_quarantine_set() == set()   # nothing downloaded -> nothing quarantined


# ---------------------------------------------------------------------------
# Stall watchdog + safe harvest (Phase 1)
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_poll_returns_stalled_when_active_transfer_freezes(tmp_path: Path):
    client = _StubClient(_status("downloading", active=True, bytes_=500))
    store, orch, *_ = _build(tmp_path, client=client, stall_minutes=0.0)
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    outcome, _ = await orch._poll_until_done(task)
    assert outcome == _OUT_STALLED


@pytest.mark.asyncio
async def test_poll_fast_fails_when_no_transfer_materialises(tmp_path: Path, monkeypatch):
    # A fresh enqueue whose slskd transfer never appears (peer offline / silently
    # rejected) bails with _OUT_NO_TRANSFER instead of waiting out the queued window.
    import services.native.download_orchestrator as orch_mod
    monkeypatch.setattr(orch_mod, "_TRANSFER_MATERIALIZE_SECONDS", 0.0)
    client = _StubClient(_status("queued", matched=0))  # 0 matched transfers = no-show
    store, orch, *_ = _build(tmp_path, client=client, queued_minutes=999.0)
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    outcome, _ = await orch._poll_until_done(task, expect_materialization=True)
    assert outcome == _OUT_NO_TRANSFER


@pytest.mark.asyncio
async def test_poll_does_not_fast_fail_a_real_queued_transfer(tmp_path: Path, monkeypatch):
    # A transfer that exists but sits queued in the peer's upload queue (matched>0) must
    # NOT trip the no-transfer fast-fail; it follows the normal queued watchdog.
    import services.native.download_orchestrator as orch_mod
    monkeypatch.setattr(orch_mod, "_TRANSFER_MATERIALIZE_SECONDS", 0.0)
    client = _StubClient(_status("queued", active=False, bytes_=0, matched=1))
    store, orch, *_ = _build(tmp_path, client=client, queued_minutes=0.0)
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    outcome, _ = await orch._poll_until_done(task, expect_materialization=True)
    assert outcome == _OUT_QUEUED


@pytest.mark.asyncio
async def test_poll_returns_queued_timeout_when_stuck_in_remote_queue(tmp_path: Path):
    client = _StubClient(_status("queued", active=False, bytes_=0, matched=1))
    store, orch, *_ = _build(tmp_path, client=client, queued_minutes=0.0)
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    outcome, _ = await orch._poll_until_done(task)
    assert outcome == _OUT_QUEUED


@pytest.mark.asyncio
async def test_stall_harvests_succeeded_subset_without_quarantining_missing(tmp_path: Path):
    """A peer that delivers 1 of 2 tracks then stalls: the arrived track imports
    ('partial'), and the never-arrived track is NOT quarantined (it isn't the
    source's fault). This is the data-loss / bad-quarantine fix."""
    client = _StubClient(
        _status("downloading", active=True, bytes_=100, files_completed=1,
                succeeded=["peer/01.flac"])  # 1 of 2 done, then frozen
    )
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9, files=2)],
        stall_minutes=0.0, max_failover=1,   # no failover -> settle on what we got
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store, track_count=2)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "partial"                       # kept the 1 track that arrived
    assert len(lib.rows) == 1
    assert await store.load_quarantine_set() == set()      # missing track NOT quarantined


# ---------------------------------------------------------------------------
# Auto-failover (Phase 2 + 5a)
# ---------------------------------------------------------------------------

class _FailoverClient:
    """Returns a status per current peer: a peer mapped to 'stall' freezes mid-
    transfer, 'complete' delivers its files. The current peer is whatever was last
    enqueued."""

    def __init__(self, behavior):
        self.behavior = behavior
        self.cancel = AsyncMock(return_value=True)
        self._current = None

    @property
    def client_name(self):
        return "failover"

    def is_configured(self):
        return True

    async def health_check(self):
        return ServiceStatus(status="ok")

    async def enqueue(self, request):
        self._current = request.files[0].username
        return TaskHandle(
            source="soulseek",
            username=self._current,
            filenames=[f.filename for f in request.files],
        )

    async def get_status(self, handle):
        mode = self.behavior.get(handle.username, "stall")
        if mode == "complete":
            return _status(
                "completed", succeeded=list(handle.filenames),
                files_total=len(handle.filenames), files_completed=len(handle.filenames),
                bytes_=100,
            )
        return _status("downloading", active=True, bytes_=10)   # frozen

    async def cancel(self, handle):
        return True

    async def list_completed_files(self, handle):
        return [Path("/fake") / f for f in handle.filenames]

    async def get_file_path(self, handle, remote_filename, size=None):
        return Path("/fake") / remote_filename

    async def diagnose_downloads_mount(self):
        return MountDiagnosis(supported=False)


@pytest.mark.asyncio
async def test_failover_skips_dead_peer_and_completes_via_next_candidate(tmp_path: Path):
    """The auto-picked peer stalls; the orchestrator fails over to the next ranked
    candidate (a different peer) and completes the album unattended."""
    client = _FailoverClient({"deadpeer": "stall", "goodpeer": "complete"})
    candidates = [_candidate(0.9, files=2, username="deadpeer"), _candidate(0.85, files=2, username="goodpeer")]
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=candidates, stall_minutes=0.0, max_failover=3,
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store, track_count=2)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert final.source_username == "goodpeer"     # advanced past the dead peer
    assert len(lib.rows) == 2


@pytest.mark.asyncio
async def test_failover_exhausted_settles_failed_when_nothing_landed(tmp_path: Path):
    client = _FailoverClient({"deadpeer": "stall", "deadpeer2": "stall"})
    candidates = [_candidate(0.9, username="deadpeer"), _candidate(0.85, username="deadpeer2")]
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=candidates, stall_minutes=0.0, max_failover=3,
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store)

    await orch.process_task(task.id)

    assert (await store.get_task(task.id)).status == "failed"


@pytest.mark.asyncio
async def test_track_download_completes_when_its_file_imports(tmp_path: Path):
    """A per-track download is complete the moment its one file imports - it must not
    depend on the imported file carrying the recording MBID (Soulseek rips rarely do),
    or every track would settle 'partial'."""
    client = _StubClient(_status("completed", files_completed=1, succeeded=["peer/01.flac"]))
    store, orch, fp, lib = _build(
        tmp_path, client=client, track_result=_candidate(0.9),
    )
    _coupled_fp(fp, lib)
    task = await _new_task(
        store, download_type="track", recording_mbid="rec-1", track_title="Song", track_count=1,
    )

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert final.files_completed == 1
    assert final.files_total == 1


def _duration_gate_fp(file_processor, library):
    """FileProcessor mock that simulates the canonical-duration gate: when the gate is
    on (manifest.is_track) and the source peer name contains 'wrong', reject the file
    as WRONG_TRACK; otherwise import it."""

    async def _proc(manifest, only_filenames=None):
        fname = manifest.target_files[0].filename
        if manifest.is_track and "wrong" in manifest.source_username:
            return ProcessResult(
                succeeded=[], failed=[FileFailure(filename=fname, reason=WRONG_TRACK)]
            )
        path = f"/lib/{fname}"
        library.rows.append({"file_path": path, "track_number": 1, "disc_number": 1})
        return ProcessResult(succeeded=[path], failed=[])

    file_processor.process_downloaded = AsyncMock(side_effect=_proc)


@pytest.mark.asyncio
async def test_track_wrong_duration_fails_over_to_right_source(tmp_path: Path):
    """A per-track download whose first source is the WRONG recording (duration gate)
    fails over to a source that has the right one - and the wrong file is NOT
    quarantined (it's a good file, just a different song)."""
    client = _FailoverClient({"wrongpeer": "complete", "rightpeer": "complete"})
    store, orch, fp, lib = _build(tmp_path, client=client, max_failover=3)
    orch._strategies["soulseek"]._track_matcher.rank = AsyncMock(return_value=[
        _candidate(0.9, username="wrongpeer"), _candidate(0.85, username="rightpeer"),
    ])
    _duration_gate_fp(fp, lib)
    task = await _new_task(
        store, download_type="track", recording_mbid="rec-1",
        track_title="Song", track_count=1, track_duration_seconds=200.0,
    )

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert final.source_username == "rightpeer"
    assert await store.load_quarantine_set() == set()   # wrong track is not blacklisted


@pytest.mark.asyncio
async def test_track_duration_fallback_accepts_best_when_all_sources_rejected(tmp_path: Path):
    """If EVERY source fails the duration gate (the MusicBrainz length is probably
    wrong), the fallback re-pulls the best source with the gate off so the user isn't
    left empty-handed."""
    client = _FailoverClient({"wrongpeer1": "complete", "wrongpeer2": "complete"})
    store, orch, fp, lib = _build(tmp_path, client=client, max_failover=3)
    orch._strategies["soulseek"]._track_matcher.rank = AsyncMock(return_value=[
        _candidate(0.9, username="wrongpeer1"), _candidate(0.85, username="wrongpeer2"),
    ])
    _duration_gate_fp(fp, lib)
    task = await _new_task(
        store, download_type="track", recording_mbid="rec-1",
        track_title="Song", track_count=1, track_duration_seconds=200.0,
    )

    await orch.process_task(task.id)

    assert (await store.get_task(task.id)).status == "completed"   # fallback delivered it


@pytest.mark.asyncio
async def test_album_unknown_track_count_partial_does_not_complete_prematurely(tmp_path: Path):
    """An album with no MusicBrainz track count must NOT be declared complete on a
    partial import - it fails over / settles 'partial', never 'completed' on 1-of-N."""
    client = _StubClient(
        _status("completed", files_completed=2, succeeded=["peer/01.flac", "peer/02.flac"])
    )
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9, files=2)], max_failover=1,
    )
    _coupled_fp(fp, lib, fail={"peer/02.flac"})
    task = await _new_task(store, track_count=None)

    await orch.process_task(task.id)

    assert (await store.get_task(task.id)).status == "partial"


@pytest.mark.asyncio
async def test_album_unknown_track_count_clean_import_completes(tmp_path: Path):
    """With no track count, a source that delivers everything it had (no failures) is
    the best 'complete' signal we have."""
    client = _StubClient(
        _status("completed", files_completed=2, succeeded=["peer/01.flac", "peer/02.flac"])
    )
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9, files=2)],
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store, track_count=None)

    await orch.process_task(task.id)

    assert (await store.get_task(task.id)).status == "completed"


@pytest.mark.asyncio
async def test_incomplete_album_repulls_whole_album_from_next_source(tmp_path: Path):
    """Candidate A 'completes' but only carries 1 of 2 tracks; the loop fails over
    to candidate B (2 tracks) and the album is then complete - Phase 5a reliable
    completion via whole-album re-pull, idempotent on the track A already had."""
    client = _FailoverClient({"thin": "complete", "full": "complete"})
    candidates = [_candidate(0.9, files=1, username="thin"), _candidate(0.85, files=2, username="full")]
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=candidates, max_failover=3,
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store, track_count=2)

    await orch.process_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert final.source_username == "full"


# ---------------------------------------------------------------------------
# Cancel / retry / resume / dispatch
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_cancel_task_ownership(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store)

    with pytest.raises(ResourceNotFoundError):
        await orch.cancel_task("does-not-exist", "user-a", "user")
    with pytest.raises(PermissionDeniedError):
        await orch.cancel_task(task.id, "user-b", "user")

    await orch.cancel_task(task.id, "user-a", "user")
    assert (await store.get_task(task.id)).status == "cancelled"


@pytest.mark.asyncio
async def test_cancel_task_admin_can_cancel_others(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store)
    await orch.cancel_task(task.id, "admin-x", "admin")
    assert (await store.get_task(task.id)).status == "cancelled"


@pytest.mark.asyncio
async def test_retry_task_creates_new_clean_task(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed")

    new_id = await orch.retry_task(task.id, "user-a", "user")

    assert new_id != task.id
    new_task = await store.get_task(new_id)
    assert new_task.retry_count == 1
    assert new_task.status == "queued"
    orch.dispatch.assert_called_once_with(new_id)


@pytest.mark.asyncio
async def test_retry_task_sets_retry_origin(tmp_path: Path):
    """A retried user task becomes origin='retry' so quota counts ignore it
    (CollectionManagement D20)."""
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed")
    assert task.origin == "user"

    new_id = await orch.retry_task(task.id, "user-a", "user")

    assert (await store.get_task(new_id)).origin == "retry"


@pytest.mark.asyncio
async def test_retry_task_propagates_upgrade_origin(tmp_path: Path):
    """An upgrade's retry must stay an upgrade - the origin-aware gate and
    replace-on-import key off origin='upgrade' (CollectionManagement D18)."""
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", origin="upgrade")

    new_id = await orch.retry_task(task.id, "user-a", "user")

    assert (await store.get_task(new_id)).origin == "upgrade"


@pytest.mark.asyncio
async def test_retry_task_rejects_non_retryable_state(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="queued")
    with pytest.raises(ValidationError):
        await orch.retry_task(task.id, "user-a", "user")


@pytest.mark.asyncio
async def test_retry_task_ownership(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="failed")
    with pytest.raises(ResourceNotFoundError):
        await orch.retry_task("missing", "user-a", "user")
    with pytest.raises(PermissionDeniedError):
        await orch.retry_task(task.id, "user-b", "user")


@pytest.mark.asyncio
async def test_cancel_task_syncs_linked_request_to_cancelled(tmp_path: Path):
    """Cancelling (or stopping the retry of) a download flips its linked request to
    'cancelled', so the album UI's "retry scheduled" line clears."""
    rh = _FakeRequestHistory(_request_record(mbid="rg-1", status="downloading"))
    store, orch, *_ = _build(tmp_path, request_history=rh)
    task = await _new_task(store, status="failed")
    rh.record.download_task_id = task.id

    await orch.cancel_task(task.id, "user-a", "user")

    assert (await store.get_task(task.id)).status == "cancelled"
    assert any(s == "cancelled" for (_m, s, _c) in rh.updates)
    assert rh.record.status == "cancelled"


@pytest.mark.asyncio
async def test_retry_task_clears_album_blocklist(tmp_path: Path):
    """A manual retry is an explicit "try again": it clears the album's blocklist so a
    release quarantined by the failed attempt is reconsidered."""
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    await store.record_quarantine(
        source="soulseek", identity=soulseek_identity("peer", "bad.flac"),
        reason="verify_failed", release_group_mbid="rg-1",
    )
    task = await _new_task(store, status="failed")

    await orch.retry_task(task.id, "user-a", "user")

    assert await store.load_quarantine_set() == set()


@pytest.mark.asyncio
async def test_create_retry_task_does_not_clear_blocklist(tmp_path: Path):
    """The shared auto-retry path (retry_failed_tasks -> _create_retry_task) must NOT
    clear the blocklist - only an explicit manual retry/re-request does."""
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    ident = soulseek_identity("peer", "bad.flac")
    await store.record_quarantine(
        source="soulseek", identity=ident, reason="verify_failed", release_group_mbid="rg-1",
    )
    task = await _new_task(store, status="failed")

    await orch._create_retry_task(task)

    assert ("soulseek", ident) in await store.load_quarantine_set()


@pytest.mark.asyncio
async def test_startup_resume_redispatches_queued(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    queued = await _new_task(store, status="queued")

    await orch.startup_resume()

    orch.dispatch.assert_called_once_with(queued.id)


@pytest.mark.asyncio
async def test_dispatch_safe_runner_sanitizes_unhandled_error(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store)

    async def _boom(_tid):
        raise RuntimeError("raw boom detail")

    orch.process_task = _boom

    handle = orch.dispatch(task.id)
    assert TaskRegistry.get_instance().is_running(f"download-{task.id}")
    await handle

    final = await store.get_task(task.id)
    assert final.status == "failed"
    assert final.error_message == "download failed"   # sanitized (AUD-11)
    assert task.id not in orch._active_tasks


@pytest.mark.asyncio
async def test_resume_single_task_completed(tmp_path: Path):
    client = _StubClient(_status("completed", files_completed=1, succeeded=["peer/01.flac"]))
    store, orch, fp, lib = _build(tmp_path, client=client)
    _coupled_fp(fp, lib)
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    await orch._resume_single_task(task.id)

    assert (await store.get_task(task.id)).status == "completed"


@pytest.mark.asyncio
async def test_resume_queued_transfer_resumes_then_completes(tmp_path: Path):
    """The restart-resume fix: a transfer slskd still reports as 'queued' is polled
    through to completion instead of being force-failed 'Transfer lost during
    restart' (the old bug)."""
    client = _StubClient()
    client.get_status = AsyncMock(side_effect=[
        _status("queued", active=False, bytes_=0),
        _status("completed", files_completed=1, succeeded=["peer/01.flac"]),
    ])
    store, orch, fp, lib = _build(tmp_path, client=client)
    _coupled_fp(fp, lib)
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    await orch._resume_single_task(task.id)

    final = await store.get_task(task.id)
    assert final.status == "completed"
    assert final.error_message != "Transfer lost during restart"


@pytest.mark.asyncio
async def test_active_transfer_respects_concurrency_cap(tmp_path: Path):
    """With the cap full, an actively-transferring download blocks until a slot
    frees; a purely queued download never takes a slot in the first place."""
    client = _StubClient()
    client.get_status = AsyncMock(side_effect=[
        _status("downloading", active=True, bytes_=10),
        _status("completed", files_completed=1, succeeded=["peer/01.flac"]),
    ])
    store, orch, *_ = _build(
        tmp_path, client=client, max_concurrent=1, stall_minutes=999, queued_minutes=999,
    )
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    await orch._download_slots.acquire()        # occupy the only slot
    poll_task = asyncio.create_task(orch._poll_until_done(task))
    await asyncio.sleep(0.05)
    assert not poll_task.done()                 # active transfer is blocked on the slot

    orch._download_slots.release()              # free it
    outcome, _ = await asyncio.wait_for(poll_task, timeout=1.0)
    assert outcome == _OUT_COMPLETED


@pytest.mark.asyncio
async def test_queued_transfer_does_not_take_a_slot(tmp_path: Path):
    """A transfer waiting in the peer's remote queue holds no slot, so it can't
    block other downloads (the M2 starvation fix)."""
    client = _StubClient()
    client.get_status = AsyncMock(side_effect=[
        _status("queued", active=False, bytes_=0),
        _status("completed", files_completed=1, succeeded=["peer/01.flac"]),
    ])
    store, orch, *_ = _build(
        tmp_path, client=client, max_concurrent=1, stall_minutes=999, queued_minutes=999,
    )
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])

    await orch._download_slots.acquire()        # cap is full...
    # ...but a queued transfer doesn't need a slot, so it still progresses
    outcome, _ = await asyncio.wait_for(orch._poll_until_done(task), timeout=1.0)
    assert outcome == _OUT_COMPLETED


@pytest.mark.asyncio
async def test_poll_until_done_bails_on_out_of_band_cancel(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="downloading", source_username="peer")
    _write_manifest(orch, task.id, ["peer/01.flac"])
    await store.update_status(task.id, "cancelled")
    task = await store.get_task(task.id)

    with pytest.raises(_Cancelled):
        await orch._poll_until_done(task)


@pytest.mark.asyncio
async def test_startup_resume_tracks_handle_so_cancel_can_reach_it(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="downloading", source_username="peer")
    started = asyncio.Event()

    async def _hang(_tid):
        started.set()
        await asyncio.sleep(3600)

    orch._resume_single_task = _hang
    await orch.startup_resume()
    await asyncio.wait_for(started.wait(), timeout=1.0)

    assert task.id in orch._active_tasks
    orch._active_tasks[task.id].cancel()


# ---------------------------------------------------------------------------
# Request/library state bridge (Phase 3)
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_terminal_completed_marks_linked_request_imported(tmp_path: Path):
    from unittest.mock import AsyncMock as _AM
    record = _request_record()
    rh = _FakeRequestHistory(record)
    on_import = _AM()
    client = _StubClient(_status("completed", files_completed=1, succeeded=["peer/01.flac"]))
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9)],
        request_history=rh, on_import=on_import,
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store)
    record.download_task_id = task.id   # request -> task link

    await orch.process_task(task.id)

    assert ("rg-1", "imported") in [(m, s) for (m, s, _c) in rh.updates]
    on_import.assert_awaited()           # caches busted + album materialised


@pytest.mark.asyncio
async def test_terminal_failed_marks_linked_request_failed(tmp_path: Path):
    record = _request_record()
    rh = _FakeRequestHistory(record)
    client = _FailoverClient({"deadpeer": "stall"})
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9, username="deadpeer")],
        stall_minutes=0.0, max_failover=1, request_history=rh,
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store)
    record.download_task_id = task.id

    await orch.process_task(task.id)

    assert any(s == "failed" for (_m, s, _c) in rh.updates)


@pytest.mark.asyncio
async def test_track_download_does_not_flip_album_request(tmp_path: Path):
    """A finished per-track download must NOT flip the album's request (which a
    different task owns), or 1 track would masquerade as a full album."""
    record = _request_record(download_task_id="some-other-album-task")
    rh = _FakeRequestHistory(record)
    client = _StubClient(_status("completed", files_completed=1, succeeded=["peer/01.flac"]))
    store, orch, fp, lib = _build(
        tmp_path, client=client, scorer_result=[_candidate(0.9)],
        request_history=rh, on_import=None,
    )
    _coupled_fp(fp, lib)
    task = await _new_task(store)   # this task's id != record.download_task_id

    await orch.process_task(task.id)

    assert rh.updates == []   # the album request was left untouched


@pytest.mark.asyncio
async def test_reap_stale_tasks_fails_orphaned_download(tmp_path: Path):
    import time as _t
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="downloading", source_username="peer")
    # No live loop owns it and last_polled_at is ancient -> the poller is dead.
    await store.update_status(task.id, "downloading", last_polled_at=_t.time() - 999_999)

    await orch.reap_stale_tasks()

    assert (await store.get_task(task.id)).status == "failed"


@pytest.mark.asyncio
async def test_reap_stale_tasks_skips_live_and_fresh(tmp_path: Path):
    import time as _t
    store, orch, *_ = _build(tmp_path)
    fresh = await _new_task(store, status="downloading", source_username="peer")
    await store.update_status(fresh.id, "downloading", last_polled_at=_t.time())

    await orch.reap_stale_tasks()

    assert (await store.get_task(fresh.id)).status == "downloading"   # recently polled -> left alone


# ---------------------------------------------------------------------------
# Auto-retry (retry_failed_tasks)
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_retry_failed_tasks_redispatches_eligible_failed(tmp_path: Path):
    """A failed task whose backoff has elapsed is re-dispatched with retry_count + 1."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=0.01)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=0)
    await store.update_status(task.id, "failed", completed_at=_t.time() - 10)

    await orch.retry_failed_tasks()

    orch.dispatch.assert_called_once()
    new_id = orch.dispatch.call_args.args[0]
    new_task = await store.get_task(new_id)
    assert new_task is not None
    assert new_task.retry_count == 1
    assert new_task.status == "queued"
    assert new_task.release_group_mbid == task.release_group_mbid


@pytest.mark.asyncio
async def test_retry_failed_tasks_disabled_is_noop(tmp_path: Path):
    store, orch, *_ = _build(tmp_path, auto_retry_enabled=False)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=0)
    await store.update_status(task.id, "failed", completed_at=_t.time() - 999_999)

    await orch.retry_failed_tasks()

    orch.dispatch.assert_not_called()


@pytest.mark.asyncio
async def test_retry_failed_tasks_respects_max_attempts(tmp_path: Path):
    """A task at the retry ceiling is not retried."""
    store, orch, *_ = _build(tmp_path, auto_retry_max_attempts=3, auto_retry_base_interval_minutes=0.01)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=3)
    await store.update_status(task.id, "failed", completed_at=_t.time() - 999_999)

    await orch.retry_failed_tasks()

    orch.dispatch.assert_not_called()


def test_next_retry_at_matches_backoff(tmp_path: Path):
    """next_retry_at = completed_at + base*2^retry_count - the same formula the sweep uses."""
    _, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=15.0)
    now = _t.time()
    task = DownloadTask(id="t", user_id="u", status="failed", retry_count=1, completed_at=now)
    # base 15m * 2^1 = 30m
    assert orch.next_retry_at(task) == pytest.approx(now + 30 * 60)
    assert orch.auto_retry_max == 6


def test_next_retry_at_none_when_not_eligible(tmp_path: Path):
    _, orch, *_ = _build(tmp_path, auto_retry_max_attempts=3, auto_retry_base_interval_minutes=15.0)
    now = _t.time()
    # exhausted
    assert orch.next_retry_at(DownloadTask(id="a", user_id="u", status="failed", retry_count=3, completed_at=now)) is None
    # not a retryable state
    assert orch.next_retry_at(DownloadTask(id="b", user_id="u", status="downloading", retry_count=0, completed_at=now)) is None
    # completed (terminal success) never retries
    assert orch.next_retry_at(DownloadTask(id="c", user_id="u", status="completed", retry_count=0, completed_at=now)) is None


def test_next_retry_at_none_and_max_zero_when_auto_retry_disabled(tmp_path: Path):
    _, orch, *_ = _build(tmp_path, auto_retry_enabled=False)
    task = DownloadTask(id="t", user_id="u", status="failed", retry_count=0, completed_at=_t.time())
    assert orch.next_retry_at(task) is None
    assert orch.auto_retry_max == 0  # advertises "no auto-retry" to the UI


def test_retry_ladder_minutes_matches_backoff(tmp_path: Path):
    """base 15m, max 6 -> the doubling ladder, capped at 24h, in minutes."""
    _, orch, *_ = _build(tmp_path, auto_retry_max_attempts=6, auto_retry_base_interval_minutes=15.0)
    assert orch.retry_ladder_minutes() == [15, 30, 60, 120, 240, 480]


def test_retry_ladder_minutes_empty_when_auto_retry_disabled(tmp_path: Path):
    _, orch, *_ = _build(tmp_path, auto_retry_enabled=False)
    assert orch.retry_ladder_minutes() == []


@pytest.mark.asyncio
async def test_retry_failed_tasks_respects_backoff(tmp_path: Path):
    """A task that failed too recently (backoff not elapsed) is not retried."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=60.0)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=0)
    await store.update_status(task.id, "failed", completed_at=_t.time() - 30)

    await orch.retry_failed_tasks()

    orch.dispatch.assert_not_called()


@pytest.mark.asyncio
async def test_retry_failed_tasks_skips_when_newer_active_exists(tmp_path: Path):
    """If a newer active task for the same album + user exists, the old failed task
    is not auto-retried (avoids duplicates from a manual retry or new request)."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=0.01)
    orch.dispatch = MagicMock()
    old = await _new_task(store, status="failed", retry_count=0)
    await store.update_status(old.id, "failed", completed_at=_t.time() - 999)
    # newer active task for the same album + user
    newer = await _new_task(store, status="downloading")

    await orch.retry_failed_tasks()

    orch.dispatch.assert_not_called()


@pytest.mark.asyncio
async def test_retry_failed_tasks_skips_when_target_already_completed(tmp_path: Path):
    """A failed task whose target was since downloaded by a newer completed task is
    NOT auto-retried - an album already in the library must never be re-fetched."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=0.01)
    orch.dispatch = MagicMock()
    failed = await _new_task(store, status="failed", retry_count=0)
    await store.update_status(failed.id, "failed", completed_at=_t.time() - 999)
    # a later retry of the same album + user succeeded
    done = await _new_task(store, status="completed", retry_count=1)
    await store.update_status(done.id, "completed", completed_at=_t.time() - 10)

    await orch.retry_failed_tasks()

    orch.dispatch.assert_not_called()


@pytest.mark.asyncio
async def test_retry_failed_tasks_retries_partial(tmp_path: Path):
    """A partial album download is eligible for auto-retry."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=0.01)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="partial", retry_count=0)
    await store.update_status(task.id, "partial", completed_at=_t.time() - 10)

    await orch.retry_failed_tasks()

    orch.dispatch.assert_called_once()


@pytest.mark.asyncio
async def test_retry_failed_tasks_skips_cancelled(tmp_path: Path):
    """Cancelled tasks are NOT auto-retried (cancelled is an explicit user action)."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=0.01)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="cancelled", retry_count=0)
    await store.update_status(task.id, "cancelled", completed_at=_t.time() - 999)

    await orch.retry_failed_tasks()

    orch.dispatch.assert_not_called()


@pytest.mark.asyncio
async def test_create_retry_task_increments_retry_count(tmp_path: Path):
    """_create_retry_task (shared by manual + auto retry) carries retry_count + 1."""
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=2)

    new_id = await orch._create_retry_task(task)

    new_task = await store.get_task(new_id)
    assert new_task.retry_count == 3
    assert new_task.status == "queued"
    orch.dispatch.assert_called_once_with(new_id)


@pytest.mark.asyncio
async def test_retry_failed_tasks_per_task_exponential_backoff(tmp_path: Path):
    """A task with retry_count=2 waits base*4, not base. One that failed on its
    first attempt (retry_count=0) and is older than base is retried; one with
    retry_count=2 that's only base*2 old is NOT (it needs base*4)."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=0.1)
    orch.dispatch = MagicMock()
    # base = 6 seconds. retry_count=0 -> backoff=6s. retry_count=2 -> backoff=24s.

    old_first = await store.create_task(
        user_id="user-a", release_group_mbid="rg-old", artist_name="A",
        album_title="Old", year=2020, track_count=1, retry_count=0, status="failed",
    )
    await store.update_status(old_first.id, "failed", completed_at=_t.time() - 10)
    # 10s > 6s backoff -> eligible

    young_third = await store.create_task(
        user_id="user-a", release_group_mbid="rg-young", artist_name="A",
        album_title="Young", year=2020, track_count=1, retry_count=2, status="failed",
    )
    await store.update_status(young_third.id, "failed", completed_at=_t.time() - 12)
    # 12s < 24s backoff -> NOT eligible

    await orch.retry_failed_tasks()

    dispatched_ids = {call.args[0] for call in orch.dispatch.call_args_list}
    new_tasks = [await store.get_task(i) for i in dispatched_ids]
    assert any(t.release_group_mbid == "rg-old" for t in new_tasks)
    assert not any(t.release_group_mbid == "rg-young" for t in new_tasks)


@pytest.mark.asyncio
async def test_retry_failed_tasks_single_sweep_no_duplicates(tmp_path: Path):
    """A single retry_failed_tasks() call must not create multiple retries for the
    same failed task (regression: the old tier loop retried the same task up to
    max_attempts times in one sweep)."""
    store, orch, *_ = _build(tmp_path, auto_retry_base_interval_minutes=0.01)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=0)
    await store.update_status(task.id, "failed", completed_at=_t.time() - 999)

    await orch.retry_failed_tasks()

    assert orch.dispatch.call_count == 1


@pytest.mark.asyncio
async def test_create_retry_task_preserves_track_fields(tmp_path: Path):
    """_create_retry_task carries track_duration_seconds, track_number, disc_number
    so a retried track download keeps its duration gate."""
    store, orch, *_ = _build(tmp_path)
    orch.dispatch = MagicMock()
    task = await store.create_task(
        user_id="user-a", download_type="track", release_group_mbid="rg-1",
        recording_mbid="rec-1", artist_name="Artist", album_title="Album",
        track_title="Song", year=2020, track_count=1,
        track_duration_seconds=212.5, track_number=3, disc_number=2,
        retry_count=1, status="failed",
    )
    await store.update_status(task.id, "failed", completed_at=_t.time() - 100)
    original = await store.get_task(task.id)
    assert original.track_duration_seconds == 212.5
    assert original.track_number == 3
    assert original.disc_number == 2

    new_id = await orch._create_retry_task(task)

    new_task = await store.get_task(new_id)
    assert new_task.download_type == "track"
    assert new_task.recording_mbid == "rec-1"
    assert new_task.track_title == "Song"
    assert new_task.track_duration_seconds == 212.5
    assert new_task.track_number == 3
    assert new_task.disc_number == 2


@pytest.mark.asyncio
async def test_create_retry_task_relinks_album_request(tmp_path: Path):
    """A retry re-points the linked request at the replacement task so a successful
    retry actually marks the request imported and busts caches (without this the
    request stays 'failed' forever, since _sync_request_on_terminal keys on the old
    task id)."""
    record = _request_record(status="failed")
    rh = _FakeRequestHistory(record)
    store, orch, *_ = _build(tmp_path, request_history=rh)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=0)
    record.download_task_id = task.id   # request -> original task link

    new_id = await orch._create_retry_task(task)

    assert rh.relinks == [("rg-1", new_id)]
    assert record.download_task_id == new_id


@pytest.mark.asyncio
async def test_create_retry_task_does_not_relink_track_request(tmp_path: Path):
    """A per-track retry must not hijack the album's request link."""
    record = _request_record(status="failed", download_task_id="album-task")
    rh = _FakeRequestHistory(record)
    store, orch, *_ = _build(tmp_path, request_history=rh)
    orch.dispatch = MagicMock()
    task = await store.create_task(
        user_id="user-a", download_type="track", release_group_mbid="rg-1",
        recording_mbid="rec-1", artist_name="A", album_title="B", track_title="S",
        retry_count=0, status="failed",
    )

    await orch._create_retry_task(task)

    assert rh.relinks == []
    assert record.download_task_id == "album-task"


@pytest.mark.asyncio
async def test_create_retry_task_skips_relink_when_request_owned_by_other_task(tmp_path: Path):
    """If the request already points at a newer task (e.g. a manual retry), an older
    auto-retry must not steal the link back."""
    record = _request_record(status="failed", download_task_id="newer-task")
    rh = _FakeRequestHistory(record)
    store, orch, *_ = _build(tmp_path, request_history=rh)
    orch.dispatch = MagicMock()
    task = await _new_task(store, status="failed", retry_count=0)

    await orch._create_retry_task(task)

    assert rh.relinks == []
    assert record.download_task_id == "newer-task"


# -- settle_after_manual_import: an "import anyway" that completes an album must stop the retry --


@pytest.mark.asyncio
async def test_settle_after_manual_import_completes_a_finished_album(tmp_path):
    # library now has all 9 tracks (7 from the download + 2 just imported by hand)
    store, orch, _fp, _lib = _build(
        tmp_path, imported_rows=[{"disc_number": 1, "track_number": n} for n in range(1, 10)]
    )
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="Led Zeppelin",
        album_title="Led Zeppelin", track_count=9, status="partial",
    )
    await store.update_status(task.id, "partial", files_completed=7)

    await orch.settle_after_manual_import(task.id)

    settled = await store.get_task(task.id)
    assert settled.status == "completed"  # album complete -> no phantom retry
    assert settled.completed_at is not None
    assert settled.files_completed == 9


@pytest.mark.asyncio
async def test_settle_after_manual_import_stays_partial_while_incomplete(tmp_path):
    # only 8 of 9 present (one held track imported, another still pending review)
    store, orch, _fp, _lib = _build(
        tmp_path, imported_rows=[{"disc_number": 1, "track_number": n} for n in range(1, 9)]
    )
    task = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="Led Zeppelin",
        album_title="Led Zeppelin", track_count=9, status="partial",
    )
    await store.update_status(task.id, "partial", files_completed=7)

    await orch.settle_after_manual_import(task.id)

    settled = await store.get_task(task.id)
    assert settled.status == "partial"  # still missing one -> stays partial
    assert settled.files_completed == 8  # but the count advanced to reflect the import


# reimport_task - re-check the mount for a download finished by hand in slskd

async def _link_candidate(store, task_id, candidate):
    job = await store.create_search_job(
        user_id="user-a", artist_name="Artist", album_title="Album", year=2020,
        track_count=1, release_group_mbid="rg-1", search_query="Artist - Album",
    )
    await store.set_search_job_candidates(job.id, [candidate])
    await store.link_picked_candidate(
        task_id, job.id, 0, candidate.username, candidate.parent_directory,
        candidate.final_score,
    )
    return job


@pytest.mark.asyncio
async def test_reimport_task_completes_when_files_now_present(tmp_path: Path):
    store, orch, fp, lib = _build(tmp_path)
    _coupled_fp(fp, lib)
    task = await _new_task(store, status="failed", track_count=1)
    await _link_candidate(store, task.id, _candidate(0.9, files=1))

    result = await orch.reimport_task(task.id, "user-a", "user")

    assert result.status == "completed"
    fp.process_downloaded.assert_awaited()


@pytest.mark.asyncio
async def test_reimport_task_partial_when_some_still_missing(tmp_path: Path):
    store, orch, fp, lib = _build(tmp_path)
    candidate = _candidate(0.9, files=2)
    missing_file = candidate.files[1].filename
    _coupled_fp(fp, lib, fail={missing_file})
    task = await _new_task(store, status="partial", track_count=2)
    await _link_candidate(store, task.id, candidate)

    result = await orch.reimport_task(task.id, "user-a", "user")

    assert result.status == "partial"


@pytest.mark.asyncio
async def test_reimport_task_rejects_no_picked_candidate(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="failed")

    with pytest.raises(ValidationError):
        await orch.reimport_task(task.id, "user-a", "user")


@pytest.mark.asyncio
async def test_reimport_task_rejects_wrong_status(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="queued")

    with pytest.raises(ValidationError):
        await orch.reimport_task(task.id, "user-a", "user")


@pytest.mark.asyncio
async def test_reimport_task_ownership(tmp_path: Path):
    store, orch, *_ = _build(tmp_path)
    task = await _new_task(store, status="failed")
    await _link_candidate(store, task.id, _candidate(0.9, files=1))

    with pytest.raises(ResourceNotFoundError):
        await orch.reimport_task("missing", "user-a", "user")
    with pytest.raises(PermissionDeniedError):
        await orch.reimport_task(task.id, "user-b", "user")


@pytest.mark.asyncio
async def test_reimport_task_second_call_after_completion_rejected(tmp_path: Path):
    store, orch, fp, lib = _build(tmp_path)
    _coupled_fp(fp, lib)
    task = await _new_task(store, status="failed", track_count=1)
    await _link_candidate(store, task.id, _candidate(0.9, files=1))

    first = await orch.reimport_task(task.id, "user-a", "user")
    assert first.status == "completed"

    with pytest.raises(ValidationError):
        await orch.reimport_task(task.id, "user-a", "user")
