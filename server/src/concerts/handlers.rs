//! Thin handlers for the six concerts routes, and the one place a concerts
//! failure meets a status code.
//!
//! Every route is a plain signed-in-user route; the session middleware has
//! already turned anonymous callers away, so the caller extractor only
//! reads the stashed session.

use axum::{
    Json, Router,
    extract::{FromRequest, FromRequestParts, Query, Request, State},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::de::DeserializeOwned;

use super::models::{
    CitySearchQuery, CitySearchResponse, ConcertsResponse, EventCitiesResponse, EventCitiesUpdate,
};
use super::service::{ConcertsError, ConcertsService};
use crate::auth::session::middleware::CurrentSession;
use crate::error::{
    FIXED_INTERNAL_MESSAGE, FIXED_UPSTREAM_MESSAGE, INTERNAL_ERROR, INVALID_INPUT, UPSTREAM_ERROR,
    busy_response, envelope_response, unauthorized_response,
};
use crate::reads::collections::models::UnseenCountResponse;

/// A concerts failure on its way to the wire.
#[derive(Debug)]
pub struct ConcertsHttpError(ConcertsError);

impl From<ConcertsError> for ConcertsHttpError {
    fn from(error: ConcertsError) -> Self {
        Self(error)
    }
}

/// No session on the request.
#[derive(Debug)]
pub struct Unauthenticated;

impl IntoResponse for Unauthenticated {
    fn into_response(self) -> Response {
        unauthorized_response("Authentication required")
    }
}

impl IntoResponse for ConcertsHttpError {
    fn into_response(self) -> Response {
        match self.0 {
            ConcertsError::InvalidInput(message) => {
                envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
            }
            ConcertsError::SearchUnavailable(cause) => {
                tracing::warn!(%cause, "city search failed");
                envelope_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    UPSTREAM_ERROR,
                    FIXED_UPSTREAM_MESSAGE,
                    None,
                )
            }
            ConcertsError::Busy(operation) => {
                let error_id = uuid::Uuid::new_v4().to_string();
                busy_response(&operation, &error_id)
            }
            ConcertsError::Internal(cause) => {
                tracing::error!(%cause, "concerts request failed");
                envelope_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    INTERNAL_ERROR,
                    FIXED_INTERNAL_MESSAGE,
                    None,
                )
            }
        }
    }
}

type HttpResult<T> = Result<Json<T>, ConcertsHttpError>;

/// The signed-in caller.
pub struct Caller(String);

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = Unauthenticated;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentSession>()
            .map(|session| Self(session.user_id.clone()))
            .ok_or(Unauthenticated)
    }
}

/// JSON body extractor keeping bad input in the shared envelope.
pub struct ValidJson<T>(T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = ConcertsHttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| {
                ConcertsError::InvalidInput(format!("Invalid request body: {cause}")).into()
            })
    }
}

/// Query extractor keeping bad input in the shared envelope.
pub struct ValidQuery<T>(T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for ValidQuery<T> {
    type Rejection = ConcertsHttpError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| {
                ConcertsError::InvalidInput(format!("Invalid query string: {cause}")).into()
            })
    }
}

/// The concerts routes, relative to `/api/v3`.
pub fn router(service: ConcertsService) -> Router {
    Router::new()
        .route("/following/concerts", get(list_concerts))
        .route(
            "/following/concerts/cities",
            get(list_event_cities).put(replace_event_cities),
        )
        .route("/following/concerts/city-search", get(search_cities))
        .route("/following/concerts/unseen-count", get(unseen_concerts))
        .route("/following/concerts/seen", post(mark_concerts_seen))
        .with_state(service)
}

/// Upcoming gigs for the caller's followed artists in their saved cities,
/// date ascending. `configured` is false when no events source is on.
#[utoipa::path(
    get,
    path = "/api/v3/following/concerts",
    tag = "concerts",
    responses(
        (status = 200, description = "The caller's concerts", body = ConcertsResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_concerts(
    State(service): State<ConcertsService>,
    Caller(user_id): Caller,
) -> HttpResult<ConcertsResponse> {
    Ok(Json(service.list(&user_id).await?))
}

/// The caller's saved cities in picker order.
#[utoipa::path(
    get,
    path = "/api/v3/following/concerts/cities",
    tag = "concerts",
    responses(
        (status = 200, description = "Saved cities", body = EventCitiesResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_event_cities(
    State(service): State<ConcertsService>,
    Caller(user_id): Caller,
) -> HttpResult<EventCitiesResponse> {
    Ok(Json(service.cities(&user_id).await?))
}

/// Replace the caller's cities with the submitted list, in order.
#[utoipa::path(
    put,
    path = "/api/v3/following/concerts/cities",
    tag = "concerts",
    request_body = EventCitiesUpdate,
    responses(
        (status = 200, description = "Saved cities", body = EventCitiesResponse),
        (status = 400, description = "Coordinates out of range or malformed body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn replace_event_cities(
    State(service): State<ConcertsService>,
    Caller(user_id): Caller,
    ValidJson(body): ValidJson<EventCitiesUpdate>,
) -> HttpResult<EventCitiesResponse> {
    Ok(Json(service.replace_cities(&user_id, body.items).await?))
}

/// City suggestions for the picker. An empty list means no such place; a
/// geocoder failure answers 503.
#[utoipa::path(
    get,
    path = "/api/v3/following/concerts/city-search",
    tag = "concerts",
    params(CitySearchQuery),
    responses(
        (status = 200, description = "Suggestions", body = CitySearchResponse),
        (status = 400, description = "Query missing or not 2-100 characters"),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "Geocoder unavailable"),
    )
)]
pub async fn search_cities(
    State(service): State<ConcertsService>,
    Caller(_user_id): Caller,
    ValidQuery(query): ValidQuery<CitySearchQuery>,
) -> HttpResult<CitySearchResponse> {
    Ok(Json(service.search_cities(&query.q).await?))
}

/// Gigs in the caller's cities first seen since they last opened the page.
#[utoipa::path(
    get,
    path = "/api/v3/following/concerts/unseen-count",
    tag = "concerts",
    responses(
        (status = 200, description = "Unseen count", body = UnseenCountResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn unseen_concerts(
    State(service): State<ConcertsService>,
    Caller(user_id): Caller,
) -> HttpResult<UnseenCountResponse> {
    Ok(Json(UnseenCountResponse {
        count: service.unseen_count(&user_id).await?,
    }))
}

/// Mark the caller's concerts seen. The count is zero afterwards.
#[utoipa::path(
    post,
    path = "/api/v3/following/concerts/seen",
    tag = "concerts",
    responses(
        (status = 200, description = "Zeroed count", body = UnseenCountResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn mark_concerts_seen(
    State(service): State<ConcertsService>,
    Caller(user_id): Caller,
) -> HttpResult<UnseenCountResponse> {
    service.mark_seen(&user_id).await?;
    Ok(Json(UnseenCountResponse { count: 0 }))
}
