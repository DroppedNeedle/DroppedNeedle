//! The client address behind trusted proxies.
//!
//! The TCP peer is the client unless it is a trusted proxy
//! (`TRUSTED_PROXY_IPS`); then `X-Forwarded-For` is walked from the right
//! and the first untrusted hop wins, or the leftmost entry when every hop
//! is trusted. These are uvicorn's `ProxyHeadersMiddleware` rules, which
//! v2 ran behind. Rate limits and lockouts key on this value and never on
//! a raw header, so an untrusted client cannot pick its own bucket.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

use crate::auth::session::middleware::TrustedProxies;

/// A resolved client address, stashed in request extensions by the layer
/// that resolved it so later handlers key on the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

/// The client address for one request from `peer`.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &TrustedProxies) -> IpAddr {
    let peer_ip = peer.ip().to_canonical();
    if !trusted.is_trusted(Some(peer)) {
        return peer_ip;
    }
    let mut hops = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        let Ok(text) = value.to_str() else {
            return peer_ip;
        };
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            match parse_hop(entry) {
                Some(ip) => hops.push(ip),
                // One unreadable hop makes the chain untrustworthy.
                None => return peer_ip,
            }
        }
    }
    let untrusted = hops
        .iter()
        .rev()
        .find(|ip| !trusted.is_trusted(Some(SocketAddr::new(**ip, 0))));
    untrusted.or(hops.first()).copied().unwrap_or(peer_ip)
}

/// One `X-Forwarded-For` entry: a bare IP, or an IP with a port.
fn parse_hop(entry: &str) -> Option<IpAddr> {
    entry
        .parse::<IpAddr>()
        .or_else(|_| entry.parse::<SocketAddr>().map(|addr| addr.ip()))
        .ok()
        .map(|ip| ip.to_canonical())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(peer: &str, forwarded: &[&str]) -> String {
        let mut headers = HeaderMap::new();
        for value in forwarded {
            headers.append("x-forwarded-for", value.parse().unwrap());
        }
        let trusted = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8").unwrap();
        client_ip(peer.parse().unwrap(), &headers, &trusted).to_string()
    }

    #[test]
    fn forwarded_for_counts_only_behind_a_trusted_peer() {
        assert_eq!(resolve("203.0.113.5:1", &["198.51.100.1"]), "203.0.113.5");
        assert_eq!(resolve("127.0.0.1:1", &[]), "127.0.0.1");
        assert_eq!(resolve("127.0.0.1:1", &["198.51.100.1"]), "198.51.100.1");
        assert_eq!(
            resolve("10.0.0.2:1", &["198.51.100.9, 198.51.100.1", "10.0.0.7"]),
            "198.51.100.1"
        );
        assert_eq!(resolve("10.0.0.2:1", &["10.0.0.9, 10.0.0.7"]), "10.0.0.9");
        assert_eq!(
            resolve("127.0.0.1:1", &["[2001:db8::1]:443"]),
            "2001:db8::1"
        );
        assert_eq!(resolve("127.0.0.1:1", &["junk"]), "127.0.0.1");
        assert_eq!(
            resolve("[::ffff:127.0.0.1]:1", &["198.51.100.1"]),
            "198.51.100.1"
        );
    }
}
