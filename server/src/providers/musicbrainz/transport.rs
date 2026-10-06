//! The wire seam: one GET per call, redirects never followed here.
//!
//! [`MbTransport`] stays a typed local port because the client walks each
//! 3xx hop itself so every hop is validated and reported. The reqwest
//! adapter serves production from the factory's no-redirect client; tests
//! script the port directly.

use thiserror::Error;

/// One outbound GET, transport-agnostic so fakes stay script-only.
#[derive(Debug, Clone)]
pub struct MbRequest {
    /// Full URL without the query string.
    pub url: String,
    /// Query pairs in send order (`fmt=json` is always appended).
    pub query: Vec<(String, String)>,
    /// Headers in send order; the client always sets User-Agent.
    pub headers: Vec<(String, String)>,
}

impl MbRequest {
    /// Render the URL with its encoded query string.
    pub fn full_url(&self) -> String {
        if self.query.is_empty() {
            return self.url.clone();
        }
        let pairs: Vec<String> = self
            .query
            .iter()
            .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
            .collect();
        format!("{}?{}", self.url, pairs.join("&"))
    }

    /// Fetch one header value, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Minimal raw response: status, the headers this client reads, and bytes.
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers in arrival order.
    pub headers: Vec<(String, String)>,
    /// Raw body bytes.
    pub body: Vec<u8>,
}

impl RawResponse {
    /// Build a response from the parts fakes script.
    pub fn new(status: u16, headers: Vec<(&str, &str)>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: headers
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
            body: body.into(),
        }
    }

    /// Fetch one header value, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Wire failure below HTTP semantics (DNS, connect, TLS, timeout, reset).
#[derive(Debug, Clone, Error)]
#[error("musicbrainz transport failure: {0}")]
pub struct TransportError(pub String);

/// The transport port. The reqwest adapter below and the scripted test
/// fakes implement it; the client never touches the network directly.
pub trait MbTransport: Send + Sync {
    /// Perform one GET, following no redirects (the client walks 3xx hops
    /// itself so every hop is validated and reported).
    fn get(
        &self,
        request: &MbRequest,
    ) -> impl Future<Output = Result<RawResponse, TransportError>> + Send;
}

/// Production adapter over the factory's no-redirect client, which carries
/// the shared timeouts and User-Agent.
pub struct ReqwestMbTransport {
    client: reqwest::Client,
}

impl ReqwestMbTransport {
    /// Wrap `HttpClientFactory::no_redirect`. Redirects must stay off
    /// because both MB clients in v2 set `follow_redirects=False` and
    /// validate each hop by hand.
    pub fn new(no_redirect: reqwest::Client) -> Self {
        Self {
            client: no_redirect,
        }
    }
}

impl MbTransport for ReqwestMbTransport {
    async fn get(&self, request: &MbRequest) -> Result<RawResponse, TransportError> {
        let mut outgoing = self.client.get(request.full_url());
        for (key, value) in &request.headers {
            outgoing = outgoing.header(key.as_str(), value.as_str());
        }
        let response = outgoing
            .send()
            .await
            .map_err(|error| TransportError(error.to_string()))?;
        let status = response.status().as_u16();
        let mut headers = Vec::new();
        for name in ["location", "retry-after"] {
            if let Some(value) = response.headers().get(name)
                && let Ok(text) = value.to_str()
            {
                headers.push((name.to_owned(), text.to_owned()));
            }
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| TransportError(error.to_string()))?
            .to_vec();
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }
}

/// Minimal percent-encoding for query pairs (letters, digits, and the
/// unreserved marks pass through; everything else becomes %XX).
fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
