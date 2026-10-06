//! Production adapters behind the discover ports.
//!
//! Live where a client exists today (ListenBrainz charts, Deezer/iTunes
//! previews, YouTube search when a key is set), honest everywhere else:
//! pages with no shelves instead of invented ones, and a typed "not
//! available" (503) for actions whose builders are not ported yet.

pub mod charts;
pub mod content;
pub mod ownership;
pub mod previews;
pub mod unbuilt;
pub mod youtube;

use std::path::PathBuf;
use std::sync::Arc;

use crate::ids::IdGenerator;
use crate::plugins::scrobble::ListenBrainzLinkStore;
use crate::providers::{
    Providers,
    adapters::{CorePacer, CoreSink, ReqwestGet},
    listenbrainz::{DEFAULT_BASE_URL, ListenBrainzClient},
    youtube::{DEFAULT_DAILY_QUOTA_LIMIT, YouTubeClient, YouTubeSettings},
};
use crate::reads::discover::{
    ports::{ChartsSource, SystemClock, YouTubeSource},
    services::ReadsDeps,
};
use crate::runtime_config::ConfigStore;

/// What production discover needs from the composition root.
pub struct DiscoverInputs {
    /// Catalog reads (ownership, local files).
    pub pool: sqlx::SqlitePool,
    /// Runtime settings, read per call.
    pub config: Arc<ConfigStore>,
    /// The factory's shared client.
    pub http: reqwest::Client,
    /// Shared limiters (ListenBrainz paces at 1/s).
    pub providers: Arc<Providers>,
    /// Users' ListenBrainz links, for "your top albums".
    pub listenbrainz_links: Arc<dyn ListenBrainzLinkStore>,
    /// The YouTube quota file (`<cache_dir>/youtube_quota.json`).
    pub youtube_quota_path: PathBuf,
}

/// Build the production discover deps.
pub fn production_deps(inputs: DiscoverInputs, ids: Arc<dyn IdGenerator>) -> ReadsDeps {
    let charts: Arc<dyn ChartsSource> =
        match CorePacer::for_source(inputs.providers.clone(), "listenbrainz") {
            Some(pacer) => Arc::new(charts::ListenBrainzCharts::new(
                ListenBrainzClient::new(inputs.http.clone(), DEFAULT_BASE_URL, pacer, CoreSink),
                inputs.listenbrainz_links,
                inputs.pool.clone(),
            )),
            None => {
                tracing::error!("no listenbrainz rate limit row; charts are unavailable");
                Arc::new(unbuilt::UnavailableCharts)
            }
        };
    let youtube: Arc<dyn YouTubeSource> = match YouTubeClient::new(
        inputs.http.clone(),
        inputs.youtube_quota_path,
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
    ReadsDeps {
        content: Arc::new(content::UnbuiltContent::new(inputs.config, inputs.pool)),
        queues: Arc::new(unbuilt::EmptyQueue::new(SystemClock)),
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
