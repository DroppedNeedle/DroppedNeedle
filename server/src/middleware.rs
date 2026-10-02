//! Request-scope middleware: ids, tracing span, and the 5xx boundary.
//!
//! The middleware resolves the request id (echoing a caller-supplied
//! `x-request-id` or minting one), serves the request inside a tracing span
//! carrying that id, method, and path only (never the query: compat puts
//! app-password secrets in the query string, so queries must never reach
//! the logs — the wiring suite trips on any full-target logging), and
//! guarantees the leak contract on the way out: any
//! native 5xx response keeps its status and `Retry-After` header but gets
//! the fixed envelope body naming the request id, and panics become a 500
//! of that same envelope. Compat (`/subsonic`, `/jellyfin`) 5xx responses
//! are exempt from the body rewrite — their shapes are protocol-pinned —
//! though panics there still become the fixed 500. Handler code can
//! therefore return raw 5xx bodies only by accident, never to the wire.

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use futures_util::FutureExt as _;
use std::panic::AssertUnwindSafe;
use tracing::Instrument as _;

use crate::{
    error::ApiError,
    ids::{REQUEST_ID_HEADER, RequestId},
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

/// Read a caller-supplied request id, if it is usable as a header value.
fn incoming_request_id(request: &Request) -> Option<String> {
    request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

/// Stamp the response with the request id; values we mint are always valid.
fn set_request_id_header(response: &mut Response, request_id: &str) {
    if let Ok(value) = request_id.parse() {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
}

#[cfg(test)]
mod tests {
    use super::is_compat_path;

    #[test]
    fn compat_paths_skip_the_5xx_rewrite() {
        assert!(is_compat_path("/subsonic/rest/ping"));
        assert!(is_compat_path("/jellyfin/System/Info/Public"));
        assert!(is_compat_path("/SUBSONIC/rest/ping"));
        assert!(!is_compat_path("/api/v3/stream/local/x"));
        assert!(!is_compat_path("/health"));
    }
}
