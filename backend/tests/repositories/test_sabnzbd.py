"""SabnzbdClient + SabnzbdDownloadClient queue, history, and cleanup contracts.

Shapes and completed-history deletion behavior mirror the owner's real 5.0.4.
"""

import traceback
from pathlib import Path

import httpx
import pytest

from core.exceptions import NewznabApiError
from repositories.protocols.download_client import EnqueueRequest, TaskHandle
from repositories.sabnzbd.sabnzbd_client import (
    SabnzbdApiError,
    SabnzbdClient,
    _redact_query_secrets,
)
from repositories.sabnzbd.sabnzbd_download_client import SabnzbdDownloadClient
from tests.mocks import sabnzbd_mock


def _client(mock):
    return SabnzbdClient(sabnzbd_mock.client_for(mock), "http://sab:8080", "key")


def _dc(mock, mount="/sabnzbd-downloads"):
    return SabnzbdDownloadClient(_client(mock), "http://sab:8080", "key", Path(mount))


def _handle(nzo_id="nzo-1", job_name="droppedneedle-t1"):
    return TaskHandle(source="usenet", job_name=job_name, nzo_id=nzo_id)


# Whole fake secret, kept at module level (never inline in a test body): tracebacks
# render the source of every frame, so an inline literal would false-positive the
# chain-rendering assertion below. Test bodies reference this name only.
_LIVE_SECRET = "live-secret-value"


def _assert_no_secret_leak(exc, secret):
    # The secret must be absent from str(exc), from the exception chain (redaction
    # must use `from None`: logger.exception renders __cause__ and unsuppressed
    # __context__), and from the full rendered traceback end to end.
    assert secret not in str(exc)
    assert exc.__cause__ is None
    assert exc.__suppress_context__ is True
    assert secret not in "".join(traceback.format_exception(exc))


@pytest.mark.asyncio
async def test_version_and_health():
    dc = _dc(sabnzbd_mock.SabnzbdMock())
    status = await dc.health_check()
    assert status.status == "ok"
    assert status.version == "5.0.4"


@pytest.mark.asyncio
async def test_auth_error_surfaces():
    client = SabnzbdClient(
        sabnzbd_mock.client_for(type("M", (), {"handler": staticmethod(sabnzbd_mock.auth_error_handler)})()),
        "http://sab:8080", "bad",
    )
    with pytest.raises(SabnzbdApiError) as exc:
        await client.queue()
    assert exc.value.auth is True


@pytest.mark.asyncio
async def test_enqueue_returns_handle_with_nzo_id(monkeypatch):
    mock = sabnzbd_mock.SabnzbdMock()
    mock.add_nzo_ids = ["nzo-xyz"]
    dc = _dc(mock)

    async def fake_fetch(url, *, timeout=60.0):
        return b"<?xml version='1.0'?><nzb></nzb>"

    monkeypatch.setattr(dc._client, "fetch_nzb", fake_fetch)
    handle = await dc.enqueue(
        EnqueueRequest(task_id="t1", source="usenet", nzb_url="https://idx/nzb",
                       job_name="droppedneedle-t1", category="audio")
    )
    assert handle.source == "usenet"
    assert handle.nzo_id == "nzo-xyz"
    assert handle.job_name == "droppedneedle-t1"
    assert len(mock.add_file_requests) == 1
    assert mock.add_url_requests == []  # success path never touches the fallback


@pytest.mark.asyncio
async def test_enqueue_falls_back_to_addurl_when_fetch_fails(monkeypatch):
    # No-response transport failure (not a content rejection or definitive HTTP
    # error): the indexer may be reachable only from the SABnzbd host, so enqueue
    # hands SABnzbd the enclosure URL.
    mock = sabnzbd_mock.SabnzbdMock()
    mock.add_nzo_ids = ["nzo-fallback"]
    dc = _dc(mock)

    async def failing_fetch(url, *, timeout=60.0):
        error = NewznabApiError("NZB fetch failed: connection refused")
        error.transport_failure = True
        raise error

    monkeypatch.setattr(dc._client, "fetch_nzb", failing_fetch)
    handle = await dc.enqueue(
        EnqueueRequest(
            task_id="t1",
            source="usenet",
            nzb_url="https://indexer.example/getnzb/abc?apikey=test-key",
            job_name="droppedneedle-t1",
            category="audio",
            priority=1,
            post_processing=3,
        )
    )
    assert handle.source == "usenet"
    assert handle.nzo_id == "nzo-fallback"
    assert handle.job_name == "droppedneedle-t1"
    assert mock.add_file_requests == []  # fallback only - never both in one enqueue
    assert mock.add_url_requests == [
        {
            "mode": "addurl",
            "name": "https://indexer.example/getnzb/abc?apikey=test-key",
            "nzbname": "droppedneedle-t1",
            "cat": "audio",
            "priority": "1",
            "pp": "3",
            "output": "json",
            "apikey": "key",
        }
    ]


@pytest.mark.asyncio
async def test_enqueue_fallback_without_nzo_id_raises(monkeypatch):
    mock = sabnzbd_mock.SabnzbdMock()
    mock.add_nzo_ids = []
    dc = _dc(mock)

    async def failing_fetch(url, *, timeout=60.0):
        error = NewznabApiError("NZB fetch failed: connection refused")
        error.transport_failure = True
        raise error

    monkeypatch.setattr(dc._client, "fetch_nzb", failing_fetch)
    with pytest.raises(SabnzbdApiError, match="no nzo_id returned"):
        await dc.enqueue(
            EnqueueRequest(task_id="t1", source="usenet", nzb_url="https://idx/nzb",
                           job_name="droppedneedle-t1", category="audio")
        )
    assert len(mock.add_url_requests) == 1
    assert mock.add_file_requests == []


@pytest.mark.asyncio
async def test_enqueue_does_not_fall_back_on_content_rejection(monkeypatch):
    # A deterministic content rejection (indexer error/limit page, not an NZB) must
    # propagate to the blocklist path - falling back to addurl would queue the error
    # page in SABnzbd instead of rejecting it before download.
    mock = sabnzbd_mock.SabnzbdMock()
    dc = _dc(mock)

    async def rejecting_fetch(url, *, timeout=60.0):
        error = NewznabApiError(
            "indexer returned a non-NZB body, not an NZB",
            details={"status": 200},
            code=200,
        )
        error.content_rejection = True
        raise error

    monkeypatch.setattr(dc._client, "fetch_nzb", rejecting_fetch)
    with pytest.raises(NewznabApiError) as exc_info:
        await dc.enqueue(
            EnqueueRequest(task_id="t1", source="usenet", nzb_url="https://idx/nzb",
                           job_name="droppedneedle-t1", category="audio")
        )
    # The marker, message, details, and code must survive for the blocklist path.
    assert exc_info.value.content_rejection is True
    assert exc_info.value.message == "indexer returned a non-NZB body, not an NZB"
    assert exc_info.value.details == {"status": 200}
    assert exc_info.value.code == 200
    assert mock.add_url_requests == []
    assert mock.add_file_requests == []


@pytest.mark.asyncio
async def test_enqueue_does_not_fall_back_on_definitive_http_error(monkeypatch):
    # The indexer answered (401/403/429/5xx): handing the same URL to SABnzbd cannot
    # help, so fail over without queueing a deterministically-doomed job.
    mock = sabnzbd_mock.SabnzbdMock()
    dc = _dc(mock)

    async def http_error_fetch(url, *, timeout=60.0):
        raise NewznabApiError("NZB fetch returned HTTP 429", details="limit hit", code=429)

    monkeypatch.setattr(dc._client, "fetch_nzb", http_error_fetch)
    with pytest.raises(NewznabApiError) as exc_info:
        await dc.enqueue(
            EnqueueRequest(task_id="t1", source="usenet", nzb_url="https://idx/nzb",
                           job_name="droppedneedle-t1", category="audio")
        )
    assert exc_info.value.message == "NZB fetch returned HTTP 429"
    assert exc_info.value.details == "limit hit"
    assert exc_info.value.code == 429
    assert mock.add_url_requests == []
    assert mock.add_file_requests == []


@pytest.mark.asyncio
async def test_only_downloading_is_active():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.queue_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Downloading",
                   mb="100.0", mbleft="40.0", percentage="60")
    status = await _dc(mock).get_status(_handle())
    assert status.status == "downloading"
    assert status.has_active_transfer is True
    assert status.progress_percent == 60.0
    # mb/mbleft are decimal-MB strings -> bytes.
    assert status.bytes_total == int(100.0 * 1024 * 1024)
    assert status.bytes_downloaded == int(60.0 * 1024 * 1024)


@pytest.mark.asyncio
async def test_queue_numbers_as_json_numbers_dont_crash_parse():
    # Some SABnzbd builds emit mb/mbleft/percentage as JSON numbers, not strings; the
    # model must accept both (else the whole queue parse raises).
    mock = sabnzbd_mock.SabnzbdMock()
    mock.queue_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Downloading",
                   mb=100.0, mbleft=25.0, percentage=75)  # floats/ints, not strings
    status = await _dc(mock).get_status(_handle())
    assert status.status == "downloading"
    assert status.progress_percent == 75.0
    assert status.bytes_downloaded == int(75.0 * 1024 * 1024)


@pytest.mark.asyncio
async def test_deleted_history_job_fails_fast():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Deleted")
    status = await _dc(mock).get_status(_handle())
    assert status.status == "failed"  # don't poll a removed job to the 6h deadline


@pytest.mark.asyncio
@pytest.mark.parametrize("state", ["Queued", "Grabbing", "Paused", "Propagating"])
async def test_queued_states_are_not_active(state):
    mock = sabnzbd_mock.SabnzbdMock()
    mock.queue_job(nzo_id="nzo-1", name="droppedneedle-t1", status=state, mb="100.0", mbleft="100.0", percentage="0")
    status = await _dc(mock).get_status(_handle())
    assert status.status == "queued"
    assert status.has_active_transfer is False  # must not trip the stall/queued watchdog


@pytest.mark.asyncio
@pytest.mark.parametrize("state", ["Verifying", "Repairing", "Extracting", "Moving"])
async def test_post_processing_is_processing_not_active(state):
    mock = sabnzbd_mock.SabnzbdMock()
    # SABnzbd zeroes the queue percentage during unpack; the download is finished, so the
    # bar must hold at 100% (status conveys the phase) instead of dropping to 0.
    mock.queue_job(nzo_id="nzo-1", name="droppedneedle-t1", status=state, mb="100.0", mbleft="0.0", percentage="0")
    status = await _dc(mock).get_status(_handle())
    assert status.status == "processing"
    assert status.has_active_transfer is False
    assert status.progress_percent == 100.0


@pytest.mark.asyncio
@pytest.mark.parametrize("state", ["Verifying", "Repairing", "Extracting", "Moving"])
async def test_post_processing_in_history_holds_at_100(state):
    # Once the job moves to history for the final unpack/move, there's no percentage at
    # all - it must still report 100%, not the struct default of 0.
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status=state, bytes_=2_000_000_000)
    status = await _dc(mock).get_status(_handle())
    assert status.status == "processing"
    assert status.progress_percent == 100.0
    assert status.bytes_downloaded == 2_000_000_000


@pytest.mark.asyncio
async def test_completed_history_maps_to_completed():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Completed",
                     storage="/data/Downloads/complete/droppedneedle-t1", bytes_=2_000_000_000)
    status = await _dc(mock).get_status(_handle())
    assert status.status == "completed"
    assert status.bytes_total == 2_000_000_000


@pytest.mark.asyncio
async def test_failed_history_maps_to_failed_with_message():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Failed",
                     fail_message="Unpacking failed, unwanted extension")
    status = await _dc(mock).get_status(_handle())
    assert status.status == "failed"
    assert "Unpacking failed" in (status.error or "")


@pytest.mark.asyncio
async def test_crash_recovery_matches_by_job_name_when_nzo_unknown():
    # nzo_id wasn't persisted (crash between addfile and persist) -> match by job name.
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-real", name="droppedneedle-t1", status="Completed", storage="/x")
    status = await _dc(mock).get_status(TaskHandle(source="usenet", job_name="droppedneedle-t1"))
    assert status.status == "completed"


@pytest.mark.asyncio
async def test_known_nzo_id_never_falls_back_to_same_job_name():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.queue_job(
        nzo_id="nzo-other", name="droppedneedle-t1", status="Downloading"
    )
    mock.history_job(
        nzo_id="nzo-1", name="droppedneedle-t1", status="Completed", storage="/x"
    )

    status = await _dc(mock).get_status(_handle())

    assert status.status == "completed"


@pytest.mark.asyncio
async def test_name_only_fallback_rejects_ambiguous_history():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(
        nzo_id="nzo-1", name="droppedneedle-t1", status="Completed", storage="/x"
    )
    mock.history_job(
        nzo_id="nzo-2", name="droppedneedle-t1", status="Completed", storage="/y"
    )

    with pytest.raises(SabnzbdApiError, match="ambiguous job identity"):
        await _dc(mock).inspect_materialization(
            TaskHandle(source="usenet", job_name="droppedneedle-t1")
        )


@pytest.mark.asyncio
async def test_completed_history_discard_never_claims_to_delete_output():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Completed", storage="/x")
    ok = await _dc(mock).discard_client_artifacts(_handle())
    assert ok is True
    assert ("history", "nzo-1") in mock.deleted
    assert mock.delete_requests[-1]["del_files"] == "0"
    assert mock.deleted_storage == []


@pytest.mark.asyncio
async def test_mock_records_sab_retaining_completed_output_for_del_files_one():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(
        nzo_id="nzo-1",
        name="droppedneedle-t1",
        status="Completed",
        storage="/complete/droppedneedle-t1",
    )

    assert await _client(mock).delete_history("nzo-1", del_files=True) is True

    assert mock.deleted_storage == []
    assert mock.retained_completed_storage == ["/complete/droppedneedle-t1"]


@pytest.mark.asyncio
async def test_failed_history_discard_removes_incomplete_data():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Failed")
    assert await _dc(mock).discard_client_artifacts(_handle()) is True
    assert mock.delete_requests[-1]["del_files"] == "1"


@pytest.mark.asyncio
async def test_active_abort_removes_client_owned_temporary_data():
    mock = sabnzbd_mock.SabnzbdMock()
    mock.queue_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Downloading")
    assert await _dc(mock).abort(_handle()) is True
    assert mock.delete_requests[-1]["mode"] == "queue"
    assert mock.delete_requests[-1]["del_files"] == "1"


@pytest.mark.asyncio
async def test_materialization_requires_exact_complete_dir_mapping(tmp_path):
    mock = sabnzbd_mock.SabnzbdMock()
    mock.complete_dir = "/data/Downloads/complete"
    job_name = "droppedneedle-t1"
    mock.history_job(
        nzo_id="nzo-1",
        name=job_name,
        status="Completed",
        storage=f"/data/Downloads/complete/audio/{job_name}",
    )
    mount = tmp_path / "complete"
    mount.mkdir()

    materialization = await _dc(mock, mount=str(mount)).inspect_materialization(
        _handle()
    )

    assert materialization.workspace_path == str(mount / "audio" / job_name)


@pytest.mark.asyncio
async def test_materialization_rejects_ambiguous_storage_mapping(tmp_path):
    mock = sabnzbd_mock.SabnzbdMock()
    mock.complete_dir = ""
    mock.history_job(
        nzo_id="nzo-1",
        name="droppedneedle-t1",
        status="Completed",
        storage="/unknown/droppedneedle-t1",
    )
    mount = tmp_path / "complete"
    mount.mkdir()

    materialization = await _dc(mock, mount=str(mount)).inspect_materialization(
        _handle()
    )

    assert materialization.workspace_path == ""


@pytest.mark.asyncio
async def test_list_completed_files_remaps_and_enumerates(tmp_path):
    # storage in SABnzbd's namespace -> remap onto the local mount -> enumerate audio.
    job = tmp_path / "complete" / "droppedneedle-t1"
    job.mkdir(parents=True)
    (job / "01 track.flac").write_bytes(b"x")
    (job / "cover.jpg").write_bytes(b"x")  # excluded
    mock = sabnzbd_mock.SabnzbdMock()
    mock.complete_dir = "/data/Downloads/complete"
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Completed",
                     storage="/data/Downloads/complete/droppedneedle-t1")
    dc = _dc(mock, mount=str(tmp_path / "complete"))
    files = await dc.list_completed_files(_handle())
    assert [f.name for f in files] == ["01 track.flac"]  # the jpg is excluded


@pytest.mark.asyncio
async def test_list_completed_files_normalizes_windows_separators(tmp_path):
    # A Windows-native SAB reports backslash storage paths; without separator
    # folding the remap misses and every import enumerates 0 (#245 shape 1).
    job = tmp_path / "complete" / "droppedneedle-t1"
    job.mkdir(parents=True)
    (job / "01 track.flac").write_bytes(b"x")
    mock = sabnzbd_mock.SabnzbdMock()
    mock.complete_dir = "C:\\Downloads\\complete"
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Completed",
                     storage="C:\\Downloads\\complete\\droppedneedle-t1")
    files = await _dc(mock, mount=str(tmp_path / "complete")).list_completed_files(
        _handle()
    )
    assert [f.name for f in files] == ["01 track.flac"]


@pytest.mark.asyncio
async def test_list_completed_files_resolves_category_subfolder_mount(tmp_path):
    # downloads_mount pointed at complete_dir/music while SAB stores under
    # complete_dir/music/job: the stripped remainder still carries the category
    # component, resolved by the suffix walk (#245 shape 2).
    job = tmp_path / "complete" / "droppedneedle-t1"
    job.mkdir(parents=True)
    (job / "01 track.flac").write_bytes(b"x")
    mock = sabnzbd_mock.SabnzbdMock()
    mock.complete_dir = "/data/Downloads/complete"
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Completed",
                     storage="/data/Downloads/complete/music/droppedneedle-t1")
    files = await _dc(mock, mount=str(tmp_path / "complete")).list_completed_files(
        _handle()
    )
    assert [f.name for f in files] == ["01 track.flac"]


@pytest.mark.asyncio
async def test_unresolvable_storage_falls_back_to_basename_with_warning(
    tmp_path, caplog
):
    import logging

    mount = tmp_path / "complete"
    mount.mkdir()
    mock = sabnzbd_mock.SabnzbdMock()
    mock.complete_dir = "/data/Downloads/complete"
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Completed",
                     storage="/elsewhere/droppedneedle-t1")
    with caplog.at_level(logging.WARNING):
        files = await _dc(mock, mount=str(mount)).list_completed_files(_handle())
    assert files == []
    assert "sabnzbd storage remap failed" in caplog.text
    assert "/elsewhere/droppedneedle-t1" in caplog.text


@pytest.mark.asyncio
async def test_exact_storage_normalizes_windows_separators(tmp_path):
    mock = sabnzbd_mock.SabnzbdMock()
    mock.complete_dir = "C:\\Downloads\\complete"
    mock.history_job(
        nzo_id="nzo-1",
        name="droppedneedle-t1",
        status="Completed",
        storage="C:\\Downloads\\complete\\audio\\droppedneedle-t1",
    )
    mount = tmp_path / "complete"
    mount.mkdir()

    materialization = await _dc(mock, mount=str(mount)).inspect_materialization(
        _handle()
    )

    assert materialization.workspace_path == str(mount / "audio" / "droppedneedle-t1")


@pytest.mark.asyncio
async def test_history_lookup_filters_to_the_job():
    # A busy SABnzbd can push our job past the 50-entry window; the client must filter
    # history to THIS job (nzo_ids/search) so it's always found.
    mock = sabnzbd_mock.SabnzbdMock()
    mock.history_job(nzo_id="nzo-1", name="droppedneedle-t1", status="Completed", storage="/x")
    await _dc(mock).get_status(_handle())
    assert mock.history_requests, "history was queried"
    last = mock.history_requests[-1]
    assert last.get("nzo_ids") == "nzo-1" or last.get("search") == "droppedneedle-t1"


@pytest.mark.asyncio
async def test_downloads_mount_healthy_checks_root_not_per_job(tmp_path):
    # Health is about the MOUNT ROOT, not the per-job folder: a healthy mount whose job
    # folder is empty/absent is a bad RELEASE, not a mount fault.
    mock = sabnzbd_mock.SabnzbdMock()
    mount = tmp_path / "complete"
    dc = _dc(mock, mount=str(mount))
    assert await dc.downloads_mount_healthy() is False  # mount root doesn't exist
    mount.mkdir(parents=True)
    assert await dc.downloads_mount_healthy() is True   # root exists+readable, even if empty
    # A missing per-job folder under a healthy root is NOT a mount fault.
    assert await dc.downloads_mount_healthy() is True


# --- SAB resilience: retry on transient transport errors / 5xx (step 6) ---------------


def _counting_client(fail_times: int, *, exc=None, status=None):
    """Mock httpx that fails the first ``fail_times`` calls (a transport error, or a 5xx
    response) then serves an empty (Idle) queue. Returns (client, calls)."""
    calls = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        calls["n"] += 1
        if calls["n"] <= fail_times:
            if exc is not None:
                raise exc
            return httpx.Response(status, content=b"upstream error")
        return httpx.Response(
            200,
            content=b'{"queue": {"status": "Idle", "paused": false, "slots": []}}',
            headers={"Content-Type": "application/json"},
        )

    return httpx.AsyncClient(transport=httpx.MockTransport(handler)), calls


@pytest.mark.asyncio
async def test_transient_transport_error_is_retried_then_succeeds():
    http, calls = _counting_client(2, exc=httpx.ConnectError("blip"))
    client = SabnzbdClient(http, "http://sab:8080", "key", retry_backoff=0)
    queue = await client.queue()
    assert calls["n"] == 3  # 2 failures + 1 success
    assert queue.status == "Idle"


@pytest.mark.asyncio
async def test_5xx_is_retried_then_succeeds():
    http, calls = _counting_client(1, status=503)
    client = SabnzbdClient(http, "http://sab:8080", "key", retry_backoff=0)
    queue = await client.queue()
    assert calls["n"] == 2
    assert queue.status == "Idle"


@pytest.mark.asyncio
async def test_persistent_transport_error_raises_after_max_attempts():
    http, calls = _counting_client(99, exc=httpx.ConnectError("down"))
    client = SabnzbdClient(http, "http://sab:8080", "key", max_attempts=3, retry_backoff=0)
    with pytest.raises(SabnzbdApiError):
        await client.queue()
    assert calls["n"] == 3  # exactly max_attempts, then gives up


@pytest.mark.asyncio
async def test_addfile_is_not_retried():
    # A non-idempotent POST must never be re-sent: re-adding would create a duplicate job
    # and the .1/.2 orphan the unique-job-name fix removed.
    calls = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        calls["n"] += 1
        raise httpx.ConnectError("blip")

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(SabnzbdApiError):
        await client.add_file("droppedneedle-t1", b"<nzb></nzb>")
    assert calls["n"] == 1  # one attempt only - no retry


@pytest.mark.asyncio
async def test_addurl_is_not_retried():
    # addurl also mutates the queue. An uncertain transport failure must not cause
    # a second enqueue attempt and a duplicate SABnzbd job.
    calls = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        calls["n"] += 1
        raise httpx.ConnectError("blip")

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(SabnzbdApiError):
        await client.add_url("droppedneedle-t1", "https://indexer.example/getnzb/abc")
    assert calls["n"] == 1


@pytest.mark.asyncio
async def test_addurl_error_scrubs_echoed_enclosure_credentials():
    # If SABnzbd echoes the submitted enclosure URL in an error body, the indexer's
    # apikey must not reach the raised message (the strategy logs it).
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={
            "status": False,
            "error": "Failed to fetch https://indexer.example/getnzb/abc?apikey=" + _LIVE_SECRET,
        })

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(SabnzbdApiError) as exc_info:
        await client.add_url(
            "droppedneedle-t1", "https://indexer.example/getnzb/abc?apikey=" + _LIVE_SECRET
        )
    assert "apikey=***" in exc_info.value.message
    _assert_no_secret_leak(exc_info.value, _LIVE_SECRET)


def test_redact_query_secrets_precision():
    # Unit-pins the regex precision properties: both credential forms in any case,
    # &-termination preserving trailing params, and no mangling of innocent words.
    assert _redact_query_secrets("https://idx/get?apikey=SECRET&next=1") == (
        "https://idx/get?apikey=***&next=1"
    )
    assert _redact_query_secrets("APIKEY=SECRET") == "APIKEY=***"
    assert _redact_query_secrets("https://idx/get?api_key=SECRET") == (
        "https://idx/get?api_key=***"
    )
    assert _redact_query_secrets("API_KEY=SECRET") == "API_KEY=***"
    assert _redact_query_secrets("https://idx/get?t=get&i=7&R=SECRET") == (
        "https://idx/get?t=get&i=7&R=***"
    )
    assert _redact_query_secrets("error=boom&filter=x") == "error=boom&filter=x"
    assert _redact_query_secrets("https://idx/get?t=get&id=abc&i=7") == (
        "https://idx/get?t=get&id=abc&i=7"
    )


@pytest.mark.asyncio
async def test_addurl_error_redaction_preserves_auth_flag():
    # The auth flag is detected before redaction and must survive the rebuild.
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={
            "status": False,
            "error": "API Key Incorrect for https://indexer.example/getnzb?apikey=" + _LIVE_SECRET,
        })

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(SabnzbdApiError) as exc_info:
        await client.add_url(
            "droppedneedle-t1", "https://indexer.example/getnzb?apikey=" + _LIVE_SECRET
        )
    assert exc_info.value.auth is True
    assert "apikey=***" in exc_info.value.message
    _assert_no_secret_leak(exc_info.value, _LIVE_SECRET)


@pytest.mark.asyncio
async def test_addurl_http_error_details_scrubbed():
    # HTTP-status failures carry the SABnzbd body in details - also scrubbed.
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            400,
            content=b"error: cannot fetch https://indexer.example/getnzb?apikey=" + _LIVE_SECRET.encode(),
        )

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(SabnzbdApiError) as exc_info:
        await client.add_url(
            "droppedneedle-t1", "https://indexer.example/getnzb?apikey=" + _LIVE_SECRET
        )
    assert "apikey=***" in exc_info.value.details
    _assert_no_secret_leak(exc_info.value, _LIVE_SECRET)


@pytest.mark.asyncio
async def test_addurl_plaintext_error_scrubs_echoed_credentials():
    # The plain-text `error: ...` form, echoing a DrunkenSlug-style URL: the `r=`
    # per-user key is scrubbed while the `i=` user id stays visible.
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            content=b"error: Failed to fetch https://indexer.example/get?t=get&id=abc&i=7&r=" + _LIVE_SECRET.encode(),
        )

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(SabnzbdApiError) as exc_info:
        await client.add_url(
            "droppedneedle-t1", "https://indexer.example/get?t=get&id=abc&i=7&r=" + _LIVE_SECRET
        )
    assert "r=***" in exc_info.value.message
    assert "i=7" in exc_info.value.message
    _assert_no_secret_leak(exc_info.value, _LIVE_SECRET)


@pytest.mark.asyncio
async def test_addurl_non_json_error_scrubs_echoed_credentials():
    # A non-JSON body embeds in the message - scrubbed the same way.
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            content=b"<html>fetch of https://indexer.example/getnzb?apikey=" + _LIVE_SECRET.encode() + b" failed</html>",
            headers={"Content-Type": "text/html"},
        )

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(SabnzbdApiError) as exc_info:
        await client.add_url(
            "droppedneedle-t1", "https://indexer.example/getnzb?apikey=" + _LIVE_SECRET
        )
    assert "apikey=***" in exc_info.value.message
    _assert_no_secret_leak(exc_info.value, _LIVE_SECRET)


# --- fetch_nzb: indexer error/limit page vs real NZB (issue #266) ------------------


def _nzb_client(body: bytes, *, status=200, content_type="text/html; charset=utf-8"):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            status, content=body, headers={"Content-Type": content_type}
        )

    return SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080",
        "key",
        retry_backoff=0,
    )


@pytest.mark.asyncio
async def test_fetch_nzb_html_error_page_raises_with_details():
    # An indexer answering the enclosure URL with an HTML error/limit page (HTTP
    # 200) must raise with structured details + code so the enqueue path can tell
    # a deterministic content rejection from a retry-worthy transport error.
    body = (
        b'<html><head><title>Indexer error</title></head><body><error code="300"'
        b' description="Limit reached, maximum downloads exceeded"/>'
        b"<!-- padding to push past truncation " + b"x" * 300 + b" --></body></html>"
    )
    client = _nzb_client(body)
    with pytest.raises(NewznabApiError) as exc_info:
        await client.fetch_nzb("http://indexer.example/getnzb/abc&i=1&r=key")
    exc = exc_info.value
    assert exc.message == (
        "indexer returned a non-NZB body (likely an error/limit page), not an NZB"
    )
    assert exc.code == 200
    assert exc.details == {
        "status": 200,
        "content_type": "text/html; charset=utf-8",
        "snippet": body.decode("ascii")[:200],
    }
    assert len(exc.details["snippet"]) <= 200
    assert getattr(exc, "content_rejection", False) is True
    assert getattr(exc, "transport_failure", False) is False


@pytest.mark.asyncio
async def test_fetch_nzb_valid_nzb_returns_bytes():
    nzb = (
        b'<?xml version="1.0" encoding="utf-8"?>'
        b'<nzb xmlns="http://www.newzbin.com/DTD/nzb/nzb-1.1"><file></file></nzb>'
    )
    client = _nzb_client(nzb, content_type="application/x-nzb")
    assert await client.fetch_nzb("http://indexer.example/getnzb/abc") == nzb


@pytest.mark.asyncio
async def test_fetch_nzb_transport_failure_marks_for_addurl_fallback():
    # A no-response transport failure (DNS/refused/timeout) carries the marker the
    # enqueue fallback gates on - unlike content rejections and HTTP errors.
    def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("down")

    client = SabnzbdClient(
        httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        "http://sab:8080", "key", retry_backoff=0,
    )
    with pytest.raises(NewznabApiError) as exc_info:
        await client.fetch_nzb("http://indexer.example/getnzb/abc")
    assert getattr(exc_info.value, "transport_failure", False) is True
    assert getattr(exc_info.value, "content_rejection", False) is False


@pytest.mark.asyncio
async def test_fetch_nzb_http_error_carries_neither_marker():
    # A definitive indexer HTTP answer (401/403/429/5xx) is neither a transport
    # failure nor a content rejection: enqueue must fail fast, not fall back.
    client = _nzb_client(b"limit hit", status=429, content_type="text/plain")
    with pytest.raises(NewznabApiError) as exc_info:
        await client.fetch_nzb("http://indexer.example/getnzb/abc")
    assert getattr(exc_info.value, "transport_failure", False) is False
    assert getattr(exc_info.value, "content_rejection", False) is False
