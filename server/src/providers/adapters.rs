//! Bridges from the shared provider seams onto production.
//!
//! The clients reach the core through three traits ([`Pacer`](super::limiter::Pacer),
//! [`DegradationSink`](super::degradation::DegradationSink),
//! [`HttpPort`](super::client::HttpPort)); this module implements each once
//! against the real core:
//!
//! - [`CorePacer`] paces audio-client calls through the verified per-source
//!   [`RateLimiter`](super::limiter::RateLimiter).
//! - [`CoreSink`] records degradation notes into the request-scoped
//!   [`DegradationContext`](super::degradation::DegradationContext).
//! - [`ReqwestGet`] serves the catalog [`HttpPort`](super::client::HttpPort)
//!   from one shared reqwest client.
//!
//! The `From` impls below map core failures and statuses onto the
//! enrichment leg types; the enrichment aggregator keeps its own leg error
//! (its tests pin that shape) and converts at the boundary.
//!
//! Not unified here, on purpose:
//!
//! - [`ProviderClient`](super::client::ProviderClient) conformance: no
//!   client performs cache writes yet, so claiming `cache_prefixes` would be
//!   vacuous conformance. The impls land with cache-aside integration.
//! - The MusicBrainz `RateGate` / `BrainzMashScheduler` pair: the official
//!   1/s gate is limiter-shaped, but the BrainzMash cooldown scheduler is
//!   endpoint-specific behavior with no core counterpart, so it stays.
//! - `MbTransport` / `CaaTransport`: typed ports whose requests carry
//!   redirect-hop validation the catalog GET port cannot express; each keeps
//!   its own reqwest adapter in its module.
//! - Providers without a verified rate row (LRCLIB, Discogs, iTunes,
//!   Wikidata, Archive, previews, YouTube, geocoding, GitHub, Skiddle,
//!   Ticketmaster): pacing one invents policy, so [`CorePacer::for_source`]
//!   declines them until their rows are verified.

use std::sync::Arc;

use super::{
    Providers,
    client::{HttpFault, HttpPort, HttpReply},
    degradation::{DegradationSink, IntegrationStatus as CoreStatus, record_current},
    enrich,
    error::ProviderError as CoreError,
    limiter::Pacer,
    listenbrainz::{ListenBrainzClient, ListenBrainzCredentials},
    lrclib::LrclibClient,
};
use crate::http_client::HttpClientFactory;

/// Limiter-backed pacing for the audio clients.
///
/// One pacer serves one verified source: [`for_source`](Self::for_source)
/// returns `None` for providers without a verified rate row rather than
/// pacing them against an invented policy. The pacer only takes limiter
/// tokens; slot-lane admission stays at explicit call sites because the
/// `Pacer` seam carries no priority to choose a lane with.
///
/// Open point for the first background caller: [`acquire`](Self::acquire)
/// paces at user priority, and there is no `Outcome` -> retriable bridge
/// to the audio retry seam (unbuilt). Growing the `Pacer` seam with a
/// priority touches every audio client and fake, so it stays until a
/// background caller needs it; the user-priority default is the safe
/// direction (background work waits behind users, never ahead).
#[derive(Debug, Clone)]
pub struct CorePacer {
    providers: Arc<Providers>,
    source: &'static str,
}

impl CorePacer {
    /// Pace `source` through the shared deps, or `None` when the source has
    /// no verified rate row.
    #[must_use]
    pub fn for_source(providers: Arc<Providers>, source: &'static str) -> Option<Self> {
        providers.limiter(source)?;
        Some(Self { providers, source })
    }

    /// Wait for one token from this pacer's bucket.
    pub async fn acquire(&self) {
        if let Some(limiter) = self.providers.limiter(self.source) {
            let _ = limiter.acquire().await;
        }
    }
}

impl Pacer for CorePacer {
    async fn acquire(&self) {
        CorePacer::acquire(self).await;
    }
}

/// Context-backed degradation recording for the provider clients.
///
/// Records into the current request's [`DegradationContext`](super::degradation::DegradationContext)
/// (a no-op outside a request scope) and keeps the cause on the log line.
/// Slice sinks report failures only, so every record lands as `Error`; the
/// deterministic flag stays false because these stringly notes cannot prove
/// a deterministic failure the way a typed [`CoreError`] can.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoreSink;

impl DegradationSink for CoreSink {
    fn record(&self, source: &'static str, message: String) {
        tracing::debug!(source, cause = message, "provider leg degraded");
        record_current(source, CoreStatus::Error, false);
    }
}

/// One shared reqwest transport behind the catalog [`HttpPort`].
///
/// All six catalog clients (Discogs, iTunes, Wikidata, Archive, previews,
/// LRCLIB) share one GET-with-query shape, so one transport serves them all.
/// Anything before a status line (bad URL, DNS, connect, TLS, timeout,
/// reset, truncated body) is an [`HttpFault`]; every answered status maps
/// into the reply, `Retry-After` included, and the client decides what the
/// status means.
#[derive(Debug, Clone)]
pub struct ReqwestGet {
    http: reqwest::Client,
}

impl ReqwestGet {
    /// Serve catalog GETs from this client.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// Serve catalog GETs from the factory's shared client.
    #[must_use]
    pub fn shared(factory: &HttpClientFactory) -> Self {
        Self::new(factory.shared().clone())
    }

    async fn fetch(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Option<(u16, Vec<u8>, Option<String>)> {
        let mut parsed = reqwest::Url::parse(url).ok()?;
        parsed.query_pairs_mut().extend_pairs(query.iter().copied());
        let response = self.http.get(parsed).send().await.ok()?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await.ok()?.to_vec();
        Some((status, body, retry_after))
    }
}

impl HttpPort for ReqwestGet {
    async fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
        let (status, body, retry_after) = self.fetch(url, query).await.ok_or(HttpFault)?;
        Ok(HttpReply {
            status,
            body,
            retry_after,
        })
    }
}

impl From<CoreError> for enrich::ProviderError {
    /// Translate a typed core failure into an enrichment leg failure,
    /// keeping the source key and the human-readable cause.
    fn from(error: CoreError) -> Self {
        Self::new(error.source(), error.to_string())
    }
}

impl From<CoreStatus> for enrich::IntegrationStatus {
    fn from(status: CoreStatus) -> Self {
        match status {
            CoreStatus::Ok => Self::Ok,
            CoreStatus::Degraded => Self::Degraded,
            CoreStatus::Error => Self::Error,
        }
    }
}

impl From<enrich::IntegrationStatus> for CoreStatus {
    fn from(status: enrich::IntegrationStatus) -> Self {
        match status {
            enrich::IntegrationStatus::Ok => Self::Ok,
            enrich::IntegrationStatus::Degraded => Self::Degraded,
            enrich::IntegrationStatus::Error => Self::Error,
        }
    }
}

// ---------------------------------------------------------------------------
// Enrichment roles: concrete clients behind the aggregator traits
// ---------------------------------------------------------------------------

/// Live ListenBrainz popularity behind the enrichment role.
///
/// Popularity reads are anonymous (no token), so this adapter serves
/// production with default credentials. `Missing` (authoritative negative)
/// reads as empty, exactly like the client's fail-soft contract; only
/// `Unavailable` becomes a leg error and degrades the row.
#[derive(Debug, Clone)]
pub struct LiveListenBrainz {
    client: ListenBrainzClient<CorePacer, CoreSink>,
}

impl LiveListenBrainz {
    /// Popularity reads against the live service.
    pub fn new(http: reqwest::Client, pacer: CorePacer) -> Self {
        Self::with_base(http, super::listenbrainz::DEFAULT_BASE_URL, pacer)
    }

    /// Popularity reads against a scripted or mirrored origin.
    pub fn with_base(http: reqwest::Client, base_url: &str, pacer: CorePacer) -> Self {
        Self {
            client: ListenBrainzClient::new(http, base_url, pacer, CoreSink),
        }
    }
}

impl enrich::ListenBrainzClient for LiveListenBrainz {
    fn is_available(&self) -> bool {
        true
    }

    fn artist_top_release_groups<'a>(
        &'a self,
        mbid: &'a str,
        count: usize,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::TopRelease>, enrich::ProviderError>> {
        Box::pin(async move {
            let creds = ListenBrainzCredentials::default();
            match self
                .client
                .artist_top_release_groups(mbid, count, &creds)
                .await
            {
                super::listenbrainz::Outcome::Found(top) => Ok(top
                    .into_iter()
                    .map(|row| enrich::TopRelease {
                        mbid: row.release_group_mbid,
                        listen_count: row.listen_count,
                    })
                    .collect()),
                super::listenbrainz::Outcome::Missing => Ok(Vec::new()),
                super::listenbrainz::Outcome::Unavailable { message, .. } => {
                    Err(enrich::ProviderError::new("listenbrainz", message))
                }
            }
        })
    }

    fn release_group_popularity_batch<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> enrich::BoxFuture<'a, Result<std::collections::HashMap<String, i64>, enrich::ProviderError>>
    {
        Box::pin(async move {
            let creds = ListenBrainzCredentials::default();
            match self.client.release_group_popularity(mbids, &creds).await {
                super::listenbrainz::Outcome::Found(counts) => Ok(counts),
                super::listenbrainz::Outcome::Missing => Ok(std::collections::HashMap::new()),
                super::listenbrainz::Outcome::Unavailable { message, .. } => {
                    Err(enrich::ProviderError::new("listenbrainz", message))
                }
            }
        })
    }
}

/// Live LRCLIB lyrics behind the enrichment role.
///
/// The client borrows its transport, so this adapter owns the [`ReqwestGet`]
/// and builds the client per call. Queries without a usable duration skip
/// the wire and read as not-found: duration is a required match key on
/// `/api/get`, so sending a fabricated zero would invent a query that can
/// never match the track.
#[derive(Debug, Clone)]
pub struct LiveLrclib {
    transport: ReqwestGet,
    base_url: String,
}

impl LiveLrclib {
    /// Lyrics reads against the live service.
    pub fn new(transport: ReqwestGet) -> Self {
        Self::with_base(transport, super::lrclib::API_BASE)
    }

    /// Lyrics reads against a scripted or mirrored origin.
    pub fn with_base(transport: ReqwestGet, base_url: &str) -> Self {
        Self {
            transport,
            base_url: base_url.to_owned(),
        }
    }
}

impl enrich::LyricsClient for LiveLrclib {
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a enrich::LyricsQuery,
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, enrich::ProviderError>> {
        Box::pin(async move {
            let duration = query
                .duration_secs
                .map(|seconds| seconds.round().max(0.0) as u32)
                .filter(|duration| *duration > 0);
            let album = query.album.as_deref().unwrap_or("");
            let Some(duration) = duration else {
                return Ok(enrich::LyricsLookup {
                    found: false,
                    plain: None,
                    synced: None,
                });
            };
            if query.artist.trim().is_empty() || query.title.trim().is_empty() {
                return Ok(enrich::LyricsLookup {
                    found: false,
                    plain: None,
                    synced: None,
                });
            }
            let client = LrclibClient::with_base(&self.transport, &self.base_url);
            match client
                .get_exact_lyrics(&query.title, &query.artist, album, duration)
                .await
            {
                Ok(found) => {
                    let (plain, synced) = match found.candidate {
                        Some(candidate) => (candidate.plain_lyrics, candidate.synced_lyrics),
                        None => (None, None),
                    };
                    Ok(enrich::LyricsLookup {
                        found: found.found,
                        plain,
                        synced,
                    })
                }
                Err(error) => Err(enrich::ProviderError::new("lrclib", lrclib_cause(&error))),
            }
        })
    }
}

/// Log-only cause for an LRCLIB leg failure. The wire never sees this; the
/// response carries the fixed degradation note.
fn lrclib_cause(error: &super::lrclib::FetchError) -> String {
    match error {
        super::lrclib::FetchError::Transport => "lrclib transport failure".to_owned(),
        super::lrclib::FetchError::RateLimited { retry_after_secs } => {
            format!("lrclib rate limited; retry in {retry_after_secs:.1}s")
        }
        super::lrclib::FetchError::Unusable => "lrclib answer unusable".to_owned(),
    }
}

/// ListenBrainz role when the integration is disabled: never available, and
/// every call fails so the aggregator degrades the leg with a note instead
/// of rendering invented counts.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredListenBrainz;

impl enrich::ListenBrainzClient for UnconfiguredListenBrainz {
    fn is_available(&self) -> bool {
        false
    }

    fn artist_top_release_groups<'a>(
        &'a self,
        _mbid: &'a str,
        _count: usize,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::TopRelease>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "listenbrainz",
                "listenbrainz is not configured".to_owned(),
            ))
        })
    }

    fn release_group_popularity_batch<'a>(
        &'a self,
        _mbids: &'a [String],
    ) -> enrich::BoxFuture<'a, Result<std::collections::HashMap<String, i64>, enrich::ProviderError>>
    {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "listenbrainz",
                "listenbrainz is not configured".to_owned(),
            ))
        })
    }
}

/// Last.fm role for production: never available. Credentials are per-user
/// and the enrichment port carries no user, so no request can
/// authenticate a Last.fm call; the aggregator falls back to ListenBrainz
/// or bare echoes. Per-user fan-out awaits a user-scoped port.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredLastFm;

impl enrich::LastFmClient for UnconfiguredLastFm {
    fn is_available(&self) -> bool {
        false
    }

    fn artist_info<'a>(
        &'a self,
        _name: &'a str,
        _mbid: &'a str,
    ) -> enrich::BoxFuture<'a, Result<Option<enrich::LastFmArtistInfo>, enrich::ProviderError>>
    {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "lastfm",
                "lastfm needs per-user credentials the port cannot supply".to_owned(),
            ))
        })
    }

    fn album_info<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _mbid: Option<&'a str>,
    ) -> enrich::BoxFuture<'a, Result<Option<enrich::LastFmAlbumInfo>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "lastfm",
                "lastfm needs per-user credentials the port cannot supply".to_owned(),
            ))
        })
    }
}

/// MusicBrainz role for production: identity lookups always fail. No
/// route serves album-page identity (the discover ports are synchronous
/// and cannot host the async client), so nothing can reach this leg;
/// failing closed keeps any future caller from trusting it.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredMusicBrainz;

impl enrich::MusicBrainzClient for UnconfiguredMusicBrainz {
    fn release_group<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> enrich::BoxFuture<'a, Result<enrich::ReleaseGroupCore, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "musicbrainz",
                "musicbrainz identity is not wired to a route".to_owned(),
            ))
        })
    }

    fn artist_core<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> enrich::BoxFuture<'a, Result<enrich::ArtistCore, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "musicbrainz",
                "musicbrainz identity is not wired to a route".to_owned(),
            ))
        })
    }
}

/// Events role for production: the feed always fails. Concert rows and
/// saved cities live in persistence that does not exist yet (no feed
/// fetching, no city store), so there is nothing to read; the lookup
/// degrades to empty with a note instead of inventing concerts.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredEvents;

impl enrich::EventsClient for UnconfiguredEvents {
    fn concerts_for_user<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::UserConcert>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "events",
                "events feed has no persistence behind it".to_owned(),
            ))
        })
    }

    fn cities<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::EventCity>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "events",
                "events feed has no persistence behind it".to_owned(),
            ))
        })
    }
}

/// Live production enrichment: the search batch adapter plus the lyrics
/// role, built once at boot and handed to [`ReadsSetup::build`](crate::reads::ReadsSetup::build).
pub struct ProductionEnrichment {
    /// Search enrichment behind the reads `EnrichmentPort`.
    pub search: Arc<enrich::AggregatingEnrichment>,
    /// Live lyrics role behind the reads `LyricsPort` (via
    /// `ProviderLyrics`), or `None` when lyrics are disabled, in which case
    /// reads stay on the empty memory port and touch no network.
    pub lyrics: Option<Arc<LiveLrclib>>,
}

/// Lyrics role when the integration is disabled: every call fails so the
/// aggregator degrades the leg with a note instead of rendering invented
/// lyrics.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredLyrics;

impl enrich::LyricsClient for UnconfiguredLyrics {
    fn exact_lyrics<'a>(
        &'a self,
        _query: &'a enrich::LyricsQuery,
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "lrclib",
                "lrclib is not configured".to_owned(),
            ))
        })
    }
}

/// Build the production enrichment pair over the shared HTTP client and
/// provider deps. ListenBrainz popularity serves only when the integration
/// is enabled, and likewise lyrics serve only when `lyrics_enabled` holds;
/// Last.fm stays unconfigured (per-user credentials), the events feed stays
/// unconfigured (no persistence), and MusicBrainz identity stays
/// unconfigured (no route serves it).
pub fn production_enrichment(
    http: &reqwest::Client,
    providers: &Arc<Providers>,
    listenbrainz_enabled: bool,
    lyrics_enabled: bool,
) -> ProductionEnrichment {
    let lb: Arc<dyn enrich::ListenBrainzClient> = match (
        listenbrainz_enabled,
        CorePacer::for_source(providers.clone(), "listenbrainz"),
    ) {
        (true, Some(pacer)) => Arc::new(LiveListenBrainz::new(http.clone(), pacer)),
        _ => Arc::new(UnconfiguredListenBrainz),
    };
    let live_lyrics =
        lyrics_enabled.then(|| Arc::new(LiveLrclib::new(ReqwestGet::new(http.clone()))));
    let lyrics_role: Arc<dyn enrich::LyricsClient> = match &live_lyrics {
        Some(live) => live.clone(),
        None => Arc::new(UnconfiguredLyrics),
    };
    let aggregator = enrich::EnrichmentAggregator::new(
        Arc::new(UnconfiguredMusicBrainz),
        lb,
        Arc::new(UnconfiguredLastFm),
        lyrics_role,
        Arc::new(UnconfiguredEvents),
    );
    ProductionEnrichment {
        search: Arc::new(enrich::AggregatingEnrichment::new(Arc::new(aggregator))),
        lyrics: live_lyrics,
    }
}

#[cfg(test)]
mod tests {
    use super::super::degradation::DegradationContext;
    use super::*;

    #[test]
    fn pacer_covers_verified_sources_only() {
        let providers = Arc::new(Providers::with_memory_cache());
        assert!(CorePacer::for_source(providers.clone(), "lastfm").is_some());
        assert!(CorePacer::for_source(providers.clone(), "musicbrainz").is_some());
        // No verified row: inventing a pace would be fabricated policy.
        assert!(CorePacer::for_source(providers.clone(), "lrclib").is_none());
        assert!(CorePacer::for_source(providers, "slskd").is_none());
    }

    #[tokio::test]
    async fn pacer_acquire_takes_a_limiter_token() {
        let providers = Arc::new(Providers::with_memory_cache());
        let pacer = CorePacer::for_source(providers.clone(), "lastfm").unwrap();
        pacer.acquire().await;
        let limiter = providers.limiter("lastfm").unwrap();
        assert_eq!(limiter.remaining(), 9, "one of the 10 burst tokens is gone");
    }

    #[tokio::test]
    async fn sink_records_failures_into_the_request_scope() {
        let sink = CoreSink;
        let (_, context) = super::super::degradation::scoped(async {
            <CoreSink as DegradationSink>::record(&sink, "lastfm", "error 29".to_owned());
            <CoreSink as DegradationSink>::record(
                &sink,
                "musicbrainz",
                "resolve: reset".to_owned(),
            );
        })
        .await;
        assert!(context.has_degradation());
        assert_eq!(
            context.summary().get("lastfm"),
            Some(&CoreStatus::Error),
            "sink records under the caller's source"
        );
        assert_eq!(
            context.summary().get("musicbrainz"),
            Some(&CoreStatus::Error),
            "musicbrainz sink records under its fixed source"
        );
        let empty = DegradationContext::new();
        assert!(!empty.has_degradation());
    }

    #[test]
    fn core_error_maps_to_a_leg_error_with_its_source() {
        let leg: enrich::ProviderError = CoreError::NotFound { provider: "lastfm" }.into();
        assert_eq!(leg.source, "lastfm");
        assert!(
            leg.message.contains("404"),
            "cause survives: {}",
            leg.message
        );
    }

    #[test]
    fn statuses_translate_both_ways() {
        assert_eq!(
            enrich::IntegrationStatus::from(CoreStatus::Degraded),
            enrich::IntegrationStatus::Degraded
        );
        assert_eq!(
            CoreStatus::from(enrich::IntegrationStatus::Error),
            CoreStatus::Error
        );
    }

    /// The shared transport answers through a module port: status, body,
    /// query encoding, and the `Retry-After` capture, against a scripted
    /// loopback peer. No live network.
    #[tokio::test]
    async fn reqwest_transport_serves_a_catalog_port() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        use super::super::client::HttpPort as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let transport = ReqwestGet::shared(&HttpClientFactory::new().unwrap());
        let pending = tokio::spawn(async move {
            transport
                .get(
                    &format!("http://{address}/api/get"),
                    &[("track_name", "Blue Room"), ("duration", "120")],
                )
                .await
        });

        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "client closed before sending headers");
            raw.extend_from_slice(&chunk[..read]);
            if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&raw);
        assert!(
            head.contains("track_name=Blue+Room"),
            "query pairs are encoded (reqwest form-encodes the space): {head}"
        );
        socket
            .write_all(
                b"HTTP/1.1 429 Slow Down\r\nretry-after: 7\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
            )
            .await
            .unwrap();

        let reply = pending.await.unwrap().unwrap();
        assert_eq!(reply.status, 429);
        assert_eq!(reply.body, b"{}");
        assert_eq!(reply.retry_after.as_deref(), Some("7"));
    }

    #[tokio::test]
    async fn reqwest_transport_maps_wire_faults_to_http_fault() {
        use super::super::client::HttpPort as _;

        // Nothing listens on this TEST-NET address; the failure must read
        // as a fault, never hang or panic.
        let transport = ReqwestGet::shared(&HttpClientFactory::new().unwrap());
        let outcome = transport.get("http://192.0.2.1/", &[]).await;
        assert!(outcome.is_err());
    }

    #[tokio::test]
    async fn unconfigured_roles_fail_with_their_source() {
        use super::super::enrich::{
            EventsClient as _, LastFmClient as _, ListenBrainzClient as _, MusicBrainzClient as _,
        };

        assert!(!UnconfiguredListenBrainz.is_available());
        assert!(!UnconfiguredLastFm.is_available());

        let error = UnconfiguredListenBrainz
            .artist_top_release_groups("mbid", 1)
            .await
            .expect_err("unconfigured popularity errors");
        assert_eq!(error.source, "listenbrainz");
        let error = UnconfiguredLastFm
            .artist_info("name", "mbid")
            .await
            .expect_err("unconfigured lastfm errors");
        assert_eq!(error.source, "lastfm");
        let error = UnconfiguredMusicBrainz
            .release_group("mbid")
            .await
            .expect_err("unconfigured identity errors");
        assert_eq!(error.source, "musicbrainz");
        let error = UnconfiguredEvents
            .cities("user")
            .await
            .expect_err("unconfigured feed errors");
        assert_eq!(error.source, "events");
    }

    #[tokio::test]
    async fn live_lrclib_skips_queries_without_a_match_key() {
        use super::super::enrich::{LyricsClient as _, LyricsQuery};

        let transport = ReqwestGet::shared(&HttpClientFactory::new().unwrap());
        // Unroutable origin: any wire attempt fails, so Ok(not-found)
        // proves the skip.
        let role = LiveLrclib::with_base(transport, "http://127.0.0.1:9/");
        for query in [
            LyricsQuery {
                artist: "Artist".to_owned(),
                title: "Title".to_owned(),
                album: None,
                duration_secs: None,
            },
            LyricsQuery {
                artist: "Artist".to_owned(),
                title: "Title".to_owned(),
                album: None,
                duration_secs: Some(0.4),
            },
            LyricsQuery {
                artist: "  ".to_owned(),
                title: "Title".to_owned(),
                album: None,
                duration_secs: Some(200.0),
            },
        ] {
            let lookup = role
                .exact_lyrics(&query)
                .await
                .expect("skip reads as absence");
            assert!(!lookup.found);
            assert!(lookup.plain.is_none());
            assert!(lookup.synced.is_none());
        }
    }

    /// The role maps popularity rows onto enrichment rows and turns a dead
    /// upstream into a leg error, against a scripted loopback peer. No live
    /// network.
    #[tokio::test]
    async fn live_listenbrainz_role_maps_rows_and_failures() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        use super::super::enrich::ListenBrainzClient as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pacer = CorePacer::for_source(Arc::new(Providers::with_memory_cache()), "listenbrainz")
            .unwrap();
        let role = LiveListenBrainz::with_base(
            HttpClientFactory::new().unwrap().shared().clone(),
            &format!("http://{address}"),
            pacer,
        );
        let pending =
            tokio::spawn(async move { role.artist_top_release_groups("artist-mbid", 5).await });

        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "client closed before sending headers");
            raw.extend_from_slice(&chunk[..read]);
            if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&raw);
        assert!(
            head.contains("/1/popularity/top-release-groups-for-artist/artist-mbid"),
            "role hits the popularity route: {head}"
        );
        let body = r#"[{"release_group_mbid":"rg-a","total_listen_count":10,"release_group":{"name":"Alpha"}},{"release_group_mbid":"","total_listen_count":99}]"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let top = pending.await.unwrap().expect("role maps rows");
        assert_eq!(top.len(), 1, "blank-mbid row skipped: {top:?}");
        assert_eq!(top[0].mbid, "rg-a");
        assert_eq!(top[0].listen_count, 10);

        // A dead upstream becomes a leg error, never invented rows.
        let pacer = CorePacer::for_source(Arc::new(Providers::with_memory_cache()), "listenbrainz")
            .unwrap();
        let dead = LiveListenBrainz::with_base(
            HttpClientFactory::new().unwrap().shared().clone(),
            "http://127.0.0.1:9/",
            pacer,
        );
        let error = dead
            .artist_top_release_groups("artist-mbid", 5)
            .await
            .expect_err("dead upstream errors");
        assert_eq!(error.source, "listenbrainz");
    }

    #[tokio::test]
    async fn production_enrichment_without_listenbrainz_echoes_bare() {
        use crate::reads::search::models::{
            AlbumEnrichmentRequest, ArtistEnrichmentRequest, EnrichmentBatchRequest,
            EnrichmentSource,
        };
        use crate::reads::search::ports::EnrichmentPort as _;

        let http = HttpClientFactory::new().unwrap();
        let providers = Arc::new(Providers::with_memory_cache());
        let pair = production_enrichment(http.shared(), &providers, false, false);
        let response = pair
            .search
            .enrich_batch(EnrichmentBatchRequest {
                artists: vec![ArtistEnrichmentRequest {
                    musicbrainz_id: "artist-mbid".to_owned(),
                    name: "Artist".to_owned(),
                }],
                albums: vec![AlbumEnrichmentRequest {
                    musicbrainz_id: "rg-mbid".to_owned(),
                    artist_name: "Artist".to_owned(),
                    album_name: "Album".to_owned(),
                }],
            })
            .await
            .expect("unconfigured batch still answers");
        assert_eq!(response.source, EnrichmentSource::None);
        assert!(response.degradations.is_empty());
        assert_eq!(response.artists.len(), 1);
        assert_eq!(response.artists[0].listen_count, None);
        assert_eq!(response.albums.len(), 1);
        assert_eq!(response.albums[0].listen_count, None);
    }

    #[test]
    fn production_enrichment_gates_live_lyrics_on_the_flag() {
        let http = HttpClientFactory::new().unwrap();
        let providers = Arc::new(Providers::with_memory_cache());
        let off = production_enrichment(http.shared(), &providers, false, false);
        assert!(off.lyrics.is_none(), "disabled lyrics wire no live client");
        let on = production_enrichment(http.shared(), &providers, false, true);
        assert!(on.lyrics.is_some(), "enabled lyrics wire the live role");
    }
}
