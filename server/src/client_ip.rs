//! The client address behind trusted proxies.
//!
//! The TCP peer is the client unless it is a trusted proxy
//! (`TRUSTED_PROXY_IPS`); then `X-Forwarded-For` is walked from the right
//! and the first untrusted hop wins, or the leftmost entry when every hop
//! is trusted. These are uvicorn's `ProxyHeadersMiddleware` rules, which
//! v2 ran behind, plus a stop at the first unreadable hop. Rate limits and
//! lockouts key on this value and never on a raw header, so a client
//! cannot pick its own bucket.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

use crate::auth::session::middleware::TrustedProxies;

/// A resolved client address, stashed in request extensions by the layer
/// that resolved it so later handlers key on the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

/// The client address for one request from `peer`, the one place the
/// server reads `X-Forwarded-For`.
///
/// Hops are walked from the right, starting next to the trusted peer. The
/// first untrusted hop is the client. An unreadable hop stops the walk and
/// the nearest readable hop to its right wins (the peer when there is
/// none): anything left of it was written by whoever wrote the junk, so a
/// client cannot steer its bucket by adding garbage. When every hop is
/// trusted, the leftmost one wins.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &TrustedProxies) -> IpAddr {
    let peer_ip = peer.ip().to_canonical();
    if !trusted.is_trusted(Some(peer)) {
        return peer_ip;
    }
    let mut hops: Vec<Option<IpAddr>> = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        match value.to_str() {
            Ok(text) => hops.extend(
                text.split(',')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(parse_hop),
            ),
            Err(_) => hops.push(None),
        }
    }
    let mut nearest = peer_ip;
    for hop in hops.iter().rev() {
        let Some(ip) = *hop else {
            return nearest;
        };
        if !trusted.is_trusted(Some(SocketAddr::new(ip, 0))) {
            return ip;
        }
        nearest = ip;
    }
    nearest
}

/// One `X-Forwarded-For` entry: a bare IP, a bracketed IPv6, or either
/// with a port.
fn parse_hop(entry: &str) -> Option<IpAddr> {
    let bare = entry
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(entry);
    bare.parse::<IpAddr>()
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
        assert_eq!(resolve("127.0.0.1:1", &["[2001:db8::2]"]), "2001:db8::2");
        assert_eq!(resolve("127.0.0.1:1", &["junk"]), "127.0.0.1");
        // Junk left of a real hop cannot move the bucket off that hop.
        assert_eq!(
            resolve("127.0.0.1:1", &["junk, 198.51.100.1"]),
            "198.51.100.1"
        );
        assert_eq!(
            resolve("10.0.0.2:1", &["198.51.100.9, junk, 10.0.0.7"]),
            "10.0.0.7"
        );
        assert_eq!(
            resolve("[::ffff:127.0.0.1]:1", &["198.51.100.1"]),
            "198.51.100.1"
        );
    }
}
