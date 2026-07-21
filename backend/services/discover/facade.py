"""Thin facade preserving the original DiscoverService public API.

All business logic lives in sub-services under ``services.discover.*``.
This class assembles them and delegates every public method call.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import msgspec.structs

if TYPE_CHECKING:
    from infrastructure.persistence.auth_store import UserRecord
    from infrastructure.persistence.discovery_snapshot_store import (
        DiscoverySnapshotStore,
    )
    from services.home.genre_artwork_service import GenreArtworkService
    from services.native.background_workload_gate import BackgroundWorkloadGate

from fastapi import HTTPException

from api.v1.schemas.discover import (
    DiscoverQueueEnrichment,
    DiscoverQueueResponse,
    DiscoverIgnoredRelease,
    PlaylistSuggestionsRequest,
    PlaylistSuggestionsResponse,
    RadioPlanRequest,
    RadioPlanResponse,
)
from api.v1.schemas.home import DiscoverPreview
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.persistence import LibraryDB, MBIDStore
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    JellyfinRepositoryProtocol,
    LibraryRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
    LastFmRepositoryProtocol,
)
from api.v1.schemas.home import HomeSection
from services.discover.enrichment_service import QueueEnrichmentService
from services.discover.homepage_service import DiscoverHomepageService
from services.discover.integration_helpers import IntegrationHelpers
from services.discover.mbid_resolution_service import MbidResolutionService
from services.discover.queue_service import DiscoverQueueService
from services.discover.radio_plan_service import RadioPlanService
from services.preferences_service import PreferencesService
from services.per_user_client_factory import PerUserClientFactory
from infrastructure.persistence.user_listening_prefs_store import (
    UserListeningPrefsStore,
)


class DiscoverService:
    """Drop-in replacement for the original monolith.

    Constructor signature is identical to the old class so that
    ``dependencies.py`` needs only an import-path change.
    """

    def __init__(
        self,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        jellyfin_repo: JellyfinRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        preferences_service: PreferencesService,
        memory_cache: CacheInterface | None = None,
        library_db: LibraryDB | None = None,
        mbid_store: MBIDStore | None = None,
        wikidata_repo: Any = None,
        lastfm_repo: LastFmRepositoryProtocol | None = None,
        audiodb_image_service: Any = None,
        genre_index: Any = None,
        radio_service: Any = None,
        playlist_service: Any = None,
        client_factory: PerUserClientFactory | None = None,
        listening_prefs_store: UserListeningPrefsStore | None = None,
        follow_service: Any = None,
        cover_repo: Any = None,
        preview_repo: Any = None,
        ownership_service: Any = None,
        genre_artwork_service: "GenreArtworkService | None" = None,
        discovery_snapshot_store: "DiscoverySnapshotStore | None" = None,
        workload_gate: "BackgroundWorkloadGate | None" = None,
    ):
        self._integration = IntegrationHelpers(preferences_service)
        self._client_factory = client_factory
        self._prefs_store = listening_prefs_store
        self._ownership = ownership_service

        self._mbid_resolution = MbidResolutionService(
            musicbrainz_repo=musicbrainz_repo,
            library_repo=library_repo,
            listenbrainz_repo=listenbrainz_repo,
            library_db=library_db,
            mbid_store=mbid_store,
        )

        self._enrichment = QueueEnrichmentService(
            musicbrainz_repo=musicbrainz_repo,
            listenbrainz_repo=listenbrainz_repo,
            preferences_service=preferences_service,
            integration=self._integration,
            memory_cache=memory_cache,
            wikidata_repo=wikidata_repo,
            lastfm_repo=lastfm_repo,
        )

        self._queue = DiscoverQueueService(
            listenbrainz_repo=listenbrainz_repo,
            jellyfin_repo=jellyfin_repo,
            musicbrainz_repo=musicbrainz_repo,
            integration=self._integration,
            mbid_resolution=self._mbid_resolution,
            library_db=library_db,
            mbid_store=mbid_store,
            lastfm_repo=lastfm_repo,
            client_factory=client_factory,
            listening_prefs_store=listening_prefs_store,
            ownership_service=ownership_service,
        )

        self._homepage = DiscoverHomepageService(
            listenbrainz_repo=listenbrainz_repo,
            jellyfin_repo=jellyfin_repo,
            library_repo=library_repo,
            musicbrainz_repo=musicbrainz_repo,
            integration=self._integration,
            mbid_resolution=self._mbid_resolution,
            memory_cache=memory_cache,
            lastfm_repo=lastfm_repo,
            audiodb_image_service=audiodb_image_service,
            genre_index=genre_index,
            mbid_store=mbid_store,
            client_factory=client_factory,
            listening_prefs_store=listening_prefs_store,
            library_db=library_db,
            follow_service=follow_service,
            cover_repo=cover_repo,
            genre_artwork_service=genre_artwork_service,
            snapshot_store=discovery_snapshot_store,
            workload_gate=workload_gate,
            ownership_service=ownership_service,
        )

        self._radio = radio_service
        self._playlist_service = playlist_service
        self._radio_plan = RadioPlanService(
            lb_repo=listenbrainz_repo,
            mb_repo=musicbrainz_repo,
            mbid_svc=self._mbid_resolution,
            library_db=library_db,
            genre_index=genre_index,
            lfm_repo=lastfm_repo,
            preview_repo=preview_repo,
        )

    async def get_discover_data(self, user_id: str):
        response = await self._homepage.get_discover_data(user_id)
        await self._apply_album_ownership(response)
        return response

    async def get_discover_preview(self, user_id: str) -> DiscoverPreview | None:
        return await self._homepage.get_discover_preview(user_id)

    async def refresh_discover_data(self, user_id: str) -> None:
        return await self._homepage.refresh_discover_data(user_id)

    async def warm_cache(self, user_id: str) -> None:
        return await self._homepage.warm_cache(user_id)

    async def peek_freshness(self, user_id: str) -> tuple[bool, bool]:
        return await self._homepage.peek_freshness(user_id)

    async def warm_cache_thorough(self, user_id: str) -> None:
        return await self._homepage.warm_cache_thorough(user_id)

    async def build_discover_data(self, user_id: str):
        response = await self._homepage.build_discover_data(user_id)
        await self._apply_album_ownership(response)
        return response

    async def _apply_album_ownership(self, response: Any) -> None:
        if self._ownership is None or response is None:
            return
        from api.v1.schemas.home import HomeAlbum, HomeSection
        from services.native.library_ownership_service import AlbumOwnershipCandidate

        sections = [
            getattr(response, field.name)
            for field in msgspec.structs.fields(type(response))
            if (value := getattr(response, field.name)) is not None
            if isinstance(value, HomeSection)
        ]
        sections.extend(getattr(response, "daily_mixes", []))
        sections.extend(getattr(response, "radio_sections", []))
        sections.extend(
            item.section for item in getattr(response, "because_you_listen_to", [])
        )
        albums = [
            item
            for section in sections
            for item in getattr(section, "items", [])
            if isinstance(item, HomeAlbum)
        ]
        top_picks = getattr(response, "top_picks", None)
        if top_picks is not None:
            albums.extend(item.album for item in top_picks.items)
        projections = await self._ownership.project_albums(
            [
                AlbumOwnershipCandidate(
                    release_group_mbid=album.mbid,
                    title=album.name,
                    album_artist=album.artist_name or "",
                    year=(
                        int(album.release_date[:4])
                        if album.release_date
                        and len(album.release_date) >= 4
                        and album.release_date[:4].isdigit()
                        else None
                    ),
                )
                for album in albums
            ]
        )
        for album, projection in zip(albums, projections):
            album.in_library = projection.owned

    async def build_radio_plan(
        self, user_id: str, request: RadioPlanRequest
    ) -> RadioPlanResponse:
        return await self._radio_plan.build_plan(user_id, request)

    async def generate_radio(self, request: Any) -> HomeSection:
        if self._radio is None:
            raise HTTPException(status_code=501, detail="Radio service not configured")
        return await self._radio.generate_radio(request)

    async def get_playlist_suggestions(
        self,
        request: PlaylistSuggestionsRequest,
        requesting: UserRecord,
    ) -> PlaylistSuggestionsResponse:
        profile = await self._playlist_service.analyse_playlist_profile(
            request.playlist_id,
            requesting,
        )
        if profile is None:
            raise HTTPException(status_code=404, detail="Playlist not found")
        if not profile.artist_mbids:
            raise HTTPException(
                status_code=422,
                detail="This playlist has no artist data to base suggestions on",
            )
        section = await self._homepage.build_playlist_suggestions(
            requesting.id,
            profile,
            request.count,
            request.source,
        )
        return PlaylistSuggestionsResponse(
            suggestions=section,
            playlist_id=request.playlist_id,
            profile=profile,
        )

    async def build_queue(
        self, user_id: str, count: int | None = None
    ) -> DiscoverQueueResponse:
        return await self._queue.build_queue(user_id, count)

    async def validate_queue_mbids(self, mbids: list[str]) -> list[str]:
        return await self._queue.validate_queue_mbids(mbids)

    async def ignore_release(
        self,
        user_id: str,
        release_group_mbid: str,
        artist_mbid: str,
        release_name: str,
        artist_name: str,
    ) -> None:
        return await self._queue.ignore_release(
            user_id, release_group_mbid, artist_mbid, release_name, artist_name
        )

    async def get_ignored_releases(self, user_id: str) -> list[DiscoverIgnoredRelease]:
        return await self._queue.get_ignored_releases(user_id)

    async def enrich_queue_item(
        self, release_group_mbid: str
    ) -> DiscoverQueueEnrichment:
        return await self._enrichment.enrich_queue_item(release_group_mbid)

    def resolve_source(self, source: str | None) -> str:
        return self._integration.resolve_source(source)
