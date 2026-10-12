//! Origin check on cookie-authenticated mutations.
//!
//! v2 relied on `SameSite=Lax` alone; this closes the gap without a CSRF-token
//! round-trip. Rule: a request authenticated via cookie with an unsafe method
//! must come from the page the server itself serves, else 403.
//! Bearer-authenticated requests are exempt (explicit, non-ambient). Safe
//! methods skip the check.
//!
//! `Sec-Fetch-Site: same-origin` settles it: the browser sets that header
//! itself, page script cannot, and it holds whatever a proxy did to the host
//! or scheme on the way in. Browsers leave the header out on plain-HTTP
//! pages (other than localhost) and old ones never send it, so without it
//! the scheme, host and port of `Origin` (or `Referer` when `Origin` is
//! absent) must equal the address the request was sent to. A request with
//! neither header fails closed (scripted callers use Bearer tokens).
//!
//! The port and scheme count because cookies ignore both and `SameSite`
//! ignores the port: another app on the same host, one port over, is
//! same-site and its forms carry this session cookie.
//!
//! The address the request was sent to is built in
//! `middleware::effective_origin`: forwarded headers are honored only from
//! trusted peers. A proxy that drops the port from `Host` or hides HTTPS
//! makes the server's idea of its own address wrong, and the browser's
//! honest `Origin` is then refused; [`proxy_mismatch`] spots that case so
//! the gate can say so in the log.

use std::fmt;

use axum::http::{
    HeaderMap, Method, Uri,
    uri::{Authority, Scheme},
};

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

/// A web origin: scheme, host and port, with the scheme's default port
/// filled in so `https://music.lan` and `https://music.lan:443` are equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// The origin of an absolute `http` or `https` URL. `None` for anything
    /// else, including the `null` a sandboxed page sends.
    pub fn from_url(url: &str) -> Option<Self> {
        let uri: Uri = url.trim().parse().ok()?;
        Self::new(uri.scheme_str()?, uri.authority()?)
    }

    /// The origin a request was addressed to: its scheme and its `Host`
    /// value (`host` or `host:port`).
    pub fn from_host(scheme: &str, host: &str) -> Option<Self> {
        Self::new(scheme, &host.trim().parse().ok()?)
    }

    fn new(scheme: &str, authority: &Authority) -> Option<Self> {
        let scheme = scheme.to_ascii_lowercase();
        let default_port = if scheme == Scheme::HTTP.as_str() {
            80
        } else if scheme == Scheme::HTTPS.as_str() {
            443
        } else {
            return None;
        };
        let host = authority.host();
        if host.is_empty() {
            return None;
        }
        // Whatever follows the host must be a real port: `Authority` reads
        // one it cannot parse as absent, which would pass for the default.
        let after_userinfo = authority.as_str().rsplit('@').next()?;
        let port = match after_userinfo.strip_prefix(host)? {
            "" => default_port,
            port => port.strip_prefix(':')?.parse().ok()?,
        };
        Some(Self {
            host: host.to_ascii_lowercase(),
            scheme,
            port,
        })
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

/// Run the check. `target` is the origin the request was addressed to,
/// `None` when the request carried no usable host.
pub fn check_origin(
    headers: &HeaderMap,
    target: Option<&Origin>,
    method: &Method,
    transport: Transport,
) -> OriginDecision {
    if transport == Transport::Bearer || !is_unsafe_method(method) {
        return OriginDecision::Allow;
    }
    if header(headers, "sec-fetch-site")
        .is_some_and(|site| site.eq_ignore_ascii_case("same-origin"))
    {
        return OriginDecision::Allow;
    }
    let Some(source) = source_header(headers) else {
        return OriginDecision::Deny;
    };
    // The dev frontend runs on its own port and reaches a debug build
    // through the debug CORS layer, which answers only these origins.
    // Release builds have neither.
    if cfg!(debug_assertions) && super::cors::is_debug_origin(source) {
        return OriginDecision::Allow;
    }
    match (Origin::from_url(source), target) {
        (Some(source), Some(target)) if source == *target => OriginDecision::Allow,
        _ => OriginDecision::Deny,
    }
}

/// The origin a refused request named, when it is on this server's host
/// and only the scheme or port differ from `target`. That is what a browser
/// on this very site sends when a reverse proxy drops the port or hides
/// HTTPS, and also what another app on the same host sends.
pub fn proxy_mismatch(headers: &HeaderMap, target: &Origin) -> Option<Origin> {
    let source = Origin::from_url(source_header(headers)?)?;
    (source.host == target.host && source != *target).then_some(source)
}

/// Where the request says it came from: `Origin`, else `Referer`.
fn source_header(headers: &HeaderMap) -> Option<&str> {
    header(headers, "origin").or_else(|| header(headers, "referer"))
}

/// A header's trimmed text, `None` when absent, empty or not text.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
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

    /// The decision for a cookie request sent to `scheme://host`.
    fn decide(scheme: &str, host: &str, method: Method, sent: &[(&str, &str)]) -> OriginDecision {
        let target = Origin::from_host(scheme, host);
        check_origin(&headers(sent), target.as_ref(), &method, Transport::Cookie)
    }

    #[test]
    fn cookie_mutation_needs_the_exact_origin_bearer_exempt() {
        use OriginDecision::{Allow, Deny};

        let lan = |sent: &[(&str, &str)]| decide("http", "music.lan:8688", Method::POST, sent);
        assert_eq!(lan(&[("origin", "http://music.lan:8688")]), Allow);
        assert_eq!(lan(&[("origin", "HTTP://Music.LAN:8688")]), Allow);
        assert_eq!(lan(&[("origin", "https://evil.test")]), Deny);
        // Same host, so same-site and same cookie jar, but another app.
        assert_eq!(lan(&[("origin", "http://music.lan:8096")]), Deny);
        assert_eq!(lan(&[("origin", "http://music.lan")]), Deny);
        assert_eq!(lan(&[("origin", "https://music.lan:8688")]), Deny);
        assert_eq!(lan(&[("origin", "null")]), Deny);
        assert_eq!(lan(&[("origin", "music.lan:8688")]), Deny);
        assert_eq!(lan(&[]), Deny);
        // Referer stands in only when Origin is absent.
        assert_eq!(
            lan(&[("referer", "http://music.lan:8688/albums?page=2")]),
            Allow
        );
        assert_eq!(lan(&[("referer", "http://music.lan:8096/albums")]), Deny);
        assert_eq!(
            lan(&[
                ("origin", "http://music.lan:8096"),
                ("referer", "http://music.lan:8688/")
            ]),
            Deny
        );

        // Default ports compare equal however either side renders them.
        let tls =
            |host: &str, origin: &str| decide("https", host, Method::DELETE, &[("origin", origin)]);
        assert_eq!(tls("music.example", "https://music.example"), Allow);
        assert_eq!(tls("music.example:443", "https://music.example"), Allow);
        assert_eq!(tls("music.example", "https://music.example:443"), Allow);
        assert_eq!(tls("music.example", "http://music.example"), Deny);
        assert_eq!(tls("music.example", "https://music.example:8443"), Deny);
        assert_eq!(tls("[::1]:8688", "https://[::1]:8688"), Allow);
        assert_eq!(tls("[::1]:8688", "https://[::2]:8688"), Deny);
        // A port that is not one never reads as the default port.
        assert_eq!(tls("music.example", "https://music.example:99999"), Deny);
        assert_eq!(tls("music.example", "https://music.example:"), Deny);
        assert_eq!(Origin::from_host("https", "music.example:99999"), None);
        assert_eq!(Origin::from_host("https", "[::1]:99999"), None);
        assert_ne!(
            Origin::from_host("http", "8:88"),
            Origin::from_host("http", "8")
        );

        // The browser's own verdict wins when it says same-origin, and
        // never widens the check when it says anything else.
        assert_eq!(lan(&[("sec-fetch-site", "same-origin")]), Allow);
        assert_eq!(
            lan(&[
                ("sec-fetch-site", "same-origin"),
                ("origin", "https://music.example")
            ]),
            Allow
        );
        for site in ["same-site", "cross-site", "none"] {
            assert_eq!(lan(&[("sec-fetch-site", site)]), Deny, "{site}");
            assert_eq!(
                lan(&[
                    ("sec-fetch-site", site),
                    ("origin", "http://music.lan:8096")
                ]),
                Deny,
                "{site}"
            );
            assert_eq!(
                lan(&[
                    ("sec-fetch-site", site),
                    ("origin", "http://music.lan:8688")
                ]),
                Allow,
                "{site}"
            );
        }

        let foreign = headers(&[("origin", "https://evil.test")]);
        let target = Origin::from_host("http", "music.lan:8688");
        assert_eq!(
            check_origin(&foreign, target.as_ref(), &Method::POST, Transport::Bearer),
            Allow
        );
        assert_eq!(
            check_origin(&foreign, target.as_ref(), &Method::GET, Transport::Cookie),
            Allow
        );
        // No usable host on the request: nothing can match it.
        assert_eq!(
            check_origin(&foreign, None, &Method::POST, Transport::Cookie),
            Deny
        );
        assert_eq!(Origin::from_host("http", ""), None);
        assert_eq!(Origin::from_host("ftp", "music.lan"), None);
    }
}
