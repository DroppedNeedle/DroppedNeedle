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
from repositories.musicbrainz_response_cache import MbCachePolicy
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
    async def page(artist, *, offset, limit, source_context, **kwargs):
        rows, total = await mb.get_artist_release_groups_or_raise(artist, offset=offset, limit=limit)
        return rows, total, source_context
    mb.get_artist_release_groups_with_context = AsyncMock(side_effect=page)
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

async def _poll_due(svc):
    await svc.store.enqueue_due_all()
    return await svc.service.run_poll()


@pytest.mark.asyncio
async def test_first_poll_seeds_baseline_and_enqueues_nothing(svc):
    await _follow_with_auto(svc.store, "user-a")
    svc.mb.get_artist_release_groups_or_raise.return_value = (
        [_rg("RG1", "Old 1"), _rg("RG2", "Old 2")],
        2,
    )
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
    assert summary.new_releases == 1
    assert summary.enqueued == 0  # feed populated, but no task created


@pytest.mark.asyncio
async def test_mb_error_does_not_advance_baseline(svc):
    await _follow_with_auto(svc.store, "user-a")
    svc.mb.get_artist_release_groups_or_raise.side_effect = ExternalServiceError(
        "MB down"
    )
    summary = await _poll_due(svc)
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
    summary = await _poll_due(svc)
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

    summary = await _poll_due(svc)

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

    summary = await _poll_due(svc)

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

    first = await _poll_due(svc)
    assert first.new_releases == 1
    assert first.enqueued == 0
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)

    svc.service._today_factory = lambda: tomorrow
    second = await _poll_due(svc)

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

    first = await _poll_due(svc)
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
    second = await _poll_due(svc)

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

    summary = await _poll_due(svc)

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

    first = await _poll_due(svc)

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

    second = await _poll_due(svc)

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

    first = await _poll_due(svc)

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

    second = await _poll_due(svc)

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

    first = await _poll_due(svc)
    assert first.new_releases == 1
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)

    svc.mb.get_artist_release_groups_or_raise.side_effect = ExternalServiceError(
        "MB down"
    )
    failed = await _poll_due(svc)
    assert failed.errors == 1
    assert "rg2" in await svc.store.pending_release_set(ARTIST_LOWER, 0)

    svc.mb.get_artist_release_groups_or_raise.side_effect = None
    svc.service._today_factory = lambda: tomorrow
    recovered = await _poll_due(svc)
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

    await svc.store.enqueue_due_all()
    first, second = await asyncio.gather(
        svc.service.run_poll(),
        svc.service.run_poll(),
    )

    assert first.enqueued + second.enqueued == 1
    svc.downloads.request_album.assert_awaited_once()


@pytest.mark.asyncio
async def test_recent_success_restart_does_no_provider_or_owned_scan(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["old"], policy_revision=0)
    svc.service._store = FollowStore(svc.db)
    assert (await svc.service.run_poll()).artists_polled == 0
    svc.mb.get_artist_release_groups_with_context.assert_not_called()
    svc.library.get_library_mbids.assert_not_called()


@pytest.mark.asyncio
async def test_complete_inventory_beyond_100_keeps_payload(svc):
    await _follow_with_auto(svc.store, "user-a")
    await _follow_with_auto(svc.store, "user-b")
    rows = [_rg(f"rg{i}", f"Old {i}") for i in range(151)]
    rows[130] = _rg("new", "Beyond page one", secondary=["Live"])
    await svc.store.seed_baseline(ARTIST_LOWER, [r["id"] for r in rows if r["id"] != "new"], policy_revision=0)
    rows[130]["secondary-types"] = []
    async def page(artist, *, offset, limit, source_context, **kwargs):
        return rows[offset:offset+limit], len(rows), source_context
    svc.mb.get_artist_release_groups_with_context.side_effect = page
    result = await _poll_due(svc)
    assert result.new_releases == result.enqueued == 1
    assert svc.downloads.request_album.await_args.kwargs["album_title"] == "Beyond page one"
    items, total = await svc.store.list_new_releases_for_user("user-b", 50, 0)
    assert total == 1
    assert (items[0].title, items[0].first_release_date, items[0].primary_type) == (
        "Beyond page one", rows[130]["first-release-date"], "Album")


@pytest.mark.asyncio
async def test_large_verification_resumes_fairly_and_restart_resets_once(svc, monkeypatch):
    import services.native.new_release_service as module
    await svc.store.follow_artist("user-a", ARTIST, "Large")
    large = [_rg(f"rg{i}", f"Title {i}") for i in range(1201)]
    calls = []
    async def page(artist, *, offset, limit, source_context, **kwargs):
        calls.append((artist, offset))
        rows = large if artist == ARTIST else [_rg("small", "Small")]
        return rows[offset:offset+limit], len(rows), source_context
    svc.mb.get_artist_release_groups_with_context.side_effect = page
    for peer in ("peer-a", "peer-b", "peer-c"):
        await svc.store.follow_artist("user-a", peer, peer)
    await svc.service.run_poll()
    assert len(calls) == 10
    assert {artist for artist, _ in calls} == {ARTIST, "peer-a", "peer-b", "peer-c"}
    while True:
        await svc.service.run_poll()
        with sqlite3.connect(svc.db) as conn:
            state = conn.execute("SELECT phase,offset FROM follow_inventory WHERE artist_mbid_lower=?", (ARTIST_LOWER,)).fetchone()
        if state and state[0] == "verifying" and state[1] > 0:
            break
    monkeypatch.setattr(module, "_PROCESS", "restarted")
    calls.clear()
    await svc.service.run_poll()
    assert calls[0] == (ARTIST, 0)
    assert len(calls) == 10
    calls.clear()
    result = await svc.service.run_poll()
    assert calls[0] == (ARTIST, 1000)
    assert result.baselined == 1
    assert await svc.store.known_release_set(ARTIST_LOWER) == {row["id"] for row in large}


@pytest.mark.asyncio
async def test_verification_change_restages_without_false_success(svc):
    await svc.store.follow_artist("user-a", ARTIST, "Artist")
    counter = 0
    async def page(artist, *, source_context, **kwargs):
        nonlocal counter
        counter += 1
        return [_rg("rg", str(counter))], 1, source_context
    svc.mb.get_artist_release_groups_with_context.side_effect = page
    assert (await svc.service.run_poll()).baselined == 0
    assert not await svc.store.has_cursor(ARTIST_LOWER)
    svc.mb.get_artist_release_groups_with_context.side_effect = None
    from repositories.musicbrainz_base import capture_mb_source_context
    svc.mb.get_artist_release_groups_with_context.return_value = ([_rg("rg", "10")], 1, capture_mb_source_context())
    assert (await svc.service.run_poll()).baselined == 1


@pytest.mark.asyncio
async def test_enrollment_during_detection_rejects_old_inventory(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, [], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = ([_rg("new", "New")], 1)
    original = svc.store.record_new_releases
    async def change_follow(*args, **kwargs):
        await svc.store.unfollow_artist("user-a", ARTIST)
        return await original(*args, **kwargs)
    svc.store.record_new_releases = change_follow
    assert (await _poll_due(svc)).enqueued == 0
    assert await svc.store.known_release_set(ARTIST_LOWER) == set()
    svc.downloads.request_album.assert_not_called()


@pytest.mark.asyncio
async def test_retry_cooldown_and_idempotent_migration_preserve_ledger(svc):
    import time
    from infrastructure.resilience.retry import CircuitOpenError
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["old"], policy_revision=0)
    cursor = await svc.store.get_release_check_state(ARTIST_LOWER)
    svc.mb.get_artist_release_groups_or_raise.side_effect = CircuitOpenError(
        "cooling down", retry_after_seconds=30000)
    assert (await _poll_due(svc)).errors == 1
    FollowStore(svc.db)
    FollowStore(svc.db)
    with sqlite3.connect(svc.db) as conn:
        due, failures = conn.execute("SELECT due_at,failures FROM follow_due WHERE artist_mbid_lower=?", (ARTIST_LOWER,)).fetchone()
    assert due > time.time() + 29900
    assert failures == 1
    assert (await svc.store.get_release_check_state(ARTIST_LOWER)).last_checked_at == cursor.last_checked_at
    assert await svc.store.known_release_set(ARTIST_LOWER) == {"old"}
    assert (await svc.service.run_poll()).artists_polled == 0


@pytest.mark.asyncio
async def test_acquisition_runs_outside_source_fence(svc):
    from repositories.musicbrainz_base import capture_mb_source_context, mb_publish_if_current
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, [], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.return_value = ([_rg("new", "New")], 1)
    entered = False
    async def acquire(**kwargs):
        nonlocal entered
        async def probe():
            nonlocal entered
            entered = True
        await asyncio.wait_for(mb_publish_if_current(capture_mb_source_context(), probe), 1)
        return "task"
    svc.service._acquisition.request_album = acquire
    assert (await _poll_due(svc)).enqueued == 1
    assert entered


@pytest.mark.asyncio
async def test_legacy_due_migration_uses_real_success(tmp_path):
    db = tmp_path / "legacy.db"
    _seed_auth_users(db)
    with sqlite3.connect(db) as conn:
        conn.executescript("""
            CREATE TABLE user_followed_artists(
                user_id TEXT,artist_mbid TEXT,artist_mbid_lower TEXT,artist_name TEXT,
                auto_download INTEGER,followed_at REAL,updated_at REAL,
                PRIMARY KEY(user_id,artist_mbid_lower));
            CREATE TABLE artist_release_check(
                artist_mbid_lower TEXT PRIMARY KEY,last_checked_at REAL,
                last_status TEXT,last_error TEXT,release_type_policy_revision INTEGER);
            INSERT INTO user_followed_artists VALUES('user-a','artist','artist','Artist',0,1,1);
            INSERT INTO artist_release_check VALUES('artist',1234,'ok',NULL,4);
        """)
    store = FollowStore(db)
    FollowStore(db)
    with sqlite3.connect(db) as conn:
        assert conn.execute("SELECT due_at FROM follow_due").fetchone()[0] == 1234 + 86400
    state = await store.get_release_check_state("artist")
    assert state.last_checked_at == 1234
    assert state.release_type_policy_revision == 4


@pytest.mark.asyncio
async def test_cancelled_detection_requires_fresh_verification(svc):
    await _follow_with_auto(svc.store, "user-a")
    svc.mb.get_artist_release_groups_or_raise.return_value = ([_rg("rg", "Title")], 1)
    svc.library.get_library_mbids.side_effect = asyncio.CancelledError()
    with pytest.raises(asyncio.CancelledError):
        await svc.service.run_poll()
    assert not await svc.store.has_cursor(ARTIST_LOWER)
    svc.library.get_library_mbids.side_effect = None
    svc.mb.get_artist_release_groups_with_context.reset_mock()
    assert (await svc.service.run_poll()).baselined == 1
    assert svc.mb.get_artist_release_groups_with_context.await_args.kwargs["offset"] == 0


@pytest.mark.asyncio
async def test_source_policy_and_expiry_discard_staging_not_ledger(svc):
    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["known"], policy_revision=0)
    first = await svc.store.prepare_inventory(ARTIST_LOWER, "source-a", 0, "process")
    await svc.store.stage_inventory_page(first, [_rg("partial", "Partial")], 2)
    second = await svc.store.prepare_inventory(ARTIST_LOWER, "source-b", 0, "process")
    assert second["offset"] == 0
    assert await svc.store.stage_inventory_page(first, [_rg("late", "Late")], 1) is None
    await svc.store.stage_inventory_page(second, [_rg("partial", "Partial")], 2)
    third = await svc.store.prepare_inventory(ARTIST_LOWER, "source-b", 1, "process")
    assert third["offset"] == 0
    await svc.store.stage_inventory_page(third, [_rg("partial", "Partial")], 2)
    with sqlite3.connect(svc.db) as conn:
        conn.execute("UPDATE follow_inventory SET progressed_at=0")
    fresh = await svc.store.prepare_inventory(ARTIST_LOWER, "source-b", 1, "process")
    assert fresh["offset"] == 0
    assert await svc.store.known_release_set(ARTIST_LOWER) == {"known"}
    assert (await svc.store.get_release_check_state(ARTIST_LOWER)).release_type_policy_revision == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("transition", ["source", "policy", "follow", "approval"])
async def test_superseded_http_failure_cannot_backoff_fresh_enrollment(svc, monkeypatch, transition):
    from infrastructure.persistence.follow_store import InventoryInvalidated
    from repositories import musicbrainz_base as mb_base

    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["known"], policy_revision=0)
    await svc.store.enqueue_due_all()
    artist = (await svc.store.list_due_artists(float("inf")))[0]
    entered = asyncio.Event()
    release = asyncio.Event()

    async def failing_page(*args, **kwargs):
        entered.set()
        await release.wait()
        raise ExternalServiceError("old request failed")

    svc.mb.get_artist_release_groups_with_context.side_effect = failing_page
    task = asyncio.create_task(svc.service._process_artist(artist))
    await entered.wait()
    try:
        if transition == "source":
            monkeypatch.setattr(mb_base, "_mb_source_generation", mb_base._mb_source_generation + 1)
            await svc.store.enqueue_due_all()
        elif transition == "policy":
            async with svc.service._policy_transition_lock:
                svc.preferences.get_preferences_with_revision.return_value = (UserPreferences(), 1)
                await svc.store.enqueue_due_all()
        elif transition == "follow":
            await svc.store.follow_artist("user-b", ARTIST, "Radiohead")
        else:
            await svc.store.upsert_approval("user-a", ARTIST, "Radiohead", "approved")
        # Recreate the same artist's staging before the old HTTP call settles.
        await svc.store.prepare_inventory(ARTIST_LOWER, "fresh-source", 1, "fresh-process")
        release.set()
        with pytest.raises(InventoryInvalidated):
            await task
        with sqlite3.connect(svc.db) as conn:
            assert conn.execute("SELECT due_at, failures FROM follow_due").fetchone() == (0, 0)
        assert await svc.store.known_release_set(ARTIST_LOWER) == {"known"}
        assert (await svc.store.get_release_check_state(ARTIST_LOWER)).last_status == "ok"
    finally:
        release.set()
        await asyncio.gather(task, return_exceptions=True)


@pytest.mark.asyncio
async def test_current_inventory_failure_backs_off_without_losing_progress(svc):
    await _follow_with_auto(svc.store, "user-a")
    state = await svc.store.prepare_inventory(ARTIST_LOWER, "source", 0, "process")
    await svc.store.stage_inventory_page(state, [_rg("first", "First")], 2)
    current = await svc.store.prepare_inventory(ARTIST_LOWER, "source", 0, "process")
    assert await svc.store.fail_inventory(state, "superseded observation") is False
    assert await svc.store.fail_inventory(current, "provider unavailable", 3600) is True
    with sqlite3.connect(svc.db) as conn:
        due_at, failures, serviced = conn.execute("SELECT due_at,failures,last_serviced FROM follow_due").fetchone()
        assert failures == 1
        assert due_at - serviced >= 3600
    resumed = await svc.store.prepare_inventory(ARTIST_LOWER, "source", 0, "process")
    assert resumed["offset"] == 1


class _FailCapturingStore:
    """Stubbed inventory store: fixed prepare state, captures fail kwargs."""

    def __init__(self, state: dict):
        self._state = state
        self.fail_kwargs: dict | None = None

    async def prepare_inventory(self, artist, source, policy, process):
        return dict(self._state)

    async def fail_inventory(self, state, error, retry_after=0, *, min_delay=0):
        self.fail_kwargs = {"retry_after": retry_after, "min_delay": min_delay}
        return True


_WALK_MIN_DELAY_CASES = {
    "fresh": (
        {"phase": "collecting", "offset": 0, "total": None, "diverge_count": 0},
        0,
    ),
    "verifying": (
        {"phase": "verifying", "offset": 100, "total": 250, "diverge_count": 0},
        3600,
    ),
    "mid_collecting": (
        {"phase": "collecting", "offset": 100, "total": 250, "diverge_count": 0},
        3600,
    ),
    "diverged": (
        {"phase": "collecting", "offset": 0, "total": None, "diverge_count": 1},
        3600,
    ),
}


def _stub_state(patch: dict) -> dict:
    return {
        "artist_mbid_lower": ARTIST_LOWER,
        "source": "stub",
        "policy": 0,
        "observation": "stub-obs",
        **patch,
    }


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["fresh", "verifying", "mid_collecting", "diverged"])
async def test_process_artist_walk_failure_min_delay(svc, kind):
    patch, expected = _WALK_MIN_DELAY_CASES[kind]
    stub = _FailCapturingStore(_stub_state(patch))
    svc.service._store = stub
    svc.mb.get_artist_release_groups_with_context.side_effect = ExternalServiceError(
        "MB down"
    )
    artist = SimpleNamespace(
        artist_mbid=ARTIST, artist_mbid_lower=ARTIST_LOWER, artist_name="Radiohead"
    )
    with pytest.raises(ExternalServiceError):
        await svc.service._process_artist(artist)
    assert stub.fail_kwargs == {"retry_after": 0, "min_delay": expected}


@pytest.mark.asyncio
@pytest.mark.parametrize(("kind", "expected"), [("fresh", 0), ("verifying", 3600)])
async def test_process_artist_rate_limit_floor_only_on_walk(svc, kind, expected):
    from core.exceptions import RateLimitedError

    patch, _ = _WALK_MIN_DELAY_CASES[kind]
    stub = _FailCapturingStore(_stub_state(patch))
    svc.service._store = stub
    svc.mb.get_artist_release_groups_with_context.side_effect = RateLimitedError(
        "slow down", retry_after_seconds=45
    )
    artist = SimpleNamespace(
        artist_mbid=ARTIST, artist_mbid_lower=ARTIST_LOWER, artist_name="Radiohead"
    )
    with pytest.raises(RateLimitedError):
        await svc.service._process_artist(artist)
    # retry_after is always forwarded; the 1h floor binds only walk failures
    assert stub.fail_kwargs == {"retry_after": 45, "min_delay": expected}


@pytest.mark.asyncio
async def test_mid_walk_failure_backs_off_one_hour(svc):
    import json
    import time

    from repositories.musicbrainz_base import capture_mb_source_context

    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    await svc.store.enqueue_due_all()
    context = capture_mb_source_context()
    source = json.dumps([context.source_mode, context.source_id, context.generation])
    staged = await svc.store.prepare_inventory(ARTIST_LOWER, source, 0, "seed-proc")
    await svc.store.stage_inventory_page(staged, [_rg("RG2", "Second")], 3)
    svc.mb.get_artist_release_groups_or_raise.side_effect = ExternalServiceError(
        "MB down"
    )

    before = time.time()
    summary = await svc.service.run_poll()

    assert summary.errors == 1
    with sqlite3.connect(svc.db) as conn:
        due, failures = conn.execute(
            "SELECT due_at, failures FROM follow_due WHERE artist_mbid_lower = ?",
            (ARTIST_LOWER,),
        ).fetchone()
    assert failures == 1
    assert due >= before + 3600


@pytest.mark.asyncio
async def test_fresh_observation_failure_keeps_900s_ladder(svc):
    import time

    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.side_effect = ExternalServiceError(
        "MB down"
    )
    await svc.store.enqueue_due_all()

    before = time.time()
    summary = await svc.service.run_poll()
    after = time.time()

    assert summary.errors == 1
    with sqlite3.connect(svc.db) as conn:
        due, failures = conn.execute(
            "SELECT due_at, failures FROM follow_due WHERE artist_mbid_lower = ?",
            (ARTIST_LOWER,),
        ).fetchone()
    assert failures == 1
    assert before + 900 <= due <= after + 900


@pytest.mark.asyncio
async def test_fresh_rate_limit_honors_retry_after_without_floor(svc):
    import time

    from core.exceptions import RateLimitedError

    await _follow_with_auto(svc.store, "user-a")
    await svc.store.seed_baseline(ARTIST_LOWER, ["rg1"], policy_revision=0)
    svc.mb.get_artist_release_groups_or_raise.side_effect = RateLimitedError(
        "slow down", retry_after_seconds=60
    )
    await svc.store.enqueue_due_all()

    before = time.time()
    summary = await svc.service.run_poll()
    after = time.time()

    assert summary.errors == 1
    with sqlite3.connect(svc.db) as conn:
        due, failures = conn.execute(
            "SELECT due_at, failures FROM follow_due WHERE artist_mbid_lower = ?",
            (ARTIST_LOWER,),
        ).fetchone()
    assert failures == 1
    # ladder wins over the small retry_after; no 1h floor on a pure 429
    assert before + 900 <= due <= after + 900


@pytest.mark.asyncio
async def test_detection_page_bypasses_cache(svc):
    await _follow_with_auto(svc.store, "user-a")
    await _poll_due(svc)
    calls = svc.mb.get_artist_release_groups_with_context.await_args_list
    assert calls, "expected at least one detection page fetch"
    for call in calls:
        assert call.kwargs.get("cache_policy") is MbCachePolicy.BYPASS
