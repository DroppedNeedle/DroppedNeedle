from __future__ import annotations

import logging
import re
import uuid

import httpx

from core.config import get_settings
from infrastructure.http.client import (
    HttpClientFactory,
    get_coverart_http_client,
    get_http_client,
    get_listenbrainz_http_client,
)

from ._registry import singleton
from .cache_providers import (
    get_cache,
    get_disk_cache,
    get_library_db,
    get_mbid_store,
    get_preferences_service,
)

logger = logging.getLogger(__name__)


def _get_configured_http_client() -> httpx.AsyncClient:
    settings = get_settings()
    advanced = get_preferences_service().get_advanced_settings()
    return get_http_client(
        settings,
        timeout=float(advanced.http_timeout),
        connect_timeout=float(advanced.http_connect_timeout),
        max_connections=advanced.http_max_connections,
    )


@singleton
def get_library_repository() -> "LibraryRepositoryProtocol":
    # answers the wide legacy surface for services not yet migrated to the native engine
    from services.native.library_manager import LibraryManager

    return LibraryManager(get_library_db())


@singleton
def get_musicbrainz_repository() -> "MusicBrainzRepository":
    from repositories.musicbrainz_repository import MusicBrainzRepository

    cache = get_cache()
    preferences_service = get_preferences_service()
    http_client = _get_configured_http_client()
    return MusicBrainzRepository(http_client, cache, preferences_service)


@singleton
def get_musicbrainz_identification_repository() -> (
    "MusicBrainzIdentificationRepository"
):
    from repositories.musicbrainz_identification_repository import (
        MusicBrainzIdentificationRepository,
    )

    return MusicBrainzIdentificationRepository(get_musicbrainz_repository())


@singleton
def get_wikidata_repository() -> "WikidataRepository":
    from repositories.wikidata_repository import WikidataRepository

    cache = get_cache()
    http_client = _get_configured_http_client()
    return WikidataRepository(http_client, cache)


@singleton
def get_listenbrainz_repository() -> "ListenBrainzRepository":
    from repositories.listenbrainz_repository import ListenBrainzRepository

    cache = get_cache()
    http_client = get_listenbrainz_http_client(
        settings=get_settings(),
        timeout=float(get_preferences_service().get_advanced_settings().http_timeout),
        connect_timeout=float(
            get_preferences_service().get_advanced_settings().http_connect_timeout
        ),
    )
    preferences = get_preferences_service()
    lb_settings = preferences.get_listenbrainz_connection()
    has_global_token = bool(lb_settings.enabled and lb_settings.user_token)
    # This global/enrichment repo has no token of its own unless a global LB
    # connection is configured. LB now anti-scraper-gates anonymous popularity
    # calls, so borrow a connected user's token (any account - popularity is public
    # data) for reads. Resolved lazily on first tokenless read; never for writes.
    fallback_token_provider = None
    if not has_global_token:
        connections_store = get_user_connections_store()

        async def fallback_token_provider() -> str | None:
            return await connections_store.get_service_token("listenbrainz")

    return ListenBrainzRepository(
        http_client=http_client,
        cache=cache,
        username=lb_settings.username if lb_settings.enabled else "",
        user_token=lb_settings.user_token if lb_settings.enabled else "",
        fallback_token_provider=fallback_token_provider,
    )


@singleton
def get_jellyfin_repository() -> "JellyfinRepository":
    from repositories.jellyfin_repository import JellyfinRepository

    cache = get_cache()
    mbid_store = get_mbid_store()
    http_client = _get_configured_http_client()
    preferences = get_preferences_service()
    jf_settings = preferences.get_jellyfin_connection()
    return JellyfinRepository(
        http_client=http_client,
        cache=cache,
        base_url=jf_settings.jellyfin_url if jf_settings.enabled else "",
        api_key=jf_settings.api_key if jf_settings.enabled else "",
        user_id=jf_settings.user_id if jf_settings.enabled else "",
        mbid_store=mbid_store,
    )


@singleton
def get_navidrome_repository() -> "NavidromeRepository":
    from repositories.navidrome_repository import NavidromeRepository

    cache = get_cache()
    http_client = _get_configured_http_client()
    preferences = get_preferences_service()
    nd_settings = preferences.get_navidrome_connection_raw()
    repo = NavidromeRepository(http_client=http_client, cache=cache)
    if nd_settings.enabled:
        repo.configure(
            url=nd_settings.navidrome_url,
            username=nd_settings.username,
            password=nd_settings.password,
        )
    adv = preferences.get_advanced_settings()
    repo.configure_cache_ttls(
        list_ttl=getattr(adv, "cache_ttl_navidrome_albums", 300),
        search_ttl=getattr(adv, "cache_ttl_navidrome_search", 120),
        genres_ttl=getattr(adv, "cache_ttl_navidrome_genres", 3600),
        detail_ttl=getattr(adv, "cache_ttl_navidrome_albums", 300),
    )
    return repo


@singleton
def get_navidrome_folder_preferences_store() -> "NavidromeFolderPreferencesStore":
    from infrastructure.persistence.navidrome_folder_preferences_store import (
        NavidromeFolderPreferencesStore,
    )

    from .cache_providers import get_persistence_write_lock

    return NavidromeFolderPreferencesStore(
        db_path=get_settings().library_db_path,
        write_lock=get_persistence_write_lock(),
    )


@singleton
def get_plex_repository() -> "PlexRepository":
    from repositories.plex_repository import PlexRepository

    cache = get_cache()
    http_client = _get_configured_http_client()
    preferences = get_preferences_service()
    plex_settings = preferences.get_plex_connection_raw()
    repo = PlexRepository(http_client=http_client, cache=cache)
    if plex_settings.enabled:
        # plex.tv account calls (the admin user import) return 400 without
        # X-Plex-Client-Identifier, and the id is otherwise created only by the Plex
        # OAuth login flow - an admin who set up Plex by pasting a token has none yet.
        client_id = preferences.get_or_create_setting(
            "plex_client_id", lambda: str(uuid.uuid4())
        )
        repo.configure(
            url=plex_settings.plex_url,
            token=plex_settings.plex_token,
            client_id=client_id,
        )
    adv = preferences.get_advanced_settings()
    repo.configure_cache_ttls(
        list_ttl=adv.cache_ttl_plex_albums,
        search_ttl=adv.cache_ttl_plex_search,
        genres_ttl=adv.cache_ttl_plex_genres,
        detail_ttl=adv.cache_ttl_plex_albums,
        stats_ttl=adv.cache_ttl_plex_stats,
    )
    return repo


@singleton
def get_youtube_repo() -> "YouTubeRepository":
    from repositories.youtube import YouTubeRepository

    http_client = _get_configured_http_client()
    preferences_service = get_preferences_service()
    yt_settings = preferences_service.get_youtube_connection()
    api_key = (
        yt_settings.api_key.strip()
        if (
            yt_settings.enabled
            and yt_settings.api_enabled
            and yt_settings.has_valid_api_key()
        )
        else ""
    )
    return YouTubeRepository(
        http_client=http_client,
        api_key=api_key,
        daily_quota_limit=yt_settings.daily_quota_limit,
    )


@singleton
def get_audiodb_repository() -> "AudioDBRepository":
    from repositories.audiodb_repository import AudioDBRepository

    settings = get_settings()
    http_client = _get_configured_http_client()
    preferences_service = get_preferences_service()
    return AudioDBRepository(
        http_client=http_client,
        preferences_service=preferences_service,
        api_key=settings.audiodb_api_key,
        premium=settings.audiodb_premium,
    )


@singleton
def get_audiodb_image_service() -> "AudioDBImageService":
    from services.audiodb_image_service import AudioDBImageService

    audiodb_repo = get_audiodb_repository()
    disk_cache = get_disk_cache()
    preferences_service = get_preferences_service()
    memory_cache = get_cache()
    return AudioDBImageService(
        audiodb_repo=audiodb_repo,
        disk_cache=disk_cache,
        preferences_service=preferences_service,
        memory_cache=memory_cache,
    )


@singleton
def get_audiodb_browse_queue() -> "AudioDBBrowseQueue":
    from services.audiodb_browse_queue import AudioDBBrowseQueue

    return AudioDBBrowseQueue()


@singleton
def get_lastfm_repository() -> "LastFmRepository":
    from repositories.lastfm_repository import LastFmRepository

    http_client = _get_configured_http_client()
    preferences = get_preferences_service()
    lf_settings = preferences.get_lastfm_connection()
    cache = get_cache()
    return LastFmRepository(
        http_client=http_client,
        cache=cache,
        api_key=lf_settings.api_key,
        shared_secret=lf_settings.shared_secret,
        session_key=lf_settings.session_key,
    )


@singleton
def get_playlist_repository() -> "PlaylistRepository":
    from repositories.playlist_repository import PlaylistRepository
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return PlaylistRepository(
        db_path=settings.library_db_path,
        write_lock=get_persistence_write_lock(),
    )


@singleton
def get_request_history_store() -> "RequestHistoryStore":
    from infrastructure.persistence.request_history import RequestHistoryStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return RequestHistoryStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_wanted_store() -> "WantedStore":
    from infrastructure.persistence.wanted_store import WantedStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return WantedStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_archive_repository() -> "ArchiveRepository":
    from infrastructure.http.client import HttpClientFactory
    from repositories.archive_repository import ArchiveRepository

    # generous timeout: this client streams whole albums, not JSON
    http = HttpClientFactory.get_client(name="internet-archive", timeout=120.0)
    return ArchiveRepository(http)


@singleton
def get_free_music_store() -> "FreeMusicStore":
    from infrastructure.persistence.free_music_store import FreeMusicStore

    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return FreeMusicStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_itunes_repository() -> "ITunesRepository":
    from infrastructure.http.client import HttpClientFactory
    from repositories.itunes_repository import ITunesRepository

    # dedicated client name: the factory caches clients by name and the first
    # caller's kwargs win, so a non-default surface gets its own entry
    http = HttpClientFactory.get_client(name="itunes-search", timeout=10.0)
    return ITunesRepository(http)


@singleton
def get_drop_import_store() -> "DropImportStore":
    from infrastructure.persistence.drop_import_store import DropImportStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return DropImportStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_events_store() -> "EventsStore":
    from infrastructure.persistence.events_store import EventsStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return EventsStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_ticketmaster_repository() -> "TicketmasterRepository":
    """Keyed from the events settings; an events settings save must
    cache_clear this (and the other events providers) so a new key takes
    effect without a restart."""
    from repositories.ticketmaster_repository import TicketmasterRepository

    http = HttpClientFactory.get_client(name="ticketmaster", timeout=15.0)
    raw = get_preferences_service().get_events_settings_raw()
    return TicketmasterRepository(http, api_key=raw.ticketmaster_api_key)


@singleton
def get_skiddle_repository() -> "SkiddleRepository":
    """Keyed from the events settings; cleared on events settings save."""
    from repositories.skiddle_repository import SkiddleRepository

    http = HttpClientFactory.get_client(name="skiddle", timeout=15.0)
    raw = get_preferences_service().get_events_settings_raw()
    return SkiddleRepository(http, api_key=raw.skiddle_api_key)


@singleton
def get_geocoding_repository() -> "GeocodingRepository":
    from repositories.geocoding_repository import GeocodingRepository

    http = HttpClientFactory.get_client(name="open-meteo-geocoding", timeout=10.0)
    return GeocodingRepository(http)


@singleton
def get_user_connections_store() -> "UserConnectionsStore":
    from infrastructure.persistence.user_connections_store import UserConnectionsStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return UserConnectionsStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_user_listening_prefs_store() -> "UserListeningPrefsStore":
    from infrastructure.persistence.user_listening_prefs_store import (
        UserListeningPrefsStore,
    )
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return UserListeningPrefsStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_discovery_batch_store() -> "DiscoveryBatchStore":
    from infrastructure.persistence.discovery_batch_store import DiscoveryBatchStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return DiscoveryBatchStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_preview_repository() -> "PreviewRepository":
    from repositories.preview_repository import PreviewRepository

    return PreviewRepository(http_client=_get_configured_http_client())


@singleton
def get_user_section_prefs_store() -> "UserSectionPrefsStore":
    from infrastructure.persistence.user_section_prefs_store import (
        UserSectionPrefsStore,
    )
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return UserSectionPrefsStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_play_history_store() -> "PlayHistoryStore":
    from infrastructure.persistence.play_history_store import PlayHistoryStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return PlayHistoryStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


@singleton
def get_follow_store() -> "FollowStore":
    from infrastructure.persistence.follow_store import FollowStore
    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return FollowStore(
        db_path=settings.library_db_path, write_lock=get_persistence_write_lock()
    )


def _build_coverart_repository(*, library_repo=None, library_db=None):
    from repositories.coverart_repository import CoverArtRepository

    settings = get_settings()
    advanced = get_preferences_service().get_advanced_settings()
    cache = get_cache()
    mb_repo = get_musicbrainz_repository()
    jellyfin_repo = get_jellyfin_repository()
    audiodb_service = get_audiodb_image_service()
    audiodb_browse_queue = get_audiodb_browse_queue()
    # Covers get their own short-budget client, not the shared 10s default (see
    # get_coverart_http_client): a cover that can't be fetched fast degrades to a
    # placeholder + background warm rather than holding the request open.
    http_client = get_coverart_http_client(settings)
    cache_dir = settings.cache_dir / "covers"
    return CoverArtRepository(
        http_client,
        cache,
        mb_repo,
        library_repo,
        jellyfin_repo,
        audiodb_service=audiodb_service,
        audiodb_browse_queue=audiodb_browse_queue,
        cache_dir=cache_dir,
        cover_cache_max_size_mb=settings.cover_cache_max_size_mb,
        cover_memory_cache_max_entries=advanced.cover_memory_cache_max_entries,
        cover_memory_cache_max_bytes=advanced.cover_memory_cache_max_size_mb
        * 1024
        * 1024,
        cover_non_monitored_ttl_seconds=advanced.cache_ttl_recently_viewed_bytes,
        library_db=library_db,
    )


@singleton
def get_coverart_repository() -> "CoverArtRepository":
    return _build_coverart_repository(
        library_repo=get_library_repository(), library_db=get_library_db()
    )


@singleton
def get_target_coverart_repository() -> "CoverArtRepository":
    """Provider-only cover lookup with no retained legacy catalog authority."""

    return _build_coverart_repository()


@singleton
def get_github_repository() -> "GitHubRepository":
    from repositories.github_repository import GitHubRepository

    cache = get_cache()
    http_client = _get_configured_http_client()
    return GitHubRepository(http_client, cache)


@singleton
def get_download_store() -> "DownloadStore":
    from infrastructure.persistence.download_store import DownloadStore

    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return DownloadStore(
        db_path=settings.library_db_path,
        write_lock=get_persistence_write_lock(),
    )


@singleton
def get_album_release_pin_store() -> "AlbumReleasePinStore":
    from infrastructure.persistence.album_release_pin_store import AlbumReleasePinStore

    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return AlbumReleasePinStore(
        db_path=settings.library_db_path,
        write_lock=get_persistence_write_lock(),
    )


@singleton
def get_user_quota_store() -> "UserQuotaStore":
    from infrastructure.persistence.user_quota_store import UserQuotaStore

    from .cache_providers import get_persistence_write_lock

    settings = get_settings()
    return UserQuotaStore(
        db_path=settings.library_db_path,
        write_lock=get_persistence_write_lock(),
    )


@singleton
def get_slskd_client() -> "SlskdClient":
    from repositories.slskd.slskd_client import SlskdClient

    # raw settings: the client needs the real api_key, not the mask
    dc = get_preferences_service().get_download_client_settings_raw()
    # unique client name avoids the per-name timeout-caching issue
    http = HttpClientFactory.get_client(name="slskd", timeout=30.0, connect_timeout=5.0)
    return SlskdClient(http, dc.url, dc.api_key)


def _mount_with_subpath(mount_path: str, subpath: str):
    """Join an optional, user-set downloads subfolder onto the mount, confined to it.

    Lets a user whose mount points at a parent (e.g. the whole media share) aim the
    finder at slskd's actual downloads dir without re-mounting. Defence-in-depth: drop
    "", ".", ".." components even though the settings struct already sanitises."""
    from pathlib import Path

    mount = Path(mount_path)
    if not subpath:
        return mount
    safe = [p for p in re.split(r"[\\/]", subpath) if p and p not in (".", "..")]
    return mount.joinpath(*safe) if safe else mount


@singleton
def get_slskd_repository() -> "SlskdRepository":
    from repositories.slskd.slskd_repository import SlskdRepository

    settings = get_settings()
    dc = get_preferences_service().get_download_client_settings_raw()
    return SlskdRepository(
        client=get_slskd_client(),
        url=dc.url,
        api_key=dc.api_key,
        downloads_mount=_mount_with_subpath(
            settings.slskd_downloads_path, dc.downloads_subpath
        ),
        concurrent_searches=settings.download_client_concurrent_searches,
        concurrent_enqueues=settings.download_client_concurrent_enqueues,
    )


@singleton
def get_slskd_indexer() -> "SlskdIndexer":
    """The search side of slskd (D2): a thin ``IndexerProtocol`` adapter over the
    slskd download repo. Phase 0 wires this as the orchestrator/service indexer so
    Soulseek search behaviour is unchanged; Phase 1+ composes it with Newznab."""
    from repositories.slskd.slskd_indexer import SlskdIndexer

    return SlskdIndexer(get_slskd_repository())


@singleton
def get_newznab_indexer() -> "NewznabIndexer":
    """The aggregate Newznab indexer (D6): one ``IndexerProtocol`` fanning out across
    every enabled configured indexer. Empty until the user adds their own (guardrail 1)."""
    from repositories.newznab.newznab_client import NewznabClient
    from repositories.newznab.newznab_indexer import NewznabIndexer, NewznabIndexerEntry

    prefs = get_preferences_service()
    raw = prefs.get_indexers_raw()
    http = HttpClientFactory.get_client(
        name="newznab", timeout=30.0, connect_timeout=5.0
    )
    entries = [
        NewznabIndexerEntry(
            NewznabClient(
                http, s.url, s.api_key, indexer_id=s.id, indexer_name=s.name or s.url
            ),
            indexer_id=s.id,
            name=s.name or s.url,
            categories=s.categories,
            enabled=s.enabled,
            priority=s.priority,
        )
        for s in raw
    ]
    # Keep the search cache TTL BELOW the auto-retry interval (02-… §Rate-limiting) so a
    # delayed re-search actually re-hits the indexer instead of serving a stale result -
    # honoured even when the admin sets a sub-5-minute retry interval.
    retry_interval_s = (
        prefs.get_download_policy().auto_retry_base_interval_minutes * 60.0
    )
    search_cache_ttl = max(30.0, min(300.0, retry_interval_s * 0.5))
    return NewznabIndexer(entries, search_cache_ttl=search_cache_ttl)


def build_newznab_client(url: str, api_key: str) -> "NewznabClient":
    """Transient (not cached) client from caller-supplied credentials, for the
    indexer Test-connection route - validates what the admin typed before saving."""
    from repositories.newznab.newznab_client import NewznabClient

    http = HttpClientFactory.get_client(
        name="newznab-verify", timeout=30.0, connect_timeout=5.0
    )
    return NewznabClient(http, url, api_key, indexer_name=url)


def build_slskd_repository(url: str, api_key: str) -> "SlskdRepository":
    """Transient (not cached) repo from caller-supplied credentials.

    Test-connection validates what the admin typed before saving, so it needs a
    one-off repo from the submitted url/key, not the stored config. Distinct httpx
    client name so it never shares the live config.
    """
    from pathlib import Path

    from repositories.slskd.slskd_client import SlskdClient
    from repositories.slskd.slskd_repository import SlskdRepository

    settings = get_settings()
    http = HttpClientFactory.get_client(
        name="slskd-verify", timeout=30.0, connect_timeout=5.0
    )
    return SlskdRepository(
        client=SlskdClient(http, url, api_key),
        url=url,
        api_key=api_key,
        downloads_mount=Path(settings.slskd_downloads_path),
        concurrent_searches=settings.download_client_concurrent_searches,
        concurrent_enqueues=settings.download_client_concurrent_enqueues,
    )


@singleton
def get_lidarr_import_repository() -> "LidarrImportRepository":
    """Read-only Lidarr import client (LidarrImport). Stateless w.r.t. credentials - its
    methods take url/api_key per call (from submitted creds for Test, from PreferencesService
    for import), so a connection save needs no cache_clear here. Dedicated client name so the
    first-caller-wins timeout doesn't leak from another repo."""
    from repositories.lidarr_import import LidarrImportRepository

    http = HttpClientFactory.get_client(
        name="lidarr_import", timeout=30.0, connect_timeout=5.0
    )
    return LidarrImportRepository(http)


@singleton
def get_sabnzbd_client() -> "SabnzbdClient":
    from repositories.sabnzbd.sabnzbd_client import SabnzbdClient

    sab = get_preferences_service().get_sabnzbd_connection_raw()
    http = HttpClientFactory.get_client(
        name="sabnzbd", timeout=60.0, connect_timeout=5.0
    )
    return SabnzbdClient(http, sab.url, sab.api_key)


@singleton
def get_sabnzbd_download_client() -> "SabnzbdDownloadClient":
    from pathlib import Path

    from repositories.sabnzbd.sabnzbd_download_client import SabnzbdDownloadClient

    sab = get_preferences_service().get_sabnzbd_connection_raw()
    return SabnzbdDownloadClient(
        get_sabnzbd_client(), sab.url, sab.api_key, Path(sab.downloads_mount)
    )


@singleton
def get_prowlarr_client() -> "ProwlarrClient":
    from repositories.prowlarr.prowlarr_client import ProwlarrClient

    pr = get_preferences_service().get_prowlarr_connection_raw()
    http = HttpClientFactory.get_client(
        name="prowlarr", timeout=60.0, connect_timeout=5.0
    )
    return ProwlarrClient(http, pr.url, pr.api_key)


@singleton
def get_prowlarr_indexer() -> "ProwlarrIndexer":
    """The single-connection Prowlarr indexer (fork feature): one ``IndexerProtocol``
    covering every indexer the user manages in Prowlarr. Returns BOTH ``usenet`` and
    ``torrent`` result arms; each strategy filters the arm it owns."""
    from repositories.prowlarr.prowlarr_indexer import ProwlarrIndexer

    prefs = get_preferences_service()
    pr = prefs.get_prowlarr_connection_raw()
    retry_interval_s = (
        prefs.get_download_policy().auto_retry_base_interval_minutes * 60.0
    )
    search_cache_ttl = max(30.0, min(300.0, retry_interval_s * 0.5))
    return ProwlarrIndexer(
        get_prowlarr_client(),
        categories=pr.categories,
        enabled=pr.enabled and bool(pr.url and pr.api_key),
        search_cache_ttl=search_cache_ttl,
    )


def get_usenet_search_indexer() -> "IndexerProtocol":
    """The indexer the USENET strategy searches with: Prowlarr when configured
    (one connection, all indexers), else the per-indexer Newznab fan-out."""
    if get_preferences_service().is_prowlarr_configured():
        return get_prowlarr_indexer()
    return get_newznab_indexer()


def build_prowlarr_client(url: str, api_key: str) -> "ProwlarrClient":
    """Transient (not cached) client from caller-supplied credentials, for the
    Prowlarr Test-connection route."""
    from repositories.prowlarr.prowlarr_client import ProwlarrClient

    http = HttpClientFactory.get_client(
        name="prowlarr-verify", timeout=30.0, connect_timeout=5.0
    )
    return ProwlarrClient(http, url, api_key)


@singleton
def get_qbittorrent_client() -> "QbittorrentClient":
    from repositories.qbittorrent.qbittorrent_client import QbittorrentClient

    qbt = get_preferences_service().get_qbittorrent_connection_raw()
    http = HttpClientFactory.get_client(
        name="qbittorrent", timeout=60.0, connect_timeout=5.0
    )
    return QbittorrentClient(http, qbt.url, qbt.api_key)


@singleton
def get_qbittorrent_download_client() -> "QbittorrentDownloadClient":
    from pathlib import Path

    from repositories.qbittorrent.qbittorrent_download_client import (
        QbittorrentDownloadClient,
    )

    qbt = get_preferences_service().get_qbittorrent_connection_raw()
    return QbittorrentDownloadClient(
        get_qbittorrent_client(),
        qbt.url,
        qbt.api_key,
        Path(qbt.downloads_mount),
        category=qbt.category,
    )


def build_qbittorrent_download_client(
    url: str, api_key: str
) -> "QbittorrentDownloadClient":
    """Transient client from caller-supplied credentials, for the Test-connection route."""
    from pathlib import Path

    from repositories.qbittorrent.qbittorrent_client import QbittorrentClient
    from repositories.qbittorrent.qbittorrent_download_client import (
        QbittorrentDownloadClient,
    )

    http = HttpClientFactory.get_client(
        name="qbittorrent-verify", timeout=60.0, connect_timeout=5.0
    )
    return QbittorrentDownloadClient(
        QbittorrentClient(http, url, api_key),
        url,
        api_key,
        Path("/tmp"),
    )


def build_sabnzbd_download_client(url: str, api_key: str) -> "SabnzbdDownloadClient":
    """Transient client from caller-supplied credentials, for the Test-connection route."""
    from pathlib import Path

    from repositories.sabnzbd.sabnzbd_client import SabnzbdClient
    from repositories.sabnzbd.sabnzbd_download_client import SabnzbdDownloadClient

    http = HttpClientFactory.get_client(
        name="sabnzbd-verify", timeout=60.0, connect_timeout=5.0
    )
    return SabnzbdDownloadClient(
        SabnzbdClient(http, url, api_key), url, api_key, Path("/tmp")
    )


_DOWNLOAD_CLIENT_PROVIDERS = {
    "slskd": get_slskd_repository,
    "sabnzbd": get_sabnzbd_download_client,
    "qbittorrent": get_qbittorrent_download_client,
}


def get_download_client(client_type: str) -> "DownloadClientProtocol":
    """Resolve a concrete adapter from the single client registry."""
    from core.exceptions import ConfigurationError

    provider = _DOWNLOAD_CLIENT_PROVIDERS.get(client_type)
    if provider is None:
        raise ConfigurationError(f"Unknown download client type: {client_type!r}")
    return provider()


def get_source_download_client(source: str, client_type: str) -> "DownloadClientProtocol":
    """Resolve a configured client and verify that it supports the source."""
    from core.exceptions import ConfigurationError

    client = get_download_client(client_type)
    if source not in client.supported_sources:
        raise ConfigurationError(
            f"Download client {client_type!r} does not support source {source!r}"
        )
    return client


@singleton
def get_download_client_repository() -> "DownloadClientProtocol":
    dc = get_preferences_service().get_download_client_settings()
    return get_source_download_client("soulseek", dc.client_type)
