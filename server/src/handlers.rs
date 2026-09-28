//! Thin Axum handlers. Each handler answers one route and returns typed
//! errors; status mapping lives in `error::ApiError`.

use axum::{Extension, Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use utoipa::{OpenApi as _, ToSchema};

use crate::{docs::ApiDoc, error::ApiError, ids::RequestId};

/// Health payload. Shape kept from v2: `status` plus the running message.
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
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Unknown routes render the shared 404 envelope, never an empty body.
pub async fn fallback_404() -> ApiError {
    ApiError::NotFound
}

/// Known route, wrong method: the shared 405 envelope, never an empty body.
pub async fn fallback_405() -> ApiError {
    ApiError::MethodNotAllowed
}

/// Failure hooks for the 5xx-leak briefs. Mounted only when the state opts
/// in; the production binary never does.
pub mod test_hooks {
    use super::*;

    /// Marker substrings that must never reach a response body. Deliberately
    /// realistic: a filesystem path, an internal host, and a secret token.
    pub const LEAK_MARKERS: &[&str] = &[
        "/srv/secrets/droppedneedle.key",
        "postgres.internal.example.com",
        "dn-secret-marker-9f31",
    ];

    /// Typed server fault carrying all leak markers in its cause.
    pub async fn typed_error(Extension(request_id): Extension<RequestId>) -> ApiError {
        let cause = format!("boom reading {} via {}", LEAK_MARKERS[0], LEAK_MARKERS[1]);
        ApiError::internal(&cause, &request_id.0)
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
    /// keeps the status and header but still rewrites the body.
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
