from typing import Any, Protocol

from infrastructure.queue.priority_queue import RequestPriority
from models.search import SearchResult
from models.album import AlbumInfo
from repositories.musicbrainz_base import MbSourceContext
from models.library_contribution import (
    MusicBrainzDuplicateFacts,
    MusicBrainzUrlResolution,
    MusicBrainzVerifiedRelease,
)


class MusicBrainzRepositoryProtocol(Protocol):
    async def search_artists(
        self, query: str, limit: int = 10, included_types: set[str] | None = None
    ) -> list[SearchResult]: ...

    async def search_albums(
        self,
        query: str,
        limit: int = 10,
        included_types: set[str] | None = None,
        included_secondary_types: set[str] | None = None,
        included_statuses: set[str] | None = None,
    ) -> list[SearchResult]: ...

    async def search_release_groups(
        self,
        artist: str,
        title: str,
        limit: int = 10,
        offset: int = 0,
        included_secondary_types: set[str] | None = None,
        include_all_types: bool = False,
        included_primary_types: set[str] | None = None,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
    ) -> list[SearchResult]: ...
    # A2: explicit priority threading - the discover-queue enrichment leg
    # passes BACKGROUND_SYNC; every other caller keeps the USER_INITIATED
    # default. NOTE: protocol modules must not use `from __future__ import
    # annotations` (signature-conformance tests compare real objects).
    async def get_artist_by_id(
        self,
        mbid: str,
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        *,
        include_releases: bool = True,
        release_group_limit: int = 50,
    ) -> dict[str, Any] | None: ...

    async def get_artist_release_groups_with_context(
        self,
        artist_mbid: str,
        offset: int = 0,
        limit: int = 50,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        *,
        preserve_fetch_width: bool = False,
        source_context: MbSourceContext | None = None,
    ) -> tuple[list[dict[str, Any]], int, MbSourceContext | None]: ...

    async def get_release_group(self, release_group_mbid: str) -> AlbumInfo | None: ...

    async def get_release(self, release_mbid: str) -> Any | None: ...

    async def get_release_group_id_from_release(
        self,
        release_mbid: str,
        *,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        source_context: MbSourceContext | None = None,
    ) -> str | None: ...

    async def get_release_groups_by_artist(
        self, artist_mbid: str, limit: int = 10
    ) -> list[dict[str, Any]]: ...

    async def get_recording_position_on_release(
        self,
        release_id: str,
        recording_mbid: str,
    ) -> tuple[int, int] | None: ...

    async def resolve_url(
        self,
        resource_url: str,
        *,
        includes: tuple[str, ...],
        priority: RequestPriority,
        bypass_cache: bool = False,
    ) -> MusicBrainzUrlResolution: ...

    async def get_release_for_verification(
        self,
        release_mbid: str,
        *,
        priority: RequestPriority,
        bypass_cache: bool = False,
    ) -> MusicBrainzVerifiedRelease | None: ...

    async def search_duplicate_releases(
        self,
        facts: MusicBrainzDuplicateFacts,
        *,
        priority: RequestPriority,
        limit: int,
    ) -> list[MusicBrainzVerifiedRelease]: ...
