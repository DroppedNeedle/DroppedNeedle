"""SabnzbdClient + SabnzbdDownloadClient queue, history, and cleanup contracts.

Shapes and completed-history deletion behavior mirror the owner's real 5.0.4.
"""

from pathlib import Path

import httpx
import pytest

from repositories.protocols.download_client import EnqueueRequest, TaskHandle
from repositories.sabnzbd.sabnzbd_client import SabnzbdApiError, SabnzbdClient
from repositories.sabnzbd.sabnzbd_download_client import SabnzbdDownloadClient
from tests.mocks import sabnzbd_mock


def _client(mock):
    return SabnzbdClient(sabnzbd_mock.client_for(mock), "http://sab:8080", "key")


def _dc(mock, mount="/sabnzbd-downloads"):
    return SabnzbdDownloadClient(_client(mock), "http://sab:8080", "key", Path(mount))


def _handle(nzo_id="nzo-1", job_name="droppedneedle-t1"):
    return TaskHandle(source="usenet", job_name=job_name, nzo_id=nzo_id)


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
