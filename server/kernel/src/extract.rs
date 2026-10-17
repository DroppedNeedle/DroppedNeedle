//! Body and query extractors that answer in the shared error envelope
//! instead of axum's plain-text rejections.

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Query, Request},
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// A JSON body. A malformed one is 400 `INVALID_INPUT`, an oversized one
/// 413 `PAYLOAD_TOO_LARGE`.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(cause) if cause.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                Err(ApiError::too_large())
            }
            Err(cause) => Err(ApiError::invalid_input(format!(
                "Invalid request body: {cause}"
            ))),
        }
    }
}

/// A query string. A malformed one is 400 `INVALID_INPUT`. It reads only
/// the request parts, so it may sit before a body extractor.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for ValidQuery<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| ApiError::invalid_input(format!("Invalid query string: {cause}")))
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, response::IntoResponse as _};

    use super::*;

    async fn reject(body: Vec<u8>) -> axum::response::Response {
        let request = Request::put("/")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        match ValidJson::<serde_json::Value>::from_request(request, &()).await {
            Ok(_) => panic!("the body was accepted"),
            Err(error) => error.into_response(),
        }
    }

    #[tokio::test]
    async fn rejections_answer_in_the_envelope_and_oversize_is_413() {
        // axum's default body limit is 2 MiB.
        let oversized = format!("\"{}\"", "a".repeat(2 * 1024 * 1024));
        let response = reject(oversized.into_bytes()).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let response = reject(b"{".to_vec()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "INVALID_INPUT");
        let message = json["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("Invalid request body: "), "{message}");
    }
}
