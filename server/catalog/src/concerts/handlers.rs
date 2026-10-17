//! Thin handlers for the six concerts routes, and the one place a concerts
//! failure meets a status code.
//!
//! Every route is a plain signed-in-user route; the session middleware has
//! already turned anonymous callers away, so the caller extractor only
//! reads the stashed session.

use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{StatusCode, request::Parts},
    routing::{get, post},
};

use super::models::{
    CitySearchQuery, CitySearchResponse, ConcertsResponse, EventCitiesResponse, EventCitiesUpdate,
};
use super::service::{ConcertsError, ConcertsService};
use crate::reads::collections::models::UnseenCountResponse;
use droppedneedle_auth::session::middleware::CurrentSession;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

impl From<ConcertsError> for ApiError {
    fn from(error: ConcertsError) -> Self {
        match error {
            ConcertsError::InvalidInput(message) => ApiError::invalid_input(message),
            ConcertsError::SearchUnavailable(cause) => ApiError::fault(
                StatusCode::SERVICE_UNAVAILABLE,
                format_args!("city search failed: {cause}"),
            ),
            ConcertsError::Busy(operation) => {
                ApiError::unavailable(Unavailable::ServerBusy, operation)
            }
            ConcertsError::Internal(cause) => ApiError::internal(cause),
        }
    }
}

type HttpResult<T> = Result<Json<T>, ApiError>;

/// The signed-in caller.
pub struct Caller(String);

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentSession>()
            .map(|session| Self(session.user_id.clone()))
            .ok_or_else(|| ApiError::unauthorized("Authentication required"))
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
