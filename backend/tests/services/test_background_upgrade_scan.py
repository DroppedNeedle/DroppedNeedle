"""Background upgrade scan (CollectionManagement Phase 5): opt-in sweep that
enqueues throttled origin='upgrade' grabs, owned by an admin."""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from api.v1.schemas.settings import DownloadPolicySettings
from core.tasks import run_background_upgrade_sweep


def _policy(**overrides) -> DownloadPolicySettings:
    return DownloadPolicySettings(
        upgrade_allowed=True, background_upgrade_scan_enabled=True,
        background_upgrade_max_per_run=2, **overrides,
    )


def _auth(users):
    auth = AsyncMock()
    auth.list_users.return_value = users
    return auth


def _item(rg: str) -> dict:
    return {"release_group_mbid": rg, "artist_name": "A", "album_title": "B",
            "year": 2000, "artist_mbid": "am-1", "current_tier": "mp3_192", "track_count": 10}


@pytest.mark.asyncio
async def test_sweep_enqueues_up_to_the_per_run_cap():
    service = AsyncMock()
    service.list_cutoff_unmet.return_value = [_item("rg-1"), _item("rg-2"), _item("rg-3")]
    service.request_upgrade_album.return_value = "task-x"
    admin = SimpleNamespace(id="admin-1", role="admin")

    enqueued = await run_background_upgrade_sweep(service, _auth([admin]), _policy())

    assert enqueued == 2  # throttled to background_upgrade_max_per_run
    assert service.request_upgrade_album.await_count == 2
    assert service.request_upgrade_album.await_args.kwargs["user_id"] == "admin-1"


@pytest.mark.asyncio
async def test_sweep_skips_satisfied_albums_and_needs_an_admin():
    service = AsyncMock()
    service.list_cutoff_unmet.return_value = [_item("rg-1"), _item("rg-2")]
    # first is already satisfied/deduped -> doesn't count against the cap
    service.request_upgrade_album.side_effect = ["already_in_library", "task-2"]
    admin = SimpleNamespace(id="admin-1", role="admin")

    enqueued = await run_background_upgrade_sweep(
        service, _auth([SimpleNamespace(id="u", role="user"), admin]), _policy()
    )
    assert enqueued == 1

    # no admin at all -> the sweep does nothing
    service.reset_mock()
    enqueued = await run_background_upgrade_sweep(
        service, _auth([SimpleNamespace(id="u", role="user")]), _policy()
    )
    assert enqueued == 0
    service.list_cutoff_unmet.assert_not_awaited()
