"""Follow + auto-download business logic.

Admins are implicitly approved with no approval row (DD3); a non-admin enabling
auto-download enters a pending standing approval. The admin display override
lives here because the store is role-agnostic.
"""

import logging
import uuid

import msgspec

from infrastructure.persistence.follow_store import (
    Approval,
    ApprovalBatch,
    FollowedArtist,
    FollowState,
    FollowStore,
    NewRelease,
)

logger = logging.getLogger(__name__)

_UNKNOWN_ARTIST = "Unknown Artist"


class FollowError(ValueError):
    """A follow business-rule violation that maps to HTTP 400."""


class FollowService:
    def __init__(self, follow_store: FollowStore, mb_repo) -> None:
        self._store = follow_store
        self._mb_repo = mb_repo

    async def _resolve_artist_name(self, artist_mbid: str) -> str:
        # fall back to a placeholder rather than failing the follow if MB is down
        artist = await self._mb_repo.get_artist_by_id(artist_mbid)
        if artist and artist.get("name"):
            return artist["name"]
        return _UNKNOWN_ARTIST

    @staticmethod
    def _apply_admin_override(role: str, state: FollowState) -> FollowState:
        # admins are approved by role and carry no approval row, so an admin
        # with auto-download on always reads 'approved'
        if role == "admin" and state.auto_download and state.auto_download_state != "approved":
            return msgspec.structs.replace(state, auto_download_state="approved")
        return state

    async def get_status(self, user_id: str, role: str, artist_mbid: str) -> FollowState:
        state = await self._store.get_follow_state(user_id, artist_mbid)
        return self._apply_admin_override(role, state)

    async def set_followed(
        self, user_id: str, role: str, artist_mbid: str, followed: bool
    ) -> FollowState:
        if followed:
            name = await self._resolve_artist_name(artist_mbid)
            await self._store.follow_artist(user_id, artist_mbid, name)
        else:
            # approval row is left intact (L4)
            await self._store.unfollow_artist(user_id, artist_mbid)
        return await self.get_status(user_id, role, artist_mbid)

    async def set_auto_download(
        self, user_id: str, role: str, artist_mbid: str, enabled: bool
    ) -> FollowState:
        state = await self._store.get_follow_state(user_id, artist_mbid)
        if not state.followed:
            raise FollowError("You must follow this artist before enabling auto-download.")
        if enabled:
            await self._store.set_auto_download_intent(user_id, artist_mbid, True)
            if role != "admin":
                # non-admins need a standing approval; admins approved by role (DD3)
                name = await self._resolve_artist_name(artist_mbid)
                await self._store.upsert_approval(user_id, artist_mbid, name, "pending")
        else:
            # leave the approval row so re-enabling keeps an existing grant
            await self._store.set_auto_download_intent(user_id, artist_mbid, False)
        return await self.get_status(user_id, role, artist_mbid)

    async def list_following(self, user_id: str, role: str) -> list[FollowedArtist]:
        artists = await self._store.list_followed_artists(user_id)
        if role != "admin":
            return artists
        return [
            msgspec.structs.replace(a, auto_download_state="approved")
            if a.auto_download and a.auto_download_state != "approved"
            else a
            for a in artists
        ]

    async def list_new_releases(
        self, user_id: str, limit: int, offset: int
    ) -> tuple[list[NewRelease], int]:
        return await self._store.list_new_releases_for_user(user_id, limit, offset)

    async def list_recent_releases(
        self, user_id: str, days: int, limit: int, include_owned: bool = True
    ) -> tuple[list[NewRelease], int]:
        """The release-log view (hub + New Releases page): everything from the
        window, owned albums flagged - or filtered out when include_owned=False."""
        return await self._store.list_recent_releases_for_user(
            user_id, days, limit, include_owned
        )

    async def count_unseen_new_releases(self, user_id: str) -> int:
        return await self._store.count_unseen_new_releases_for_user(user_id)

    async def mark_new_releases_seen(self, user_id: str) -> None:
        await self._store.mark_new_releases_seen(user_id)

    async def list_pending_approvals(self) -> list[Approval]:
        return await self._store.list_pending_approvals()

    async def approve(
        self, user_id: str, artist_mbid: str, reviewer: tuple[str, str | None]
    ) -> bool:
        return await self._store.set_approval_state(user_id, artist_mbid, "approved", reviewer)

    async def reject(
        self, user_id: str, artist_mbid: str, reviewer: tuple[str, str | None]
    ) -> bool:
        # deny and flip intent off, keeping the follow (L4)
        updated = await self._store.set_approval_state(user_id, artist_mbid, "rejected", reviewer)
        if updated:
            await self._store.set_auto_download_intent(user_id, artist_mbid, False)
        return updated

    async def revoke(
        self, user_id: str, artist_mbid: str, reviewer: tuple[str, str | None]
    ) -> bool:
        # revoke a prior grant and flip intent off, keeping the follow (L4)
        updated = await self._store.set_approval_state(user_id, artist_mbid, "revoked", reviewer)
        if updated:
            await self._store.set_auto_download_intent(user_id, artist_mbid, False)
        return updated

    # --- Bulk "Lidarr Import" approval batches (LidarrImport D3) ------------------

    async def create_import_batch(
        self, user_id: str, artists: list[tuple[str, str]]
    ) -> str:
        """Create one grouped pending approval batch over ``artists`` = [(mbid, name)] and
        return its ``batch_id``. Called by LidarrImportService for non-admin importers."""
        batch_id = uuid.uuid4().hex
        await self._store.create_import_approval_batch(user_id, artists, batch_id)
        return batch_id

    async def list_pending_batches(self) -> list[ApprovalBatch]:
        return await self._store.list_pending_approval_batches()

    async def approve_batch(
        self, batch_id: str, reviewer: tuple[str, str | None]
    ) -> int:
        return await self._store.set_batch_approval_state(batch_id, "approved", reviewer)

    async def reject_batch(
        self, batch_id: str, reviewer: tuple[str, str | None]
    ) -> int:
        return await self._store.set_batch_approval_state(batch_id, "rejected", reviewer)
