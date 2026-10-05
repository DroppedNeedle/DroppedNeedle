//! HTTP mapping for the collections routes.
//!
//! [`CollectionsHttpError`] is the only place a collections failure meets a
//! status code; bodies come from the shared renderers in `crate::error`.
//! The body extractors keep malformed input inside the same envelope
//! instead of Axum's plain-text 400.

use axum::{
    Json,
    extract::{FromRequest, Query, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

use super::error::CollectionsError;
use crate::error::{
    CONFLICT, FIXED_INTERNAL_MESSAGE, FORBIDDEN, INTERNAL_ERROR, INVALID_INPUT, NOT_FOUND,
    NOT_FOUND_MESSAGE, busy_response, envelope_response, fault_response, unauthorized_response,
};

/// A collections failure on its way to the wire.
#[derive(Debug)]
pub struct CollectionsHttpError(pub CollectionsError);

impl From<CollectionsError> for CollectionsHttpError {
    fn from(error: CollectionsError) -> Self {
        Self(error)
    }
}

impl IntoResponse for CollectionsHttpError {
    fn into_response(self) -> Response {
        match self.0 {
            CollectionsError::Unauthorized { message } => unauthorized_response(message),
            CollectionsError::Forbidden { message } => {
                envelope_response(StatusCode::FORBIDDEN, FORBIDDEN, message, None)
            }
            CollectionsError::NotFound => {
                envelope_response(StatusCode::NOT_FOUND, NOT_FOUND, NOT_FOUND_MESSAGE, None)
            }
            CollectionsError::InvalidInput { message } => {
                envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
            }
            CollectionsError::Conflict { message } => {
                envelope_response(StatusCode::CONFLICT, CONFLICT, message, None)
            }
            CollectionsError::Busy { error_id } => busy_response("collections", &error_id),
            CollectionsError::Internal { error_id } => fault_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR,
                FIXED_INTERNAL_MESSAGE,
                &error_id,
            ),
        }
    }
}

/// Handler result type.
pub type HttpResult<T> = Result<T, CollectionsHttpError>;

/// JSON body extractor rendering bad input in the shared envelope.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = CollectionsHttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| {
                CollectionsHttpError(CollectionsError::InvalidInput {
                    message: format!("Invalid request body: {cause}"),
                })
            })
    }
}

/// Query extractor rendering bad input in the shared envelope.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = CollectionsHttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| {
                CollectionsHttpError(CollectionsError::InvalidInput {
                    message: format!("Invalid query string: {cause}"),
                })
            })
    }
}
