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

use crate::auth::users::UsersDeps;
use crate::db::WriteLane;
use crate::ids::IdGenerator;
use crate::plugins::scrobble::ListenBrainzLinkStore;
use crate::providers::{
    Providers,
    adapters::{CorePacer, CoreSink, ReqwestGet},
    lastfm::{self, LastFmClient},
    listenbrainz::ListenBrainzClient,
    youtube::{DEFAULT_DAILY_QUOTA_LIMIT, YouTubeClient, YouTubeSettings},
};
use crate::reads::catalog::Catalog;
use crate::reads::catalog::library::LocalCatalog;
use crate::reads::catalog::upstream::{InstanceLastFmKey, Upstream};
use crate::reads::discover::{
    ports::{ChartsSource, QueueStore, SystemClock, YouTubeSource},
    services::ReadsDeps,
};
use crate::remotes::connections::{
    ConfigServers, ConnectionResolver, CredentialCoder, SqliteConnectionStore,
};
use crate::runtime_config::{ConfigStore, secret_sections::AdvancedSettings};

/// What production discover needs from the composition root.
pub struct DiscoverInputs {
    /// Catalog reads (ownership, local files).
    pub pool: sqlx::SqlitePool,
    /// The writer lane (queue ignores and saved decks).
    pub lane: WriteLane,
    /// Runtime settings, read per call.
    pub config: Arc<ConfigStore>,
    /// The factory's shared client.
    pub http: reqwest::Client,
    /// The factory's no-redirect client (MusicBrainz checks each hop).
    pub no_redirect: reqwest::Client,
    /// Shared limiters (ListenBrainz paces at 1/s).
    pub providers: Arc<Providers>,
    /// Users' ListenBrainz links, for "your top albums".
    pub listenbrainz_links: Arc<dyn ListenBrainzLinkStore>,
    /// The YouTube quota file (`<cache_dir>/youtube_quota.json`).
    pub youtube_quota_path: PathBuf,
    /// The Last.fm switch and the users' Last.fm links.
    pub users: UsersDeps,
    /// The instance Last.fm API key, read per call.
    pub lastfm_key: InstanceLastFmKey,
    /// Whether the discover warm cycle runs (the deployment kill switch).
    pub discover_warmer: bool,
    /// Base URLs of the chart and discovery upstreams.
    pub endpoints: crate::reads::catalog::upstream::Endpoints,
    /// The cover and artist image cache, which genre pages warm.
    pub artwork: Option<crate::reads::platform::artwork::ArtworkService>,
    /// Where boot puts the requests side of discovery batches, once the
    /// acquisition setup exists.
    pub batch_requests: batches::RequestsSlot,
}

/// Build the production discover deps.
pub fn production_deps(inputs: DiscoverInputs, ids: Arc<dyn IdGenerator>) -> ReadsDeps {
    let lastfm_charts = match CorePacer::for_source(inputs.providers.clone(), lastfm::SOURCE) {
        Some(pacer) => Some(charts::LastFmCharts::new(
            LastFmClient::new(
                inputs.http.clone(),
                &inputs.endpoints.lastfm,
                pacer,
                CoreSink,
            ),
            inputs.lastfm_key.clone(),
            inputs.users.clone(),
        )),
        None => {
            tracing::error!("no last.fm rate limit row; last.fm charts are unavailable");
            None
        }
    };
    let charts: Arc<dyn ChartsSource> =
        match CorePacer::for_source(inputs.providers.clone(), "listenbrainz") {
            Some(pacer) => Arc::new(charts::LiveCharts::new(
                ListenBrainzClient::new(
                    inputs.http.clone(),
                    &inputs.endpoints.listenbrainz,
                    pacer,
                    CoreSink,
                ),
                inputs.listenbrainz_links.clone(),
                lastfm_charts,
                inputs.pool.clone(),
            )),
            None => {
                tracing::error!("no listenbrainz rate limit row; charts are unavailable");
                Arc::new(unbuilt::UnavailableCharts)
            }
        };
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
        Ok(client) => Arc::new(youtube::LiveYouTube::new(client, inputs.config.clone())),
        Err(error) => {
            tracing::error!(%error, "youtube quota file unreadable; youtube search is off");
            Arc::new(unbuilt::UnavailableYouTube)
        }
    };
    let upstream = live_upstream(&inputs);
    let sources = Arc::new(live_sources(&inputs, upstream.clone()));
    let settings = queue_settings(inputs.config.clone());
    let queues = live_queue(&inputs, sources.clone(), youtube.clone(), settings.clone());
    let radio = radio::LiveRadio::new(radio::RadioInputs {
        sources: sources.clone(),
        queue_db: queue::store::QueueDb::new(inputs.pool.clone(), inputs.lane.clone()),
        settings,
        pool: inputs.pool.clone(),
        catalog: Catalog::new(upstream, LocalCatalog::new(inputs.pool.clone())),
        previews: ReqwestGet::new(inputs.http.clone()),
    });
    let batches = batches::LiveBatches::new(
        inputs.pool.clone(),
        inputs.lane.clone(),
        inputs.users.users.clone(),
        inputs.batch_requests.clone(),
    );
    let page = page::LiveDiscover::new(page::PageInputs {
        sources: sources.clone(),
        queue_db: queue::store::QueueDb::new(inputs.pool.clone(), inputs.lane.clone()),
        db: page::store::PageDb::new(inputs.pool.clone(), inputs.lane.clone()),
        config: inputs.config.clone(),
        queues: queues.clone(),
        warmer_enabled: inputs.discover_warmer,
    });
    let (home, genre_art, charts) = home::wire(home::HomeWiring {
        pool: inputs.pool.clone(),
        config: inputs.config.clone(),
        sources: sources.clone(),
        discover: page.clone(),
        charts,
        artwork: inputs.artwork.clone(),
    });
    ReadsDeps {
        content: Arc::new(
            content::LiveContent::new(inputs.config, inputs.pool.clone(), page)
                .with_home(home, genre_art),
        ),
        queues,
        batches: Arc::new(batches),
        charts,
        previews: Arc::new(previews::LivePreviews::new(
            ReqwestGet::new(inputs.http),
            &inputs.endpoints,
        )),
        youtube,
        radio: Arc::new(radio),
        now_playing: Arc::new(unbuilt::NoPresence),
        ids,
        clock: Arc::new(SystemClock),
    }
}

/// The catalog upstreams discover reads through.
fn live_upstream(inputs: &DiscoverInputs) -> Upstream {
    Upstream::from_clients(
        inputs.http.clone(),
        inputs.no_redirect.clone(),
        inputs.providers.clone(),
        inputs.config.clone(),
        inputs.users.clone(),
        inputs.endpoints.clone(),
    )
    .with_instance_lastfm_key(inputs.lastfm_key.clone())
}

/// The live provider reads the queue, the page and radio share.
fn live_sources(inputs: &DiscoverInputs, upstream: Upstream) -> queue::live_sources::LiveSources {
    let jellyfin = Arc::new(ConnectionResolver::new(
        Arc::new(SqliteConnectionStore::new(
            inputs.pool.clone(),
            inputs.lane.clone(),
        )),
        Arc::new(CredentialCoder::new(inputs.users.crypto.clone())),
        Arc::new(ConfigServers::new(inputs.config.clone())),
    ));
    queue::live_sources::LiveSources::new(
        upstream,
        inputs.providers.clone(),
        inputs.http.clone(),
        inputs.listenbrainz_links.clone(),
        jellyfin,
        inputs.pool.clone(),
    )
}

/// The queue deck over the live providers and the queue tables.
fn live_queue(
    inputs: &DiscoverInputs,
    sources: Arc<queue::live_sources::LiveSources>,
    youtube: Arc<dyn YouTubeSource>,
    settings: queue::SettingsFn,
) -> Arc<dyn QueueStore> {
    let db = queue::store::QueueDb::new(inputs.pool.clone(), inputs.lane.clone());
    let cache = inputs.providers.cache.clone();
    Arc::new(queue::LiveQueue::new(sources, db, youtube, cache, settings))
}

/// The queue settings, read from the advanced settings on every call.
fn queue_settings(config: Arc<ConfigStore>) -> queue::SettingsFn {
    Arc::new(move || {
        let advanced = config
            .get_raw::<AdvancedSettings>()
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read advanced settings; the queue uses defaults");
                AdvancedSettings::default()
            });
        queue::QueueSettings::from_advanced(&advanced)
    })
}
