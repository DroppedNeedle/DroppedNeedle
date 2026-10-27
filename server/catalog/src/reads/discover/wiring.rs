//! The builder that wires the discover services.
//!
//! ListenBrainz and Last.fm charts, Deezer/iTunes previews, YouTube search
//! when a key is set, the queue deck, radio and discovery batches. A part
//! that cannot run answers empty or with a typed "not available", never
//! with invented rows.

use std::path::PathBuf;
use std::sync::Arc;

use crate::reads::catalog::Catalog;
use crate::reads::catalog::library::LocalCatalog;
use crate::reads::catalog::upstream::{LiveCatalogSettings, Upstream};
use crate::reads::discover::services::ReadsDeps;
use crate::reads::discover::{
    batches, charts, content, home, page, previews, queue, radio, youtube,
};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_media::playback::listening::{ListenBrainzLinkStore, ListeningStore};
use droppedneedle_media::remotes::connections::ConnectionResolver;
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
    /// The kernel clock, which dates the YouTube daily budget.
    pub clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    /// Where built pages queue the warm of their covers and artist images.
    pub warm: Option<crate::reads::platform::artwork::warm::CoverWarm>,
    /// The requests side of discovery batches.
    pub batch_requests: Arc<dyn batches::BatchRequests>,
    /// Whether a download source is on, for the "set up a download
    /// client" prompts.
    pub download_readiness: Arc<dyn content::DownloadReadiness>,
    /// The app's connection resolver, for the user's Jellyfin reads.
    pub remotes: Arc<ConnectionResolver>,
}

/// The provider reads behind the queue deck, the discover page and radio,
/// and home. Production reads the live providers for all three.
pub struct DiscoverSources {
    /// What the queue deck reads.
    pub queue: Arc<dyn queue::sources::QueueSources>,
    /// What the discover page and radio read.
    pub page: Arc<dyn page::sources::PageSources>,
    /// What home and the genre pages read.
    pub home: Arc<dyn home::sources::HomeSources>,
}

impl DiscoverSources {
    /// The live provider reads, shared by all three.
    pub fn live(inputs: &DiscoverInputs, config: &ConfigStore) -> Self {
        let sources = Arc::new(live_sources(inputs, live_upstream(inputs, config)));
        Self {
            queue: sources.clone(),
            page: sources.clone(),
            home: sources,
        }
    }
}

/// Build the production discover deps, each holding live handles from
/// `config` on the settings it reads.
pub fn production_deps(inputs: DiscoverInputs, config: &ConfigStore) -> ReadsDeps {
    let sources = DiscoverSources::live(&inputs, config);
    deps_over(inputs, config, sources)
}

/// Build the discover deps over `sources`, each holding live handles from
/// `config` on the settings it reads.
pub fn deps_over(
    inputs: DiscoverInputs,
    config: &ConfigStore,
    sources: DiscoverSources,
) -> ReadsDeps {
    let lastfm_charts = Some(charts::LastFmCharts::new(
        inputs.providers.lastfm.clone(),
        inputs.users.clone(),
    ));
    let charts = charts::LiveCharts::new(
        inputs.providers.listenbrainz.clone(),
        inputs.listenbrainz_links.clone(),
        lastfm_charts,
        inputs.db.sqlx().clone(),
    );
    let client = match YouTubeClient::new(
        inputs.providers.youtube.clone(),
        inputs.youtube_quota_path.clone(),
        YouTubeSettings {
            api_key: String::new(),
            enabled: false,
            api_enabled: false,
            daily_quota_limit: DEFAULT_DAILY_QUOTA_LIMIT,
        },
        Arc::clone(&inputs.clock),
        inputs.providers.cache.clone(),
    ) {
        Ok(client) => Some(client),
        Err(error) => {
            tracing::error!(%error, "youtube quota file unreadable; youtube search is off");
            None
        }
    };
    let youtube = Arc::new(youtube::LiveYouTube::new(
        client,
        config.live(),
        Arc::clone(&inputs.clock),
    ));
    let settings = config.live();
    let queues = Arc::new(queue::LiveQueue::new(
        sources.queue,
        queue::store::QueueDb::new(inputs.db.clone()),
        youtube.clone(),
        inputs.providers.cache.clone(),
        settings.clone(),
    ));
    let radio = radio::LiveRadio::new(radio::RadioInputs {
        sources: sources.page.clone(),
        queue_db: queue::store::QueueDb::new(inputs.db.clone()),
        settings,
        pool: inputs.db.sqlx().clone(),
        catalog: Catalog::new(
            live_upstream(&inputs, config),
            LocalCatalog::new(inputs.db.clone()),
        ),
        previews: inputs.providers.previews.clone(),
        clock: Arc::clone(&inputs.clock),
    });
    let batches = batches::LiveBatches::new(
        inputs.db.clone(),
        Arc::clone(&inputs.clock),
        Some(inputs.batch_requests.clone()),
    );
    let (home, page, genre_art, charts) = home::wire(home::HomeWiring {
        pool: inputs.db.sqlx().clone(),
        integrations: content::IntegrationSettings::new(config, inputs.download_readiness.clone()),
        sources: sources.home,
        discover: page::PageInputs {
            sources: sources.page,
            queue_db: queue::store::QueueDb::new(inputs.db.clone()),
            db: page::store::PageDb::new(inputs.db.clone()),
            advanced: config.live(),
            integrations: content::IntegrationSettings::new(
                config,
                inputs.download_readiness.clone(),
            ),
            queues: queues.clone(),
            warmer_enabled: inputs.discover_warmer,
            warm: inputs.warm.clone(),
        },
        charts,
    });
    ReadsDeps {
        content: Arc::new(
            content::LiveContent::new(
                content::IntegrationSettings::new(config, inputs.download_readiness.clone()),
                inputs.db.sqlx().clone(),
                page,
            )
            .with_home(home, genre_art),
        ),
        queues,
        batches: Arc::new(batches),
        charts: Arc::new(charts),
        previews: Arc::new(previews::LivePreviews::new(
            inputs.providers.previews.clone(),
        )),
        youtube,
        radio: Arc::new(radio),
    }
}

/// The catalog upstreams discover reads through.
fn live_upstream(inputs: &DiscoverInputs, config: &ConfigStore) -> Upstream {
    Upstream::new(
        inputs.providers.clone(),
        Arc::new(LiveCatalogSettings::new(config)),
        inputs.users.clone(),
    )
}

/// The live provider reads the queue, the page and radio share.
fn live_sources(inputs: &DiscoverInputs, upstream: Upstream) -> queue::live_sources::LiveSources {
    queue::live_sources::LiveSources::new(
        upstream,
        inputs.providers.clone(),
        inputs.http.clone(),
        inputs.listenbrainz_links.clone(),
        inputs.remotes.clone(),
        ListeningStore::new(
            inputs.db.clone(),
            inputs.users.crypto.clone(),
            inputs.clock.clone(),
        ),
    )
}
