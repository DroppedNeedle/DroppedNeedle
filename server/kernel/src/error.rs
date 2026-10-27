//! Typed API errors and the shared error envelope.
//!
//! Every native error renders as `{"error": {code, message, details}}` with a
//! SCREAMING_SNAKE code. Client faults (4xx) carry a message safe for users.
//! [`ApiError`] is the one error type that becomes a response: an area maps
//! its own errors into it, and that mapping is where a status is chosen.
//!
//! A server fault (5xx) never carries its cause on the wire. The cause rides
//! in a private response extension ([`Fault`]) to the request boundary
//! (`request_scope`), which logs it once with the request id and writes the
//! body: the fixed internal error with `details.error_id`, or, for the short
//! closed list in [`Unavailable`], that answer's fixed sentence.

use std::fmt::Display;

use axum::{
    Json,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

/// Resource or route not found.
pub const NOT_FOUND: &str = "NOT_FOUND";
/// Known route hit with the wrong method.
pub const METHOD_NOT_ALLOWED: &str = "METHOD_NOT_ALLOWED";
/// Unspecified server fault.
pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";
/// Missing or invalid credential.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Valid credential lacking rights.
pub const FORBIDDEN: &str = "FORBIDDEN";
/// The request itself is wrong. The message is user-facing.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// The request conflicts with current state. The message is user-facing.
pub const CONFLICT: &str = "CONFLICT";
/// A body larger than the route allows.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
/// The caller is over its request budget.
pub const RATE_LIMITED: &str = "RATE_LIMITED";
/// The database was too busy to answer in time.
pub const SERVER_BUSY: &str = "SERVER_BUSY";
/// The user's media server did not answer or answered with an error.
pub const REMOTE_UNAVAILABLE: &str = "REMOTE_UNAVAILABLE";
/// GitHub refused a plugin install or update request.
pub const UPSTREAM_REFUSED: &str = "UPSTREAM_REFUSED";
/// GitHub is rate limiting plugin installs from this server.
pub const UPSTREAM_RATE_LIMITED: &str = "UPSTREAM_RATE_LIMITED";
/// GitHub could not be reached during a plugin install or update.
pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";
/// GitHub answered a plugin install or update with something unreadable.
pub const UPSTREAM_UNREADABLE: &str = "UPSTREAM_UNREADABLE";
/// An outside service (MusicBrainz, Last.fm, ...) is down, rate limiting
/// or unreachable. `details.provider` names it.
pub const PROVIDER_UNAVAILABLE: &str = "PROVIDER_UNAVAILABLE";
/// An outside service needs a key or account first, or refused the one
/// saved (409). `details.provider` names it.
pub const PROVIDER_NOT_CONFIGURED: &str = "PROVIDER_NOT_CONFIGURED";

/// User-facing 404 message, kept from v2.
pub const NOT_FOUND_MESSAGE: &str = "Not found";
/// Fixed 405 message.
pub const METHOD_NOT_ALLOWED_MESSAGE: &str = "Method not allowed";
/// Fixed 5xx message. This string is the whole body contract for server
/// faults: no cause text, hosts, or paths may join it.
pub const FIXED_INTERNAL_MESSAGE: &str = "Internal server error";
/// Fixed 413 message.
pub const FIXED_TOO_LARGE_MESSAGE: &str = "Request body too large";
/// Fixed 429 message.
pub const FIXED_RATE_LIMITED_MESSAGE: &str = "Too many requests";

/// The `error` object inside every error response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorBody {
    /// SCREAMING_SNAKE machine code.
    pub code: String,
    /// Human message. Fixed for 5xx, descriptive for 4xx.
    pub message: String,
    /// Extra detail, or null when there is none.
    pub details: Option<Value>,
}

/// Top-level error envelope.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    /// The single error payload.
    pub error: ErrorBody,
}

/// The closed list of 5xx answers that reach the client with their own
/// code and a fixed sentence. Every other 5xx is the fixed internal error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The database was too busy to answer in time.
    ServerBusy,
    /// The user's media server did not answer or answered with an error.
    MediaServer,
    /// GitHub refused a plugin install or update request.
    UpstreamRefused,
    /// GitHub is rate limiting this server.
    UpstreamRateLimited {
        /// Seconds until GitHub takes requests again, when it said.
        retry_after: Option<u64>,
    },
    /// GitHub could not be reached during a plugin install or update.
    UpstreamError,
    /// GitHub answered a plugin install or update with something unreadable.
    UpstreamUnreadable,
    /// An outside service is down, rate limiting or unreachable.
    Provider {
        /// The service's lowercase key (`musicbrainz`, `lastfm`, ...).
        provider: &'static str,
        /// Seconds until it takes requests again, when it said.
        retry_after: Option<u64>,
    },
}

/// What one [`Unavailable`] answers: everything the boundary writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableAnswer {
    /// Response status.
    pub status: StatusCode,
    /// Machine code.
    pub code: &'static str,
    /// The fixed sentence shown to the user.
    pub sentence: &'static str,
    /// Seconds for the `Retry-After` header, when there is one.
    pub retry_after: Option<u64>,
    /// Whether `retry_after` is also repeated in `details.retry_after`.
    pub retry_after_in_details: bool,
    /// The outside service named in `details.provider`, when there is one.
    pub provider: Option<&'static str>,
}

impl Unavailable {
    /// Status, code, sentence and headers of this answer.
    pub fn answer(&self) -> UnavailableAnswer {
        let provider = match *self {
            Self::Provider { provider, .. } => Some(provider),
            _ => None,
        };
        let (status, code, sentence, retry_after, retry_after_in_details) = match *self {
            Self::ServerBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                SERVER_BUSY,
                "The server is busy. Try again in a moment.",
                Some(1),
                false,
            ),
            Self::MediaServer => (
                StatusCode::BAD_GATEWAY,
                REMOTE_UNAVAILABLE,
                "Your media server did not answer or returned an error.",
                None,
                false,
            ),
            Self::UpstreamRefused => (
                StatusCode::BAD_GATEWAY,
                UPSTREAM_REFUSED,
                "GitHub refused the request",
                None,
                false,
            ),
            Self::UpstreamRateLimited { retry_after } => (
                StatusCode::SERVICE_UNAVAILABLE,
                UPSTREAM_RATE_LIMITED,
                "GitHub is rate limiting this server, try again later",
                retry_after,
                true,
            ),
            Self::UpstreamError => (
                StatusCode::BAD_GATEWAY,
                UPSTREAM_ERROR,
                "GitHub could not be reached",
                None,
                false,
            ),
            Self::UpstreamUnreadable => (
                StatusCode::BAD_GATEWAY,
                UPSTREAM_UNREADABLE,
                "GitHub sent an answer DroppedNeedle could not read.",
                None,
                false,
            ),
            Self::Provider { retry_after, .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                PROVIDER_UNAVAILABLE,
                "An outside service is not answering. Try again later.",
                retry_after,
                true,
            ),
        };
        UnavailableAnswer {
            status,
            code,
            sentence,
            retry_after,
            retry_after_in_details,
            provider,
        }
    }
}

impl UnavailableAnswer {
    /// The body, naming `error_id` when the request id is known.
    pub fn envelope(&self, error_id: Option<&str>) -> ErrorEnvelope {
        let mut details = serde_json::Map::new();
        if let Some(error_id) = error_id {
            details.insert("error_id".to_owned(), json!(error_id));
        }
        if let Some(provider) = self.provider {
            details.insert("provider".to_owned(), json!(provider));
        }
        if self.retry_after_in_details
            && let Some(seconds) = self.retry_after
        {
            details.insert("retry_after".to_owned(), json!(seconds));
        }
        ErrorEnvelope {
            error: ErrorBody {
                code: self.code.to_owned(),
                message: self.sentence.to_owned(),
                details: (!details.is_empty()).then_some(Value::Object(details)),
            },
        }
    }
}

/// A server fault on its way to the request boundary, carried as a
/// response extension. Only this module builds one, it is never
/// serialized, and its cause never becomes a body or a header.
#[derive(Debug, Clone)]
pub struct Fault {
    unavailable: Option<Unavailable>,
    cause: Option<String>,
}

impl Fault {
    /// The closed-list answer this fault passes through, if any.
    pub fn unavailable(&self) -> Option<Unavailable> {
        self.unavailable
    }

    /// The cause for the log line, if one was given.
    pub fn cause(&self) -> Option<&str> {
        self.cause.as_deref()
    }
}

/// The one error type that becomes a response. Boxed, so a `Result`
/// carrying it stays one pointer wide on the error side.
#[derive(Debug)]
pub struct ApiError(Box<Parts>);

#[derive(Debug)]
struct Parts {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Option<Value>,
    headers: HeaderMap,
    fault: Option<Fault>,
}

impl ApiError {
    /// An error with an area's own code. Meant for 4xx answers: a 5xx
    /// status answers as [`ApiError::fault`] with `message` as the cause, so
    /// the message never reaches the wire.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        let message = message.into();
        if status.is_server_error() {
            return Self::fault(status, message);
        }
        Self(Box::new(Parts {
            status,
            code,
            message,
            details: None,
            headers: HeaderMap::new(),
            fault: None,
        }))
    }

    /// 404 with the fixed message.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, NOT_FOUND, NOT_FOUND_MESSAGE)
    }

    /// 405 with the fixed message.
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            METHOD_NOT_ALLOWED,
            METHOD_NOT_ALLOWED_MESSAGE,
        )
    }

    /// 400 `INVALID_INPUT`. The message is shown to the user.
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, INVALID_INPUT, message)
    }

    /// 409 `CONFLICT`. The message is shown to the user.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, CONFLICT, message)
    }

    /// 409 `PROVIDER_NOT_CONFIGURED`, naming the outside service in
    /// `details.provider`. The message is shown to the user.
    pub fn provider_not_configured(provider: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, PROVIDER_NOT_CONFIGURED, message)
            .with_details(json!({ "provider": provider }))
    }

    /// 403 `FORBIDDEN`.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, FORBIDDEN, message)
    }

    /// 401 `UNAUTHORIZED` with the `WWW-Authenticate: Bearer` challenge.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::unauthorized_without_challenge(message)
            .with_header(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))
    }

    /// 401 `UNAUTHORIZED` without a challenge header.
    pub fn unauthorized_without_challenge(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, UNAUTHORIZED, message)
    }

    /// 413 `PAYLOAD_TOO_LARGE` with the fixed message.
    pub fn too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            PAYLOAD_TOO_LARGE,
            FIXED_TOO_LARGE_MESSAGE,
        )
    }

    /// 429 `RATE_LIMITED` with `Retry-After` (at least one second).
    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            RATE_LIMITED,
            FIXED_RATE_LIMITED_MESSAGE,
        )
        .with_header(header::RETRY_AFTER, retry_after_secs.max(1).into())
    }

    /// A 500. The cause is logged once by the request boundary, never sent.
    pub fn internal(cause: impl Display) -> Self {
        Self::fault(StatusCode::INTERNAL_SERVER_ERROR, cause)
    }

    /// A server fault keeping its own 5xx status (a 502 or 503 that is not
    /// on the closed list). A status that is not 5xx answers 500. The body
    /// is the fixed internal error; the cause goes to the log only.
    pub fn fault(status: StatusCode, cause: impl Display) -> Self {
        let status = if status.is_server_error() {
            status
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        Self(Box::new(Parts {
            status,
            code: INTERNAL_ERROR,
            message: FIXED_INTERNAL_MESSAGE.to_owned(),
            details: None,
            headers: HeaderMap::new(),
            fault: Some(Fault {
                unavailable: None,
                cause: Some(cause.to_string()),
            }),
        }))
    }

    /// One of the closed-list answers, with a cause for the log.
    pub fn unavailable(variant: Unavailable, cause: impl Display) -> Self {
        let mut error = Self::from(variant);
        if let Some(fault) = error.0.fault.as_mut() {
            fault.cause = Some(cause.to_string());
        }
        error
    }

    /// Attach `details`. Ignored on a server fault, whose body is fixed.
    pub fn with_details(mut self, details: Value) -> Self {
        if self.0.fault.is_none() {
            self.0.details = Some(details);
        }
        self
    }

    /// Add a response header. The boundary keeps only `Retry-After` on a 5xx.
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.0.headers.insert(name, value);
        self
    }

    /// The response status.
    pub fn status(&self) -> StatusCode {
        self.0.status
    }

    /// The machine code.
    pub fn code(&self) -> &'static str {
        self.0.code
    }

    /// Render the fixed 5xx envelope for one request id.
    pub fn internal_response(error_id: &str) -> Response {
        Self::server_error_response(StatusCode::INTERNAL_SERVER_ERROR, error_id)
    }

    /// Render the fixed 5xx envelope keeping the original status. Only the
    /// body is fixed; the status keeps its distinct 5xx meaning (a 503 stays
    /// a 503). Callers pass a server-error status.
    pub fn server_error_response(status: StatusCode, error_id: &str) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: INTERNAL_ERROR.to_owned(),
                message: FIXED_INTERNAL_MESSAGE.to_owned(),
                details: Some(json!({ "error_id": error_id })),
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<Unavailable> for ApiError {
    fn from(variant: Unavailable) -> Self {
        let answer = variant.answer();
        let mut headers = HeaderMap::new();
        if let Some(seconds) = answer.retry_after {
            headers.insert(header::RETRY_AFTER, seconds.into());
        }
        Self(Box::new(Parts {
            status: answer.status,
            code: answer.code,
            message: answer.sentence.to_owned(),
            details: None,
            headers,
            fault: Some(Fault {
                unavailable: Some(variant),
                cause: None,
            }),
        }))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let parts = *self.0;
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: parts.code.to_owned(),
                message: parts.message,
                details: parts.details,
            },
        };
        let mut response = (parts.status, Json(body)).into_response();
        response.headers_mut().extend(parts.headers);
        if let Some(fault) = parts.fault {
            response.extensions_mut().insert(fault);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fault_never_puts_its_cause_on_the_wire() {
        let response = ApiError::fault(StatusCode::NOT_FOUND, "secret at /srv/x").into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let headers = format!("{:?}", response.headers());
        assert!(!headers.contains("secret"), "{headers}");
        let fault = response.extensions().get::<Fault>().unwrap();
        assert_eq!(fault.cause(), Some("secret at /srv/x"));

        let response = ApiError::new(StatusCode::BAD_GATEWAY, CONFLICT, "secret").into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(response.extensions().get::<Fault>().is_some());
    }
}
