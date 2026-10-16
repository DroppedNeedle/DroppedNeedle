//! Session cookie build/parse.
//!
//! Policy: httpOnly, `SameSite=Lax`, `Secure` auto-marked on HTTPS (direct
//! scheme or `X-Forwarded-Proto`, so plain-HTTP LAN installs keep working)
//! and for a sign-in from a stated `https` address ([`secure_for_sign_in`]),
//! `Path=<base>/api/v3`, 30-day max-age. The `__Host-` prefix stays rejected
//! (see module docs): base-path serving and HTTP LAN forbid it.
//!
//! Each cookie goes by two names, chosen by its mark: `__Secure-<name>`
//! when marked `Secure`, `<name>` when not ([`CookieName`]). The session
//! cookie is `droppedneedle_sid` or `__Secure-droppedneedle_sid`. A request
//! that carries the `__Secure-` name is read by it alone, and sign-out ends
//! the session under each name the request carries and clears both.

use std::fmt::Display;

use axum::http::{
    HeaderMap,
    header::{COOKIE, SET_COOKIE},
};

use super::{origin::named_origin, stated::StatedAddresses, tokens::SESSION_MAX_AGE_SECS};
use droppedneedle_kernel::runtime_config::origin::Origin;

/// The session cookie.
pub const SESSION_COOKIE: CookieName = CookieName {
    plain: "droppedneedle_sid",
    secure: "__Secure-droppedneedle_sid",
};

/// The cookie binding a pending single sign-on login to the browser that
/// started it.
pub const OIDC_STATE_COOKIE: CookieName = CookieName {
    plain: "droppedneedle_oidc_state",
    secure: "__Secure-droppedneedle_oidc_state",
};

/// The lifetime part of a `Set-Cookie` value that removes a cookie.
const GONE: &str = "Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT";

/// One of our cookies under its two names. A cookie marked `Secure` is set
/// as `__Secure-<name>`, an unmarked one as `<name>`, so the http and https
/// addresses on one host name each keep their own cookie in a browser.
///
/// What browsers do, checked 2026-10-16 against RFC 6265bis
/// (draft-ietf-httpbis-rfc6265bis-22) and MDN's browser compatibility data
/// (`http.headers.Set-Cookie.host_secure_prefixes`):
///
/// - Cookies ignore scheme and port. An answer over plain HTTP cannot store
///   a `Secure` cookie (§5.7 step 13), nor an unmarked one under the name
///   of a `Secure` one whose path covers it (§5.7 step 16). With one name
///   for both, the https sign-in would block the http one.
/// - A `__Secure-` cookie is stored only when marked `Secure` (§5.7 step
///   20), so only an https answer can set or clear it, and a nameless
///   cookie whose value starts with `__Secure-` is refused (§5.7 step 22).
///   Browsers match the prefix case-insensitively (§5.4); we read the
///   exact name only.
/// - Any plain-HTTP answer for the host name (another port, someone on the
///   network) can set the plain name. So a request carrying the `__Secure-`
///   name is read by it alone, valid or not: falling back to the plain one
///   would let a planted cookie take over the https address once its own
///   session is revoked.
/// - Chrome 49, Firefox 50 and Safari 13 and later enforce the prefix, and
///   Edge, Opera and the Android and iOS browsers follow them. An older
///   browser treats the name as any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CookieName {
    plain: &'static str,
    secure: &'static str,
}

impl CookieName {
    /// The name a cookie with this mark is set under.
    fn under(self, mark: SecureMark) -> &'static str {
        if mark.0 { self.secure } else { self.plain }
    }

    /// One `Set-Cookie` value, named by its mark.
    fn line(self, mark: SecureMark, value: &str, path: &str, lifetime: &str) -> String {
        format!(
            "{}={value}; Path={path}; {lifetime}; HttpOnly; SameSite=Lax{}",
            self.under(mark),
            mark.attribute(),
        )
    }

    /// The `Set-Cookie` values that hand the cookie over: the cookie under
    /// the name its mark gives and, for an unmarked one, a marked clear of
    /// the `__Secure-` name after it. Over plain HTTP the browser ignores
    /// that clear, so an https sign-in survives. At an https address the
    /// server failed to mark, it removes the `__Secure-` cookie, which
    /// would otherwise outrank the new one.
    pub fn set(
        self,
        value: &str,
        path: &str,
        max_age: impl Display,
        mark: SecureMark,
    ) -> Vec<String> {
        let mut lines = vec![self.line(mark, value, path, &format!("Max-Age={max_age}"))];
        if !mark.0 {
            lines.push(self.line(SecureMark(true), "", path, GONE));
        }
        lines
    }

    /// The `Set-Cookie` values that clear the cookie under both names, the
    /// `__Secure-` one marked (a browser refuses it otherwise).
    pub fn clear(self, path: &str) -> [String; 2] {
        [
            self.line(SecureMark(false), "", path, GONE),
            self.line(SecureMark(true), "", path, GONE),
        ]
    }

    /// The cookie's value from the request: the `__Secure-` name's when the
    /// request carries it with a value, otherwise the plain name's. Order
    /// in the `Cookie` header does not matter; where a name appears more
    /// than once its first non-empty value counts.
    pub fn read(self, headers: &HeaderMap) -> Option<String> {
        first_value(headers, self.secure).or_else(|| first_value(headers, self.plain))
    }

    /// The value under each name the request carries, `__Secure-` first.
    pub fn read_each(self, headers: &HeaderMap) -> Vec<String> {
        let mut values: Vec<String> = [self.secure, self.plain]
            .into_iter()
            .filter_map(|name| first_value(headers, name))
            .collect();
        values.dedup();
        values
    }
}

/// The first non-empty value of the cookie named exactly `name`.
fn first_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let value = value.trim().trim_matches('"');
            (key.trim() == name && !value.is_empty()).then(|| value.to_owned())
        })
}

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

/// The `Set-Cookie` values attaching the session ([`CookieName::set`]).
/// The raw token travels only here in cookie mode, never in the response
/// body.
pub fn set_session_cookie(raw_token: &str, base_path: &str, secure: SecureMark) -> Vec<String> {
    SESSION_COOKIE.set(
        raw_token,
        &cookie_path(base_path),
        SESSION_MAX_AGE_SECS,
        secure,
    )
}

/// The `Set-Cookie` values clearing the session under both names (logout).
pub fn clear_session_cookie(base_path: &str) -> [String; 2] {
    SESSION_COOKIE.clear(&cookie_path(base_path))
}

/// Push a `Set-Cookie` header onto a response being built.
pub fn push_set_cookie(headers: &mut HeaderMap, cookie_value: &str) {
    if let Ok(value) = cookie_value.parse() {
        headers.append(SET_COOKIE, value);
    }
}

/// Read our session cookie from the request headers ([`CookieName::read`]).
/// Returns the raw token.
pub fn read_session_cookie(headers: &HeaderMap) -> Option<String> {
    SESSION_COOKIE.read(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mark names the cookie: `__Secure-droppedneedle_sid` with
    /// `Secure`, `droppedneedle_sid` without. The `__Secure-` name, when
    /// sent with a value, is the only one read; the old v2 name never is.
    #[test]
    fn the_mark_names_the_cookie_and_the_secure_name_is_read_first() {
        let tls = set_session_cookie("tok", "/music/", SecureMark(true));
        assert_eq!(
            tls,
            [
                "__Secure-droppedneedle_sid=tok; Path=/music/api/v3; Max-Age=2592000; HttpOnly; SameSite=Lax; Secure"
            ],
            "a marked sign-in sets its cookie alone"
        );

        let lan = set_session_cookie("tok", "", SecureMark(false));
        assert_eq!(lan.len(), 2, "{lan:?}");
        assert_eq!(
            lan[0],
            "droppedneedle_sid=tok; Path=/api/v3; Max-Age=2592000; HttpOnly; SameSite=Lax"
        );
        assert!(
            lan[1].starts_with("__Secure-droppedneedle_sid=; Path=/api/v3; Max-Age=0; ")
                && lan[1].ends_with("; Secure"),
            "an unmarked sign-in clears the marked name after it: {}",
            lan[1]
        );

        let cleared = clear_session_cookie("/music");
        assert!(cleared[0].starts_with("droppedneedle_sid=; Path=/music/api/v3; Max-Age=0; "));
        assert!(!cleared[0].contains("Secure"), "{}", cleared[0]);
        assert!(
            cleared[1].starts_with("__Secure-droppedneedle_sid=; Path=/music/api/v3; Max-Age=0; ")
        );
        assert!(cleared[1].ends_with("; Secure"), "{}", cleared[1]);

        let state = OIDC_STATE_COOKIE.set("h", "/api", 600, SecureMark(true));
        assert_eq!(
            state,
            [
                "__Secure-droppedneedle_oidc_state=h; Path=/api; Max-Age=600; HttpOnly; SameSite=Lax; Secure"
            ]
        );

        let read = |cookie: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(COOKIE, cookie.parse().unwrap());
            read_session_cookie(&headers)
        };
        let reads = |cookie: &str, token: Option<&str>| {
            assert_eq!(read(cookie).as_deref(), token, "{cookie}");
        };
        reads("other=1; droppedneedle_sid=a; theme=dark", Some("a"));
        reads(
            "__Secure-droppedneedle_sid=b; droppedneedle_sid=a",
            Some("b"),
        );
        reads(
            "droppedneedle_sid=a; __Secure-droppedneedle_sid=b",
            Some("b"),
        );
        reads(
            "__Secure-droppedneedle_sid=; droppedneedle_sid=a",
            Some("a"),
        );
        reads("droppedneedle_sid=; droppedneedle_sid=a", Some("a"));
        reads(
            "__secure-droppedneedle_sid=b; droppedneedle_sid=a",
            Some("a"),
        );
        reads("droppedneedle_session=old", None);
        assert_eq!(read_session_cookie(&HeaderMap::new()), None);
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
    fn the_reader_skips_cookie_headers_that_are_not_text() {
        use axum::http::HeaderValue;
        let mut headers = HeaderMap::new();
        headers.append(COOKIE, HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap());
        headers.append(COOKIE, HeaderValue::from_static("droppedneedle_sid=tok-9"));
        assert_eq!(read_session_cookie(&headers).as_deref(), Some("tok-9"));

        let mut only_bad = HeaderMap::new();
        only_bad.append(COOKIE, HeaderValue::from_bytes(&[0xff]).unwrap());
        assert_eq!(read_session_cookie(&only_bad), None);
    }
}
