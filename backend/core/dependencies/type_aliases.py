"""FastAPI ``Annotated[..., Depends()]`` type aliases for route handlers."""

from __future__ import annotations

import asyncio
from collections.abc import Callable
from typing import Annotated

from fastapi import Depends

from core.config import Settings, get_settings
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.disk_cache import DiskMetadataCache
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.persistence.wanted_store import WantedStore
from middleware import (
    CurrentUserDep as CurrentUserDep,
    CurrentAdminDep as CurrentAdminDep,
    CurrentTokenDep as CurrentTokenDep,
)
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
from services.native.library_policy_service import LibraryPolicyService
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.library_management_preview_service import (
    LibraryManagementPreviewService,
)
from services.native.library_management_undo_service import LibraryManagementUndoService
from services.native.library_management_baseline_service import (
    LibraryManagementBaselineService,
)
from services.native.library_management_duplicate_service import (
    LibraryManagementDuplicateService,
)
from services.native.library_management_recovery_service import (
    LibraryManagementRecoveryService,
)
from services.native.canonical_release_metadata_service import (
    CanonicalReleaseMetadataService,
)
from services.native.effective_metadata_projection_service import (
    EffectiveMetadataProjectionService,
)
from services.native.library_management_override_service import (
    LibraryManagementOverrideService,
)
from services.native.genre_projection_service import GenreProjectionService
from services.native.artwork_projection_service import ArtworkProjectionService
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.library_policy_reconciliation_service import (
    LibraryPolicyReconciliationService,
)
from services.native.target_library_policy_service import TargetLibraryPolicyService
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.library_scan_coordinator import LibraryScanCoordinator
from services.native.library_ownership_service import LibraryOwnershipService
from services.native.identification_queue_service import IdentificationQueueService
from services.native.library_administrative_work_service import (
    LibraryAdministrativeWorkService,
)
from services.native.album_coverage_service import AlbumCoverageService
from services.native.album_identification_service import AlbumIdentificationService
from services.native.reidentification_service import ReidentificationService
from services.native.album_edition_finder_service import AlbumEditionFinderService
from services.native.library_review_service import LibraryReviewService
from services.native.library_operation_service import LibraryOperationService
from services.native.catalog_correction_service import CatalogCorrectionService
from services.native.identity_repair_service import IdentityRepairService
from services.native.artist_identity_reconciliation_service import (
    ArtistIdentityReconciliationService,
)
from services.native.library_diagnostics_service import LibraryDiagnosticsService
from services.native.explicit_reidentification_worker import (
    ExplicitReidentificationWorker,
)
from services.native.target_native_library_service import TargetNativeLibraryService
from services.native.edition_conversion_service import EditionConversionService
from services.native.library_contribution_service import LibraryContributionService
from services.native.target_catalog_writer_service import TargetCatalogWriterService
from services.native.wanted_watcher_service import WantedWatcherService
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
from services.compat.native_lyrics_service import NativeLyricsService
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
from services.home.cached_local_artwork_service import CachedLocalArtworkService

from .cache_providers import (
    get_cache,
    get_disk_cache,
    get_native_library_store,
    get_library_management_blob_store,
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
    get_library_policy_service,
    get_target_library_policy_reconciliation_service,
    get_legacy_pending_migration_service,
    get_library_management_profile_service,
    get_library_management_preview_service,
    get_library_management_undo_service,
    get_library_management_baseline_service,
    get_library_management_duplicate_service,
    get_library_management_recovery_service,
    get_canonical_release_metadata_service,
    get_effective_metadata_projection_service,
    get_library_management_override_service,
    get_genre_projection_service,
    get_artwork_projection_service,
    get_audio_write_planning_service,
    get_target_library_policy_service,
    get_library_policy_resolver,
    get_target_library_scan_coordinator,
    get_target_library_ownership_service,
    get_target_identification_queue,
    get_library_administrative_work_service,
    get_target_album_coverage_service,
    get_target_album_identification_service,
    get_mb_provider_availability,
    get_target_reidentification_service,
    get_target_album_edition_finder_service,
    get_target_library_review_service,
    get_target_library_operation_service,
    get_target_catalog_correction_service,
    get_target_identity_repair_service,
    get_artist_identity_reconciliation_service,
    get_target_library_diagnostics_service,
    get_target_explicit_reidentification_worker,
    get_target_native_library_service,
    get_edition_conversion_service,
    get_library_contribution_service,
    get_target_catalog_writer_service,
    get_wanted_watcher_service,
    get_cached_local_artwork_service,
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
    get_release_type_policy_transition_lock,
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
from .compat_providers import get_native_lyrics_service


SettingsDep = Annotated[Settings, Depends(get_settings)]
CacheDep = Annotated[CacheInterface, Depends(get_cache)]
DiskCacheDep = Annotated[DiskMetadataCache, Depends(get_disk_cache)]
NativeLibraryStoreDep = Annotated[NativeLibraryStore, Depends(get_native_library_store)]
LibraryManagementBlobStoreDep = Annotated[
    LibraryManagementBlobStore, Depends(get_library_management_blob_store)
]
CachedLocalArtworkServiceDep = Annotated[
    CachedLocalArtworkService, Depends(get_cached_local_artwork_service)
]
PreferencesServiceDep = Annotated[PreferencesService, Depends(get_preferences_service)]
ReleaseTypePolicyTransitionLockDep = Annotated[
    asyncio.Lock, Depends(get_release_type_policy_transition_lock)
]
LibraryPolicyServiceDep = Annotated[
    LibraryPolicyService, Depends(get_library_policy_service)
]
LegacyPendingMigrationServiceDep = Annotated[
    "LegacyPendingMigrationService",
    Depends(get_legacy_pending_migration_service),
]
LibraryManagementProfileServiceDep = Annotated[
    LibraryManagementProfileService,
    Depends(get_library_management_profile_service),
]
LibraryManagementPreviewServiceDep = Annotated[
    LibraryManagementPreviewService,
    Depends(get_library_management_preview_service),
]
LibraryManagementUndoServiceDep = Annotated[
    LibraryManagementUndoService,
    Depends(get_library_management_undo_service),
]
LibraryManagementBaselineServiceDep = Annotated[
    LibraryManagementBaselineService,
    Depends(get_library_management_baseline_service),
]
LibraryManagementDuplicateServiceDep = Annotated[
    LibraryManagementDuplicateService,
    Depends(get_library_management_duplicate_service),
]
LibraryManagementRecoveryServiceDep = Annotated[
    LibraryManagementRecoveryService,
    Depends(get_library_management_recovery_service),
]
CanonicalReleaseMetadataServiceDep = Annotated[
    CanonicalReleaseMetadataService,
    Depends(get_canonical_release_metadata_service),
]
EffectiveMetadataProjectionServiceDep = Annotated[
    EffectiveMetadataProjectionService,
    Depends(get_effective_metadata_projection_service),
]
LibraryManagementOverrideServiceDep = Annotated[
    LibraryManagementOverrideService,
    Depends(get_library_management_override_service),
]
GenreProjectionServiceDep = Annotated[
    GenreProjectionService,
    Depends(get_genre_projection_service),
]
ArtworkProjectionServiceDep = Annotated[
    ArtworkProjectionService,
    Depends(get_artwork_projection_service),
]
AudioWritePlanningServiceDep = Annotated[
    AudioWritePlanningService,
    Depends(get_audio_write_planning_service),
]
TargetLibraryPolicyServiceDep = Annotated[
    TargetLibraryPolicyService, Depends(get_target_library_policy_service)
]
LibraryPolicyResolverDep = Annotated[
    LibraryPolicyResolver, Depends(get_library_policy_resolver)
]
TargetLibraryScanCoordinatorDep = Annotated[
    LibraryScanCoordinator, Depends(get_target_library_scan_coordinator)
]
LibraryPolicyReconciliationServiceDep = Annotated[
    LibraryPolicyReconciliationService,
    Depends(get_target_library_policy_reconciliation_service),
]
TargetLibraryOwnershipServiceDep = Annotated[
    LibraryOwnershipService, Depends(get_target_library_ownership_service)
]
TargetIdentificationQueueDep = Annotated[
    IdentificationQueueService, Depends(get_target_identification_queue)
]
LibraryAdministrativeWorkServiceDep = Annotated[
    LibraryAdministrativeWorkService,
    Depends(get_library_administrative_work_service),
]
TargetAlbumIdentificationServiceDep = Annotated[
    AlbumIdentificationService, Depends(get_target_album_identification_service)
]
MbProviderAvailabilityDep = Annotated[
    Callable[[], bool], Depends(get_mb_provider_availability)
]
TargetAlbumCoverageServiceDep = Annotated[
    AlbumCoverageService, Depends(get_target_album_coverage_service)
]
TargetReidentificationServiceDep = Annotated[
    ReidentificationService, Depends(get_target_reidentification_service)
]
TargetAlbumEditionFinderServiceDep = Annotated[
    AlbumEditionFinderService, Depends(get_target_album_edition_finder_service)
]
LibraryReviewServiceDep = Annotated[
    LibraryReviewService, Depends(get_target_library_review_service)
]
LibraryOperationServiceDep = Annotated[
    LibraryOperationService, Depends(get_target_library_operation_service)
]
CatalogCorrectionServiceDep = Annotated[
    CatalogCorrectionService, Depends(get_target_catalog_correction_service)
]
IdentityRepairServiceDep = Annotated[
    IdentityRepairService, Depends(get_target_identity_repair_service)
]
ArtistIdentityReconciliationServiceDep = Annotated[
    ArtistIdentityReconciliationService,
    Depends(get_artist_identity_reconciliation_service),
]
LibraryDiagnosticsServiceDep = Annotated[
    LibraryDiagnosticsService, Depends(get_target_library_diagnostics_service)
]
ExplicitReidentificationWorkerDep = Annotated[
    ExplicitReidentificationWorker,
    Depends(get_target_explicit_reidentification_worker),
]
TargetNativeLibraryServiceDep = Annotated[
    TargetNativeLibraryService, Depends(get_target_native_library_service)
]
EditionConversionServiceDep = Annotated[
    EditionConversionService, Depends(get_edition_conversion_service)
]
LibraryContributionServiceDep = Annotated[
    LibraryContributionService, Depends(get_library_contribution_service)
]
TargetCatalogWriterServiceDep = Annotated[
    TargetCatalogWriterService, Depends(get_target_catalog_writer_service)
]
LibraryRepositoryDep = Annotated[
    LibraryRepositoryProtocol, Depends(get_library_repository)
]
MusicBrainzRepositoryDep = Annotated[
    MusicBrainzRepository, Depends(get_musicbrainz_repository)
]
WikidataRepositoryDep = Annotated[WikidataRepository, Depends(get_wikidata_repository)]
ListenBrainzRepositoryDep = Annotated[
    ListenBrainzRepository, Depends(get_listenbrainz_repository)
]
JellyfinRepositoryDep = Annotated[JellyfinRepository, Depends(get_jellyfin_repository)]
CoverArtRepositoryDep = Annotated[CoverArtRepository, Depends(get_coverart_repository)]
SearchServiceDep = Annotated[SearchService, Depends(get_search_service)]
SearchEnrichmentServiceDep = Annotated[
    SearchEnrichmentService, Depends(get_search_enrichment_service)
]
ArtistServiceDep = Annotated[ArtistService, Depends(get_artist_service)]
AlbumServiceDep = Annotated[AlbumService, Depends(get_album_service)]
RequestServiceDep = Annotated[RequestService, Depends(get_request_service)]
LibraryServiceDep = Annotated[LibraryService, Depends(get_library_service)]
StatusServiceDep = Annotated[StatusService, Depends(get_status_service)]
CacheServiceDep = Annotated[CacheService, Depends(get_cache_service)]
HomeServiceDep = Annotated[HomeService, Depends(get_home_service)]
HomeChartsServiceDep = Annotated[HomeChartsService, Depends(get_home_charts_service)]
SettingsServiceDep = Annotated[SettingsService, Depends(get_settings_service)]
ArtistDiscoveryServiceDep = Annotated[
    ArtistDiscoveryService, Depends(get_artist_discovery_service)
]
AlbumDiscoveryServiceDep = Annotated[
    AlbumDiscoveryService, Depends(get_album_discovery_service)
]
DiscoverServiceDep = Annotated[DiscoverService, Depends(get_discover_service)]
DiscoverQueueManagerDep = Annotated[
    DiscoverQueueManager, Depends(get_discover_queue_manager)
]
YouTubeRepositoryDep = Annotated[YouTubeRepository, Depends(get_youtube_repo)]
YouTubeServiceDep = Annotated[YouTubeService, Depends(get_youtube_service)]
RequestHistoryStoreDep = Annotated[
    RequestHistoryStore, Depends(get_request_history_store)
]
WantedStoreDep = Annotated[WantedStore, Depends(get_wanted_store)]
WantedWatcherServiceDep = Annotated[
    WantedWatcherService, Depends(get_wanted_watcher_service)
]
RequestsPageServiceDep = Annotated[
    RequestsPageService, Depends(get_requests_page_service)
]
JellyfinPlaybackServiceDep = Annotated[
    JellyfinPlaybackService, Depends(get_jellyfin_playback_service)
]
LocalFilesServiceDep = Annotated[LocalFilesService, Depends(get_local_files_service)]
NativeLyricsServiceDep = Annotated[
    NativeLyricsService, Depends(get_native_lyrics_service)
]
JellyfinLibraryServiceDep = Annotated[
    JellyfinLibraryService, Depends(get_jellyfin_library_service)
]
LastFmRepositoryDep = Annotated[LastFmRepository, Depends(get_lastfm_repository)]
LastFmAuthServiceDep = Annotated[LastFmAuthService, Depends(get_lastfm_auth_service)]
ScrobbleServiceDep = Annotated[ScrobbleService, Depends(get_scrobble_service)]
PlaylistRepositoryDep = Annotated[PlaylistRepository, Depends(get_playlist_repository)]
PlaylistServiceDep = Annotated[PlaylistService, Depends(get_playlist_service)]
NavidromeRepositoryDep = Annotated[
    NavidromeRepository, Depends(get_navidrome_repository)
]
NavidromeLibraryServiceDep = Annotated[
    NavidromeLibraryService, Depends(get_navidrome_library_service)
]
NavidromePlaybackServiceDep = Annotated[
    NavidromePlaybackService, Depends(get_navidrome_playback_service)
]
PlexRepositoryDep = Annotated[PlexRepository, Depends(get_plex_repository)]
PlexLibraryServiceDep = Annotated[PlexLibraryService, Depends(get_plex_library_service)]
PlexPlaybackServiceDep = Annotated[
    PlexPlaybackService, Depends(get_plex_playback_service)
]
CacheStatusServiceDep = Annotated[CacheStatusService, Depends(get_cache_status_service)]
GitHubRepositoryDep = Annotated[GitHubRepository, Depends(get_github_repository)]
VersionServiceDep = Annotated[VersionService, Depends(get_version_service)]
