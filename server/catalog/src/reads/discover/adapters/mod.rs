//! Production adapters behind the discover ports.
//!
//! Live where the work is ported (ListenBrainz and Last.fm charts,
//! Deezer/iTunes previews, YouTube search when a key is set, the queue
//! deck, radio, discovery batches), honest everywhere else: pages with no
//! shelves instead of invented ones, and a typed "not available" (503)
//! for actions whose builders are not ported yet.

pub mod batches;
pub mod charts;
pub mod content;
pub mod home;
pub mod ownership;
pub mod page;
pub mod previews;
pub mod queue;
pub mod radio;
pub mod unbuilt;
pub mod youtube;

use std::path::PathBuf;
use std::sync::Arc;

use crate::reads::catalog::Catalog;
use crate::reads::catalog::library::LocalCatalog;
use crate::reads::catalog::upstream::{LiveCatalogSettings, Upstream};
use crate::reads::discover::{
    ports::{ChartsSource, QueueStore, YouTubeSource},
    services::ReadsDeps,
};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live, secret_sections::AdvancedSettings};
use droppedneedle_media::remotes::connections::{
    ConfigServers, ConnectionResolver, CredentialCoder, SqliteConnectionStore,
};
use droppedneedle_plugins::scrobble::ListenBrainzLinkStore;
use droppedneedle_providers::{
    Providers,
    youtube::{DEFAULT_DAILY_QUOTA_LIMIT, YouTubeClient, YouTubeSettings},
};

/// What production discover needs from the composition root.
pub struct DiscoverInputs {
    /// The database: catalog reads (ownership, local files), queue
    /// ignores and saved decks.
    pub db: Db,
    /// The factory's shared client.
    pub http: reqwest::Client,
    /// Shared limiters (ListenBrainz paces at 1/s).
    pub providers: Arc<Providers>,
    /// Users' ListenBrainz links, for "your top albums".
    pub listenbrainz_links: Arc<dyn ListenBrainzLinkStore>,
    /// The YouTube quota file (`<cache_dir>/youtube_quota.json`).
    pub youtube_quota_path: PathBuf,
    /// The Last.fm switch and the users' Last.fm links.
    pub users: UsersDeps,
    /// Whether the discover warm cycle runs (the deployment kill switch).
    pub discover_warmer: bool,
    /// Base URLs of the chart and discovery upstreams.
    pub endpoints: crate::reads::catalog::upstream::Endpoints,
    /// The cover and artist image cache, which genre pages warm.
    pub artwork: Option<crate::reads::platform::artwork::ArtworkService>,
    /// The requests side of discovery batches.
    pub batch_requests: Arc<dyn batches::BatchRequests>,
}

/// Build the production discover deps, each holding live handles from
/// `config` on the settings it reads.
pub fn production_deps(inputs: DiscoverInputs, config: &ConfigStore) -> ReadsDeps {
    let lastfm_charts = Some(charts::LastFmCharts::new(
        inputs.providers.lastfm.clone(),
        inputs.users.clone(),
    ));
    let charts: Arc<dyn ChartsSource> = Arc::new(charts::LiveCharts::new(
        inputs.providers.listenbrainz.clone(),
        inputs.listenbrainz_links.clone(),
        lastfm_charts,
        inputs.db.sqlx().clone(),
    ));
    let youtube: Arc<dyn YouTubeSource> = match YouTubeClient::with_base_url(
        inputs.http.clone(),
        inputs.youtube_quota_path.clone(),
        YouTubeSettings {
            api_key: String::new(),
            enabled: false,
            api_enabled: false,
            daily_quota_limit: DEFAULT_DAILY_QUOTA_LIMIT,
        },
        inputs.endpoints.youtube.as_str(),
    ) {
        Ok(client) => Arc::new(youtube::LiveYouTube::new(client, config.live())),
        Err(error) => {
            tracing::error!(%error, "youtube quota file unreadable; youtube search is off");
            Arc::new(unbuilt::UnavailableYouTube)
        }
    };
    let upstream = live_upstream(&inputs, config);
    let sources = Arc::new(live_sources(&inputs, config, upstream.clone()));
    let settings = config.live();
    let queues = live_queue(&inputs, sources.clone(), youtube.clone(), settings.clone());
    let radio = radio::LiveRadio::new(radio::RadioInputs {
        sources: sources.clone(),
        queue_db: queue::store::QueueDb::new(inputs.db.clone()),
        settings,
        pool: inputs.db.sqlx().clone(),
        catalog: Catalog::new(upstream, LocalCatalog::new(inputs.db.sqlx().clone())),
        previews: inputs.providers.previews.clone(),
    });
    let batches = batches::LiveBatches::new(inputs.db.clone(), Some(inputs.batch_requests.clone()));
    let (home, page, genre_art, charts) = home::wire(home::HomeWiring {
        pool: inputs.db.sqlx().clone(),
        integrations: content::IntegrationSettings::new(config),
        sources: sources.clone(),
        discover: page::PageInputs {
            sources: sources.clone(),
            queue_db: queue::store::QueueDb::new(inputs.db.clone()),
            db: page::store::PageDb::new(inputs.db.clone()),
            advanced: config.live(),
            integrations: content::IntegrationSettings::new(config),
            queues: queues.clone(),
            warmer_enabled: inputs.discover_warmer,
        },
        charts,
        artwork: inputs.artwork.clone(),
    });
    ReadsDeps {
        content: Arc::new(
            content::LiveContent::new(
                content::IntegrationSettings::new(config),
                inputs.db.sqlx().clone(),
                page,
            )
            .with_home(home, genre_art),
        ),
        queues,
        batches: Arc::new(batches),
        charts,
        previews: Arc::new(previews::LivePreviews::new(
            inputs.providers.previews.clone(),
        )),
        youtube,
        radio: Arc::new(radio),
        now_playing: Arc::new(unbuilt::NoPresence),
        clock: Arc::new(SystemClock),
    }
}

/// The catalog upstreams discover reads through.
fn live_upstream(inputs: &DiscoverInputs, config: &ConfigStore) -> Upstream {
    Upstream::new(
        inputs.providers.clone(),
        Arc::new(LiveCatalogSettings::new(config)),
        inputs.users.clone(),
        inputs.endpoints.clone(),
    )
}

/// The live provider reads the queue, the page and radio share.
fn live_sources(
    inputs: &DiscoverInputs,
    config: &ConfigStore,
    upstream: Upstream,
) -> queue::live_sources::LiveSources {
    let jellyfin = Arc::new(ConnectionResolver::new(
        Arc::new(SqliteConnectionStore::new(inputs.db.clone())),
        Arc::new(CredentialCoder::new(inputs.users.crypto.clone())),
        Arc::new(ConfigServers::new(config)),
    ));
    queue::live_sources::LiveSources::new(
        upstream,
        inputs.providers.clone(),
        inputs.http.clone(),
        inputs.listenbrainz_links.clone(),
        jellyfin,
        inputs.db.sqlx().clone(),
    )
}

/// The queue deck over the live providers and the queue tables.
fn live_queue(
    inputs: &DiscoverInputs,
    sources: Arc<queue::live_sources::LiveSources>,
    youtube: Arc<dyn YouTubeSource>,
    settings: Live<AdvancedSettings>,
) -> Arc<dyn QueueStore> {
    let db = queue::store::QueueDb::new(inputs.db.clone());
    let cache = inputs.providers.cache.clone();
    Arc::new(queue::LiveQueue::new(sources, db, youtube, cache, settings))
}
