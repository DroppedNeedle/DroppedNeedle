//! Shared provider core: the executor, the rate table, caching,
//! coalescing, degradation.
//!
//! Every upstream client builds on this crate instead of reimplementing
//! resilience. The pieces:
//!
//! - [`executor`]: the one way a provider request goes out. Pacing at the
//!   caller's priority, a deadline, a capped read, typed classification,
//!   retry inside a budget, `Retry-After` as a cooldown, and one record of
//!   the outcome for health and the request's degradation notes.
//! - [`rates`]: the one rate table, every row with its date and basis.
//! - [`limiter`]: the token buckets built from the table, with priority
//!   waiters, cooldowns and in-flight caps.
//! - [`error`]: typed [`ProviderError`](error::ProviderError) with distinct
//!   401/403/404/400 semantics and `Retry-After` parsing.
//! - [`slots`]: priority-queue lanes (user / image / background).
//! - [`cache`]: cache-aside helpers with registered key prefixes and
//!   per-source invalidation.
//! - [`singleflight`]: request coalescing for concurrent identical fetches.
//! - [`degradation`]: the request-scoped degradation context.
//! - [`adapters`]: the pacing and recording bridges the clients not yet on
//!   the executor still use.
//!
//! # Wiring
//!
//! ```rust,ignore
//! let providers = Providers::build(&platform, endpoints);
//! ```
//!
//! # Provider clients
//!
//! Each client is reached by module path
//! (`providers::musicbrainz::MusicBrainzClient`, ...). The names are not
//! re-exported at this root, on purpose: several collide across modules
//! and with the role traits in `reads::enrichment`, so the module path is
//! the disambiguator.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod acoustid;
pub mod adapters;
pub mod audiodb;
pub mod cache;
pub mod client;
pub mod coverart;
pub mod degradation;
pub mod discogs;
pub mod error;
pub mod executor;
pub mod geocoding;
pub mod github;
pub mod health;
pub mod itunes;
pub mod lastfm;
pub mod limiter;
pub mod listenbrainz;
pub mod lrclib;
pub mod matrix;
pub mod musicbrainz;
pub mod plextv;
pub mod preview;
pub mod rates;
pub mod retry;
pub mod singleflight;
pub mod skiddle;
pub mod slots;
pub mod ticketmaster;
pub mod wikidata;
pub mod youtube;

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::platform::Platform;

pub use adapters::{CorePacer, CoreSink, HealthSink, ReqwestGet};
pub use cache::{
    InMemoryProviderCache, ProviderCache, cache_aside_bytes, cache_aside_json, digest_key,
    invalidate_source, namespaced_key, prefixes_for,
};
pub use client::{
    ContractViolation, HttpFault, HttpPort, HttpReply, ProviderClient, check_client_contract,
};
pub use degradation::{
    DegradationContext, DegradationSink, IntegrationStatus, NoopSink, ProviderOutcome,
    aggregate_status, degraded_none, record_current, scoped, with_current,
};
pub use error::{
    MAX_RETRY_AFTER, PayloadKind, ProviderError, TransportKind, classify_status, parse_retry_after,
};
pub use executor::{Executor, ReqwestWire, Wire};
pub use limiter::{LimiterSet, OverCapacity, Pacer, RateLimiter, RatePolicy, RequestPriority};
pub use matrix::{OperationKind, SourceRole, apply, role};
pub use retry::{RetryPolicy, execute};
pub use singleflight::Singleflight;
pub use slots::{SlotError, SlotManager, SlotStats, USER_QUIET_WINDOW};

/// Where the providers on the executor live. Filled from the server's one
/// list of outside addresses; tests point it at loopback or scripted
/// hosts.
#[derive(Debug, Clone)]
pub struct ProviderEndpoints {
    /// Official MusicBrainz API root.
    pub musicbrainz: String,
    /// BrainzMash API root.
    pub brainzmash: String,
    /// Cover Art Archive origin.
    pub coverart: String,
    /// ListenBrainz API root.
    pub listenbrainz: String,
}

impl ProviderEndpoints {
    /// Every address on the discard port, where nothing listens, so a
    /// test can never reach a real provider.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn closed() -> Self {
        Self {
            musicbrainz: "http://127.0.0.1:9/musicbrainz/ws/2".to_owned(),
            brainzmash: "http://127.0.0.1:9/brainzmash/ws/2".to_owned(),
            coverart: "http://127.0.0.1:9/coverart".to_owned(),
            listenbrainz: "http://127.0.0.1:9/listenbrainz".to_owned(),
        }
    }
}

/// The shared provider dependencies, built once at boot and shared by
/// every area: the limiters of the rate table, the slot lanes, the byte
/// cache, the service health registry, and the clients that send through
/// the executor.
#[derive(Debug)]
pub struct Providers {
    /// One token bucket per rate table row.
    pub limiters: LimiterSet,
    /// User / image / background slot lanes.
    pub slots: SlotManager,
    /// Shared byte cache behind every client's cache-aside reads.
    pub cache: Arc<InMemoryProviderCache>,
    /// Which services are degraded right now, for `GET /system/health`.
    pub health: Arc<health::ServiceHealth>,
    /// MusicBrainz: the official service, BrainzMash and a mirror.
    pub musicbrainz: musicbrainz::MusicBrainzClient,
    /// The Cover Art Archive.
    pub coverart: coverart::CaaClient,
    /// ListenBrainz: every read, the credential check and scrobble
    /// forwarding, on one bucket.
    pub listenbrainz: listenbrainz::ListenBrainzClient,
}

impl Providers {
    /// The providers over the real network, with the platform's HTTP
    /// clients and clock.
    #[must_use]
    pub fn build(platform: &Platform, endpoints: ProviderEndpoints) -> Self {
        Self::over_wire(
            Arc::new(executor::ReqwestWire::new(&platform.http)),
            Arc::clone(&platform.clock),
            endpoints,
        )
    }

    /// The providers over `wire`, reading wall time from `clock`. The one
    /// builder: [`build`](Self::build) passes the real network, tests a
    /// scripted wire or one that reaches a loopback server.
    #[must_use]
    pub fn over_wire(
        wire: Arc<dyn Wire>,
        clock: Arc<dyn Clock>,
        endpoints: ProviderEndpoints,
    ) -> Self {
        Self::assemble(wire, clock, endpoints, LimiterSet::new())
    }

    /// The providers over `wire` with every limiter unpaced, for suites
    /// that do not care about pacing: cooldowns and in-flight caps still
    /// hold, rates do not. Every address is on the discard port.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn unpaced(wire: Arc<dyn Wire>) -> Self {
        Self::unpaced_at(wire, ProviderEndpoints::closed())
    }

    /// [`unpaced`](Self::unpaced) with the providers at `endpoints`.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn unpaced_at(wire: Arc<dyn Wire>, endpoints: ProviderEndpoints) -> Self {
        Self::assemble(
            wire,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            endpoints,
            LimiterSet::unpaced(),
        )
    }

    fn assemble(
        wire: Arc<dyn Wire>,
        clock: Arc<dyn Clock>,
        endpoints: ProviderEndpoints,
        limiters: LimiterSet,
    ) -> Self {
        let health = Arc::new(health::ServiceHealth::new());
        let executor = |spec: &'static executor::Spec, row: &str| {
            let limiter = limiters.shared(row).cloned().unwrap_or_else(|| {
                // Every executor names a row of the table; a test pins it.
                tracing::error!(row, "no rate row; pacing at one request a second");
                Arc::new(RateLimiter::new(RatePolicy::new(1.0, 1)))
            });
            Executor::new(
                spec,
                limiter,
                Arc::clone(&wire),
                Arc::clone(&health),
                Arc::clone(&clock),
            )
        };
        let musicbrainz = musicbrainz::MusicBrainzClient::new(
            executor(&musicbrainz::client::OFFICIAL, "musicbrainz"),
            executor(&musicbrainz::client::BRAINZMASH, "brainzmash"),
            executor(&musicbrainz::client::MIRROR, "musicbrainz-mirror"),
            &endpoints.musicbrainz,
            &endpoints.brainzmash,
        );
        let coverart = coverart::CaaClient::new(
            executor(&coverart::SPEC, coverart::SOURCE),
            &endpoints.coverart,
        );
        let listenbrainz = listenbrainz::ListenBrainzClient::new(
            executor(&listenbrainz::SPEC, listenbrainz::SOURCE),
            &endpoints.listenbrainz,
        );
        Self {
            limiters,
            slots: SlotManager::new(),
            cache: Arc::new(InMemoryProviderCache::new()),
            health,
            musicbrainz,
            coverart,
            listenbrainz,
        }
    }

    /// The providers over a wire nothing answers on, for tests that never
    /// reach a provider through the executor.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn closed() -> Self {
        Self::over_wire(
            Arc::new(executor::ScriptedWire::new()),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            ProviderEndpoints::closed(),
        )
    }

    /// One provider's limiter, or `None` for providers without a row.
    #[must_use]
    pub fn limiter(&self, source: &str) -> Option<&RateLimiter> {
        self.limiters.limiter(source)
    }
}
