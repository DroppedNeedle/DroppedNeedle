//! The builder that wires the discover services.
//!
//! ListenBrainz and Last.fm charts, Deezer/iTunes previews, YouTube search
//! when a key is set, the weekly set and radio. A part
//! that cannot run answers empty or with a typed "not available", never
//! with invented rows.

use std::path::PathBuf;
use std::sync::Arc;

use crate::discover::listening::{ListeningData, ListeningNetwork, LiveNetwork};
use crate::discover::services::DiscoverDeps;
use crate::discover::{
    cache, charts, content, due, engine, facts, feedback, forget, home, later, page, previews,
    queue, radio, record, scheduler, tunings, youtube,
};
use crate::metadata::library::LocalCatalog;
use crate::metadata::ports::{NoDetails, NoFollows, NoPurchaseLinks};
use crate::metadata::upstream::{LiveCatalogSettings, Upstream};
use crate::metadata::{Catalog, PageSources};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_media::history::HistoryStore;
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_media::playback::sqlite::PlaybackStore;
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
    /// The app's listening store: users' ListenBrainz links, for "your
    /// top albums", and their listening preferences.
    pub listening: ListeningStore,
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
    pub warm: crate::artwork::warm::CoverWarm,
    /// The jobs table Discover queues its background work for a person on.
    pub jobs: droppedneedle_kernel::jobs::queue::JobQueue,
    /// Whether a download source is on, for the "set up a download
    /// client" prompts.
    pub download_readiness: Arc<dyn content::DownloadReadiness>,
    /// The app's connection resolver, for the user's Jellyfin reads.
    pub remotes: Arc<ConnectionResolver>,
    /// The playback rows, for forgetting how a person's plays ended.
    pub playback: Arc<PlaybackStore>,
    /// The imported listening history, for forgetting it.
    pub history: Arc<HistoryStore>,
}

/// The live listening reads production makes.
pub fn live_network(inputs: &DiscoverInputs, config: &ConfigStore) -> Arc<dyn ListeningNetwork> {
    network_over(inputs, config, reads_catalog(inputs, config))
}

/// The live listening reads, reading discographies through `catalog`.
fn network_over(
    inputs: &DiscoverInputs,
    config: &ConfigStore,
    catalog: Catalog,
) -> Arc<dyn ListeningNetwork> {
    Arc::new(LiveNetwork::new(
        live_upstream(inputs, config),
        catalog,
        &inputs.providers,
        inputs.http.clone(),
        inputs.remotes.clone(),
        inputs.listening.clone(),
    ))
}

/// Build the production discover deps, each holding live handles from
/// `config` on the settings it reads. Radio and the listening reads share
/// one catalog, so a large discography has one walker.
pub fn production_deps(inputs: DiscoverInputs, config: &ConfigStore) -> DiscoverDeps {
    let catalog = reads_catalog(&inputs, config);
    let network = network_over(&inputs, config, catalog.clone());
    deps_with(inputs, config, network, catalog)
}

/// Build the discover deps over the listening reads of `network`, each
/// holding live handles from `config` on the settings it reads.
pub fn deps_over(
    inputs: DiscoverInputs,
    config: &ConfigStore,
    network: Arc<dyn ListeningNetwork>,
) -> DiscoverDeps {
    let catalog = reads_catalog(&inputs, config);
    deps_with(inputs, config, network, catalog)
}

/// [`deps_over`] with radio reading through `catalog`.
fn deps_with(
    inputs: DiscoverInputs,
    config: &ConfigStore,
    network: Arc<dyn ListeningNetwork>,
    catalog: Catalog,
) -> DiscoverDeps {
    let kept = facts::KeptFacts::new(inputs.db.clone(), Arc::clone(&inputs.clock));
    let listening = ListeningData::new(
        Arc::clone(&network),
        inputs.providers.cache.clone(),
        kept.clone(),
    );
    let charts = charts::LiveCharts::new(
        listening.clone(),
        inputs.db.clone(),
        config.live(),
        inputs.clock.clone(),
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
    // One count per person, shared by every page cache and the forget.
    let forgets = cache::ForgetCounts::default();
    let answers = Arc::new(feedback::Answers::new(
        inputs.db.clone(),
        Arc::clone(&inputs.clock),
        settings.clone(),
    ));
    let radio = radio::LiveRadio::new(radio::RadioInputs {
        listening: listening.clone(),
        queue_db: queue::store::QueueDb::new(inputs.db.clone()),
        settings,
        db: inputs.db.clone(),
        catalog,
        previews: inputs.providers.previews.clone(),
        cache: inputs.providers.cache.clone(),
        clock: Arc::clone(&inputs.clock),
    });
    let pages = page::DiscoverPages::new(page::PageInputs {
        listening: listening.clone(),
        queue_db: queue::store::QueueDb::new(inputs.db.clone()),
        db: inputs.db.clone(),
        clock: Arc::clone(&inputs.clock),
        advanced: config.live(),
        integrations: content::IntegrationSettings::new(config, inputs.download_readiness.clone()),
        warm: inputs.warm.clone(),
        forgets: forgets.clone(),
    });
    let (home, genre_art, charts) = home::wire(home::HomeWiring {
        db: inputs.db.clone(),
        clock: Arc::clone(&inputs.clock),
        integrations: content::IntegrationSettings::new(config, inputs.download_readiness.clone()),
        listening: listening.clone(),
        discover: pages.clone(),
        warm: inputs.warm.clone(),
        charts,
        forgets: forgets.clone(),
    });
    let fresh_feed = listening.fresh().clone();
    let new_releases =
        crate::discover::new_releases::NewReleaseLists::new(inputs.db.clone(), listening.clone());
    let fact_jobs = Arc::new(facts::job::FactJobs::new(kept, network));
    let due = Arc::new(due::Due::new(
        inputs.db.clone(),
        inputs.jobs.clone(),
        Arc::clone(&inputs.clock),
        inputs.discover_warmer,
        (fact_jobs.due(), inputs.history.clone(), fresh_feed.clone()),
    ));
    let taste = Arc::new(engine::jobs::TasteJobs::new(engine::jobs::TasteInputs {
        db: inputs.db.clone(),
        clock: Arc::clone(&inputs.clock),
        listening: listening.clone(),
        advanced: config.live(),
        forgets: forgets.clone(),
        due: Arc::clone(&due),
    }));
    let pool = Arc::new(engine::pool::PoolJobs::new(engine::pool::PoolInputs {
        db: inputs.db.clone(),
        clock: Arc::clone(&inputs.clock),
        listening: listening.clone(),
        forgets: forgets.clone(),
    }));
    let weekly = Arc::new(engine::weekly::WeeklySets::new(
        engine::weekly::WeeklyInputs {
            db: inputs.db.clone(),
            clock: Arc::clone(&inputs.clock),
            listening: listening.clone(),
            forgets: forgets.clone(),
            due: Arc::clone(&due),
            answers: Arc::clone(&answers),
        },
    ));
    let warm_scheduler = Arc::new(scheduler::WarmScheduler::new(
        scheduler::activity::ActivityDb::new(inputs.db.clone()),
        listening,
        Arc::clone(&inputs.clock),
        inputs.discover_warmer,
        Arc::clone(&due),
        vec![Arc::new(pages.clone()), Arc::new(home.clone())],
    ));
    let page = page::LiveDiscover::new(pages, warm_scheduler);
    DiscoverDeps {
        content: Arc::new(content::LiveContent::new(
            content::IntegrationSettings::new(config, inputs.download_readiness.clone()),
            inputs.db.clone(),
            page,
            home,
            genre_art,
            Arc::clone(&due),
        )),
        answers,
        charts: Arc::new(charts),
        previews: Arc::new(previews::LivePreviews::new(
            inputs.providers.previews.clone(),
        )),
        youtube,
        radio: Arc::new(radio),
        record: record::PickRecord::new(inputs.db.clone(), Arc::clone(&inputs.clock)),
        history: forget::HistoryStores::new(
            inputs.db.clone(),
            inputs.playback.clone(),
            inputs.history.clone(),
            forgets,
        ),
        facts: fact_jobs,
        fresh: Arc::new(fresh_feed),
        taste,
        pool,
        evidence: engine::evidence::Evidence::new(inputs.db.clone()),
        tunings: tunings::Tunings::new(inputs.db.clone(), Arc::clone(&inputs.clock), due),
        later: later::LaterPile::new(inputs.db.clone(), Arc::clone(&inputs.clock)),
        new_releases,
        weekly,
    }
}

/// The catalog radio and the listening reads use. They read albums and
/// discographies only: no follow state, no plugin links or details, and
/// nothing they read warms images.
fn reads_catalog(inputs: &DiscoverInputs, config: &ConfigStore) -> Catalog {
    Catalog::new(
        live_upstream(inputs, config),
        LocalCatalog::new(inputs.db.clone()),
        PageSources {
            follows: Arc::new(NoFollows),
            purchase_links: Arc::new(NoPurchaseLinks),
            details: Arc::new(NoDetails),
            cover_warm: inputs.warm.clone(),
            facts: facts::KeptFacts::new(inputs.db.clone(), Arc::clone(&inputs.clock)),
        },
    )
}

/// The catalog upstreams discover reads through.
fn live_upstream(inputs: &DiscoverInputs, config: &ConfigStore) -> Upstream {
    Upstream::new(
        inputs.providers.clone(),
        Arc::new(LiveCatalogSettings::new(config)),
        inputs.users.clone(),
        inputs.listening.clone(),
    )
}
