//! Discover and home shelves before their builders are ported.
//!
//! The v2 shelf builders (because-you-listen-to, fresh releases, daily
//! mixes, the queue deck) are a later port. Until then the pages answer
//! with no shelves at all rather than invented rows, the integration
//! status is read from the settings the way v2 read it, and library
//! membership checks run against the catalog. Interactions that only make
//! sense with a builder behind them (activity, refresh, queue-card
//! enrichment) answer "not available".

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;

use crate::reads::discover::{
    models::{
        DiscoverActivityResponse, DiscoverQueuePreview, DiscoverResponse, HomeResponse,
        IntegrationStatus, QueueEnrichment,
    },
    ports::{BoxFuture, DiscoverContent, ProviderFailure},
};
use crate::runtime_config::{
    ConfigStore,
    secret_sections::{
        DownloadClients, JellyfinConnection, LastFmSettings, ListenBrainzConnection,
        NavidromeConnection, PlexConnection, SlskdConnection, YouTubeConnection,
    },
};

/// Genre artwork schema the frontend expects (v2 default).
const GENRE_ARTWORK_SCHEMA: &str = "v2";

/// Shelves with nothing invented: empty pages, real status, real
/// membership.
pub struct UnbuiltContent {
    config: Arc<ConfigStore>,
    pool: SqlitePool,
}

impl UnbuiltContent {
    /// Read settings from `config` and the catalog from `pool`.
    pub fn new(config: Arc<ConfigStore>, pool: SqlitePool) -> Self {
        Self { config, pool }
    }

    /// The v2 integration flags, read from the current settings. A section
    /// that cannot be read counts as off, with a log line.
    fn status(&self) -> IntegrationStatus {
        fn read<T: Default>(name: &str, value: Result<T, crate::runtime_config::ConfigError>) -> T {
            value.unwrap_or_else(|error| {
                tracing::warn!(%error, section = name, "cannot read settings; treating as off");
                T::default()
            })
        }
        let lb = read(
            "listenbrainz",
            self.config.get_raw::<ListenBrainzConnection>(),
        );
        let lastfm = read("lastfm", self.config.get::<LastFmSettings>());
        let youtube = read("youtube", self.config.get_raw::<YouTubeConnection>());
        let jellyfin = read("jellyfin", self.config.get_raw::<JellyfinConnection>());
        let navidrome = read("navidrome", self.config.get_raw::<NavidromeConnection>());
        let plex = read("plex", self.config.get_raw::<PlexConnection>());
        let slskd = read("slskd", self.config.get_raw::<SlskdConnection>());
        let sabnzbd = read("download_clients", self.config.get_raw::<DownloadClients>()).sabnzbd;
        IntegrationStatus {
            listenbrainz: lb.enabled && !lb.username.is_empty(),
            jellyfin: jellyfin.enabled
                && !jellyfin.jellyfin_url.is_empty()
                && !jellyfin.api_key.is_empty(),
            download_client: (slskd.enabled && !slskd.url.is_empty() && !slskd.api_key.is_empty())
                || (sabnzbd.enabled && !sabnzbd.url.is_empty() && !sabnzbd.api_key.is_empty()),
            youtube: youtube.enabled,
            lastfm: lastfm.enabled,
            navidrome: navidrome.enabled
                && !navidrome.navidrome_url.is_empty()
                && !navidrome.username.is_empty()
                && !navidrome.password.is_empty(),
            youtube_api: youtube.enabled
                && youtube.api_enabled
                && !youtube.api_key.expose().trim().is_empty(),
            plex: plex.enabled
                && !plex.plex_url.is_empty()
                && !plex.plex_token.is_empty()
                && !plex.music_library_ids.is_empty(),
            // The native library and its scanner are always present (v2).
            library: true,
            localfiles: true,
        }
    }
}

impl DiscoverContent for UnbuiltContent {
    fn discover<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverResponse, ProviderFailure>> {
        let status = self.status();
        Box::pin(async move {
            Ok(DiscoverResponse {
                because_you_listen_to: Vec::new(),
                discover_queue_enabled: false,
                fresh_releases: None,
                missing_essentials: None,
                rediscover: None,
                artists_you_might_like: None,
                popular_in_your_genres: None,
                genre_list: None,
                globally_trending: None,
                weekly_exploration: None,
                integration_status: Some(status),
                service_prompts: Vec::new(),
                genre_artwork: HashMap::new(),
                genre_artwork_schema_version: GENRE_ARTWORK_SCHEMA.to_owned(),
                lastfm_weekly_artist_chart: None,
                lastfm_weekly_album_chart: None,
                lastfm_recent_scrobbles: None,
                daily_mixes: Vec::new(),
                radio_sections: Vec::new(),
                top_picks: None,
                listeners_like_you: None,
                anniversaries: None,
                new_from_followed: None,
                unexplored_genres: None,
                generated_at: None,
                refresh_started_at: None,
                section_status: HashMap::new(),
                refreshing: false,
                service_status: None,
            })
        })
    }

    fn home<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<HomeResponse, ProviderFailure>> {
        let status = self.status();
        Box::pin(async move {
            Ok(HomeResponse {
                recently_added: None,
                library_artists: None,
                library_albums: None,
                recommended_artists: None,
                trending_artists: None,
                popular_albums: None,
                recently_played: None,
                top_genres: None,
                genre_list: None,
                fresh_releases: None,
                favorite_artists: None,
                your_top_albums: None,
                weekly_exploration: None,
                service_prompts: Vec::new(),
                integration_status: Some(status),
                genre_artwork: HashMap::new(),
                genre_artwork_schema_version: GENRE_ARTWORK_SCHEMA.to_owned(),
                discover_preview: None,
                service_status: None,
                refreshing: false,
            })
        })
    }

    fn integration_status(&self) -> BoxFuture<'_, Result<IntegrationStatus, ProviderFailure>> {
        let status = self.status();
        Box::pin(async move { Ok(status) })
    }

    fn has_local_files(&self) -> BoxFuture<'_, Result<bool, ProviderFailure>> {
        Box::pin(async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE availability = 'indexed')",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|error| ProviderFailure::failed(format!("local files check: {error}")))
        })
    }

    fn record_activity<'a>(
        &'a self,
        _user_id: &'a str,
        _feature: &'a str,
        _artist_mbid: Option<&'a str>,
        _section: Option<&'a str>,
        _provider: Option<&'a str>,
    ) -> BoxFuture<'a, Result<DiscoverActivityResponse, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Discover personalization")) })
    }

    fn trigger_refresh<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Discover refresh")) })
    }

    fn enrich_queue_item<'a>(
        &'a self,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<QueueEnrichment, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Discover queue enrichment")) })
    }

    fn preview_queue_item<'a>(
        &'a self,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverQueuePreview, ProviderFailure>> {
        Box::pin(async { Err(ProviderFailure::not_available("Discover queue previews")) })
    }

    fn validate_queue_mbids<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderFailure>> {
        Box::pin(async move { owned_release_groups(&self.pool, mbids).await })
    }
}

/// The given release groups that some library album is identified as.
async fn owned_release_groups(
    pool: &SqlitePool,
    mbids: &[String],
) -> Result<Vec<String>, ProviderFailure> {
    let wanted: Vec<&str> = mbids.iter().map(String::as_str).collect();
    let owned = super::ownership::owned_albums(pool, &wanted).await?;
    Ok(mbids
        .iter()
        .filter(|mbid| owned.contains_key(&mbid.trim().to_ascii_lowercase()))
        .cloned()
        .collect())
}
