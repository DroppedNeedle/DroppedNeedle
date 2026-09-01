"""NewReleaseService: baseline detection, fan-out enqueue, and graceful
degradation (Phase 4)."""

import asyncio
import sqlite3
import threading
from datetime import date as utc_date, datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.settings import UserPreferences
from core.exceptions import ConfigurationError, ExternalServiceError
from infrastructure.persistence.follow_store import FollowStore
from infrastructure.queue.priority_queue import RequestPriority
from services.native.download_service import ALREADY_IN_LIBRARY
from services.native.new_release_service import NewReleaseService
from tests.helpers import make_builtin_dispatcher

ARTIST = "AAAAAAAA-1111-2222-3333-444444444444"
ARTIST_LOWER = ARTIST.lower()


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users "
            "(id TEXT PRIMARY KEY, display_name TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'user')"
        )
        conn.executemany(
            "INSERT OR IGNORE INTO auth_users (id, display_name, role) VALUES (?, ?, ?)",
            [
                ("user-a", "Alice", "user"),
                ("user-b", "Bob", "user"),
                ("admin-1", "Admin", "admin"),
            ],
        )
        conn.execute(
            "CREATE TABLE IF NOT EXISTS library_files "
            "(id TEXT PRIMARY KEY, release_group_mbid TEXT, deleted_at REAL)"
        )
        conn.commit()
    finally:
        conn.close()


def _rg(mbid: str, title: str, *, primary="Album", secondary=None, date=None):
    date = date or utc_date.today().isoformat()
    d = {
        "id": mbid,
        "title": title,
        "primary-type": primary,
        "first-release-date": date,
    }
    if secondary is not None:
        d["secondary-types"] = secondary
    return d


@pytest.fixture
def svc(tmp_path: Path):
    db = tmp_path / "library.db"
    store = FollowStore(db_path=db, write_lock=threading.Lock())
    _seed_auth_users(db)

    mb = AsyncMock()
    mb.get_artist_release_groups_or_raise = AsyncMock(return_value=([], 0))
    downloads = AsyncMock()
    downloads.request_album = AsyncMock(return_value="task-1")
    download_store = AsyncMock()
    download_store.get_active_task_for_album_any_user = AsyncMock(return_value=None)
    library = AsyncMock()
    library.get_library_mbids = AsyncMock(return_value=set())
    sse = AsyncMock()
    preferences = MagicMock()
    preferences.get_preferences_with_revision.return_value = (
        UserPreferences(),
        0,
    )

    service = NewReleaseService(
        follow_store=store,
        mb_repo=mb,
        acquisition=make_builtin_dispatcher(lambda: downloads),
        download_store=download_store,
        library_repo=library,
        sse_publisher=sse,
        inter_artist_delay=0.0,
        preferences_service=preferences,
        policy_transition_lock=asyncio.Lock(),
    )
    return SimpleNamespace(
        service=service,
        store=store,
        mb=mb,
        downloads=downloads,
        download_store=download_store,
        preferences=preferences,
        library=library,
        sse=sse,
        db=db,
    )


async def _follow_with_auto(store, user_id, *, state="approved"):
    await store.follow_artist(user_id, ARTIST, "Radiohead")
    await store.set_auto_download_intent(user_id, ARTIST, True)
    if state:
        await store.upsert_approval(user_id, ARTIST, "Radiohead", state)


@pytest.mark.asyncio
async def test_first_poll_seeds_baseline_and_enqueues_nothing(svc):
    await _follow_with_auto(svc.store, "user-a")
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old 1"), _rg("RG2", "Old 2")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.baselined == 1
    assert summary.new_releases == 0
    svc.downloads.request_album.assert_not_called()
    assert await svc.store.has_cursor(ARTIST_LOWER) is True
    assert await svc.store.known_release_set(ARTIST_LOWER) == {"rg1", "rg2"}
    items, total = await svc.store.list_new_releases_for_user("user-a", 50, 0)
    assert total == 0


@pytest.mark.asyncio
async def test_second_poll_detects_and_enqueues_for_approved(svc):
    dispatch = svc.service._acquisition.request_album
    svc.service._acquisition.request_album = AsyncMock(wraps=dispatch)
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "Brand New")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.new_releases == 1
    assert summary.enqueued == 1
    svc.downloads.request_album.assert_awaited_once()
    kwargs = svc.downloads.request_album.await_args.kwargs
    assert kwargs["user_id"] == "user-a"
    assert kwargs["release_group_mbid"] == "RG2"
    assert (
        svc.service._acquisition.request_album.await_args.kwargs["track_count_priority"]
        is RequestPriority.BACKGROUND_SYNC
    )
    svc.sse.publish.assert_awaited_once()
    items, total = await svc.store.list_new_releases_for_user("user-a", 50, 0)
    assert total == 1 and items[0].release_group_mbid == "RG2"


@pytest.mark.asyncio
async def test_owned_release_group_is_excluded(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.library.get_library_mbids.return_value = {"rg2"}  # already owned
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "Owned New")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.new_releases == 0
    svc.downloads.request_album.assert_not_called()


@pytest.mark.asyncio
async def test_future_dated_release_is_feed_only_until_due(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "Upcoming", date="2099-01-01")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.new_releases == 1
    assert summary.enqueued == 0
    svc.downloads.request_album.assert_not_called()
    _items, total = await svc.store.list_new_releases_for_user("user-a", 50, 0)
    assert total == 1
    assert "rg2" in await svc.store.known_release_set(ARTIST_LOWER)
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)


@pytest.mark.asyncio
async def test_noisy_secondary_type_is_filtered(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "Live Album", secondary=["Live"])],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.new_releases == 0
    svc.downloads.request_album.assert_not_called()


@pytest.mark.asyncio
async def test_pending_follower_gets_feed_but_no_enqueue(svc):
    await _follow_with_auto(svc.store, "user-a", state="pending")  # not yet approved
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.new_releases == 1
    assert summary.enqueued == 0
    svc.downloads.request_album.assert_not_called()
    _items, total = await svc.store.list_new_releases_for_user("user-a", 50, 0)
    assert total == 1  # still in Wanted
    assert await svc.store.pending_release_set(ARTIST_LOWER, 0) == set()


@pytest.mark.asyncio
async def test_two_followers_enqueue_once(svc):
    await _follow_with_auto(svc.store, "user-a")
    await _follow_with_auto(svc.store, "user-b")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.enqueued == 1
    svc.downloads.request_album.assert_awaited_once()  # DD5: one task across followers
    assert (
        svc.downloads.request_album.await_args.kwargs["user_id"] == "user-a"
    )  # deterministic


@pytest.mark.asyncio
async def test_active_task_any_user_blocks_enqueue(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.download_store.get_active_task_for_album_any_user.return_value = (
        object()
    )  # in flight
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.new_releases == 1
    assert summary.enqueued == 0
    svc.downloads.request_album.assert_not_called()


@pytest.mark.asyncio
async def test_already_in_library_sentinel_skips_sse(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.downloads.request_album.return_value = ALREADY_IN_LIBRARY
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.enqueued == 0
    svc.sse.publish.assert_not_called()


@pytest.mark.asyncio
async def test_config_error_does_not_crash(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.downloads.request_album.side_effect = ConfigurationError(
        "download client disabled"
    )
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )
    summary = await svc.service.run_poll()
    assert summary.new_releases == 1
    assert summary.enqueued == 0  # feed populated, but no task created


@pytest.mark.asyncio
async def test_mb_error_does_not_advance_baseline(svc):
    await _follow_with_auto(svc.store, "user-a")
    svc.mb.get_artist_release_groups_or_raise.side_effect = ExternalServiceError(
        "MB down"
    )
    summary = await svc.service.run_poll()
    assert summary.errors == 1
    assert summary.baselined == 0
    # no cursor created -> the next run still baselines (never treats back-catalog as new)
    assert await svc.store.has_cursor(ARTIST_LOWER) is False


@pytest.mark.asyncio
async def test_mb_error_after_baseline_preserves_known_set(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1", "rg2"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.side_effect = ExternalServiceError(
        "MB down"
    )
    summary = await svc.service.run_poll()
    assert summary.errors == 1
    assert await svc.store.known_release_set(ARTIST_LOWER) == {
        "rg1",
        "rg2",
    }  # unchanged


@pytest.mark.asyncio
async def test_release_type_preferences_include_soundtrack_and_demo(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.preferences.get_preferences_with_revision.return_value = (
        UserPreferences(
            primary_types=[" ALBUM ", "album"],
            secondary_types=["studio", " SOUNDTRACK ", "demo"],
        ),
        0,
    )
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [
            _rg("RG1", "Old"),
            _rg("RG2", "Soundtrack", secondary=["Soundtrack"]),
            _rg("RG3", "Demo", secondary=["Demo"]),
            _rg("RG4", "Compilation", secondary=["Compilation"]),
            _rg("RG5", "Other", primary="Other"),
        ],
        5,
    )

    summary = await svc.service.run_poll()

    assert summary.new_releases == 2
    assert summary.enqueued == 2
    assert await svc.store.known_release_set(ARTIST_LOWER) == {
        "rg1",
        "rg2",
        "rg3",
        "rg4",
        "rg5",
    }
    assert svc.downloads.request_album.await_count == 2


@pytest.mark.asyncio
async def test_historical_or_incomplete_dates_are_known_without_feed(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [
            _rg("RG1", "Old"),
            _rg("RG2", "Historical", date="2020-01-01"),
            _rg("RG3", "Year Only", date="2026"),
            _rg("RG4", "Month Only", date="2026-08"),
            _rg("RG5", "Malformed", date="2026-99-99"),
            _rg("RG6", "Current"),
        ],
        6,
    )

    summary = await svc.service.run_poll()

    assert summary.new_releases == 1
    assert summary.enqueued == 1
    assert await svc.store.known_release_set(ARTIST_LOWER) == {
        "rg1",
        "rg2",
        "rg3",
        "rg4",
        "rg5",
        "rg6",
    }
    items, total = await svc.store.list_new_releases_for_user("user-a", 50, 0)
    assert total == 1
    assert items[0].release_group_mbid.casefold() == "rg6"


@pytest.mark.asyncio
async def test_future_release_becomes_dispatchable_when_due(svc):
    today = utc_date.today()
    tomorrow = today + timedelta(days=1)
    svc.service._today_factory = lambda: today
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    release_groups = [
        _rg("RG1", "Old", date=today.isoformat()),
        _rg("RG2", "Tomorrow", date=tomorrow.isoformat()),
    ]
    svc.mb.get_artist_release_groups_or_raise.return_value = (release_groups, 2)

    first = await svc.service.run_poll()
    assert first.new_releases == 1
    assert first.enqueued == 0
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)

    svc.service._today_factory = lambda: tomorrow
    second = await svc.service.run_poll()

    assert second.new_releases == 0
    assert second.enqueued == 1
    assert await svc.store.pending_release_set(ARTIST_LOWER, 0) == set()


@pytest.mark.asyncio
async def test_failed_acquisition_retries_after_cursor_advances(svc):
    today = utc_date.today()
    tomorrow = today + timedelta(days=1)
    svc.service._today_factory = lambda: today

    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [
            _rg("RG1", "Old"),
            _rg("RG2", "Retry Me", date=today.isoformat()),
        ],
        2,
    )
    svc.downloads.request_album.side_effect = RuntimeError("queue unavailable")

    first = await svc.service.run_poll()
    assert first.new_releases == 1
    assert first.enqueued == 0
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)

    conn = sqlite3.connect(svc.db)
    try:
        conn.execute(
            "UPDATE artist_release_check SET last_checked_at = ? "
            "WHERE artist_mbid_lower = ?",
            (
                datetime.combine(
                    tomorrow,
                    datetime.min.time(),
                    tzinfo=timezone.utc,
                ).timestamp(),
                ARTIST_LOWER,
            ),
        )
        conn.commit()
    finally:
        conn.close()

    svc.service._today_factory = lambda: tomorrow
    svc.downloads.request_album.side_effect = None
    second = await svc.service.run_poll()

    assert second.new_releases == 0
    assert second.enqueued == 1
    assert await svc.store.pending_release_set(ARTIST_LOWER, 0) == set()


@pytest.mark.asyncio
async def test_policy_revision_change_rebaselines_without_backfill(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.preferences.get_preferences_with_revision.return_value = (
        UserPreferences(primary_types=["album"], secondary_types=["studio"]),
        1,
    )
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )

    summary = await svc.service.run_poll()

    assert summary.baselined == 1
    assert summary.new_releases == 0
    assert summary.enqueued == 0
    assert await svc.store.pending_release_set(ARTIST_LOWER, 1) == set()
    items, total = await svc.store.list_new_releases_for_user("user-a", 50, 0)
    assert total == 0


@pytest.mark.asyncio
async def test_auto_disabled_follow_does_not_retroactively_enqueue(svc):
    await svc.store.follow_artist("user-a", ARTIST, "Radiohead")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )

    first = await svc.service.run_poll()

    assert first.new_releases == 1
    assert first.enqueued == 0
    items, total = await svc.store.list_new_releases_for_user("user-a", 50, 0)
    assert total == 1
    assert items[0].release_group_mbid.casefold() == "rg2"
    assert await svc.store.known_release_set(ARTIST_LOWER) >= {"rg1", "rg2"}
    assert await svc.store.pending_release_set(ARTIST_LOWER, 0) == set()

    await svc.store.set_auto_download_intent("user-a", ARTIST, True)
    await svc.store.upsert_approval("user-a", ARTIST, "Radiohead", "approved")
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "Already discovered"), _rg("RG3", "New")],
        3,
    )

    second = await svc.service.run_poll()

    assert second.new_releases == 1
    assert second.enqueued == 1
    svc.downloads.request_album.assert_awaited_once()
    assert svc.downloads.request_album.await_args.kwargs["release_group_mbid"] == "RG3"


@pytest.mark.asyncio
async def test_future_release_without_auto_follower_is_not_backfilled(svc):
    today = utc_date.today()
    future = (today + timedelta(days=1)).isoformat()
    svc.service._today_factory = lambda: today
    await _follow_with_auto(svc.store, "user-a", state="pending")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "Future", date=future)],
        2,
    )

    first = await svc.service.run_poll()

    assert first.new_releases == 1
    assert first.enqueued == 0
    assert await svc.store.known_release_set(ARTIST_LOWER) >= {"rg1", "rg2"}
    assert await svc.store.pending_release_set(ARTIST_LOWER, 0) == set()

    await svc.store.set_auto_download_intent("user-a", ARTIST, True)
    await svc.store.upsert_approval("user-a", ARTIST, "Radiohead", "approved")
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [
            _rg("RG1", "Old", date=today.isoformat()),
            _rg("RG2", "Future", date=future),
            _rg("RG3", "New", date=today.isoformat()),
        ],
        3,
    )

    second = await svc.service.run_poll()

    assert second.new_releases == 1
    assert second.enqueued == 1
    svc.downloads.request_album.assert_awaited_once()
    assert svc.downloads.request_album.await_args.kwargs["release_group_mbid"] == "RG3"


@pytest.mark.asyncio
async def test_provider_error_does_not_drop_pending_release(svc):
    today = utc_date.today()
    tomorrow = today + timedelta(days=1)
    svc.service._today_factory = lambda: today
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    release_groups = [
        _rg("RG1", "Old", date=today.isoformat()),
        _rg("RG2", "Tomorrow", date=tomorrow.isoformat()),
    ]
    svc.mb.get_artist_release_groups_or_raise.return_value = (release_groups, 2)

    first = await svc.service.run_poll()
    assert first.new_releases == 1
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)

    svc.mb.get_artist_release_groups_or_raise.side_effect = ExternalServiceError(
        "MB down"
    )
    failed = await svc.service.run_poll()
    assert failed.errors == 1
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)

    svc.mb.get_artist_release_groups_or_raise.side_effect = None
    svc.service._today_factory = lambda: tomorrow
    recovered = await svc.service.run_poll()
    assert recovered.enqueued == 1
    assert await svc.store.pending_release_set(ARTIST_LOWER, 0) == set()


@pytest.mark.asyncio
async def test_overlapping_polls_do_not_duplicate_acquisition(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old"), _rg("RG2", "New")],
        2,
    )

    first, second = await asyncio.gather(
        svc.service.run_poll(),
        svc.service.run_poll(),
    )

    assert first.enqueued + second.enqueued == 1
    svc.downloads.request_album.assert_awaited_once()
