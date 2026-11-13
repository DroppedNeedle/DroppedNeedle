//! The smallest HTTP surface the slskd client needs.
//!
//! slskd is a service the user runs, so it is called through this seam
//! rather than a provider client: it needs GET, POST and DELETE, serializes
//! through its own semaphores, is never paced and caches nothing. Anything before a status line (bad URL, DNS, connect, TLS,
//! timeout, reset, truncated body) is an [`HttpFault`]; every answered
//! status maps into the reply and the client decides what it means.

use std::future::Future;
use std::time::Duration;

use droppedneedle_kernel::http_client::{BodyError, read_capped, transport_error};

use crate::client_error::retry_after;

/// Largest slskd answer read. A search's responses list every file every
/// peer offered and the transfer list is unpaged, so both can run to
/// tens of megabytes.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// One answered HTTP response: status, the `Retry-After` wait when the
/// server sent one, and the raw body.
#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: Vec<u8>,
}

/// The transport broke down before producing a status code. The detail
/// never holds the request URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFault(pub String);

/// GET + POST + DELETE against one base URL. `path` is the route below
/// `/api/v0`, one value per path segment: each is encoded as one segment,
/// so a peer name holding `/`, `?` or `#` stays one name. Bodies are raw
/// bytes; the client owns JSON encoding. `query` holds raw `key=value`
/// pairs.
pub trait SlskdHttp: Send + Sync {
    fn get(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> impl Future<Output = Result<HttpReply, HttpFault>> + Send;
    fn post(
        &self,
        path: &[&str],
        body: Vec<u8>,
    ) -> impl Future<Output = Result<HttpReply, HttpFault>> + Send;
    fn delete(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> impl Future<Output = Result<HttpReply, HttpFault>> + Send;
}

/// Production [`SlskdHttp`] over one shared reqwest client.
///
/// The `reqwest::Client` is injected (the HTTP client is owned by the
/// caller, never acquired here) and follows a redirect only on the same
/// host. The API key travels only in the `X-API-Key` header, which a
/// redirect elsewhere would carry to another address, and is never logged.
/// Debug is hand-written: the key never appears in debug output.
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

    /// The address of `path` below `/api/v0`, built on the parsed base URL
    /// with each value pushed as one encoded segment.
    fn url(&self, path: &[&str]) -> Result<reqwest::Url, HttpFault> {
        let unusable = || HttpFault("the slskd URL is not a usable address".to_owned());
        let mut url = reqwest::Url::parse(&self.base_url).map_err(|_| unusable())?;
        url.path_segments_mut()
            .map_err(|()| unusable())?
            .pop_if_empty()
            .extend(["api", "v0"])
            .extend(path);
        Ok(url)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<HttpReply, HttpFault> {
        let response = request
            .send()
            .await
            .map_err(|err| HttpFault(transport_error(err)))?;
        let status = response.status().as_u16();
        let retry_after = retry_after(response.headers());
        let body = read_capped(response, MAX_BODY_BYTES)
            .await
            .map_err(|cause| match cause {
                BodyError::TooLarge => HttpFault("slskd answer exceeded the size limit".to_owned()),
                BodyError::Transport(cause) => HttpFault(cause),
            })?;
        Ok(HttpReply {
            status,
            retry_after,
            body,
        })
    }
}

impl SlskdHttp for ReqwestSlskdHttp {
    async fn get(&self, path: &[&str], query: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
        let request = self
            .http
            .get(self.url(path)?)
            .header("X-API-Key", &self.api_key)
            .query(query);
        self.send(request).await
    }

    async fn post(&self, path: &[&str], body: Vec<u8>) -> Result<HttpReply, HttpFault> {
        let request = self
            .http
            .post(self.url(path)?)
            .header("X-API-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .body(body);
        self.send(request).await
    }

    async fn delete(&self, path: &[&str], query: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
        let request = self
            .http
            .delete(self.url(path)?)
            .header("X-API-Key", &self.api_key)
            .query(query);
        self.send(request).await
    }
}
