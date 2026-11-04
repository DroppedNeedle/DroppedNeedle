//! Credential extraction.
//!
//! Extraction order is Bearer-then-cookie (v2 semantics): an explicit
//! `Authorization: Bearer` header wins when present, otherwise the session
//! cookie.

use axum::http::HeaderMap;

/// Which transport carried the credential. Cookie sessions are ambient (CSRF
/// exposure, hence the origin check); Bearer is explicit and exempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// `Authorization: Bearer` header.
    Bearer,
    /// The session cookie, under either of its names.
    Cookie,
}

/// Extract the presented credential, Bearer-then-cookie. Returns the raw token
/// and the transport that carried it.
///
/// A present Bearer token wins outright (v2 parity): when the `Authorization`
/// header carries the Bearer token, an empty value yields `None` with no cookie
/// fallthrough. Only a missing or non-Bearer token falls through to the cookie.
pub fn extract(headers: &HeaderMap) -> Option<(String, Transport)> {
    if has_bearer_scheme(headers) {
        return extract_bearer(headers).map(|token| (token, Transport::Bearer));
    }
    super::cookies::read_session_cookie(headers).map(|token| (token, Transport::Cookie))
}

/// Read a Bearer token the `Authorization` header, if well-formed and non-empty.
/// The scheme matches case-insensitively per RFC 7235 (v2 parity); the `Bearer `
/// prefix with its space must be present, so a bare `Bearer` is not a scheme.
pub fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = strip_bearer_prefix(value)?.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

/// True when the `Authorization` header carries the Bearer token (any casing),
/// regardless of whether a token follows it.
fn has_bearer_scheme(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| strip_bearer_prefix(value).is_some())
}

/// Split off a case-insensitive `Bearer ` prefix (scheme plus its space).
fn strip_bearer_prefix(value: &str) -> Option<&str> {
    const PREFIX_LEN: usize = "bearer ".len();
    let prefix = value.get(..PREFIX_LEN)?;
    prefix
        .eq_ignore_ascii_case("bearer ")
        .then(|| &value[PREFIX_LEN..])
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

    /// Bearer wins (any casing), then the cookie; an empty Bearer never
    /// falls through, while a bare `Bearer` word or another scheme does.
    #[test]
    fn token_extraction_order() {
        const COOKIE: &str = "droppedneedle_sid=tok-c";
        let bearer = |token: &str| Some((token.to_owned(), Transport::Bearer));
        let cookie = Some(("tok-c".to_owned(), Transport::Cookie));
        type Case = (
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
            Option<(String, Transport)>,
        );
        let cases: [Case; 11] = [
            (
                "bearer beats cookie",
                Some("Bearer tok-b"),
                Some(COOKIE),
                bearer("tok-b"),
            ),
            ("cookie alone", None, Some(COOKIE), cookie.clone()),
            ("nothing presented", None, None, None),
            ("empty bearer alone", Some("Bearer  "), None, None),
            ("scheme Bearer", Some("Bearer tok-1"), None, bearer("tok-1")),
            ("scheme bearer", Some("bearer tok-1"), None, bearer("tok-1")),
            ("scheme BEARER", Some("BEARER tok-1"), None, bearer("tok-1")),
            ("scheme bEaReR", Some("bEaReR tok-1"), None, bearer("tok-1")),
            (
                "a bare Bearer word is not a scheme (v2 parity)",
                Some("Bearer"),
                Some(COOKIE),
                cookie.clone(),
            ),
            (
                "empty bearer never falls through to the cookie",
                Some("Bearer  "),
                Some(COOKIE),
                None,
            ),
            (
                "non-Bearer schemes still fall through",
                Some("Basic dXNlcjpwYXNz"),
                Some(COOKIE),
                cookie.clone(),
            ),
        ];
        for (label, authorization, cookie_header, expected) in cases {
            let mut pairs = Vec::new();
            if let Some(value) = authorization {
                pairs.push(("authorization", value));
            }
            if let Some(value) = cookie_header {
                pairs.push(("cookie", value));
            }
            assert_eq!(extract(&headers(&pairs)), expected, "{label}");
        }
    }
}
