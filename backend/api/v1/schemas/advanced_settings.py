import msgspec

from infrastructure.msgspec_fastapi import AppStruct


def _validate_range(value: int | float, field_name: str, minimum: int | float, maximum: int | float) -> None:
    if value < minimum or value > maximum:
        raise msgspec.ValidationError(f"{field_name} must be between {minimum} and {maximum}")


def _coerce_positive_int(value: object, field_name: str) -> int:
    if value is None:
        raise msgspec.ValidationError(f"{field_name} cannot be null")
    try:
        result = int(float(value))
    except (TypeError, ValueError) as exc:
        raise msgspec.ValidationError(f"Invalid integer value for {field_name}: {value}") from exc
    if result <= 0:
        raise msgspec.ValidationError(f"{field_name} must be positive")
    return result


def _mask_api_key(key: str) -> str:
    if len(key) > 3:
        return f"***...{key[-3:]}"
    return "***"


def _is_masked_api_key(value: str) -> bool:
    # Known limitation: a real key starting with "***" would be treated as masked
    # and discarded on save, preserving the previous key instead.
    return value.startswith("***")


class AdvancedSettings(AppStruct):
    cache_ttl_album_library: int = 86400
    cache_ttl_album_non_library: int = 21600
    cache_ttl_artist_library: int = 21600
    cache_ttl_artist_non_library: int = 21600
    cache_ttl_artist_discovery_library: int = 21600
    cache_ttl_artist_discovery_non_library: int = 3600
    cache_ttl_search: int = 3600
    cache_ttl_local_files_recently_added: int = 120
    cache_ttl_jellyfin_recently_played: int = 300
    cache_ttl_jellyfin_favorites: int = 300
    cache_ttl_jellyfin_genres: int = 3600
    cache_ttl_jellyfin_library_stats: int = 600
    cache_ttl_navidrome_albums: int = 300
    cache_ttl_navidrome_artists: int = 300
    cache_ttl_navidrome_recent: int = 120
    cache_ttl_navidrome_favorites: int = 120
    cache_ttl_navidrome_search: int = 120
    cache_ttl_navidrome_genres: int = 3600
    cache_ttl_navidrome_stats: int = 600
    cache_ttl_plex_albums: int = 300
    cache_ttl_plex_search: int = 120
    cache_ttl_plex_genres: int = 3600
    cache_ttl_plex_stats: int = 600
    http_timeout: int = 10
    http_connect_timeout: int = 5
    http_max_connections: int = 200
    # Per-kind result ceiling for OpenSubsonic search2/search3.  The protocol
    # leaves page sizing to clients; keep a practical default while allowing
    # operators to accommodate clients that request larger pages.
    subsonic_search_page_limit: int = 1000
    batch_artist_images: int = 10
    batch_albums: int = 8
    delay_artist: float = 0.5
    delay_albums: float = 0.3
    artist_discovery_warm_interval: int = 14400
    artist_discovery_warm_delay: float = 0.5
    artist_discovery_precache_delay: float = 0.2
    artist_discovery_precache_concurrency: int = 5
    memory_cache_max_entries: int = 10000
    memory_cache_cleanup_interval: int = 300
    cover_memory_cache_max_entries: int = 128
    cover_memory_cache_max_size_mb: int = 16
    disk_cache_cleanup_interval: int = 600
    recent_metadata_max_size_mb: int = 500
    recent_covers_max_size_mb: int = 1024
    persistent_metadata_ttl_hours: int = 24
    discover_queue_size: int = 10
    discover_queue_ttl: int = 86400
    discover_queue_auto_generate: bool = True
    discover_queue_polling_interval: int = 4000
    discover_queue_warm_cycle_build: bool = True
    discover_queue_seed_artists: int = 3
    discover_queue_wildcard_slots: int = 2
    discover_queue_similar_artists_limit: int = 15
    discover_queue_albums_per_similar: int = 5
    discover_queue_enrich_ttl: int = 86400
    discover_queue_lastfm_mbid_max_lookups: int = 10
    discover_picks_genre_affinity_weight: float = 0.7
    discover_picks_count: int = 12
    frontend_ttl_home: int = 300000
    frontend_ttl_discover: int = 1800000
    frontend_ttl_library: int = 300000
    frontend_ttl_recently_added: int = 300000
    frontend_ttl_discover_queue: int = 86400000
    frontend_ttl_search: int = 300000
    frontend_ttl_local_files_sidebar: int = 120000
    frontend_ttl_jellyfin_sidebar: int = 120000
    frontend_ttl_plex_sidebar: int = 120000
    frontend_ttl_playlist_sources: int = 900000
    audiodb_enabled: bool = True
    audiodb_name_search_fallback: bool = False
    direct_remote_images_enabled: bool = True
    prefer_local_cover_art: bool = True
    audiodb_api_key: str = "123"
    cache_ttl_audiodb_found: int = 604800
    cache_ttl_audiodb_not_found: int = 86400
    cache_ttl_audiodb_library: int = 1209600
    cache_ttl_recently_viewed_bytes: int = 172800
    sync_stall_timeout_minutes: int = 10
    sync_max_timeout_hours: int = 8
    audiodb_prewarm_concurrency: int = 4
    audiodb_prewarm_delay: float = 0.3
    genre_section_ttl: int = 21600
    request_concurrency: int = 2
    request_history_retention_days: int = 180
    ignored_releases_retention_days: int = 365
    orphan_cover_demote_interval_hours: int = 24
    store_prune_interval_hours: int = 6

    def __post_init__(self) -> None:
        if not self.audiodb_api_key or not self.audiodb_api_key.strip():
            self.audiodb_api_key = "123"
        ranges: dict[str, tuple[int | float, int | float]] = {
            "cache_ttl_album_library": (3600, 604800),
            "cache_ttl_album_non_library": (60, 86400),
            "cache_ttl_artist_library": (3600, 604800),
            "cache_ttl_artist_non_library": (3600, 604800),
            "cache_ttl_artist_discovery_library": (3600, 604800),
            "cache_ttl_artist_discovery_non_library": (3600, 604800),
            "cache_ttl_search": (60, 86400),
            "cache_ttl_local_files_recently_added": (60, 3600),
            "cache_ttl_jellyfin_recently_played": (60, 3600),
            "cache_ttl_jellyfin_favorites": (60, 3600),
            "cache_ttl_jellyfin_genres": (60, 86400),
            "cache_ttl_jellyfin_library_stats": (60, 3600),
            "cache_ttl_navidrome_albums": (60, 3600),
            "cache_ttl_navidrome_artists": (60, 3600),
            "cache_ttl_navidrome_recent": (60, 3600),
            "cache_ttl_navidrome_favorites": (60, 3600),
            "cache_ttl_navidrome_search": (60, 3600),
            "cache_ttl_navidrome_genres": (60, 86400),
            "cache_ttl_navidrome_stats": (60, 3600),
            "cache_ttl_plex_albums": (60, 3600),
            "cache_ttl_plex_search": (60, 3600),
            "cache_ttl_plex_genres": (60, 86400),
            "cache_ttl_plex_stats": (60, 3600),
            "http_timeout": (5, 60),
            "http_connect_timeout": (1, 30),
            "http_max_connections": (50, 500),
            "subsonic_search_page_limit": (1, 2_147_483_647),
            "batch_artist_images": (1, 20),
            "batch_albums": (1, 20),
            "delay_artist": (0.0, 5.0),
            "delay_albums": (0.0, 5.0),
            "artist_discovery_warm_interval": (300, 604800),
            "artist_discovery_warm_delay": (0.0, 5.0),
            "artist_discovery_precache_delay": (0.0, 5.0),
            "memory_cache_max_entries": (1000, 100000),
            "memory_cache_cleanup_interval": (60, 3600),
            "cover_memory_cache_max_entries": (16, 2048),
            "cover_memory_cache_max_size_mb": (1, 1024),
            "disk_cache_cleanup_interval": (60, 3600),
            "recent_metadata_max_size_mb": (100, 5000),
            "recent_covers_max_size_mb": (100, 10000),
            "persistent_metadata_ttl_hours": (1, 168),
            "artist_discovery_precache_concurrency": (1, 8),
            "sync_stall_timeout_minutes": (2, 30),
            "sync_max_timeout_hours": (1, 48),
            "audiodb_prewarm_concurrency": (1, 8),
            "request_concurrency": (1, 5),
            "audiodb_prewarm_delay": (0.0, 5.0),
            "discover_queue_size": (1, 20),
            "discover_queue_ttl": (3600, 604800),
            "discover_queue_polling_interval": (1000, 30000),
            "discover_queue_seed_artists": (1, 10),
            "discover_queue_wildcard_slots": (0, 10),
            "discover_queue_similar_artists_limit": (5, 50),
            "discover_queue_albums_per_similar": (1, 20),
            "discover_queue_enrich_ttl": (3600, 604800),
            "discover_queue_lastfm_mbid_max_lookups": (1, 50),
            "discover_picks_genre_affinity_weight": (0.0, 1.0),
            "discover_picks_count": (4, 30),
            "frontend_ttl_home": (60000, 3600000),
            "frontend_ttl_discover": (60000, 86400000),
            "frontend_ttl_library": (60000, 3600000),
            "frontend_ttl_recently_added": (60000, 3600000),
            "frontend_ttl_discover_queue": (3600000, 604800000),
            "frontend_ttl_search": (60000, 3600000),
            "frontend_ttl_local_files_sidebar": (60000, 3600000),
            "frontend_ttl_jellyfin_sidebar": (60000, 3600000),
            "frontend_ttl_plex_sidebar": (60000, 3600000),
            "frontend_ttl_playlist_sources": (60000, 3600000),
            "cache_ttl_audiodb_found": (3600, 2592000),
            "cache_ttl_audiodb_not_found": (3600, 604800),
            "cache_ttl_audiodb_library": (86400, 2592000),
            "cache_ttl_recently_viewed_bytes": (3600, 604800),
            "genre_section_ttl": (3600, 604800),
            "request_history_retention_days": (30, 3650),
            "ignored_releases_retention_days": (30, 3650),
            "orphan_cover_demote_interval_hours": (1, 168),
            "store_prune_interval_hours": (1, 168),
        }
        for field_name, (minimum, maximum) in ranges.items():
            _validate_range(getattr(self, field_name), field_name, minimum, maximum)


class FrontendCacheTTLs(AppStruct):
    home: int = 300000
    discover: int = 1800000
    library: int = 300000
    recently_added: int = 300000
    discover_queue: int = 86400000
    search: int = 300000
    local_files_sidebar: int = 120000
    jellyfin_sidebar: int = 120000
    plex_sidebar: int = 120000
    playlist_sources: int = 900000
    discover_queue_polling_interval: int = 4000
    discover_queue_auto_generate: bool = True


class AdvancedSettingsFrontend(AppStruct):
    cache_ttl_album_library: int = 24
    cache_ttl_album_non_library: int = 6
    cache_ttl_artist_library: int = 6
    cache_ttl_artist_non_library: int = 6
    cache_ttl_artist_discovery_library: int = 6
    cache_ttl_artist_discovery_non_library: int = 1
    cache_ttl_search: int = 60
    cache_ttl_local_files_recently_added: int = 2
    cache_ttl_jellyfin_recently_played: int = 5
    cache_ttl_jellyfin_favorites: int = 5
    cache_ttl_jellyfin_genres: int = 60
    cache_ttl_jellyfin_library_stats: int = 10
    cache_ttl_navidrome_albums: int = 5
    cache_ttl_navidrome_artists: int = 5
    cache_ttl_navidrome_recent: int = 2
    cache_ttl_navidrome_favorites: int = 2
    cache_ttl_navidrome_search: int = 2
    cache_ttl_navidrome_genres: int = 60
    cache_ttl_navidrome_stats: int = 10
    cache_ttl_plex_albums: int = 5
    cache_ttl_plex_search: int = 2
    cache_ttl_plex_genres: int = 60
    cache_ttl_plex_stats: int = 10
    http_timeout: int = 10
    http_connect_timeout: int = 5
    http_max_connections: int = 200
    subsonic_search_page_limit: int = 1000
    batch_artist_images: int = 10
    batch_albums: int = 8
    delay_artist: float = 0.5
    delay_albums: float = 0.3
    artist_discovery_warm_interval: int = 240
    artist_discovery_warm_delay: float = 0.5
    artist_discovery_precache_delay: float = 0.2
    memory_cache_max_entries: int = 10000
    memory_cache_cleanup_interval: int = 300
    cover_memory_cache_max_entries: int = 128
    cover_memory_cache_max_size_mb: int = 16
    disk_cache_cleanup_interval: int = 10
    recent_metadata_max_size_mb: int = 500
    recent_covers_max_size_mb: int = 1024
    persistent_metadata_ttl_hours: int = 24
    discover_queue_size: int = 10
    discover_queue_ttl: int = 24
    discover_queue_auto_generate: bool = True
    discover_queue_polling_interval: int = 4
    discover_queue_warm_cycle_build: bool = True
    discover_queue_seed_artists: int = 3
    discover_queue_wildcard_slots: int = 2
    discover_queue_similar_artists_limit: int = 15
    discover_queue_albums_per_similar: int = 5
    discover_queue_enrich_ttl: int = 24
    discover_queue_lastfm_mbid_max_lookups: int = 10
    discover_picks_genre_affinity_weight: float = 0.7
    discover_picks_count: int = 12
    frontend_ttl_home: int = 5
    frontend_ttl_discover: int = 30
    frontend_ttl_library: int = 5
    frontend_ttl_recently_added: int = 5
    frontend_ttl_discover_queue: int = 1440
    frontend_ttl_search: int = 5
    frontend_ttl_local_files_sidebar: int = 2
    frontend_ttl_jellyfin_sidebar: int = 2
    frontend_ttl_plex_sidebar: int = 2
    frontend_ttl_playlist_sources: int = 15
    audiodb_enabled: bool = True
    audiodb_name_search_fallback: bool = False
    direct_remote_images_enabled: bool = True
    prefer_local_cover_art: bool = True
    audiodb_api_key: str = "123"
    cache_ttl_audiodb_found: int = 168
    cache_ttl_audiodb_not_found: int = 24
    cache_ttl_audiodb_library: int = 336
    cache_ttl_recently_viewed_bytes: int = 48
    genre_section_ttl: int = 6
    request_history_retention_days: int = 180
    ignored_releases_retention_days: int = 365
    orphan_cover_demote_interval_hours: int = 24
    store_prune_interval_hours: int = 6
    sync_stall_timeout_minutes: int = 10
    sync_max_timeout_hours: int = 8
    audiodb_prewarm_concurrency: int = 4
    audiodb_prewarm_delay: float = 0.3
    request_concurrency: int = 2
    artist_discovery_precache_concurrency: int = 5

    def __post_init__(self) -> None:
        int_coerce_fields = [
            "cache_ttl_album_library",
            "cache_ttl_album_non_library",
            "cache_ttl_artist_library",
            "cache_ttl_artist_non_library",
            "cache_ttl_artist_discovery_library",
            "cache_ttl_artist_discovery_non_library",
            "cache_ttl_search",
            "cache_ttl_local_files_recently_added",
            "cache_ttl_jellyfin_recently_played",
            "cache_ttl_jellyfin_favorites",
            "cache_ttl_jellyfin_genres",
            "cache_ttl_jellyfin_library_stats",
            "cache_ttl_navidrome_albums",
            "cache_ttl_navidrome_artists",
            "cache_ttl_navidrome_recent",
            "cache_ttl_navidrome_favorites",
            "cache_ttl_navidrome_search",
            "cache_ttl_navidrome_genres",
            "cache_ttl_navidrome_stats",
            "cache_ttl_plex_albums",
            "cache_ttl_plex_search",
            "cache_ttl_plex_genres",
            "cache_ttl_plex_stats",
            "cache_ttl_audiodb_found",
            "cache_ttl_audiodb_not_found",
            "cache_ttl_audiodb_library",
            "cache_ttl_recently_viewed_bytes",
            "genre_section_ttl",
            "request_history_retention_days",
            "ignored_releases_retention_days",
            "orphan_cover_demote_interval_hours",
            "store_prune_interval_hours",
            "discover_picks_count",
        ]
        for field_name in int_coerce_fields:
            setattr(self, field_name, _coerce_positive_int(getattr(self, field_name), field_name))

        ranges: dict[str, tuple[int | float, int | float]] = {
            "cache_ttl_album_library": (1, 168),
            "cache_ttl_album_non_library": (1, 24),
            "cache_ttl_artist_library": (1, 168),
            "cache_ttl_artist_non_library": (1, 168),
            "cache_ttl_artist_discovery_library": (1, 168),
            "cache_ttl_artist_discovery_non_library": (1, 168),
            "cache_ttl_search": (1, 1440),
            "cache_ttl_local_files_recently_added": (1, 60),
            "cache_ttl_jellyfin_recently_played": (1, 60),
            "cache_ttl_jellyfin_favorites": (1, 60),
            "cache_ttl_jellyfin_genres": (1, 1440),
            "cache_ttl_jellyfin_library_stats": (1, 60),
            "cache_ttl_navidrome_albums": (1, 60),
            "cache_ttl_navidrome_artists": (1, 60),
            "cache_ttl_navidrome_recent": (1, 60),
            "cache_ttl_navidrome_favorites": (1, 60),
            "cache_ttl_navidrome_search": (1, 60),
            "cache_ttl_navidrome_genres": (1, 1440),
            "cache_ttl_navidrome_stats": (1, 60),
            "cache_ttl_plex_albums": (1, 60),
            "cache_ttl_plex_search": (1, 60),
            "cache_ttl_plex_genres": (1, 1440),
            "cache_ttl_plex_stats": (1, 60),
            "http_timeout": (5, 60),
            "http_connect_timeout": (1, 30),
            "http_max_connections": (50, 500),
            "subsonic_search_page_limit": (1, 2_147_483_647),
            "batch_artist_images": (1, 20),
            "batch_albums": (1, 20),
            "delay_artist": (0.0, 5.0),
            "delay_albums": (0.0, 5.0),
            "artist_discovery_warm_interval": (5, 10080),
            "artist_discovery_warm_delay": (0.0, 5.0),
            "artist_discovery_precache_delay": (0.0, 5.0),
            "memory_cache_max_entries": (1000, 100000),
            "memory_cache_cleanup_interval": (60, 3600),
            "cover_memory_cache_max_entries": (16, 2048),
            "cover_memory_cache_max_size_mb": (1, 1024),
            "disk_cache_cleanup_interval": (1, 60),
            "recent_metadata_max_size_mb": (100, 5000),
            "recent_covers_max_size_mb": (100, 10000),
            "persistent_metadata_ttl_hours": (1, 168),
            "discover_queue_size": (1, 20),
            "discover_queue_ttl": (1, 168),
            "discover_queue_polling_interval": (1, 30),
            "discover_queue_seed_artists": (1, 10),
            "discover_queue_wildcard_slots": (0, 10),
            "discover_queue_similar_artists_limit": (5, 50),
            "discover_queue_albums_per_similar": (1, 20),
            "discover_queue_enrich_ttl": (1, 168),
            "discover_queue_lastfm_mbid_max_lookups": (1, 50),
            "discover_picks_genre_affinity_weight": (0.0, 1.0),
            "discover_picks_count": (4, 30),
            "frontend_ttl_home": (1, 60),
            "frontend_ttl_discover": (1, 1440),
            "frontend_ttl_library": (1, 60),
            "frontend_ttl_recently_added": (1, 60),
            "frontend_ttl_discover_queue": (60, 10080),
            "frontend_ttl_search": (1, 60),
            "frontend_ttl_local_files_sidebar": (1, 60),
            "frontend_ttl_jellyfin_sidebar": (1, 60),
            "frontend_ttl_plex_sidebar": (1, 60),
            "frontend_ttl_playlist_sources": (1, 60),
            "cache_ttl_audiodb_found": (1, 720),
            "cache_ttl_audiodb_not_found": (1, 168),
            "cache_ttl_audiodb_library": (24, 720),
            "cache_ttl_recently_viewed_bytes": (1, 168),
            "genre_section_ttl": (1, 168),
            "request_history_retention_days": (30, 3650),
            "ignored_releases_retention_days": (30, 3650),
            "orphan_cover_demote_interval_hours": (1, 168),
            "store_prune_interval_hours": (1, 168),
            "sync_stall_timeout_minutes": (2, 30),
            "sync_max_timeout_hours": (1, 48),
            "audiodb_prewarm_concurrency": (1, 8),
            "request_concurrency": (1, 5),
            "audiodb_prewarm_delay": (0.0, 5.0),
            "artist_discovery_precache_concurrency": (1, 8),
        }
        for field_name, (minimum, maximum) in ranges.items():
            _validate_range(getattr(self, field_name), field_name, minimum, maximum)

    @staticmethod
    def from_backend(settings: AdvancedSettings) -> "AdvancedSettingsFrontend":
        return AdvancedSettingsFrontend(
            cache_ttl_album_library=settings.cache_ttl_album_library // 3600,
            cache_ttl_album_non_library=settings.cache_ttl_album_non_library // 3600,
            cache_ttl_artist_library=settings.cache_ttl_artist_library // 3600,
            cache_ttl_artist_non_library=settings.cache_ttl_artist_non_library // 3600,
            cache_ttl_artist_discovery_library=settings.cache_ttl_artist_discovery_library // 3600,
            cache_ttl_artist_discovery_non_library=settings.cache_ttl_artist_discovery_non_library // 3600,
            cache_ttl_search=settings.cache_ttl_search // 60,
            cache_ttl_local_files_recently_added=settings.cache_ttl_local_files_recently_added // 60,
            cache_ttl_jellyfin_recently_played=settings.cache_ttl_jellyfin_recently_played // 60,
            cache_ttl_jellyfin_favorites=settings.cache_ttl_jellyfin_favorites // 60,
            cache_ttl_jellyfin_genres=settings.cache_ttl_jellyfin_genres // 60,
            cache_ttl_jellyfin_library_stats=settings.cache_ttl_jellyfin_library_stats // 60,
            cache_ttl_navidrome_albums=settings.cache_ttl_navidrome_albums // 60,
            cache_ttl_navidrome_artists=settings.cache_ttl_navidrome_artists // 60,
            cache_ttl_navidrome_recent=settings.cache_ttl_navidrome_recent // 60,
            cache_ttl_navidrome_favorites=settings.cache_ttl_navidrome_favorites // 60,
            cache_ttl_navidrome_search=settings.cache_ttl_navidrome_search // 60,
            cache_ttl_navidrome_genres=settings.cache_ttl_navidrome_genres // 60,
            cache_ttl_navidrome_stats=settings.cache_ttl_navidrome_stats // 60,
            cache_ttl_plex_albums=settings.cache_ttl_plex_albums // 60,
            cache_ttl_plex_search=settings.cache_ttl_plex_search // 60,
            cache_ttl_plex_genres=settings.cache_ttl_plex_genres // 60,
            cache_ttl_plex_stats=settings.cache_ttl_plex_stats // 60,
            http_timeout=settings.http_timeout,
            http_connect_timeout=settings.http_connect_timeout,
            http_max_connections=settings.http_max_connections,
            subsonic_search_page_limit=settings.subsonic_search_page_limit,
            batch_artist_images=settings.batch_artist_images,
            batch_albums=settings.batch_albums,
            delay_artist=settings.delay_artist,
            delay_albums=settings.delay_albums,
            artist_discovery_warm_interval=settings.artist_discovery_warm_interval // 60,
            artist_discovery_warm_delay=settings.artist_discovery_warm_delay,
            artist_discovery_precache_delay=settings.artist_discovery_precache_delay,
            memory_cache_max_entries=settings.memory_cache_max_entries,
            memory_cache_cleanup_interval=settings.memory_cache_cleanup_interval,
            cover_memory_cache_max_entries=settings.cover_memory_cache_max_entries,
            cover_memory_cache_max_size_mb=settings.cover_memory_cache_max_size_mb,
            disk_cache_cleanup_interval=settings.disk_cache_cleanup_interval // 60,
            recent_metadata_max_size_mb=settings.recent_metadata_max_size_mb,
            recent_covers_max_size_mb=settings.recent_covers_max_size_mb,
            persistent_metadata_ttl_hours=settings.persistent_metadata_ttl_hours,
            discover_queue_size=settings.discover_queue_size,
            discover_queue_ttl=settings.discover_queue_ttl // 3600,
            discover_queue_auto_generate=settings.discover_queue_auto_generate,
            discover_queue_polling_interval=settings.discover_queue_polling_interval // 1000,
            discover_queue_warm_cycle_build=settings.discover_queue_warm_cycle_build,
            discover_queue_seed_artists=settings.discover_queue_seed_artists,
            discover_queue_wildcard_slots=settings.discover_queue_wildcard_slots,
            discover_queue_similar_artists_limit=settings.discover_queue_similar_artists_limit,
            discover_queue_albums_per_similar=settings.discover_queue_albums_per_similar,
            discover_queue_enrich_ttl=settings.discover_queue_enrich_ttl // 3600,
            discover_queue_lastfm_mbid_max_lookups=settings.discover_queue_lastfm_mbid_max_lookups,
            discover_picks_genre_affinity_weight=settings.discover_picks_genre_affinity_weight,
            discover_picks_count=settings.discover_picks_count,
            frontend_ttl_home=settings.frontend_ttl_home // 60000,
            frontend_ttl_discover=settings.frontend_ttl_discover // 60000,
            frontend_ttl_library=settings.frontend_ttl_library // 60000,
            frontend_ttl_recently_added=settings.frontend_ttl_recently_added // 60000,
            frontend_ttl_discover_queue=settings.frontend_ttl_discover_queue // 60000,
            frontend_ttl_search=settings.frontend_ttl_search // 60000,
            frontend_ttl_local_files_sidebar=settings.frontend_ttl_local_files_sidebar // 60000,
            frontend_ttl_jellyfin_sidebar=settings.frontend_ttl_jellyfin_sidebar // 60000,
            frontend_ttl_plex_sidebar=settings.frontend_ttl_plex_sidebar // 60000,
            frontend_ttl_playlist_sources=settings.frontend_ttl_playlist_sources // 60000,
            audiodb_enabled=settings.audiodb_enabled,
            audiodb_name_search_fallback=settings.audiodb_name_search_fallback,
            direct_remote_images_enabled=settings.direct_remote_images_enabled,
            prefer_local_cover_art=settings.prefer_local_cover_art,
            audiodb_api_key=_mask_api_key(settings.audiodb_api_key),
            cache_ttl_audiodb_found=settings.cache_ttl_audiodb_found // 3600,
            cache_ttl_audiodb_not_found=settings.cache_ttl_audiodb_not_found // 3600,
            cache_ttl_audiodb_library=settings.cache_ttl_audiodb_library // 3600,
            cache_ttl_recently_viewed_bytes=settings.cache_ttl_recently_viewed_bytes // 3600,
            genre_section_ttl=settings.genre_section_ttl // 3600,
            request_history_retention_days=settings.request_history_retention_days,
            ignored_releases_retention_days=settings.ignored_releases_retention_days,
            orphan_cover_demote_interval_hours=settings.orphan_cover_demote_interval_hours,
            store_prune_interval_hours=settings.store_prune_interval_hours,
            sync_stall_timeout_minutes=settings.sync_stall_timeout_minutes,
            sync_max_timeout_hours=settings.sync_max_timeout_hours,
            audiodb_prewarm_concurrency=settings.audiodb_prewarm_concurrency,
            audiodb_prewarm_delay=settings.audiodb_prewarm_delay,
            request_concurrency=settings.request_concurrency,
            artist_discovery_precache_concurrency=settings.artist_discovery_precache_concurrency,
        )

    def to_backend(self) -> AdvancedSettings:
        return AdvancedSettings(
            cache_ttl_album_library=self.cache_ttl_album_library * 3600,
            cache_ttl_album_non_library=self.cache_ttl_album_non_library * 3600,
            cache_ttl_artist_library=self.cache_ttl_artist_library * 3600,
            cache_ttl_artist_non_library=self.cache_ttl_artist_non_library * 3600,
            cache_ttl_artist_discovery_library=self.cache_ttl_artist_discovery_library * 3600,
            cache_ttl_artist_discovery_non_library=self.cache_ttl_artist_discovery_non_library * 3600,
            cache_ttl_search=self.cache_ttl_search * 60,
            cache_ttl_local_files_recently_added=self.cache_ttl_local_files_recently_added * 60,
            cache_ttl_jellyfin_recently_played=self.cache_ttl_jellyfin_recently_played * 60,
            cache_ttl_jellyfin_favorites=self.cache_ttl_jellyfin_favorites * 60,
            cache_ttl_jellyfin_genres=self.cache_ttl_jellyfin_genres * 60,
            cache_ttl_jellyfin_library_stats=self.cache_ttl_jellyfin_library_stats * 60,
            cache_ttl_navidrome_albums=self.cache_ttl_navidrome_albums * 60,
            cache_ttl_navidrome_artists=self.cache_ttl_navidrome_artists * 60,
            cache_ttl_navidrome_recent=self.cache_ttl_navidrome_recent * 60,
            cache_ttl_navidrome_favorites=self.cache_ttl_navidrome_favorites * 60,
            cache_ttl_navidrome_search=self.cache_ttl_navidrome_search * 60,
            cache_ttl_navidrome_genres=self.cache_ttl_navidrome_genres * 60,
            cache_ttl_navidrome_stats=self.cache_ttl_navidrome_stats * 60,
            cache_ttl_plex_albums=self.cache_ttl_plex_albums * 60,
            cache_ttl_plex_search=self.cache_ttl_plex_search * 60,
            cache_ttl_plex_genres=self.cache_ttl_plex_genres * 60,
            cache_ttl_plex_stats=self.cache_ttl_plex_stats * 60,
            http_timeout=self.http_timeout,
            http_connect_timeout=self.http_connect_timeout,
            http_max_connections=self.http_max_connections,
            subsonic_search_page_limit=self.subsonic_search_page_limit,
            batch_artist_images=self.batch_artist_images,
            batch_albums=self.batch_albums,
            delay_artist=self.delay_artist,
            delay_albums=self.delay_albums,
            artist_discovery_warm_interval=self.artist_discovery_warm_interval * 60,
            artist_discovery_warm_delay=self.artist_discovery_warm_delay,
            artist_discovery_precache_delay=self.artist_discovery_precache_delay,
            memory_cache_max_entries=self.memory_cache_max_entries,
            memory_cache_cleanup_interval=self.memory_cache_cleanup_interval,
            cover_memory_cache_max_entries=self.cover_memory_cache_max_entries,
            cover_memory_cache_max_size_mb=self.cover_memory_cache_max_size_mb,
            disk_cache_cleanup_interval=self.disk_cache_cleanup_interval * 60,
            recent_metadata_max_size_mb=self.recent_metadata_max_size_mb,
            recent_covers_max_size_mb=self.recent_covers_max_size_mb,
            persistent_metadata_ttl_hours=self.persistent_metadata_ttl_hours,
            discover_queue_size=self.discover_queue_size,
            discover_queue_ttl=self.discover_queue_ttl * 3600,
            discover_queue_auto_generate=self.discover_queue_auto_generate,
            discover_queue_polling_interval=self.discover_queue_polling_interval * 1000,
            discover_queue_warm_cycle_build=self.discover_queue_warm_cycle_build,
            discover_queue_seed_artists=self.discover_queue_seed_artists,
            discover_queue_wildcard_slots=self.discover_queue_wildcard_slots,
            discover_queue_similar_artists_limit=self.discover_queue_similar_artists_limit,
            discover_queue_albums_per_similar=self.discover_queue_albums_per_similar,
            discover_queue_enrich_ttl=self.discover_queue_enrich_ttl * 3600,
            discover_queue_lastfm_mbid_max_lookups=self.discover_queue_lastfm_mbid_max_lookups,
            discover_picks_genre_affinity_weight=self.discover_picks_genre_affinity_weight,
            discover_picks_count=self.discover_picks_count,
            frontend_ttl_home=self.frontend_ttl_home * 60000,
            frontend_ttl_discover=self.frontend_ttl_discover * 60000,
            frontend_ttl_library=self.frontend_ttl_library * 60000,
            frontend_ttl_recently_added=self.frontend_ttl_recently_added * 60000,
            frontend_ttl_discover_queue=self.frontend_ttl_discover_queue * 60000,
            frontend_ttl_search=self.frontend_ttl_search * 60000,
            frontend_ttl_local_files_sidebar=self.frontend_ttl_local_files_sidebar * 60000,
            frontend_ttl_jellyfin_sidebar=self.frontend_ttl_jellyfin_sidebar * 60000,
            frontend_ttl_plex_sidebar=self.frontend_ttl_plex_sidebar * 60000,
            frontend_ttl_playlist_sources=self.frontend_ttl_playlist_sources * 60000,
            audiodb_enabled=self.audiodb_enabled,
            audiodb_name_search_fallback=self.audiodb_name_search_fallback,
            direct_remote_images_enabled=self.direct_remote_images_enabled,
            prefer_local_cover_art=self.prefer_local_cover_art,
            audiodb_api_key=self.audiodb_api_key,
            cache_ttl_audiodb_found=self.cache_ttl_audiodb_found * 3600,
            cache_ttl_audiodb_not_found=self.cache_ttl_audiodb_not_found * 3600,
            cache_ttl_audiodb_library=self.cache_ttl_audiodb_library * 3600,
            cache_ttl_recently_viewed_bytes=self.cache_ttl_recently_viewed_bytes * 3600,
            genre_section_ttl=self.genre_section_ttl * 3600,
            request_history_retention_days=self.request_history_retention_days,
            ignored_releases_retention_days=self.ignored_releases_retention_days,
            orphan_cover_demote_interval_hours=self.orphan_cover_demote_interval_hours,
            store_prune_interval_hours=self.store_prune_interval_hours,
            sync_stall_timeout_minutes=self.sync_stall_timeout_minutes,
            sync_max_timeout_hours=self.sync_max_timeout_hours,
            audiodb_prewarm_concurrency=self.audiodb_prewarm_concurrency,
            audiodb_prewarm_delay=self.audiodb_prewarm_delay,
            request_concurrency=self.request_concurrency,
            artist_discovery_precache_concurrency=self.artist_discovery_precache_concurrency,
        )
