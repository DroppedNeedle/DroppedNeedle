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
use crate::reads::discover::listening::{ListeningData, ListeningNetwork, LiveNetwork};
use crate::reads::discover::services::ReadsDeps;
use crate::reads::discover::{
    batches, charts, content, home, page, previews, queue, radio, scheduler, youtube,
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
    /// ignores and the page cache.
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
    /// The kernel clock, which dates the YouTube daily budget and ages
    /// the page cache.
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

/// The live listening reads production makes.
pub fn live_network(inputs: &DiscoverInputs, config: &ConfigStore) -> Arc<dyn ListeningNetwork> {
    Arc::new(LiveNetwork::new(
        live_upstream(inputs, config),
        &inputs.providers,
        inputs.http.clone(),
        inputs.listenbrainz_links.clone(),
        inputs.remotes.clone(),
        ListeningStore::new(
            inputs.db.clone(),
            inputs.users.crypto.clone(),
            inputs.clock.clone(),
        ),
    ))
}

/// Build the production discover deps, each holding live handles from
/// `config` on the settings it reads.
pub fn production_deps(inputs: DiscoverInputs, config: &ConfigStore) -> ReadsDeps {
    let network = live_network(&inputs, config);
    deps_over(inputs, config, network)
}

/// Build the discover deps over the listening reads of `network`, each
/// holding live handles from `config` on the settings it reads.
pub fn deps_over(
    inputs: DiscoverInputs,
    config: &ConfigStore,
    network: Arc<dyn ListeningNetwork>,
) -> ReadsDeps {
    let listening = ListeningData::new(network, inputs.providers.cache.clone());
    let charts = charts::LiveCharts::new(listening.clone(), inputs.db.sqlx().clone());
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
    let queues = Arc::new(queue::LiveQueue::new(queue::QueueInputs {
        listening: listening.clone(),
        db: inputs.db.clone(),
        clock: Arc::clone(&inputs.clock),
        youtube: youtube.clone(),
        cache: inputs.providers.cache.clone(),
        settings: settings.clone(),
    }));
    let radio = radio::LiveRadio::new(radio::RadioInputs {
        listening: listening.clone(),
        queue_db: queue::store::QueueDb::new(inputs.db.clone()),
        settings,
        pool: inputs.db.sqlx().clone(),
        catalog: Catalog::new(
            live_upstream(&inputs, config),
            LocalCatalog::new(inputs.db.clone()),
        ),
        previews: inputs.providers.previews.clone(),
        cache: inputs.providers.cache.clone(),
        clock: Arc::clone(&inputs.clock),
    });
    let batches = batches::LiveBatches::new(
        inputs.db.clone(),
        Arc::clone(&inputs.clock),
        Some(inputs.batch_requests.clone()),
    );
    let pages = page::DiscoverPages::new(page::PageInputs {
        listening: listening.clone(),
        queue_db: queue::store::QueueDb::new(inputs.db.clone()),
        db: inputs.db.clone(),
        clock: Arc::clone(&inputs.clock),
        advanced: config.live(),
        integrations: content::IntegrationSettings::new(config, inputs.download_readiness.clone()),
        warm: inputs.warm.clone(),
    });
    let (home, genre_art, charts) = home::wire(home::HomeWiring {
        db: inputs.db.clone(),
        clock: Arc::clone(&inputs.clock),
        integrations: content::IntegrationSettings::new(config, inputs.download_readiness.clone()),
        listening: listening.clone(),
        discover: pages.clone(),
        warm: inputs.warm.clone(),
        charts,
    });
    let warm_scheduler = Arc::new(scheduler::WarmScheduler::new(
        scheduler::activity::ActivityDb::new(inputs.db.clone()),
        listening,
        Arc::clone(&inputs.clock),
        inputs.discover_warmer,
        vec![
            Arc::new(pages.clone()),
            Arc::new(home.clone()),
            Arc::clone(&queues) as Arc<dyn scheduler::Warm>,
        ],
    ));
    let page = page::LiveDiscover::new(pages, warm_scheduler);
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
