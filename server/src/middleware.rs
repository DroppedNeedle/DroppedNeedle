//! Request-scope middleware: ids, tracing span, and the 5xx boundary.
//!
//! The middleware resolves the request id (echoing a caller-supplied
//! `x-request-id` or minting one), serves the request inside a tracing span
//! carrying that id, and guarantees the leak contract on the way out: any
//! 5xx response keeps its status and `Retry-After` header but gets the fixed
//! envelope body naming the request id, and panics become a 500 of that same
//! envelope. Handler code can therefore return raw 5xx bodies only by
//! accident, never to the wire.

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
        Ok(response) if response.status().is_server_error() => {
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
