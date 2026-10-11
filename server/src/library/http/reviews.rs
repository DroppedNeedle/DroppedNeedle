//! Identification review routes: list and read reviews, and settle one by
//! choosing a candidate, keeping the album as tagged, dismissing it,
//! taking the match off, taking the files out (and back), or starting
//! identification again.
//!
//! Reading is open to every signed-in user, as before. Choosing, keeping
//! tags, dismissing and detaching are curator work, like other identity
//! decisions. Excluding, restoring and retrying change what the library
//! holds or start a job, so they are admin-only, as in v2.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::auth::{Principal, RequireAdmin, RequireCurator};
use super::error::{LibraryError, ValidJson, ValidQuery};
use super::models::{
    ApproveBody, CandidateView, IdentityView, PenaltyView, ReviewResolveResponse,
    ReviewRetryAllResponse, ReviewRetryBody, ReviewView, ReviewsResponse, SkippedReviews, snake,
};
use super::operations::{OperationView, OperationsHttpError, blocking, job_view};
use crate::library::identify::models::{AlbumIdentity, CandidateEvidence};
use crate::library::operations::reasons::review_reason;
use crate::library::operations::reviews::{
    ReviewAction, ReviewFilter, ReviewRow, parse_state, state_name,
};
use crate::library::operations::service::Operations;
use crate::library::wiring::LibrarySetup;

/// Largest review page.
const PAGE_MAX: u32 = 200;

/// Review routes, relative for nesting under `/api/v3`.
pub fn reviews_router() -> Router<LibrarySetup> {
    Router::new()
        .route("/library/reviews", get(list_reviews))
        .route(
            "/library/reviews/retry-pending",
            post(retry_pending_reviews),
        )
        .route("/library/reviews/{id}", get(get_review))
        .route("/library/reviews/{id}/approve", post(approve_review))
        .route("/library/reviews/{id}/reject", post(reject_review))
        .route("/library/reviews/{id}/dismiss", post(dismiss_review))
        .route("/library/reviews/{id}/detach", post(detach_review))
        .route("/library/reviews/{id}/exclude", post(exclude_review))
        .route("/library/reviews/{id}/restore", post(restore_review))
        .route("/library/reviews/{id}/retry", post(retry_review))
}

/// Review list filter and paging.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReviewsQuery {
    /// `pending` (default), `approved`, `rejected`, `dismissed`,
    /// `excluded` or `all`.
    pub state: Option<String>,
    /// Only this album's reviews.
    pub album_id: Option<String>,
    /// Only reviews filed with this reason code.
    pub reason_code: Option<String>,
    /// Album title or artist contains this, ignoring case.
    pub search: Option<String>,
    /// Page size, 1 to 200 (default 50).
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

fn candidate_view(candidate: &CandidateEvidence) -> CandidateView {
    CandidateView {
        candidate_key: candidate.candidate_key.clone(),
        release_group_mbid: candidate.release_group_mbid.clone(),
        release_mbid: candidate.release_mbid.clone(),
        album_title: candidate.album_title.clone(),
        album_artist_name: candidate.album_artist_name.clone(),
        score: candidate.score,
        distance: candidate.distance,
        penalties: candidate
            .penalties
            .iter()
            .map(|penalty| PenaltyView {
                name: penalty.name.clone(),
                share: penalty.share,
            })
            .collect(),
        reason_code: candidate.reason_code.clone(),
        supported_tracks: candidate.supported_count(),
        contradictory_tracks: candidate.contradictory_count(),
    }
}

fn review_view(row: &ReviewRow) -> ReviewView {
    let review = &row.review;
    let mut candidates: Vec<CandidateView> = review.candidates.iter().map(candidate_view).collect();
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    ReviewView {
        id: review.id.clone(),
        album_id: review.local_album_id.clone(),
        album_title: row.album_title.clone(),
        artist_name: row.artist_name.clone(),
        reason_code: review.reason_code.clone(),
        reason: review_reason(&review.reason_code).into(),
        state: state_name(review.state).to_owned(),
        resolved_by: review.resolved_by_user_id.clone(),
        selected_candidate_key: review.selected_candidate_key.clone(),
        candidates,
        created_ms: row.created_ms,
        updated_ms: row.updated_ms,
    }
}

fn identity_view(identity: &AlbumIdentity) -> IdentityView {
    IdentityView {
        release_mbid: identity.release_mbid.clone(),
        release_group_mbid: identity.release_group_mbid.clone(),
        decision_source: snake(&identity.decision_source),
    }
}

/// Read one review back after a change.
async fn settled(state: LibrarySetup, id: String) -> Result<ReviewView, OperationsHttpError> {
    let ops = Operations::new(&state);
    let row = blocking(move || ops.review(&id)).await?;
    Ok(review_view(&row))
}

/// Identification reviews, most recently changed first.
#[utoipa::path(
    get,
    path = "/api/v3/library/reviews",
    params(ReviewsQuery),
    responses(
        (status = 200, description = "One page of reviews", body = ReviewsResponse),
        (status = 400, description = "Unknown state"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_reviews(
    State(state): State<LibrarySetup>,
    _caller: Principal,
    ValidQuery(query): ValidQuery<ReviewsQuery>,
) -> Result<Json<ReviewsResponse>, OperationsHttpError> {
    let review_state = if query.state.is_none() {
        Some(crate::library::identify::models::ReviewState::Pending)
    } else {
        parse_state(query.state.as_deref())?
    };
    let limit = query.limit.unwrap_or(50).clamp(1, PAGE_MAX);
    let offset = query.offset.unwrap_or(0);
    let filter = ReviewFilter {
        state: review_state,
        album_id: query.album_id.filter(|id| !id.is_empty()),
        reason_code: query.reason_code.filter(|code| !code.is_empty()),
        search: query.search,
        limit,
        offset,
    };
    let ops = Operations::new(&state);
    let (rows, total) = blocking(move || ops.reviews(&filter)).await?;
    Ok(Json(ReviewsResponse {
        reviews: rows.iter().map(review_view).collect(),
        total,
        limit,
        offset,
    }))
}

/// One review with its candidates.
#[utoipa::path(
    get,
    path = "/api/v3/library/reviews/{id}",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "The review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown review id"),
    )
)]
pub async fn get_review(
    State(state): State<LibrarySetup>,
    _caller: Principal,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, OperationsHttpError> {
    Ok(Json(settled(state, id).await?))
}

/// Approve a review with the curator's chosen candidate.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/approve",
    params(("id" = String, Path, description = "Review id")),
    request_body = ApproveBody,
    responses(
        (status = 200, description = "Settled review", body = ReviewResolveResponse),
        (status = 400, description = "Missing candidate key"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review or candidate"),
        (status = 409, description = "Review already settled"),
    )
)]
pub async fn approve_review(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ApproveBody>,
) -> Result<Json<ReviewResolveResponse>, OperationsHttpError> {
    if body.candidate_key.is_empty() {
        return Err(LibraryError::InvalidInput {
            message: "Candidate key must not be empty".to_owned(),
        }
        .into());
    }
    let user_id = caller.0.user_id;
    let service = state.clone();
    let review_id = id.clone();
    let (_, identity) = tokio::task::spawn_blocking(move || {
        service.approve_review(&review_id, &user_id, &body.candidate_key)
    })
    .await
    .map_err(|cause| LibraryError::internal(&cause))?
    .map_err(LibraryError::from)?;
    Ok(Json(ReviewResolveResponse {
        review: settled(state, id).await?,
        identity: identity.as_ref().map(identity_view),
    }))
}

/// Reject a review: the album keeps its tags, nothing seals.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/reject",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "Settled review", body = ReviewResolveResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled"),
    )
)]
pub async fn reject_review(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(id): Path<String>,
) -> Result<Json<ReviewResolveResponse>, OperationsHttpError> {
    let user_id = caller.0.user_id;
    let service = state.clone();
    let review_id = id.clone();
    tokio::task::spawn_blocking(move || service.reject_review(&review_id, &user_id))
        .await
        .map_err(|cause| LibraryError::internal(&cause))?
        .map_err(LibraryError::from)?;
    Ok(Json(ReviewResolveResponse {
        review: settled(state, id).await?,
        identity: None,
    }))
}

async fn act(
    state: LibrarySetup,
    id: String,
    action: ReviewAction,
    actor: String,
) -> Result<Json<ReviewView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let row = blocking(move || ops.settle_review(&id, action, &actor)).await?;
    Ok(Json(review_view(&row)))
}

/// Close a review without a decision. The album stays as it is.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/dismiss",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled"),
    )
)]
pub async fn dismiss_review(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, OperationsHttpError> {
    act(state, id, ReviewAction::Dismiss, caller.0.user_id).await
}

/// Take the album's MusicBrainz match off and keep its own tags. The
/// album shows as unmatched, and the change can be undone from the album
/// like an edition choice.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/detach",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled"),
    )
)]
pub async fn detach_review(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, OperationsHttpError> {
    act(state, id, ReviewAction::Detach, caller.0.user_id).await
}

/// Take the album's files out of the library. Nothing is deleted; scans
/// keep the files out until the review is restored.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/exclude",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled"),
    )
)]
pub async fn exclude_review(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, OperationsHttpError> {
    act(state, id, ReviewAction::Exclude, caller.0.user_id).await
}

/// Put an excluded album's files back into the library.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/restore",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "The review's album was not excluded"),
    )
)]
pub async fn restore_review(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, OperationsHttpError> {
    act(state, id, ReviewAction::Restore, caller.0.user_id).await
}

/// Start identification again for the review's album. The new job
/// carries the decision, so the review is dismissed.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/{id}/retry",
    params(("id" = String, Path, description = "Review id")),
    request_body = ReviewRetryBody,
    responses(
        (status = 200, description = "Re-identification job", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown review id, or the album has no files"),
        (status = 409, description = "Review already settled, album excluded, or needs the Local metadata confirmation"),
    )
)]
pub async fn retry_review(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ReviewRetryBody>,
) -> Result<Json<OperationView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let actor = caller.0.user_id;
    let key = body.idempotency_key.filter(|key| !key.trim().is_empty());
    let job =
        blocking(move || ops.retry_review(&id, &actor, key, body.confirm_local_metadata)).await?;
    Ok(Json(job_view(&job)))
}

/// Start identification again for every album with a pending review.
/// Reviews whose album cannot start stay open and are counted by reason.
#[utoipa::path(
    post,
    path = "/api/v3/library/reviews/retry-pending",
    request_body = ReviewRetryBody,
    responses(
        (status = 200, description = "What started and what did not", body = ReviewRetryAllResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn retry_pending_reviews(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<ReviewRetryBody>,
) -> Result<Json<ReviewRetryAllResponse>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let actor = caller.0.user_id;
    let outcome =
        blocking(move || ops.retry_pending_reviews(&actor, body.confirm_local_metadata)).await?;
    Ok(Json(ReviewRetryAllResponse {
        started: outcome.started,
        skipped: outcome
            .skipped
            .into_iter()
            .map(|(reason, count)| SkippedReviews {
                reason: reason.into(),
                count,
            })
            .collect(),
    }))
}
