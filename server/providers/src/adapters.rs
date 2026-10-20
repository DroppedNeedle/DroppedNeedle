//! The shared GET port the catalog clients not yet on the executor use.
//!
//! [`ReqwestGet`] serves the catalog [`HttpPort`](super::client::HttpPort)
//! from one shared reqwest client. It goes once those clients move onto
//! the [`executor`](super::executor), which paces through its rate row
//! itself.

use super::client::{HttpFault, HttpPort, HttpReply};
use droppedneedle_kernel::http_client::HttpClientFactory;

/// Largest catalog answer read. Wikidata entities carry every language
/// and Archive.org item metadata lists every file, so either can pass
/// 4 MiB.
const MAX_CATALOG_BODY_BYTES: usize = 16 * 1024 * 1024;

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
        let body =
            match droppedneedle_kernel::http_client::read_capped(response, MAX_CATALOG_BODY_BYTES)
                .await
            {
                Ok(body) => body,
                Err(error) => {
                    tracing::warn!(kind = error.kind(), "catalog answer unreadable");
                    return None;
                }
            };
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
    use super::*;

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
