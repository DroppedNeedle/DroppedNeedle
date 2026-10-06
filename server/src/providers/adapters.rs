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
//! Not unified here, on purpose:
//!
//! - [`ProviderClient`](super::client::ProviderClient) conformance: no
//!   client performs cache writes yet, so claiming `cache_prefixes` would be
//!   vacuous conformance. The impls land with cache-aside integration.
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
    limiter::Pacer,
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
    /// no verified rate row. The row is checked against the policy table,
    /// not the limiter set, so the test-only unpaced set still builds
    /// clients (and never waits).
    #[must_use]
    pub fn for_source(providers: Arc<Providers>, source: &'static str) -> Option<Self> {
        super::limiter::policy_for(source)?;
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
/// Client sinks report failures only, so every record lands as `Error`; the
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
}
