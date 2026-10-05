//! Credential extraction plus the 401/403 response shapes.
//!
//! Extraction order is Bearer-then-cookie (v2 semantics): an explicit
//! `Authorization: Bearer` header wins when present, otherwise the session
//! cookie. Every 401 carries `WWW-Authenticate: Bearer` (v2 parity) and the
//! shared error envelope with `UNAUTHORIZED`/`FORBIDDEN` codes, rendered
//! through the crate's `error` module (no local envelope duplicate).
//!
//! The user-owned-resource matrix this preserves: no valid session -> 401,
//! valid session without rights -> 403 (or 404 where the resource must stay
//! hidden), owner ok, admin ok. The 401 and 403 rows live here; per-resource
//! 403-or-404 choices land with the role extractors.

use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use crate::error::{ErrorBody, ErrorEnvelope};

/// Which transport carried the credential. Cookie sessions are ambient (CSRF
/// exposure, hence the origin check); Bearer is explicit and exempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// `Authorization: Bearer` header.
    Bearer,
    /// `droppedneedle_session` cookie.
    Cookie,
}

/// Machine code for missing/invalid sessions.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Machine code for valid sessions lacking rights (and origin-check failures).
pub const FORBIDDEN: &str = "FORBIDDEN";

/// Extract the presented credential, Bearer-then-cookie. Returns the raw token
/// and the transport that carried it.
///
/// A present Bearer token wins outright (v2 parity): when the `Authorization`
/// header carries the Bearer token, an empty value yields `None` with NO cookie
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

/// 401 response: shared envelope, `WWW-Authenticate: Bearer`.
pub fn unauthorized_response(message: &str) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: UNAUTHORIZED.to_owned(),
            message: message.to_owned(),
            details: None,
        },
    };
    (
        StatusCode::UNAUTHORIZED,
        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
        Json(body),
    )
        .into_response()
}

/// 403 response: shared envelope, no auth challenge (the session is valid).
pub fn forbidden_response(message: &str) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: FORBIDDEN.to_owned(),
            message: message.to_owned(),
            details: None,
        },
    };
    (StatusCode::FORBIDDEN, Json(body)).into_response()
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
    fn bearer_wins_then_cookie() {
        let both = headers(&[
            ("authorization", "Bearer tok-b"),
            ("cookie", "droppedneedle_session=tok-c"),
        ]);
        assert_eq!(
            extract(&both),
            Some(("tok-b".to_owned(), Transport::Bearer))
        );

        let cookie_only = headers(&[("cookie", "droppedneedle_session=tok-c")]);
        assert_eq!(
            extract(&cookie_only),
            Some(("tok-c".to_owned(), Transport::Cookie))
        );

        assert_eq!(extract(&HeaderMap::new()), None);
        let empty_bearer = headers(&[("authorization", "Bearer  ")]);
        assert_eq!(extract(&empty_bearer), None);
    }

    #[test]
    fn bearer_scheme_matches_any_casing() {
        for scheme in ["Bearer", "bearer", "BEARER", "bEaReR"] {
            let headers = headers(&[("authorization", &format!("{scheme} tok-1"))]);
            assert_eq!(
                extract(&headers),
                Some(("tok-1".to_owned(), Transport::Bearer)),
                "{scheme} must authenticate"
            );
        }
        let bare = headers(&[
            ("authorization", "Bearer"),
            ("cookie", "droppedneedle_session=tok-c"),
        ]);
        assert_eq!(
            extract(&bare),
            Some(("tok-c".to_owned(), Transport::Cookie)),
            "a bare Bearer word is not a scheme (v2 parity)"
        );
    }

    #[test]
    fn empty_bearer_never_falls_through_to_cookie() {
        let empty = headers(&[
            ("authorization", "Bearer  "),
            ("cookie", "droppedneedle_session=tok-c"),
        ]);
        assert_eq!(extract(&empty), None);
        let other_scheme = headers(&[
            ("authorization", "Basic dXNlcjpwYXNz"),
            ("cookie", "droppedneedle_session=tok-c"),
        ]);
        assert_eq!(
            extract(&other_scheme),
            Some(("tok-c".to_owned(), Transport::Cookie)),
            "non-Bearer schemes still fall through"
        );
    }
}
