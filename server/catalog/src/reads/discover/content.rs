//! Discover and home content.
//!
//! Discover is served by the live page ([`LiveDiscover`]): built in the
//! background, kept in the page cache, refreshed on demand and by the
//! warm scheduler. Home is served by [`LiveHome`] the same way. Both carry
//! the integration status read from the settings the way v2 read it, and
//! genre tiles on both get artwork from the collection.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::reads::discover::genre::artwork::GenreArt;
use crate::reads::discover::home::{self, LiveHome, genre_names};
use crate::reads::discover::page::{LiveDiscover, shelves::shelf_of};
use crate::reads::discover::scheduler::{ArtistDetail, Feature};
use crate::reads::discover::{
    models::{
        DiscoverActivityResponse, DiscoverResponse, HomeResponse, IntegrationStatus, SectionKey,
    },
    ports::{BoxFuture, ProviderFailure},
};
use droppedneedle_kernel::runtime_config::{
    ConfigStore, Listed, Live, Plain,
    secret_sections::{
        JellyfinConnection, LastFmSettings, ListenBrainzConnection, NavidromeConnection,
        PlexConnection, YouTubeConnection,
    },
};

/// Whether a download source is on: slskd, SABnzbd or a plugin download
/// client. Acquisition answers it from the same source list its worker
/// uses. A trait because acquisition sits above the catalog, so the
/// catalog cannot name its answer directly.
pub trait DownloadReadiness: Send + Sync {
    /// Whether music can be acquired now.
    fn can_acquire(&self) -> bool;
}

/// One section now; a section that cannot be read counts as off, with a
/// log line.
fn read<S: Listed + Default>(settings: &Live<S>) -> Arc<Plain<S>> {
    settings.get().unwrap_or_else(|error| {
        tracing::warn!(%error, section = S::KEY, "cannot read settings; treating as off");
        Arc::default()
    })
}

/// The sections the v2 integration flags are read from, each a live
/// handle, and acquisition's answer on download sources.
#[derive(Clone)]
pub struct IntegrationSettings {
    downloads: Arc<dyn DownloadReadiness>,
    listenbrainz: Live<ListenBrainzConnection>,
    lastfm: Live<LastFmSettings>,
    youtube: Live<YouTubeConnection>,
    jellyfin: Live<JellyfinConnection>,
    navidrome: Live<NavidromeConnection>,
    plex: Live<PlexConnection>,
}

impl IntegrationSettings {
    /// Live handles on the integration sections in `config`; whether a
    /// download source is on comes from `downloads`.
    pub fn new(config: &ConfigStore, downloads: Arc<dyn DownloadReadiness>) -> Self {
        Self {
            downloads,
            listenbrainz: config.live(),
            lastfm: config.live(),
            youtube: config.live(),
            jellyfin: config.live(),
            navidrome: config.live(),
            plex: config.live(),
        }
    }

    /// Whether a download source is on, as acquisition answers it.
    pub fn download_client_ready(&self) -> bool {
        self.downloads.can_acquire()
    }

    /// The v2 integration flags, read from the current settings.
    pub fn status(&self) -> IntegrationStatus {
        let lb = read(&self.listenbrainz);
        let lastfm = read(&self.lastfm);
        let youtube = read(&self.youtube);
        let jellyfin = read(&self.jellyfin);
        let navidrome = read(&self.navidrome);
        let plex = read(&self.plex);
        IntegrationStatus {
            listenbrainz: lb.enabled && !lb.username.is_empty(),
            jellyfin: jellyfin.enabled
                && !jellyfin.jellyfin_url.is_empty()
                && !jellyfin.api_key.is_empty(),
            download_client: self.download_client_ready(),
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

/// The live discover page and, when given, the live home page, with
/// the real integration status.
pub struct LiveContent {
    settings: IntegrationSettings,
    pool: SqlitePool,
    page: LiveDiscover,
    home: Option<(LiveHome, Arc<GenreArt>)>,
}

impl LiveContent {
    /// Read the integration flags from `settings` and the catalog from
    /// `pool`; discover comes from `page`. Home has no shelves until
    /// [`Self::with_home`].
    pub fn new(settings: IntegrationSettings, pool: SqlitePool, page: LiveDiscover) -> Self {
        Self {
            settings,
            pool,
            page,
            home: None,
        }
    }

    /// Serve home from `home`, and give genre tiles artwork from `art`.
    #[must_use]
    pub fn with_home(mut self, home: LiveHome, art: Arc<GenreArt>) -> Self {
        self.home = Some((home, art));
        self
    }

    fn status(&self) -> IntegrationStatus {
        self.settings.status()
    }
}

impl LiveContent {
    /// Cached discover shelves for the user, with section status attached.
    pub fn discover<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverResponse, ProviderFailure>> {
        let status = self.status();
        Box::pin(async move {
            let mut page = self.page.page(user_id, status).await;
            if let Some((_, art)) = &self.home {
                let names = genre_names(shelf_of(&page.sections, SectionKey::GenreList));
                if !names.is_empty() {
                    page.genre_artwork = art.batch(&names).await;
                }
            }
            Ok(page)
        })
    }

    /// Cached home shelves for the user.
    pub fn home<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<HomeResponse, ProviderFailure>> {
        let status = self.status();
        Box::pin(async move {
            if let Some((home, _)) = &self.home {
                // A visit keeps home in the warm cycle while it is used. A
                // repeat within five minutes is noted in memory only.
                let scheduler = Arc::clone(self.page.scheduler());
                let user = user_id.to_owned();
                tokio::spawn(async move {
                    if let Err(error) = scheduler
                        .record(&user, Feature::Home, ArtistDetail::default())
                        .await
                    {
                        tracing::warn!(%error, "could not record home activity");
                    }
                });
                return Ok(home.page(user_id).await);
            }
            Ok(home::empty_page(status, false))
        })
    }

    /// Integration availability behind the shelves.
    pub fn integration_status(&self) -> BoxFuture<'_, Result<IntegrationStatus, ProviderFailure>> {
        let status = self.status();
        Box::pin(async move { Ok(status) })
    }

    /// Whether local files back playback (refines the status row).
    pub fn has_local_files(&self) -> BoxFuture<'_, Result<bool, ProviderFailure>> {
        Box::pin(async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE availability = 'indexed')",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|error| ProviderFailure::failed(format!("local files check: {error}")))
        })
    }

    /// Record one discover interaction; returns the personalization cursor.
    pub fn record_activity<'a>(
        &'a self,
        user_id: &'a str,
        feature: Feature,
        detail: ArtistDetail<'a>,
    ) -> BoxFuture<'a, Result<DiscoverActivityResponse, ProviderFailure>> {
        Box::pin(async move { self.page.record_activity(user_id, feature, detail).await })
    }

    /// Trigger a background discover rebuild for the user.
    pub fn trigger_refresh<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async move {
            self.page.refresh(user_id).await;
            Ok(())
        })
    }

    /// One pass of the warm cycle: prune old activity and rebuild what
    /// recent activity says is due.
    pub fn run_due_tick(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.page.scheduler().tick().await })
    }
}
