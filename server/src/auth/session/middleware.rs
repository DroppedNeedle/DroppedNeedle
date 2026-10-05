//! Deny-by-default session middleware over `/api/v3/*`.
//!
//! Order of checks per request: scope (non-v3 passes through; the layer mounts
//! on the v3 router so this is defence in depth) -> public allowlist -> credential
//! extraction (Bearer-then-cookie) -> session lookup (unknown/revoked/expired
//! -> 401 + `WWW-Authenticate: Bearer`) -> origin check on cookie mutations
//! against the effective host (forwarded host only from trusted proxies)
//! (-> 403). Success stashes [`CurrentSession`] in the request extensions for
//! handlers and role extractors; role gating itself is handler-level, as is
//! the per-resource 403-or-404 choice.
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
//! effective host honors `X-Forwarded-Host` only when the TCP peer is
//! trusted, and the login routes mark `Secure` from `X-Forwarded-Proto`
//! under the same verdict. Anything else fails closed to the direct `Host`
//! header and scheme. The peer comes from axum `ConnectInfo`, so the server
//! must serve with connect info for non-loopback peers to ever verify.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};
use thiserror::Error;

use super::{
    allowlist::is_public,
    extract::{Transport, forbidden_response, unauthorized_response},
    origin::{OriginDecision, check_origin},
    store::{SessionKind, SessionStore, now_unix},
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
/// The list holds IPs and CIDRs, comma-separated, with `*` trusting every
/// peer (the v2 spelling with the same warning: only behind infrastructure
/// that strips spoofed headers). Default is loopback, the v2 default.
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
}

impl<S> SessionAuth<S> {
    /// Build the shared state. Trusted proxies default to loopback; use
    /// [`SessionAuth::with_trusted_proxies`] when a proxy fronts the app.
    pub fn new(store: S, base_path: &str) -> Self {
        Self {
            store,
            base_path: base_path.to_owned(),
            trusted_proxies: TrustedProxies::default(),
        }
    }

    /// Trust the given proxies for forwarded host/proto. Fed from deployment
    /// config at wiring.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
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
    if is_public(&path) {
        return next.run(request).await;
    }
    let Some((raw_token, transport)) = super::extract::extract(request.headers()) else {
        return unauthorized_response("Not authenticated");
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
            tracing::error!(%error, error_id, "session store lookup failed");
            return crate::error::ApiError::internal_response(&error_id);
        }
    };
    let Some(record) = record else {
        return unauthorized_response("Invalid or expired token");
    };
    if check_origin(
        request.headers(),
        &effective_host(&request, &auth.trusted_proxies),
        request.method(),
        transport,
    ) == OriginDecision::Deny
    {
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

/// Effective request host for the origin check. From a trusted proxy the
/// first `X-Forwarded-Host` entry wins; otherwise the direct `Host` header,
/// else the URI host. Unknown or untrusted peers fail closed to the direct
/// host, so a spoofed forwarded host can never satisfy the check.
pub fn effective_host(request: &Request, trusted: &TrustedProxies) -> String {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    if trusted.is_trusted(peer) {
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
}
