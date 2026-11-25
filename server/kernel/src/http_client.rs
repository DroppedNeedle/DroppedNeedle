//! The single factory for outbound HTTP clients.
//!
//! The factory owns timeouts, pool behavior, and the DroppedNeedle
//! User-Agent. It builds its clients once at boot: the shared one follows
//! redirects, the [`no_redirect`](HttpClientFactory::no_redirect) one never
//! does, for callers that validate every hop themselves or must not be
//! bounced to another host, the
//! [`same_host_redirects`](HttpClientFactory::same_host_redirects) one
//! follows only a hop that stays on the first URL's host, for services
//! that take a key in a header, and the
//! [`direct_no_redirect`](HttpClientFactory::direct_no_redirect) one also
//! skips any proxy and has no total timeout, for plugin audio streams.
//! The [`lan_probe`](HttpClientFactory::lan_probe) one reaches only
//! private and loopback addresses, for the setup guide's look at the
//! services on the user's own network.
//! Provider rates live in the providers crate's rate table, and every
//! provider in that crate sends through its executor, which paces,
//! retries and cools down.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::{
    Client, Url,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect::{Attempt, Policy},
};
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
    /// an application name, version and contact address. musicbrainz.org's
    /// edge drops connections whose User-Agent starts with `DroppedNeedle/`,
    /// so the name is `DroppedNeedleApp`. The version keeps v2's leading `v`,
    /// the form MetaBrainz's gateway matches (their reply, 2026-11-02).
    pub fn user_agent(&self) -> String {
        format!(
            "DroppedNeedleApp/v{} ({}; https://www.droppedneedle.com)",
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
    same_host: Client,
    remote_audio: Client,
    direct: Client,
    lan_probe: Client,
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
            same_host: builder()
                .redirect(Policy::custom(|attempt| {
                    if same_host_hop(&attempt) {
                        attempt.follow()
                    } else {
                        attempt.stop()
                    }
                }))
                .build()?,
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
            // The setup guide asks fixed names on the user's network, and a
            // name the network's DNS passes upstream must not reach the
            // internet or a cloud metadata address.
            lan_probe: Client::builder()
                .user_agent(settings.user_agent())
                .timeout(LAN_PROBE_TIMEOUT)
                .connect_timeout(LAN_PROBE_TIMEOUT)
                .redirect(Policy::none())
                .no_proxy()
                .dns_resolver(Arc::new(LanResolver))
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

    /// The same client following a redirect only while it stays on the
    /// host the request was sent to (`same_host_hop`): a key sent in
    /// a header goes to no other address. Any other 3xx comes back as the
    /// response.
    pub fn same_host_redirects(&self) -> &Client {
        &self.same_host
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

    /// For the setup guide's keyless look at the services on the user's
    /// network: a name resolves only to the addresses [`keep_lan_address`]
    /// keeps, with no proxy (so the connection goes to those addresses), no
    /// redirects, no cookies and [`LAN_PROBE_TIMEOUT`] for the whole
    /// request. A URL naming an address instead of a host is not resolved,
    /// so its caller checks the address itself.
    pub fn lan_probe(&self) -> &Client {
        &self.lan_probe
    }
}

/// The whole of one [`HttpClientFactory::lan_probe`] request: lookup,
/// connect and body.
pub const LAN_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// One name lookup for [`HttpClientFactory::lan_probe`].
const LAN_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);

/// Whether the setup guide's client may connect to `address`: private
/// (10/8, 172.16/12, 192.168/16), carrier-grade NAT (100.64/10), loopback
/// and unique local (fc00::/7) addresses, IPv4 ones written as IPv6
/// included. Everything else is refused, the cloud metadata addresses
/// inside those ranges among them.
pub fn keep_lan_address(address: IpAddr) -> bool {
    const METADATA_V4: Ipv4Addr = Ipv4Addr::new(100, 100, 100, 200);
    const METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
    match address.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4 != METADATA_V4
                && (a == 10
                    || a == 127
                    || (a == 172 && (16..32).contains(&b))
                    || (a == 192 && b == 168)
                    || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            v6 != METADATA_V6
                && (v6 == Ipv6Addr::LOCALHOST || (v6.segments()[0] & 0xfe00) == 0xfc00)
        }
    }
}

/// Why [`HttpClientFactory::lan_probe`]'s lookup gave no address.
#[derive(Debug, Error)]
#[error("the name has no private address")]
pub struct LanLookupError;

/// What a [`HttpClientFactory::lan_probe`] request's failure means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanProbeFailure {
    /// The name does not resolve, or only to addresses it may not reach.
    NotFound,
    /// Nothing answered in time, or the connection was refused or broke.
    NoAnswer,
    /// Anything else.
    Other,
}

/// Read a [`HttpClientFactory::lan_probe`] request's error.
pub fn lan_probe_failure(error: &reqwest::Error) -> LanProbeFailure {
    let first: &(dyn std::error::Error + 'static) = error;
    let mut causes = std::iter::successors(Some(first), |inner| inner.source());
    if causes.any(|inner| inner.is::<LanLookupError>()) {
        return LanProbeFailure::NotFound;
    }
    if error.is_timeout() || error.is_connect() || error.is_body() || error.is_request() {
        LanProbeFailure::NoAnswer
    } else {
        LanProbeFailure::Other
    }
}

/// Looks a name up as given, with a trailing dot so no search domain is
/// added, and keeps only the addresses [`keep_lan_address`] keeps.
struct LanResolver;

impl Resolve for LanResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_owned();
        Box::pin(async move {
            let lookup = tokio::net::lookup_host((format!("{host}."), 0));
            let found: Vec<SocketAddr> =
                match tokio::time::timeout(LAN_LOOKUP_TIMEOUT, lookup).await {
                    Ok(Ok(addresses)) => addresses
                        .filter(|address| keep_lan_address(address.ip()))
                        .collect(),
                    _ => Vec::new(),
                };
            if found.is_empty() {
                return Err(Box::new(LanLookupError) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(found.into_iter()) as Addrs)
        })
    }
}

/// Redirects [`HttpClientFactory::same_host_redirects`] follows at most.
const SAME_HOST_HOPS: usize = 3;

/// Whether a redirect stays where the request was sent, as a Newznab
/// indexer's API redirects must (`providers::newznab`): the first URL's
/// host, no credentials, never from https to http, and the same port as
/// [`same_port`] reads it. At most [`SAME_HOST_HOPS`] are followed.
fn same_host_hop(attempt: &Attempt<'_>) -> bool {
    let to = attempt.url();
    let previous = attempt.previous();
    let Some(first) = previous.first() else {
        return false;
    };
    if previous.len() > SAME_HOST_HOPS
        || !to.username().is_empty()
        || to.password().is_some()
        || to.host_str() != first.host_str()
    {
        return false;
    }
    let downgrade = to.scheme() == "http" && previous.iter().any(|url| url.scheme() == "https");
    !downgrade && same_port(first, to)
}

/// The same scheme on the same port, or http moving to https on the port
/// it named (Plex's `http://host:32400` to `https://host:32400`) or from
/// its default port to https's own.
fn same_port(first: &Url, to: &Url) -> bool {
    match (first.scheme(), to.scheme()) {
        ("http", "http") | ("https", "https") => {
            first.port_or_known_default() == to.port_or_known_default()
        }
        ("http", "https") => first.port() == to.port(),
        _ => false,
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

/// Why a body could not be written to a file.
pub enum FileBodyError {
    /// The body is larger than the cap it was read under.
    TooLarge,
    /// The connection broke part-way. Never holds the request URL.
    Transport(String),
    /// The file could not be written.
    Write(std::io::Error),
}

/// Write a body to `file` as it arrives, giving up as soon as it passes
/// `cap` bytes, whether or not the server declared its length: for an
/// answer too large to hold in memory. Answers the bytes written. What was
/// written before a failure stays in the file for the caller to remove.
pub async fn read_capped_to(
    mut response: reqwest::Response,
    cap: u64,
    file: &mut tokio::fs::File,
) -> Result<u64, FileBodyError> {
    use tokio::io::AsyncWriteExt as _;
    if response.content_length().is_some_and(|length| length > cap) {
        return Err(FileBodyError::TooLarge);
    }
    let mut written: u64 = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| FileBodyError::Transport(transport_error(error)))?
    {
        written = written.saturating_add(chunk.len() as u64);
        if written > cap {
            return Err(FileBodyError::TooLarge);
        }
        file.write_all(&chunk).await.map_err(FileBodyError::Write)?;
    }
    file.flush().await.map_err(FileBodyError::Write)?;
    Ok(written)
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

    /// A same-host redirect keeps its port, except that http may move to
    /// https on the port it named or from its default port to https's own.
    #[test]
    fn a_same_host_hop_keeps_its_port() {
        let hop =
            |from: &str, to: &str| same_port(&Url::parse(from).unwrap(), &Url::parse(to).unwrap());
        assert!(hop("http://h:32400/", "https://h:32400/"));
        assert!(hop("http://h/", "https://h/"));
        assert!(!hop("http://h:32400/", "https://h:8443/"));
        assert!(!hop("http://h:32400/", "https://h/"));
        assert!(!hop("http://h/", "https://h:8443/"));
        assert!(!hop("http://h:8080/", "http://h:8081/"));
        assert!(!hop("https://h/", "http://h/"));
        assert!(!hop("https://h:32400/", "http://h:32400/"));
    }

    /// The setup guide's client reaches private, unique local and loopback
    /// addresses only, however they are written.
    #[test]
    fn the_lan_probe_keeps_only_private_and_loopback_addresses() {
        let cases = [
            ("0.0.0.0", false),
            ("::", false),
            ("169.254.169.254", false),
            ("fe80::1", false),
            ("::ffff:169.254.169.254", false),
            ("::ffff:10.0.0.1", true),
            ("64:ff9b::1", false),
            ("ff02::1", false),
            ("100.100.100.200", false),
            ("fd00:ec2::254", false),
            ("172.17.0.2", true),
            ("fd12::1", true),
            ("::1", true),
            ("127.0.0.1", true),
            ("192.168.1.20", true),
            ("100.64.0.1", true),
            ("172.32.0.1", false),
            ("8.8.8.8", false),
            ("2606:4700::1111", false),
        ];
        for (address, kept) in cases {
            let ip: IpAddr = address.parse().unwrap();
            assert_eq!(keep_lan_address(ip), kept, "{address}");
        }
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
        assert!(head.contains("user-agent: droppedneedleapp/v"));
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
