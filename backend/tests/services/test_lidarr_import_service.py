"""LidarrImportService: candidate annotation, import logic, D9/re-import regressions,
and the DR2 "zero MusicBrainz calls" guarantee. Uses the mock Lidarr transport + a REAL
FollowStore + a REAL FollowService (with an AsyncMock mb_repo we assert is never touched)."""

import sqlite3
import threading
from pathlib import Path

import pytest
from unittest.mock import AsyncMock

from api.v1.schemas.settings import LidarrImportConnectionSettings
from core.exceptions import ConfigurationError
from infrastructure.persistence.follow_store import FollowStore
from repositories.lidarr_import import LidarrImportRepository
from services.follow_service import FollowService
from services.lidarr_import_service import LidarrImportService
from tests.mocks import lidarr_mock

VALID_SELECTION = sorted(lidarr_mock.EXPECTED_FOLLOW_MBIDS)


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users "
            "(id TEXT PRIMARY KEY, display_name TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'user')"
        )
        conn.executemany(
            "INSERT OR IGNORE INTO auth_users (id, display_name, role) VALUES (?, ?, ?)",
            [("admin-1", "Admin", "admin"), ("user-a", "Alice", "user")],
        )
        conn.commit()
    finally:
        conn.close()


class _FakePrefs:
    def __init__(self, url: str, api_key: str) -> None:
        self._c = LidarrImportConnectionSettings(url=url, api_key=api_key)

    def get_lidarr_import_connection_raw(self) -> LidarrImportConnectionSettings:
        return self._c


@pytest.fixture
def ctx(tmp_path: Path):
    db_path = tmp_path / "library.db"
    store = FollowStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    mb_repo = AsyncMock()
    follow_service = FollowService(store, mb_repo)
    repo = LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.lidarr_handler))
    prefs = _FakePrefs("http://lidarr.test", lidarr_mock.GOOD_KEY)
    service = LidarrImportService(repo, prefs, store, follow_service)
    return service, store, mb_repo


@pytest.mark.asyncio
async def test_not_configured_raises(tmp_path: Path):
    db_path = tmp_path / "library.db"
    store = FollowStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    service = LidarrImportService(
        LidarrImportRepository(lidarr_mock.client_for(lidarr_mock.lidarr_handler)),
        _FakePrefs("", ""),
        store,
        FollowService(store, AsyncMock()),
    )
    with pytest.raises(ConfigurationError):
        await service.list_import_candidates("user-a")


@pytest.mark.asyncio
async def test_list_candidates_filters_and_annotates(ctx):
    service, store, _ = ctx
    result = await service.list_import_candidates("user-a")
    # Only monitored + valid MBID (unmonitored + empty-MBID excluded).
    assert result.total == len(lidarr_mock.EXPECTED_FOLLOW_MBIDS)
    by_mbid = {c.mbid: c for c in result.artists}
    assert set(by_mbid) == lidarr_mock.EXPECTED_FOLLOW_MBIDS
    assert by_mbid[lidarr_mock.MBID_AUTO].would_auto_download is True
    assert by_mbid[lidarr_mock.MBID_PLAIN].would_auto_download is False
    assert all(c.already_following is False for c in result.artists)


@pytest.mark.asyncio
async def test_list_candidates_marks_already_following(ctx):
    service, store, _ = ctx
    await store.follow_artists_bulk("user-a", [(lidarr_mock.MBID_PLAIN, "Plain Artist")])
    result = await service.list_import_candidates("user-a")
    by_mbid = {c.mbid: c for c in result.artists}
    assert by_mbid[lidarr_mock.MBID_PLAIN].already_following is True
    assert by_mbid[lidarr_mock.MBID_AUTO].already_following is False


@pytest.mark.asyncio
async def test_import_non_admin_creates_one_batch_over_new_auto_only(ctx):
    service, store, mb_repo = ctx
    summary = await service.import_artists("user-a", "user", VALID_SELECTION)

    assert summary.imported == len(lidarr_mock.EXPECTED_FOLLOW_MBIDS)
    assert summary.already_following == 0
    assert summary.skipped_invalid == 0
    assert summary.auto_download_enabled == len(lidarr_mock.EXPECTED_AUTO_DOWNLOAD_MBIDS)
    assert summary.approval_batch_id is not None

    followed = {f.artist_mbid for f in await store.list_followed_artists("user-a")}
    assert followed == lidarr_mock.EXPECTED_FOLLOW_MBIDS

    batches = await store.list_pending_approval_batches()
    assert len(batches) == 1
    assert batches[0].artist_count == len(lidarr_mock.EXPECTED_AUTO_DOWNLOAD_MBIDS)

    # Not eligible yet (pending), and 0 MusicBrainz calls (DR2).
    assert await store.list_auto_download_followers(lidarr_mock.MBID_AUTO.lower()) == []
    mb_repo.get_artist_by_id.assert_not_called()


@pytest.mark.asyncio
async def test_import_admin_no_batch_eligible_immediately(ctx):
    service, store, _ = ctx
    summary = await service.import_artists("admin-1", "admin", VALID_SELECTION)
    assert summary.approval_batch_id is None
    assert summary.auto_download_enabled == len(lidarr_mock.EXPECTED_AUTO_DOWNLOAD_MBIDS)
    assert await store.list_pending_approval_batches() == []
    # Admin is approved by role -> immediately an eligible follower.
    assert await store.list_auto_download_followers(lidarr_mock.MBID_AUTO.lower()) == ["admin-1"]


@pytest.mark.asyncio
async def test_import_plain_artist_is_follow_only(ctx):
    service, store, _ = ctx
    await service.import_artists("user-a", "user", [lidarr_mock.MBID_PLAIN])
    by_mbid = {f.artist_mbid: f for f in await store.list_followed_artists("user-a")}
    assert by_mbid[lidarr_mock.MBID_PLAIN].auto_download is False
    assert await store.list_pending_approval_batches() == []


@pytest.mark.asyncio
async def test_invalid_mbid_skipped_and_unmonitored_ignored(ctx):
    service, store, _ = ctx
    summary = await service.import_artists(
        "user-a",
        "user",
        ["not-a-real-mbid", lidarr_mock.MBID_UNMONITORED, lidarr_mock.MBID_PLAIN],
    )
    assert summary.skipped_invalid == 1  # the malformed MBID
    # Unmonitored is a valid MBID but not currently monitored in Lidarr -> ignored (DR3),
    # neither imported nor skipped.
    assert summary.imported == 1
    followed = {f.artist_mbid for f in await store.list_followed_artists("user-a")}
    assert followed == {lidarr_mock.MBID_PLAIN}


@pytest.mark.asyncio
async def test_reimport_is_additive_and_counts_split(ctx):
    service, store, _ = ctx
    await service.import_artists("user-a", "user", [lidarr_mock.MBID_PLAIN])
    summary = await service.import_artists("user-a", "user", VALID_SELECTION)
    # PLAIN already followed; the rest are new.
    assert summary.already_following == 1
    assert summary.imported == len(lidarr_mock.EXPECTED_FOLLOW_MBIDS) - 1
    # No double-count and nothing unfollowed.
    followed = {f.artist_mbid for f in await store.list_followed_artists("user-a")}
    assert followed == lidarr_mock.EXPECTED_FOLLOW_MBIDS


@pytest.mark.asyncio
async def test_reimport_with_narrower_selection_never_unfollows(ctx):
    # D6: a re-import that omits a previously-imported artist must NOT unfollow it.
    service, store, _ = ctx
    await service.import_artists("user-a", "user", [lidarr_mock.MBID_PLAIN, lidarr_mock.MBID_ENDED])
    summary = await service.import_artists("user-a", "user", [lidarr_mock.MBID_PLAIN])
    assert summary.imported == 0
    assert summary.already_following == 1
    followed = {f.artist_mbid for f in await store.list_followed_artists("user-a")}
    # ENDED, absent from the narrower re-import, is still followed - nothing was pruned.
    assert followed == {lidarr_mock.MBID_PLAIN, lidarr_mock.MBID_ENDED}


@pytest.mark.asyncio
async def test_reimport_does_not_rearm_locally_disabled_auto_download(ctx):
    service, store, _ = ctx
    # First import arms AUTO; user then turns auto-download OFF locally.
    await service.import_artists("user-a", "user", [lidarr_mock.MBID_AUTO])
    await store.set_auto_download_intent("user-a", lidarr_mock.MBID_AUTO, False)

    summary = await service.import_artists("user-a", "user", [lidarr_mock.MBID_AUTO])
    # Already-followed -> never re-armed (D9). Intent stays off; no new batch.
    assert summary.auto_download_enabled == 0
    assert summary.approval_batch_id is None
    by_mbid = {f.artist_mbid: f for f in await store.list_followed_artists("user-a")}
    assert by_mbid[lidarr_mock.MBID_AUTO].auto_download is False


@pytest.mark.asyncio
async def test_reimport_keeps_approved_grant_approved(ctx):
    service, store, _ = ctx
    # Import + admin-approve AUTO for a non-admin user.
    await service.import_artists("user-a", "user", [lidarr_mock.MBID_AUTO])
    batch = (await store.list_pending_approval_batches())[0]
    await store.set_batch_approval_state(batch.batch_id, "approved", ("admin-1", "Admin"))
    assert await store.list_auto_download_followers(lidarr_mock.MBID_AUTO.lower()) == ["user-a"]

    # Re-import must not downgrade the approved grant or re-batch it.
    summary = await service.import_artists("user-a", "user", [lidarr_mock.MBID_AUTO])
    assert summary.auto_download_enabled == 0
    assert summary.approval_batch_id is None
    assert await store.list_auto_download_followers(lidarr_mock.MBID_AUTO.lower()) == ["user-a"]
    assert await store.list_pending_approval_batches() == []
