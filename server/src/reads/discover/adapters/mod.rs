//! Production adapters behind the discover ports.
//!
//! Live where the work is ported (ListenBrainz and Last.fm charts,
//! Deezer/iTunes previews, YouTube search when a key is set, the queue
//! deck), honest everywhere else: pages with no shelves instead of
//! invented ones, and a typed "not available" (503) for actions whose
//! builders are not ported yet.

pub mod charts;
pub mod content;
pub mod ownership;
pub mod previews;
pub mod queue;
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
    listenbrainz::{DEFAULT_BASE_URL, ListenBrainzClient},
    youtube::{DEFAULT_DAILY_QUOTA_LIMIT, YouTubeClient, YouTubeSettings},
};
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
}

/// Build the production discover deps.
pub fn production_deps(inputs: DiscoverInputs, ids: Arc<dyn IdGenerator>) -> ReadsDeps {
    let lastfm_charts = match CorePacer::for_source(inputs.providers.clone(), lastfm::SOURCE) {
        Some(pacer) => Some(charts::LastFmCharts::new(
            LastFmClient::new(
                inputs.http.clone(),
                lastfm::DEFAULT_BASE_URL,
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
                ListenBrainzClient::new(inputs.http.clone(), DEFAULT_BASE_URL, pacer, CoreSink),
                inputs.listenbrainz_links.clone(),
                lastfm_charts,
                inputs.pool.clone(),
            )),
            None => {
                tracing::error!("no listenbrainz rate limit row; charts are unavailable");
                Arc::new(unbuilt::UnavailableCharts)
            }
        };
    let youtube: Arc<dyn YouTubeSource> = match YouTubeClient::new(
        inputs.http.clone(),
        inputs.youtube_quota_path.clone(),
        YouTubeSettings {
            api_key: String::new(),
            enabled: false,
            api_enabled: false,
            daily_quota_limit: DEFAULT_DAILY_QUOTA_LIMIT,
        },
    ) {
        Ok(client) => Arc::new(youtube::LiveYouTube::new(client, inputs.config.clone())),
        Err(error) => {
            tracing::error!(%error, "youtube quota file unreadable; youtube search is off");
            Arc::new(unbuilt::UnavailableYouTube)
        }
    };
    let queues = live_queue(&inputs, youtube.clone());
    ReadsDeps {
        content: Arc::new(content::UnbuiltContent::new(
            inputs.config,
            inputs.pool.clone(),
        )),
        queues,
        batches: Arc::new(unbuilt::UnavailableBatches),
        charts,
        previews: Arc::new(previews::LivePreviews::new(ReqwestGet::new(inputs.http))),
        youtube,
        radio: Arc::new(unbuilt::UnavailableRadio),
        now_playing: Arc::new(unbuilt::NoPresence),
        ids,
        clock: Arc::new(SystemClock),
    }
}

/// The queue deck over the live providers and the queue tables.
fn live_queue(inputs: &DiscoverInputs, youtube: Arc<dyn YouTubeSource>) -> Arc<dyn QueueStore> {
    let upstream = Upstream::from_clients(
        inputs.http.clone(),
        inputs.no_redirect.clone(),
        inputs.providers.clone(),
        inputs.config.clone(),
        inputs.users.clone(),
    )
    .with_instance_lastfm_key(inputs.lastfm_key.clone());
    let jellyfin = Arc::new(ConnectionResolver::new(
        Arc::new(SqliteConnectionStore::new(
            inputs.pool.clone(),
            inputs.lane.clone(),
        )),
        Arc::new(CredentialCoder::new(inputs.users.crypto.clone())),
        Arc::new(ConfigServers::new(inputs.config.clone())),
    ));
    let db = queue::store::QueueDb::new(inputs.pool.clone(), inputs.lane.clone());
    let cache = inputs.providers.cache.clone();
    let config = inputs.config.clone();
    let settings: queue::SettingsFn = Arc::new(move || {
        let advanced = config
            .get_raw::<AdvancedSettings>()
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read advanced settings; the queue uses defaults");
                AdvancedSettings::default()
            });
        queue::QueueSettings::from_advanced(&advanced)
    });
    let sources = queue::live_sources::LiveSources::new(
        upstream,
        inputs.providers.clone(),
        inputs.http.clone(),
        inputs.listenbrainz_links.clone(),
        jellyfin,
        inputs.pool.clone(),
    );
    Arc::new(queue::LiveQueue::new(
        Arc::new(sources),
        db,
        youtube,
        cache,
        settings,
    ))
}
