"""NzbgetClient + NzbgetDownloadClient queue, history, and cleanup contracts.

The behaviours asserted here are the ones that differ from SABnzbd and so cannot be
inferred from its suite: Lo/Hi size reassembly, the compound ``CATEGORY/DETAIL``
history status, ``${MainDir}`` expansion, the editqueue command chosen for completed
versus failed cleanup, and the fact that credentials never reach a URL.
"""

import base64
from pathlib import Path

import httpx
import pytest

from core.exceptions import NewznabApiError
from repositories.nzbget.nzbget_client import NzbgetApiError, NzbgetClient
from repositories.nzbget.nzbget_download_client import NzbgetDownloadClient
from repositories.protocols.download_client import EnqueueRequest, TaskHandle
from tests.mocks import nzbget_mock

JOB = "droppedneedle-task-1"


def _client(mock):
    return NzbgetClient(nzbget_mock.client_for(mock), "http://nzbget:6789", "nzbget", "secret")


def _dc(mock, mount="/nzbget-downloads"):
    return NzbgetDownloadClient(
        _client(mock), "http://nzbget:6789", "nzbget", "secret", Path(mount)
    )


# --- transport ------------------------------------------------------------------


@pytest.mark.asyncio
async def test_credentials_use_basic_auth_and_never_reach_the_url():
    seen: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["auth"] = request.headers.get("authorization", "")
        return httpx.Response(
            200, json={"version": "1.1", "result": "24.3"}, headers={"Content-Type": "application/json"}
        )

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = NzbgetClient(http, "http://nzbget:6789", "nzbget", "secret")
    assert await client.version() == "24.3"
    # NZBGet also accepts /user:pass/jsonrpc; using it would leak the password into
    # every URL and from there into exception text.
    assert seen["url"] == "http://nzbget:6789/jsonrpc"
    assert "secret" not in seen["url"]
    assert seen["auth"].startswith("Basic ")


@pytest.mark.asyncio
async def test_401_is_reported_as_an_auth_failure():
    http = httpx.AsyncClient(transport=httpx.MockTransport(nzbget_mock.auth_error_handler))
    client = NzbgetClient(http, "http://nzbget:6789", "nzbget", "wrong")
    with pytest.raises(NzbgetApiError) as exc:
        await client.version()
    assert exc.value.auth is True


@pytest.mark.asyncio
async def test_jsonrpc_error_envelope_raises():
    mock = nzbget_mock.NzbgetMock()
    client = _client(mock)
    with pytest.raises(NzbgetApiError, match="unknown method"):
        await client._call("bogus", [], timeout=5.0)


@pytest.mark.asyncio
async def test_non_json_body_names_the_likely_cause():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"<html>NZBGet web UI</html>")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = NzbgetClient(http, "http://nzbget:6789", "nzbget", "secret")
    with pytest.raises(NzbgetApiError, match="non-JSON"):
        await client.version()


# --- enqueue --------------------------------------------------------------------


@pytest.mark.asyncio
async def test_enqueue_posts_the_validated_nzb_base64_and_returns_the_nzbid(monkeypatch):
    mock = nzbget_mock.NzbgetMock()
    mock.append_result = 42
    dc = _dc(mock)

    async def fake_fetch(url, *, timeout=60.0):
        return b"<?xml version='1.0'?><nzb></nzb>"

    monkeypatch.setattr(dc._client, "fetch_nzb", fake_fetch)
    handle = await dc.enqueue(
        EnqueueRequest(task_id="task-1", source="usenet", nzb_url="http://idx/x.nzb", job_name=JOB)
    )
    assert handle.nzo_id == "42"
    assert handle.job_name == JOB
    params = mock.append_calls[0]
    assert params[0] == f"{JOB}.nzb"
    assert base64.b64decode(params[1]).startswith(b"<?xml")
    # DupeKey carries the same correlation key, and FORCE stops NZBGet discarding the
    # job as a duplicate of an earlier grab the orchestrator already decided to replace.
    assert params[6] == JOB
    assert params[8] == "FORCE"


@pytest.mark.asyncio
async def test_enqueue_rejects_when_nzbget_returns_zero(monkeypatch):
    mock = nzbget_mock.NzbgetMock()
    mock.append_result = 0
    dc = _dc(mock)

    async def fake_fetch(url, *, timeout=60.0):
        return b"<nzb/>"

    monkeypatch.setattr(dc._client, "fetch_nzb", fake_fetch)
    with pytest.raises(NzbgetApiError, match="no NZBID"):
        await dc.enqueue(
            EnqueueRequest(task_id="t", source="usenet", nzb_url="http://idx/x.nzb", job_name=JOB)
        )


@pytest.mark.asyncio
async def test_transport_failure_falls_back_to_letting_nzbget_fetch_the_url(monkeypatch):
    mock = nzbget_mock.NzbgetMock()
    dc = _dc(mock)

    async def fake_fetch(url, *, timeout=60.0):
        error = NewznabApiError("boom")
        error.transport_failure = True
        raise error

    monkeypatch.setattr(dc._client, "fetch_nzb", fake_fetch)
    handle = await dc.enqueue(
        EnqueueRequest(task_id="t", source="usenet", nzb_url="http://idx/x.nzb", job_name=JOB)
    )
    assert handle.nzo_id == str(mock.append_result)
    assert mock.append_calls[0][1] == "http://idx/x.nzb"


@pytest.mark.asyncio
async def test_content_rejection_is_not_handed_to_nzbget(monkeypatch):
    mock = nzbget_mock.NzbgetMock()
    dc = _dc(mock)

    async def fake_fetch(url, *, timeout=60.0):
        error = NewznabApiError("indexer error page")
        error.content_rejection = True
        raise error

    monkeypatch.setattr(dc._client, "fetch_nzb", fake_fetch)
    # The indexer answered, so handing NZBGet the same URL cannot help; the failure has
    # to reach the blocklist path instead.
    with pytest.raises(NewznabApiError):
        await dc.enqueue(
            EnqueueRequest(task_id="t", source="usenet", nzb_url="http://idx/x.nzb", job_name=JOB)
        )
    assert mock.append_calls == []


# --- status ---------------------------------------------------------------------


@pytest.mark.asyncio
async def test_downloading_group_reports_progress_from_lo_hi_sizes():
    size = 5 * 1024 * 1024 * 1024  # 5 GiB, so the Hi half is non-zero
    mock = nzbget_mock.NzbgetMock().group(
        nzb_id=7, name=JOB, status="DOWNLOADING", file_size=size, remaining=size // 4
    )
    status = await _dc(mock).get_status(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    assert status.status == "downloading"
    assert status.has_active_transfer is True
    assert status.bytes_total == size
    assert status.bytes_downloaded == size - size // 4
    assert status.progress_percent == pytest.approx(75.0)


@pytest.mark.parametrize("state", ["QUEUED", "PAUSED", "FETCHING"])
@pytest.mark.asyncio
async def test_non_moving_states_are_queued_not_active(state):
    mock = nzbget_mock.NzbgetMock().group(nzb_id=7, name=JOB, status=state)
    status = await _dc(mock).get_status(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    # These must not set has_active_transfer, or the stall watchdog picks the wrong clock.
    assert status.status == "queued"
    assert status.has_active_transfer is False


@pytest.mark.parametrize("state", ["UNPACKING", "REPAIRING", "MOVING", "PP_QUEUED"])
@pytest.mark.asyncio
async def test_post_processing_holds_the_bar_at_100(state):
    mock = nzbget_mock.NzbgetMock().group(
        nzb_id=7, name=JOB, status=state, file_size=1000, remaining=0
    )
    status = await _dc(mock).get_status(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    assert status.status == "processing"
    assert status.progress_percent == 100.0
    assert status.has_active_transfer is False


@pytest.mark.asyncio
async def test_success_history_completes():
    mock = nzbget_mock.NzbgetMock().history(
        nzb_id=7, name=JOB, status="SUCCESS/ALL", file_size=2048
    )
    status = await _dc(mock).get_status(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    assert status.status == "completed"
    assert status.bytes_downloaded == 2048


@pytest.mark.asyncio
async def test_failure_history_names_the_stage_that_failed():
    mock = nzbget_mock.NzbgetMock().history(
        nzb_id=7, name=JOB, status="FAILURE/UNPACK", unpack_status="FAILURE"
    )
    status = await _dc(mock).get_status(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    assert status.status == "failed"
    assert "unpack" in status.error


@pytest.mark.asyncio
async def test_deleted_history_is_terminal():
    mock = nzbget_mock.NzbgetMock().history(nzb_id=7, name=JOB, status="DELETED/MANUAL")
    status = await _dc(mock).get_status(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    # Otherwise the orchestrator polls a job that no longer exists to the 6-hour deadline.
    assert status.status == "failed"


@pytest.mark.asyncio
async def test_unknown_job_reads_as_queued_with_no_matches():
    status = await _dc(nzbget_mock.NzbgetMock()).get_status(
        TaskHandle(source="usenet", nzo_id="7", job_name=JOB)
    )
    assert status.status == "queued"
    assert status.matched_transfers == 0


@pytest.mark.asyncio
async def test_job_is_found_by_dupe_key_before_the_nzbid_is_known():
    mock = nzbget_mock.NzbgetMock().group(
        nzb_id=7, name="renamed-by-nzbget", status="DOWNLOADING", dupe_key=JOB
    )
    # The crash-recovery window: the append response never reached us, so only the
    # pre-enqueue key is available.
    status = await _dc(mock).get_status(TaskHandle(source="usenet", job_name=JOB))
    assert status.status == "downloading"


@pytest.mark.asyncio
async def test_ambiguous_identity_raises():
    mock = nzbget_mock.NzbgetMock()
    mock.group(nzb_id=7, name=JOB, status="DOWNLOADING").group(
        nzb_id=8, name=JOB, status="QUEUED"
    )
    with pytest.raises(NzbgetApiError, match="ambiguous"):
        await _dc(mock).get_status(TaskHandle(source="usenet", job_name=JOB))


# --- cleanup --------------------------------------------------------------------


@pytest.mark.asyncio
async def test_abort_final_deletes_an_active_job():
    mock = nzbget_mock.NzbgetMock().group(nzb_id=7, name=JOB, status="DOWNLOADING")
    assert await _dc(mock).abort(TaskHandle(source="usenet", nzo_id="7", job_name=JOB)) is True
    assert mock.editqueue_calls[0][0] == "GroupFinalDelete"


@pytest.mark.asyncio
async def test_abort_refuses_to_touch_a_completed_job():
    mock = nzbget_mock.NzbgetMock().history(nzb_id=7, name=JOB, status="SUCCESS/ALL")
    # Completed output is not client-owned temporary data; cleanup must validate and
    # remove the workspace first.
    assert await _dc(mock).abort(TaskHandle(source="usenet", nzo_id="7", job_name=JOB)) is False
    assert mock.editqueue_calls == []


@pytest.mark.asyncio
async def test_discard_keeps_files_for_a_completed_job():
    mock = nzbget_mock.NzbgetMock().history(nzb_id=7, name=JOB, status="SUCCESS/ALL")
    await _dc(mock).discard_client_artifacts(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    # HistoryDelete keeps the files; HistoryFinalDelete would delete output we imported.
    assert mock.editqueue_calls[0][0] == "HistoryDelete"


@pytest.mark.asyncio
async def test_discard_removes_files_for_a_failed_job():
    mock = nzbget_mock.NzbgetMock().history(nzb_id=7, name=JOB, status="FAILURE/UNPACK")
    await _dc(mock).discard_client_artifacts(TaskHandle(source="usenet", nzo_id="7", job_name=JOB))
    assert mock.editqueue_calls[0][0] == "HistoryFinalDelete"


@pytest.mark.asyncio
async def test_discard_waits_while_the_job_is_still_queued():
    mock = nzbget_mock.NzbgetMock().group(nzb_id=7, name=JOB, status="DOWNLOADING")
    assert (
        await _dc(mock).discard_client_artifacts(
            TaskHandle(source="usenet", nzo_id="7", job_name=JOB)
        )
        is False
    )


# --- config and paths -----------------------------------------------------------


@pytest.mark.asyncio
async def test_dest_dir_expands_the_main_dir_reference():
    mock = nzbget_mock.NzbgetMock()
    mock.main_dir = "/downloads"
    mock.dest_dir = "${MainDir}/dst"
    # config returns values as written in nzbget.conf, so an unexpanded DestDir would
    # never match the absolute paths NZBGet reports for finished jobs.
    assert await _dc(mock).get_complete_dir() == "/downloads/dst"


@pytest.mark.asyncio
async def test_categories_come_from_the_numbered_config_options():
    mock = nzbget_mock.NzbgetMock()
    mock.categories = ["music", "droppedneedle"]
    assert await _dc(mock).get_categories() == ["music", "droppedneedle"]


@pytest.mark.asyncio
async def test_completed_files_are_enumerated_from_the_remapped_job_folder(tmp_path):
    job_dir = tmp_path / "Artist - Album"
    job_dir.mkdir()
    (job_dir / "01.flac").write_bytes(b"x")
    (job_dir / "cover.jpg").write_bytes(b"x")
    mock = nzbget_mock.NzbgetMock()
    mock.main_dir = "/downloads"
    mock.dest_dir = "${MainDir}/dst"
    mock.history(nzb_id=7, name=JOB, status="SUCCESS/ALL", dest_dir="/downloads/dst/Artist - Album")
    files = await _dc(mock, mount=str(tmp_path)).list_completed_files(
        TaskHandle(source="usenet", nzo_id="7", job_name=JOB)
    )
    assert [f.name for f in files] == ["01.flac"]


@pytest.mark.asyncio
async def test_final_dir_wins_over_dest_dir(tmp_path):
    moved = tmp_path / "moved"
    moved.mkdir()
    (moved / "01.flac").write_bytes(b"x")
    mock = nzbget_mock.NzbgetMock()
    mock.main_dir = "/downloads"
    mock.dest_dir = "${MainDir}/dst"
    # A post-processing script that moves the output sets FinalDir; DestDir then points
    # at a folder that no longer holds the files.
    mock.history(
        nzb_id=7,
        name=JOB,
        status="SUCCESS/ALL",
        dest_dir="/downloads/dst/original",
        final_dir="/downloads/dst/moved",
    )
    files = await _dc(mock, mount=str(tmp_path)).list_completed_files(
        TaskHandle(source="usenet", nzo_id="7", job_name=JOB)
    )
    assert [f.name for f in files] == ["01.flac"]


@pytest.mark.asyncio
async def test_health_check_reports_the_version():
    status = await _dc(nzbget_mock.NzbgetMock()).health_check()
    assert status.status == "ok"
    assert status.version == "24.3"


@pytest.mark.asyncio
async def test_health_check_never_raises():
    def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("refused")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = NzbgetClient(http, "http://nzbget:6789", "nzbget", "secret", max_attempts=1)
    dc = NzbgetDownloadClient(
        client, "http://nzbget:6789", "nzbget", "secret", Path("/nzbget-downloads")
    )
    assert (await dc.health_check()).status == "error"
