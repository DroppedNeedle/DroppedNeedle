"""WantedWatcherService tests (Wanted plan §6 Phase 1).

Covers: the enrolment classifier tied to the ORCHESTRATOR'S imported message
constants (never copied strings), re-enrolment rules, the quarantine-untouched
guarantee, seen-candidate dedup (one SSE, then seen_only), auto-tier dispatch +
request relink writes, capped per-track partial dispatch with per-recording
dedup, satisfaction-first (no search on a covered want), the active-work
guards, cadence math with jitter bounds, and dormancy."""

import threading
import time
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

from api.v1.schemas.settings import WantedWatcherSettings
from core.exceptions import ResourceNotFoundError
from infrastructure.persistence.request_history import RequestHistoryRecord
from infrastructure.persistence.wanted_store import WantedStore
from models.download import ScoredCandidate
from services.native.download_orchestrator import (
    _FILES_NOT_FOUND_MSG,
    _IMPORT_FAILED_MSG,
    _NO_MATCH_MSG,
    _NO_SOURCE_MSG,
    DownloadOrchestrator,
)
from services.native.download_service import ALREADY_IN_LIBRARY, DownloadService
from services.native.wanted_watcher_service import (
    WantedWatcherService,
    _interval_days,
)

_DAY = 86400.0


def _record(
    mbid: str = "rg-1",
    status: str = "failed",
    user_id: str | None = "user-a",
    task_id: str | None = "task-1",
    requested_at: str | None = None,
    year: int | None = 2026,
) -> RequestHistoryRecord:
    return RequestHistoryRecord(
        musicbrainz_id=mbid,
        artist_name="Yan Qing",
        album_title="the arrival",
        requested_at=requested_at or datetime.now(timezone.utc).isoformat(),
        status=status,
        user_id=user_id,
        download_task_id=task_id,
        year=year,
    )


def _task(
    status: str = "failed",
    error: str | None = None,
    task_id: str = "task-1",
    retry_count: int = 0,
):
    return SimpleNamespace(
        id=task_id, status=status, error_message=error, retry_count=retry_count
    )


def _cand(
    tier: str = "manual",
    username: str = "peer",
    directory: str = "dir-a",
    source: str = "soulseek",
) -> ScoredCandidate:
    return ScoredCandidate(
        source=source,
        username=username,
        parent_directory=directory,
        tier=tier,
        final_score=0.9,
    )


def _track(rec: str | None, title: str, position: int, length: int = 200_000):
    return SimpleNamespace(
        recording_id=rec, title=title, position=position, disc_number=1, length=length
    )


def _row(rec: str, row_id: int = 1) -> dict:
    return {
        "id": row_id,
        "recording_mbid": rec,
        "disc_number": 1,
        "track_number": row_id,
        "track_title": f"t{row_id}",
        "duration_seconds": 200.0,
    }


class _Env(SimpleNamespace):
    pass


@pytest.fixture
def env(tmp_path) -> _Env:
    store = WantedStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())
    requests = AsyncMock()
    requests.async_get_history.return_value = ([], 0)
    requests.async_get_record.return_value = None
    download_store = AsyncMock()
    download_store.get_active_task_for_album_any_user.return_value = None
    download_store.has_unresolved_held_for_task.return_value = False
    download_store.get_task.return_value = None
    ds = AsyncMock()
    ds.next_retry_at = Mock(return_value=None)  # sync method on the real service
    ds.scout_album.return_value = []
    ds.request_album.return_value = "task-new"
    ds.request_track.return_value = "task-track"
    library = AsyncMock()
    library.get_file_rows_for_album.return_value = []
    library.get_library_mbids.return_value = set()
    album_service = AsyncMock()
    album_service.get_album_tracks_info.side_effect = ResourceNotFoundError("MB down")
    mb = AsyncMock()
    first_release_date = (datetime.now(timezone.utc) - timedelta(days=13)).strftime(
        "%Y-%m-%d"
    )
    mb.get_release_group_by_id.return_value = {"first-release-date": first_release_date}
    sse = AsyncMock()
    prefs = Mock()
    prefs.get_wanted_settings.return_value = WantedWatcherSettings()
    watcher = WantedWatcherService(
        wanted_store=store,
        request_history=requests,
        download_store=download_store,
        get_download_service=lambda: ds,
        library_manager=library,
        album_service=album_service,
        mb_repo=mb,
        sse_publisher=sse,
        preferences=prefs,
        inter_want_delay=0.0,
    )
    return _Env(
        watcher=watcher,
        store=store,
        requests=requests,
        download_store=download_store,
        ds=ds,
        library=library,
        album_service=album_service,
        mb=mb,
        sse=sse,
        prefs=prefs,
        settings=prefs.get_wanted_settings.return_value,
        first_release_date=first_release_date,
    )


def _serve_history(env: _Env, failed=(), incomplete=()):
    async def history(page=1, page_size=200, status_filter=None):
        records = {"failed": list(failed), "incomplete": list(incomplete)}.get(
            status_filter, []
        )
        return records, len(records)

    env.requests.async_get_history.side_effect = history


async def _add_watch(env: _Env, mbid="rg-1", kind="missing", due=True, **overrides):
    fields = {
        "release_group_mbid": mbid,
        "user_id": "user-a",
        "artist_name": "Yan Qing",
        "album_title": "the arrival",
        "kind": kind,
        "next_check_at": time.time() - 1 if due else time.time() + 3600,
        "artist_mbid": "am-1",
        "year": 2026,
    }
    fields.update(overrides)
    await env.store.create_watch(**fields)
    return await env.store.get_watch(mbid)


@pytest.mark.asyncio
async def test_library_removal_opt_out_rearms_a_fulfilled_watch(env):
    await _add_watch(env)
    await env.store.mark_fulfilled("rg-1", "imported")

    changed = await env.watcher.continue_after_library_removal("rg-1")

    watch = await env.store.get_watch("rg-1")
    assert changed is True
    assert watch is not None
    assert watch.state == "watching"
    assert watch.next_check_at <= time.time()


@pytest.mark.asyncio
async def test_library_removal_opt_out_does_not_revive_a_stopped_watch(env):
    await _add_watch(env)
    await env.store.stop_watch("rg-1")

    changed = await env.watcher.continue_after_library_removal("rg-1")

    watch = await env.store.get_watch("rg-1")
    assert changed is False
    assert watch is not None
    assert watch.state == "stopped"


# --- enrolment classifier, tied to the orchestrator's constants (§4.5) ---


def test_orchestrator_messages_start_with_the_imported_constants(tmp_path):
    """The tie-test: the classifier prefix-matches _NO_SOURCE_MSG/_NO_MATCH_MSG,
    so the strings the orchestrator ACTUALLY writes must start with them."""
    orch = DownloadOrchestrator(
        client=Mock(),
        indexer=Mock(),
        download_store=Mock(),
        file_processor=Mock(),
        library_manager=Mock(),
        scorer=Mock(),
        track_matcher=Mock(),
        manifest_codec=Mock(),
        event_bus=Mock(),
        staging_path=tmp_path,
        naming_template="{artist}/{album}",
    )
    assert orch._no_source_message().startswith(_NO_SOURCE_MSG)
    assert orch._no_match_message().startswith(_NO_MATCH_MSG)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "message",
    [
        f"{_NO_SOURCE_MSG} on Soulseek",
        f"{_NO_SOURCE_MSG} on Soulseek or Usenet",
        f"{_NO_MATCH_MSG} on Usenet",
    ],
)
async def test_availability_failures_enrol_as_missing(env, message):
    _serve_history(env, failed=[_record()])
    env.download_store.get_task.return_value = _task(error=message)
    summary = await env.watcher.run_sweep()
    assert summary.enrolled == 1
    watch = await env.store.get_watch("rg-1")
    assert watch is not None
    assert watch.kind == "missing"
    assert watch.user_id == "user-a"
    assert watch.first_release_date == env.first_release_date
    # first check lands one age-curve interval out (13-day-old release -> 2 d ± 20 %)
    delta = watch.next_check_at - time.time()
    assert 0.8 * 2 * _DAY * 0.95 <= delta <= 1.2 * 2 * _DAY


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "message", [_FILES_NOT_FOUND_MSG, _IMPORT_FAILED_MSG, "download failed", None]
)
async def test_local_faults_do_not_enrol(env, message):
    _serve_history(env, failed=[_record()])
    env.download_store.get_task.return_value = _task(error=message)
    summary = await env.watcher.run_sweep()
    assert summary.enrolled == 0
    assert await env.store.get_watch("rg-1") is None


@pytest.mark.asyncio
async def test_incomplete_enrols_as_partial_without_message_check(env):
    _serve_history(env, incomplete=[_record(status="incomplete")])
    env.download_store.get_task.return_value = _task(status="partial", error=None)
    summary = await env.watcher.run_sweep()
    assert summary.enrolled == 1
    assert (await env.store.get_watch("rg-1")).kind == "partial"


@pytest.mark.asyncio
async def test_failed_request_with_no_linked_task_does_not_enrol(env):
    _serve_history(env, failed=[_record(task_id=None)])
    summary = await env.watcher.run_sweep()
    assert summary.enrolled == 0
    env.download_store.get_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_task_awaiting_auto_retry_does_not_enrol(env):
    _serve_history(env, failed=[_record()])
    env.download_store.get_task.return_value = _task(
        error=f"{_NO_SOURCE_MSG} on Soulseek"
    )
    env.ds.next_retry_at = Mock(return_value=time.time() + 900)
    assert (await env.watcher.run_sweep()).enrolled == 0


@pytest.mark.asyncio
async def test_pending_requests_are_never_queried_so_parked_tasks_cannot_enrol(env):
    """§4.10: a parked-for-review task leaves its request 'pending' - the watcher
    only ever asks for failed/incomplete rows, so it can't see it at all."""
    _serve_history(env)  # nothing in failed/incomplete
    await env.watcher.run_sweep()
    filters = {
        call.kwargs.get("status_filter")
        for call in env.requests.async_get_history.await_args_list
    }
    assert filters <= {"failed", "incomplete"}
    assert await env.store.list_watches(None) == []


@pytest.mark.asyncio
async def test_requesterless_records_do_not_enrol(env):
    _serve_history(env, failed=[_record(user_id=None)])
    env.download_store.get_task.return_value = _task(error=_NO_MATCH_MSG)
    assert (await env.watcher.run_sweep()).enrolled == 0


@pytest.mark.asyncio
async def test_release_date_falls_back_to_request_year(env):
    _serve_history(env, failed=[_record()])
    env.download_store.get_task.return_value = _task(error=_NO_MATCH_MSG)
    env.mb.get_release_group_by_id.return_value = None  # degraded fetch
    await env.watcher.run_sweep()
    assert (await env.store.get_watch("rg-1")).first_release_date == "2026"


# --- re-enrolment rules (§5.2.1) ---


@pytest.mark.asyncio
async def test_stopped_and_dormant_watches_never_auto_revive(env):
    await _add_watch(env, "rg-stop", due=False)
    await env.store.stop_watch("rg-stop")
    await _add_watch(env, "rg-dorm", due=False)
    await env.store.record_cycle(
        "rg-dorm",
        outcome="no_results",
        next_check_at=time.time(),
        quiet=True,
        go_dormant=True,
    )
    _serve_history(env, failed=[_record(mbid="rg-stop"), _record(mbid="rg-dorm")])
    env.download_store.get_task.return_value = _task(error=_NO_MATCH_MSG)
    assert (await env.watcher.run_sweep()).enrolled == 0
    assert (await env.store.get_watch("rg-stop")).state == "stopped"
    assert (await env.store.get_watch("rg-dorm")).state == "dormant"


@pytest.mark.asyncio
async def test_fulfilled_watch_rearms_only_on_a_newer_failed_request(env):
    await _add_watch(env, due=False)
    fulfilled_at = time.time()
    await env.store.mark_fulfilled("rg-1", "satisfied", now=fulfilled_at)

    # an OLDER request (pre-fulfilment) does not re-arm
    old_iso = datetime.fromtimestamp(fulfilled_at - 3600, tz=timezone.utc).isoformat()
    _serve_history(env, failed=[_record(requested_at=old_iso)])
    env.download_store.get_task.return_value = _task(error=_NO_MATCH_MSG)
    assert (await env.watcher.run_sweep()).enrolled == 0
    assert (await env.store.get_watch("rg-1")).state == "fulfilled"

    # a NEWER request that failed again re-arms with fresh counters
    new_iso = datetime.fromtimestamp(fulfilled_at + 3600, tz=timezone.utc).isoformat()
    _serve_history(env, failed=[_record(requested_at=new_iso)])
    assert (await env.watcher.run_sweep()).enrolled == 1
    watch = await env.store.get_watch("rg-1")
    assert watch.state == "watching"
    assert watch.check_count == 0


# --- quarantine untouched (D5) ---


@pytest.mark.asyncio
async def test_watcher_cycle_never_touches_the_quarantine(env):
    """A full auto-dispatching cycle must never reach delete_quarantine_for_album."""
    await _add_watch(env)
    env.ds.scout_album.return_value = [_cand(tier="auto")]
    await env.watcher.run_sweep()
    env.ds.request_album.assert_awaited_once()
    env.download_store.delete_quarantine_for_album.assert_not_called()


@pytest.mark.asyncio
async def test_request_album_with_wanted_origin_skips_the_blocklist_clear():
    """Service-level (real DownloadService, mock store): origin='wanted' skips the
    clear; a manual origin still clears (§5.5 seam)."""
    store = AsyncMock()
    store.get_active_task_for_album.return_value = None
    store.delete_quarantine_for_album.return_value = 2
    store.create_task.return_value = SimpleNamespace(id="t-1")
    library = AsyncMock()
    library.album_quality_tier.return_value = None
    service = DownloadService(
        download_client=Mock(),
        indexer=AsyncMock(),
        scorer=AsyncMock(),
        library_manager=library,
        download_store=store,
        event_bus=AsyncMock(),
        orchestrator=Mock(),
    )
    await service.request_album(
        user_id="u",
        release_group_mbid="rg-1",
        artist_name="A",
        album_title="B",
        origin="wanted",
    )
    store.delete_quarantine_for_album.assert_not_awaited()

    await service.request_album(
        user_id="u",
        release_group_mbid="rg-1",
        artist_name="A",
        album_title="B",
        origin="user",
    )
    store.delete_quarantine_for_album.assert_awaited_once_with("rg-1")


# --- seen-candidate dedup (D2) ---


@pytest.mark.asyncio
async def test_same_manual_candidates_badge_once_then_seen_only(env):
    await _add_watch(env)
    env.ds.scout_album.return_value = [
        _cand(username="peer-a", directory="d1"),
        _cand(username="peer-b", directory="d2"),
    ]
    await env.watcher.run_sweep()
    watch = await env.store.get_watch("rg-1")
    assert watch.last_outcome == "new_manual"
    assert watch.new_candidate_count == 2
    badge_events = [
        c
        for c in env.sse.publish.await_args_list
        if c.args[1] == "wanted_new_candidates"
    ]
    assert len(badge_events) == 1

    # cycle 2: identical results -> seen_only, no second SSE, badge kept
    await env.store.reschedule("rg-1", time.time() - 1)
    await env.watcher.run_sweep()
    watch = await env.store.get_watch("rg-1")
    assert watch.last_outcome == "seen_only"
    assert watch.new_candidate_count == 2
    badge_events = [
        c
        for c in env.sse.publish.await_args_list
        if c.args[1] == "wanted_new_candidates"
    ]
    assert len(badge_events) == 1


@pytest.mark.asyncio
async def test_rejected_tier_results_count_as_no_results(env):
    await _add_watch(env)
    env.ds.scout_album.return_value = [_cand(tier="rejected")]
    await env.watcher.run_sweep()
    watch = await env.store.get_watch("rg-1")
    assert watch.last_outcome == "no_results"
    assert watch.quiet_streak == 1
    env.sse.publish.assert_not_awaited()


# --- auto-tier dispatch (D2/D8, §4.8) ---


@pytest.mark.asyncio
async def test_auto_tier_dispatches_album_and_relinks_the_request(env):
    await _add_watch(env)
    env.ds.scout_album.return_value = [
        _cand(tier="auto"),
        _cand(username="p2", directory="d2"),
    ]
    summary = await env.watcher.run_sweep()
    assert summary.dispatched == 1

    kwargs = env.ds.request_album.await_args.kwargs
    assert kwargs["origin"] == "wanted"
    assert kwargs["user_id"] == "user-a"
    assert kwargs["release_group_mbid"] == "rg-1"
    # §4.8: the two request writes, or the found download never flips the request
    env.requests.async_update_status.assert_any_await("rg-1", "pending")
    env.requests.async_update_download_task_id.assert_awaited_once_with(
        "rg-1", "task-new"
    )

    watch = await env.store.get_watch("rg-1")
    assert watch.last_outcome == "auto_dispatched"
    # ALL returned identities recorded as seen
    assert len(await env.store.seen_identities("rg-1")) == 2
    # reschedule is short (~1 day) so the next cycle observes the outcome
    assert watch.next_check_at - time.time() <= 1.2 * _DAY


@pytest.mark.asyncio
async def test_auto_download_toggle_off_badges_instead_of_dispatching(env):
    env.prefs.get_wanted_settings.return_value = WantedWatcherSettings(
        auto_download_on_find=False
    )
    await _add_watch(env)
    env.ds.scout_album.return_value = [_cand(tier="auto")]
    await env.watcher.run_sweep()
    env.ds.request_album.assert_not_awaited()
    assert (await env.store.get_watch("rg-1")).last_outcome == "new_manual"


@pytest.mark.asyncio
async def test_already_in_library_dispatch_fulfils_the_watch(env):
    await _add_watch(env)
    env.ds.scout_album.return_value = [_cand(tier="auto")]
    env.ds.request_album.return_value = ALREADY_IN_LIBRARY
    summary = await env.watcher.run_sweep()
    assert summary.fulfilled == 1
    assert (await env.store.get_watch("rg-1")).state == "fulfilled"
    env.requests.async_update_download_task_id.assert_not_awaited()


# --- partial wants (D6/D9) ---


@pytest.mark.asyncio
async def test_partial_want_dispatches_missing_tracks_capped_with_logged_drop(
    env, caplog
):
    await _add_watch(env, kind="partial")
    tracks = [_track(f"rec-{i}", f"Track {i}", i) for i in range(1, 9)]  # 8 expected
    env.album_service.get_album_tracks_info.side_effect = None
    env.album_service.get_album_tracks_info.return_value = SimpleNamespace(
        tracks=tracks
    )
    env.library.get_file_rows_for_album.return_value = [_row("rec-1")]  # 1 of 8 held
    env.ds.scout_album.return_value = [_cand(tier="auto")]

    with caplog.at_level("INFO"):
        await env.watcher.run_sweep()

    # 7 uncovered, cap 5 - and the covered track is never re-requested
    assert env.ds.request_track.await_count == 5
    requested = {
        c.kwargs["recording_mbid"] for c in env.ds.request_track.await_args_list
    }
    assert "rec-1" not in requested
    for call in env.ds.request_track.await_args_list:
        assert call.kwargs["origin"] == "wanted"
    assert any(r.message == "wanted.track_dispatch_capped" for r in caplog.records)
    # per-track dispatches must NOT hijack the album request's task link (§4.8)
    env.requests.async_update_download_task_id.assert_not_awaited()


@pytest.mark.asyncio
async def test_per_track_dispatch_dedups_on_the_recording():
    """Dispatch the same recording twice -> one task (request_track's per-recording
    dedup, the partial-want duplicate protection per §4.9)."""
    store = AsyncMock()
    store.get_active_task_for_track.side_effect = [None, SimpleNamespace(id="t-1")]
    store.create_task.return_value = SimpleNamespace(id="t-1")
    library = AsyncMock()
    library.has_track.return_value = False
    service = DownloadService(
        download_client=Mock(),
        indexer=AsyncMock(),
        scorer=AsyncMock(),
        library_manager=library,
        download_store=store,
        event_bus=AsyncMock(),
        orchestrator=Mock(),
    )
    first = await service.request_track(
        user_id="u",
        recording_mbid="rec-1",
        artist_name="A",
        track_title="T",
        release_group_mbid="rg-1",
        origin="wanted",
    )
    second = await service.request_track(
        user_id="u",
        recording_mbid="rec-1",
        artist_name="A",
        track_title="T",
        release_group_mbid="rg-1",
        origin="wanted",
    )
    assert first == second == "t-1"
    store.create_task.assert_awaited_once()


@pytest.mark.asyncio
async def test_partial_want_without_tracklist_fails_open(env):
    await _add_watch(env, kind="partial")  # album_service raises by default
    await env.watcher.run_sweep()
    env.ds.scout_album.assert_not_awaited()
    watch = await env.store.get_watch("rg-1")
    assert watch.check_count == 0  # not a recorded cycle, just pushed out
    assert watch.next_check_at > time.time()


# --- satisfaction-first (D6 edge case) ---


@pytest.mark.asyncio
async def test_covered_want_fulfils_without_searching(env):
    await _add_watch(env)
    tracks = [_track("rec-1", "One", 1), _track("rec-2", "Two", 2)]
    env.album_service.get_album_tracks_info.side_effect = None
    env.album_service.get_album_tracks_info.return_value = SimpleNamespace(
        tracks=tracks
    )
    env.library.get_file_rows_for_album.return_value = [
        _row("rec-1", 1),
        _row("rec-2", 2),
    ]

    summary = await env.watcher.run_sweep()

    assert summary.fulfilled == 1
    env.ds.scout_album.assert_not_awaited()  # the whole point: no search
    assert (await env.store.get_watch("rg-1")).state == "fulfilled"
    status_call = env.requests.async_update_status.await_args
    assert status_call.args[0] == "rg-1"
    assert status_call.args[1] == "imported"
    assert status_call.kwargs["completed_at"] is not None


@pytest.mark.asyncio
async def test_missing_want_without_tracklist_uses_library_presence(env):
    await _add_watch(env)  # no tracklist by default
    env.library.get_library_mbids.return_value = {"rg-1"}
    summary = await env.watcher.run_sweep()
    assert summary.fulfilled == 1
    env.ds.scout_album.assert_not_awaited()


# --- active-work guards (§4.9) ---


@pytest.mark.asyncio
async def test_active_album_task_guard_skips_the_scout(env):
    await _add_watch(env)
    env.download_store.get_active_task_for_album_any_user.return_value = _task(
        status="downloading"
    )
    await env.watcher.run_sweep()
    env.ds.scout_album.assert_not_awaited()
    watch = await env.store.get_watch("rg-1")
    assert watch.last_outcome is None  # untouched
    assert watch.next_check_at > time.time()


@pytest.mark.asyncio
async def test_pending_auto_retry_guard_skips_the_scout(env):
    await _add_watch(env)
    env.requests.async_get_record.return_value = _record()
    env.download_store.get_task.return_value = _task()
    env.ds.next_retry_at = Mock(return_value=time.time() + 600)
    await env.watcher.run_sweep()
    env.ds.scout_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_unresolved_held_import_guard_skips_the_scout(env):
    await _add_watch(env)
    env.requests.async_get_record.return_value = _record()
    env.download_store.get_task.return_value = _task()
    env.download_store.has_unresolved_held_for_task.return_value = True
    await env.watcher.run_sweep()
    env.ds.scout_album.assert_not_awaited()


# --- cadence + dormancy (D3) ---


def _date_days_ago(days: int) -> str:
    return (datetime.now(timezone.utc) - timedelta(days=days)).strftime("%Y-%m-%d")


def test_interval_days_age_curve():
    now = time.time()
    assert _interval_days(_date_days_ago(10), 0, now) == 2.0
    assert _interval_days(_date_days_ago(60), 0, now) == 4.0
    assert _interval_days(_date_days_ago(200), 0, now) == 7.0
    assert _interval_days(_date_days_ago(400), 0, now) == 14.0
    assert _interval_days(_date_days_ago(400), 10, now) == 28.0  # quiet doubling
    assert _interval_days(None, 0, now) == 14.0  # unknown = old
    # a year-only fallback date resolves to Jan 1 of that year (mid-2026 -> 90d-1y bucket)
    assert _interval_days("2026", 0, now) == 7.0
    assert _interval_days("garbage", 0, now) == 14.0


def test_interval_seconds_jitter_bounds(env):
    now = time.time()
    for _ in range(100):
        seconds = env.watcher._interval_seconds(None, 0, now)
        assert 0.8 * 14 * _DAY <= seconds <= 1.2 * 14 * _DAY


@pytest.mark.asyncio
async def test_want_goes_dormant_after_the_watch_window(env):
    old = time.time() - 400 * _DAY
    await _add_watch(env, created_at=old)
    env.ds.scout_album.return_value = []
    await env.watcher.run_sweep()
    watch = await env.store.get_watch("rg-1")
    assert watch.state == "dormant"
    assert watch.last_outcome == "no_results"


# --- "None of these - keep watching" (owner decision 2026-07-06) ---


def _parked_task(mbid: str = "rg-1", user_id: str = "user-a"):
    return SimpleNamespace(
        id="task-parked",
        user_id=user_id,
        status="queued",
        release_group_mbid=mbid,
        download_type="album",
        artist_name="Yan Qing",
        album_title="the arrival",
        artist_mbid="am-1",
        year=2026,
        retry_count=1,
        error_message=None,
    )


@pytest.mark.asyncio
async def test_dismiss_review_cancels_watches_and_records_seen(env):
    env.download_store.get_parked_task_for_search_job.return_value = _parked_task()
    env.download_store.get_search_job_candidates.return_value = [
        _cand(username="peer-a", directory="d1"),
        _cand(username="peer-b", directory="d2"),
    ]

    watch = await env.watcher.dismiss_review("job-1", "user-a", "user")

    env.ds.cancel_task.assert_awaited_once_with("task-parked", "user-a", "user")
    assert watch.state == "watching"
    assert watch.kind == "missing"
    assert watch.user_id == "user-a"
    assert watch.first_release_date == env.first_release_date
    # every rejected candidate is seen: the watcher never badges those copies again
    assert len(await env.store.seen_identities("rg-1")) == 2


@pytest.mark.asyncio
async def test_dismiss_review_rejected_candidates_never_badge_again(env):
    env.download_store.get_parked_task_for_search_job.return_value = _parked_task()
    rejected = [_cand(username="peer-a", directory="d1")]
    env.download_store.get_search_job_candidates.return_value = rejected
    await env.watcher.dismiss_review("job-1", "user-a", "user")

    # next watcher cycle finds the SAME manual-tier candidate -> seen_only, no SSE
    await env.store.reschedule("rg-1", time.time() - 1)
    env.ds.scout_album.return_value = rejected
    await env.watcher.run_sweep()
    watch = await env.store.get_watch("rg-1")
    assert watch.last_outcome == "seen_only"
    badge_events = [
        c
        for c in env.sse.publish.await_args_list
        if c.args[1] == "wanted_new_candidates"
    ]
    assert badge_events == []


@pytest.mark.asyncio
async def test_dismiss_review_404_when_nothing_parked(env):
    from core.exceptions import ResourceNotFoundError

    env.download_store.get_parked_task_for_search_job.return_value = None
    with pytest.raises(ResourceNotFoundError):
        await env.watcher.dismiss_review("job-1", "user-a", "user")


@pytest.mark.asyncio
async def test_dismiss_review_403_for_non_owner(env):
    from core.exceptions import PermissionDeniedError

    env.download_store.get_parked_task_for_search_job.return_value = _parked_task()
    with pytest.raises(PermissionDeniedError):
        await env.watcher.dismiss_review("job-1", "someone-else", "user")
    env.ds.cancel_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_dismiss_review_revives_a_stopped_watch(env):
    await _add_watch(env, due=False)
    await env.store.stop_watch("rg-1")
    env.download_store.get_parked_task_for_search_job.return_value = _parked_task()
    env.download_store.get_search_job_candidates.return_value = []

    watch = await env.watcher.dismiss_review("job-1", "user-a", "user")

    assert watch.state == "watching"


@pytest.mark.asyncio
async def test_dismiss_review_track_task_watches_as_partial(env):
    task = _parked_task()
    task.download_type = "track"
    env.download_store.get_parked_task_for_search_job.return_value = task
    env.download_store.get_search_job_candidates.return_value = []

    watch = await env.watcher.dismiss_review("job-1", "user-a", "user")

    assert watch.kind == "partial"


# --- read-only retrying rows (owner decision 2026-07-06) ---


@pytest.mark.asyncio
async def test_list_retrying_returns_requests_with_a_pending_retry(env):
    _serve_history(env, failed=[_record()])
    env.download_store.get_task.return_value = _task()
    env.ds.auto_retry_max = 6
    due = time.time() + 900
    env.ds.next_retry_at = Mock(return_value=due)

    items = await env.watcher.list_retrying_for("user-a", "user")

    assert len(items) == 1
    entry = items[0]
    assert entry.release_group_mbid == "rg-1"
    assert entry.retry_count == 0
    assert entry.max_attempts == 6
    assert entry.next_retry_at == due


@pytest.mark.asyncio
async def test_list_retrying_excludes_exhausted_and_taskless_requests(env):
    _serve_history(
        env,
        failed=[
            _record(mbid="rg-exhausted"),
            _record(mbid="rg-taskless", task_id=None),
        ],
    )
    env.download_store.get_task.return_value = _task()
    env.ds.next_retry_at = Mock(return_value=None)  # ladder exhausted

    assert await env.watcher.list_retrying_for("user-a", "user") == []


@pytest.mark.asyncio
async def test_list_retrying_scopes_to_the_caller_unless_admin(env):
    _serve_history(
        env,
        failed=[_record(mbid="rg-mine"), _record(mbid="rg-theirs", user_id="user-b")],
    )
    env.download_store.get_task.return_value = _task()
    env.ds.next_retry_at = Mock(return_value=time.time() + 60)

    mine = await env.watcher.list_retrying_for("user-a", "user")
    assert [i.release_group_mbid for i in mine] == ["rg-mine"]
    everyone = await env.watcher.list_retrying_for("admin-id", "admin")
    assert {i.release_group_mbid for i in everyone} == {"rg-mine", "rg-theirs"}


# --- toggles + error isolation ---


@pytest.mark.asyncio
async def test_disabled_toggle_short_circuits_the_sweep(env):
    env.prefs.get_wanted_settings.return_value = WantedWatcherSettings(enabled=False)
    await _add_watch(env)
    summary = await env.watcher.run_sweep()
    assert summary == type(summary)()
    env.requests.async_get_history.assert_not_awaited()
    env.ds.scout_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_partial_toggle_off_skips_enrolment_and_checks(env):
    env.prefs.get_wanted_settings.return_value = WantedWatcherSettings(
        watch_partial_albums=False
    )
    await _add_watch(env, kind="partial")
    _serve_history(env, incomplete=[_record(mbid="rg-2", status="incomplete")])
    env.download_store.get_task.return_value = _task(status="partial")
    summary = await env.watcher.run_sweep()
    assert summary.enrolled == 0
    env.ds.scout_album.assert_not_awaited()


@pytest.mark.asyncio
async def test_one_bad_want_never_kills_the_sweep(env):
    await _add_watch(env, "rg-bad", next_check_at=time.time() - 100)
    await _add_watch(env, "rg-good", next_check_at=time.time() - 10)
    calls = []

    async def scout(**kwargs):
        calls.append(kwargs["release_group_mbid"])
        if kwargs["release_group_mbid"] == "rg-bad":
            raise RuntimeError("boom")
        return []

    env.ds.scout_album.side_effect = scout
    summary = await env.watcher.run_sweep()
    assert calls == ["rg-bad", "rg-good"]  # the good one still ran
    assert summary.errors == 1
    assert summary.checked == 1
    bad = await env.store.get_watch("rg-bad")
    assert bad.last_outcome == "error"
    assert bad.next_check_at > time.time()  # rescheduled, not stuck due


@pytest.mark.asyncio
async def test_scout_configuration_error_reschedules_quietly(env):
    from core.exceptions import ConfigurationError

    await _add_watch(env)
    env.ds.scout_album.side_effect = ConfigurationError("downloads disabled")
    summary = await env.watcher.run_sweep()
    assert summary.errors == 0
    watch = await env.store.get_watch("rg-1")
    assert watch.last_outcome is None
    assert watch.next_check_at > time.time()
