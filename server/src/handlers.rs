//! Thin Axum handlers. Each handler answers one route and returns typed
//! errors; status mapping lives in `error::ApiError`.

use axum::Json;
use serde::Serialize;
use utoipa::{OpenApi as _, ToSchema};

use crate::docs::ApiDoc;
use droppedneedle_kernel::error::ApiError;

/// Health payload, in the v2 shape. The route is public, so it says
/// nothing about the server's internals; admins read those from the
/// admin routes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Service state, `ok` when serving.
    pub status: String,
    /// Human-readable running message.
    pub message: String,
}

/// Liveness probe. Public, unauthenticated, cheap.
#[utoipa::path(
    get,
    tag = "health",
    path = "/health",
    responses((status = 200, description = "Server is running", body = HealthResponse))
)]
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        message: "DroppedNeedle backend running".to_owned(),
    })
}

/// The generated OpenAPI document feeding the TypeScript pipeline.
#[utoipa::path(
    get,
    tag = "health",
    path = "/openapi.json",
    responses((status = 200, description = "This API document", body = Object))
)]
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Unknown routes render the shared 404 envelope, never an empty body.
pub async fn fallback_404() -> ApiError {
    ApiError::not_found()
}

/// Known route, wrong method: the shared 405 envelope, never an empty body.
pub async fn fallback_405() -> ApiError {
    ApiError::method_not_allowed()
}

/// Failure hooks for the 5xx-leak tests. Mounted only when the state opts
/// in; the production binary never does.
#[cfg(any(test, feature = "test-support"))]
pub mod test_hooks {
    use axum::{
        Json,
        extract::{Path, RawQuery},
        http::{HeaderValue, StatusCode, header},
        response::IntoResponse,
    };

    use super::HealthResponse;
    use droppedneedle_kernel::error::{ApiError, Unavailable};

    /// Marker substrings that must never reach a response body. They look
    /// real on purpose: a filesystem path, an internal host, a secret token.
    pub const LEAK_MARKERS: &[&str] = &[
        "/srv/secrets/droppedneedle.key",
        "postgres.internal.example.com",
        "dn-secret-marker-9f31",
    ];

    fn leaky_cause() -> String {
        format!(
            "boom reading {} via {} ({})",
            LEAK_MARKERS[0], LEAK_MARKERS[1], LEAK_MARKERS[2]
        )
    }

    /// Typed server fault carrying all leak markers in its cause: a 500, or
    /// with `?status=503` a 503 with `Retry-After: 30`.
    pub async fn typed_error(RawQuery(query): RawQuery) -> ApiError {
        if query.as_deref() == Some("status=503") {
            return ApiError::fault(StatusCode::SERVICE_UNAVAILABLE, leaky_cause())
                .with_header(header::RETRY_AFTER, HeaderValue::from_static("30"));
        }
        ApiError::internal(leaky_cause())
    }

    /// One closed-list answer, picked by its code, with the leak markers in
    /// its cause. The GitHub rate limit says 42 seconds, the provider
    /// outage 30.
    pub async fn unavailable(Path(code): Path<String>) -> ApiError {
        let variant = match code.as_str() {
            "SERVER_BUSY" => Unavailable::ServerBusy,
            "REMOTE_UNAVAILABLE" => Unavailable::MediaServer,
            "UPSTREAM_REFUSED" => Unavailable::UpstreamRefused,
            "UPSTREAM_RATE_LIMITED" => Unavailable::UpstreamRateLimited {
                retry_after: Some(42),
            },
            "UPSTREAM_ERROR" => Unavailable::UpstreamError,
            "UPSTREAM_UNREADABLE" => Unavailable::UpstreamUnreadable,
            "PROVIDER_UNAVAILABLE" => Unavailable::Provider {
                provider: "listenbrainz",
                retry_after: Some(30),
            },
            _ => return ApiError::not_found(),
        };
        ApiError::unavailable(variant, leaky_cause())
    }

    /// A panicking handler. The middleware must still answer the envelope.
    /// The panic is the point: this route exists only to prove the 5xx
    /// boundary holds when handler code blows up.
    #[allow(clippy::panic)]
    pub async fn panicking_handler() -> Json<HealthResponse> {
        panic!("boom with {}", LEAK_MARKERS[2]);
    }

    /// A raw 500 bypassing `ApiError`. The middleware rewrites it.
    pub async fn raw_500() -> impl IntoResponse {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("raw failure at {} ({})", LEAK_MARKERS[0], LEAK_MARKERS[2]),
        )
    }

    /// A raw 503 with `Retry-After` bypassing `ApiError`. The middleware
    /// keeps the status and header but still rewrites the body. Also
    /// mounted under each compat prefix.
    pub async fn raw_503() -> impl IntoResponse {
        let mut response = (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("raw failure at {} ({})", LEAK_MARKERS[0], LEAK_MARKERS[2]),
        )
            .into_response();
        response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("120"),
        );
        response
    }
}
