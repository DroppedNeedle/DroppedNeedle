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
    get_preview_repository,
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
        invalidate_albums=_build_scan_invalidation(get_cache(), get_disk_cache()),
    )


@singleton
def get_file_processor() -> "FileProcessor":
    from pathlib import Path

    from core.config import get_settings
    from services.native.file_processor import FileProcessor
    from services.native.recycle_bin import resolve_bin_path

    from .repo_providers import get_download_client_repository, get_download_store

    lib = get_preferences_service().get_library_settings_raw()
    policy = get_preferences_service().get_download_policy()
    return FileProcessor(
        get_audio_tagger(),
        naming_engine=get_naming_template_engine(),
        library_manager=get_library_manager(),
        library_paths=[Path(p) for p in lib.library_paths],
        client=get_download_client_repository(),
        slskd_downloads_path=Path(get_settings().slskd_downloads_path),
        fingerprinter=get_audio_fingerprinter(),
        verify_downloads=policy.verify_downloads,
        download_store=get_download_store(),
        held_dir=Path(get_settings().cache_dir) / "held",
        recycle_bin=resolve_bin_path(policy.recycle_bin_path, lib.library_paths),
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


@singleton
def get_free_music_service() -> "FreeMusicService":
    from services.native.free_music_service import FreeMusicService

    from .repo_providers import get_archive_repository, get_free_music_store

    return FreeMusicService(
        store=get_free_music_store(),
        archive=get_archive_repository(),
        drop_import=get_drop_import_service(),
        preferences_service=get_preferences_service(),
        sse_publisher=get_sse_publisher(),
    )


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


@singleton
def get_drop_import_service() -> "DropImportService":
    from core.config import get_settings
    from services.native.drop_import_service import DropImportService

    from .repo_providers import (
        get_drop_import_store,
        get_request_history_store,
        get_wanted_store,
    )

    canonical = _build_import_invalidation(get_cache(), get_disk_cache(), get_library_db())

    async def on_drop_import(*, mbid, artist_mbid, artist_name, title, year):  # noqa: ANN001, ANN202
        # the canonical invalidator takes a request record; a drop needs no
        # request to exist, so synthesize the album-shaped subset it reads
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

    return DropImportService(
        store=get_drop_import_store(),
        tagger=get_audio_tagger(),
        fingerprinter=get_audio_fingerprinter(),
        album_identifier=get_album_identifier(),
        mb_matcher=get_musicbrainz_matcher(),
        naming_engine=get_naming_template_engine(),
        library_manager=get_library_manager(),
        preferences_service=get_preferences_service(),
        request_history=get_request_history_store(),
        wanted_store=get_wanted_store(),
        sse_publisher=get_sse_publisher(),
        on_import=on_drop_import,
        staging_root=get_settings().root_app_dir / "imports",
    )


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
def get_lidarr_import_service() -> "LidarrImportService":
    from services.lidarr_import_service import LidarrImportService

    from .repo_providers import get_lidarr_import_repository

    return LidarrImportService(
        repo=get_lidarr_import_repository(),
        preferences=get_preferences_service(),
        follow_store=get_follow_store(),
        follow_service=get_follow_service(),
    )


@singleton
def get_new_release_service() -> "NewReleaseService":
    from services.native.new_release_service import NewReleaseService

    from .repo_providers import get_download_store

    return NewReleaseService(
        follow_store=get_follow_store(),
        mb_repo=get_musicbrainz_repository(),
        acquisition=get_acquisition_dispatcher(),
        download_store=get_download_store(),
        library_repo=get_library_repository(),
        sse_publisher=get_sse_publisher(),
    )


@singleton
def get_wanted_watcher_service() -> "WantedWatcherService":
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
        get_download_service=get_download_service,
        library_manager=get_library_repository(),
        album_service=get_album_service(),
        mb_repo=get_musicbrainz_repository(),
        sse_publisher=get_sse_publisher(),
        preferences=get_preferences_service(),
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


@singleton
def get_personal_mix_service() -> "PersonalMixService":
    from services.personal_mix_service import PersonalMixService
    from core.dependencies.auth_providers import get_auth_store

    return PersonalMixService(
        client_factory=get_per_user_client_factory(),
        mb_repo=get_musicbrainz_repository(),
        library_repo=get_library_repository(),
        playlist_service=get_playlist_service(),
        acquisition=get_acquisition_dispatcher(),
        listening_prefs_store=get_user_listening_prefs_store(),
        connections_store=get_user_connections_store(),
        auth_store=get_auth_store(),
    )


@singleton
def get_album_service() -> "AlbumService":
    from services.album_service import AlbumService

    library_repo = get_library_repository()
    mb_repo = get_musicbrainz_repository()
    library_db = get_library_db()
    memory_cache = get_cache()
    disk_cache = get_disk_cache()
    from .repo_providers import get_album_release_pin_store

    preferences_service = get_preferences_service()
    audiodb_image_service = get_audiodb_image_service()
    browse_queue = get_audiodb_browse_queue()
    return AlbumService(
        library_repo, mb_repo, library_db, memory_cache, disk_cache, preferences_service,
        audiodb_image_service, browse_queue,
        release_pin_store=get_album_release_pin_store(),
    )


@singleton
def get_quota_service() -> "QuotaService":
    from services.quota_service import QuotaService

    from .auth_providers import get_auth_store
    from .cache_providers import get_library_db
    from .repo_providers import get_download_store, get_user_quota_store

    return QuotaService(
        preferences=get_preferences_service(),
        user_quotas=get_user_quota_store(),
        request_history=get_request_history_store(),
        download_store=get_download_store(),
        library_db=get_library_db(),
        auth_store=get_auth_store(),
    )


@singleton
def get_request_service() -> "RequestService":
    from services.request_service import RequestService

    request_history = get_request_history_store()
    return RequestService(
        request_history,
        get_download_service=get_download_service,
        quota_service=get_quota_service(),
        acquisition=get_acquisition_dispatcher(),
    )


def _build_scan_invalidation(memory_cache, disk_cache):
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
        ]
        await asyncio.gather(*per_album, *shared, return_exceptions=True)
        await asyncio.gather(
            *(disk_cache.delete_album(rg) for rg in changed_rgs),
            return_exceptions=True,
        )

    return invalidate


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
        get_download_service=get_download_service,
        download_store=get_download_store(),
        acquisition=get_acquisition_dispatcher(),
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
def get_wrapped_service() -> "WrappedService":
    from services.wrapped_service import WrappedService
    from core.dependencies.auth_providers import get_auth_store

    return WrappedService(
        auth_store=get_auth_store(),
        client_factory=get_per_user_client_factory(),
        charts_service=get_home_charts_service(),
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
def get_scrobble_service() -> "ScrobbleService":
    from services.scrobble_service import ScrobbleService

    return ScrobbleService(
        client_factory=get_per_user_client_factory(),
        listening_prefs_store=get_user_listening_prefs_store(),
        play_history_store=get_play_history_store(),
        plugin_host=get_plugin_host(),
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
        lfm_repo=lastfm_repo,
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
        follow_service=get_follow_service(),
        cover_repo=get_coverart_repository(),
        preview_repo=get_preview_repository(),
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
    return NavidromePlaybackService(navidrome_repo, cache, get_per_user_client_factory())


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


def _resolve_acquisition_providers() -> tuple:
    """Resolve the per-source (indexer, download-client) pairs through the plugin
    registry - the acquisition seam. The built-in plugins return the exact same
    singletons the orchestrator used pre-plugins (``get_slskd_repository`` /
    ``get_slskd_indexer`` / ``get_newznab_indexer`` / ``get_sabnzbd_download_client``),
    so behaviour is unchanged; the direct providers remain only as a fallback for
    the (should-never-happen) case of a quarantined built-in."""
    from .plugin_providers import get_plugin_manager
    from .repo_providers import (
        get_download_client_repository,
        get_newznab_indexer,
        get_sabnzbd_download_client,
        get_slskd_indexer,
    )

    manager = get_plugin_manager()
    soulseek = manager.get_plugin("soulseek")
    usenet = manager.get_plugin("usenet")
    if soulseek is None or usenet is None:  # pragma: no cover - built-ins always load
        logger.error(
            "plugins.builtin_missing - falling back to direct providers",
            extra={"soulseek": soulseek is not None, "usenet": usenet is not None},
        )
    client = soulseek.get_download_client() if soulseek else get_download_client_repository()
    indexer = soulseek.get_indexer() if soulseek else get_slskd_indexer()
    usenet_client = usenet.get_download_client() if usenet else get_sabnzbd_download_client()
    usenet_indexer = usenet.get_indexer() if usenet else get_newznab_indexer()
    return client, indexer, usenet_client, usenet_indexer


@singleton
def get_download_orchestrator() -> "DownloadOrchestrator":
    from pathlib import Path

    from core.config import get_settings
    from services.native.download_orchestrator import DownloadOrchestrator

    from .repo_providers import get_download_store

    client, indexer, usenet_client, usenet_indexer = _resolve_acquisition_providers()
    prefs = get_preferences_service()
    lib = prefs.get_library_settings_raw()
    policy = prefs.get_download_policy()
    dc = prefs.get_download_client_settings_raw()
    sab = prefs.get_sabnzbd_connection_raw()
    usenet_enabled = prefs.is_usenet_ready()
    # manifest is metadata only (audio lands in the client's dir), so staging need not be
    # on the library filesystem; default it under cache_dir
    staging_path = (
        Path(lib.staging_path) if lib.staging_path
        else Path(get_settings().cache_dir) / "download-staging"
    )
    return DownloadOrchestrator(
        client=client,
        indexer=indexer,
        download_store=get_download_store(),
        file_processor=get_file_processor(),
        library_manager=get_library_manager(),
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
        on_import_callback=_build_import_invalidation(
            get_cache(), get_disk_cache(), get_library_db()
        ),
        usenet_indexer=usenet_indexer,
        usenet_client=usenet_client,
        usenet_scorer=get_newznab_release_scorer(),
        usenet_enabled=usenet_enabled,
        soulseek_enabled=dc.enabled,
        source_priority=prefs.get_source_priority(),
        album_service=get_album_service(),
        usenet_category=sab.category,
        usenet_priority=sab.priority,
        usenet_post_processing=sab.post_processing,
        usenet_min_release_age_minutes=policy.usenet_min_release_age_minutes,
        # Fresh reader (not the snapshot above) so an automatic re-dispatch re-gates a
        # stored candidate against the CURRENT quality range even mid-flight (Phase 2).
        get_download_policy=lambda: get_preferences_service().get_download_policy(),
    )


@singleton
def get_download_service() -> "DownloadService":
    from services.native.download_service import DownloadService

    from .repo_providers import (
        get_album_release_pin_store,
        get_download_store,
    )

    client, indexer, _usenet_client, usenet_indexer = _resolve_acquisition_providers()
    prefs = get_preferences_service()
    dc = prefs.get_download_client_settings_raw()
    policy = prefs.get_download_policy()
    usenet_enabled = prefs.is_usenet_ready()
    # The service is "enabled" if ANY source can act (slskd OR usenet), so a Usenet-only
    # install isn't blocked by the slskd-disabled guard.
    return DownloadService(
        download_client=client,
        indexer=indexer,
        scorer=get_album_preflight_scorer(),
        library_manager=get_library_repository(),
        download_store=get_download_store(),
        event_bus=get_sse_publisher(),
        orchestrator=get_download_orchestrator(),
        file_processor=get_file_processor(),
        matcher=get_musicbrainz_matcher(),
        musicbrainz=get_musicbrainz_repository(),
        album_service=get_album_service(),
        track_matcher=get_track_matcher(),
        auto_accept_threshold=policy.preflight_score_auto_accept,
        manual_threshold=policy.preflight_score_manual_min,
        enabled=dc.enabled or usenet_enabled,
        usenet_indexer=usenet_indexer,
        usenet_scorer=get_newznab_release_scorer(),
        usenet_enabled=usenet_enabled,
        soulseek_enabled=dc.enabled,
        upgrade_allowed=policy.upgrade_allowed,
        quality_cutoff=policy.quality_cutoff,
        quota_service=get_quota_service(),
        release_pin_store=get_album_release_pin_store(),
    )
