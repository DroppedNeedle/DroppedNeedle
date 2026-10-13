//! Request-scope middleware: ids, tracing span, and the 5xx boundary.
//!
//! The middleware resolves the request id (echoing a caller-supplied
//! `x-request-id` or minting one), serves the request inside a tracing span
//! carrying that id, method, and path only (never the query: compat puts
//! app-password secrets in the query string, so queries must never reach
//! the logs; the wiring tests fail on any full-target logging), and
//! guarantees the leak contract on the way out: any
//! native 5xx response keeps its status and `Retry-After` header but gets
//! the fixed envelope body naming the request id, and panics become a 500
//! of that same envelope. Compat (`/subsonic`, `/jellyfin`) 5xx responses
//! are exempt from the body rewrite (their shapes are fixed by the
//! protocols), though panics there still become the fixed 500. Handler code can
//! therefore return raw 5xx bodies only by accident, never to the wire.
//!
//! The HSTS layer lives here too. It wraps the request scope and adds one
//! response header, see [`strict_transport_security`].

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use futures_util::FutureExt as _;
use std::{panic::AssertUnwindSafe, sync::Arc};
use tracing::Instrument as _;

use crate::{
    auth::session::middleware::{TrustedProxies, arrived_over_https},
    error::ApiError,
    ids::{REQUEST_ID_HEADER, RequestId},
    runtime_config::{
        ConfigStore,
        sections::{HSTS_MAX_AGE_LIMIT, SecuritySettings},
    },
    state::AppState,
};

/// Whether a path belongs to the compat shims (case-insensitive).
/// Compat 5xx responses keep their protocol-shaped bodies: rewriting
/// them into the native [`ApiError`](crate::error::ApiError) envelope
/// would break Subsonic/Jellyfin clients parsing their own shape.
fn is_compat_path(path: &str) -> bool {
    let folded = path.to_lowercase();
    folded.starts_with("/subsonic") || folded.starts_with("/jellyfin")
}

/// Enforce the request scope described in the module docs.
pub async fn request_scope(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let compat_path = is_compat_path(request.uri().path());
    let request_id = incoming_request_id(&request).unwrap_or_else(|| state.ids.new_id());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %request.method(),
        path = %request.uri().path(),
    );

    let outcome = AssertUnwindSafe(next.run(request).instrument(span))
        .catch_unwind()
        .await;
    let mut response = match outcome {
        // Compat paths keep status, headers (including `Retry-After`),
        // and body untouched: their 5xx shapes are protocol-pinned.
        Ok(response) if response.status().is_server_error() && !compat_path => {
            let status = response.status();
            let retry_after = response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .cloned();
            tracing::error!(%request_id, %status, "replaced 5xx body with fixed envelope");
            let mut rewritten = ApiError::server_error_response(status, &request_id);
            if let Some(value) = retry_after {
                rewritten
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
            rewritten
        }
        Ok(response) => response,
        Err(_) => {
            tracing::error!(%request_id, "request handler panicked");
            ApiError::internal_response(&request_id)
        }
    };

    set_request_id_header(&mut response, &request_id);
    response
}

/// What the HSTS layer needs: the live settings and the proxies whose
/// forwarded scheme counts.
#[derive(Debug, Clone)]
pub struct TransportSecurity {
    config: Arc<ConfigStore>,
    trusted_proxies: TrustedProxies,
}

impl TransportSecurity {
    /// Read the settings and the trusted proxies the session gate uses.
    pub fn from_state(state: &AppState) -> Self {
        Self {
            config: state.auth.config_store.clone(),
            trusted_proxies: state.auth.session_auth.trusted_proxies.clone(),
        }
    }
}

/// Add `Strict-Transport-Security` to the response when the Security
/// settings ask for it and the request arrived over HTTPS.
///
/// A browser that has seen the header refuses plain HTTP to this host for
/// `max-age` seconds, so it is never sent on a plain-HTTP request, on a
/// forwarded scheme from a peer that is not a trusted proxy, or when the
/// settings cannot be read. The settings are read after the handler ran, so
/// the response to the save that turns HSTS off already goes without it.
pub async fn strict_transport_security(
    State(security): State<TransportSecurity>,
    request: Request,
    next: Next,
) -> Response {
    let https = arrived_over_https(&request, &security.trusted_proxies);
    let mut response = next.run(request).await;
    if !https {
        return response;
    }
    match security.config.get::<SecuritySettings>() {
        Ok(settings) => {
            if let Some(value) = hsts_value(&settings) {
                response
                    .headers_mut()
                    .insert(axum::http::header::STRICT_TRANSPORT_SECURITY, value);
            }
        }
        Err(error) => tracing::debug!(%error, "security settings unreadable; no HSTS header"),
    }
    response
}

/// The header value for these settings, the v2 format. `None` when HSTS is
/// off: a max-age of zero or less. Saves refuse a max-age outside
/// `0..=HSTS_MAX_AGE_LIMIT`, but a config file written before that check,
/// or by hand, can still hold one: a negative reads as off and anything
/// above the limit is sent as the limit.
fn hsts_value(settings: &SecuritySettings) -> Option<axum::http::HeaderValue> {
    if settings.hsts_max_age <= 0 {
        return None;
    }
    let max_age = settings.hsts_max_age.min(HSTS_MAX_AGE_LIMIT);
    let mut value = format!("max-age={max_age}");
    if settings.hsts_include_subdomains {
        value.push_str("; includeSubDomains");
    }
    if settings.hsts_preload {
        value.push_str("; preload");
    }
    axum::http::HeaderValue::from_str(&value).ok()
}

/// Longest caller-supplied request id kept; longer ones are replaced.
const MAX_REQUEST_ID_LEN: usize = 128;

/// Read a caller-supplied request id. It is echoed into logs and error
/// bodies, so only short ids of letters, digits, `-`, `_`, `.` and `:` are
/// kept; anything else gets a minted id instead.
fn incoming_request_id(request: &Request) -> Option<String> {
    request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_safe_request_id(value))
        .map(str::to_owned)
}

fn is_safe_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// Stamp the response with the request id; values we mint are always valid.
fn set_request_id_header(response: &mut Response, request_id: &str) {
    if let Ok(value) = request_id.parse() {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
}

#[cfg(test)]
mod tests {
    use super::{HSTS_MAX_AGE_LIMIT, SecuritySettings, hsts_value};
    use super::{is_compat_path, is_safe_request_id};

    #[test]
    fn stored_max_age_out_of_range_is_off_or_capped() {
        let stored = |hsts_max_age| SecuritySettings {
            hsts_max_age,
            ..SecuritySettings::default()
        };
        assert_eq!(hsts_value(&stored(-1)), None);
        assert_eq!(
            hsts_value(&stored(i64::MAX)).unwrap(),
            format!("max-age={HSTS_MAX_AGE_LIMIT}").as_str()
        );
    }

    #[test]
    fn request_ids_are_short_and_plain() {
        assert!(is_safe_request_id("4f1c2a9e-77b0-4d3e-9d1b-2f3a4b5c6d7e"));
        assert!(!is_safe_request_id(""));
        assert!(!is_safe_request_id(&"a".repeat(129)));
        assert!(!is_safe_request_id("id with spaces"));
        assert!(!is_safe_request_id("line\nbreak"));
    }

    #[test]
    fn compat_paths_skip_the_5xx_rewrite() {
        assert!(is_compat_path("/subsonic/rest/ping"));
        assert!(is_compat_path("/jellyfin/System/Info/Public"));
        assert!(is_compat_path("/SUBSONIC/rest/ping"));
        assert!(!is_compat_path("/api/v3/stream/local/x"));
        assert!(!is_compat_path("/health"));
    }
}
