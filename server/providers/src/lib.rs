//! The outside services DroppedNeedle reads from, and the one way a
//! request to them goes out.
//!
//! [`Providers`] is built once at boot and handed to every area. It holds
//! one client per service (MusicBrainz, the Cover Art Archive,
//! ListenBrainz, Last.fm, TheAudioDB, AcoustID, Discogs, iTunes, Deezer,
//! LRCLIB, Wikidata, Ticketmaster, Skiddle, Open-Meteo, YouTube, GitHub,
//! Spotify, plex.tv, Pwned Passwords, the configured OIDC provider, the
//! user's Newznab indexers and the image hosts), the limiters, the provider
//! answer cache and the service health registry. Each client sends through
//! its own [`Executor`].
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
//! - [`web_address`]: the http-or-https rule for every link address an
//!   answer carries.
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
pub mod hibp;
pub mod images;
pub mod itunes;
pub mod lastfm;
pub mod limiter;
pub mod listenbrainz;
pub mod lrclib;
pub mod musicbrainz;
pub mod newznab;
pub mod oidc;
pub mod plextv;
pub mod preview;
pub mod rates;
pub mod singleflight;
pub mod skiddle;
pub mod spotify;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod ticketmaster;
pub mod web_address;
pub mod wikidata;
pub mod youtube;

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::cache::CacheDb;
use droppedneedle_kernel::platform::Platform;

pub use cache::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
pub use calls::CallOutcome;
pub use degradation::{IntegrationStatus, record_current, scoped};
pub use error::{PayloadKind, ProviderError, TransportKind};
pub use executor::{Executor, ReqwestWire, Wire};
pub use limiter::{LimiterSet, RateLimiter, RatePolicy, RequestPriority};
pub use singleflight::Singleflight;
pub use web_address::WebAddress;

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
    /// GitHub API origin, which plugin installs read.
    pub github_api: String,
    /// GitHub archive download origin for plugin installs.
    pub github_codeload: String,
    /// Spotify Web API root.
    pub spotify_api: String,
    /// Spotify accounts origin: the authorize page and the token grants.
    pub spotify_accounts: String,
    /// A playlist cover origin allowed beside Spotify's CDN: the tests'
    /// mock CDN. `None` in production.
    pub spotify_cover_cdn: Option<String>,
    /// The origin Deezer serves artist pictures from.
    pub deezer_images: String,
    /// plex.tv account API root.
    pub plex_tv: String,
    /// The Pwned Passwords range API, up to the hash prefix.
    pub pwned_passwords: String,
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
    /// Artist images, TheAudioDB thumbnails and Spotify playlist covers by
    /// URL.
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
    /// GitHub repository reads for plugin installs, on the version
    /// check's row.
    pub github_repos: github::GitHubRepos,
    /// Spotify: the token grants and the playlist reads of an import.
    pub spotify: spotify::SpotifyClient,
    /// plex.tv: the Plex sign-in, the Plex user import and the media
    /// server adapter's token lookup, on one bucket.
    pub plextv: plextv::PlexTvClient,
    /// The user's Newznab indexers, each on a bucket of its own shaped by
    /// the `newznab` row, held to the daily caps it reports.
    pub newznab: newznab::NewznabIndexers,
    /// Pwned Passwords range lookups for the password screen.
    pub hibp: hibp::HibpClient,
    /// The OIDC provider an admin configured: discovery, signing keys,
    /// the token endpoint and userinfo, on one bucket.
    pub oidc: oidc::OidcClient,
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

    pub(crate) fn assemble(
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
            images::ImageExecutors {
                audiodb: executor(&images::AUDIODB_SPEC, images::AUDIODB_MEDIA),
                wikimedia: executor(&images::WIKIMEDIA_SPEC, images::WIKIMEDIA_MEDIA),
                deezer: executor(&images::DEEZER_SPEC, preview::DEEZER),
                spotify: executor(&images::SPOTIFY_SPEC, spotify::SOURCE),
            },
            images::ImageHosts::new(
                &endpoints.deezer_images,
                endpoints.spotify_cover_cdn.as_deref(),
            ),
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
        let github_api = executor(&github::SPEC, github::SOURCE);
        let github = github::GitHubClient::new(github_api.clone(), &endpoints.github_releases);
        let github_repos = github::GitHubRepos::new(
            github_api,
            executor(&github::ARCHIVE_SPEC, github::SOURCE),
            &endpoints.github_api,
            &endpoints.github_codeload,
        );
        let spotify = spotify::SpotifyClient::new(
            executor(&spotify::SPEC, spotify::SOURCE),
            &endpoints.spotify_api,
            &endpoints.spotify_accounts,
        );
        let plextv =
            plextv::PlexTvClient::new(executor(&plextv::SPEC, plextv::SOURCE), &endpoints.plex_tv);
        let newznab = newznab::NewznabIndexers::new(
            limiters.for_executor(newznab::SOURCE),
            Arc::clone(&wire),
            Arc::clone(&health),
            Arc::clone(&clock),
        );
        let hibp = hibp::HibpClient::new(
            executor(&hibp::SPEC, hibp::SOURCE),
            &endpoints.pwned_passwords,
        );
        let oidc = oidc::OidcClient::new(
            executor(&oidc::SPEC, oidc::SOURCE),
            executor(&oidc::TOKEN_SPEC, oidc::SOURCE),
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
            github_repos,
            spotify,
            plextv,
            newznab,
            hibp,
            oidc,
        }
    }

    /// One provider's limiter, or `None` for providers without a row.
    #[must_use]
    pub fn limiter(&self, source: &str) -> Option<&RateLimiter> {
        self.limiters.limiter(source)
    }
}
