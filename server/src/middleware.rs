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
//!   failure shape: the app's from `CompatSetup::server_error`, upgrade
//!   mode's from the default Subsonic settings ([`Boundary`]).
//!
//! A fault that would answer the fixed internal error answers
//! `DISK_FULL` (507) instead while the data or temp folder has less than
//! [`DISK_FULL_BELOW`] free, read at that moment through the boundary's
//! free-space probe. It is decided from the disk, never from the error's
//! text.
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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::state::AppState;
use droppedneedle_kernel::{
    config::AppConfig,
    disk::{DISK_FULL_BELOW, READ_BOUND, least_free},
    error::{ApiError, Fault, Unavailable},
    ids::{IdGenerator, REQUEST_ID_HEADER, RequestId},
};

/// The protocol failure a compat path answers with: path, whether the
/// request was a HEAD, its query, the status and `Retry-After`.
type CompatFailure =
    dyn Fn(&str, bool, Option<&str>, StatusCode, Option<HeaderValue>) -> Response + Send + Sync;

/// The least free bytes on the data and temp folders, read now; `None`
/// when that cannot be told. Blocking.
pub type FreeSpace = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

/// What the request boundary needs: fresh request ids, the compat failure
/// shape and the free-space probe.
#[derive(Clone)]
pub struct Boundary {
    ids: Arc<dyn IdGenerator>,
    compat_failure: Arc<CompatFailure>,
    free_space: FreeSpace,
    /// Set while a free-space read runs: a read that hangs is left to end,
    /// and none starts beside it.
    reading: Arc<AtomicBool>,
}

impl Boundary {
    /// Request ids from `ids`, free space on the data and temp folders from
    /// `free_space`, compat failures from `compat_failure`.
    pub fn new(
        ids: Arc<dyn IdGenerator>,
        free_space: FreeSpace,
        compat_failure: impl Fn(&str, bool, Option<&str>, StatusCode, Option<HeaderValue>) -> Response
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            ids,
            compat_failure: Arc::new(compat_failure),
            free_space,
            reading: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The probe over the data and temp folders of `config`.
    pub fn disk_probe(config: &AppConfig) -> FreeSpace {
        let folders: Vec<std::path::PathBuf> = config
            .library_db_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .into_iter()
            .chain([config.temp_dir.clone()])
            .collect();
        Arc::new(move || least_free(&folders))
    }

    /// The free space the boundary's probe reads now. Blocking.
    pub fn free_space_now(&self) -> Option<u64> {
        (self.free_space)()
    }

    /// The app's boundary: its ids, and compat failures in the shape the
    /// saved Subsonic settings give.
    pub fn from_state(state: &AppState) -> Self {
        let compat = state.compat.clone();
        Self::new(
            state.ids.clone(),
            Self::disk_probe(&state.config),
            move |path, head, query, status, retry_after| {
                compat.server_error(path, head, query, status, retry_after)
            },
        )
    }
}

/// Enforce the request scope described in the module docs.
pub async fn request_scope(
    State(state): State<Boundary>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = incoming_request_id(&request).unwrap_or_else(|| state.ids.new_id());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let head = method == Method::HEAD;
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
                log_fault(&request_id, &method, &path, status, fault.as_ref());
                (state.compat_failure)(&path, head, query.as_deref(), status, retry_after)
            } else {
                let disk_full = fault.as_ref().and_then(Fault::unavailable).is_none()
                    && disk_is_full(&state).await;
                native_server_error(
                    &request_id,
                    &method,
                    &path,
                    status,
                    retry_after,
                    fault,
                    disk_full,
                )
            }
        }
        Err(_) => {
            tracing::error!(%request_id, %method, %path, "request handler panicked");
            if is_compat_path(&path) {
                (state.compat_failure)(
                    &path,
                    head,
                    query.as_deref(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                )
            } else if disk_is_full(&state).await {
                tracing::warn!(%request_id, "the data disk is full; the panic answers DISK_FULL");
                disk_full_response(&request_id)
            } else {
                ApiError::internal_response(&request_id)
            }
        }
    };

    set_request_id_header(&mut response, &request_id);
    response
}

/// Clears the boundary's reading flag when its read returns or panics.
struct ReadDone(Arc<AtomicBool>);

impl Drop for ReadDone {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Whether the data or temp folder has less than [`DISK_FULL_BELOW`] free,
/// read on the blocking pool under [`READ_BOUND`]. A read that fails, does
/// not answer in time, or would start while an earlier one still runs is
/// not a full disk.
async fn disk_is_full(boundary: &Boundary) -> bool {
    if boundary.reading.swap(true, Ordering::AcqRel) {
        return false;
    }
    let probe = Arc::clone(&boundary.free_space);
    let reading = Arc::clone(&boundary.reading);
    let read = tokio::time::timeout(
        READ_BOUND,
        tokio::task::spawn_blocking(move || {
            let _done = ReadDone(reading);
            probe()
        }),
    )
    .await;
    matches!(read, Ok(Ok(Some(free))) if free < DISK_FULL_BELOW)
}

/// The `DISK_FULL` answer, with no `Retry-After`: waiting does not free a
/// disk.
fn disk_full_response(request_id: &str) -> Response {
    let answer = Unavailable::DiskFull.answer();
    (answer.status, Json(answer.envelope(Some(request_id)))).into_response()
}

/// The native 5xx body: the closed-list answer the fault names, `DISK_FULL`
/// when `disk_full`, or the fixed internal error. Status and `Retry-After`
/// are kept, except that a full disk answers its own status and none.
fn native_server_error(
    request_id: &str,
    method: &Method,
    path: &str,
    status: StatusCode,
    retry_after: Option<HeaderValue>,
    fault: Option<Fault>,
    disk_full: bool,
) -> Response {
    log_fault(request_id, method, path, status, fault.as_ref());
    if disk_full {
        tracing::warn!(%request_id, "the data disk is full; the fault answers DISK_FULL");
        return disk_full_response(request_id);
    }
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

/// Log a 5xx once with its request id and route: a closed-list answer at
/// warn, anything else at error.
fn log_fault(
    request_id: &str,
    method: &Method,
    path: &str,
    status: StatusCode,
    fault: Option<&Fault>,
) {
    let cause = fault.and_then(Fault::cause).unwrap_or("none given");
    match fault.and_then(Fault::unavailable) {
        Some(variant) => {
            let code = variant.answer().code;
            tracing::warn!(%request_id, %method, path, %status, code, cause, "request unavailable");
        }
        None if fault.is_some() => {
            tracing::error!(%request_id, %method, path, %status, cause, "request failed");
        }
        None => {
            tracing::error!(%request_id, %method, path, %status, "5xx answered with the fixed body");
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
