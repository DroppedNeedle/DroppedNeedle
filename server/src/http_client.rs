//! The single factory for outbound HTTP clients.
//!
//! The factory owns timeouts, pool behavior, and the DroppedNeedle
//! User-Agent. It builds two clients once at boot with the same settings:
//! the shared one follows redirects, the [`no_redirect`](HttpClientFactory::no_redirect)
//! one never does, for callers that validate every hop themselves or must
//! not be bounced to another host. Per-provider rate policy lives in
//! `provider_policy`, and per-upstream resilience lives with each client.

use std::time::Duration;

use reqwest::{Client, redirect::Policy};
use thiserror::Error;

/// Default total request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default TCP/TLS connect timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default idle connections kept per host.
pub const DEFAULT_MAX_IDLE_PER_HOST: usize = 50;
/// Default User-Agent contact address.
pub const DEFAULT_CONTACT_EMAIL: &str = "contact@droppedneedle.com";

/// Outbound client tuning, read from the deployment environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSettings {
    /// Total request timeout.
    pub timeout: Duration,
    /// TCP/TLS connect timeout.
    pub connect_timeout: Duration,
    /// Idle connections kept per host.
    pub max_idle_per_host: usize,
    /// Contact address carried in the User-Agent.
    pub contact_email: String,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_idle_per_host: DEFAULT_MAX_IDLE_PER_HOST,
            contact_email: DEFAULT_CONTACT_EMAIL.to_owned(),
        }
    }
}

impl HttpSettings {
    /// The User-Agent every outbound request carries. MusicBrainz asks for
    /// an application name, version and contact address.
    pub fn user_agent(&self) -> String {
        format!(
            "DroppedNeedle/{} ( {} ; +https://github.com/DroppedNeedle/DroppedNeedle )",
            env!("CARGO_PKG_VERSION"),
            self.contact_email
        )
    }
}

/// Builds the outbound clients. Cheap to clone; build once at boot.
#[derive(Debug, Clone)]
pub struct HttpClientFactory {
    client: Client,
    no_redirect: Client,
}

impl HttpClientFactory {
    /// Build both clients with the default settings.
    pub fn new() -> Result<Self, HttpClientError> {
        Self::with_settings(&HttpSettings::default())
    }

    /// Build both clients from the deployment settings.
    pub fn with_settings(settings: &HttpSettings) -> Result<Self, HttpClientError> {
        let builder = || {
            Client::builder()
                .user_agent(settings.user_agent())
                .timeout(settings.timeout)
                .connect_timeout(settings.connect_timeout)
                .pool_max_idle_per_host(settings.max_idle_per_host)
        };
        Ok(Self {
            client: builder().build()?,
            no_redirect: builder().redirect(Policy::none()).build()?,
        })
    }

    /// The shared client for upstreams with default behavior.
    pub fn shared(&self) -> &Client {
        &self.client
    }

    /// The same client with redirects off: a 3xx comes back as the
    /// response instead of being followed.
    pub fn no_redirect(&self) -> &Client {
        &self.no_redirect
    }
}

/// Outbound client construction failure.
#[derive(Debug, Error)]
pub enum HttpClientError {
    /// The HTTP client could not be built (TLS backend or pool setup).
    #[error("cannot build HTTP client: {0}")]
    Build(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// Serve one request with `reply` and return the request head.
    async fn one_request(client: Client, reply: &'static [u8]) -> (String, u16) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pending =
            tokio::spawn(async move { client.get(format!("http://{address}/")).send().await });
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "client closed before sending headers");
            raw.extend_from_slice(&chunk[..read]);
        }
        socket.write_all(reply).await.unwrap();
        let status = pending.await.unwrap().unwrap().status().as_u16();
        (String::from_utf8(raw).unwrap().to_lowercase(), status)
    }

    #[tokio::test]
    async fn both_clients_send_the_contact_user_agent_and_only_one_redirects() {
        let settings = HttpSettings {
            contact_email: "ops@example.org".to_owned(),
            ..HttpSettings::default()
        };
        let factory = HttpClientFactory::with_settings(&settings).unwrap();
        let redirect: &[u8] = b"HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:1/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

        let (head, status) = one_request(factory.no_redirect().clone(), redirect).await;
        assert_eq!(status, 302, "the no-redirect client hands back the 3xx");
        assert!(head.contains("user-agent: droppedneedle/"));
        assert!(head.contains("ops@example.org"));

        let ok: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        let (head, status) = one_request(factory.shared().clone(), ok).await;
        assert_eq!(status, 200);
        assert!(head.contains(&format!(
            "user-agent: {}",
            settings.user_agent().to_lowercase()
        )));
    }
}
