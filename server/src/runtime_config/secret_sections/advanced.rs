//! Advanced tuning: cache lifetimes, HTTP limits, batching, queues, and
//! the AudioDB key.

use super::*;

// --- advanced_settings (closed export allowlist) ---------------------------
// Kept: user-meaningful TTL/perf fields below. Dropped as internal tuning:
// artist_discovery_warm_interval, artist_discovery_warm_delay,
// artist_discovery_precache_delay, artist_discovery_precache_concurrency,
// audiodb_prewarm_concurrency, audiodb_prewarm_delay, cache_ttl_recently_viewed_bytes,
// cache_ttl_local_files_recently_added.
// The AudioDB key is encrypted at rest now (v2 stored it plaintext).

/// Advanced tuning: cache TTLs, HTTP trio, batching, queues, AudioDB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AdvancedSettings {
    /// Library album TTL.
    pub cache_ttl_album_library: i64,
    /// Non-library album TTL.
    pub cache_ttl_album_non_library: i64,
    /// Library artist TTL.
    pub cache_ttl_artist_library: i64,
    /// Non-library artist TTL.
    pub cache_ttl_artist_non_library: i64,
    /// Library discovery TTL.
    pub cache_ttl_artist_discovery_library: i64,
    /// Non-library discovery TTL.
    pub cache_ttl_artist_discovery_non_library: i64,
    /// Search TTL.
    pub cache_ttl_search: i64,
    /// Jellyfin recently-played TTL.
    pub cache_ttl_jellyfin_recently_played: i64,
    /// Jellyfin favorites TTL.
    pub cache_ttl_jellyfin_favorites: i64,
    /// Jellyfin genres TTL.
    pub cache_ttl_jellyfin_genres: i64,
    /// Jellyfin library-stats TTL.
    pub cache_ttl_jellyfin_library_stats: i64,
    /// Navidrome albums TTL.
    pub cache_ttl_navidrome_albums: i64,
    /// Navidrome artists TTL.
    pub cache_ttl_navidrome_artists: i64,
    /// Navidrome recent TTL.
    pub cache_ttl_navidrome_recent: i64,
    /// Navidrome favorites TTL.
    pub cache_ttl_navidrome_favorites: i64,
    /// Navidrome search TTL.
    pub cache_ttl_navidrome_search: i64,
    /// Navidrome genres TTL.
    pub cache_ttl_navidrome_genres: i64,
    /// Navidrome stats TTL.
    pub cache_ttl_navidrome_stats: i64,
    /// Plex albums TTL.
    pub cache_ttl_plex_albums: i64,
    /// Plex search TTL.
    pub cache_ttl_plex_search: i64,
    /// Plex genres TTL.
    pub cache_ttl_plex_genres: i64,
    /// Plex stats TTL.
    pub cache_ttl_plex_stats: i64,
    /// Outbound HTTP timeout (overrides the factory default).
    pub http_timeout: i64,
    /// Outbound connect timeout.
    pub http_connect_timeout: i64,
    /// Outbound pool size.
    pub http_max_connections: i64,
    /// Artist-image batch size.
    pub batch_artist_images: i64,
    /// Album batch size.
    pub batch_albums: i64,
    /// Artist delay.
    pub delay_artist: f64,
    /// Album delay.
    pub delay_albums: f64,
    /// Memory-cache entries.
    pub memory_cache_max_entries: i64,
    /// Memory-cache cleanup cadence.
    pub memory_cache_cleanup_interval: i64,
    /// Cover memory-cache entries.
    pub cover_memory_cache_max_entries: i64,
    /// Cover memory-cache MB.
    pub cover_memory_cache_max_size_mb: i64,
    /// Disk-cache cleanup cadence.
    pub disk_cache_cleanup_interval: i64,
    /// Recent-metadata MB.
    pub recent_metadata_max_size_mb: i64,
    /// Recent-covers MB.
    pub recent_covers_max_size_mb: i64,
    /// Persistent-metadata TTL hours.
    pub persistent_metadata_ttl_hours: i64,
    /// Discover queue size.
    pub discover_queue_size: i64,
    /// Discover queue TTL.
    pub discover_queue_ttl: i64,
    /// Discover queue auto-generate.
    pub discover_queue_auto_generate: bool,
    /// Discover queue polling ms.
    pub discover_queue_polling_interval: i64,
    /// Discover seed artists.
    pub discover_queue_seed_artists: i64,
    /// Discover wildcard slots.
    pub discover_queue_wildcard_slots: i64,
    /// Whether the background warm cycle may build queue decks.
    pub discover_queue_warm_cycle_build: bool,
    /// Similar artists fetched per seed artist.
    pub discover_queue_similar_artists_limit: i64,
    /// Albums taken per similar artist.
    pub discover_queue_albums_per_similar: i64,
    /// Queue card details cache lifetime, seconds.
    pub discover_queue_enrich_ttl: i64,
    /// MusicBrainz lookups one Last.fm album batch may spend.
    pub discover_queue_lastfm_mbid_max_lookups: i64,
    /// Discover picks count.
    pub discover_picks_count: i64,
    /// Discover genre-affinity weight.
    pub discover_picks_genre_affinity_weight: f64,
    /// Frontend home TTL ms.
    pub frontend_ttl_home: i64,
    /// Frontend discover TTL ms.
    pub frontend_ttl_discover: i64,
    /// Frontend library TTL ms.
    pub frontend_ttl_library: i64,
    /// Frontend recently-added TTL ms.
    pub frontend_ttl_recently_added: i64,
    /// Frontend discover-queue TTL ms.
    pub frontend_ttl_discover_queue: i64,
    /// Frontend search TTL ms.
    pub frontend_ttl_search: i64,
    /// Frontend local-files sidebar TTL ms.
    pub frontend_ttl_local_files_sidebar: i64,
    /// Frontend Jellyfin sidebar TTL ms.
    pub frontend_ttl_jellyfin_sidebar: i64,
    /// Frontend Plex sidebar TTL ms.
    pub frontend_ttl_plex_sidebar: i64,
    /// Frontend playlist-sources TTL ms.
    pub frontend_ttl_playlist_sources: i64,
    /// AudioDB master switch.
    pub audiodb_enabled: bool,
    /// AudioDB name-search fallback.
    pub audiodb_name_search_fallback: bool,
    /// Direct remote images.
    pub direct_remote_images_enabled: bool,
    /// Prefer local cover art.
    pub prefer_local_cover_art: bool,
    /// AudioDB key (encrypted at rest; v2 stored it plaintext). A missing
    /// field reads as empty (then the read path falls back to the "123"
    /// default); only an explicitly stored value decrypts.
    #[serde(default)]
    #[schema(value_type = String)]
    pub audiodb_api_key: Secret,
    /// AudioDB hit TTL.
    pub cache_ttl_audiodb_found: i64,
    /// AudioDB miss TTL.
    pub cache_ttl_audiodb_not_found: i64,
    /// AudioDB library TTL.
    pub cache_ttl_audiodb_library: i64,
    /// Sync stall timeout minutes.
    pub sync_stall_timeout_minutes: i64,
    /// Sync max timeout hours.
    pub sync_max_timeout_hours: i64,
    /// Genre-section TTL.
    pub genre_section_ttl: i64,
    /// Request concurrency.
    pub request_concurrency: i64,
    /// Request-history retention days.
    pub request_history_retention_days: i64,
    /// Ignored-releases retention days.
    pub ignored_releases_retention_days: i64,
    /// Orphan-cover demote cadence hours.
    pub orphan_cover_demote_interval_hours: i64,
    /// Store-prune cadence hours.
    pub store_prune_interval_hours: i64,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            cache_ttl_album_library: 86400,
            cache_ttl_album_non_library: 21600,
            cache_ttl_artist_library: 21600,
            cache_ttl_artist_non_library: 21600,
            cache_ttl_artist_discovery_library: 21600,
            cache_ttl_artist_discovery_non_library: 3600,
            cache_ttl_search: 3600,
            cache_ttl_jellyfin_recently_played: 300,
            cache_ttl_jellyfin_favorites: 300,
            cache_ttl_jellyfin_genres: 3600,
            cache_ttl_jellyfin_library_stats: 600,
            cache_ttl_navidrome_albums: 300,
            cache_ttl_navidrome_artists: 300,
            cache_ttl_navidrome_recent: 120,
            cache_ttl_navidrome_favorites: 120,
            cache_ttl_navidrome_search: 120,
            cache_ttl_navidrome_genres: 3600,
            cache_ttl_navidrome_stats: 600,
            cache_ttl_plex_albums: 300,
            cache_ttl_plex_search: 120,
            cache_ttl_plex_genres: 3600,
            cache_ttl_plex_stats: 600,
            http_timeout: 10,
            http_connect_timeout: 5,
            http_max_connections: 200,
            batch_artist_images: 10,
            batch_albums: 8,
            delay_artist: 0.5,
            delay_albums: 0.3,
            memory_cache_max_entries: 10000,
            memory_cache_cleanup_interval: 300,
            cover_memory_cache_max_entries: 128,
            cover_memory_cache_max_size_mb: 16,
            disk_cache_cleanup_interval: 600,
            recent_metadata_max_size_mb: 500,
            recent_covers_max_size_mb: 1024,
            persistent_metadata_ttl_hours: 24,
            discover_queue_size: 10,
            discover_queue_ttl: 86400,
            discover_queue_auto_generate: true,
            discover_queue_polling_interval: 4000,
            discover_queue_seed_artists: 3,
            discover_queue_wildcard_slots: 2,
            discover_queue_warm_cycle_build: true,
            discover_queue_similar_artists_limit: 15,
            discover_queue_albums_per_similar: 5,
            discover_queue_enrich_ttl: 86400,
            discover_queue_lastfm_mbid_max_lookups: 10,
            discover_picks_count: 12,
            discover_picks_genre_affinity_weight: 0.7,
            frontend_ttl_home: 300000,
            frontend_ttl_discover: 1800000,
            frontend_ttl_library: 300000,
            frontend_ttl_recently_added: 300000,
            frontend_ttl_discover_queue: 86400000,
            frontend_ttl_search: 300000,
            frontend_ttl_local_files_sidebar: 120000,
            frontend_ttl_jellyfin_sidebar: 120000,
            frontend_ttl_plex_sidebar: 120000,
            frontend_ttl_playlist_sources: 900000,
            audiodb_enabled: true,
            audiodb_name_search_fallback: false,
            direct_remote_images_enabled: true,
            prefer_local_cover_art: true,
            audiodb_api_key: Secret::default(),
            cache_ttl_audiodb_found: 604800,
            cache_ttl_audiodb_not_found: 86400,
            cache_ttl_audiodb_library: 1209600,
            sync_stall_timeout_minutes: 10,
            sync_max_timeout_hours: 8,
            genre_section_ttl: 21600,
            request_concurrency: 2,
            request_history_retention_days: 180,
            ignored_releases_retention_days: 365,
            orphan_cover_demote_interval_hours: 24,
            store_prune_interval_hours: 6,
        }
    }
}

impl Section for AdvancedSettings {
    const KEY: &'static str = "advanced_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        let key = Self::KEY;
        let ints: &[(&str, i64, i64, i64)] = &[
            (
                "cache_ttl_album_library",
                self.cache_ttl_album_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_album_non_library",
                self.cache_ttl_album_non_library,
                60,
                86400,
            ),
            (
                "cache_ttl_artist_library",
                self.cache_ttl_artist_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_artist_non_library",
                self.cache_ttl_artist_non_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_artist_discovery_library",
                self.cache_ttl_artist_discovery_library,
                3600,
                604800,
            ),
            (
                "cache_ttl_artist_discovery_non_library",
                self.cache_ttl_artist_discovery_non_library,
                3600,
                604800,
            ),
            ("cache_ttl_search", self.cache_ttl_search, 60, 86400),
            (
                "cache_ttl_jellyfin_recently_played",
                self.cache_ttl_jellyfin_recently_played,
                60,
                3600,
            ),
            (
                "cache_ttl_jellyfin_favorites",
                self.cache_ttl_jellyfin_favorites,
                60,
                3600,
            ),
            (
                "cache_ttl_jellyfin_genres",
                self.cache_ttl_jellyfin_genres,
                60,
                86400,
            ),
            (
                "cache_ttl_jellyfin_library_stats",
                self.cache_ttl_jellyfin_library_stats,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_albums",
                self.cache_ttl_navidrome_albums,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_artists",
                self.cache_ttl_navidrome_artists,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_recent",
                self.cache_ttl_navidrome_recent,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_favorites",
                self.cache_ttl_navidrome_favorites,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_search",
                self.cache_ttl_navidrome_search,
                60,
                3600,
            ),
            (
                "cache_ttl_navidrome_genres",
                self.cache_ttl_navidrome_genres,
                60,
                86400,
            ),
            (
                "cache_ttl_navidrome_stats",
                self.cache_ttl_navidrome_stats,
                60,
                3600,
            ),
            (
                "cache_ttl_plex_albums",
                self.cache_ttl_plex_albums,
                60,
                3600,
            ),
            (
                "cache_ttl_plex_search",
                self.cache_ttl_plex_search,
                60,
                3600,
            ),
            (
                "cache_ttl_plex_genres",
                self.cache_ttl_plex_genres,
                60,
                86400,
            ),
            ("cache_ttl_plex_stats", self.cache_ttl_plex_stats, 60, 3600),
            ("http_timeout", self.http_timeout, 5, 60),
            ("http_connect_timeout", self.http_connect_timeout, 1, 30),
            ("http_max_connections", self.http_max_connections, 50, 500),
            ("batch_artist_images", self.batch_artist_images, 1, 20),
            ("batch_albums", self.batch_albums, 1, 20),
            (
                "memory_cache_max_entries",
                self.memory_cache_max_entries,
                1000,
                100000,
            ),
            (
                "memory_cache_cleanup_interval",
                self.memory_cache_cleanup_interval,
                60,
                3600,
            ),
            (
                "cover_memory_cache_max_entries",
                self.cover_memory_cache_max_entries,
                16,
                2048,
            ),
            (
                "cover_memory_cache_max_size_mb",
                self.cover_memory_cache_max_size_mb,
                1,
                1024,
            ),
            (
                "disk_cache_cleanup_interval",
                self.disk_cache_cleanup_interval,
                60,
                3600,
            ),
            (
                "recent_metadata_max_size_mb",
                self.recent_metadata_max_size_mb,
                100,
                5000,
            ),
            (
                "recent_covers_max_size_mb",
                self.recent_covers_max_size_mb,
                100,
                10000,
            ),
            (
                "persistent_metadata_ttl_hours",
                self.persistent_metadata_ttl_hours,
                1,
                168,
            ),
            ("discover_queue_size", self.discover_queue_size, 1, 20),
            ("discover_queue_ttl", self.discover_queue_ttl, 3600, 604800),
            (
                "discover_queue_polling_interval",
                self.discover_queue_polling_interval,
                1000,
                30000,
            ),
            (
                "discover_queue_seed_artists",
                self.discover_queue_seed_artists,
                1,
                10,
            ),
            (
                "discover_queue_wildcard_slots",
                self.discover_queue_wildcard_slots,
                0,
                10,
            ),
            (
                "discover_queue_similar_artists_limit",
                self.discover_queue_similar_artists_limit,
                5,
                50,
            ),
            (
                "discover_queue_albums_per_similar",
                self.discover_queue_albums_per_similar,
                1,
                20,
            ),
            (
                "discover_queue_enrich_ttl",
                self.discover_queue_enrich_ttl,
                3600,
                604800,
            ),
            (
                "discover_queue_lastfm_mbid_max_lookups",
                self.discover_queue_lastfm_mbid_max_lookups,
                1,
                50,
            ),
            ("discover_picks_count", self.discover_picks_count, 4, 30),
            ("frontend_ttl_home", self.frontend_ttl_home, 60000, 3600000),
            (
                "frontend_ttl_discover",
                self.frontend_ttl_discover,
                60000,
                86400000,
            ),
            (
                "frontend_ttl_library",
                self.frontend_ttl_library,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_recently_added",
                self.frontend_ttl_recently_added,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_discover_queue",
                self.frontend_ttl_discover_queue,
                3600000,
                604800000,
            ),
            (
                "frontend_ttl_search",
                self.frontend_ttl_search,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_local_files_sidebar",
                self.frontend_ttl_local_files_sidebar,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_jellyfin_sidebar",
                self.frontend_ttl_jellyfin_sidebar,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_plex_sidebar",
                self.frontend_ttl_plex_sidebar,
                60000,
                3600000,
            ),
            (
                "frontend_ttl_playlist_sources",
                self.frontend_ttl_playlist_sources,
                60000,
                3600000,
            ),
            (
                "cache_ttl_audiodb_found",
                self.cache_ttl_audiodb_found,
                3600,
                2592000,
            ),
            (
                "cache_ttl_audiodb_not_found",
                self.cache_ttl_audiodb_not_found,
                3600,
                604800,
            ),
            (
                "cache_ttl_audiodb_library",
                self.cache_ttl_audiodb_library,
                86400,
                2592000,
            ),
            (
                "sync_stall_timeout_minutes",
                self.sync_stall_timeout_minutes,
                2,
                30,
            ),
            ("sync_max_timeout_hours", self.sync_max_timeout_hours, 1, 48),
            ("genre_section_ttl", self.genre_section_ttl, 3600, 604800),
            ("request_concurrency", self.request_concurrency, 1, 5),
            (
                "request_history_retention_days",
                self.request_history_retention_days,
                30,
                3650,
            ),
            (
                "ignored_releases_retention_days",
                self.ignored_releases_retention_days,
                30,
                3650,
            ),
            (
                "orphan_cover_demote_interval_hours",
                self.orphan_cover_demote_interval_hours,
                1,
                168,
            ),
            (
                "store_prune_interval_hours",
                self.store_prune_interval_hours,
                1,
                168,
            ),
        ];
        for (field, value, min, max) in ints {
            check_range(key, field, *value, *min, *max)?;
        }
        let floats: &[(&str, f64, f64, f64)] = &[
            ("delay_artist", self.delay_artist, 0.0, 5.0),
            ("delay_albums", self.delay_albums, 0.0, 5.0),
            (
                "discover_picks_genre_affinity_weight",
                self.discover_picks_genre_affinity_weight,
                0.0,
                1.0,
            ),
        ];
        for (field, value, min, max) in floats {
            check_range(key, field, *value, *min, *max)?;
        }
        Ok(())
    }

    fn normalize(&mut self) {
        if self.audiodb_api_key.expose().trim().is_empty() {
            self.audiodb_api_key = Secret::new("123");
        }
    }
}

impl SecretSection for AdvancedSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.audiodb_api_key,
            mask: AUDIODB_API_KEY_MASK,
            strip: false,
        }]
    }
}
