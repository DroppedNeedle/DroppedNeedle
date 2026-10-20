//! The single factory for outbound HTTP clients.
//!
//! The factory owns timeouts, pool behavior, and the DroppedNeedle
//! User-Agent. It builds three clients once at boot: the shared one follows
//! redirects, the [`no_redirect`](HttpClientFactory::no_redirect) one never
//! does, for callers that validate every hop themselves or must not be
//! bounced to another host, and the
//! [`direct_no_redirect`](HttpClientFactory::direct_no_redirect) one also
//! skips any proxy and has no total timeout, for plugin audio streams.
//! Provider rates live in the providers crate's rate table, and every
//! provider in that crate sends through its executor, which paces,
//! retries and cools down.

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
    remote_audio: Client,
    direct: Client,
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
            // Remote server audio: no total timeout (a long track streams
            // for minutes; the caller times the answer and each chunk), and
            // no redirects, so a server token never goes to another host.
            remote_audio: Client::builder()
                .user_agent(settings.user_agent())
                .connect_timeout(settings.connect_timeout)
                .redirect(Policy::none())
                .build()?,
            // No total timeout: it carries long audio streams, which the
            // caller paces chunk by chunk instead.
            direct: Client::builder()
                .user_agent(settings.user_agent())
                .connect_timeout(settings.connect_timeout)
                .redirect(Policy::none())
                .no_proxy()
                .build()?,
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

    /// For audio streamed from the user's own remote servers (Jellyfin,
    /// Navidrome, Plex): no total timeout and no redirects.
    pub fn remote_audio(&self) -> &Client {
        &self.remote_audio
    }

    /// Redirects off and no proxy: the connection goes straight to the
    /// address the client resolved, so a caller checking the connected
    /// peer (plugin stream URLs) sees the real server, not a proxy.
    pub fn direct_no_redirect(&self) -> &Client {
        &self.direct
    }
}

/// Outbound client construction failure.
#[derive(Debug, Error)]
pub enum HttpClientError {
    /// The HTTP client could not be built (TLS backend or pool setup).
    #[error("cannot build HTTP client: {0}")]
    Build(#[from] reqwest::Error),
}

/// What went wrong with a request, safe to log, store or show.
///
/// A reqwest error prints the URL it was sent to, and that URL often
/// holds a secret: SABnzbd and Newznab take the API key as a query
/// parameter, and a user's base URL can carry userinfo or a proxy path.
/// This drops the URL and keeps the causes under it (connection refused,
/// timed out, bad certificate), which describe the socket and never the
/// request. Checked against reqwest 0.12.28 with hyper-util 0.1.21.
pub fn transport_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    append_causes(&error, &mut text);
    text.chars().take(300).collect()
}

/// Append what lies under `error`, one cause after another. A cause that
/// is itself a reqwest error is passed over, since it prints a URL of its
/// own and one borrowed here cannot have it removed (a redirect refused
/// under `https_only` nests one). What lies under it is still kept.
fn append_causes(error: &(dyn std::error::Error + 'static), text: &mut String) {
    let mut cause = error.source();
    while let Some(inner) = cause {
        if !inner.is::<reqwest::Error>() {
            text.push_str(": ");
            text.push_str(&inner.to_string());
        }
        cause = inner.source();
    }
}

/// Why a response body could not be read.
pub enum BodyError {
    /// The body is larger than the cap it was read under.
    TooLarge,
    /// The connection broke part-way. Never holds the request URL.
    Transport(String),
}

impl BodyError {
    /// A short fixed description, for errors that must not carry
    /// transport text.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::TooLarge => "answer too large",
            Self::Transport(_) => "body read failed",
        }
    }
}

/// Read a body, giving up as soon as it passes `cap` bytes, whether or
/// not the server declared its length.
pub async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<Vec<u8>, BodyError> {
    if response
        .content_length()
        .is_some_and(|length| length > cap as u64)
    {
        return Err(BodyError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| BodyError::Transport(transport_error(error)))?
    {
        if body.len() + chunk.len() > cap {
            return Err(BodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
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

    /// A reqwest error found further down the chain keeps its URL, so its
    /// own line is left out while the causes beneath it still show.
    #[tokio::test]
    async fn a_nested_request_error_never_prints_its_url() {
        #[derive(Debug)]
        struct Outer(reqwest::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("outer")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let nested = Client::new()
            .get(format!("http://{address}/?apikey=MARKERKEY7f3a91"))
            .send()
            .await
            .unwrap_err();
        assert!(nested.to_string().contains("MARKERKEY7f3a91"));

        let mut text = String::from("outer");
        append_causes(&Outer(nested), &mut text);
        assert!(!text.contains("MARKERKEY7f3a91"), "{text}");
        assert!(text.contains("client error"), "{text}");
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
