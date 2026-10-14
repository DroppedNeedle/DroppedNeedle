//! Deny-by-default session middleware over `/api/v3/*`.
//!
//! Order of checks per request: scope (non-v3 passes through; the layer mounts
//! on the v3 router so this is defence in depth) -> public allowlist -> credential
//! extraction (Bearer-then-cookie) -> session lookup (unknown/revoked/expired
//! -> 401 + `WWW-Authenticate: Bearer`; each such failure also takes a token
//! from the caller's auth-failure bucket, and an empty bucket answers 429)
//! -> origin check on cookie mutations
//! against the effective origin (forwarded host and scheme only from trusted
//! proxies) and the addresses saved in the `public_address` setting (-> 403).
//! Success stashes [`CurrentSession`] in the request
//! extensions for handlers and role extractors; role gating itself is
//! handler-level, as is the per-resource 403-or-404 choice.
//!
//! ## Layer order (wiring)
//!
//! In axum the last `.layer()` call is outermost. The v3 router layers, from
//! first call to last: `rate_limit` (inner, closest to handlers), then
//! `require_session`, then debug-only CORS (outer). Per request that runs
//! CORS -> session -> rate limit -> handler, so the limiter sees the
//! [`CurrentSession`] this gate stashes and keys by user. Public paths reach
//! the limiter without one and are keyed by client address. The
//! request-scope middleware wraps the whole app outside all of these. Compat
//! routers mount outside the session layer with their own app-password
//! auth; no native token is ever accepted on compat paths and no app
//! password on native paths.
//!
//! ## Trusted proxies
//!
//! [`TrustedProxies`] names the reverse proxies allowed to set
//! `X-Forwarded-*` (v2 `ProxyHeadersMiddleware` semantics): the
//! effective origin honors `X-Forwarded-Host` and `X-Forwarded-Proto` only
//! when the TCP peer is trusted, and the login routes mark `Secure` from
//! `X-Forwarded-Proto` under the same verdict. Anything else fails closed to
//! the direct `Host` header and scheme. The peer comes from axum
//! `ConnectInfo`, so the server must serve with connect info for
//! non-loopback peers to ever verify.
//!
//! A reverse proxy nobody listed is the usual way this goes wrong: every
//! client then shares the proxy's address for rate limits and HTTPS goes
//! unseen, so the session cookie loses `Secure`. The gate logs one warning
//! per process when it sees the signs (see [`warn_unlisted_proxy`]).
//!
//! ## Stated addresses
//!
//! A proxy that cannot be fixed leaves the effective origin wrong for good.
//! The `public_address` setting is the way out: the gate reads its list for
//! each cookie-authenticated write and hands it to the origin check, which
//! compares it with the browser's `Origin` or `Referer` only. A list that
//! cannot be read counts as empty, which is the strict check, and the log
//! says so once ([`StatedAddresses`]). The sign-in routes read the same
//! list for one thing: a sign-in from a stated `https` address gets a
//! `Secure` cookie (`cookies::secure_for_sign_in`). How a request arrived,
//! and so HSTS, never depends on it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};
use thiserror::Error;

use super::{
    allowlist::is_public,
    cookies::is_secure_trusted,
    extract::{Transport, forbidden_response, unauthorized_response},
    origin::{Origin, OriginDecision, check_origin, is_unsafe_method, named_origin},
    rate_limit::{AUTH_FAILURE_CLASS, RateLimiter, rate_limited_response},
    stated::StatedAddresses,
    store::{SessionKind, SessionStore, SessionStoreError, now_unix},
    tokens::hash_token,
};

/// Authenticated session stashed in request extensions by the middleware.
#[derive(Debug, Clone)]
pub struct CurrentSession {
    /// Owning user id.
    pub user_id: String,
    /// Session row id (for the session-list `current` marker).
    pub session_id: String,
    /// Standard or companion.
    pub kind: SessionKind,
    /// Transport that carried the credential (origin-check audit).
    pub transport: Transport,
}

/// Proxies trusted to set `X-Forwarded-*` headers.
///
/// The list holds IPs and CIDRs, comma-separated. `*` is an explicit
/// operator opt-in that trusts every peer and every forwarded hop, so
/// [`crate::client_ip::client_ip`] takes the leftmost readable
/// `X-Forwarded-For` entry as the client (the v2 spelling with the same
/// warning: only behind infrastructure that strips spoofed headers).
/// Default is loopback, the v2 default.
#[derive(Debug, Clone)]
pub struct TrustedProxies {
    trust_all: bool,
    hosts: Vec<IpAddr>,
    networks: Vec<Subnet>,
}

/// One bad entry in a trusted-proxy list.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid trusted proxy entry {0:?}: expected an IP, a CIDR, or \"*\"")]
pub struct InvalidProxyEntry(pub String);

/// A parsed CIDR subnet, stored as base address plus prefix length.
#[derive(Debug, Clone, Copy)]
enum Subnet {
    /// IPv4 base plus prefix length (0-32).
    V4 { base: u32, prefix: u8 },
    /// IPv6 base plus prefix length (0-128).
    V6 { base: u128, prefix: u8 },
}

impl TrustedProxies {
    /// Loopback only (`127.0.0.1`, `::1`): direct-serve and same-host-proxy
    /// deploys need nothing else.
    pub fn loopback() -> Self {
        Self {
            trust_all: false,
            hosts: vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
            networks: Vec::new(),
        }
    }

    /// Parse a comma-separated list of IPs and CIDRs, or `*` for all peers.
    /// Blank entries are skipped; any other invalid entry fails the list.
    pub fn parse(list: &str) -> Result<Self, InvalidProxyEntry> {
        let mut proxies = Self {
            trust_all: false,
            hosts: Vec::new(),
            networks: Vec::new(),
        };
        for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if entry == "*" {
                proxies.trust_all = true;
                continue;
            }
            if let Some((addr, prefix)) = entry.split_once('/') {
                let subnet = parse_subnet(addr.trim(), prefix.trim())
                    .map_err(|()| InvalidProxyEntry(entry.to_owned()))?;
                proxies.networks.push(subnet);
            } else {
                let host: IpAddr = entry
                    .parse()
                    .map_err(|_| InvalidProxyEntry(entry.to_owned()))?;
                proxies.hosts.push(host);
            }
        }
        Ok(proxies)
    }

    /// True when forwarded headers from `peer` may be honored. An unknown
    /// peer (no `ConnectInfo`, e.g. in-process tests) is untrusted unless the
    /// list trusts all peers.
    pub fn is_trusted(&self, peer: Option<SocketAddr>) -> bool {
        match peer {
            None => self.trust_all,
            Some(addr) => self.trust_all || self.matches(addr.ip()),
        }
    }

    /// True when the peer IP is listed or inside a listed subnet. IPv4-mapped
    /// IPv6 peers compare as their IPv4 form so dual-stack loopback verifies.
    fn matches(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(mapped) => mapped.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            IpAddr::V4(_) => ip,
        };
        self.hosts.contains(&ip) || self.networks.iter().any(|net| net.contains(ip))
    }
}

impl Default for TrustedProxies {
    fn default() -> Self {
        Self::loopback()
    }
}

impl Subnet {
    /// True when the address falls inside this subnet (same family only).
    fn contains(self, ip: IpAddr) -> bool {
        match (self, ip) {
            (Subnet::V4 { base, prefix }, IpAddr::V4(addr)) => {
                let mask = prefix_mask_u32(prefix);
                u32::from(addr) & mask == base & mask
            }
            (Subnet::V6 { base, prefix }, IpAddr::V6(addr)) => {
                let mask = prefix_mask_u128(prefix);
                u128::from(addr) & mask == base & mask
            }
            _ => false,
        }
    }
}

/// Parse one `address/prefix` CIDR entry.
fn parse_subnet(addr: &str, prefix: &str) -> Result<Subnet, ()> {
    let prefix: u8 = prefix.parse().map_err(|_| ())?;
    if let Ok(v4) = addr.parse::<Ipv4Addr>() {
        if prefix > 32 {
            return Err(());
        }
        return Ok(Subnet::V4 {
            base: u32::from(v4),
            prefix,
        });
    }
    if let Ok(v6) = addr.parse::<Ipv6Addr>() {
        if prefix > 128 {
            return Err(());
        }
        return Ok(Subnet::V6 {
            base: u128::from(v6),
            prefix,
        });
    }
    Err(())
}

/// High `prefix` bits set for an IPv4 netmask.
fn prefix_mask_u32(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

/// High `prefix` bits set for an IPv6 netmask.
fn prefix_mask_u128(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

/// A warning worth one log line per process: the first caller to claim it
/// logs, everyone after stays quiet. Clones share the flag.
#[derive(Debug, Clone, Default)]
pub struct WarnOnce(Arc<AtomicBool>);

impl WarnOnce {
    /// True for the first caller only.
    pub(super) fn claim(&self) -> bool {
        !self.0.load(Ordering::Relaxed) && !self.0.swap(true, Ordering::Relaxed)
    }
}

/// Shared middleware state: the session store plus deployment facts.
#[derive(Debug, Clone)]
pub struct SessionAuth<S> {
    /// Token store.
    pub store: S,
    /// Deployment base path (`""` at the domain root). The router strips
    /// it before this layer runs.
    pub base_path: String,
    /// Proxies trusted to set `X-Forwarded-*`; loopback by default (v2
    /// parity). Off-host proxies are set from deployment config at wiring.
    pub trusted_proxies: TrustedProxies,
    /// Shared limiter; failed authentications are charged per client
    /// address, since the request limiter runs after this gate.
    pub limits: Arc<RateLimiter>,
    /// The saved public addresses. None is stated by default.
    stated: StatedAddresses,
    /// Spent by the first refused request that looks like a proxy
    /// misreporting this server's address.
    origin_mismatch_warned: WarnOnce,
    /// Spent by the first refused request from a page on another host.
    other_host_warned: WarnOnce,
    /// Spent by the first forwarded header from an unlisted private peer.
    unlisted_proxy_warned: WarnOnce,
}

impl<S> SessionAuth<S> {
    /// Build the shared state. Trusted proxies default to loopback; use
    /// [`SessionAuth::with_trusted_proxies`] when a proxy fronts the app.
    pub fn new(store: S, base_path: &str) -> Self {
        Self {
            store,
            base_path: base_path.to_owned(),
            trusted_proxies: TrustedProxies::default(),
            limits: Arc::new(RateLimiter::new()),
            stated: StatedAddresses::default(),
            origin_mismatch_warned: WarnOnce::default(),
            other_host_warned: WarnOnce::default(),
            unlisted_proxy_warned: WarnOnce::default(),
        }
    }

    /// Share the app's request limiter (auth-failure budget).
    pub fn with_limits(mut self, limits: Arc<RateLimiter>) -> Self {
        self.limits = limits;
        self
    }

    /// Trust the given proxies for forwarded host/proto. Fed from deployment
    /// config at wiring.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }

    /// Accept a change from the stated public addresses. Without them
    /// only the address the server works out is accepted.
    pub fn with_stated_addresses(mut self, stated: StatedAddresses) -> Self {
        self.stated = stated;
        self
    }
}

/// Deny-by-default session gate. See module docs for the check order.
pub async fn require_session<S>(
    State(auth): State<SessionAuth<S>>,
    request: Request,
    next: Next,
) -> Response
where
    S: SessionStore,
{
    // The router strips the base path before any layer runs, so this is the
    // same base-relative path the routes match on.
    let path = request.uri().path().to_owned();
    if !path.starts_with("/api/v3") {
        return next.run(request).await;
    }
    warn_unlisted_proxy(&auth.unlisted_proxy_warned, &request, &auth.trusted_proxies);
    if is_public(&path) {
        return next.run(request).await;
    }
    let Some((raw_token, transport)) = super::extract::extract(request.headers()) else {
        return auth_failure(&auth.limits, &request, "Not authenticated");
    };
    let record = match auth
        .store
        .lookup_valid(&hash_token(&raw_token), now_unix())
        .await
    {
        Ok(record) => record,
        Err(error) => {
            let error_id = request
                .extensions()
                .get::<crate::ids::RequestId>()
                .map(|id| id.0.clone())
                .unwrap_or_default();
            if matches!(error, SessionStoreError::Busy) {
                return crate::error::busy_response("auth.session.lookup", &error_id);
            }
            tracing::error!(%error, error_id, "session store lookup failed");
            return crate::error::ApiError::internal_response(&error_id);
        }
    };
    let Some(record) = record else {
        return auth_failure(&auth.limits, &request, "Invalid or expired token");
    };
    let target = effective_origin(&request, &auth.trusted_proxies);
    let stated = if transport == Transport::Cookie && is_unsafe_method(request.method()) {
        auth.stated.origins()
    } else {
        Vec::new()
    };
    if check_origin(
        request.headers(),
        target.as_ref(),
        &stated,
        request.method(),
        transport,
    ) == OriginDecision::Deny
    {
        warn_refused_origin(&auth, request.headers(), target.as_ref());
        return forbidden_response("Origin not allowed");
    }
    let mut request = request;
    request.extensions_mut().insert(CurrentSession {
        user_id: record.user_id,
        session_id: record.id,
        kind: record.kind,
        transport,
    });
    next.run(request).await
}

/// 401 for a failed authentication, charged to the caller's address. Once
/// that budget is spent the answer is 429, so junk tokens on protected
/// paths are throttled even though the request limiter never sees them.
fn auth_failure(limits: &RateLimiter, request: &Request, message: &str) -> Response {
    let outcome = limits.check(AUTH_FAILURE_CLASS, &limits.caller_key(request));
    if outcome.allowed {
        unauthorized_response(message)
    } else {
        rate_limited_response(AUTH_FAILURE_CLASS, outcome.retry_after_secs)
    }
}

/// Headers a reverse proxy adds, any of which marks the peer as one.
const FORWARDED_HEADERS: [&str; 4] = [
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-forwarded-host",
    "forwarded",
];

/// Warn, once per process, when a forwarded header arrives from a
/// private-range peer that is not a trusted proxy: that is what a reverse
/// proxy in another container or on another LAN host looks like before it
/// is listed. A public peer sending the header is a client making things
/// up and gets no log line.
fn warn_unlisted_proxy(warned: &WarnOnce, request: &Request, trusted: &TrustedProxies) {
    let Some(peer) = peer_of(request).map(|peer| peer.ip().to_canonical()) else {
        return;
    };
    let private = match peer {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
    };
    if private
        && !trusted.trust_all
        && !trusted.matches(peer)
        && FORWARDED_HEADERS
            .iter()
            .any(|name| request.headers().contains_key(*name))
        && warned.claim()
    {
        tracing::warn!(
            %peer,
            "Ignoring the forwarded headers sent by {peer}, which is not a trusted proxy. \
             If it is your reverse proxy, add it to TRUSTED_PROXY_IPS: until then every \
             client shares its rate limits and HTTPS goes undetected."
        );
    }
}

/// Which of the two one-time warnings a refused change may claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefusalHint {
    /// The page is on this server's host with another scheme or port: the
    /// proxy drops the port or hides HTTPS.
    SchemeOrPort,
    /// The page is on another host: the proxy replaces `Host`. A real
    /// cross-site attempt looks the same.
    OtherHost,
}

/// The longest host a warning writes out: the longest DNS name.
const LONGEST_LOGGED_HOST: usize = 253;

/// The warning a refused change may claim, with the origin it named.
/// None when the request carried `Sec-Fetch-Site`: a value that did not
/// pass is the browser saying the page is not the address it sent the
/// request to, so no proxy stands between the two, and the one line is
/// kept for a refusal a proxy can explain. None for a host too long to be
/// a name, which is not worth a log line.
fn refusal_hint(
    headers: &axum::http::HeaderMap,
    target: Option<&Origin>,
) -> Option<(RefusalHint, Origin)> {
    if headers.contains_key("sec-fetch-site") {
        return None;
    }
    let (target, source) = (target?, named_origin(headers)?);
    if [target, &source]
        .iter()
        .any(|origin| origin.host().len() > LONGEST_LOGGED_HOST)
    {
        return None;
    }
    let hint = if source.same_host(target) {
        RefusalHint::SchemeOrPort
    } else {
        RefusalHint::OtherHost
    };
    Some((hint, source))
}

/// Log a refused change, once per process for each [`RefusalHint`], so a
/// proxy that misreports this server's address shows up as more than 403s
/// in the browser. The line names the parsed origin, never the header's
/// own text.
fn warn_refused_origin<S>(
    auth: &SessionAuth<S>,
    headers: &axum::http::HeaderMap,
    target: Option<&Origin>,
) {
    let (Some(target), Some((hint, source))) = (target, refusal_hint(headers, target)) else {
        return;
    };
    let (warned, fault) = match hint {
        RefusalHint::SchemeOrPort => (
            &auth.origin_mismatch_warned,
            "is dropping the port or hiding HTTPS",
        ),
        RefusalHint::OtherHost => (&auth.other_host_warned, "is replacing the Host header"),
    };
    if warned.claim() {
        tracing::warn!(
            %source,
            %target,
            "Refused a change from a page at {source}: this server sees itself at {target}. \
             If that page is DroppedNeedle, the reverse proxy {fault}. Fix the proxy (see the \
             reverse proxy paragraph under Configuration in the README), or state the address \
             under Settings > Security > Public address or with `droppedneedle-tool \
             public-address`."
        );
    }
}

/// The origin the browser addressed this request to, for the origin check:
/// [`effective_host`] plus the scheme, which is `https` on direct TLS or on
/// `X-Forwarded-Proto: https` from a trusted proxy. A host without a port
/// is on the scheme's default port, so a proxy on any other port has to
/// pass the browser's `Host` through unchanged.
pub fn effective_origin(request: &Request, trusted: &TrustedProxies) -> Option<Origin> {
    Origin::from_host(
        if arrived_over_https(request, trusted) {
            "https"
        } else {
            "http"
        },
        &effective_host(request, trusted),
    )
}

/// True when the browser reached this request over HTTPS: direct TLS, or
/// `X-Forwarded-Proto: https` set by a trusted proxy. The origin check and
/// the HSTS header both decide by this.
pub fn arrived_over_https(request: &Request, trusted: &TrustedProxies) -> bool {
    is_secure_trusted(
        request.uri().scheme_str().unwrap_or("http"),
        request.headers(),
        trusted.is_trusted(peer_of(request)),
    )
}

/// The TCP peer, when the server runs with connect info.
fn peer_of(request: &Request) -> Option<SocketAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0)
}

/// Effective request host for the origin check. From a trusted proxy the
/// first `X-Forwarded-Host` entry wins; otherwise the direct `Host` header,
/// else the URI host. Unknown or untrusted peers fail closed to the direct
/// host, so a spoofed forwarded host can never satisfy the check.
pub fn effective_host(request: &Request, trusted: &TrustedProxies) -> String {
    if trusted.is_trusted(peer_of(request)) {
        let forwarded = request
            .headers()
            .get("x-forwarded-host")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(',').next().unwrap_or("").trim())
            .filter(|host| !host.is_empty());
        if let Some(host) = forwarded {
            return host.to_owned();
        }
    }
    if let Some(host) = request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        return host.to_owned();
    }
    request.uri().host().unwrap_or("").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(addr: &str) -> Option<SocketAddr> {
        Some(addr.parse().unwrap())
    }

    #[test]
    fn proxy_list_parses_ips_cidrs_and_star() {
        let list = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8, ::1").unwrap();
        assert!(list.is_trusted(peer("127.0.0.1:1")));
        assert!(list.is_trusted(peer("10.9.8.7:1")));
        assert!(!list.is_trusted(peer("11.0.0.1:1")));
        assert!(list.is_trusted(peer("[::1]:1")));
        assert!(!list.is_trusted(None));

        let all = TrustedProxies::parse("*").unwrap();
        assert!(all.is_trusted(None));
        assert!(all.is_trusted(peer("203.0.113.9:1")));

        assert!(TrustedProxies::parse("nope").is_err());
        assert!(TrustedProxies::parse("10.0.0.0/33").is_err());
        assert!(!TrustedProxies::parse(",,  ").unwrap().is_trusted(None));

        let loopback = TrustedProxies::default();
        assert!(loopback.is_trusted(peer("127.0.0.1:9")));
        assert!(!loopback.is_trusted(peer("192.168.1.2:9")));
        assert!(loopback.is_trusted(peer("[::ffff:127.0.0.1]:9")));
    }

    #[test]
    fn a_refused_change_claims_a_warning_only_when_the_proxy_may_be_at_fault() {
        fn hint_for(target: &str, sent: &[(&str, &str)]) -> Option<RefusalHint> {
            let mut headers = axum::http::HeaderMap::new();
            for (name, value) in sent {
                headers.insert(
                    axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    value.parse().unwrap(),
                );
            }
            let target = Origin::from_host("http", target);
            assert!(target.is_some());
            refusal_hint(&headers, target.as_ref()).map(|(hint, _)| hint)
        }
        let hint = |sent: &[(&str, &str)]| hint_for("music.test", sent);
        let port_dropped = ("origin", "http://music.test:8080");
        let https_hidden = ("referer", "https://music.test/albums");
        let other_host = ("origin", "https://music.example");
        assert_eq!(hint(&[port_dropped]), Some(RefusalHint::SchemeOrPort));
        assert_eq!(hint(&[https_hidden]), Some(RefusalHint::SchemeOrPort));
        assert_eq!(hint(&[other_host]), Some(RefusalHint::OtherHost));
        assert_eq!(hint(&[]), None);
        assert_eq!(hint(&[("origin", "null")]), None);

        // The browser has said the page is not the address it sent the
        // request to, so the proxy is not what stands between them.
        for site in ["cross-site", "same-site", "none", ""] {
            let said = ("sec-fetch-site", site);
            assert_eq!(hint(&[said, port_dropped]), None, "{site:?}");
            assert_eq!(hint(&[said, other_host]), None, "{site:?}");
        }

        // No host longer than a DNS name is written to the log.
        let long = "a".repeat(300);
        assert_eq!(hint(&[("origin", &format!("https://{long}"))]), None);
        assert_eq!(hint(&[("origin", &format!("http://{long}:8080"))]), None);
        assert_eq!(hint_for(&long, &[port_dropped]), None);
        let longest_name = "a".repeat(253);
        assert_eq!(
            hint(&[("origin", &format!("https://{longest_name}"))]),
            Some(RefusalHint::OtherHost)
        );
    }

    #[test]
    fn effective_host_honors_forwarded_host_only_from_trusted_peers() {
        fn request(peer: Option<SocketAddr>, forwarded: Option<&str>) -> Request {
            let mut request = Request::builder()
                .uri("/api/v3/me")
                .header("host", "direct.test")
                .body(axum::body::Body::empty())
                .unwrap();
            if let Some(host) = forwarded {
                request
                    .headers_mut()
                    .insert("x-forwarded-host", host.parse().unwrap());
            }
            if let Some(addr) = peer {
                request.extensions_mut().insert(ConnectInfo(addr));
            }
            request
        }

        let trusted = TrustedProxies::default();
        let loopback: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let remote: SocketAddr = "203.0.113.9:1".parse().unwrap();
        assert_eq!(
            effective_host(&request(Some(loopback), Some("proxy.test")), &trusted),
            "proxy.test"
        );
        assert_eq!(
            effective_host(
                &request(Some(loopback), Some("proxy.test, other.test")),
                &trusted
            ),
            "proxy.test"
        );
        assert_eq!(
            effective_host(&request(Some(remote), Some("proxy.test")), &trusted),
            "direct.test"
        );
        assert_eq!(
            effective_host(&request(None, Some("proxy.test")), &trusted),
            "direct.test"
        );
        assert_eq!(
            effective_host(&request(Some(loopback), None), &trusted),
            "direct.test"
        );
    }

    #[test]
    fn effective_origin_takes_scheme_and_host_only_from_trusted_peers() {
        fn request(peer: &str) -> Request {
            let mut request = Request::builder()
                .uri("/api/v3/me")
                .header("host", "app:8688")
                .header("x-forwarded-host", "music.example")
                .header("x-forwarded-proto", "https")
                .body(axum::body::Body::empty())
                .unwrap();
            let peer: SocketAddr = peer.parse().unwrap();
            request.extensions_mut().insert(ConnectInfo(peer));
            request
        }

        let trusted = TrustedProxies::default();
        assert_eq!(
            effective_origin(&request("127.0.0.1:1"), &trusted),
            Origin::from_url("https://music.example")
        );
        assert_eq!(
            effective_origin(&request("203.0.113.9:1"), &trusted),
            Origin::from_url("http://app:8688")
        );
    }
}
