//! YouTube video lookups behind the queue deck, over the one YouTube client
//! the server builds for its quota file. The settings are read per call
//! and pushed into the client, so a new key or limit applies at once.
//! Without a client (its quota file could not be read) search is off and
//! says so.

use std::collections::HashMap;

use crate::reads::discover::{
    models::YouTubeQuotaResponse,
    ports::{BoxFuture, ProviderFailure},
};
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::runtime_config::{Live, secret_sections::YouTubeConnection};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::youtube::{SearchKind, YouTubeClient, YouTubeSettings, YoutubeError};

/// Why search is off when the quota file could not be read.
const NO_QUOTA_FILE: &str = "YouTube search is off: its quota file could not be read.";

/// Live YouTube search.
pub struct LiveYouTube {
    client: Option<YouTubeClient>,
    settings: Live<YouTubeConnection>,
    clock: Arc<dyn Clock>,
}

impl LiveYouTube {
    /// Search through `client`, configured from `settings` on every call;
    /// `clock` dates the quota reset. `None` (the quota file could not be
    /// read) keeps search off.
    pub fn new(
        client: Option<YouTubeClient>,
        settings: Live<YouTubeConnection>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client,
            settings,
            clock,
        }
    }

    /// Push the current settings into the client. An unreadable or invalid
    /// section leaves the previous settings in place, with a log line.
    fn refresh(&self, client: &YouTubeClient) {
        let settings = match self.settings.get() {
            Ok(section) => section,
            Err(error) => {
                tracing::warn!(%error, "cannot read youtube settings; keeping the last ones");
                return;
            }
        };
        let pushed = client.update_settings(YouTubeSettings {
            api_key: settings.api_key.expose().to_owned(),
            enabled: settings.enabled,
            api_enabled: settings.api_enabled,
            daily_quota_limit: u32::try_from(settings.daily_quota_limit).unwrap_or(0),
        });
        if let Err(error) = pushed {
            tracing::warn!(%error, "youtube settings rejected; keeping the last ones");
        }
    }

    async fn search(
        &self,
        kind: SearchKind,
        artist: &str,
        title: &str,
    ) -> Result<Option<String>, ProviderFailure> {
        let Some(client) = &self.client else {
            return Err(ProviderFailure::not_configured(NO_QUOTA_FILE));
        };
        self.refresh(client);
        // Every search is a person's request (a preview, a queue card, a
        // saved link); nothing searches YouTube in the background.
        let priority = RequestPriority::UserInitiated;
        let found = match kind {
            SearchKind::Album => client.search_video(artist, title, priority).await,
            SearchKind::Track => client.search_track(artist, title, priority).await,
        };
        found.map_err(|error| match error {
            YoutubeError::NotConfigured(_) => ProviderFailure::not_configured(
                "YouTube search needs an API key under Settings > YouTube.",
            ),
            YoutubeError::QuotaExhausted => ProviderFailure::Exhausted(
                "Today's YouTube search quota is used up; it resets at midnight UTC.".to_owned(),
            ),
            YoutubeError::Provider(error) => ProviderFailure::Provider(error),
            other => ProviderFailure::failed(other.to_string()),
        })
    }
}

/// Unix second of the next UTC midnight after `now`, when the daily quota
/// resets.
fn next_utc_midnight(now: i64) -> i64 {
    (now.div_euclid(86_400) + 1) * 86_400
}

impl LiveYouTube {
    /// Whether the data API (quota'd) is configured.
    pub fn is_configured(&self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        self.refresh(client);
        client.is_configured()
    }

    /// Album video lookup. Returns the video id, when found.
    pub fn search_video<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(self.search(SearchKind::Album, artist, album))
    }

    /// Track video lookup. Returns the video id, when found.
    pub fn search_track<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(self.search(SearchKind::Track, artist, track))
    }

    /// Whether the pair resolves from cache.
    pub fn is_cached<'a>(
        &'a self,
        artist: &'a str,
        name: &'a str,
        track: bool,
    ) -> BoxFuture<'a, bool> {
        let kind = if track {
            SearchKind::Track
        } else {
            SearchKind::Album
        };
        Box::pin(async move {
            match &self.client {
                Some(client) => client.is_cached(artist, name, kind).await,
                None => false,
            }
        })
    }

    /// Cache membership for deduped pairs, keyed `artist|track` (lowercased).
    pub fn cached_tracks<'a>(
        &'a self,
        pairs: &'a [(String, String)],
    ) -> BoxFuture<'a, HashMap<String, bool>> {
        Box::pin(async move {
            let Some(client) = &self.client else {
                return HashMap::new();
            };
            let borrowed: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(artist, track)| (artist.as_str(), track.as_str()))
                .collect();
            client.are_cached(&borrowed).await
        })
    }

    /// Quota state. `None` when unconfigured (the route answers 404).
    pub fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>> {
        Box::pin(async move {
            let client = self.client.as_ref()?;
            if !self.is_configured() {
                return None;
            }
            let status = client.get_quota_status().await;
            Some(YouTubeQuotaResponse {
                used: i64::from(status.used),
                limit: i64::from(status.limit),
                resets_at: next_utc_midnight(self.clock.now_unix()),
            })
        })
    }
}
