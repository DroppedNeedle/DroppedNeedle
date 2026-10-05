//! The smallest HTTP surface the slskd client needs.
//!
//! A local seam on purpose rather than the provider catalog `HttpPort`:
//! that port is GET-only with catalog rate rows and cache prefixes, while
//! slskd needs GET, POST, and DELETE, serializes through its own semaphores
//! and caches nothing. Anything before a status line (bad URL, DNS, connect, TLS,
//! timeout, reset, truncated body) is an [`HttpFault`]; every answered
//! status maps into the reply and the client decides what it means.

use std::future::Future;

/// One answered HTTP response: status plus raw body.
#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The transport broke down before producing a status code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFault(pub String);

/// GET + POST + DELETE against one base URL. Bodies are raw bytes; the
/// client owns JSON encoding. `query` holds raw `key=value` pairs.
pub trait SlskdHttp: Send + Sync {
    fn get(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> impl Future<Output = Result<HttpReply, HttpFault>> + Send;
    fn post(
        &self,
        path: &str,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<HttpReply, HttpFault>> + Send;
    fn delete(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> impl Future<Output = Result<HttpReply, HttpFault>> + Send;
}

/// Production [`SlskdHttp`] over one shared reqwest client.
///
/// The `reqwest::Client` is injected (the HTTP client is owned by the
/// caller, never acquired here). The API key travels only in the
/// `X-API-Key` header and is never logged. Debug is hand-written: the key
/// never appears in debug output.
#[derive(Clone)]
pub struct ReqwestSlskdHttp {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl std::fmt::Debug for ReqwestSlskdHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReqwestSlskdHttp")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl ReqwestSlskdHttp {
    #[must_use]
    pub fn new(http: reqwest::Client, base_url: &str, api_key: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v0{path}", self.base_url)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<HttpReply, HttpFault> {
        let response = request
            .send()
            .await
            .map_err(|err| HttpFault(err.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|err| HttpFault(err.to_string()))?
            .to_vec();
        Ok(HttpReply { status, body })
    }
}

impl SlskdHttp for ReqwestSlskdHttp {
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
        let request = self
            .http
            .get(self.url(path))
            .header("X-API-Key", &self.api_key)
            .query(query);
        self.send(request).await
    }

    async fn post(&self, path: &str, body: Vec<u8>) -> Result<HttpReply, HttpFault> {
        let request = self
            .http
            .post(self.url(path))
            .header("X-API-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .body(body);
        self.send(request).await
    }

    async fn delete(&self, path: &str, query: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
        let request = self
            .http
            .delete(self.url(path))
            .header("X-API-Key", &self.api_key)
            .query(query);
        self.send(request).await
    }
}
