//! The outside services DroppedNeedle reads from, and the one way a
//! request to them goes out.
//!
//! [`Providers`] is built once at boot and handed to every area. It holds
//! one client per service (MusicBrainz, the Cover Art Archive,
//! ListenBrainz, Last.fm, TheAudioDB, AcoustID, Discogs, iTunes, Deezer,
//! LRCLIB, Wikidata, Ticketmaster, Skiddle, Open-Meteo, YouTube, GitHub
//! and the image hosts), the limiters, the provider answer cache and the
//! service health registry. Each client sends through its own [`Executor`].
//!
//! - [`executor`]: sends one request. It waits for a token at the caller's
//!   priority, sends with a deadline, reads the body under a cap, types the
//!   answer, retries what may be retried inside a budget, turns a
//!   `Retry-After` into a cooldown, and records the outcome for service
//!   health and the request's degradation notes.
//! - [`rates`]: the rate table, the only place a provider's pace is
//!   written, each row with the date and source it was read from.
//! - [`limiter`]: one token bucket per row, with priority waiters,
//!   cooldowns and in-flight caps, and the [`RequestPriority`] every caller
//!   names.
//! - [`error`]: [`ProviderError`], which keeps 401, 403, 404 and 400
//!   apart, and the `Retry-After` reader.
//! - [`health`]: which services are degraded right now.
//! - [`degradation`]: the per-request record of which sources failed.
//! - [`cache`]: provider answers kept in the cache file, keyed by source,
//!   behind one fetch helper.
//! - [`singleflight`]: one fetch for many identical callers.
//! - [`calls`]: the outbound calls each row has sent since start, by
//!   priority and outcome, for the admin view.
//!
//! Each client is reached by its module path
//! (`providers::musicbrainz::MusicBrainzClient`). The names are not
//! re-exported here because several collide across modules.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod acoustid;
pub mod audiodb;
pub mod cache;
pub mod calls;
mod client;
pub mod coverart;
pub mod degradation;
pub mod discogs;
pub mod error;
pub mod executor;
pub mod geocoding;
pub mod github;
pub mod health;
pub mod images;
pub mod itunes;
pub mod lastfm;
pub mod limiter;
pub mod listenbrainz;
pub mod lrclib;
pub mod musicbrainz;
pub mod plextv;
pub mod preview;
pub mod rates;
pub mod singleflight;
pub mod skiddle;
pub mod ticketmaster;
pub mod wikidata;
pub mod youtube;

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::cache::CacheDb;
use droppedneedle_kernel::platform::Platform;

pub use cache::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
pub use calls::{CallCount, CallOutcome};
pub use degradation::{DegradationContext, IntegrationStatus, record_current, scoped};
pub use error::{MAX_RETRY_AFTER, PayloadKind, ProviderError, TransportKind};
pub use executor::{Executor, ReqwestWire, Wire};
pub use limiter::{LimiterSet, OverCapacity, RateLimiter, RatePolicy, RequestPriority};
pub use singleflight::Singleflight;

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
    /// Last.fm API root.
    pub lastfm: String,
    /// TheAudioDB API root, without the key segment.
    pub audiodb: String,
    /// AcoustID API origin.
    pub acoustid: String,
    /// Discogs API origin.
    pub discogs: String,
    /// iTunes Search endpoint.
    pub itunes: String,
    /// Deezer API origin.
    pub deezer: String,
    /// LRCLIB origin.
    pub lrclib: String,
    /// Wikidata origin.
    pub wikidata: String,
    /// Wikipedia origin pattern with `{lang}`.
    pub wikipedia: String,
    /// Wikimedia Commons origin.
    pub commons: String,
    /// Ticketmaster Discovery root.
    pub ticketmaster: String,
    /// Skiddle API root.
    pub skiddle: String,
    /// Open-Meteo geocoding search URL.
    pub geocoding: String,
    /// YouTube Data API host.
    pub youtube: String,
    /// The releases list behind the version check.
    pub github_releases: String,
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
            lastfm: "http://127.0.0.1:9/lastfm/".to_owned(),
            audiodb: "http://127.0.0.1:9/audiodb".to_owned(),
            acoustid: "http://127.0.0.1:9/acoustid".to_owned(),
            discogs: "http://127.0.0.1:9/discogs".to_owned(),
            itunes: "http://127.0.0.1:9/itunes/search".to_owned(),
            deezer: "http://127.0.0.1:9/deezer".to_owned(),
            lrclib: "http://127.0.0.1:9/lrclib".to_owned(),
            wikidata: "http://127.0.0.1:9/wikidata".to_owned(),
            wikipedia: "http://127.0.0.1:9/wikipedia-{lang}".to_owned(),
            commons: "http://127.0.0.1:9/commons".to_owned(),
            ticketmaster: "http://127.0.0.1:9/ticketmaster".to_owned(),
            skiddle: "http://127.0.0.1:9/skiddle".to_owned(),
            geocoding: "http://127.0.0.1:9/geocoding".to_owned(),
            youtube: "http://127.0.0.1:9/youtube".to_owned(),
            github_releases: "http://127.0.0.1:9/releases".to_owned(),
        }
    }
}

/// The shared provider dependencies, built once at boot and shared by
/// every area: the limiters of the rate table, the provider answer cache,
/// the service health registry, and the clients that send through the
/// executor.
#[derive(Debug)]
pub struct Providers {
    /// One token bucket per rate table row, each counting its calls.
    pub limiters: LimiterSet,
    /// Provider answers kept in the cache file.
    pub cache: ProviderCacheStore,
    /// Which services are degraded right now, for `GET /system/health`.
    pub health: Arc<health::ServiceHealth>,
    /// MusicBrainz: the official service, BrainzMash and a mirror.
    pub musicbrainz: musicbrainz::MusicBrainzClient,
    /// The Cover Art Archive.
    pub coverart: coverart::CaaClient,
    /// ListenBrainz: every read, the credential check and scrobble
    /// forwarding, on one bucket.
    pub listenbrainz: listenbrainz::ListenBrainzClient,
    /// Last.fm: every read, account linking and scrobble forwarding, on
    /// one bucket.
    pub lastfm: lastfm::LastFmClient,
    /// TheAudioDB lookups, on the free or the premium row by key.
    pub audiodb: audiodb::AudioDbClient,
    /// AcoustID lookups for identification, imports and Library
    /// Management, on one bucket.
    pub acoustid: acoustid::AcoustIdClient,
    /// Artist images and TheAudioDB thumbnails by URL.
    pub images: images::ImageClient,
    /// Discogs releases for contributions.
    pub discogs: discogs::DiscogsClient,
    /// iTunes purchase links; the preview leg shares its bucket.
    pub itunes: itunes::ITunesClient,
    /// 30-second previews: Deezer, then iTunes.
    pub previews: preview::PreviewClient,
    /// LRCLIB lyrics.
    pub lrclib: lrclib::LrclibClient,
    /// Wikidata, Wikipedia and Commons, on one bucket.
    pub wikidata: wikidata::WikidataClient,
    /// Ticketmaster events for the concerts sweep.
    pub ticketmaster: ticketmaster::TicketmasterClient,
    /// Skiddle events for the concerts sweep.
    pub skiddle: skiddle::SkiddleClient,
    /// Open-Meteo city search for the concerts city picker.
    pub geocoding: geocoding::GeocodingClient,
    /// The YouTube Data API, which the one quota-keeping YouTube client
    /// sends through.
    pub youtube: youtube::YouTubeApi,
    /// GitHub releases for the version check.
    pub github: github::GitHubClient,
}

impl Providers {
    /// The providers over the real network, with the platform's HTTP
    /// clients and clock, keeping answers in `cache`.
    #[must_use]
    pub fn build(platform: &Platform, endpoints: ProviderEndpoints, cache: CacheDb) -> Self {
        Self::assemble(
            Arc::new(executor::ReqwestWire::new(&platform.http)),
            Arc::clone(&platform.clock),
            endpoints,
            LimiterSet::new(),
            cache,
        )
    }

    /// The providers over `wire`, reading wall time from `clock`, with a
    /// cache in a scratch file. Tests pass a scripted wire or one that
    /// reaches a loopback server.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn over_wire(
        wire: Arc<dyn Wire>,
        clock: Arc<dyn Clock>,
        endpoints: ProviderEndpoints,
    ) -> Self {
        Self::assemble(
            wire,
            clock,
            endpoints,
            LimiterSet::new(),
            CacheDb::scratch(),
        )
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
            CacheDb::scratch(),
        )
    }

    fn assemble(
        wire: Arc<dyn Wire>,
        clock: Arc<dyn Clock>,
        endpoints: ProviderEndpoints,
        limiters: LimiterSet,
        cache: CacheDb,
    ) -> Self {
        let cache = ProviderCacheStore::new(cache, Arc::clone(&clock));
        let health = Arc::new(health::ServiceHealth::new());
        let executor = |spec: &'static executor::Spec, row: &str| {
            Executor::new(
                spec,
                limiters.for_executor(row),
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
        let lastfm =
            lastfm::LastFmClient::new(executor(&lastfm::SPEC, lastfm::SOURCE), &endpoints.lastfm);
        let audiodb = audiodb::AudioDbClient::new(
            executor(&audiodb::SPEC, audiodb::SOURCE),
            executor(&audiodb::SPEC, audiodb::PREMIUM_ROW),
            &endpoints.audiodb,
        );
        let acoustid = acoustid::AcoustIdClient::new(
            executor(&acoustid::SPEC, acoustid::SOURCE),
            &endpoints.acoustid,
        );
        let images = images::ImageClient::new(
            executor(&images::AUDIODB_SPEC, images::AUDIODB_MEDIA),
            executor(&images::WIKIMEDIA_SPEC, images::WIKIMEDIA_MEDIA),
            executor(&images::DEEZER_SPEC, preview::DEEZER),
        );
        let discogs = discogs::DiscogsClient::new(
            executor(&discogs::SPEC, discogs::SOURCE),
            &endpoints.discogs,
        );
        let itunes =
            itunes::ITunesClient::new(executor(&itunes::SPEC, itunes::SOURCE), &endpoints.itunes);
        let previews = preview::PreviewClient::new(
            executor(&preview::DEEZER_SPEC, preview::DEEZER),
            &endpoints.deezer,
            itunes.clone(),
        );
        let lrclib =
            lrclib::LrclibClient::new(executor(&lrclib::SPEC, lrclib::SOURCE), &endpoints.lrclib);
        let wikidata = wikidata::WikidataClient::new(
            executor(&wikidata::SPEC, wikidata::RATE_ROW),
            &endpoints.wikidata,
            &endpoints.wikipedia,
            &endpoints.commons,
        );
        let ticketmaster = ticketmaster::TicketmasterClient::new(
            executor(&ticketmaster::SPEC, ticketmaster::SOURCE),
            &endpoints.ticketmaster,
        );
        let skiddle = skiddle::SkiddleClient::new(
            executor(&skiddle::SPEC, skiddle::SOURCE),
            &endpoints.skiddle,
        );
        let geocoding = geocoding::GeocodingClient::new(
            executor(&geocoding::SPEC, geocoding::RATE_ROW),
            &endpoints.geocoding,
        );
        let youtube = youtube::YouTubeApi::new(
            executor(&youtube::SPEC, youtube::SOURCE),
            &endpoints.youtube,
        );
        let github = github::GitHubClient::new(
            executor(&github::SPEC, github::SOURCE),
            &endpoints.github_releases,
        );
        Self {
            limiters,
            cache,
            health,
            musicbrainz,
            coverart,
            listenbrainz,
            lastfm,
            audiodb,
            acoustid,
            images,
            discogs,
            itunes,
            previews,
            lrclib,
            wikidata,
            ticketmaster,
            skiddle,
            geocoding,
            youtube,
            github,
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

#[cfg(test)]
mod tests {
    use super::Providers;

    /// Building the providers hands every executor its row; a row name
    /// missing from the rate table stops the build in a debug test.
    #[test]
    fn every_executor_names_a_rate_row() {
        let providers = Providers::closed();
        assert!(providers.limiter("musicbrainz").is_some());
    }
}
