//! Shared provider core: pacing, retry, caching, coalescing, degradation.
//!
//! Every upstream client builds on this module instead of
//! reimplementing resilience. The pieces:
//!
//! - [`error`]: typed [`ProviderError`](error::ProviderError) with distinct
//!   401/403/404/400 semantics (no blanket non-2xx mapping) and `Retry-After`
//!   / rate-limit header honoring for 429/503.
//! - [`limiter`]: token-bucket limiters with priority waiters, one per
//!   verified policy row (MusicBrainz 1/s hard, ListenBrainz 1/s, AudioDB
//!   30/min, AcoustID 3/s, Cover Art Archive ~1/s + backoff, Last.fm 5/s +
//!   backoff).
//! - [`retry`]: idempotency-aware retry with exponential backoff, jitter,
//!   `Retry-After` override, and an optional budget. Writes run exactly once.
//! - [`slots`]: priority-queue lanes (user / image / background). Background
//!   jobs pass the background priority explicitly at the call site.
//! - [`cache`]: cache-aside helpers with registered key prefixes and
//!   per-source invalidation.
//! - [`singleflight`]: request coalescing for concurrent identical fetches.
//! - [`degradation`]: typed outcomes plus the request-scoped degradation
//!   context. Optional sources fail record-then-`None`; the recording is the
//!   error signal.
//! - [`matrix`]: the operation/source matrix. Only a dead primary source
//!   fails the request, per operation.
//! - [`client`]: the [`ProviderClient`](client::ProviderClient) surface every
//!   client implements, plus the [`check_client_contract`](client::check_client_contract)
//!   conformance harness for client tests.
//! - [`adapters`]: bridges from the shared core seams
//!   onto production (limiter-backed pacing, context-backed degradation
//!   recording, one shared reqwest transport, core/enrichment error mapping,
//!   enrichment role adapters over the concrete clients).
//!
//! # Wiring
//!
//! ```rust,ignore
//! let providers = providers::Providers::with_memory_cache();
//! ```
//!
//! # Provider clients
//!
//! Each client is reached by module path
//! (`providers::musicbrainz::MusicBrainzClient`, ...). The names are not
//! re-exported at this root, on purpose: several collide across
//! modules (`enrich::MusicBrainzClient` is an aggregation role trait while
//! `musicbrainz::MusicBrainzClient` is the concrete HTTP client, and the
//! same holds for `LastFmClient`, `LyricsLookup`, `IntegrationStatus`, and
//! `ProviderError`), so the module path is the disambiguator. The shared
//! seams the clients pace, record, and fetch through
//! ([`Pacer`](limiter::Pacer), [`DegradationSink`](degradation::DegradationSink),
//! [`HttpPort`](client::HttpPort)) live in the core modules and are
//! re-exported at this root; see [`adapters`] for their production bridges.

pub mod acoustid;
pub mod adapters;
pub mod archive;
pub mod audiodb;
pub mod cache;
pub mod client;
pub mod coverart;
pub mod degradation;
pub mod discogs;
pub mod enrich;
pub mod error;
pub mod geocoding;
pub mod github;
pub mod itunes;
pub mod lastfm;
pub mod limiter;
pub mod listenbrainz;
pub mod lrclib;
pub mod matrix;
pub mod musicbrainz;
pub mod preview;
pub mod retry;
pub mod singleflight;
pub mod skiddle;
pub mod slots;
pub mod ticketmaster;
pub mod wikidata;
pub mod youtube;

use std::sync::Arc;

pub use adapters::{CorePacer, CoreSink, ReqwestGet};
pub use cache::{
    InMemoryProviderCache, ProviderCache, cache_aside_bytes, cache_aside_json, digest_key,
    invalidate_source, namespaced_key, prefixes_for,
};
pub use client::{
    ContractViolation, HttpFault, HttpPort, HttpReply, ProviderClient, check_client_contract,
};
pub use degradation::{
    DegradationContext, DegradationSink, IntegrationStatus, NoopSink, ProviderOutcome,
    aggregate_status, degraded_none, record_current, record_outcome_current, scoped, with_current,
};
pub use error::{MAX_RETRY_AFTER, ProviderError, classify_status, parse_retry_after};
pub use limiter::{LimiterSet, OverCapacity, Pacer, RateLimiter, RatePolicy, policy_for};
pub use matrix::{OperationKind, SourceRole, apply, outcome_from, role};
#[cfg(any(test, feature = "test-support"))]
pub use retry::ManualClock;
pub use retry::{Clock, RetryPolicy, TokioClock, execute};
pub use singleflight::Singleflight;
pub use slots::{RequestPriority, SlotError, SlotManager, SlotStats, USER_QUIET_WINDOW};

/// The shared provider dependencies, built once at boot and cloned into
/// every provider client: the six verified limiters, the three slot lanes,
/// and the byte cache.
#[derive(Debug)]
pub struct Providers {
    /// One token bucket per verified policy row.
    pub limiters: LimiterSet,
    /// User / image / background slot lanes.
    pub slots: SlotManager,
    /// Shared byte cache behind every client's cache-aside reads.
    pub cache: Arc<dyn ProviderCache>,
}

impl Providers {
    /// Build the shared deps around one cache.
    #[must_use]
    pub fn new(cache: Arc<dyn ProviderCache>) -> Self {
        Self {
            limiters: LimiterSet::new(),
            slots: SlotManager::new(),
            cache,
        }
    }

    /// Build the shared deps with the in-process TTL cache.
    #[must_use]
    pub fn with_memory_cache() -> Self {
        Self::new(Arc::new(InMemoryProviderCache::new()))
    }

    /// One provider's limiter, or `None` for providers without a row.
    #[must_use]
    pub fn limiter(&self, source: &str) -> Option<&RateLimiter> {
        self.limiters.limiter(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deps_carry_all_six_limiters() {
        let providers = Providers::with_memory_cache();
        for source in [
            "musicbrainz",
            "listenbrainz",
            "audiodb",
            "acoustid",
            "coverartarchive",
            "lastfm",
        ] {
            assert!(
                providers.limiter(source).is_some(),
                "deps carry a {source} limiter"
            );
        }
        assert!(providers.limiter("slskd").is_none());
    }
}
