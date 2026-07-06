"""Cost-control enforcement (CollectionManagement Feature C, D6/D8/D9/D11/D20).

Two distinct layers - deliberately NOT one choke point:

- **Layer 1, request-count quota, at submit.** A plain user's ask is recorded in
  ``request_history`` long before any download task exists, so the count gate runs
  where the ask is made: ``RequestService.request_album``/``request_batch`` and the
  per-track request route (tracks bypass approval, so their ``download_tasks`` row
  is the ask). Rolling window (D9); pending asks count.

- **Layer 2, byte caps, at every download-task-creation site.** The global library
  cap (all roles) and the per-user storage quota apply when bytes will actually be
  pulled: ``DownloadService.request_album`` (which ``request_track`` funnels into)
  AND the manual-pick path ``pick_candidate``. The requester's role is looked up by
  ``user_id`` here rather than threaded through the call sites.

``admin`` and ``trusted`` are exempt from the per-user quotas (D8); the global cap
applies to everyone; ``origin='upgrade'`` is exempt from all of it (an upgrade is
size-neutral: the old file is recycled). Caps block at/over and never evict (A3).
"""

import logging
import time
from datetime import datetime, timedelta, timezone
from typing import TYPE_CHECKING

from core.exceptions import ValidationError
from infrastructure.msgspec_fastapi import AppStruct

if TYPE_CHECKING:
    from infrastructure.persistence.auth_store import AuthStore
    from infrastructure.persistence.download_store import DownloadStore
    from infrastructure.persistence.library_db import LibraryDB
    from infrastructure.persistence.request_history import RequestHistoryStore
    from infrastructure.persistence.user_quota_store import UserQuotaStore
    from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

_GIB = 1024**3
_EXEMPT_ROLES = ("admin", "trusted")


class EffectiveQuota(AppStruct):
    """A user's effective limits: override if set, else the global default (0 = unlimited)."""

    request_quota_count: int
    request_quota_days: int
    storage_quota_gb: int


class QuotaUsage(AppStruct):
    """One user's current standing against their effective quota (admin UI)."""

    user_id: str
    quota: EffectiveQuota
    requests_in_window: int
    storage_bytes: int
    exempt: bool


def _gb_label(bytes_used: int, cap_gb: int) -> str:
    return f"{bytes_used / _GIB:.1f} / {cap_gb} GB"


class QuotaService:
    def __init__(
        self,
        preferences: "PreferencesService",
        user_quotas: "UserQuotaStore",
        request_history: "RequestHistoryStore",
        download_store: "DownloadStore",
        library_db: "LibraryDB",
        auth_store: "AuthStore",
    ) -> None:
        self._preferences = preferences
        self._user_quotas = user_quotas
        self._request_history = request_history
        self._download_store = download_store
        self._library_db = library_db
        self._auth = auth_store

    async def effective_quota(self, user_id: str) -> EffectiveQuota:
        policy = self._preferences.get_download_policy()
        override = await self._user_quotas.get(user_id)
        return EffectiveQuota(
            request_quota_count=(
                override.request_quota_count
                if override is not None and override.request_quota_count is not None
                else policy.default_request_quota_count
            ),
            request_quota_days=(
                override.request_quota_days
                if override is not None and override.request_quota_days is not None
                else policy.default_request_quota_days
            ),
            storage_quota_gb=(
                override.storage_quota_gb
                if override is not None and override.storage_quota_gb is not None
                else policy.default_storage_quota_gb
            ),
        )

    async def count_requests_in_window(self, user_id: str, window_days: int) -> int:
        """Asks in the rolling window: album asks from request_history (ISO
        timestamps) + track asks from download_tasks (epoch floats) - the two
        halves keep their native timestamp formats."""
        now = datetime.now(timezone.utc)
        since_iso = (now - timedelta(days=window_days)).isoformat()
        since_epoch = time.time() - window_days * 86400
        albums = await self._request_history.async_count_user_requests_since(user_id, since_iso)
        tracks = await self._download_store.count_user_track_requests_since(user_id, since_epoch)
        return albums + tracks

    async def check_request_quota(
        self, user_id: str | None, user_role: str | None, new_requests: int = 1
    ) -> None:
        """Layer 1 (submit-time). Raises ``ValidationError`` when the ask would
        exceed the user's rolling request quota. Admin/trusted are exempt (D8)."""
        if not user_id or user_role in _EXEMPT_ROLES:
            return
        quota = await self.effective_quota(user_id)
        if quota.request_quota_count <= 0:
            return  # 0 = unlimited
        used = await self.count_requests_in_window(user_id, quota.request_quota_days)
        if used + new_requests > quota.request_quota_count:
            raise ValidationError(
                f"Request limit reached ({quota.request_quota_count} per "
                f"{quota.request_quota_days} days - you've used {used})"
            )

    async def check_storage_admission(self, user_id: str, origin: str = "user") -> None:
        """Layer 2 (task-creation time). Global cap first (all roles), then the
        per-user storage quota (role looked up here, admin/trusted exempt).
        ``origin='upgrade'`` skips both (size-neutral: the old file is recycled);
        ``origin='retry'`` skips both too - a retry re-attempts an ask that was
        already admitted at its original submit, and gating it here would make the
        two retry buttons (requests page vs downloads queue) behave differently.
        ``origin='wanted'`` skips both for the same reason as retry: the wanted
        watcher re-dispatches an ask that was already admitted (Wanted D8).
        Raises ``ValidationError`` at/over."""
        if origin in ("upgrade", "retry", "wanted"):
            return
        policy = self._preferences.get_download_policy()
        if policy.max_library_size_gb > 0:
            total = await self._library_db.get_total_library_bytes()
            if total >= policy.max_library_size_gb * _GIB:
                raise ValidationError(
                    "Library storage limit reached "
                    f"({_gb_label(total, policy.max_library_size_gb)})"
                )
        user = await self._auth.get_user_by_id(user_id)
        if user is None or user.role in _EXEMPT_ROLES:
            return
        quota = await self.effective_quota(user_id)
        if quota.storage_quota_gb <= 0:
            return
        used = await self._library_db.get_user_library_bytes(user_id)
        if used >= quota.storage_quota_gb * _GIB:
            raise ValidationError(
                f"Your storage budget is full ({_gb_label(used, quota.storage_quota_gb)})"
            )

    async def usage_for(self, user_id: str, user_role: str | None = None) -> QuotaUsage:
        """One user's standing (drives the SettingsUsers usage bars)."""
        if user_role is None:
            user = await self._auth.get_user_by_id(user_id)
            user_role = user.role if user is not None else None
        quota = await self.effective_quota(user_id)
        window = quota.request_quota_days
        return QuotaUsage(
            user_id=user_id,
            quota=quota,
            requests_in_window=await self.count_requests_in_window(user_id, window),
            storage_bytes=await self._library_db.get_user_library_bytes(user_id),
            exempt=user_role in _EXEMPT_ROLES,
        )

    async def get_override(self, user_id: str):
        """The user's raw override row (None fields inherit), or None if none set."""
        return await self._user_quotas.get(user_id)

    async def set_override(
        self,
        user_id: str,
        *,
        request_quota_count: int | None,
        request_quota_days: int | None,
        storage_quota_gb: int | None,
    ) -> None:
        for name, value, low in (
            ("request_quota_count", request_quota_count, 0),
            ("request_quota_days", request_quota_days, 1),
            ("storage_quota_gb", storage_quota_gb, 0),
        ):
            if value is not None and not (low <= value <= 1_000_000):
                raise ValidationError(f"{name} must be between {low} and 1000000")
        await self._user_quotas.set(
            user_id,
            request_quota_count=request_quota_count,
            request_quota_days=request_quota_days,
            storage_quota_gb=storage_quota_gb,
        )
