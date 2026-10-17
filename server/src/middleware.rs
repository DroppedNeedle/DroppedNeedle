//! Request-scope middleware: ids, tracing span, and the 5xx boundary.
//!
//! The middleware resolves the request id (echoing a caller-supplied
//! `x-request-id` or minting one) and serves the request inside a tracing
//! span carrying that id, method, and path only (never the query: compat
//! puts app-password secrets in the query string, so queries must never
//! reach the logs; the wiring tests fail on any full-target logging).
//!
//! It is also the one place a 5xx body is written and logged. On the way
//! out, every 5xx keeps its status and `Retry-After` and nothing else:
//! - A native answer from the closed list
//!   ([`Unavailable`](droppedneedle_kernel::error::Unavailable)) gets that
//!   answer's code and fixed sentence, with the request id.
//! - Any other native 5xx gets the fixed internal error naming the request
//!   id, and a panic becomes that same 500.
//! - A compat (`/subsonic`, `/jellyfin`) 5xx or panic gets its protocol's
//!   failure shape, from `CompatSetup::server_error`.
//!
//! The cause an [`ApiError`](droppedneedle_kernel::error::ApiError) carried
//! is logged here once, with the request id.

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use droppedneedle_compat::shared::path_case::is_compat_path;
use futures_util::FutureExt as _;
use std::panic::AssertUnwindSafe;
use tracing::Instrument as _;

use crate::state::AppState;
use droppedneedle_kernel::{
    error::{ApiError, Fault},
    ids::{REQUEST_ID_HEADER, RequestId},
};

/// Enforce the request scope described in the module docs.
pub async fn request_scope(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = incoming_request_id(&request).unwrap_or_else(|| state.ids.new_id());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let path = request.uri().path().to_owned();
    let head = request.method() == Method::HEAD;
    // Read by the compat failure shape for `f` and `callback` only.
    let query = request.uri().query().map(str::to_owned);

    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %request.method(),
        path = %path,
    );

    let outcome = AssertUnwindSafe(next.run(request).instrument(span))
        .catch_unwind()
        .await;
    let mut response = match outcome {
        Ok(response) if !response.status().is_server_error() => response,
        Ok(mut response) => {
            let status = response.status();
            let retry_after = response.headers().get(header::RETRY_AFTER).cloned();
            let fault = response.extensions_mut().remove::<Fault>();
            if is_compat_path(&path) {
                log_fault(&request_id, status, fault.as_ref());
                state
                    .compat
                    .server_error(&path, head, query.as_deref(), status, retry_after)
            } else {
                native_server_error(&request_id, status, retry_after, fault)
            }
        }
        Err(_) => {
            tracing::error!(%request_id, "request handler panicked");
            if is_compat_path(&path) {
                state.compat.server_error(
                    &path,
                    head,
                    query.as_deref(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                )
            } else {
                ApiError::internal_response(&request_id)
            }
        }
    };

    set_request_id_header(&mut response, &request_id);
    response
}

/// The native 5xx body: the closed-list answer the fault names, or the
/// fixed internal error. Status and `Retry-After` are kept.
fn native_server_error(
    request_id: &str,
    status: StatusCode,
    retry_after: Option<HeaderValue>,
    fault: Option<Fault>,
) -> Response {
    log_fault(request_id, status, fault.as_ref());
    let mut response = match fault.as_ref().and_then(Fault::unavailable) {
        Some(variant) => {
            let answer = variant.answer();
            (answer.status, Json(answer.envelope(Some(request_id)))).into_response()
        }
        None => ApiError::server_error_response(status, request_id),
    };
    if let Some(value) = retry_after {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Log a 5xx once with its request id: a closed-list answer at warn,
/// anything else at error.
fn log_fault(request_id: &str, status: StatusCode, fault: Option<&Fault>) {
    let cause = fault.and_then(Fault::cause).unwrap_or("none given");
    match fault.and_then(Fault::unavailable) {
        Some(variant) => {
            let code = variant.answer().code;
            tracing::warn!(%request_id, %status, code, cause, "request unavailable");
        }
        None if fault.is_some() => {
            tracing::error!(%request_id, %status, cause, "request failed");
        }
        None => {
            tracing::error!(%request_id, %status, "5xx answered with the fixed body");
        }
    }
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
    use super::is_safe_request_id;

    #[test]
    fn request_ids_are_short_and_plain() {
        assert!(is_safe_request_id("4f1c2a9e-77b0-4d3e-9d1b-2f3a4b5c6d7e"));
        assert!(!is_safe_request_id(""));
        assert!(!is_safe_request_id(&"a".repeat(129)));
        assert!(!is_safe_request_id("id with spaces"));
        assert!(!is_safe_request_id("line\nbreak"));
    }
}
