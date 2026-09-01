from typing import Protocol

from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_base import MbSourceContext
from repositories.musicbrainz_management_models import (  # noqa: F401
    MbManagementArtist,
    MbManagementArtistCredit,
    MbManagementRelation,
    MbManagementRelease,
    MbManagementTrack,
)


class CanonicalMusicBrainzRepositoryProtocol(Protocol):
    async def get_canonical_release(
        self,
        release_mbid: str,
        *,
        includes: tuple[str, ...],
        preferred_locales: tuple[str, ...] = (),
        artist_standardization: str = "credited",
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        bypass_cache: bool = False,
        source_context: MbSourceContext | None = None,
    ) -> MbManagementRelease | None: ...

    async def resolve_recording_mbid(
        self,
        recording_mbid: str,
        *,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> str | None: ...
