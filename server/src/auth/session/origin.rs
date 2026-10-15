//! Origin check on cookie-authenticated mutations.
//!
//! v2 relied on `SameSite=Lax` alone; this closes the gap without a CSRF-token
//! round-trip. Rule: a request authenticated via cookie with an unsafe method
//! must come from the page the server itself serves, or from an address an
//! admin has stated as this server's own, else 403.
//! Bearer-authenticated requests are exempt (explicit, non-ambient). Safe
//! methods skip the check.
//!
//! `Sec-Fetch-Site: same-origin` settles it: the browser sets that header
//! itself, page script cannot, and it holds whatever a proxy did to the host
//! or scheme on the way in. Browsers leave the header out on plain-HTTP
//! pages (other than localhost) and old ones never send it, so without it
//! the scheme, host and port of `Origin` (or `Referer` when `Origin` is
//! absent) must equal the address the request was sent to, or one of the
//! stated addresses. A request with neither header fails closed (scripted
//! callers use Bearer tokens).
//!
//! The port and scheme count because cookies ignore both and `SameSite`
//! ignores the port: another app on the same host, one port over, is
//! same-site and its forms carry this session cookie.
//!
//! The address the request was sent to is built in
//! `middleware::effective_origin`: forwarded headers are honored only from
//! trusted peers. A proxy that drops the port from `Host` or hides HTTPS
//! makes the server's idea of its own address wrong, and the browser's
//! honest `Origin` is then refused. The gate says so in the log, once.
//!
//! The stated addresses are the `public_address` setting, the way out for
//! an operator who cannot fix the proxy. They add to the address the server
//! works out and never replace it, so the app opened directly at its own
//! port always works. They are compared with the browser's `Origin` (or
//! `Referer`) and nothing else: no `Host`, no forwarded header and no other
//! part of the request can widen the check.

use axum::http::{HeaderMap, Method};

use super::extract::Transport;
use droppedneedle_kernel::runtime_config::origin::Origin;

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

/// Run the check. `target` is the origin the request was addressed to,
/// `None` when the request carried no usable host. `stated` is the saved
/// `public_address` list. Of `headers`, only `Sec-Fetch-Site`, `Origin` and
/// `Referer` are read.
pub fn check_origin(
    headers: &HeaderMap,
    target: Option<&Origin>,
    stated: &[Origin],
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
    let Some(source) = Origin::from_url(source) else {
        return OriginDecision::Deny;
    };
    if target == Some(&source) || stated.contains(&source) {
        OriginDecision::Allow
    } else {
        OriginDecision::Deny
    }
}

/// The origin a request says it came from, parsed. `None` when it named
/// none, or one that is not `http` or `https`.
pub fn named_origin(headers: &HeaderMap) -> Option<Origin> {
    Origin::from_url(source_header(headers)?)
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

    /// The decision for a cookie request sent to `scheme://host`, with no
    /// public address stated.
    fn decide(scheme: &str, host: &str, method: Method, sent: &[(&str, &str)]) -> OriginDecision {
        let target = Origin::from_host(scheme, host);
        check_origin(
            &headers(sent),
            target.as_ref(),
            &[],
            &method,
            Transport::Cookie,
        )
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
            check_origin(
                &foreign,
                target.as_ref(),
                &[],
                &Method::POST,
                Transport::Bearer
            ),
            Allow
        );
        assert_eq!(
            check_origin(
                &foreign,
                target.as_ref(),
                &[],
                &Method::GET,
                Transport::Cookie
            ),
            Allow
        );
        // No usable host on the request: nothing can match it.
        assert_eq!(
            check_origin(&foreign, None, &[], &Method::POST, Transport::Cookie),
            Deny
        );
        assert_eq!(Origin::from_host("http", ""), None);
        assert_eq!(Origin::from_host("ftp", "music.lan"), None);

        // A proxy dropped the port and hid HTTPS, so the server sees itself
        // at http://music.example. The admin stated the real addresses.
        let stated: Vec<Origin> = ["https://music.example", "http://music.lan:8080"]
            .into_iter()
            .map(|address| Origin::from_stated(address).unwrap())
            .collect();
        let behind_proxy = |method: Method, transport, sent: &[(&str, &str)]| {
            check_origin(
                &headers(sent),
                Origin::from_host("http", "music.example").as_ref(),
                &stated,
                &method,
                transport,
            )
        };
        let put = |sent: &[(&str, &str)]| behind_proxy(Method::PUT, Transport::Cookie, sent);
        assert_eq!(put(&[("origin", "https://music.example")]), Allow);
        assert_eq!(put(&[("origin", "https://music.example:443")]), Allow);
        assert_eq!(put(&[("origin", "HTTPS://Music.Example")]), Allow);
        assert_eq!(put(&[("origin", "http://music.lan:8080")]), Allow);
        // What the server works out for itself still passes.
        assert_eq!(put(&[("origin", "http://music.example")]), Allow);
        // Scheme, host and port all have to match a stated address.
        assert_eq!(put(&[("origin", "https://music.example:8443")]), Deny);
        assert_eq!(put(&[("origin", "http://music.lan")]), Deny);
        assert_eq!(put(&[("origin", "http://music.lan:8096")]), Deny);
        assert_eq!(put(&[("origin", "https://music.lan:8080")]), Deny);
        assert_eq!(put(&[("origin", "https://music.example.evil.test")]), Deny);
        assert_eq!(put(&[("origin", "https://music.example@evil.test")]), Deny);
        assert_eq!(put(&[("origin", "https://evil.test")]), Deny);
        // A subdomain of a stated host, and that host with a trailing dot.
        assert_eq!(put(&[("origin", "https://evil.music.example")]), Deny);
        assert_eq!(put(&[("origin", "https://music.example.")]), Deny);
        assert_eq!(put(&[("origin", "null")]), Deny);
        assert_eq!(put(&[]), Deny);
        // Referer stands in only when Origin is absent.
        assert_eq!(put(&[("referer", "https://music.example/albums")]), Allow);
        // A stated address in the query names nothing.
        assert_eq!(
            put(&[("referer", "https://evil.test/?next=https://music.example")]),
            Deny
        );
        assert_eq!(
            put(&[
                ("origin", "https://evil.test"),
                ("referer", "https://music.example/")
            ]),
            Deny
        );
        // No other header can name a stated address into the check.
        assert_eq!(
            put(&[
                ("origin", "https://evil.test"),
                ("host", "music.lan:8080"),
                ("x-forwarded-host", "music.lan:8080"),
                ("x-forwarded-proto", "https"),
                ("forwarded", "host=music.example;proto=https"),
                ("sec-fetch-site", "same-site"),
            ]),
            Deny
        );
        assert_eq!(
            behind_proxy(Method::PUT, Transport::Bearer, &[]),
            Allow,
            "bearer stays exempt"
        );
        assert_eq!(
            behind_proxy(Method::GET, Transport::Cookie, &[]),
            Allow,
            "safe methods stay exempt"
        );
    }

    #[test]
    fn a_stated_address_is_an_origin_and_nothing_more() {
        let stored = |text: &str| Origin::from_stated(text).map(|origin| origin.serialized());
        for (typed, kept) in [
            ("https://music.example.com", "https://music.example.com"),
            ("https://music.example.com/", "https://music.example.com"),
            ("  HTTPS://Music.Example.COM  ", "https://music.example.com"),
            ("https://music.example.com:443", "https://music.example.com"),
            ("http://music.lan:80/", "http://music.lan"),
            ("http://music.lan:8688", "http://music.lan:8688"),
            ("https://music.lan:80", "https://music.lan:80"),
            ("http://192.168.1.5:8688", "http://192.168.1.5:8688"),
            ("http://[::1]:8688", "http://[::1]:8688"),
            ("https://[2001:DB8::1]", "https://[2001:db8::1]"),
            (
                "https://m\u{fc}nchen.example",
                "https://xn--mnchen-3ya.example",
            ),
        ] {
            assert_eq!(stored(typed).as_deref(), Ok(kept), "{typed}");
            // What is kept reads back as the same origin, and a browser on
            // that page sends exactly it.
            assert_eq!(Origin::from_stated(kept), Origin::from_stated(typed));
            assert_eq!(Origin::from_url(kept), Origin::from_stated(typed).ok());
        }
        for refused in [
            "",
            "music.example.com",
            "music.example.com:8688",
            "//music.example.com",
            "ftp://music.example.com",
            "file:///srv/music",
            "https://",
            "https://music.example.com/music",
            "https://music.example.com/music/",
            "https://music.example.com//",
            "https://music.example.com?next=1",
            "https://music.example.com/?",
            "https://music.example.com#top",
            "https://admin@music.example.com",
            "https://admin:secret@music.example.com",
            "https://music.example.com@evil.test",
            "https://*.example.com",
            "https://*",
            "https://%2A.example.com",
            "https://%2a.example.com",
            "http://music.lan:0",
            "http://music.lan:65536",
            "http://music.lan:port",
            "http://music lan",
            "http://music.lan http://other.lan",
            "null",
        ] {
            assert!(stored(refused).is_err(), "{refused:?} was accepted");
        }
    }
}
