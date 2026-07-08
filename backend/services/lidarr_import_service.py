"""Read-only Lidarr → follows importer (LidarrImport).

Turns a user's Lidarr *monitored artists* into DroppedNeedle *follows*, keyed 1:1 on the
MusicBrainz artist MBID (Lidarr's ``foreignArtistId`` == ``user_followed_artists.artist_mbid``).

Design guarantees (see .dev-notes/LidarrImport):
- **0 MusicBrainz calls at import** (DR2): names come from Lidarr, written via the bulk store
  method - never the per-artist ``FollowService.set_followed`` path (which does an MB lookup).
- **Authoritative auto-download** (DR3): eligibility is re-fetched from Lidarr's
  ``monitorNewItems == "all"`` on import, never trusted from the client.
- **Additive & D9-safe** (DR4/DR6): a pre-read of existing follows drives real counts and
  restricts auto-download mirroring to *brand-new* follows, so re-import never re-arms or
  downgrades an already-followed artist.
"""

import logging

from api.v1.schemas.lidarr_import import (
    LidarrArtistCandidate,
    LidarrArtistListResponse,
    LidarrImportResponse,
)
from core.exceptions import ConfigurationError
from infrastructure.persistence.follow_store import FollowStore
from infrastructure.validators import is_valid_mbid
from repositories.lidarr_import import LidarrImportRepository
from services.follow_service import FollowService
from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)


class LidarrImportService:
    def __init__(
        self,
        repo: LidarrImportRepository,
        preferences: PreferencesService,
        follow_store: FollowStore,
        follow_service: FollowService,
    ) -> None:
        self._repo = repo
        self._prefs = preferences
        self._follow_store = follow_store
        self._follow_service = follow_service

    async def _monitored_artists(self):
        """Re-fetch Lidarr and keep only monitored artists with a valid MBID. Raises
        ``ConfigurationError`` (400, message reaches the user) when Lidarr isn't connected."""
        conn = self._prefs.get_lidarr_import_connection_raw()
        if not (conn.url and conn.api_key):
            raise ConfigurationError(
                "Lidarr is not connected. An admin must configure it in Settings first."
            )
        artists = await self._repo.list_artists(conn.url, conn.api_key)
        return [
            a for a in artists if a.monitored and is_valid_mbid(a.foreign_artist_id)
        ]

    async def list_import_candidates(self, user_id: str) -> LidarrArtistListResponse:
        monitored = await self._monitored_artists()
        existing = await self._follow_store.existing_followed_lower(
            user_id, [a.foreign_artist_id.lower() for a in monitored]
        )
        candidates = [
            LidarrArtistCandidate(
                mbid=a.foreign_artist_id,
                name=a.artist_name,
                monitor_new_items=a.monitor_new_items,
                already_following=a.foreign_artist_id.lower() in existing,
                would_auto_download=(a.monitor_new_items == "all"),
            )
            for a in monitored
        ]
        return LidarrArtistListResponse(artists=candidates, total=len(candidates))

    async def import_artists(
        self, user_id: str, role: str, selected_mbids: list[str]
    ) -> LidarrImportResponse:
        monitored = await self._monitored_artists()
        # Authoritative map from the re-fetch (DR3): a client-supplied MBID not currently
        # monitored in Lidarr is silently ignored.
        by_lower = {a.foreign_artist_id.lower(): a for a in monitored}

        selected_valid_lower: list[str] = []
        skipped_invalid = 0
        seen: set[str] = set()
        for mbid in selected_mbids:
            if not is_valid_mbid(mbid):
                skipped_invalid += 1
                continue
            low = mbid.strip().lower()
            if low in seen:
                continue
            seen.add(low)
            if low in by_lower:
                selected_valid_lower.append(low)

        # Pre-read existing follows BEFORE any write (DR6): the bulk UPSERT can't tell a fresh
        # insert from a conflict, so both the counts and the D9 rule need the prior state.
        existing = await self._follow_store.existing_followed_lower(
            user_id, selected_valid_lower
        )
        new_lowers = [low for low in selected_valid_lower if low not in existing]
        imported = len(new_lowers)
        already_following = len(selected_valid_lower) - imported
        # D9: mirror auto-download ONLY for brand-new follows monitored as "all".
        auto_dl_lowers = [
            low for low in new_lowers if by_lower[low].monitor_new_items == "all"
        ]

        # Writes - ORDER MATTERS (3 separate transactions; ordered so a mid-sequence crash
        # fails SAFE, leaving auto_download=0 rather than intent-on-without-approval).
        # (a) bulk follow the whole valid selection; already-followed rows are idempotent
        #     no-ops that preserve auto_download + followed_at (DR4).
        await self._follow_store.follow_artists_bulk(
            user_id,
            [
                (by_lower[low].foreign_artist_id, by_lower[low].artist_name)
                for low in selected_valid_lower
            ],
        )
        # (b) approvals for the auto-download subset, BEFORE flipping intent.
        approval_batch_id: str | None = None
        if auto_dl_lowers and role != "admin":
            approval_batch_id = await self._follow_service.create_import_batch(
                user_id,
                [
                    (by_lower[low].foreign_artist_id, by_lower[low].artist_name)
                    for low in auto_dl_lowers
                ],
            )
        # (c) flip auto-download intent on for the auto-download subset.
        if auto_dl_lowers:
            await self._follow_store.set_auto_download_intent_bulk(
                user_id,
                [by_lower[low].foreign_artist_id for low in auto_dl_lowers],
                True,
            )

        logger.info(
            "Lidarr import for user %s: imported=%d already_following=%d skipped_invalid=%d "
            "auto_download=%d batch=%s",
            user_id,
            imported,
            already_following,
            skipped_invalid,
            len(auto_dl_lowers),
            approval_batch_id or "-",
        )
        return LidarrImportResponse(
            imported=imported,
            already_following=already_following,
            skipped_invalid=skipped_invalid,
            auto_download_enabled=len(auto_dl_lowers),
            approval_batch_id=approval_batch_id,
        )
