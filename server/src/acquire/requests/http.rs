//! Handler-layer mapping for requests errors, plus the body and query
//! extractors that keep malformed input inside the shared envelope.

use axum::{
    Json,
    extract::{FromRequest, Query, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

use super::error::{QUOTA_EXCEEDED, RequestsError, STORAGE_FULL};
use crate::error::{
    ApiError, CONFLICT, FORBIDDEN, INVALID_INPUT, NOT_FOUND, NOT_FOUND_MESSAGE, busy_response,
    envelope_response, unauthorized_response,
};

/// A requests error on its way out of a handler.
#[derive(Debug)]
pub struct HttpError(pub RequestsError);

impl From<RequestsError> for HttpError {
    fn from(error: RequestsError) -> Self {
        Self(error)
    }
}

/// Status and shared envelope for each requests error. 5xx bodies are
/// fixed; the cause stays in the log.
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        match self.0 {
            RequestsError::Unauthorized { message } => unauthorized_response(message),
            RequestsError::Forbidden { message } => {
                envelope_response(StatusCode::FORBIDDEN, FORBIDDEN, message, None)
            }
            RequestsError::NotFound => {
                envelope_response(StatusCode::NOT_FOUND, NOT_FOUND, NOT_FOUND_MESSAGE, None)
            }
            RequestsError::InvalidInput { message } => {
                envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
            }
            RequestsError::Conflict { message } => {
                envelope_response(StatusCode::CONFLICT, CONFLICT, message, None)
            }
            RequestsError::QuotaExceeded { message, details } => envelope_response(
                StatusCode::TOO_MANY_REQUESTS,
                QUOTA_EXCEEDED,
                message,
                Some(details),
            ),
            RequestsError::StorageFull { message, details } => {
                envelope_response(StatusCode::FORBIDDEN, STORAGE_FULL, message, Some(details))
            }
            RequestsError::Busy { operation } => {
                busy_response(&operation, &uuid::Uuid::new_v4().to_string())
            }
            RequestsError::Internal { error_id } => ApiError::internal_response(&error_id),
        }
    }
}

/// JSON body extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = HttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| {
                HttpError(RequestsError::InvalidInput {
                    message: format!("Invalid request body: {cause}"),
                })
            })
    }
}

/// Query extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400. All query access goes through
/// this; no bare `Query` appears in handlers.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = HttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| {
                HttpError(RequestsError::InvalidInput {
                    message: format!("Invalid query string: {cause}"),
                })
            })
    }
}
