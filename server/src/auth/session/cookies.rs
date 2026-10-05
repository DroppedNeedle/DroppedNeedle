//! Session cookie build/parse.
//!
//! Policy: name `droppedneedle_session`, httpOnly,
//! `SameSite=Lax`, `Secure` auto-marked on HTTPS (direct scheme or
//! `X-Forwarded-Proto`, so plain-HTTP LAN installs keep working),
//! `Path=<base>/api/v3`, 30-day max-age. The `__Host-` prefix stays rejected
//! (see module docs): base-path serving and HTTP LAN forbid it.

use axum::http::{HeaderMap, header::SET_COOKIE};

use super::tokens::SESSION_MAX_AGE_SECS;

/// Cookie name, unchanged from v2.
pub const COOKIE_NAME: &str = "droppedneedle_session";

/// True when the request arrived over HTTPS: direct TLS (caller passes the
/// effective scheme) or `X-Forwarded-Proto: https` behind a proxy. This form
/// trusts the forwarded value unconditionally; new callers must use
/// [`is_secure_trusted`] with the peer verdict instead.
pub fn is_secure(scheme: &str, headers: &HeaderMap) -> bool {
    is_secure_trusted(scheme, headers, true)
}

/// Trust-aware form of [`is_secure`]: the forwarded proto (first entry only)
/// is honored only when the peer is a trusted proxy, so a spoofed
/// `X-Forwarded-Proto` from an untrusted peer cannot mark `Secure` on plain
/// HTTP. Direct TLS always wins regardless of headers.
pub fn is_secure_trusted(scheme: &str, headers: &HeaderMap, from_trusted_proxy: bool) -> bool {
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

/// Cookie path for a deployment base path: `<base>/api/v3`. The base is `""`
/// at the domain root or `/music`-style without a trailing slash.
pub fn cookie_path(base_path: &str) -> String {
    format!("{}/api/v3", base_path.trim_end_matches('/'))
}

/// One `Set-Cookie` value attaching the session. The raw token travels only
/// here in cookie mode, never in the response body.
pub fn set_cookie_value(raw_token: &str, base_path: &str, secure: bool) -> String {
    let mut value = format!(
        "{COOKIE_NAME}={raw_token}; Path={}; Max-Age={SESSION_MAX_AGE_SECS}; HttpOnly; SameSite=Lax",
        cookie_path(base_path),
    );
    if secure {
        value.push_str("; Secure");
    }
    value
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
        let lan = set_cookie_value("tok", "", false);
        assert!(lan.starts_with("droppedneedle_session=tok; "));
        assert!(lan.contains("Path=/api/v3; "));
        assert!(lan.contains("Max-Age=2592000; "));
        assert!(lan.contains("HttpOnly; SameSite=Lax"));
        assert!(!lan.contains("Secure"));

        let tls = set_cookie_value("tok", "/music/", true);
        assert!(tls.contains("Path=/music/api/v3; "));
        assert!(tls.ends_with("; Secure"));

        let cleared = clear_cookie_value("/music");
        assert!(cleared.contains("Path=/music/api/v3; "));
        assert!(cleared.contains("Max-Age=0"));
    }

    #[test]
    fn secure_detection_covers_direct_and_proxied_tls() {
        let plain = HeaderMap::new();
        assert!(is_secure("https", &plain));
        assert!(!is_secure("http", &plain));

        let mut forwarded = HeaderMap::new();
        forwarded.insert("x-forwarded-proto", "https".parse().unwrap());
        assert!(is_secure("http", &forwarded));

        let mut mixed = HeaderMap::new();
        mixed.insert("x-forwarded-proto", "http, https".parse().unwrap());
        assert!(!is_secure("http", &mixed));
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
