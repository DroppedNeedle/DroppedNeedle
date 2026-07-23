from __future__ import annotations

import asyncio
import logging

from infrastructure.cache.cache_keys import (
    library_raw_albums_key,
    library_requested_mbids_key,
    HOME_RESPONSE_PREFIX,
    DISCOVER_RESPONSE_PREFIX,
    DAILY_MIX_PREFIX,
    TOP_PICKS_PREFIX,
    ALBUM_INFO_PREFIX,
    ARTIST_INFO_PREFIX,
    LIBRARY_PREFIX,
    LIBRARY_ALBUM_DETAILS_PREFIX,
    library_identification_prefixes,
)
from infrastructure.persistence.request_history import RequestHistoryRecord

from ._registry import singleton
from .cache_providers import (
    get_cache,
    get_disk_cache,
    get_library_db,
    get_genre_index,
    get_youtube_store,
    get_mbid_store,
    get_sync_state_store,
    get_scan_state_store,
    get_discovery_snapshot_store,
    get_preferences_service,
)
from .repo_providers import (
    get_preview_repository,
    get_library_repository,
    get_musicbrainz_repository,
    get_musicbrainz_identification_repository,
    get_wikidata_repository,
    get_listenbrainz_repository,
    get_lrclib_repository,
    get_jellyfin_repository,
    get_navidrome_repository,
    get_plex_repository,
    get_coverart_repository,
    get_target_coverart_repository,
    get_youtube_repo,
    get_audiodb_image_service,
    get_audiodb_browse_queue,
    get_lastfm_repository,
    get_playlist_repository,
    get_request_history_store,
    get_user_connections_store,
    get_user_listening_prefs_store,
    get_play_history_store,
    get_follow_store,
    get_github_repository,
)

logger = logging.getLogger(__name__)


@singleton
def get_background_workload_gate() -> "BackgroundWorkloadGate":
    from services.native.background_workload_gate import BackgroundWorkloadGate

    return BackgroundWorkloadGate()


@singleton
def get_library_filesystem_coordinator() -> "LibraryFilesystemCoordinator":
    from services.native.library_filesystem_coordinator import (
        LibraryFilesystemCoordinator,
    )

    return LibraryFilesystemCoordinator()


@singleton
def get_cached_local_artwork_service() -> "CachedLocalArtworkService":
    from core.config import get_settings
    from services.home.cached_local_artwork_service import CachedLocalArtworkService

    from .cache_providers import get_native_library_store

    return CachedLocalArtworkService(
        get_native_library_store(), get_settings().cache_dir / "covers"
    )


@singleton
def get_genre_artwork_service() -> "GenreArtworkService":
    from core.config import get_settings
    from services.home.genre_artwork_service import GenreArtworkService

    from .cache_providers import get_native_library_store

    return GenreArtworkService(
        get_native_library_store(),
        get_cache(),
        get_cached_local_artwork_service(),
        get_settings().cache_dir / "genre_sections",
    )


@singleton
def get_target_library_repository() -> "TargetLibraryRepository":
    from services.native.target_library_repository import TargetLibraryRepository

    from .cache_providers import get_native_library_store
    from .repo_providers import get_request_history_store

    return TargetLibraryRepository(
        get_native_library_store(), get_request_history_store()
    )


@singleton
def get_target_genre_index() -> "TargetGenreIndex":
    from services.native.target_reference_adapters import TargetGenreIndex

    from .cache_providers import get_native_library_store

    return TargetGenreIndex(get_native_library_store())


@singleton
def get_target_album_release_pin_store() -> "TargetAlbumReleasePinStore":
    from services.native.target_reference_adapters import TargetAlbumReleasePinStore

    from .cache_providers import get_native_library_store

    return TargetAlbumReleasePinStore(get_native_library_store())


@singleton
def get_target_library_ownership_service() -> "LibraryOwnershipService":
    from services.native.library_ownership_service import LibraryOwnershipService

    from .cache_providers import get_native_library_store

    return LibraryOwnershipService(get_native_library_store())


@singleton
def get_target_native_library_service() -> "TargetNativeLibraryService":
    from services.native.target_native_library_service import TargetNativeLibraryService

    from .cache_providers import get_native_library_store

    return TargetNativeLibraryService(get_native_library_store())


@singleton
def get_library_contribution_service() -> "LibraryContributionService":
    from services.native.library_contribution_service import LibraryContributionService

    from .cache_providers import get_cache, get_native_library_store
    from .repo_providers import get_discogs_repository, get_musicbrainz_repository

    return LibraryContributionService(
        get_native_library_store(),
        discogs_repository=get_discogs_repository(),
        musicbrainz_repository=get_musicbrainz_repository(),
        cache=get_cache(),
    )


@singleton
def get_library_contribution_verification_worker() -> (
    "LibraryContributionVerificationWorker"
):
    from services.native.library_contribution_verification_worker import (
        LibraryContributionVerificationWorker,
    )

    from .cache_providers import get_native_library_store
    from .repo_providers import get_musicbrainz_repository

    return LibraryContributionVerificationWorker(
        get_native_library_store(),
        get_library_contribution_service(),
        get_musicbrainz_repository(),
    )


@singleton
def get_target_local_files_service() -> "LocalFilesService":
    from services.local_files_service import LocalFilesService

    return LocalFilesService(
        get_target_library_repository(), get_preferences_service(), get_cache()
    )


@singleton
def get_target_catalog_writer_service() -> "TargetCatalogWriterService":
    from services.native.target_catalog_writer_service import TargetCatalogWriterService

    from .cache_providers import get_native_library_store

    return TargetCatalogWriterService(
        get_native_library_store(),
        get_target_local_files_service(),
        get_target_native_library_service(),
        get_audio_tagger(),
    )


@singleton
def get_library_policy_resolver() -> "LibraryPolicyResolver":
    from services.native.library_policy_resolver import LibraryPolicyResolver

    return LibraryPolicyResolver(get_preferences_service().get_typed_library_settings())


@singleton
def get_library_policy_service() -> "LibraryPolicyService":
    from services.native.library_policy_service import LibraryPolicyService

    return LibraryPolicyService(
        preferences=get_preferences_service(),
        library_db=get_library_db(),
        resolver_getter=get_library_policy_resolver,
        resolver_clearer=get_library_policy_resolver.cache_clear,
    )


@singleton
def get_library_management_profile_service() -> "LibraryManagementProfileService":
    from services.native.library_management_profile_service import (
        LibraryManagementProfileService,
    )

    return LibraryManagementProfileService(get_preferences_service())


@singleton
def get_canonical_release_metadata_service() -> "CanonicalReleaseMetadataService":
    from services.native.canonical_release_metadata_service import (
        CanonicalReleaseMetadataService,
    )

    from .cache_providers import get_native_library_store

    return CanonicalReleaseMetadataService(
        get_native_library_store(), get_musicbrainz_repository()
    )


@singleton
def get_effective_metadata_projection_service() -> "EffectiveMetadataProjectionService":
    from services.native.effective_metadata_projection_service import (
        EffectiveMetadataProjectionService,
    )

    return EffectiveMetadataProjectionService()


@singleton
def get_library_management_override_service() -> "LibraryManagementOverrideService":
    from services.native.library_management_override_service import (
        LibraryManagementOverrideService,
    )

    from .cache_providers import get_native_library_store

    return LibraryManagementOverrideService(get_native_library_store())


@singleton
def get_genre_normalizer() -> "GenreNormalizer":
    from services.native.genre_normalizer import GenreNormalizer

    return GenreNormalizer()


@singleton
def get_genre_projection_service() -> "GenreProjectionService":
    from services.native.genre_projection_service import GenreProjectionService

    return GenreProjectionService(
        get_genre_normalizer(),
        listenbrainz=get_listenbrainz_repository(),
        lastfm=get_lastfm_repository(),
    )


@singleton
def get_lyrics_projection_service() -> "LyricsProjectionService":
    from services.native.lyrics_projection_service import LyricsProjectionService

    return LyricsProjectionService(get_lrclib_repository())


@singleton
def get_replaygain_analysis_service() -> "ReplayGainAnalysisService":
    from services.native.replaygain_analysis_service import ReplayGainAnalysisService

    return ReplayGainAnalysisService()


@singleton
def get_artwork_processor() -> "ArtworkProcessor":
    from infrastructure.audio.artwork_processor import ArtworkProcessor

    return ArtworkProcessor()


@singleton
def get_artwork_projection_service() -> "ArtworkProjectionService":
    from services.native.artwork_projection_service import ArtworkProjectionService

    return ArtworkProjectionService(
        get_target_coverart_repository(), get_artwork_processor()
    )


@singleton
def get_target_library_policy_reconciliation_service() -> (
    "LibraryPolicyReconciliationService"
):
    from services.native.library_policy_reconciliation_service import (
        LibraryPolicyReconciliationService,
    )

    from .cache_providers import get_native_library_store

    return LibraryPolicyReconciliationService(
        get_native_library_store(),
        get_library_policy_resolver,
        get_target_library_scan_coordinator(),
    )


@singleton
def get_target_library_policy_service() -> "TargetLibraryPolicyService":
    from core.dependencies.cleanup import clear_library_policy_dependent_caches
    from services.native.target_library_policy_service import TargetLibraryPolicyService
    from services.native.library_policy_service import LibraryPolicyService

    from .cache_providers import get_native_library_store

    return TargetLibraryPolicyService(
        LibraryPolicyService(
            preferences=get_preferences_service(),
            library_db=None,
            resolver_getter=get_library_policy_resolver,
            resolver_clearer=get_library_policy_resolver.cache_clear,
        ),
        get_target_library_policy_reconciliation_service(),
        get_native_library_store(),
        on_settings_saved=clear_library_policy_dependent_caches,
        transition_lock=get_library_policy_transition_lock(),
    )


@singleton
def get_library_policy_transition_lock():
    import asyncio

    return asyncio.Lock()


@singleton
def get_legacy_catalog_importer() -> "LegacyCatalogImporter":
    from services.native.legacy_catalog_importer import LegacyCatalogImporter

    from .cache_providers import get_native_library_store

    return LegacyCatalogImporter(
        get_native_library_store(),
        get_library_policy_resolver(),
        get_audio_tagger(),
    )


@singleton
def get_target_library_scan_coordinator() -> "LibraryScanCoordinator":
    from services.native.library_indexer import LibraryIndexer
    from services.native.library_inventory_scanner import LibraryInventoryScanner
    from services.native.library_reconciler import LibraryReconciler
    from services.native.library_scan_coordinator import LibraryScanCoordinator
    from services.native.library_scan_events import LibraryScanEventPublisher

    from .cache_providers import get_native_library_store

    store = get_native_library_store()
    filesystem = get_library_filesystem_coordinator()
    return LibraryScanCoordinator(
        store,
        LibraryInventoryScanner(store, filesystem_coordinator=filesystem),
        LibraryIndexer(store, get_audio_tagger(), filesystem_coordinator=filesystem),
        LibraryReconciler(store, filesystem),
        get_library_policy_resolver,
        LibraryScanEventPublisher(store, get_sse_publisher()),
        workload_gate=get_background_workload_gate(),
        filesystem_coordinator=filesystem,
    )


@singleton
def get_target_library_scan_scheduler() -> "LibraryAutomaticScanScheduler":
    from services.native.library_scan_scheduler import LibraryAutomaticScanScheduler

    return LibraryAutomaticScanScheduler()


@singleton
def get_target_identification_queue() -> "IdentificationQueueService":
    from services.native.identification_queue_service import IdentificationQueueService

    from .cache_providers import get_native_library_store

    return IdentificationQueueService(get_native_library_store())


@singleton
def get_target_album_identification_service() -> "AlbumIdentificationService":
    from services.native.album_candidate_service import AlbumCandidateService
    from services.native.album_evidence_engine import AlbumEvidenceEngine
    from services.native.album_identification_service import AlbumIdentificationService
    from services.native.conditional_fingerprint_service import (
        ConditionalFingerprintService,
    )

    from .cache_providers import get_native_library_store

    store = get_native_library_store()
    cache = get_cache()

    async def invalidate(_domains: set[str]) -> None:
        await asyncio.gather(
            *(
                cache.clear_prefix(prefix)
                for prefix in library_identification_prefixes()
            )
        )
        await get_discovery_snapshot_store().mark_discover_stale()

    return AlbumIdentificationService(
        store,
        get_target_identification_queue(),
        AlbumCandidateService(get_musicbrainz_identification_repository()),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, get_audio_fingerprinter()),
        invalidate,
        get_automatic_scan_management_service().schedule_identified_album,
    )


@singleton
def get_target_album_coverage_service() -> "AlbumCoverageService":
    from services.native.album_coverage_service import AlbumCoverageService

    from .cache_providers import get_native_library_store

    return AlbumCoverageService(
        get_native_library_store(), get_target_identification_queue()
    )


@singleton
def get_target_reidentification_service() -> "ReidentificationService":
    from services.native.reidentification_service import ReidentificationService

    from .cache_providers import get_native_library_store

    return ReidentificationService(get_native_library_store())


@singleton
def get_target_library_review_service() -> "LibraryReviewService":
    from services.native.library_review_service import LibraryReviewService

    from .cache_providers import get_native_library_store

    return LibraryReviewService(
        get_native_library_store(),
        resolver_getter=get_library_policy_resolver,
        on_identified=(
            get_automatic_scan_management_service().schedule_identified_album
        ),
    )


@singleton
def get_target_library_operation_service() -> "LibraryOperationService":
    from services.native.library_operation_service import LibraryOperationService

    from .cache_providers import get_native_library_store

    return LibraryOperationService(
        get_native_library_store(),
        on_identified=(
            get_automatic_scan_management_service().schedule_identified_album
        ),
    )


@singleton
def get_target_catalog_correction_service() -> "CatalogCorrectionService":
    from services.native.catalog_correction_service import CatalogCorrectionService

    from .cache_providers import get_native_library_store

    return CatalogCorrectionService(get_native_library_store())


@singleton
def get_target_identity_repair_service() -> "IdentityRepairService":
    from services.native.album_evidence_engine import AlbumEvidenceEngine
    from services.native.identity_repair_service import IdentityRepairService

    from .cache_providers import get_native_library_store

    return IdentityRepairService(
        get_native_library_store(),
        get_musicbrainz_identification_repository(),
        AlbumEvidenceEngine(),
    )


@singleton
def get_target_library_diagnostics_service() -> "LibraryDiagnosticsService":
    from services.native.library_diagnostics_service import LibraryDiagnosticsService

    from .cache_providers import get_native_library_store

    return LibraryDiagnosticsService(get_native_library_store())


@singleton
def get_target_explicit_reidentification_worker() -> "ExplicitReidentificationWorker":
    from services.native.album_candidate_service import AlbumCandidateService
    from services.native.album_evidence_engine import AlbumEvidenceEngine
    from services.native.conditional_fingerprint_service import (
        ConditionalFingerprintService,
    )
    from services.native.explicit_reidentification_worker import (
        ExplicitReidentificationWorker,
    )

    from .cache_providers import get_native_library_store

    store = get_native_library_store()
    return ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(get_musicbrainz_identification_repository()),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, get_audio_fingerprinter()),
        get_background_workload_gate(),
        get_automatic_scan_management_service().schedule_identified_album,
    )


@singleton
def get_library_management_planner() -> "LibraryManagementPlanner":
    from services.native.library_management_planner import LibraryManagementPlanner

    from .cache_providers import (
        get_library_management_blob_store,
        get_native_library_store,
    )

    return LibraryManagementPlanner(
        get_native_library_store(),
        get_preferences_service(),
        get_canonical_release_metadata_service(),
        get_effective_metadata_projection_service(),
        get_genre_projection_service(),
        get_artwork_projection_service(),
        get_audio_metadata_engine(),
        get_audio_write_planning_service(),
        get_naming_template_engine(),
        get_tagging_script_engine(),
        get_library_management_blob_store(),
        get_background_workload_gate(),
        lyrics=get_lyrics_projection_service(),
        replaygain=get_replaygain_analysis_service(),
    )


@singleton
def get_automatic_scan_management_service() -> "AutomaticScanManagementService":
    from services.native.automatic_scan_management_service import (
        AutomaticScanManagementService,
    )

    from .cache_providers import get_native_library_store

    return AutomaticScanManagementService(
        get_native_library_store(),
        get_library_management_profile_service(),
        get_library_management_planner(),
    )


@singleton
def get_library_management_preview_service() -> "LibraryManagementPreviewService":
    from services.native.library_management_preview_service import (
        LibraryManagementPreviewService,
    )

    from .cache_providers import (
        get_library_management_blob_store,
        get_native_library_store,
    )

    return LibraryManagementPreviewService(
        get_native_library_store(),
        get_preferences_service(),
        get_library_management_profile_service(),
        get_library_management_planner(),
        get_audio_metadata_engine(),
        get_library_management_blob_store(),
    )


@singleton
def get_library_management_publisher() -> "LibraryManagementPublisher":
    from services.native.library_management_publisher import LibraryManagementPublisher

    from .cache_providers import (
        get_library_management_blob_store,
        get_native_library_store,
    )

    return LibraryManagementPublisher(
        get_native_library_store(),
        get_preferences_service(),
        get_audio_metadata_engine(),
        get_audio_write_planning_service(),
        get_library_management_blob_store(),
        get_library_filesystem_coordinator(),
        on_commit=get_library_management_post_commit_service().after_commit,
    )


@singleton
def get_library_management_post_commit_service() -> (
    "LibraryManagementPostCommitService"
):
    from services.native.library_management_post_commit_service import (
        LibraryManagementPostCommitService,
    )

    from .cache_providers import (
        get_discovery_snapshot_store,
        get_native_library_store,
    )

    return LibraryManagementPostCommitService(
        get_native_library_store(),
        get_preferences_service(),
        get_cache(),
        get_disk_cache(),
        get_discovery_snapshot_store(),
        get_jellyfin_repository,
    )


@singleton
def get_library_management_notification_service() -> (
    "LibraryManagementNotificationService"
):
    from services.native.library_management_notification_service import (
        LibraryManagementNotificationService,
    )

    from .cache_providers import get_native_library_store

    return LibraryManagementNotificationService(
        get_native_library_store(),
        get_jellyfin_repository,
    )


@singleton
def get_automatic_import_management_service() -> "AutomaticImportManagementService":
    from services.native.automatic_import_management_service import (
        AutomaticImportManagementService,
    )

    return AutomaticImportManagementService(
        get_library_management_profile_service(),
        get_library_management_planner(),
        get_canonical_release_metadata_service(),
        get_effective_metadata_projection_service(),
        get_genre_projection_service(),
        get_artwork_projection_service(),
        get_audio_metadata_engine(),
        get_audio_write_planning_service(),
        get_naming_template_engine(),
        get_tagging_script_engine(),
        lyrics=get_lyrics_projection_service(),
        replaygain=get_replaygain_analysis_service(),
    )


@singleton
def get_library_management_undo_service() -> "LibraryManagementUndoService":
    from services.native.library_management_undo_service import (
        LibraryManagementUndoService,
    )

    from .cache_providers import (
        get_library_management_blob_store,
        get_native_library_store,
    )

    return LibraryManagementUndoService(
        get_native_library_store(),
        get_preferences_service(),
        get_audio_metadata_engine(),
        get_library_management_blob_store(),
        get_library_filesystem_coordinator(),
    )


@singleton
def get_library_management_baseline_service() -> "LibraryManagementBaselineService":
    from services.native.library_management_baseline_service import (
        LibraryManagementBaselineService,
    )

    from .cache_providers import (
        get_library_management_blob_store,
        get_native_library_store,
    )

    return LibraryManagementBaselineService(
        get_native_library_store(),
        get_preferences_service(),
        get_audio_metadata_engine(),
        get_library_management_blob_store(),
        get_library_filesystem_coordinator(),
        get_library_management_undo_service(),
    )


@singleton
def get_library_management_duplicate_service() -> "LibraryManagementDuplicateService":
    from services.native.library_management_duplicate_service import (
        LibraryManagementDuplicateService,
    )

    from .cache_providers import get_native_library_store

    return LibraryManagementDuplicateService(
        get_native_library_store(),
        get_preferences_service(),
        get_library_filesystem_coordinator(),
    )


@singleton
def get_library_management_recovery_service() -> "LibraryManagementRecoveryService":
    from services.native.library_management_recovery_service import (
        LibraryManagementRecoveryService,
    )

    from .cache_providers import get_native_library_store

    return LibraryManagementRecoveryService(
        get_native_library_store(),
        get_library_management_publisher(),
        get_library_filesystem_coordinator(),
    )


@singleton
def get_library_management_worker() -> "LibraryManagementWorker":
    from services.native.library_management_worker import LibraryManagementWorker

    from .cache_providers import get_native_library_store

    return LibraryManagementWorker(
        get_native_library_store(),
        get_library_management_planner(),
        get_library_management_publisher(),
        get_library_management_undo_service(),
        get_library_management_baseline_service(),
        get_library_management_duplicate_service(),
    )


@singleton
def get_target_library_operation_supervisor() -> "LibraryOperationSupervisor":
    from services.native.library_operation_supervisor import LibraryOperationSupervisor

    from .cache_providers import get_native_library_store

    return LibraryOperationSupervisor(
        get_native_library_store(),
        get_target_library_operation_service(),
        get_target_identity_repair_service(),
        get_target_explicit_reidentification_worker(),
        get_background_workload_gate(),
        get_library_management_worker(),
        get_library_management_notification_service(),
    )


@singleton
def get_audio_metadata_engine() -> "AudioMetadataEngine":
    from infrastructure.audio.metadata_engine import AudioMetadataEngine

    return AudioMetadataEngine()


@singleton
def get_audio_write_planning_service() -> "AudioWritePlanningService":
    from services.native.audio_write_planning_service import AudioWritePlanningService

    return AudioWritePlanningService(get_audio_metadata_engine())


@singleton
def get_audio_tagger() -> "AudioTagger":
    from infrastructure.audio.tagger import AudioTagger

    return AudioTagger(get_audio_metadata_engine())


@singleton
def get_naming_template_engine() -> "NamingTemplateEngine":
    from services.native.naming import NamingTemplateEngine

    return NamingTemplateEngine()


@singleton
def get_tagging_script_engine() -> "TaggingScriptEngine":
    from services.native.tagging_scripts import TaggingScriptEngine

    return TaggingScriptEngine()


@singleton
def get_musicbrainz_matcher() -> "MusicBrainzMatcher":
    from services.native.musicbrainz_matcher import MusicBrainzMatcher

    return MusicBrainzMatcher(get_musicbrainz_repository())


@singleton
def get_album_identifier() -> "AlbumIdentifier":
    from services.native.album_matcher import AlbumIdentifier

    return AlbumIdentifier(get_musicbrainz_repository())


@singleton
def get_audio_fingerprinter() -> "AudioFingerprinter":
    from infrastructure.audio.fingerprinter import AudioFingerprinter
    from infrastructure.http.client import HttpClientFactory
    from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter

    # unique client name avoids the per-name timeout-caching issue
    http = HttpClientFactory.get_client(name="acoustid-fingerprint", timeout=15.0)
    # read the decrypted key fresh per call so a settings change applies without restart
    preferences_service = get_preferences_service()

    def _acoustid_api_key() -> str:
        return preferences_service.get_typed_library_settings_raw().acoustid_api_key

    rate_limiter = TokenBucketRateLimiter(rate=3.0, capacity=3)
    return AudioFingerprinter(http, _acoustid_api_key, rate_limiter)


@singleton
def get_library_manager() -> "LibraryManager":
    # reuse the repo provider's singleton so there is one instance (one write lock)
    return get_library_repository()  # type: ignore[return-value]


@singleton
def get_library_scanner() -> "LibraryScanner":
    from services.native.library_scanner import LibraryScanner

    # singleton so the cancel route and the running scan share one `_cancel` event
    return LibraryScanner(
        audio_tagger=get_audio_tagger(),
        fingerprinter=get_audio_fingerprinter(),
        mb_matcher=get_musicbrainz_matcher(),
        album_identifier=get_album_identifier(),
        library_manager=get_library_manager(),
        scan_state_store=get_scan_state_store(),
        event_bus=get_sse_publisher(),
        invalidate_albums=_build_scan_invalidation(
            get_cache(), get_disk_cache(), get_discovery_snapshot_store()
        ),
    )


def _build_file_processor(
    library_manager,
    library_paths,
    *,
    library_root_ids=None,
    publish_import_bundle=None,
    policy_revision_getter=None,
) -> "FileProcessor":
    from pathlib import Path

    from core.config import get_settings
    from services.native.file_processor import FileProcessor
    from services.native.recycle_bin import resolve_bin_path

    from .repo_providers import get_download_client_repository, get_download_store

    policy = get_preferences_service().get_download_policy()
    return FileProcessor(
        get_audio_tagger(),
        naming_engine=get_naming_template_engine(),
        library_manager=library_manager,
        library_paths=[Path(path) for path in library_paths],
        client=get_download_client_repository(),
        slskd_downloads_path=Path(get_settings().slskd_downloads_path),
        fingerprinter=get_audio_fingerprinter(),
        verify_downloads=policy.verify_downloads,
        download_store=get_download_store(),
        held_dir=Path(get_settings().cache_dir) / "held",
        recycle_bin=resolve_bin_path(policy.recycle_bin_path, library_paths),
        library_root_ids=library_root_ids,
        publish_import_bundle=publish_import_bundle,
        policy_revision_getter=policy_revision_getter,
    )


@singleton
def get_file_processor() -> "FileProcessor":
    return get_target_file_processor()


@singleton
def get_target_import_library_service() -> "TargetImportLibraryService":
    from services.native.target_import_library_service import TargetImportLibraryService

    from .cache_providers import get_native_library_store

    return TargetImportLibraryService(
        get_native_library_store(),
        get_library_policy_resolver,
        get_target_identification_queue(),
        policy_transition_lock=get_library_policy_transition_lock(),
        filesystem_coordinator=get_library_filesystem_coordinator(),
        management_publisher=get_library_management_publisher(),
        automatic_management=get_automatic_import_management_service(),
    )


@singleton
def get_target_file_processor() -> "FileProcessor":
    resolver = get_library_policy_resolver()
    import_library = get_target_import_library_service()
    return _build_file_processor(
        import_library,
        [root.path for root in resolver.settings.library_roots],
        library_root_ids=[root.id for root in resolver.settings.library_roots],
        publish_import_bundle=import_library.publish_import_bundle,
        policy_revision_getter=lambda: get_library_policy_resolver().policy_revision,
    )


@singleton
def get_sse_publisher() -> "SSEPublisher":
    from infrastructure.sse_publisher import SSEPublisher

    return SSEPublisher()


@singleton
def get_plugin_host() -> "PluginHost":
    from core.config import get_settings
    from infrastructure.plugins.host import PluginHost

    return PluginHost(
        plugins_dir=get_settings().root_app_dir / "plugins",
        preferences_service=get_preferences_service(),
    )


def _build_free_music_service(drop_import) -> "FreeMusicService":
    from services.native.free_music_service import FreeMusicService

    from .repo_providers import get_archive_repository, get_free_music_store

    return FreeMusicService(
        store=get_free_music_store(),
        archive=get_archive_repository(),
        drop_import=drop_import,
        preferences_service=get_preferences_service(),
        sse_publisher=get_sse_publisher(),
    )


@singleton
def get_free_music_service() -> "FreeMusicService":
    return _build_free_music_service(get_drop_import_service())


@singleton
def get_target_free_music_service() -> "FreeMusicService":
    return _build_free_music_service(get_target_drop_import_service())


@singleton
def get_acquisition_dispatcher() -> "AcquisitionDispatcher":
    from services.acquisition_dispatcher import AcquisitionDispatcher

    # holds the getters, not instances: a settings save rebuilds DownloadService,
    # and Free Music reads its settings per request - so the dispatcher itself
    # never needs rebuilding
    return AcquisitionDispatcher(
        get_download_service=get_download_service,
        get_free_music_service=get_free_music_service,
        preferences_service=get_preferences_service(),
    )


@singleton
def get_target_acquisition_dispatcher() -> "AcquisitionDispatcher":
    from services.acquisition_dispatcher import AcquisitionDispatcher

    return AcquisitionDispatcher(
        get_download_service=get_target_download_service,
        get_free_music_service=get_target_free_music_service,
        preferences_service=get_preferences_service(),
        ownership_service=get_target_library_ownership_service(),
    )


@singleton
def get_get_it_service() -> "GetItService":
    from services.get_it_service import GetItService

    from .repo_providers import get_itunes_repository, get_musicbrainz_repository

    return GetItService(
        mb_repo=get_musicbrainz_repository(),
        itunes_repo=get_itunes_repository(),
        preferences_service=get_preferences_service(),
        cache=get_cache(),
        plugin_host=get_plugin_host(),
    )


def _build_drop_import_service(
    library_manager,
    on_import,
    *,
    publish_import_bundle=None,
    policy_revision_getter=None,
) -> "DropImportService":
    from core.config import get_settings
    from services.native.drop_import_service import DropImportService

    from .repo_providers import (
        get_drop_import_store,
        get_request_history_store,
        get_wanted_store,
    )

    return DropImportService(
        store=get_drop_import_store(),
        tagger=get_audio_tagger(),
        fingerprinter=get_audio_fingerprinter(),
        album_identifier=get_album_identifier(),
        mb_matcher=get_musicbrainz_matcher(),
        naming_engine=get_naming_template_engine(),
        library_manager=library_manager,
        preferences_service=get_preferences_service(),
        request_history=get_request_history_store(),
        wanted_store=get_wanted_store(),
        sse_publisher=get_sse_publisher(),
        on_import=on_import,
        staging_root=get_settings().root_app_dir / "imports",
        publish_import_bundle=publish_import_bundle,
        policy_revision_getter=policy_revision_getter,
    )


def _drop_import_callback(canonical):
    async def on_drop_import(*, mbid, artist_mbid, artist_name, title, year):  # noqa: ANN001, ANN202
        record = RequestHistoryRecord(
            musicbrainz_id=mbid,
            artist_name=artist_name or "",
            album_title=title or "",
            requested_at="",
            status="imported",
            artist_mbid=artist_mbid,
            year=year,
        )
        await canonical(record)

    return on_drop_import


@singleton
def get_drop_import_service() -> "DropImportService":
    return get_target_drop_import_service()


@singleton
def get_target_drop_import_service() -> "DropImportService":
    canonical = _build_target_import_invalidation(get_cache(), get_disk_cache())
    import_library = get_target_import_library_service()
    return _build_drop_import_service(
        import_library,
        _drop_import_callback(canonical),
        publish_import_bundle=import_library.publish_import_bundle,
        policy_revision_getter=lambda: get_library_policy_resolver().policy_revision,
    )


@singleton
def get_now_playing_service() -> "NowPlayingService":
    from services.now_playing_service import NowPlayingService

    return NowPlayingService(get_sse_publisher(), get_user_listening_prefs_store())


def _build_search_service(
    library_repo, ownership_service=None, coverart_repo=None
) -> "SearchService":
    from services.search_service import SearchService

    mb_repo = get_musicbrainz_repository()
    coverart_repo = coverart_repo or get_coverart_repository()
    preferences_service = get_preferences_service()
    audiodb_image_service = get_audiodb_image_service()
    browse_queue = get_audiodb_browse_queue()
    return SearchService(
        mb_repo,
        library_repo,
        coverart_repo,
        preferences_service,
        audiodb_image_service,
        browse_queue,
        ownership_service,
    )


@singleton
def get_search_service() -> "SearchService":
    return _build_search_service(get_library_repository())


@singleton
def get_target_search_service() -> "SearchService":
    return _build_search_service(
        get_target_library_repository(),
        get_target_library_ownership_service(),
        get_target_coverart_repository(),
    )


def _build_artist_service(
    library_repo, library_db=None, ownership_service=None
) -> "ArtistService":
    from services.artist_service import ArtistService

    mb_repo = get_musicbrainz_repository()
    wikidata_repo = get_wikidata_repository()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    disk_cache = get_disk_cache()
    audiodb_image_service = get_audiodb_image_service()
    browse_queue = get_audiodb_browse_queue()
    return ArtistService(
        mb_repo,
        library_repo,
        wikidata_repo,
        preferences_service,
        memory_cache,
        disk_cache,
        audiodb_image_service,
        browse_queue,
        library_db,
        ownership_service,
    )


@singleton
def get_artist_service() -> "ArtistService":
    return _build_artist_service(get_library_repository(), get_library_db())


@singleton
def get_target_artist_service() -> "ArtistService":
    return _build_artist_service(
        get_target_library_repository(),
        ownership_service=get_target_library_ownership_service(),
    )


@singleton
def get_follow_service() -> "FollowService":
    from services.follow_service import FollowService

    return FollowService(get_follow_store(), get_musicbrainz_repository())


@singleton
def get_lidarr_import_service() -> "LidarrImportService":
    from services.lidarr_import_service import LidarrImportService

    from .repo_providers import get_lidarr_import_repository

    return LidarrImportService(
        repo=get_lidarr_import_repository(),
        preferences=get_preferences_service(),
        follow_store=get_follow_store(),
        follow_service=get_follow_service(),
    )


def _build_new_release_service(*, library_repo, acquisition) -> "NewReleaseService":
    from services.native.new_release_service import NewReleaseService

    from .repo_providers import get_download_store

    return NewReleaseService(
        follow_store=get_follow_store(),
        mb_repo=get_musicbrainz_repository(),
        acquisition=acquisition,
        download_store=get_download_store(),
        library_repo=library_repo,
        sse_publisher=get_sse_publisher(),
    )


@singleton
def get_new_release_service() -> "NewReleaseService":
    return _build_new_release_service(
        library_repo=get_library_repository(),
        acquisition=get_acquisition_dispatcher(),
    )


@singleton
def get_target_new_release_service() -> "NewReleaseService":
    return _build_new_release_service(
        library_repo=get_target_library_repository(),
        acquisition=get_target_acquisition_dispatcher(),
    )


def _build_wanted_watcher_service(
    *, download_service_getter, library_manager, album_service
) -> "WantedWatcherService":
    from services.native.wanted_watcher_service import WantedWatcherService

    from .repo_providers import (
        get_download_store,
        get_request_history_store,
        get_wanted_store,
    )

    return WantedWatcherService(
        wanted_store=get_wanted_store(),
        request_history=get_request_history_store(),
        download_store=get_download_store(),
        # provider, not an instance: a settings save rebuilds the DownloadService
        # singleton and the watcher must always dispatch through the current one
        get_download_service=download_service_getter,
        library_manager=library_manager,
        album_service=album_service,
        mb_repo=get_musicbrainz_repository(),
        sse_publisher=get_sse_publisher(),
        preferences=get_preferences_service(),
    )


@singleton
def get_wanted_watcher_service() -> "WantedWatcherService":
    return _build_wanted_watcher_service(
        download_service_getter=get_download_service,
        library_manager=get_library_repository(),
        album_service=get_album_service(),
    )


@singleton
def get_target_wanted_watcher_service() -> "WantedWatcherService":
    return _build_wanted_watcher_service(
        download_service_getter=get_target_download_service,
        library_manager=get_target_library_repository(),
        album_service=get_target_album_service(),
    )


@singleton
def get_events_service() -> "EventsService":
    from services.events_service import EventsService

    from .repo_providers import get_events_store

    return EventsService(
        events_store=get_events_store(),
        preferences=get_preferences_service(),
    )


@singleton
def get_events_watcher_service() -> "EventsWatcherService":
    from services.native.events_watcher_service import EventsWatcherService

    from .repo_providers import (
        get_events_store,
        get_skiddle_repository,
        get_ticketmaster_repository,
    )

    # an events settings save cache_clears this provider together with both
    # repo providers (see routes/settings.py:_clear_events_cache), so holding
    # instances here is safe - the whole chain rebuilds at once
    return EventsWatcherService(
        events_store=get_events_store(),
        follow_store=get_follow_store(),
        ticketmaster_repo=get_ticketmaster_repository(),
        skiddle_repo=get_skiddle_repository(),
        preferences=get_preferences_service(),
        sse_publisher=get_sse_publisher(),
    )


def get_events_watcher_getter():
    return get_events_watcher_service


@singleton
def get_target_events_watcher_service() -> "EventsWatcherService":
    from models.events import SweepArtist
    from services.native.events_watcher_service import EventsWatcherService

    from .repo_providers import (
        get_events_store,
        get_skiddle_repository,
        get_ticketmaster_repository,
    )

    async def target_library_artists() -> list[SweepArtist]:
        artists = await get_target_library_repository().get_artists()
        return [
            SweepArtist(
                artist_mbid=str(artist["mbid"]),
                artist_mbid_lower=str(artist["mbid"]).lower(),
                artist_name=str(artist["name"]),
            )
            for artist in artists
        ]

    return EventsWatcherService(
        events_store=get_events_store(),
        follow_store=get_follow_store(),
        ticketmaster_repo=get_ticketmaster_repository(),
        skiddle_repo=get_skiddle_repository(),
        preferences=get_preferences_service(),
        sse_publisher=get_sse_publisher(),
        library_artist_getter=target_library_artists,
    )


def _build_personal_mix_service(
    *, library_repo, playlist_service, acquisition
) -> "PersonalMixService":
    from services.personal_mix_service import PersonalMixService
    from core.dependencies.auth_providers import get_auth_store

    return PersonalMixService(
        client_factory=get_per_user_client_factory(),
        mb_repo=get_musicbrainz_repository(),
        library_repo=library_repo,
        playlist_service=playlist_service,
        acquisition=acquisition,
        listening_prefs_store=get_user_listening_prefs_store(),
        connections_store=get_user_connections_store(),
        auth_store=get_auth_store(),
    )


@singleton
def get_personal_mix_service() -> "PersonalMixService":
    return _build_personal_mix_service(
        library_repo=get_library_repository(),
        playlist_service=get_playlist_service(),
        acquisition=get_acquisition_dispatcher(),
    )


@singleton
def get_target_personal_mix_service() -> "PersonalMixService":
    from .compat_providers import get_target_consumer_composition

    return _build_personal_mix_service(
        library_repo=get_target_library_repository(),
        playlist_service=get_target_consumer_composition().playlists,
        acquisition=get_target_acquisition_dispatcher(),
    )


def _build_album_service(
    library_repo, library_db, ownership_service=None, release_pin_store=None
) -> "AlbumService":
    from services.album_service import AlbumService

    mb_repo = get_musicbrainz_repository()
    memory_cache = get_cache()
    disk_cache = get_disk_cache()
    from .repo_providers import get_album_release_pin_store

    preferences_service = get_preferences_service()
    audiodb_image_service = get_audiodb_image_service()
    browse_queue = get_audiodb_browse_queue()
    return AlbumService(
        library_repo,
        mb_repo,
        library_db,
        memory_cache,
        disk_cache,
        preferences_service,
        audiodb_image_service,
        browse_queue,
        release_pin_store=release_pin_store or get_album_release_pin_store(),
        ownership_service=ownership_service,
    )


@singleton
def get_album_service() -> "AlbumService":
    return _build_album_service(get_library_repository(), get_library_db())


@singleton
def get_target_album_service() -> "AlbumService":
    target = get_target_library_repository()
    return _build_album_service(
        target,
        target,
        get_target_library_ownership_service(),
        get_target_album_release_pin_store(),
    )


def _build_quota_service(library_db) -> "QuotaService":
    from services.quota_service import QuotaService

    from .auth_providers import get_auth_store
    from .repo_providers import get_download_store, get_user_quota_store

    return QuotaService(
        preferences=get_preferences_service(),
        user_quotas=get_user_quota_store(),
        request_history=get_request_history_store(),
        download_store=get_download_store(),
        library_db=library_db,
        auth_store=get_auth_store(),
    )


@singleton
def get_quota_service() -> "QuotaService":
    return _build_quota_service(get_library_db())


@singleton
def get_target_quota_service() -> "QuotaService":
    return _build_quota_service(get_target_library_repository())


def _build_request_service(
    download_service_getter,
    acquisition,
    ownership_service=None,
    quota_service=None,
    album_service=None,
) -> "RequestService":
    from services.request_service import RequestService

    request_history = get_request_history_store()
    return RequestService(
        request_history,
        get_download_service=download_service_getter,
        quota_service=quota_service or get_quota_service(),
        acquisition=acquisition,
        ownership_service=ownership_service,
        album_service=album_service,
        mbid_store=get_mbid_store(),
    )


@singleton
def get_request_service() -> "RequestService":
    return _build_request_service(
        get_download_service,
        get_acquisition_dispatcher(),
        album_service=get_album_service(),
    )


@singleton
def get_target_request_service() -> "RequestService":
    return _build_request_service(
        get_target_download_service,
        get_target_acquisition_dispatcher(),
        get_target_library_ownership_service(),
        get_target_quota_service(),
        get_target_album_service(),
    )


def _build_scan_invalidation(memory_cache, disk_cache, snapshot_store=None):
    """Bust the cached album pages of the release groups a scan or re-identify re-attributed,
    so they reflect the new identity without a manual refresh. The per-album entries are
    deleted for each changed group; the shared library/home list caches are cleared once."""

    async def invalidate(changed_rgs: set[str]) -> None:
        if not changed_rgs:
            return
        per_album = []
        for rg in changed_rgs:
            per_album.append(memory_cache.delete(f"{ALBUM_INFO_PREFIX}{rg}"))
            per_album.append(memory_cache.delete(f"{LIBRARY_ALBUM_DETAILS_PREFIX}{rg}"))
        shared = [
            memory_cache.delete(library_raw_albums_key()),
            memory_cache.clear_prefix(f"{LIBRARY_PREFIX}library:"),
            memory_cache.clear_prefix(HOME_RESPONSE_PREFIX),
            memory_cache.clear_prefix(DISCOVER_RESPONSE_PREFIX),
            memory_cache.clear_prefix(DAILY_MIX_PREFIX),
            memory_cache.clear_prefix(TOP_PICKS_PREFIX),
        ]
        await asyncio.gather(*per_album, *shared, return_exceptions=True)
        await asyncio.gather(
            *(disk_cache.delete_album(rg) for rg in changed_rgs),
            return_exceptions=True,
        )
        if snapshot_store is not None:
            await snapshot_store.mark_discover_stale()

    return invalidate


def _build_target_import_invalidation(memory_cache, disk_cache):
    async def on_import(record: RequestHistoryRecord) -> None:
        invalidations = [
            memory_cache.delete(library_raw_albums_key()),
            memory_cache.clear_prefix(f"{LIBRARY_PREFIX}library:"),
            memory_cache.delete(library_requested_mbids_key()),
            memory_cache.clear_prefix(HOME_RESPONSE_PREFIX),
            memory_cache.delete(f"{ALBUM_INFO_PREFIX}{record.musicbrainz_id}"),
            memory_cache.delete(
                f"{LIBRARY_ALBUM_DETAILS_PREFIX}{record.musicbrainz_id}"
            ),
        ]
        if record.artist_mbid:
            invalidations.append(
                memory_cache.delete(f"{ARTIST_INFO_PREFIX}{record.artist_mbid}")
            )
        await asyncio.gather(*invalidations, return_exceptions=True)
        if record.artist_mbid:
            await asyncio.gather(
                disk_cache.delete_album(record.musicbrainz_id),
                disk_cache.delete_artist(record.artist_mbid),
                return_exceptions=True,
            )
        else:
            try:
                await disk_cache.delete_album(record.musicbrainz_id)
            except OSError as exc:
                logger.warning(
                    "Failed to delete disk cache album %s during import invalidation: %s",
                    record.musicbrainz_id,
                    exc,
                )

    return on_import


def _build_import_invalidation(memory_cache, disk_cache, library_db):
    """Invalidate shared caches and retain the legacy album materialization."""

    invalidate = _build_target_import_invalidation(memory_cache, disk_cache)

    async def on_import(record: RequestHistoryRecord) -> None:
        await invalidate(record)
        try:
            await library_db.upsert_album(
                {
                    "mbid": record.musicbrainz_id,
                    "artist_mbid": record.artist_mbid or "",
                    "artist_name": record.artist_name or "",
                    "title": record.album_title or "",
                    "year": record.year,
                    "cover_url": record.cover_url or "",
                }
            )
        except Exception as ex:  # noqa: BLE001
            logger.warning("Failed to upsert album into library cache: %s", ex)

    return on_import


def _build_requests_page_service(
    *, library_repo, on_import, download_service_getter, acquisition
) -> "RequestsPageService":
    from services.requests_page_service import RequestsPageService

    request_history = get_request_history_store()

    async def merged_library_mbids() -> set[str]:
        return set(await library_repo.get_library_mbids())

    from .repo_providers import get_download_store

    return RequestsPageService(
        library_repo=library_repo,
        request_history=request_history,
        library_mbids_fn=merged_library_mbids,
        on_import_callback=on_import,
        get_download_service=download_service_getter,
        download_store=get_download_store(),
        acquisition=acquisition,
    )


@singleton
def get_requests_page_service() -> "RequestsPageService":
    return _build_requests_page_service(
        library_repo=get_library_repository(),
        on_import=_build_import_invalidation(
            get_cache(), get_disk_cache(), get_library_db()
        ),
        download_service_getter=get_download_service,
        acquisition=get_acquisition_dispatcher(),
    )


@singleton
def get_target_requests_page_service() -> "RequestsPageService":
    return _build_requests_page_service(
        library_repo=get_target_library_repository(),
        on_import=_build_target_import_invalidation(get_cache(), get_disk_cache()),
        download_service_getter=get_target_download_service,
        acquisition=get_target_acquisition_dispatcher(),
    )


@singleton
def get_playlist_service() -> "PlaylistService":
    from services.playlist_service import PlaylistService
    from core.config import get_settings
    from core.dependencies.auth_providers import get_auth_store

    settings = get_settings()
    playlist_repo = get_playlist_repository()
    return PlaylistService(
        repo=playlist_repo,
        cache_dir=settings.cache_dir,
        cache=get_cache(),
        genre_index=get_genre_index(),
        auth_store=get_auth_store(),
        library_db=get_library_db(),
    )


@singleton
def get_library_service() -> "LibraryService":
    from services.library_service import LibraryService

    library_repo = get_library_repository()
    library_db = get_library_db()
    cover_repo = get_coverart_repository()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    disk_cache = get_disk_cache()
    artist_discovery_service = get_artist_discovery_service()
    audiodb_image_service = get_audiodb_image_service()
    local_files_service = get_local_files_service()
    jellyfin_library_service = get_jellyfin_library_service()
    navidrome_library_service = get_navidrome_library_service()
    sync_state_store = get_sync_state_store()
    genre_index = get_genre_index()
    return LibraryService(
        library_repo,
        library_db,
        cover_repo,
        preferences_service,
        memory_cache,
        disk_cache,
        artist_discovery_service=artist_discovery_service,
        audiodb_image_service=audiodb_image_service,
        local_files_service=local_files_service,
        jellyfin_library_service=jellyfin_library_service,
        navidrome_library_service=navidrome_library_service,
        navidrome_folder_scope_service=get_navidrome_folder_scope_service(),
        sync_state_store=sync_state_store,
        genre_index=genre_index,
    )


@singleton
def get_status_service() -> "StatusService":
    from services.status_service import StatusService

    from .repo_providers import get_download_client_repository

    return StatusService(get_download_client_repository(), get_library_manager())


@singleton
def get_target_status_service() -> "StatusService":
    from services.status_service import StatusService

    from .repo_providers import get_download_client_repository

    return StatusService(
        get_download_client_repository(),
        get_target_import_library_service(),
    )


def _build_home_service(
    library_repo, play_history_store, ownership_service=None, genre_artwork_service=None
) -> "HomeService":
    from services.home_service import HomeService
    from core.config import get_settings

    settings = get_settings()
    listenbrainz_repo = get_listenbrainz_repository()
    jellyfin_repo = get_jellyfin_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    lastfm_repo = get_lastfm_repository()
    audiodb_image_service = get_audiodb_image_service()
    return HomeService(
        listenbrainz_repo=listenbrainz_repo,
        jellyfin_repo=jellyfin_repo,
        library_repo=library_repo,
        musicbrainz_repo=musicbrainz_repo,
        preferences_service=preferences_service,
        memory_cache=memory_cache,
        lastfm_repo=lastfm_repo,
        audiodb_image_service=audiodb_image_service,
        cache_dir=settings.cache_dir,
        client_factory=get_per_user_client_factory(),
        listening_prefs_store=get_user_listening_prefs_store(),
        play_history_store=play_history_store,
        ownership_service=ownership_service,
        genre_artwork_service=genre_artwork_service,
        workload_gate=get_background_workload_gate(),
    )


@singleton
def get_home_service() -> "HomeService":
    return _build_home_service(get_library_repository(), get_play_history_store())


@singleton
def get_target_home_service() -> "HomeService":
    from .compat_providers import get_target_consumer_composition

    target = get_target_consumer_composition()
    return _build_home_service(
        target.repository,
        target.history,
        target.ownership,
        get_genre_artwork_service(),
    )


@singleton
def get_genre_cover_prewarm_service() -> "GenreCoverPrewarmService":
    return _build_genre_cover_prewarm_service(get_coverart_repository())


def _build_genre_cover_prewarm_service(cover_repo) -> "GenreCoverPrewarmService":
    from services.genre_cover_prewarm_service import GenreCoverPrewarmService

    return GenreCoverPrewarmService(cover_repo=cover_repo)


@singleton
def get_target_genre_cover_prewarm_service() -> "GenreCoverPrewarmService":
    return _build_genre_cover_prewarm_service(get_target_coverart_repository())


def _build_home_charts_service(
    library_repo,
    genre_artwork_service=None,
    genre_index=None,
    prewarm_service=None,
) -> "HomeChartsService":
    from services.home_charts_service import HomeChartsService

    listenbrainz_repo = get_listenbrainz_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    genre_index = genre_index or get_genre_index()
    lastfm_repo = get_lastfm_repository()
    preferences_service = get_preferences_service()
    prewarm_service = prewarm_service or get_genre_cover_prewarm_service()
    return HomeChartsService(
        listenbrainz_repo=listenbrainz_repo,
        library_repo=library_repo,
        musicbrainz_repo=musicbrainz_repo,
        genre_index=genre_index,
        lastfm_repo=lastfm_repo,
        preferences_service=preferences_service,
        prewarm_service=prewarm_service,
        client_factory=get_per_user_client_factory(),
        listening_prefs_store=get_user_listening_prefs_store(),
        genre_artwork_service=genre_artwork_service,
    )


@singleton
def get_home_charts_service() -> "HomeChartsService":
    return _build_home_charts_service(get_library_repository())


@singleton
def get_target_home_charts_service() -> "HomeChartsService":
    return _build_home_charts_service(
        get_target_library_repository(),
        get_genre_artwork_service(),
        get_target_genre_index(),
        get_target_genre_cover_prewarm_service(),
    )


def _build_wrapped_service(charts_service) -> "WrappedService":
    from services.wrapped_service import WrappedService
    from core.dependencies.auth_providers import get_auth_store

    return WrappedService(
        auth_store=get_auth_store(),
        client_factory=get_per_user_client_factory(),
        charts_service=charts_service,
    )


@singleton
def get_wrapped_service() -> "WrappedService":
    return _build_wrapped_service(get_home_charts_service())


@singleton
def get_target_wrapped_service() -> "WrappedService":
    return _build_wrapped_service(get_target_home_charts_service())


@singleton
def get_settings_service() -> "SettingsService":
    from services.settings_service import SettingsService

    preferences_service = get_preferences_service()
    cache = get_cache()
    return SettingsService(
        preferences_service,
        cache,
        discovery_snapshot_store=get_discovery_snapshot_store(),
    )


@singleton
def get_target_settings_service() -> "SettingsService":
    from services.settings_service import SettingsService

    return SettingsService(
        get_preferences_service(),
        get_cache(),
        navidrome_library_getter=get_target_navidrome_library_service,
        plex_library_getter=get_target_plex_library_service,
        discovery_snapshot_store=get_discovery_snapshot_store(),
    )


def _build_artist_discovery_service(
    library_repo, library_db
) -> "ArtistDiscoveryService":
    from services.artist_discovery_service import ArtistDiscoveryService
    from core.dependencies.auth_providers import get_auth_store

    listenbrainz_repo = get_listenbrainz_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    lastfm_repo = get_lastfm_repository()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    return ArtistDiscoveryService(
        listenbrainz_repo=listenbrainz_repo,
        musicbrainz_repo=musicbrainz_repo,
        library_db=library_db,
        library_repo=library_repo,
        memory_cache=memory_cache,
        lastfm_repo=lastfm_repo,
        preferences_service=preferences_service,
        client_factory=get_per_user_client_factory(),
        auth_store=get_auth_store(),
    )


@singleton
def get_artist_discovery_service() -> "ArtistDiscoveryService":
    return _build_artist_discovery_service(get_library_repository(), get_library_db())


@singleton
def get_target_artist_discovery_service() -> "ArtistDiscoveryService":
    target = get_target_library_repository()
    return _build_artist_discovery_service(target, target)


@singleton
def get_artist_enrichment_service() -> "ArtistEnrichmentService":
    from services.artist_enrichment_service import ArtistEnrichmentService

    lastfm_repo = get_lastfm_repository()
    preferences_service = get_preferences_service()
    return ArtistEnrichmentService(
        lastfm_repo=lastfm_repo,
        preferences_service=preferences_service,
    )


@singleton
def get_album_enrichment_service() -> "AlbumEnrichmentService":
    from services.album_enrichment_service import AlbumEnrichmentService

    lastfm_repo = get_lastfm_repository()
    preferences_service = get_preferences_service()
    return AlbumEnrichmentService(
        lastfm_repo=lastfm_repo,
        preferences_service=preferences_service,
    )


def _build_album_discovery_service(library_repo, library_db) -> "AlbumDiscoveryService":
    from services.album_discovery_service import AlbumDiscoveryService

    listenbrainz_repo = get_listenbrainz_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    from services.discover.mbid_resolution_service import MbidResolutionService

    album_disc_mbid_svc = MbidResolutionService(
        musicbrainz_repo=musicbrainz_repo,
        library_repo=library_repo,
        listenbrainz_repo=listenbrainz_repo,
        library_db=library_db,
        mbid_store=get_mbid_store(),
    )
    return AlbumDiscoveryService(
        listenbrainz_repo=listenbrainz_repo,
        musicbrainz_repo=musicbrainz_repo,
        library_db=library_db,
        library_repo=library_repo,
        client_factory=get_per_user_client_factory(),
        lastfm_repo=get_lastfm_repository(),
        mbid_svc=album_disc_mbid_svc,
    )


@singleton
def get_album_discovery_service() -> "AlbumDiscoveryService":
    return _build_album_discovery_service(get_library_repository(), get_library_db())


@singleton
def get_target_album_discovery_service() -> "AlbumDiscoveryService":
    target = get_target_library_repository()
    return _build_album_discovery_service(target, target)


@singleton
def get_search_enrichment_service() -> "SearchEnrichmentService":
    from services.search_enrichment_service import SearchEnrichmentService

    mb_repo = get_musicbrainz_repository()
    lb_repo = get_listenbrainz_repository()
    preferences_service = get_preferences_service()
    lastfm_repo = get_lastfm_repository()
    return SearchEnrichmentService(mb_repo, lb_repo, preferences_service, lastfm_repo)


@singleton
def get_youtube_service() -> "YouTubeService":
    from services.youtube_service import YouTubeService

    youtube_repo = get_youtube_repo()
    youtube_store = get_youtube_store()
    return YouTubeService(youtube_repo=youtube_repo, youtube_store=youtube_store)


@singleton
def get_lastfm_auth_service() -> "LastFmAuthService":
    from services.lastfm_auth_service import LastFmAuthService

    lastfm_repo = get_lastfm_repository()
    return LastFmAuthService(lastfm_repo=lastfm_repo)


@singleton
def get_per_user_client_factory() -> "PerUserClientFactory":
    from core.config import get_settings
    from services.per_user_client_factory import PerUserClientFactory

    return PerUserClientFactory(
        connections_store=get_user_connections_store(),
        preferences_service=get_preferences_service(),
        cache=get_cache(),
        settings=get_settings(),
    )


@singleton
def get_spotify_import_service() -> "SpotifyImportService":
    from services.spotify_import_service import SpotifyImportService

    return SpotifyImportService(
        client_factory=get_per_user_client_factory(),
        playlist_repo=get_playlist_repository(),
        mb_repo=get_musicbrainz_repository(),
        playlist_service=get_playlist_service(),
    )


@singleton
def get_target_spotify_import_service() -> "SpotifyImportService":
    from services.spotify_import_service import SpotifyImportService
    from .compat_providers import get_target_consumer_composition

    target = get_target_consumer_composition()
    return SpotifyImportService(
        client_factory=get_per_user_client_factory(),
        playlist_repo=None,
        mb_repo=get_musicbrainz_repository(),
        playlist_service=target.playlists,
        async_playlist_repo=target.playlist_repository,
    )


@singleton
def get_scrobble_service() -> "ScrobbleService":
    from services.scrobble_service import ScrobbleService

    return ScrobbleService(
        client_factory=get_per_user_client_factory(),
        listening_prefs_store=get_user_listening_prefs_store(),
        play_history_store=get_play_history_store(),
        plugin_host=get_plugin_host(),
    )


def _build_discover_service(
    library_repo,
    library_db,
    playlist_service,
    ownership_service=None,
    artist_discovery=None,
    album_discovery=None,
    cover_repo=None,
    genre_artwork_service=None,
    genre_index=None,
) -> "DiscoverService":
    from services.discover_service import DiscoverService
    from services.discover.radio_service import DiscoverRadioService
    from services.discover.mbid_resolution_service import MbidResolutionService
    from services.discover.integration_helpers import IntegrationHelpers
    from services.home_transformers import HomeDataTransformers

    listenbrainz_repo = get_listenbrainz_repository()
    jellyfin_repo = get_jellyfin_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    mbid_store = get_mbid_store()
    wikidata_repo = get_wikidata_repository()
    lastfm_repo = get_lastfm_repository()
    audiodb_image_service = get_audiodb_image_service()
    genre_index = genre_index or get_genre_index()

    radio_mbid_svc = MbidResolutionService(
        musicbrainz_repo=musicbrainz_repo,
        library_repo=library_repo,
        listenbrainz_repo=listenbrainz_repo,
        library_db=library_db,
        mbid_store=mbid_store,
    )
    radio_integration = IntegrationHelpers(preferences_service)
    radio_service = DiscoverRadioService(
        lb_repo=listenbrainz_repo,
        mb_repo=musicbrainz_repo,
        mbid_svc=radio_mbid_svc,
        lfm_repo=lastfm_repo,
        artist_discovery=artist_discovery or get_artist_discovery_service(),
        album_discovery=album_discovery or get_album_discovery_service(),
        genre_index=genre_index,
        integration=radio_integration,
        transformers=HomeDataTransformers(jellyfin_repo),
    )

    return DiscoverService(
        listenbrainz_repo=listenbrainz_repo,
        jellyfin_repo=jellyfin_repo,
        library_repo=library_repo,
        musicbrainz_repo=musicbrainz_repo,
        preferences_service=preferences_service,
        memory_cache=memory_cache,
        library_db=library_db,
        mbid_store=mbid_store,
        wikidata_repo=wikidata_repo,
        lastfm_repo=lastfm_repo,
        audiodb_image_service=audiodb_image_service,
        genre_index=genre_index,
        radio_service=radio_service,
        playlist_service=playlist_service,
        client_factory=get_per_user_client_factory(),
        listening_prefs_store=get_user_listening_prefs_store(),
        follow_service=get_follow_service(),
        cover_repo=cover_repo or get_coverart_repository(),
        preview_repo=get_preview_repository(),
        ownership_service=ownership_service,
        genre_artwork_service=genre_artwork_service,
        discovery_snapshot_store=get_discovery_snapshot_store(),
        workload_gate=get_background_workload_gate(),
    )


@singleton
def get_discover_service() -> "DiscoverService":
    return _build_discover_service(
        get_library_repository(), get_library_db(), get_playlist_service()
    )


@singleton
def get_target_discover_service() -> "DiscoverService":
    from .compat_providers import get_target_consumer_composition

    target = get_target_consumer_composition()
    return _build_discover_service(
        target.repository,
        target.repository,
        target.playlists,
        target.ownership,
        get_target_artist_discovery_service(),
        get_target_album_discovery_service(),
        target.covers,
        get_genre_artwork_service(),
        get_target_genre_index(),
    )


@singleton
def get_discovery_batch_service() -> "DiscoveryBatchService":
    from services.discovery_batch_service import DiscoveryBatchService
    from .repo_providers import get_discovery_batch_store, get_request_history_store

    return DiscoveryBatchService(
        batch_store=get_discovery_batch_store(),
        request_service=get_request_service(),
        request_history=get_request_history_store(),
        library_service=get_library_service(),
        library_db=get_library_db(),
        get_download_service=get_download_service,
    )


@singleton
def get_target_discovery_batch_service() -> "DiscoveryBatchService":
    from services.discovery_batch_service import DiscoveryBatchService
    from .compat_providers import get_target_consumer_composition
    from .repo_providers import get_discovery_batch_store, get_request_history_store

    target = get_target_consumer_composition()
    return DiscoveryBatchService(
        batch_store=get_discovery_batch_store(),
        request_service=get_target_request_service(),
        request_history=get_request_history_store(),
        library_service=target.discovery_batch_library,
        library_db=target.repository,
        get_download_service=get_target_download_service,
    )


def _build_discover_queue_manager(
    discover_service, cover_repo
) -> "DiscoverQueueManager":
    from services.discover_queue_manager import DiscoverQueueManager

    preferences_service = get_preferences_service()
    return DiscoverQueueManager(
        discover_service,
        preferences_service,
        cover_repo=cover_repo,
        snapshot_store=get_discovery_snapshot_store(),
    )


@singleton
def get_discover_queue_manager() -> "DiscoverQueueManager":
    return _build_discover_queue_manager(
        get_discover_service(), get_coverart_repository()
    )


@singleton
def get_target_discover_queue_manager() -> "DiscoverQueueManager":
    from .compat_providers import get_target_consumer_composition

    return _build_discover_queue_manager(
        get_target_discover_service(), get_target_consumer_composition().covers
    )


@singleton
def get_jellyfin_playback_service() -> "JellyfinPlaybackService":
    from services.jellyfin_playback_service import JellyfinPlaybackService

    jellyfin_repo = get_jellyfin_repository()
    cache = get_cache()
    return JellyfinPlaybackService(jellyfin_repo, cache, get_per_user_client_factory())


@singleton
def get_local_files_service() -> "LocalFilesService":
    from services.local_files_service import LocalFilesService

    library_repo = get_library_repository()
    preferences_service = get_preferences_service()
    cache = get_cache()
    return LocalFilesService(library_repo, preferences_service, cache)


@singleton
def get_jellyfin_library_service() -> "JellyfinLibraryService":
    from services.jellyfin_library_service import JellyfinLibraryService

    jellyfin_repo = get_jellyfin_repository()
    preferences_service = get_preferences_service()
    return JellyfinLibraryService(
        jellyfin_repo, preferences_service, get_per_user_client_factory()
    )


@singleton
def get_navidrome_library_service() -> "NavidromeLibraryService":
    from services.navidrome_library_service import NavidromeLibraryService

    navidrome_repo = get_navidrome_repository()
    preferences_service = get_preferences_service()
    library_db = get_library_db()
    mbid_store = get_mbid_store()
    return NavidromeLibraryService(
        navidrome_repo,
        preferences_service,
        library_db,
        mbid_store,
        get_per_user_client_factory(),
    )


@singleton
def get_target_navidrome_library_service() -> "NavidromeLibraryService":
    from services.navidrome_library_service import NavidromeLibraryService

    return NavidromeLibraryService(
        get_navidrome_repository(),
        get_preferences_service(),
        get_target_library_repository(),
        get_mbid_store(),
        get_per_user_client_factory(),
    )


@singleton
def get_navidrome_folder_scope_service() -> "NavidromeFolderScopeService":
    from services.navidrome_folder_scope_service import NavidromeFolderScopeService

    from .repo_providers import (
        get_navidrome_folder_preferences_store,
        get_navidrome_repository,
    )

    return NavidromeFolderScopeService(
        get_navidrome_folder_preferences_store(), get_navidrome_repository()
    )


@singleton
def get_navidrome_playback_service() -> "NavidromePlaybackService":
    from services.navidrome_playback_service import NavidromePlaybackService

    navidrome_repo = get_navidrome_repository()
    cache = get_cache()
    return NavidromePlaybackService(
        navidrome_repo, cache, get_per_user_client_factory()
    )


@singleton
def get_plex_library_service() -> "PlexLibraryService":
    from services.plex_library_service import PlexLibraryService

    plex_repo = get_plex_repository()
    preferences_service = get_preferences_service()
    library_db = get_library_db()
    mbid_store = get_mbid_store()
    return PlexLibraryService(
        plex_repo,
        preferences_service,
        library_db,
        mbid_store,
        get_per_user_client_factory(),
    )


@singleton
def get_target_plex_library_service() -> "PlexLibraryService":
    from services.plex_library_service import PlexLibraryService

    return PlexLibraryService(
        get_plex_repository(),
        get_preferences_service(),
        get_target_library_repository(),
        get_mbid_store(),
        get_per_user_client_factory(),
    )


@singleton
def get_plex_playback_service() -> "PlexPlaybackService":
    from services.plex_playback_service import PlexPlaybackService

    plex_repo = get_plex_repository()
    cache = get_cache()
    return PlexPlaybackService(plex_repo, cache, get_per_user_client_factory())


@singleton
def get_version_service() -> "VersionService":
    from services.version_service import VersionService

    github_repo = get_github_repository()
    return VersionService(github_repo)


def _build_spec_policy(policy):
    """Map the API ``DownloadPolicySettings`` onto the decoupled spec ``SpecPolicy`` (the
    composition root is the one place coupling the two is fine). The size/term gates
    activate when the user configures them; grab-time min-age stays off (its post-fail
    cousin ``usenet_min_release_age_minutes`` is NOT a grab-time gate)."""
    from services.native.acquisition.decision import SpecPolicy

    return SpecPolicy(
        quality_min=policy.quality_min,
        quality_max=policy.quality_max,
        max_size_mb=policy.max_size_mb,
        ignored_terms=tuple(policy.ignored_terms),
        required_terms=tuple(policy.required_terms),
        usenet_retention_days=policy.usenet_retention_days,
    )


@singleton
def get_album_preflight_scorer() -> "AlbumPreflightScorer":
    from services.native.album_preflight_scorer import AlbumPreflightScorer

    from .repo_providers import get_download_store

    policy = get_preferences_service().get_download_policy()
    return AlbumPreflightScorer(
        get_download_store(),
        flac_mp3_only=policy.flac_mp3_only,
        policy=_build_spec_policy(policy),
    )


@singleton
def get_track_matcher() -> "TrackMatcher":
    from services.native.track_matcher import TrackMatcher

    from .repo_providers import get_download_store

    policy = get_preferences_service().get_download_policy()
    return TrackMatcher(
        get_download_store(),
        quality_min=policy.quality_min,
        quality_max=policy.quality_max,
        flac_mp3_only=policy.flac_mp3_only,
    )


@singleton
def get_newznab_release_scorer() -> "NewznabReleaseScorer":
    from services.native.newznab_release_scorer import NewznabReleaseScorer

    from .repo_providers import get_download_store

    policy = get_preferences_service().get_download_policy()
    return NewznabReleaseScorer(
        get_download_store(),
        flac_mp3_only=policy.flac_mp3_only,
        policy=_build_spec_policy(policy),
    )


@singleton
def get_download_manifest_codec() -> "ManifestCodec":
    from models.download_manifest import ManifestCodec

    return ManifestCodec()


@singleton
def get_acquisition_cleanup_service() -> "AcquisitionCleanupService":
    from pathlib import Path

    from services.native.acquisition_cleanup_service import AcquisitionCleanupService

    from .cache_providers import get_native_library_store
    from .repo_providers import get_download_client_for_source, get_download_store

    return AcquisitionCleanupService(
        get_download_store(),
        get_native_library_store(),
        get_download_client_for_source,
        lambda: Path(
            get_preferences_service().get_sabnzbd_connection_raw().downloads_mount
        ),
    )


def _build_download_orchestrator(
    *, file_processor, library_manager, album_service, on_import_callback
) -> "DownloadOrchestrator":
    from pathlib import Path

    from core.config import get_settings
    from services.native.download_orchestrator import DownloadOrchestrator

    from .repo_providers import (
        get_download_client_repository,
        get_download_store,
        get_newznab_indexer,
        get_sabnzbd_download_client,
        get_slskd_indexer,
        get_wanted_store,
    )

    prefs = get_preferences_service()
    lib = prefs.get_typed_library_settings_raw()
    policy = prefs.get_download_policy()
    dc = prefs.get_download_client_settings_raw()
    sab = prefs.get_sabnzbd_connection_raw()
    usenet_enabled = prefs.is_usenet_ready()
    # manifest is metadata only (audio lands in the client's dir), so staging need not be
    # on the library filesystem; default it under cache_dir
    staging_path = (
        Path(lib.staging_path)
        if lib.staging_path
        else Path(get_settings().cache_dir) / "download-staging"
    )
    return DownloadOrchestrator(
        client=get_download_client_repository(),
        indexer=get_slskd_indexer(),
        download_store=get_download_store(),
        file_processor=file_processor,
        library_manager=library_manager,
        scorer=get_album_preflight_scorer(),
        track_matcher=get_track_matcher(),
        manifest_codec=get_download_manifest_codec(),
        event_bus=get_sse_publisher(),
        staging_path=staging_path,
        naming_template=lib.naming_template,
        auto_accept_threshold=policy.preflight_score_auto_accept,
        manual_threshold=policy.preflight_score_manual_min,
        stall_timeout_minutes=policy.download_stall_timeout_minutes,
        queued_timeout_minutes=policy.download_queued_timeout_minutes,
        max_failover_attempts=policy.max_failover_attempts,
        max_concurrent_downloads=policy.max_concurrent_downloads,
        auto_retry_enabled=policy.auto_retry_enabled,
        auto_retry_max_attempts=policy.auto_retry_max_attempts,
        auto_retry_base_interval_minutes=policy.auto_retry_base_interval_minutes,
        request_history=get_request_history_store(),
        on_import_callback=on_import_callback,
        usenet_indexer=get_newznab_indexer(),
        usenet_client=get_sabnzbd_download_client(),
        usenet_scorer=get_newznab_release_scorer(),
        usenet_enabled=usenet_enabled,
        soulseek_enabled=dc.enabled,
        source_priority=prefs.get_source_priority(),
        album_service=album_service,
        usenet_category=sab.category,
        usenet_priority=sab.priority,
        usenet_post_processing=sab.post_processing,
        usenet_min_release_age_minutes=policy.usenet_min_release_age_minutes,
        # Fresh reader (not the snapshot above) so an automatic re-dispatch re-gates a
        # stored candidate against the CURRENT quality range even mid-flight (Phase 2).
        get_download_policy=lambda: get_preferences_service().get_download_policy(),
        wanted_store=get_wanted_store(),
        cleanup_service=get_acquisition_cleanup_service(),
    )


@singleton
def get_download_orchestrator() -> "DownloadOrchestrator":
    return get_target_download_orchestrator()


@singleton
def get_target_download_orchestrator() -> "DownloadOrchestrator":
    return _build_download_orchestrator(
        file_processor=get_target_file_processor(),
        library_manager=get_target_library_repository(),
        album_service=get_target_album_service(),
        on_import_callback=_build_target_import_invalidation(
            get_cache(), get_disk_cache()
        ),
    )


def _build_download_service(
    library_repository,
    ownership_service=None,
    *,
    orchestrator=None,
    file_processor=None,
    album_service=None,
    library_reconciler=None,
    quota_service=None,
    release_pin_store=None,
) -> "DownloadService":
    from services.native.download_service import DownloadService

    from .repo_providers import (
        get_album_release_pin_store,
        get_download_client_repository,
        get_download_store,
        get_newznab_indexer,
        get_slskd_indexer,
    )

    prefs = get_preferences_service()
    dc = prefs.get_download_client_settings_raw()
    policy = prefs.get_download_policy()
    usenet_enabled = prefs.is_usenet_ready()
    # The service is "enabled" if ANY source can act (slskd OR usenet), so a Usenet-only
    # install isn't blocked by the slskd-disabled guard.
    return DownloadService(
        download_client=get_download_client_repository(),
        indexer=get_slskd_indexer(),
        scorer=get_album_preflight_scorer(),
        library_manager=library_repository,
        download_store=get_download_store(),
        event_bus=get_sse_publisher(),
        orchestrator=orchestrator or get_download_orchestrator(),
        file_processor=file_processor or get_file_processor(),
        matcher=get_musicbrainz_matcher(),
        musicbrainz=get_musicbrainz_repository(),
        album_service=album_service or get_album_service(),
        track_matcher=get_track_matcher(),
        auto_accept_threshold=policy.preflight_score_auto_accept,
        manual_threshold=policy.preflight_score_manual_min,
        enabled=dc.enabled or usenet_enabled,
        usenet_indexer=get_newznab_indexer(),
        usenet_scorer=get_newznab_release_scorer(),
        usenet_enabled=usenet_enabled,
        soulseek_enabled=dc.enabled,
        upgrade_allowed=policy.upgrade_allowed,
        quality_cutoff=policy.quality_cutoff,
        quota_service=quota_service or get_quota_service(),
        release_pin_store=release_pin_store or get_album_release_pin_store(),
        ownership_service=ownership_service,
        library_reconciler=library_reconciler,
    )


@singleton
def get_download_service() -> "DownloadService":
    return get_target_download_service()


@singleton
def get_target_download_service() -> "DownloadService":
    return _build_download_service(
        get_target_library_repository(),
        get_target_library_ownership_service(),
        orchestrator=get_target_download_orchestrator(),
        file_processor=get_target_file_processor(),
        album_service=get_target_album_service(),
        library_reconciler=get_target_import_library_service(),
        quota_service=get_target_quota_service(),
        release_pin_store=get_target_album_release_pin_store(),
    )
