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
pub(crate) mod sql;
pub mod store;
pub mod sweep;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::secret_sections::{EventsSettings, EventsSweepScope};
use droppedneedle_kernel::runtime_config::{Live as LiveSettings, Plain, Secret};

pub use events::{ConcertsEvents, ConcertsNew};
pub use service::ConcertsService;
pub use sources::Endpoints;
pub use sweep::ConcertsSweep;

/// The concerts bundle on `AppState`: the routes plus the sweep the jobs
/// bundle runs.
#[derive(Clone)]
pub struct ConcertsSetup {
    live: Option<Live>,
}

#[derive(Clone)]
struct Live {
    store: store::ConcertsStore,
    settings: LiveSettings<EventsSettings>,
    sources: Arc<sources::Sources>,
    events: Arc<dyn ConcertsEvents>,
}

impl ConcertsSetup {
    /// Production wiring over the platform's database, shared outbound
    /// client, events settings and event hub, with the sources at
    /// `endpoints`.
    pub fn from_platform(platform: &Platform, endpoints: Endpoints) -> Self {
        Self::new(
            platform.db.clone(),
            platform.http.shared().clone(),
            platform.settings.live(),
            endpoints,
            Arc::new(platform.events.clone()),
        )
    }

    /// Wiring from parts; tests point `endpoints` at local fakes and
    /// record `concerts_new` through `events`.
    pub fn new(
        db: Db,
        http: reqwest::Client,
        settings: LiveSettings<EventsSettings>,
        endpoints: Endpoints,
        events: Arc<dyn ConcertsEvents>,
    ) -> Self {
        Self {
            live: Some(Live {
                store: store::ConcertsStore::new(db),
                settings,
                sources: Arc::new(sources::Sources::new(http, endpoints)),
                events,
            }),
        }
    }

    /// The sweep the events watcher and the settings kick run; `None` when
    /// unwired.
    pub fn sweep(&self) -> Option<ConcertsSweep> {
        self.live.as_ref().map(|live| {
            ConcertsSweep::new(
                live.store.clone(),
                live.settings.clone(),
                Arc::clone(&live.sources),
                Arc::clone(&live.events),
            )
        })
    }

    /// The service the concerts routes run on; `None` when unwired.
    pub fn service(&self) -> Option<ConcertsService> {
        self.live.as_ref().map(|live| {
            ConcertsService::new(
                live.store.clone(),
                live.settings.clone(),
                Arc::clone(&live.sources),
            )
        })
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
    pub fn read(settings: &LiveSettings<EventsSettings>) -> Option<Self> {
        let settings = read_settings(settings)?;
        if !settings.enabled {
            return None;
        }
        let keyed = |on: bool, key: Secret| (on && !key.is_empty()).then_some(key);
        let active = Self {
            ticketmaster: keyed(
                settings.ticketmaster_enabled,
                settings.ticketmaster_api_key.clone(),
            ),
            skiddle: keyed(settings.skiddle_enabled, settings.skiddle_api_key.clone()),
            scope: settings.sweep_scope,
        };
        (active.ticketmaster.is_some() || active.skiddle.is_some()).then_some(active)
    }

    /// The configured sweep scope, whether or not a source is ready.
    pub fn scope(settings: &LiveSettings<EventsSettings>) -> EventsSweepScope {
        read_settings(settings)
            .map(|settings| settings.sweep_scope)
            .unwrap_or_default()
    }
}

fn read_settings(settings: &LiveSettings<EventsSettings>) -> Option<Arc<Plain<EventsSettings>>> {
    settings
        .get()
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
