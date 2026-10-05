//! Origin check on cookie-authenticated mutations.
//!
//! v2 relied on `SameSite=Lax` alone; this closes the gap without a CSRF-token
//! round-trip. Rule: a request authenticated via cookie with an unsafe method
//! must present `Origin` (or `Referer` fallback) whose host matches the
//! request host, else 403. Bearer-authenticated requests are exempt (explicit,
//! non-ambient). Safe methods skip the check; missing `Origin` on an unsafe
//! cookie request fails closed (scripted callers use Bearer tokens).
//!
//! Host comparison ignores case and port: proxies and browsers disagree on
//! default-port rendering, and the host is the security-relevant part.
//! Effective host/scheme behind trusted proxies follows
//! `middleware::TrustedProxies` (v2 `ProxyHeadersMiddleware`
//! semantics): forwarded headers are honored only from trusted peers.

use axum::http::{HeaderMap, Method};

use super::extract::Transport;

/// Unsafe methods carrying ambient-credential risk.
pub fn is_unsafe_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Outcome of the origin check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginDecision {
    /// Request may proceed.
    Allow,
    /// Cookie mutation from a foreign or missing origin: answer 403.
    Deny,
}

/// Run the check. `host` is the effective request host (Host header or URI).
pub fn check_origin(
    headers: &HeaderMap,
    host: &str,
    method: &Method,
    transport: Transport,
) -> OriginDecision {
    if transport == Transport::Bearer || !is_unsafe_method(method) {
        return OriginDecision::Allow;
    }
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| referer_origin(headers));
    match origin.and_then(url_host) {
        Some(origin_host) if hosts_match(&origin_host, host) => OriginDecision::Allow,
        _ => OriginDecision::Deny,
    }
}

/// Derive a comparable origin from the `Referer` fallback header.
fn referer_origin(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::REFERER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Host part of an absolute URL (`scheme://host[:port][/...]`), lowercased.
/// Returns `None` for relative or malformed values.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host_port = rest.split('/').next().unwrap_or("");
    let host = host_port.split('@').next_back().unwrap_or("");
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.split(']').next())
        .unwrap_or_else(|| host.split(':').next().unwrap_or(""));
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// Case-insensitive host equality, ignoring ports on both sides.
fn hosts_match(origin_host: &str, request_host: &str) -> bool {
    let request_host = request_host.split('@').next_back().unwrap_or("");
    let request_host = request_host
        .strip_prefix('[')
        .and_then(|h| h.split(']').next())
        .unwrap_or_else(|| request_host.split(':').next().unwrap_or(""));
    !request_host.is_empty() && origin_host.eq_ignore_ascii_case(request_host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn cookie_post_needs_matching_origin_bearer_exempt() {
        let evil = headers(&[("origin", "https://evil.test")]);
        assert_eq!(
            check_origin(&evil, "music.lan:8688", &Method::POST, Transport::Cookie),
            OriginDecision::Deny
        );
        assert_eq!(
            check_origin(&evil, "music.lan:8688", &Method::POST, Transport::Bearer),
            OriginDecision::Allow
        );
        let same = headers(&[("origin", "http://music.lan:8688")]);
        assert_eq!(
            check_origin(&same, "music.lan:8688", &Method::DELETE, Transport::Cookie),
            OriginDecision::Allow
        );
        assert_eq!(
            check_origin(
                &HeaderMap::new(),
                "music.lan",
                &Method::POST,
                Transport::Cookie
            ),
            OriginDecision::Deny
        );
        assert_eq!(
            check_origin(
                &HeaderMap::new(),
                "music.lan",
                &Method::GET,
                Transport::Cookie
            ),
            OriginDecision::Allow
        );
        let referer = headers(&[("referer", "http://music.lan/albums?page=2")]);
        assert_eq!(
            check_origin(&referer, "music.lan", &Method::PUT, Transport::Cookie),
            OriginDecision::Allow
        );
    }
}
