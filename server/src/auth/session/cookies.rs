//! Session cookie build/parse.
//!
//! Policy: name `droppedneedle_session`, httpOnly,
//! `SameSite=Lax`, `Secure` auto-marked on HTTPS (direct scheme or
//! `X-Forwarded-Proto`, so plain-HTTP LAN installs keep working) and for a
//! sign-in from a stated `https` address ([`secure_for_sign_in`]),
//! `Path=<base>/api/v3`, 30-day max-age. The `__Host-` prefix stays rejected
//! (see module docs): base-path serving and HTTP LAN forbid it.

use axum::http::{HeaderMap, header::SET_COOKIE};

use super::{
    origin::{Origin, named_origin},
    stated::StatedAddresses,
    tokens::SESSION_MAX_AGE_SECS,
};

/// Cookie name, unchanged from v2.
pub const COOKIE_NAME: &str = "droppedneedle_session";

/// True when the request arrived over HTTPS: direct TLS (caller passes the
/// effective scheme) or `X-Forwarded-Proto: https` (first entry only) from a
/// trusted proxy. A spoofed
/// `X-Forwarded-Proto` from an untrusted peer cannot mark `Secure` on plain
/// HTTP. Direct TLS always wins regardless of headers.
///
/// Not for the sign-in routes: they ask [`secure_for_sign_in`], and this
/// stays private to the session module so that none can ask anything else.
pub(super) fn is_secure_trusted(
    scheme: &str,
    headers: &HeaderMap,
    from_trusted_proxy: bool,
) -> bool {
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    if !from_trusted_proxy {
        return false;
    }
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(|proto| proto.split(',').next().unwrap_or("").trim())
        .is_some_and(|first| first.eq_ignore_ascii_case("https"))
}

/// Whether a cookie is marked `Secure`. Only [`secure_for_sign_in`] says
/// yes, so a route that sets a cookie cannot decide it any other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecureMark(bool);

impl SecureMark {
    /// Unmarked, for a `Set-Cookie` that only clears a cookie. Over HTTPS
    /// a browser lets an unmarked clear remove a `Secure` cookie too.
    pub const fn for_clearing() -> Self {
        Self(false)
    }

    /// What the mark adds to the end of a `Set-Cookie` value.
    pub fn attribute(self) -> &'static str {
        if self.0 { "; Secure" } else { "" }
    }
}

/// Whether the cookies a sign-in sets are marked `Secure`. True when the
/// request arrived over HTTPS ([`is_secure_trusted`]), and also when the
/// page that sent it is at a stated address whose scheme is `https`: behind
/// a proxy that hides HTTPS the first rule alone leaves the cookie
/// unmarked, even on an address the admin has stated.
///
/// The page is the request's `Origin`, or its `Referer` only when there is
/// no `Origin`, and must equal the stated address in scheme, host and port.
/// Nothing else the request sends takes part, and the second rule only ever
/// adds `Secure`. A browser on that page reached it over HTTPS, so it sends
/// the cookie back. A sign-in from any other address, a plain-HTTP one
/// stated beside it included, gets the first rule's answer.
///
/// What that answer is worth depends on the browser. Cookies ignore the
/// scheme and the port, and a browser will not let a plain-HTTP answer
/// replace a `Secure` cookie. So a browser that holds the `Secure` cookie
/// from `https://nas.lan` drops the one a sign-in at `http://nas.lan:8688`
/// sets, and cannot sign in there until it signs out at the `https`
/// address. Another host name, or the IP address, is unaffected. A proxy
/// that reports HTTPS has always had the same effect.
///
/// `Origin` can be believed here because the browser sets it and page
/// script cannot. A client that makes one up changes only the cookie in
/// its own answer. The list is read only when the first rule says no.
pub fn secure_for_sign_in(
    scheme: &str,
    headers: &HeaderMap,
    from_trusted_proxy: bool,
    stated: &StatedAddresses,
) -> SecureMark {
    SecureMark(secure_given(scheme, headers, from_trusted_proxy, || {
        stated.origins()
    }))
}

/// [`secure_for_sign_in`] over a list that is fetched only when needed.
fn secure_given(
    scheme: &str,
    headers: &HeaderMap,
    from_trusted_proxy: bool,
    stated: impl FnOnce() -> Vec<Origin>,
) -> bool {
    is_secure_trusted(scheme, headers, from_trusted_proxy)
        || named_origin(headers).is_some_and(|page| page.is_https() && stated().contains(&page))
}

/// Cookie path for a deployment base path: `<base>/api/v3`. The base is `""`
/// at the domain root or `/music`-style without a trailing slash.
pub fn cookie_path(base_path: &str) -> String {
    format!("{}/api/v3", base_path.trim_end_matches('/'))
}

/// One `Set-Cookie` value attaching the session. The raw token travels only
/// here in cookie mode, never in the response body.
pub fn set_cookie_value(raw_token: &str, base_path: &str, secure: SecureMark) -> String {
    format!(
        "{COOKIE_NAME}={raw_token}; Path={}; Max-Age={SESSION_MAX_AGE_SECS}; HttpOnly; SameSite=Lax{}",
        cookie_path(base_path),
        secure.attribute(),
    )
}

/// One `Set-Cookie` value clearing the session (logout): same path, dead age.
pub fn clear_cookie_value(base_path: &str) -> String {
    format!(
        "{COOKIE_NAME}=; Path={}; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; HttpOnly; SameSite=Lax",
        cookie_path(base_path),
    )
}

/// Push a `Set-Cookie` header onto a response being built.
pub fn push_set_cookie(headers: &mut HeaderMap, cookie_value: &str) {
    if let Ok(value) = cookie_value.parse() {
        headers.append(SET_COOKIE, value);
    }
}

/// Read our session cookie from the request headers. Returns the raw token.
pub fn read_session_cookie(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(axum::http::header::COOKIE) {
        let Ok(cookies) = value.to_str() else {
            continue;
        };
        for pair in cookies.split(';') {
            let mut parts = pair.splitn(2, '=');
            let name = parts.next().unwrap_or("").trim();
            if name == COOKIE_NAME {
                let token = parts.next().unwrap_or("").trim().trim_matches('"');
                if !token.is_empty() {
                    return Some(token.to_owned());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_flags_and_base_path() {
        let lan = set_cookie_value("tok", "", SecureMark(false));
        assert!(lan.starts_with("droppedneedle_session=tok; "));
        assert!(lan.contains("Path=/api/v3; "));
        assert!(lan.contains("Max-Age=2592000; "));
        assert!(lan.contains("HttpOnly; SameSite=Lax"));
        assert!(!lan.contains("Secure"));

        let tls = set_cookie_value("tok", "/music/", SecureMark(true));
        assert!(tls.contains("Path=/music/api/v3; "));
        assert!(tls.ends_with("; Secure"));

        let cleared = clear_cookie_value("/music");
        assert!(cleared.contains("Path=/music/api/v3; "));
        assert!(cleared.contains("Max-Age=0"));
    }

    #[test]
    fn secure_detection_covers_direct_and_proxied_tls() {
        let plain = HeaderMap::new();
        assert!(is_secure_trusted("https", &plain, false));
        assert!(!is_secure_trusted("http", &plain, true));

        let mut forwarded = HeaderMap::new();
        forwarded.insert("x-forwarded-proto", "https".parse().unwrap());
        assert!(is_secure_trusted("http", &forwarded, true));

        let mut mixed = HeaderMap::new();
        mixed.insert("x-forwarded-proto", "http, https".parse().unwrap());
        assert!(!is_secure_trusted("http", &mixed, true));
    }

    #[test]
    fn a_sign_in_from_a_stated_https_address_is_secure() {
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
        fn origins(stated: &[&str]) -> Vec<Origin> {
            stated
                .iter()
                .map(|address| Origin::from_stated(address).unwrap())
                .collect()
        }
        // A proxy that hides HTTPS: plain HTTP as far as the server can see.
        let hidden = |stated: &[&str], sent: &[(&str, &str)]| {
            secure_given("http", &headers(sent), true, || origins(stated))
        };
        let both = ["https://music.example", "http://music.example:8080"];
        let https = ("origin", "https://music.example");
        let under_https = ("referer", "https://music.example/login?next=%2F");

        assert!(hidden(&both, &[https]));
        assert!(hidden(&both, &[("origin", "HTTPS://Music.Example:443")]));
        assert!(hidden(&both, &[under_https]));

        // A stated http address never marks the cookie, and stating an
        // https one does nothing for a sign-in from anywhere else.
        assert!(!hidden(&both, &[("origin", "http://music.example:8080")]));
        assert!(!hidden(&["http://music.example"], &[https]));
        assert!(!hidden(&["http://music.example:443"], &[https]));
        assert!(!hidden(&both, &[("origin", "http://192.168.1.5:8688")]));
        assert!(!hidden(&both, &[("origin", "https://music.example:8443")]));
        assert!(!hidden(&both, &[("origin", "https://evil.music.example")]));
        assert!(!hidden(&both, &[("origin", "https://other.example")]));
        assert!(!hidden(&both, &[]));
        // Referer stands in only when there is no Origin at all.
        assert!(!hidden(&both, &[("origin", "null"), under_https]));
        assert!(!hidden(
            &both,
            &[("origin", "http://music.example:8080"), under_https]
        ));
        // No other header names a stated address into the decision.
        assert!(!hidden(
            &both,
            &[
                ("host", "music.example"),
                ("x-forwarded-host", "music.example"),
                ("forwarded", "host=music.example;proto=https"),
                ("sec-fetch-site", "same-origin"),
            ]
        ));

        // With nothing stated the answer is the old rule's, whatever the
        // request names; and where the old rule says yes the list is not
        // read, so nothing in it or in the request can say no.
        let mut forwarded = headers(&[https]);
        forwarded.insert("x-forwarded-proto", "https".parse().unwrap());
        for sent in [headers(&[]), headers(&[https]), forwarded] {
            for (scheme, trusted) in [("http", true), ("http", false), ("https", false)] {
                let old = is_secure_trusted(scheme, &sent, trusted);
                assert_eq!(secure_given(scheme, &sent, trusted, Vec::new), old);
                if old {
                    assert!(secure_given(scheme, &sent, trusted, || {
                        unreachable!("the list is read only when the old rule says no")
                    }));
                }
            }
        }
        let plain_page = headers(&[("origin", "http://music.example:8080")]);
        assert!(secure_given("https", &plain_page, false, || origins(&both)));
    }

    #[test]
    fn cookie_parse_reads_ours_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            "other=1; droppedneedle_session=tok-9; theme=dark"
                .parse()
                .unwrap(),
        );
        assert_eq!(read_session_cookie(&headers).as_deref(), Some("tok-9"));
        assert_eq!(read_session_cookie(&HeaderMap::new()), None);
    }

    #[test]
    fn secure_trust_gating_ignores_spoofed_proto() {
        let mut spoofed = HeaderMap::new();
        spoofed.insert("x-forwarded-proto", "https".parse().unwrap());
        assert!(is_secure_trusted("http", &spoofed, true));
        assert!(!is_secure_trusted("http", &spoofed, false));

        let mut downgrade = HeaderMap::new();
        downgrade.insert("x-forwarded-proto", "http".parse().unwrap());
        assert!(is_secure_trusted("https", &downgrade, false));
        assert!(!is_secure_trusted("http", &HeaderMap::new(), true));
    }

    #[test]
    fn cookie_parse_skips_non_utf8_headers() {
        use axum::http::HeaderValue;
        let mut headers = HeaderMap::new();
        headers.append(
            axum::http::header::COOKIE,
            HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        headers.append(
            axum::http::header::COOKIE,
            HeaderValue::from_static("droppedneedle_session=tok-9"),
        );
        assert_eq!(read_session_cookie(&headers).as_deref(), Some("tok-9"));

        let mut only_bad = HeaderMap::new();
        only_bad.append(
            axum::http::header::COOKIE,
            HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        assert_eq!(read_session_cookie(&only_bad), None);
    }
}
