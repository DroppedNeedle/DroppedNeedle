"""FastAPI ``Annotated[..., Depends()]`` type aliases for route handlers."""

from __future__ import annotations

from typing import Annotated

from fastapi import Depends

from core.config import Settings, get_settings
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.persistence.wanted_store import WantedStore
from middleware import CurrentUserDep as CurrentUserDep, CurrentAdminDep as CurrentAdminDep, CurrentTokenDep as CurrentTokenDep
from repositories.protocols import LibraryRepositoryProtocol
from repositories.musicbrainz_repository import MusicBrainzRepository
from repositories.wikidata_repository import WikidataRepository
from repositories.listenbrainz_repository import ListenBrainzRepository
from repositories.jellyfin_repository import JellyfinRepository
from repositories.coverart_repository import CoverArtRepository
from repositories.youtube import YouTubeRepository
from repositories.lastfm_repository import LastFmRepository
from repositories.playlist_repository import PlaylistRepository
from repositories.navidrome_repository import NavidromeRepository
from repositories.plex_repository import PlexRepository
from repositories.github_repository import GitHubRepository
from services.preferences_service import PreferencesService
from services.search_service import SearchService
from services.search_enrichment_service import SearchEnrichmentService
from services.artist_service import ArtistService
from services.album_service import AlbumService
from services.request_service import RequestService
from services.library_service import LibraryService
from services.status_service import StatusService
from services.cache_service import CacheService
from services.home_service import HomeService
from services.home_charts_service import HomeChartsService
from services.settings_service import SettingsService
from services.artist_discovery_service import ArtistDiscoveryService
from services.album_discovery_service import AlbumDiscoveryService
from services.discover_service import DiscoverService
from services.discover_queue_manager import DiscoverQueueManager
from services.youtube_service import YouTubeService
from services.requests_page_service import RequestsPageService
from services.jellyfin_playback_service import JellyfinPlaybackService
from services.local_files_service import LocalFilesService
from services.jellyfin_library_service import JellyfinLibraryService
from services.navidrome_library_service import NavidromeLibraryService
from services.navidrome_playback_service import NavidromePlaybackService
from services.plex_library_service import PlexLibraryService
from services.plex_playback_service import PlexPlaybackService
from services.playlist_service import PlaylistService
from services.lastfm_auth_service import LastFmAuthService
from services.scrobble_service import ScrobbleService
from services.cache_status_service import CacheStatusService
from services.version_service import VersionService

from .cache_providers import (
    get_cache,
    get_disk_cache,
    get_preferences_service,
    get_cache_service,
    get_cache_status_service,
)
from .repo_providers import (
    get_library_repository,
    get_musicbrainz_repository,
    get_wikidata_repository,
    get_listenbrainz_repository,
    get_jellyfin_repository,
    get_coverart_repository,
    get_youtube_repo,
    get_lastfm_repository,
    get_playlist_repository,
    get_request_history_store,
    get_wanted_store,
    get_navidrome_repository,
    get_plex_repository,
    get_github_repository,
)
from .service_providers import (
    get_search_service,
    get_search_enrichment_service,
    get_artist_service,
    get_album_service,
    get_request_service,
    get_requests_page_service,
    get_playlist_service,
    get_library_service,
    get_status_service,
    get_home_service,
    get_home_charts_service,
    get_settings_service,
    get_artist_discovery_service,
    get_album_discovery_service,
    get_discover_service,
    get_discover_queue_manager,
    get_youtube_service,
    get_lastfm_auth_service,
    get_scrobble_service,
    get_jellyfin_playback_service,
    get_local_files_service,
    get_jellyfin_library_service,
    get_navidrome_library_service,
    get_navidrome_playback_service,
    get_plex_library_service,
    get_plex_playback_service,
    get_version_service,
)


SettingsDep = Annotated[Settings, Depends(get_settings)]
CacheDep = Annotated[CacheInterface, Depends(get_cache)]
DiskCacheDep = Annotated[DiskMetadataCache, Depends(get_disk_cache)]
PreferencesServiceDep = Annotated[PreferencesService, Depends(get_preferences_service)]
LibraryRepositoryDep = Annotated[LibraryRepositoryProtocol, Depends(get_library_repository)]
MusicBrainzRepositoryDep = Annotated[MusicBrainzRepository, Depends(get_musicbrainz_repository)]
WikidataRepositoryDep = Annotated[WikidataRepository, Depends(get_wikidata_repository)]
ListenBrainzRepositoryDep = Annotated[ListenBrainzRepository, Depends(get_listenbrainz_repository)]
JellyfinRepositoryDep = Annotated[JellyfinRepository, Depends(get_jellyfin_repository)]
CoverArtRepositoryDep = Annotated[CoverArtRepository, Depends(get_coverart_repository)]
SearchServiceDep = Annotated[SearchService, Depends(get_search_service)]
SearchEnrichmentServiceDep = Annotated[SearchEnrichmentService, Depends(get_search_enrichment_service)]
ArtistServiceDep = Annotated[ArtistService, Depends(get_artist_service)]
AlbumServiceDep = Annotated[AlbumService, Depends(get_album_service)]
RequestServiceDep = Annotated[RequestService, Depends(get_request_service)]
LibraryServiceDep = Annotated[LibraryService, Depends(get_library_service)]
StatusServiceDep = Annotated[StatusService, Depends(get_status_service)]
CacheServiceDep = Annotated[CacheService, Depends(get_cache_service)]
HomeServiceDep = Annotated[HomeService, Depends(get_home_service)]
HomeChartsServiceDep = Annotated[HomeChartsService, Depends(get_home_charts_service)]
SettingsServiceDep = Annotated[SettingsService, Depends(get_settings_service)]
ArtistDiscoveryServiceDep = Annotated[ArtistDiscoveryService, Depends(get_artist_discovery_service)]
AlbumDiscoveryServiceDep = Annotated[AlbumDiscoveryService, Depends(get_album_discovery_service)]
DiscoverServiceDep = Annotated[DiscoverService, Depends(get_discover_service)]
DiscoverQueueManagerDep = Annotated[DiscoverQueueManager, Depends(get_discover_queue_manager)]
YouTubeRepositoryDep = Annotated[YouTubeRepository, Depends(get_youtube_repo)]
YouTubeServiceDep = Annotated[YouTubeService, Depends(get_youtube_service)]
RequestHistoryStoreDep = Annotated[RequestHistoryStore, Depends(get_request_history_store)]
WantedStoreDep = Annotated[WantedStore, Depends(get_wanted_store)]
RequestsPageServiceDep = Annotated[RequestsPageService, Depends(get_requests_page_service)]
JellyfinPlaybackServiceDep = Annotated[JellyfinPlaybackService, Depends(get_jellyfin_playback_service)]
LocalFilesServiceDep = Annotated[LocalFilesService, Depends(get_local_files_service)]
JellyfinLibraryServiceDep = Annotated[JellyfinLibraryService, Depends(get_jellyfin_library_service)]
LastFmRepositoryDep = Annotated[LastFmRepository, Depends(get_lastfm_repository)]
LastFmAuthServiceDep = Annotated[LastFmAuthService, Depends(get_lastfm_auth_service)]
ScrobbleServiceDep = Annotated[ScrobbleService, Depends(get_scrobble_service)]
PlaylistRepositoryDep = Annotated[PlaylistRepository, Depends(get_playlist_repository)]
PlaylistServiceDep = Annotated[PlaylistService, Depends(get_playlist_service)]
NavidromeRepositoryDep = Annotated[NavidromeRepository, Depends(get_navidrome_repository)]
NavidromeLibraryServiceDep = Annotated[NavidromeLibraryService, Depends(get_navidrome_library_service)]
NavidromePlaybackServiceDep = Annotated[NavidromePlaybackService, Depends(get_navidrome_playback_service)]
PlexRepositoryDep = Annotated[PlexRepository, Depends(get_plex_repository)]
PlexLibraryServiceDep = Annotated[PlexLibraryService, Depends(get_plex_library_service)]
PlexPlaybackServiceDep = Annotated[PlexPlaybackService, Depends(get_plex_playback_service)]
CacheStatusServiceDep = Annotated[CacheStatusService, Depends(get_cache_status_service)]
GitHubRepositoryDep = Annotated[GitHubRepository, Depends(get_github_repository)]
VersionServiceDep = Annotated[VersionService, Depends(get_version_service)]

# Import backend providers at the end to avoid circular dependencies
from .backend_providers import get_request_backend_service
from services.request_backend_service import RequestBackendService

RequestBackendServiceDep = Annotated[RequestBackendService, Depends(get_request_backend_service)]
