//! Which OIDC provider URLs the server may talk to.
//!
//! The rule, stated once: every provider URL (the issuer and each endpoint
//! its discovery document names) must be `https`. Plain `http` is allowed
//! only when the host is this machine or a private network: loopback,
//! RFC 1918 and carrier-grade NAT ranges (Tailscale lives there), IPv4 and
//! IPv6 link-local, and IPv6 unique-local addresses. A host name over
//! `http` must resolve to such addresses only. Anything else is refused
//! with a message the admin can act on, because a login token, a client
//! secret and the user's identity would cross the internet in clear.

use std::net::{IpAddr, Ipv4Addr};

/// Why a provider URL is refused. The message is shown to admins.
pub fn refusal(url: &str) -> String {
    format!(
        "OIDC provider URL {url} uses plain http on a public address. Use https, or keep the \
         provider on this machine or your local network"
    )
}

/// True for the addresses plain `http` is allowed on.
pub fn is_local_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local_v4(v4);
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link local fe80::/10
        }
    }
}

fn is_local_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || (a == 100 && (64..128).contains(&b)) // 100.64.0.0/10
}

/// The host of a parsed URL as written: an IP literal, or a name.
enum Host {
    Ip(IpAddr),
    Name(String),
}

fn host_of(parsed: &reqwest::Url) -> Option<Host> {
    let raw = parsed.host_str()?;
    let bare = raw.trim_start_matches('[').trim_end_matches(']');
    Some(match bare.parse::<IpAddr>() {
        Ok(ip) => Host::Ip(ip),
        Err(_) => Host::Name(bare.to_owned()),
    })
}

/// Decide whether the server may call `url`, resolving host names served
/// over plain http.
pub async fn check_provider_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("{url} is not a valid URL"))?;
    match parsed.scheme() {
        "https" => return Ok(()),
        "http" => {}
        _ => return Err(format!("{url} must be an https URL")),
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let local = match host_of(&parsed) {
        Some(Host::Ip(ip)) => is_local_address(ip),
        Some(Host::Name(name)) if name.eq_ignore_ascii_case("localhost") => true,
        Some(Host::Name(name)) => match tokio::net::lookup_host((name.as_str(), port)).await {
            Ok(addresses) => {
                let addresses: Vec<_> = addresses.collect();
                !addresses.is_empty()
                    && addresses
                        .iter()
                        .all(|address| is_local_address(address.ip()))
            }
            Err(_) => return Err(format!("Cannot resolve the OIDC provider host in {url}")),
        },
        None => false,
    };
    if local { Ok(()) } else { Err(refusal(url)) }
}

/// A quick check without DNS, for the boot-time warning: true when `url`
/// is plain http and its host is not obviously local. Host names other
/// than `localhost` count as not obviously local; the login resolves them.
pub fn looks_public_http(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "http" {
        return false;
    }
    match host_of(&parsed) {
        Some(Host::Ip(ip)) => !is_local_address(ip),
        Some(Host::Name(name)) => !name.eq_ignore_ascii_case("localhost"),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plain_http_is_allowed_only_on_local_addresses() {
        for url in [
            "https://idp.example.com",
            "https://8.8.8.8",
            "http://127.0.0.1:9000",
            "http://localhost:8080/realms/x",
            "http://192.168.1.20",
            "http://10.0.0.5",
            "http://100.100.1.1",
            "http://[::1]:8080",
            "http://[fd00::1]",
        ] {
            assert_eq!(check_provider_url(url).await, Ok(()), "{url}");
        }
        for url in [
            "http://8.8.8.8",
            "http://[2001:4860::8888]",
            "ftp://10.0.0.1",
        ] {
            assert!(check_provider_url(url).await.is_err(), "{url}");
        }
        assert!(looks_public_http("http://auth.example.com"));
        assert!(!looks_public_http("http://192.168.1.2"));
        assert!(!looks_public_http("https://auth.example.com"));
    }
}
