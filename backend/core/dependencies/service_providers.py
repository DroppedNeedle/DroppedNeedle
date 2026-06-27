from __future__ import annotations

import asyncio
import logging

from infrastructure.cache.cache_keys import (
    library_raw_albums_key,
    library_requested_mbids_key,
    HOME_RESPONSE_PREFIX,
    ALBUM_INFO_PREFIX,
    ARTIST_INFO_PREFIX,
    LIBRARY_PREFIX,
    LIBRARY_ALBUM_DETAILS_PREFIX,
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
    get_preferences_service,
)
from .repo_providers import (
    get_library_repository,
    get_musicbrainz_repository,
    get_wikidata_repository,
    get_listenbrainz_repository,
    get_jellyfin_repository,
    get_navidrome_repository,
    get_plex_repository,
    get_coverart_repository,
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
def get_audio_tagger() -> "AudioTagger":
    from infrastructure.audio.tagger import AudioTagger

    return AudioTagger()


@singleton
def get_naming_template_engine() -> "NamingTemplateEngine":
    from services.native.naming import NamingTemplateEngine

    return NamingTemplateEngine()


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
        return preferences_service.get_library_settings_raw().acoustid_api_key

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
    )


@singleton
def get_file_processor() -> "FileProcessor":
    from pathlib import Path

    from core.config import get_settings
    from services.native.file_processor import FileProcessor

    from .repo_providers import get_download_client_repository

    lib = get_preferences_service().get_library_settings_raw()
    dc = get_preferences_service().get_download_client_settings_raw()
    return FileProcessor(
        get_audio_tagger(),
        naming_engine=get_naming_template_engine(),
        library_manager=get_library_manager(),
        library_paths=[Path(p) for p in lib.library_paths],
        client=get_download_client_repository(),
        slskd_downloads_path=Path(get_settings().slskd_downloads_path),
        fingerprinter=get_audio_fingerprinter(),
        verify_downloads=dc.verify_downloads,
    )


@singleton
def get_sse_publisher() -> "SSEPublisher":
    from infrastructure.sse_publisher import SSEPublisher

    return SSEPublisher()


@singleton
def get_now_playing_service() -> "NowPlayingService":
    from services.now_playing_service import NowPlayingService

    return NowPlayingService(get_sse_publisher(), get_user_listening_prefs_store())


@singleton
def get_search_service() -> "SearchService":
    from services.search_service import SearchService

    mb_repo = get_musicbrainz_repository()
    library_repo = get_library_repository()
    coverart_repo = get_coverart_repository()
    preferences_service = get_preferences_service()
    audiodb_image_service = get_audiodb_image_service()
    browse_queue = get_audiodb_browse_queue()
    return SearchService(mb_repo, library_repo, coverart_repo, preferences_service, audiodb_image_service, browse_queue)


@singleton
def get_artist_service() -> "ArtistService":
    from services.artist_service import ArtistService

    mb_repo = get_musicbrainz_repository()
    library_repo = get_library_repository()
    wikidata_repo = get_wikidata_repository()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    disk_cache = get_disk_cache()
    audiodb_image_service = get_audiodb_image_service()
    browse_queue = get_audiodb_browse_queue()
    library_db = get_library_db()
    return ArtistService(mb_repo, library_repo, wikidata_repo, preferences_service, memory_cache, disk_cache, audiodb_image_service, browse_queue, library_db)


@singleton
def get_follow_service() -> "FollowService":
    from services.follow_service import FollowService

    return FollowService(get_follow_store(), get_musicbrainz_repository())


@singleton
def get_new_release_service() -> "NewReleaseService":
    from services.native.new_release_service import NewReleaseService

    from .repo_providers import get_download_store

    return NewReleaseService(
        follow_store=get_follow_store(),
        mb_repo=get_musicbrainz_repository(),
        download_service=get_download_service(),
        download_store=get_download_store(),
        library_repo=get_library_repository(),
        sse_publisher=get_sse_publisher(),
    )


@singleton
def get_album_service() -> "AlbumService":
    from services.album_service import AlbumService

    library_repo = get_library_repository()
    mb_repo = get_musicbrainz_repository()
    library_db = get_library_db()
    memory_cache = get_cache()
    disk_cache = get_disk_cache()
    preferences_service = get_preferences_service()
    audiodb_image_service = get_audiodb_image_service()
    browse_queue = get_audiodb_browse_queue()
    return AlbumService(library_repo, mb_repo, library_db, memory_cache, disk_cache, preferences_service, audiodb_image_service, browse_queue)


@singleton
def get_request_service() -> "RequestService":
    from services.request_service import RequestService

    request_history = get_request_history_store()
    return RequestService(request_history, get_download_service())


def _build_import_invalidation(memory_cache, disk_cache, library_db):
    """The canonical 'an album just landed in the library' invalidation: bust the
    album/library/home caches and materialise the album row. Shared by the requests
    reconciler and the download orchestrator's terminal-state bridge so a completed
    download surfaces in the UI immediately."""

    async def on_import(record: RequestHistoryRecord) -> None:
        invalidations = [
            memory_cache.delete(library_raw_albums_key()),
            memory_cache.clear_prefix(f"{LIBRARY_PREFIX}library:"),
            memory_cache.delete(library_requested_mbids_key()),
            memory_cache.clear_prefix(HOME_RESPONSE_PREFIX),
            memory_cache.delete(f"{ALBUM_INFO_PREFIX}{record.musicbrainz_id}"),
            memory_cache.delete(f"{LIBRARY_ALBUM_DETAILS_PREFIX}{record.musicbrainz_id}"),
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
        try:
            await library_db.upsert_album({
                "mbid": record.musicbrainz_id,
                "artist_mbid": record.artist_mbid or "",
                "artist_name": record.artist_name or "",
                "title": record.album_title or "",
                "year": record.year,
                "cover_url": record.cover_url or "",
            })
        except Exception as ex:  # noqa: BLE001
            logger.warning("Failed to upsert album into library cache: %s", ex)

    return on_import


@singleton
def get_requests_page_service() -> "RequestsPageService":
    from services.requests_page_service import RequestsPageService

    library_repo = get_library_repository()
    request_history = get_request_history_store()
    memory_cache = get_cache()
    disk_cache = get_disk_cache()
    library_db = get_library_db()

    on_import = _build_import_invalidation(memory_cache, disk_cache, library_db)

    library_service = get_library_service()

    async def merged_library_mbids() -> set[str]:
        return set(await library_service.get_library_mbids())

    from .repo_providers import get_download_store

    return RequestsPageService(
        library_repo=library_repo,
        request_history=request_history,
        library_mbids_fn=merged_library_mbids,
        on_import_callback=on_import,
        download_service=get_download_service(),
        download_store=get_download_store(),
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
        library_repo, library_db, cover_repo, preferences_service,
        memory_cache, disk_cache,
        artist_discovery_service=artist_discovery_service,
        audiodb_image_service=audiodb_image_service,
        local_files_service=local_files_service,
        jellyfin_library_service=jellyfin_library_service,
        navidrome_library_service=navidrome_library_service,
        sync_state_store=sync_state_store,
        genre_index=genre_index,
    )


@singleton
def get_status_service() -> "StatusService":
    from services.status_service import StatusService

    from .repo_providers import get_download_client_repository

    return StatusService(get_download_client_repository(), get_library_manager())


@singleton
def get_home_service() -> "HomeService":
    from services.home_service import HomeService
    from core.config import get_settings

    settings = get_settings()
    listenbrainz_repo = get_listenbrainz_repository()
    jellyfin_repo = get_jellyfin_repository()
    library_repo = get_library_repository()
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
        play_history_store=get_play_history_store(),
    )


@singleton
def get_genre_cover_prewarm_service() -> "GenreCoverPrewarmService":
    from services.genre_cover_prewarm_service import GenreCoverPrewarmService

    cover_repo = get_coverart_repository()
    return GenreCoverPrewarmService(cover_repo=cover_repo)


@singleton
def get_home_charts_service() -> "HomeChartsService":
    from services.home_charts_service import HomeChartsService

    listenbrainz_repo = get_listenbrainz_repository()
    library_repo = get_library_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    genre_index = get_genre_index()
    lastfm_repo = get_lastfm_repository()
    preferences_service = get_preferences_service()
    prewarm_service = get_genre_cover_prewarm_service()
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
    )


@singleton
def get_settings_service() -> "SettingsService":
    from services.settings_service import SettingsService

    preferences_service = get_preferences_service()
    cache = get_cache()
    return SettingsService(preferences_service, cache)


@singleton
def get_artist_discovery_service() -> "ArtistDiscoveryService":
    from services.artist_discovery_service import ArtistDiscoveryService
    from core.dependencies.auth_providers import get_auth_store

    listenbrainz_repo = get_listenbrainz_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    library_db = get_library_db()
    library_repo = get_library_repository()
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


@singleton
def get_album_discovery_service() -> "AlbumDiscoveryService":
    from services.album_discovery_service import AlbumDiscoveryService

    listenbrainz_repo = get_listenbrainz_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    library_db = get_library_db()
    library_repo = get_library_repository()
    return AlbumDiscoveryService(
        listenbrainz_repo=listenbrainz_repo,
        musicbrainz_repo=musicbrainz_repo,
        library_db=library_db,
        library_repo=library_repo,
        client_factory=get_per_user_client_factory(),
    )


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
def get_scrobble_service() -> "ScrobbleService":
    from services.scrobble_service import ScrobbleService

    return ScrobbleService(
        client_factory=get_per_user_client_factory(),
        listening_prefs_store=get_user_listening_prefs_store(),
        play_history_store=get_play_history_store(),
    )


@singleton
def get_discover_service() -> "DiscoverService":
    from services.discover_service import DiscoverService
    from services.discover.radio_service import DiscoverRadioService
    from services.discover.mbid_resolution_service import MbidResolutionService
    from services.discover.integration_helpers import IntegrationHelpers
    from services.home_transformers import HomeDataTransformers

    listenbrainz_repo = get_listenbrainz_repository()
    jellyfin_repo = get_jellyfin_repository()
    library_repo = get_library_repository()
    musicbrainz_repo = get_musicbrainz_repository()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    library_db = get_library_db()
    mbid_store = get_mbid_store()
    wikidata_repo = get_wikidata_repository()
    lastfm_repo = get_lastfm_repository()
    audiodb_image_service = get_audiodb_image_service()
    genre_index = get_genre_index()

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
        artist_discovery=get_artist_discovery_service(),
        album_discovery=get_album_discovery_service(),
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
        playlist_service=get_playlist_service(),
        client_factory=get_per_user_client_factory(),
        listening_prefs_store=get_user_listening_prefs_store(),
    )


@singleton
def get_discover_queue_manager() -> "DiscoverQueueManager":
    from services.discover_queue_manager import DiscoverQueueManager

    discover_service = get_discover_service()
    preferences_service = get_preferences_service()
    cover_repo = get_coverart_repository()
    return DiscoverQueueManager(discover_service, preferences_service, cover_repo=cover_repo)


@singleton
def get_jellyfin_playback_service() -> "JellyfinPlaybackService":
    from services.jellyfin_playback_service import JellyfinPlaybackService

    jellyfin_repo = get_jellyfin_repository()
    cache = get_cache()
    return JellyfinPlaybackService(jellyfin_repo, cache)


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
    return JellyfinLibraryService(jellyfin_repo, preferences_service)


@singleton
def get_navidrome_library_service() -> "NavidromeLibraryService":
    from services.navidrome_library_service import NavidromeLibraryService

    navidrome_repo = get_navidrome_repository()
    preferences_service = get_preferences_service()
    library_db = get_library_db()
    mbid_store = get_mbid_store()
    return NavidromeLibraryService(navidrome_repo, preferences_service, library_db, mbid_store)


@singleton
def get_navidrome_playback_service() -> "NavidromePlaybackService":
    from services.navidrome_playback_service import NavidromePlaybackService

    navidrome_repo = get_navidrome_repository()
    cache = get_cache()
    return NavidromePlaybackService(navidrome_repo, cache)


@singleton
def get_plex_library_service() -> "PlexLibraryService":
    from services.plex_library_service import PlexLibraryService

    plex_repo = get_plex_repository()
    preferences_service = get_preferences_service()
    library_db = get_library_db()
    mbid_store = get_mbid_store()
    return PlexLibraryService(plex_repo, preferences_service, library_db, mbid_store)


@singleton
def get_plex_playback_service() -> "PlexPlaybackService":
    from services.plex_playback_service import PlexPlaybackService

    plex_repo = get_plex_repository()
    cache = get_cache()
    return PlexPlaybackService(plex_repo, cache)


@singleton
def get_version_service() -> "VersionService":
    from services.version_service import VersionService

    github_repo = get_github_repository()
    return VersionService(github_repo)


@singleton
def get_album_preflight_scorer() -> "AlbumPreflightScorer":
    from services.native.album_preflight_scorer import AlbumPreflightScorer

    from .repo_providers import get_download_store

    dc = get_preferences_service().get_download_client_settings_raw()
    return AlbumPreflightScorer(
        get_download_store(),
        quality_min=dc.quality_min,
        quality_max=dc.quality_max,
        flac_mp3_only=dc.flac_mp3_only,
    )


@singleton
def get_track_matcher() -> "TrackMatcher":
    from services.native.track_matcher import TrackMatcher

    from .repo_providers import get_download_store

    dc = get_preferences_service().get_download_client_settings_raw()
    return TrackMatcher(
        get_download_store(),
        quality_min=dc.quality_min,
        quality_max=dc.quality_max,
        flac_mp3_only=dc.flac_mp3_only,
    )


@singleton
def get_download_manifest_codec() -> "ManifestCodec":
    from models.download_manifest import ManifestCodec

    return ManifestCodec()


@singleton
def get_download_orchestrator() -> "DownloadOrchestrator":
    from pathlib import Path

    from core.config import get_settings
    from services.native.download_orchestrator import DownloadOrchestrator

    from .repo_providers import get_download_client_repository, get_download_store

    lib = get_preferences_service().get_library_settings_raw()
    dc = get_preferences_service().get_download_client_settings_raw()
    # manifest is metadata only (audio lands in slskd's dir), so staging need not be on
    # the library filesystem; default it under cache_dir
    staging_path = (
        Path(lib.staging_path) if lib.staging_path
        else Path(get_settings().cache_dir) / "download-staging"
    )
    return DownloadOrchestrator(
        client=get_download_client_repository(),
        download_store=get_download_store(),
        file_processor=get_file_processor(),
        library_manager=get_library_manager(),
        scorer=get_album_preflight_scorer(),
        track_matcher=get_track_matcher(),
        manifest_codec=get_download_manifest_codec(),
        event_bus=get_sse_publisher(),
        staging_path=staging_path,
        naming_template=lib.naming_template,
        auto_accept_threshold=dc.preflight_score_auto_accept,
        manual_threshold=dc.preflight_score_manual_min,
        stall_timeout_minutes=dc.download_stall_timeout_minutes,
        queued_timeout_minutes=dc.download_queued_timeout_minutes,
        max_failover_attempts=dc.max_failover_attempts,
        max_concurrent_downloads=dc.max_concurrent_downloads,
        auto_retry_enabled=dc.auto_retry_enabled,
        auto_retry_max_attempts=dc.auto_retry_max_attempts,
        auto_retry_base_interval_minutes=dc.auto_retry_base_interval_minutes,
        request_history=get_request_history_store(),
        on_import_callback=_build_import_invalidation(
            get_cache(), get_disk_cache(), get_library_db()
        ),
    )


@singleton
def get_download_service() -> "DownloadService":
    from services.native.download_service import DownloadService

    from .repo_providers import get_download_client_repository, get_download_store

    dc = get_preferences_service().get_download_client_settings_raw()
    return DownloadService(
        download_client=get_download_client_repository(),
        scorer=get_album_preflight_scorer(),
        library_manager=get_library_repository(),
        download_store=get_download_store(),
        event_bus=get_sse_publisher(),
        orchestrator=get_download_orchestrator(),
        matcher=get_musicbrainz_matcher(),
        musicbrainz=get_musicbrainz_repository(),
        album_service=get_album_service(),
        auto_accept_threshold=dc.preflight_score_auto_accept,
        manual_threshold=dc.preflight_score_manual_min,
        enabled=dc.enabled,
    )
