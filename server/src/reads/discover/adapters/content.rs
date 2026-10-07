//! Discover and home content behind one port.
//!
//! Discover is served by the live page ([`LiveDiscover`]): built in the
//! background, kept as a snapshot, refreshed on demand and by the warm
//! cycle. Home shelves are not ported yet, so home answers with no shelves
//! rather than invented rows. Both carry the integration status read from
//! the settings the way v2 read it.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;

use crate::reads::discover::adapters::page::{GENRE_ARTWORK_SCHEMA, LiveDiscover};
use crate::reads::discover::{
    models::{DiscoverActivityResponse, DiscoverResponse, HomeResponse, IntegrationStatus},
    ports::{BoxFuture, DiscoverContent, ProviderFailure},
};
use crate::runtime_config::{
    ConfigStore,
    secret_sections::{
        DownloadClients, JellyfinConnection, LastFmSettings, ListenBrainzConnection,
        NavidromeConnection, PlexConnection, SlskdConnection, YouTubeConnection,
    },
};

fn read<T: Default>(name: &str, value: Result<T, crate::runtime_config::ConfigError>) -> T {
    value.unwrap_or_else(|error| {
        tracing::warn!(%error, section = name, "cannot read settings; treating as off");
        T::default()
    })
}

/// Whether some download client is set up: slskd or SABnzbd with a URL
/// and a key.
pub fn download_client_ready(config: &ConfigStore) -> bool {
    let slskd = read("slskd", config.get_raw::<SlskdConnection>());
    let sabnzbd = read("download_clients", config.get_raw::<DownloadClients>()).sabnzbd;
    (slskd.enabled && !slskd.url.is_empty() && !slskd.api_key.is_empty())
        || (sabnzbd.enabled && !sabnzbd.url.is_empty() && !sabnzbd.api_key.is_empty())
}

/// The live discover page plus home without shelves, with the real
/// integration status.
pub struct LiveContent {
    config: Arc<ConfigStore>,
    pool: SqlitePool,
    page: LiveDiscover,
}

impl LiveContent {
    /// Read settings from `config` and the catalog from `pool`; discover
    /// comes from `page`.
    pub fn new(config: Arc<ConfigStore>, pool: SqlitePool, page: LiveDiscover) -> Self {
        Self { config, pool, page }
    }

    /// True when the user switched the Discover Queue section off on the
    /// discover page (v2 blanked the flag for a hidden section). A failed
    /// read shows the section, with a log line.
    async fn queue_section_hidden(&self, user_id: &str) -> bool {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM user_section_prefs WHERE user_id = ?1 \
             AND page = 'discover' AND section_key = 'discover_queue' AND enabled = 0)",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "section prefs unreadable; showing the discover queue");
            false
        })
    }

    /// The v2 integration flags, read from the current settings. A section
    /// that cannot be read counts as off, with a log line.
    fn status(&self) -> IntegrationStatus {
        let lb = read(
            "listenbrainz",
            self.config.get_raw::<ListenBrainzConnection>(),
        );
        let lastfm = read("lastfm", self.config.get::<LastFmSettings>());
        let youtube = read("youtube", self.config.get_raw::<YouTubeConnection>());
        let jellyfin = read("jellyfin", self.config.get_raw::<JellyfinConnection>());
        let navidrome = read("navidrome", self.config.get_raw::<NavidromeConnection>());
        let plex = read("plex", self.config.get_raw::<PlexConnection>());
        IntegrationStatus {
            listenbrainz: lb.enabled && !lb.username.is_empty(),
            jellyfin: jellyfin.enabled
                && !jellyfin.jellyfin_url.is_empty()
                && !jellyfin.api_key.is_empty(),
            download_client: download_client_ready(&self.config),
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

impl DiscoverContent for LiveContent {
    fn discover<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverResponse, ProviderFailure>> {
        let status = self.status();
        Box::pin(async move {
            let queue_hidden = self.queue_section_hidden(user_id).await;
            let mut page = self.page.page(user_id, status).await;
            // The queue deck is built; the page shows its entry point
            // unless the user turned the section off.
            page.discover_queue_enabled = !queue_hidden;
            Ok(page)
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
        user_id: &'a str,
        feature: &'a str,
        artist_mbid: Option<&'a str>,
        section: Option<&'a str>,
        provider: Option<&'a str>,
    ) -> BoxFuture<'a, Result<DiscoverActivityResponse, ProviderFailure>> {
        Box::pin(async move {
            self.page
                .record_activity(user_id, feature, artist_mbid, section, provider)
                .await
        })
    }

    fn trigger_refresh<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async move {
            self.page.refresh(user_id).await;
            Ok(())
        })
    }

    fn run_due_tick(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.page.run_due(None).await })
    }
}
