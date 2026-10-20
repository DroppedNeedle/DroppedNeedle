//! The three outside services the concerts feature calls: which service,
//! with which key. Pacing, retries and rests after rate limits or a run of
//! failures are the provider executor's, on each service's rate row. API
//! keys are not stored here: callers pass the key they read from settings
//! for this run.

use std::time::Duration;

use droppedneedle_providers::Providers;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::error::ProviderError;
use droppedneedle_providers::geocoding::{DEFAULT_CITY_COUNT, GeoCity, GeocodingClient};
use droppedneedle_providers::skiddle::{self, SkiddleArtist, SkiddleClient, SkiddleEvent};
use droppedneedle_providers::ticketmaster::{self, TicketmasterClient, TmAttraction, TmEvent};

use super::sweep::FETCH_TIMEOUT;

/// The concerts feature's view of Ticketmaster, Skiddle and the geocoder.
#[derive(Debug)]
pub struct Sources {
    ticketmaster: TicketmasterClient,
    skiddle: SkiddleClient,
    geocoding: GeocodingClient,
}

impl Sources {
    /// Sources over the providers' one client of each service.
    pub fn new(providers: &Providers) -> Self {
        Self {
            ticketmaster: providers.ticketmaster.clone(),
            skiddle: providers.skiddle.clone(),
            geocoding: providers.geocoding.clone(),
        }
    }

    /// Ticketmaster music attractions matching a name.
    pub async fn tm_attractions(
        &self,
        key: &str,
        name: &str,
        priority: RequestPriority,
    ) -> Result<Vec<TmAttraction>, ProviderError> {
        rested(
            ticketmaster::SOURCE,
            self.ticketmaster.cooling_for(),
            self.ticketmaster.resting_after_failures(),
            priority,
        )?;
        self.ticketmaster
            .search_attractions(key, name, priority)
            .await
    }

    /// Upcoming Ticketmaster events for one attraction, every page; each
    /// page is retried on its own.
    pub async fn tm_events(
        &self,
        key: &str,
        attraction_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<TmEvent>, ProviderError> {
        rested(
            ticketmaster::SOURCE,
            self.ticketmaster.cooling_for(),
            self.ticketmaster.resting_after_failures(),
            priority,
        )?;
        self.ticketmaster
            .events_for_attraction(key, attraction_id, priority)
            .await
    }

    /// Skiddle acts matching a name.
    pub async fn skiddle_artists(
        &self,
        key: &str,
        name: &str,
        priority: RequestPriority,
    ) -> Result<Vec<SkiddleArtist>, ProviderError> {
        rested(
            skiddle::SOURCE,
            self.skiddle.cooling_for(),
            self.skiddle.resting_after_failures(),
            priority,
        )?;
        self.skiddle.search_artists(key, name, priority).await
    }

    /// Upcoming Skiddle events for one Skiddle artist id.
    pub async fn skiddle_events(
        &self,
        key: &str,
        artist_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<SkiddleEvent>, ProviderError> {
        rested(
            skiddle::SOURCE,
            self.skiddle.cooling_for(),
            self.skiddle.resting_after_failures(),
            priority,
        )?;
        self.skiddle
            .events_for_artist(key, artist_id, priority)
            .await
    }

    /// Cities matching a search, best first.
    pub async fn cities(
        &self,
        query: &str,
        priority: RequestPriority,
    ) -> Result<Vec<GeoCity>, ProviderError> {
        self.geocoding
            .search_cities(query, DEFAULT_CITY_COUNT, priority)
            .await
    }
}

/// A background call declines at once, sending nothing, while the
/// provider rests after a run of failed calls (an outage: waiting would
/// only meet the next failure and re-arm the rest), or rests longer than
/// [`FETCH_TIMEOUT`], after which the fetch gives up anyway (a spent daily
/// allowance). The sweep records the artist as failed and keeps its
/// listings, never as having no gigs. A shorter rest the provider asked
/// for is waited out, and user-facing calls follow the executor's rule.
fn rested(
    provider: &'static str,
    cooling: Duration,
    after_failures: bool,
    priority: RequestPriority,
) -> Result<(), ProviderError> {
    if priority.is_user_facing() || cooling.is_zero() {
        return Ok(());
    }
    if after_failures {
        return Err(ProviderError::Unavailable {
            provider,
            retry_after: Some(cooling),
        });
    }
    if cooling <= FETCH_TIMEOUT {
        return Ok(());
    }
    Err(ProviderError::RateLimited {
        provider,
        retry_after: Some(cooling),
    })
}
