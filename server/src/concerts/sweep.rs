//! The daily sweep that fills the concerts feed.
//!
//! One pass over the distinct followed artists (plus every library artist
//! in library scope). Per artist and per enabled source: resolve the
//! source's own artist entity (cached for seven days, negative results
//! included), fetch upcoming events, map them, drop anything more than a
//! year out, drop Skiddle rows Ticketmaster also lists, then apply the
//! diff in one transaction: upsert what is listed, delete what vanished
//! from a source swept this run. A source switched off keeps its rows.
//! Followers of an artist with new listings get a `concerts_new` event.
//!
//! A failing artist is logged and marked on its cursor, then waits for the
//! next sweep; the sweep itself never stops for one artist. Ticketmaster's
//! free tier allows 5,000 calls a day and a steady-state artist costs about
//! 1.14 calls, so a sweep covers at most [`MAX_ARTISTS_PER_SWEEP`] artists,
//! least recently checked first; the rest rotate into the next sweep.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use super::events::{ConcertsEvents, ConcertsNew};
use super::matching::{
    Confidence, EventRow, SweepArtist, TmBasis, dedupe_across_sources, map_skiddle_event,
    map_tm_event, pick_skiddle_ids, pick_tm_attraction,
};
use super::models::EventSource;
use super::sources::Sources;
use super::store::{ConcertsStore, StoreError};
use super::{ActiveSources, now_unix, today};
use crate::jobs::events_watcher::{EventsWatcher, SweepEnd};
use crate::jobs::registry::BoxFuture;
use crate::jobs::schedule::sleep_or_stop;
use crate::providers::error::ProviderError;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::EventsSweepScope;

/// Source resolutions older than this are looked up again, from v2.
pub const RESOLUTION_TTL_SECS: f64 = 7.0 * 86_400.0;
/// Artists swept per run, sized to Ticketmaster's daily quota, from v2.
pub const MAX_ARTISTS_PER_SWEEP: usize = 3500;
/// Past gigs stay this many days so late-night shows do not vanish early.
const PRUNE_GRACE_DAYS: i64 = 2;
/// Far-future noise guard. A constant on purpose, not a setting.
const HORIZON_DAYS: i64 = 365;
/// Cap on one source's fetch for one artist.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Pause between artists, from v2.
const INTER_ARTIST_DELAY: Duration = Duration::from_secs(1);

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepSummary {
    /// Artists walked.
    pub artists_swept: usize,
    /// Listings first seen this run.
    pub events_new: usize,
    /// Past rows pruned.
    pub events_pruned: usize,
    /// Artists that failed.
    pub errors: usize,
}

/// Why one artist failed.
enum ArtistError {
    /// A source failed or timed out; recorded on the artist's cursor.
    Source(String),
    /// The database failed.
    Store(StoreError),
}

impl From<StoreError> for ArtistError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<ProviderError> for ArtistError {
    fn from(error: ProviderError) -> Self {
        Self::Source(error.to_string())
    }
}

/// The sweep, as the events watcher loop and the settings kick run it.
#[derive(Clone)]
pub struct ConcertsSweep {
    store: ConcertsStore,
    config: Arc<ConfigStore>,
    sources: Arc<Sources>,
    events: Arc<dyn ConcertsEvents>,
    inter_artist_delay: Duration,
}

impl ConcertsSweep {
    /// Sweep over the shared store, settings, sources and event sink.
    pub fn new(
        store: ConcertsStore,
        config: Arc<ConfigStore>,
        sources: Arc<Sources>,
        events: Arc<dyn ConcertsEvents>,
    ) -> Self {
        Self {
            store,
            config,
            sources,
            events,
            inter_artist_delay: INTER_ARTIST_DELAY,
        }
    }

    /// Skip the pause between artists (tests).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn without_delay(mut self) -> Self {
        self.inter_artist_delay = Duration::ZERO;
        self
    }

    /// Run one sweep. Settings and keys are read here, once per run.
    pub async fn run(
        &self,
        skip_recent_hours: Option<f64>,
        stop: &Notify,
    ) -> Result<(SweepEnd, SweepSummary), StoreError> {
        let Some(active) = ActiveSources::read(&self.config) else {
            tracing::debug!("events sweep skipped: no enabled source with a key");
            return Ok((SweepEnd::Finished, SweepSummary::default()));
        };
        let today = today();
        let cutoff = today
            .checked_sub(time::Duration::days(PRUNE_GRACE_DAYS))
            .unwrap_or(today);
        let horizon = today
            .checked_add(time::Duration::days(HORIZON_DAYS))
            .unwrap_or(today)
            .to_string();
        let mut summary = SweepSummary {
            events_pruned: self.store.prune_before(cutoff.to_string()).await?,
            ..SweepSummary::default()
        };
        let artists = self.sweep_set(active.scope, skip_recent_hours).await?;
        summary.artists_swept = artists.len();
        for (index, artist) in artists.iter().enumerate() {
            let outcome = tokio::select! {
                biased;
                () = stop.notified() => {
                    tracing::info!(?summary, "events sweep stopped mid-artist");
                    return Ok((SweepEnd::Stopped, summary));
                }
                outcome = self.process_artist(artist, &active, &horizon) => outcome,
            };
            match outcome {
                Ok(new_events) => summary.events_new += new_events,
                Err(ArtistError::Source(cause)) => {
                    tracing::warn!(artist = %artist.mbid_lower, %cause, "events source unavailable");
                    if let Err(error) = self
                        .store
                        .record_failure(&artist.mbid_lower, cause, now_unix())
                        .await
                    {
                        tracing::error!(artist = %artist.mbid_lower, %error, "events cursor write failed");
                    }
                    summary.errors += 1;
                }
                Err(ArtistError::Store(error)) => {
                    tracing::error!(artist = %artist.mbid_lower, %error, "events sweep store failure");
                    summary.errors += 1;
                }
            }
            if index + 1 < artists.len() && sleep_or_stop(self.inter_artist_delay, stop).await {
                tracing::info!(?summary, "events sweep stopped early");
                return Ok((SweepEnd::Stopped, summary));
            }
        }
        tracing::info!(?summary, "events sweep complete");
        Ok((SweepEnd::Finished, summary))
    }

    /// The artists for this run: least recently checked first, capped.
    /// `skip_recent_hours` drops artists checked within the window (the
    /// boot catch-up uses it so a restart does not spend the day's quota
    /// twice).
    async fn sweep_set(
        &self,
        scope: EventsSweepScope,
        skip_recent_hours: Option<f64>,
    ) -> Result<Vec<SweepArtist>, StoreError> {
        let mut by_mbid: BTreeMap<String, SweepArtist> = self
            .store
            .followed_artists()
            .await?
            .into_iter()
            .map(|artist| (artist.mbid_lower.clone(), artist))
            .collect();
        if scope == EventsSweepScope::Library {
            for artist in self.store.library_artists().await? {
                by_mbid.entry(artist.mbid_lower.clone()).or_insert(artist);
            }
        }
        let ages = self.store.cursor_ages().await?;
        let age = |artist: &SweepArtist| ages.get(&artist.mbid_lower).copied().unwrap_or(0.0);
        let mut artists: Vec<SweepArtist> = by_mbid.into_values().collect();
        artists.sort_by(|a, b| age(a).total_cmp(&age(b)));
        if let Some(hours) = skip_recent_hours {
            let fresh_after = now_unix() - hours * 3600.0;
            artists.retain(|artist| age(artist) < fresh_after);
        }
        if artists.len() > MAX_ARTISTS_PER_SWEEP {
            tracing::warn!(
                artists = artists.len(),
                budget = MAX_ARTISTS_PER_SWEEP,
                "events sweep over budget; the least recently checked go now, the rest next time"
            );
            artists.truncate(MAX_ARTISTS_PER_SWEEP);
        }
        Ok(artists)
    }

    /// Sweep one artist. Returns how many listings were new.
    async fn process_artist(
        &self,
        artist: &SweepArtist,
        active: &ActiveSources,
        horizon: &str,
    ) -> Result<usize, ArtistError> {
        let mut collected = Vec::new();
        let mut swept = HashSet::new();
        if let Some(key) = &active.ticketmaster {
            collected.extend(timed(self.fetch_ticketmaster(artist, key.expose())).await?);
            swept.insert(EventSource::Ticketmaster);
        }
        if let Some(key) = &active.skiddle {
            collected.extend(timed(self.fetch_skiddle(artist, key.expose())).await?);
            swept.insert(EventSource::Skiddle);
        }
        collected.retain(|row| row.local_date.as_str() <= horizon);
        let collected = dedupe_across_sources(collected);

        let existing = self.store.event_keys(&artist.mbid_lower).await?;
        let current: HashSet<(EventSource, String)> = collected.iter().map(EventRow::key).collect();
        let deletes: Vec<(EventSource, String)> = existing
            .iter()
            .filter(|key| swept.contains(&key.0) && !current.contains(*key))
            .cloned()
            .collect();
        let new_events = current.difference(&existing).count();
        self.store
            .apply_sweep(&artist.mbid_lower, collected, deletes, now_unix())
            .await?;
        if new_events > 0 {
            self.notify_followers(artist, new_events).await;
        }
        Ok(new_events)
    }

    async fn fetch_ticketmaster(
        &self,
        artist: &SweepArtist,
        key: &str,
    ) -> Result<Vec<EventRow>, ArtistError> {
        let cached = self.store.tm_resolution(&artist.mbid_lower).await?;
        let (attraction_id, basis) = match cached {
            Some(found) if now_unix() - found.resolved_at <= RESOLUTION_TTL_SECS => {
                (found.attraction_id, found.basis)
            }
            _ => {
                let attractions = self.sources.tm_attractions(key, &artist.name).await?;
                let (id, basis) =
                    pick_tm_attraction(&attractions, &artist.name, &artist.mbid_lower);
                self.store
                    .set_tm_resolution(&artist.mbid_lower, id.clone(), basis, now_unix())
                    .await?;
                (id, basis)
            }
        };
        let Some(attraction_id) = attraction_id else {
            return Ok(Vec::new());
        };
        let confidence = if basis == TmBasis::Mbid {
            Confidence::Mbid
        } else {
            Confidence::Name
        };
        let events = self.sources.tm_events(key, &attraction_id).await?;
        Ok(events
            .iter()
            .filter_map(|event| map_tm_event(event, artist, confidence))
            .collect())
    }

    async fn fetch_skiddle(
        &self,
        artist: &SweepArtist,
        key: &str,
    ) -> Result<Vec<EventRow>, ArtistError> {
        let cached = self.store.skiddle_resolution(&artist.mbid_lower).await?;
        let ids = match cached {
            Some(found) if now_unix() - found.resolved_at <= RESOLUTION_TTL_SECS => {
                found.artist_ids
            }
            _ => {
                let candidates = self.sources.skiddle_artists(key, &artist.name).await?;
                let ids = pick_skiddle_ids(&candidates, &artist.name);
                self.store
                    .set_skiddle_resolution(&artist.mbid_lower, ids.clone(), now_unix())
                    .await?;
                ids
            }
        };
        // Duplicate Skiddle ids for one act return the same events; the
        // last copy of a listing wins.
        let mut by_listing: HashMap<String, EventRow> = HashMap::new();
        for id in &ids {
            for event in self.sources.skiddle_events(key, id).await? {
                if let Some(row) = map_skiddle_event(&event, artist) {
                    by_listing.insert(row.source_event_id.clone(), row);
                }
            }
        }
        Ok(by_listing.into_values().collect())
    }

    /// Best-effort `concerts_new` fan-out to the artist's followers.
    async fn notify_followers(&self, artist: &SweepArtist, new_events: usize) {
        let followers = match self.store.followers(&artist.mbid_lower).await {
            Ok(followers) => followers,
            Err(error) => {
                tracing::warn!(artist = %artist.mbid_lower, %error, "concerts_new skipped: followers unreadable");
                return;
            }
        };
        let event = ConcertsNew {
            artist_mbid: artist.mbid.clone(),
            artist_name: artist.name.clone(),
            new_events,
        };
        for user_id in &followers {
            self.events.concerts_new(user_id, &event);
        }
    }
}

/// Bound one source fetch for one artist.
async fn timed<T>(fetch: impl Future<Output = Result<T, ArtistError>>) -> Result<T, ArtistError> {
    tokio::time::timeout(FETCH_TIMEOUT, fetch)
        .await
        .unwrap_or_else(|_| Err(ArtistError::Source("timed out".to_owned())))
}

impl EventsWatcher for ConcertsSweep {
    fn run_sweep(
        &self,
        skip_recent_hours: Option<f64>,
        stop: Arc<Notify>,
    ) -> BoxFuture<'_, Result<SweepEnd, String>> {
        Box::pin(async move {
            self.run(skip_recent_hours, &stop)
                .await
                .map(|(end, _)| end)
                .map_err(|error| error.to_string())
        })
    }
}
