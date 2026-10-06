//! YouTube video lookups behind the queue deck, over the one YouTube client
//! the server builds for its quota file. The settings are read per call
//! and pushed into the client, so a new key or limit applies at once.

use std::collections::HashMap;
use std::sync::Arc;

use crate::providers::youtube::{SearchKind, YouTubeClient, YouTubeSettings, YoutubeError};
use crate::reads::discover::{
    models::YouTubeQuotaResponse,
    ports::{BoxFuture, ProviderFailure, YouTubeSource},
};
use crate::runtime_config::{ConfigStore, secret_sections::YouTubeConnection};

/// Live YouTube search.
pub struct LiveYouTube {
    client: YouTubeClient,
    config: Arc<ConfigStore>,
}

impl LiveYouTube {
    /// Search through `client`, configured from `config` on every call.
    pub fn new(client: YouTubeClient, config: Arc<ConfigStore>) -> Self {
        Self { client, config }
    }

    /// Push the current settings into the client. An unreadable or invalid
    /// section leaves the previous settings in place, with a log line.
    fn refresh(&self) {
        let settings = match self.config.get_raw::<YouTubeConnection>() {
            Ok(section) => section,
            Err(error) => {
                tracing::warn!(%error, "cannot read youtube settings; keeping the last ones");
                return;
            }
        };
        let pushed = self.client.update_settings(YouTubeSettings {
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
        self.refresh();
        let found = match kind {
            SearchKind::Album => self.client.search_video(artist, title).await,
            SearchKind::Track => self.client.search_track(artist, title).await,
        };
        found.map_err(|error| match error {
            YoutubeError::NotConfigured(_) => ProviderFailure::not_available("YouTube search"),
            YoutubeError::QuotaExhausted => {
                ProviderFailure::not_available("YouTube search (today's quota is used up)")
            }
            other => ProviderFailure::failed(other.to_string()),
        })
    }
}

/// Unix second of the next UTC midnight, when the daily quota resets.
fn next_utc_midnight() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    (now / 86_400 + 1) * 86_400
}

impl YouTubeSource for LiveYouTube {
    fn is_configured(&self) -> bool {
        self.refresh();
        self.client.is_configured()
    }

    fn search_video<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(self.search(SearchKind::Album, artist, album))
    }

    fn search_track<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(self.search(SearchKind::Track, artist, track))
    }

    fn is_cached(&self, artist: &str, name: &str, track: bool) -> bool {
        let kind = if track {
            SearchKind::Track
        } else {
            SearchKind::Album
        };
        self.client.is_cached(artist, name, kind)
    }

    fn cached_tracks(&self, pairs: &[(String, String)]) -> HashMap<String, bool> {
        let borrowed: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(artist, track)| (artist.as_str(), track.as_str()))
            .collect();
        self.client.are_cached(&borrowed)
    }

    fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>> {
        Box::pin(async move {
            if !self.is_configured() {
                return None;
            }
            let status = self.client.get_quota_status().await;
            Some(YouTubeQuotaResponse {
                used: i64::from(status.used),
                limit: i64::from(status.limit),
                resets_at: next_utc_midnight(),
            })
        })
    }
}
