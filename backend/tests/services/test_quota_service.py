"""QuotaService (CollectionManagement Feature C) - real stores over one tmp SQLite
file, mirroring production wiring (all stores share the db + write lock).

Layer 1: rolling request-count quota at submit (pending counts; track asks count;
retries/upgrades don't; admin/trusted exempt; window rolls; override > default).
Layer 2: global byte cap (all roles; upgrades exempt) + per-user storage quota
(scan-discovered files unowned; admin/trusted exempt).
"""

import threading
import time
from pathlib import Path

import pytest

from core.config import Settings
from core.exceptions import ValidationError
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.persistence.user_quota_store import UserQuotaStore
from models.audio import AudioInfo, AudioTag
from services.native.library_manager import LibraryManager
from services.preferences_service import PreferencesService
from services.quota_service import QuotaService


class _Env:
    def __init__(self, tmp_path: Path):
        lock = threading.Lock()
        db = tmp_path / "library.db"
        self.auth = AuthStore(db_path=db, write_lock=lock)
        self.download_store = DownloadStore(db_path=db, write_lock=lock)
        self.library_db = LibraryDB(db_path=db, write_lock=lock)
        self.request_history = RequestHistoryStore(db_path=db, write_lock=lock)
        self.user_quotas = UserQuotaStore(db_path=db, write_lock=lock)
        self.library = LibraryManager(self.library_db)
        settings = Settings()
        settings.config_file_path = tmp_path / "config.json"
        self.preferences = PreferencesService(settings)
        self.quota = QuotaService(
            preferences=self.preferences,
            user_quotas=self.user_quotas,
            request_history=self.request_history,
            download_store=self.download_store,
            library_db=self.library_db,
            auth_store=self.auth,
        )

    def set_policy(self, **fields) -> None:
        from api.v1.schemas.settings import DownloadPolicySettings

        self.preferences.save_download_policy(DownloadPolicySettings(**fields))

    async def add_user(self, user_id: str, role: str = "user") -> None:
        await self.auth.create_user(id=user_id, display_name=user_id, role=role)

    async def record_album_ask(self, user_id: str, mbid: str) -> None:
        await self.request_history.async_record_request(
            musicbrainz_id=mbid, artist_name="A", album_title="B",
            user_id=user_id, initial_status="awaiting_approval",
        )

    async def add_track_task(self, user_id: str, rec: str, origin: str = "user") -> None:
        await self.download_store.create_task(
            user_id=user_id, download_type="track", release_group_mbid="rg-t",
            recording_mbid=rec, artist_name="A", album_title="B", origin=origin,
        )

    async def add_library_file(
        self, path: str, *, size: int, rg: str, track: int, task_id: str | None = None
    ) -> None:
        tag = AudioTag(
            title=f"T{track}", artist="A", album="B", album_artist="A",
            track_number=track, disc_number=1, musicbrainz_release_group_id=rg,
        )
        info = AudioInfo(
            duration_seconds=100.0, bitrate=320, sample_rate=44100, channels=2,
            file_format="mp3", file_size_bytes=size,
        )
        await self.library.upsert_file(
            Path(path), tag, info, release_group_mbid=rg,
            recording_mbid=f"rec-{rg}-{track}", download_task_id=task_id,
            source="download" if task_id else "scan",
        )


@pytest.fixture
def env(tmp_path: Path) -> _Env:
    return _Env(tmp_path)


# --- Layer 1: request-count quota (submit) ------------------------------------


@pytest.mark.asyncio
async def test_request_quota_blocks_at_limit_and_pending_counts(env: _Env):
    await env.add_user("u1")
    env.set_policy(default_request_quota_count=2, default_request_quota_days=7)
    # two PENDING asks (never approved) still count - that's the point of submit-time
    await env.record_album_ask("u1", "rg-1")
    await env.record_album_ask("u1", "rg-2")

    with pytest.raises(ValidationError) as exc:
        await env.quota.check_request_quota("u1", "user")
    assert "Request limit reached" in str(exc.value)


@pytest.mark.asyncio
async def test_request_quota_counts_track_asks_but_not_retries_or_upgrades(env: _Env):
    await env.add_user("u1")
    env.set_policy(default_request_quota_count=2, default_request_quota_days=7)
    await env.add_track_task("u1", "rec-1", origin="user")
    await env.add_track_task("u1", "rec-2", origin="retry")
    await env.add_track_task("u1", "rec-3", origin="upgrade")

    assert await env.quota.count_requests_in_window("u1", 7) == 1
    await env.quota.check_request_quota("u1", "user")  # 1 + 1 <= 2 -> allowed

    await env.add_track_task("u1", "rec-4", origin="user")
    with pytest.raises(ValidationError):
        await env.quota.check_request_quota("u1", "user")


@pytest.mark.asyncio
@pytest.mark.parametrize("role", ["admin", "trusted"])
async def test_request_quota_exempts_admin_and_trusted(env: _Env, role: str):
    await env.add_user("u1", role=role)
    env.set_policy(default_request_quota_count=1, default_request_quota_days=7)
    await env.record_album_ask("u1", "rg-1")
    await env.quota.check_request_quota("u1", role)  # no raise


@pytest.mark.asyncio
async def test_request_quota_window_rolls(env: _Env):
    await env.add_user("u1")
    env.set_policy(default_request_quota_count=1, default_request_quota_days=7)
    # a track ask 8 days old falls outside the 7-day window
    await env.add_track_task("u1", "rec-old")

    def age_task(conn):
        conn.execute(
            "UPDATE download_tasks SET created_at = ? WHERE recording_mbid = 'rec-old'",
            (time.time() - 8 * 86400,),
        )

    await env.download_store._write(age_task)

    assert await env.quota.count_requests_in_window("u1", 7) == 0
    await env.quota.check_request_quota("u1", "user")  # allowed again


@pytest.mark.asyncio
async def test_request_quota_zero_is_unlimited_and_override_beats_default(env: _Env):
    await env.add_user("u1")
    env.set_policy(default_request_quota_count=0)
    await env.record_album_ask("u1", "rg-1")
    await env.quota.check_request_quota("u1", "user")  # unlimited default

    # per-user override tightens it below current usage
    await env.quota.set_override(
        "u1", request_quota_count=1, request_quota_days=None, storage_quota_gb=None
    )
    with pytest.raises(ValidationError):
        await env.quota.check_request_quota("u1", "user")

    # NULL fields inherit: days comes from the default
    effective = await env.quota.effective_quota("u1")
    assert effective.request_quota_count == 1
    assert effective.request_quota_days == 7


@pytest.mark.asyncio
async def test_request_quota_batch_counts_n(env: _Env):
    await env.add_user("u1")
    env.set_policy(default_request_quota_count=3, default_request_quota_days=7)
    await env.record_album_ask("u1", "rg-1")

    await env.quota.check_request_quota("u1", "user", new_requests=2)  # 1+2 <= 3
    with pytest.raises(ValidationError):
        await env.quota.check_request_quota("u1", "user", new_requests=3)


# --- Layer 2: byte caps (task creation) ----------------------------------------


@pytest.mark.asyncio
async def test_global_cap_blocks_all_roles_but_not_upgrades(env: _Env):
    await env.add_user("u1", role="admin")
    env.set_policy(max_library_size_gb=1)
    # 1 GiB of scanned files -> at the cap
    await env.add_library_file("/m/a/01.mp3", size=1024**3, rg="rg-1", track=1)

    with pytest.raises(ValidationError) as exc:
        await env.quota.check_storage_admission("u1", "user")
    assert "Library storage limit reached" in str(exc.value)

    await env.quota.check_storage_admission("u1", "upgrade")  # upgrades exempt


@pytest.mark.asyncio
async def test_global_cap_zero_unlimited_and_under_allows(env: _Env):
    await env.add_user("u1")
    await env.add_library_file("/m/a/01.mp3", size=1024**3, rg="rg-1", track=1)
    env.set_policy(max_library_size_gb=0)
    await env.quota.check_storage_admission("u1", "user")  # unlimited

    env.set_policy(max_library_size_gb=2)
    await env.quota.check_storage_admission("u1", "user")  # 1 GiB < 2 GB cap


@pytest.mark.asyncio
async def test_user_storage_quota_counts_only_owned_downloads(env: _Env):
    await env.add_user("u1")
    env.set_policy(default_storage_quota_gb=1)
    task = await env.download_store.create_task(
        user_id="u1", release_group_mbid="rg-1", artist_name="A", album_title="B"
    )
    # 1 GiB owned by u1's download + 5 GiB of scanned (unowned) files
    await env.add_library_file("/m/a/01.mp3", size=1024**3, rg="rg-1", track=1, task_id=task.id)
    await env.add_library_file("/m/b/01.mp3", size=5 * 1024**3, rg="rg-2", track=1)

    assert await env.library_db.get_user_library_bytes("u1") == 1024**3

    with pytest.raises(ValidationError) as exc:
        await env.quota.check_storage_admission("u1", "user")
    assert "Your storage budget is full" in str(exc.value)


@pytest.mark.asyncio
@pytest.mark.parametrize("role", ["admin", "trusted"])
async def test_user_storage_quota_exempts_curators(env: _Env, role: str):
    await env.add_user("u1", role=role)
    env.set_policy(default_storage_quota_gb=1)
    task = await env.download_store.create_task(
        user_id="u1", release_group_mbid="rg-1", artist_name="A", album_title="B"
    )
    await env.add_library_file("/m/a/01.mp3", size=2 * 1024**3, rg="rg-1", track=1, task_id=task.id)

    await env.quota.check_storage_admission("u1", "user")  # exempt via role lookup


@pytest.mark.asyncio
async def test_usage_report_and_override_roundtrip(env: _Env):
    await env.add_user("u1")
    env.set_policy(default_request_quota_count=5, default_storage_quota_gb=10)
    await env.record_album_ask("u1", "rg-1")

    usage = await env.quota.usage_for("u1")
    assert usage.requests_in_window == 1
    assert usage.quota.request_quota_count == 5
    assert usage.exempt is False

    # all-NULL override deletes the row (pure inherit)
    await env.quota.set_override(
        "u1", request_quota_count=9, request_quota_days=None, storage_quota_gb=None
    )
    assert (await env.quota.get_override("u1")).request_quota_count == 9
    await env.quota.set_override(
        "u1", request_quota_count=None, request_quota_days=None, storage_quota_gb=None
    )
    assert await env.quota.get_override("u1") is None

    with pytest.raises(ValidationError):
        await env.quota.set_override(
            "u1", request_quota_count=-1, request_quota_days=None, storage_quota_gb=None
        )


@pytest.mark.asyncio
async def test_storage_admission_exempts_retries(env: _Env):
    """A retry re-attempts an ask that was admitted at its original submit - both
    retry buttons (requests page and downloads queue) must behave the same."""
    await env.add_user("u1")
    env.set_policy(max_library_size_gb=1)
    await env.add_library_file("/m/a/01.mp3", size=1024**3, rg="rg-1", track=1)

    await env.quota.check_storage_admission("u1", "retry")  # no raise
    with pytest.raises(ValidationError):
        await env.quota.check_storage_admission("u1", "user")
