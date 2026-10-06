//! Concerts: upcoming gigs for followed artists, near the cities each user
//! picks.
//!
//! A daily sweep ([`sweep`]) walks the followed artists through
//! Ticketmaster and Skiddle and keeps one shared feed in SQLite
//! ([`store`]). The routes ([`handlers`]) read that feed through
//! [`service`], narrow it to the caller's cities, keep the caller's city
//! list, count what is new since they last looked, and proxy city search
//! to the Open-Meteo geocoder. [`sources`] wraps the three outside services
//! with pacing, retry and a circuit breaker. [`events`] is the seam for the
//! `concerts_new` push.
//!
//! Settings come from the `events` section and are read per call (routes)
//! or per run (sweep), so a key change applies without a restart.
//!
//! Dates are UTC: "today" decides which gigs are upcoming and which past
//! rows to prune. v2 used server-local time; the container pins `TZ=UTC`,
//! so the two agree in production.

pub mod events;
pub mod handlers;
pub mod matching;
pub mod models;
pub mod service;
pub mod sources;
pub mod store;
pub mod sweep;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use sqlx::SqlitePool;

use crate::db::{DbRuntime, WriteLane};
use crate::http_client::HttpClientFactory;
use crate::runtime_config::secret_sections::{EventsSettings, EventsSweepScope};
use crate::runtime_config::{ConfigStore, Secret};

pub use events::{ConcertsEvents, ConcertsNew, NoEventStream};
pub use service::ConcertsService;
pub use sources::Endpoints;
pub use sweep::ConcertsSweep;

/// The concerts bundle on `AppState`: the routes plus the sweep the jobs
/// bundle runs. Test states without a database carry an unwired bundle
/// that mounts no routes and has no sweep.
#[derive(Clone)]
pub struct ConcertsSetup {
    live: Option<Live>,
}

#[derive(Clone)]
struct Live {
    store: store::ConcertsStore,
    config: Arc<ConfigStore>,
    sources: Arc<sources::Sources>,
    events: Arc<dyn ConcertsEvents>,
}

impl ConcertsSetup {
    /// Production wiring over the serving runtime, the shared outbound
    /// client and the settings store. `concerts_new` goes nowhere until the
    /// event stream is wired through [`ConcertsSetup::with_events`].
    pub fn from_runtime(
        runtime: &DbRuntime,
        http: &HttpClientFactory,
        config: &Arc<ConfigStore>,
    ) -> Self {
        Self::new(
            runtime.pool().clone(),
            runtime.lane().clone(),
            http.shared().clone(),
            Arc::clone(config),
            Endpoints::default(),
        )
    }

    /// Wiring from parts; tests point `endpoints` at local fakes.
    pub fn new(
        pool: SqlitePool,
        lane: WriteLane,
        http: reqwest::Client,
        config: Arc<ConfigStore>,
        endpoints: Endpoints,
    ) -> Self {
        Self {
            live: Some(Live {
                store: store::ConcertsStore::new(pool, lane),
                config,
                sources: Arc::new(sources::Sources::new(http, endpoints)),
                events: Arc::new(NoEventStream),
            }),
        }
    }

    /// No database: no routes, no sweep.
    #[cfg(any(test, feature = "test-support"))]
    pub fn unwired() -> Self {
        Self { live: None }
    }

    /// Send `concerts_new` events to `events`.
    #[must_use]
    pub fn with_events(mut self, events: Arc<dyn ConcertsEvents>) -> Self {
        if let Some(live) = &mut self.live {
            live.events = events;
        }
        self
    }

    /// The sweep the events watcher and the settings kick run; `None` when
    /// unwired.
    pub fn sweep(&self) -> Option<ConcertsSweep> {
        self.live.as_ref().map(|live| {
            ConcertsSweep::new(
                live.store.clone(),
                Arc::clone(&live.config),
                Arc::clone(&live.sources),
                Arc::clone(&live.events),
            )
        })
    }

    /// The six concerts routes, mounted inside the session gate. Empty
    /// when unwired.
    pub fn gated_router(&self) -> Router {
        self.live
            .as_ref()
            .map(|live| {
                handlers::router(ConcertsService::new(
                    live.store.clone(),
                    Arc::clone(&live.config),
                    Arc::clone(&live.sources),
                ))
            })
            .unwrap_or_default()
    }
}

/// The events settings that matter for one call or run.
#[derive(Debug, Clone)]
pub(crate) struct ActiveSources {
    /// Ticketmaster key when that source is on and keyed.
    pub ticketmaster: Option<Secret>,
    /// Skiddle key when that source is on and keyed.
    pub skiddle: Option<Secret>,
    /// Which artists the sweep covers.
    pub scope: EventsSweepScope,
}

impl ActiveSources {
    /// The ready sources, or `None` when the feature is off or no source
    /// is both enabled and keyed (v2 `is_events_source_ready`).
    pub fn read(config: &ConfigStore) -> Option<Self> {
        let settings = read_settings(config)?;
        if !settings.enabled {
            return None;
        }
        let keyed = |on: bool, key: Secret| (on && !key.is_empty()).then_some(key);
        let active = Self {
            ticketmaster: keyed(settings.ticketmaster_enabled, settings.ticketmaster_api_key),
            skiddle: keyed(settings.skiddle_enabled, settings.skiddle_api_key),
            scope: settings.sweep_scope,
        };
        (active.ticketmaster.is_some() || active.skiddle.is_some()).then_some(active)
    }

    /// The configured sweep scope, whether or not a source is ready.
    pub fn scope(config: &ConfigStore) -> EventsSweepScope {
        read_settings(config)
            .map(|settings| settings.sweep_scope)
            .unwrap_or_default()
    }
}

fn read_settings(config: &ConfigStore) -> Option<EventsSettings> {
    config
        .get_raw::<EventsSettings>()
        .inspect_err(|error| tracing::debug!(%error, "cannot read events settings; concerts off"))
        .ok()
}

/// Wall clock as unix seconds, the feed's timestamp unit.
pub(crate) fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Today's UTC date.
pub(crate) fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}
