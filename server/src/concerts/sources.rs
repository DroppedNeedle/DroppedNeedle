//! The three outside services the concerts feature calls, with v2's
//! resilience around each: a token bucket per service, three attempts with
//! backoff on retriable failures (transport, 429, 5xx), and a circuit
//! breaker that fails fast for a minute after five failed calls in a row.
//!
//! The buckets and breakers live as long as the server, so a burst of
//! settings kicks or city searches shares one budget. API keys are not
//! stored here: callers pass the key they read from settings for this run.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::providers::error::ProviderError;
use crate::providers::geocoding::{self, DEFAULT_CITY_COUNT, GeoCity, GeocodingClient};
use crate::providers::limiter::{RateLimiter, RatePolicy};
use crate::providers::retry::{self, RetryPolicy, TokioClock};
use crate::providers::skiddle::{self, SkiddleArtist, SkiddleClient, SkiddleEvent};
use crate::providers::ticketmaster::{self, TicketmasterClient, TmAttraction, TmEvent};

/// Consecutive failed calls that open a breaker, from v2.
const BREAKER_THRESHOLD: u32 = 5;
/// How long an open breaker fails fast, from v2.
const BREAKER_OPEN_FOR: Duration = Duration::from_secs(60);

/// Base URLs, overridable so tests can point at local fakes.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// Ticketmaster Discovery root.
    pub ticketmaster: String,
    /// Skiddle API root.
    pub skiddle: String,
    /// Open-Meteo geocoding search URL.
    pub geocoding: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            ticketmaster: ticketmaster::TICKETMASTER_API_URL.to_owned(),
            skiddle: skiddle::SKIDDLE_API_URL.to_owned(),
            geocoding: geocoding::GEOCODING_API_URL.to_owned(),
        }
    }
}

/// Fails fast after repeated failures so an outage does not cost three
/// attempts per artist for the whole sweep. After the open window, calls
/// go through again; one success closes it.
#[derive(Debug, Default)]
struct Breaker {
    state: Mutex<BreakerState>,
}

#[derive(Debug, Default)]
struct BreakerState {
    failures: u32,
    open_until: Option<Instant>,
}

impl Breaker {
    fn lock(&self) -> std::sync::MutexGuard<'_, BreakerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_open(&self) -> bool {
        self.lock()
            .open_until
            .is_some_and(|until| Instant::now() < until)
    }

    fn record<T>(&self, outcome: &Result<T, ProviderError>) {
        let mut state = self.lock();
        match outcome {
            Ok(_) => *state = BreakerState::default(),
            Err(error) if error.trips_breaker() => {
                state.failures = state.failures.saturating_add(1);
                if state.failures >= BREAKER_THRESHOLD {
                    state.open_until = Some(Instant::now() + BREAKER_OPEN_FOR);
                }
            }
            Err(_) => {}
        }
    }
}

/// One paced, retried, breaker-guarded service.
#[derive(Debug)]
struct Guarded {
    source: &'static str,
    limiter: Arc<RateLimiter>,
    breaker: Breaker,
    retry: RetryPolicy,
}

impl Guarded {
    fn new(source: &'static str, per_second: f64, max_delay: Duration) -> Self {
        Self {
            source,
            limiter: Arc::new(RateLimiter::new(RatePolicy::new(per_second, 1))),
            breaker: Breaker::default(),
            retry: RetryPolicy::new(3, Duration::from_millis(500), max_delay),
        }
    }

    async fn call<T, E, F, Fut>(&self, mut operation: F) -> Result<T, ProviderError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>> + Send,
        E: Into<ProviderError>,
    {
        if self.breaker.is_open() {
            return Err(ProviderError::Unavailable {
                provider: self.source,
                retry_after: None,
            });
        }
        let outcome = retry::execute(&self.retry, &TokioClock, true, || {
            let attempt = operation();
            async move { attempt.await.map_err(Into::into) }
        })
        .await;
        self.breaker.record(&outcome);
        outcome
    }
}

/// The concerts feature's view of Ticketmaster, Skiddle and the geocoder.
#[derive(Debug)]
pub struct Sources {
    http: reqwest::Client,
    endpoints: Endpoints,
    ticketmaster: Guarded,
    skiddle: Guarded,
    geocoding: Guarded,
}

impl Sources {
    /// Sources over the shared outbound client. Rates are v2's: 2 req/s
    /// for Ticketmaster (its documented floor), 1 req/s for Skiddle (no
    /// documented allocation), 2 req/s for the geocoder.
    pub fn new(http: reqwest::Client, endpoints: Endpoints) -> Self {
        Self {
            http,
            endpoints,
            ticketmaster: Guarded::new(ticketmaster::SOURCE, 2.0, Duration::from_secs(5)),
            skiddle: Guarded::new(skiddle::SOURCE, 1.0, Duration::from_secs(5)),
            geocoding: Guarded::new(geocoding::SOURCE, 2.0, Duration::from_secs(3)),
        }
    }

    fn tm_client(&self, key: &str) -> TicketmasterClient {
        TicketmasterClient::with_base_url(self.http.clone(), key, &self.endpoints.ticketmaster)
            .with_limiter(Arc::clone(&self.ticketmaster.limiter))
    }

    fn skiddle_client(&self, key: &str) -> SkiddleClient {
        SkiddleClient::with_base_url(self.http.clone(), key, &self.endpoints.skiddle)
            .with_limiter(Arc::clone(&self.skiddle.limiter))
    }

    /// Ticketmaster music attractions matching a name.
    pub async fn tm_attractions(
        &self,
        key: &str,
        name: &str,
    ) -> Result<Vec<TmAttraction>, ProviderError> {
        let client = self.tm_client(key);
        self.ticketmaster
            .call(|| client.search_attractions(name))
            .await
    }

    /// Upcoming Ticketmaster events for one attraction.
    pub async fn tm_events(
        &self,
        key: &str,
        attraction_id: &str,
    ) -> Result<Vec<TmEvent>, ProviderError> {
        let client = self.tm_client(key);
        self.ticketmaster
            .call(|| client.events_for_attraction(attraction_id))
            .await
    }

    /// Skiddle acts matching a name.
    pub async fn skiddle_artists(
        &self,
        key: &str,
        name: &str,
    ) -> Result<Vec<SkiddleArtist>, ProviderError> {
        let client = self.skiddle_client(key);
        self.skiddle.call(|| client.search_artists(name)).await
    }

    /// Upcoming Skiddle events for one Skiddle artist id.
    pub async fn skiddle_events(
        &self,
        key: &str,
        artist_id: &str,
    ) -> Result<Vec<SkiddleEvent>, ProviderError> {
        let client = self.skiddle_client(key);
        self.skiddle
            .call(|| client.events_for_artist(artist_id))
            .await
    }

    /// Cities matching a search, best first.
    pub async fn cities(&self, query: &str) -> Result<Vec<GeoCity>, ProviderError> {
        let client = GeocodingClient::with_base_url(self.http.clone(), &self.endpoints.geocoding)
            .with_limiter(Arc::clone(&self.geocoding.limiter));
        self.geocoding
            .call(|| client.search_cities(query, DEFAULT_CITY_COUNT))
            .await
    }
}
