//! The reverse proxies trusted to set `X-Forwarded-*` headers, parsed from
//! the deployment's list of IPs and CIDRs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use thiserror::Error;

/// Proxies trusted to set `X-Forwarded-*` headers.
///
/// The list holds IPs and CIDRs, comma-separated. `*` is an explicit
/// operator opt-in that trusts every peer and every forwarded hop, so
/// the client IP lookup takes the leftmost readable `X-Forwarded-For` entry as the client (the v2 spelling with the same
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
}
