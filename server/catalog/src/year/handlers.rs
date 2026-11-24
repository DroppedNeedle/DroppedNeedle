//! The year in review's two routes, for the caller's own year only.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidQuery;
use droppedneedle_kernel::mount::{Routes, routes};

use super::models::{YearReview, YearReviewQuery, YearReviewYears};
use super::{YearError, YearReviews};

impl From<YearError> for ApiError {
    fn from(error: YearError) -> Self {
        match error {
            YearError::NotFound => ApiError::new(
                StatusCode::NOT_FOUND,
                "YEAR_NOT_FOUND",
                "There is no review for that year.",
            ),
            YearError::Busy(operation) => ApiError::unavailable(Unavailable::ServerBusy, operation),
            YearError::Internal(cause) => ApiError::internal(cause),
        }
    }
}

/// The years the caller can look back on: the current one, then each
/// earlier year with a play, newest first.
#[utoipa::path(
    get,
    tag = "year",
    path = "/api/v3/me/year-in-review",
    operation_id = "me_list_year_reviews",
    params(
        ("zone" = Option<String>, Query, description = "IANA time zone to count in; the server's when missing or unknown"),
    ),
    responses(
        (status = 200, description = "The caller's years", body = YearReviewYears),
        (status = 400, description = "The zone is not a time zone name"),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn list_year_reviews(
    State(reviews): State<YearReviews>,
    User(user): User,
    ValidQuery(query): ValidQuery<YearReviewQuery>,
) -> Result<Json<YearReviewYears>, ApiError> {
    reviews
        .years(user.user_id(), query.zone.as_ref())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The caller's review of one year.
#[utoipa::path(
    get,
    tag = "year",
    path = "/api/v3/me/year-in-review/{year}",
    operation_id = "me_get_year_review",
    params(
        ("year" = String, Path, description = "Calendar year, 1 to 9999; anything else answers 404"),
        ("zone" = Option<String>, Query, description = "IANA time zone to count in; the server's when missing or unknown"),
    ),
    responses(
        (status = 200, description = "The caller's year", body = YearReview),
        (status = 400, description = "The zone is not a time zone name"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No review for that year (YEAR_NOT_FOUND)"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn get_year_review(
    State(reviews): State<YearReviews>,
    User(user): User,
    Path(year): Path<String>,
    ValidQuery(query): ValidQuery<YearReviewQuery>,
) -> Result<Json<YearReview>, ApiError> {
    reviews
        .review(user.user_id(), &year, query.zone.as_ref())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The year in review routes, for any signed-in person.
pub fn routes() -> Routes<YearReviews> {
    Routes::new()
        .user(routes!(list_year_reviews))
        .user(routes!(get_year_review))
}
