//! Identification review routes: list and read reviews, and settle one by
//! choosing a candidate, keeping the album as tagged, dismissing it,
//! taking the match off, taking the files out (and back), or starting
//! identification again.
//!
//! Reading is open to every signed-in user, as before. Choosing, keeping
//! tags, dismissing and detaching are curator work, like other identity
//! decisions. Excluding, restoring and retrying change what the library
//! holds or start a job, so they are admin-only, as in v2.

use crate::service::Identification;
use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::blocking;
use super::models::{
    ApproveBody, CandidateView, IdentityView, PenaltyView, ReasonCount, ReviewBulkPreviewResponse,
    ReviewBulkResponse, ReviewResolveResponse, ReviewRetryAllResponse, ReviewRetryBody, ReviewView,
    ReviewsResponse, SkippedReviews, TrackEvidenceView,
};
use super::operations::{OperationView, job_view};
use crate::identify::models::{AlbumIdentity, CandidateEvidence};
use crate::operations::models::OperationError;
use crate::operations::reasons::{self, review_reason};
use crate::operations::reviews::{
    BulkAction, ReviewAction, ReviewFilter, ReviewOrder, ReviewRow, Tally, parse_state,
};
use crate::operations::service::Operations;
use crate::wiring::LibraryServices;
use droppedneedle_auth::{Admin, Curator, User};

/// Largest review page.
const PAGE_MAX: u32 = 200;

/// Review routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .user(routes!(list_reviews))
        .admin(routes!(retry_pending_reviews))
        .curator(routes!(preview_bulk_reviews))
        .curator(routes!(apply_bulk_reviews))
        .user(routes!(get_review))
        .curator(routes!(approve_review))
        .curator(routes!(reject_review))
        .curator(routes!(dismiss_review))
        .curator(routes!(detach_review))
        .admin(routes!(exclude_review))
        .admin(routes!(restore_review))
        .admin(routes!(retry_review))
}

/// Review list filter and paging. A bulk change takes the same filter.
#[derive(Debug, Clone, Default, Deserialize, IntoParams, ToSchema)]
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
    /// Albums with a file in this library folder.
    pub root_id: Option<String>,
    /// Albums with a file under this folder policy: `automatic`,
    /// `local_metadata` or `excluded`.
    pub policy: Option<String>,
    /// Albums with an identification job in this state: `queued`,
    /// `running`, `deferred`, `succeeded`, `failed` or `attention`.
    pub job_state: Option<String>,
    /// Leave out albums whose identification is still queued or running.
    pub exclude_active_jobs: Option<bool>,
    /// Filed at or after this time (Unix milliseconds).
    pub created_from: Option<i64>,
    /// Filed at or before this time (Unix milliseconds).
    pub created_to: Option<i64>,
    /// Changed at or after this time (Unix milliseconds).
    pub updated_from: Option<i64>,
    /// Changed at or before this time (Unix milliseconds).
    pub updated_to: Option<i64>,
    /// Only reviews that list at least one release.
    pub with_candidates: Option<bool>,
    /// `newest` (default) or `oldest` change first, or `tracks` for the
    /// albums with the most files first.
    pub sort: Option<String>,
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
        track_evidence: Vec::new(),
    }
}

fn evidence_view(row: &ReviewRow, candidate: &CandidateEvidence) -> Vec<TrackEvidenceView> {
    candidate
        .track_evidence
        .iter()
        .map(|evidence| TrackEvidenceView {
            local_track_id: evidence.local_track_id.clone(),
            title: row
                .tracks
                .iter()
                .find(|(id, _)| *id == evidence.local_track_id)
                .map(|(_, title)| title.clone())
                .unwrap_or_default(),
            classification: evidence.classification,
            evidence_kinds: evidence.evidence_kinds.clone(),
        })
        .collect()
}

fn review_view(row: &ReviewRow) -> ReviewView {
    let review = &row.review;
    let mut candidates: Vec<CandidateView> = review
        .candidates
        .iter()
        .map(|candidate| CandidateView {
            track_evidence: evidence_view(row, candidate),
            ..candidate_view(candidate)
        })
        .collect();
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    ReviewView {
        id: review.id.clone(),
        album_id: review.local_album_id.clone(),
        album_title: row.album_title.clone(),
        artist_name: row.artist_name.clone(),
        reason_code: review.reason_code.clone(),
        reason: review_reason(&review.reason_code).into(),
        state: review.state,
        resolved_by: review.resolved_by_user_id.clone(),
        selected_candidate_key: review.selected_candidate_key.clone(),
        candidates,
        created_ms: row.created_ms,
        updated_ms: row.updated_ms,
        manual_identity: row.manual_identity,
    }
}

fn identity_view(identity: &AlbumIdentity) -> IdentityView {
    IdentityView {
        release_mbid: identity.release_mbid.clone(),
        release_group_mbid: identity.release_group_mbid.clone(),
        decision_source: identity.decision_source,
    }
}

/// Read one review back after a change.
async fn settled(ops: Operations, id: String) -> Result<ReviewView, ApiError> {
    let row = blocking(move || ops.review(&id)).await?;
    Ok(review_view(&row))
}

/// Identification reviews, most recently changed first.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/reviews",
    params(ReviewsQuery),
    responses(
        (status = 200, description = "One page of reviews", body = ReviewsResponse),
        (status = 400, description = "Unknown state"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_reviews(
    State(ops): State<Operations>,
    _caller: User,
    ValidQuery(query): ValidQuery<ReviewsQuery>,
) -> Result<Json<ReviewsResponse>, ApiError> {
    let filter = review_filter(query, Vec::new())?;
    let (limit, offset) = (filter.limit, filter.offset);
    let (rows, total, counts) = blocking(move || ops.reviews(&filter)).await?;
    Ok(Json(ReviewsResponse {
        reviews: rows.iter().map(review_view).collect(),
        total,
        reason_counts: counts
            .into_iter()
            .map(|(code, count)| ReasonCount {
                reason: review_reason(&code).into(),
                reason_code: code,
                count,
            })
            .collect(),
        limit,
        offset,
    }))
}

fn known(value: Option<String>, allowed: &[&str]) -> Result<Option<String>, OperationError> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) if !allowed.contains(&value.as_str()) => {
            Err(OperationError::Invalid(reasons::REVIEW_FILTER_INVALID))
        }
        other => Ok(other),
    }
}

/// The store filter for a list query (or a bulk selection).
fn review_filter(
    query: ReviewsQuery,
    review_ids: Vec<String>,
) -> Result<ReviewFilter, OperationError> {
    let state = if query.state.is_none() {
        Some(crate::identify::models::ReviewState::Pending)
    } else {
        parse_state(query.state.as_deref())?
    };
    let order = match query.sort.as_deref() {
        None | Some("") | Some("newest") => ReviewOrder::Newest,
        Some("oldest") => ReviewOrder::Oldest,
        Some("tracks") => ReviewOrder::MostTracks,
        Some(_) => return Err(OperationError::Invalid(reasons::REVIEW_FILTER_INVALID)),
    };
    Ok(ReviewFilter {
        state,
        album_id: query.album_id.filter(|id| !id.is_empty()),
        reason_code: query.reason_code.filter(|code| !code.is_empty()),
        search: query.search,
        review_ids,
        root_ids: query
            .root_id
            .filter(|id| !id.is_empty())
            .into_iter()
            .collect(),
        policy: known(query.policy, &["automatic", "local_metadata", "excluded"])?,
        job_state: known(
            query.job_state,
            &[
                "queued",
                "running",
                "deferred",
                "succeeded",
                "failed",
                "attention",
            ],
        )?,
        exclude_active_jobs: query.exclude_active_jobs.unwrap_or(false),
        created_from: query.created_from,
        created_to: query.created_to,
        updated_from: query.updated_from,
        updated_to: query.updated_to,
        with_candidates: query.with_candidates.unwrap_or(false),
        order,
        limit: query.limit.unwrap_or(50).clamp(1, PAGE_MAX),
        offset: query.offset.unwrap_or(0),
    })
}

/// One review with its candidates.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/reviews/{id}",
    params(("id" = String, Path, description = "Review id")),
    responses(
        (status = 200, description = "The review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown review id"),
    )
)]
pub async fn get_review(
    State(ops): State<Operations>,
    _caller: User,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, ApiError> {
    Ok(Json(settled(ops, id).await?))
}

/// Approve a review with the curator's chosen candidate.
#[utoipa::path(
    post,
    tag = "library management",
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
    State(service): State<Identification>,
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ApproveBody>,
) -> Result<Json<ReviewResolveResponse>, ApiError> {
    if body.candidate_key.is_empty() {
        return Err(ApiError::invalid_input("Candidate key must not be empty"));
    }
    let user_id = caller.user_id().to_owned();
    let review_id = id.clone();
    let (_, identity) =
        blocking(move || service.approve_review(&review_id, &user_id, &body.candidate_key)).await?;
    Ok(Json(ReviewResolveResponse {
        review: settled(ops, id).await?,
        identity: identity.as_ref().map(identity_view),
    }))
}

/// Reject a review: the album keeps its tags, nothing seals.
#[utoipa::path(
    post,
    tag = "library management",
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
    State(service): State<Identification>,
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
) -> Result<Json<ReviewResolveResponse>, ApiError> {
    let user_id = caller.user_id().to_owned();
    let review_id = id.clone();
    blocking(move || service.reject_review(&review_id, &user_id)).await?;
    Ok(Json(ReviewResolveResponse {
        review: settled(ops, id).await?,
        identity: None,
    }))
}

async fn act(
    ops: Operations,
    id: String,
    action: ReviewAction,
    actor: String,
) -> Result<Json<ReviewView>, ApiError> {
    let row = blocking(move || ops.settle_review(&id, action, &actor)).await?;
    Ok(Json(review_view(&row)))
}

/// Close a review without a decision. The album stays as it is.
#[utoipa::path(
    post,
    tag = "library management",
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
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, ApiError> {
    act(ops, id, ReviewAction::Dismiss, caller.user_id().to_owned()).await
}

/// Take the album's MusicBrainz match off and keep its own tags. The
/// album shows as unmatched, and the change can be undone from the album
/// like an edition choice.
#[utoipa::path(
    post,
    tag = "library management",
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
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, ApiError> {
    act(ops, id, ReviewAction::Detach, caller.user_id().to_owned()).await
}

/// Take the album's files out of the library. Nothing is deleted; scans
/// keep the files out until the review is restored.
#[utoipa::path(
    post,
    tag = "library management",
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
    State(ops): State<Operations>,
    caller: Admin,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, ApiError> {
    act(ops, id, ReviewAction::Exclude, caller.user_id().to_owned()).await
}

/// Put an excluded album's files back into the library.
#[utoipa::path(
    post,
    tag = "library management",
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
    State(ops): State<Operations>,
    caller: Admin,
    Path(id): Path<String>,
) -> Result<Json<ReviewView>, ApiError> {
    act(ops, id, ReviewAction::Restore, caller.user_id().to_owned()).await
}

/// Start identification again for the review's album. The new job
/// carries the decision, so the review is dismissed.
#[utoipa::path(
    post,
    tag = "library management",
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
    State(ops): State<Operations>,
    caller: Admin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ReviewRetryBody>,
) -> Result<Json<OperationView>, ApiError> {
    let actor = caller.user_id().to_owned();
    let key = body.idempotency_key.filter(|key| !key.trim().is_empty());
    let job =
        blocking(move || ops.retry_review(&id, &actor, key, body.confirm_local_metadata)).await?;
    Ok(Json(job_view(&job)))
}

/// Start identification again for every pending review a filter selects
/// (all of them by default), or with `preview` count what would start.
/// Reviews whose album cannot start stay open and are counted by reason.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/retry-pending",
    request_body = ReviewRetryAllBody,
    responses(
        (status = 200, description = "What started and what did not", body = ReviewRetryAllResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn retry_pending_reviews(
    State(ops): State<Operations>,
    caller: Admin,
    ValidJson(body): ValidJson<ReviewRetryAllBody>,
) -> Result<Json<ReviewRetryAllResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let mut filter = review_filter(body.filter, Vec::new())?;
    if !body.root_ids.is_empty() {
        filter.root_ids = body.root_ids;
    }
    let preview = body.preview;
    let confirm = body.confirm_local_metadata;
    let outcome =
        blocking(move || ops.retry_pending_reviews(&actor, confirm, &filter, preview)).await?;
    Ok(Json(ReviewRetryAllResponse {
        preview,
        started: outcome.done,
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

/// Retry identification for many reviews.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ReviewRetryAllBody {
    /// Confirms a one-off MusicBrainz lookup for files under a Local
    /// metadata policy.
    #[serde(default)]
    pub confirm_local_metadata: bool,
    /// Pending reviews matching this list filter (paging is ignored).
    #[serde(default)]
    pub filter: ReviewsQuery,
    /// Albums in these library folders (overrides the filter's folder).
    #[serde(default)]
    pub root_ids: Vec<String>,
    /// Count what would start and why the rest would not; start nothing.
    #[serde(default)]
    pub preview: bool,
}

/// A bulk review change: what to do, and to which reviews.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReviewBulkBody {
    /// `keep_tagged`, `exclude` or `accept`.
    pub action: String,
    /// Reviews picked by hand. Combined with `filter` when both are given.
    #[serde(default)]
    pub review_ids: Vec<String>,
    /// Reviews matching a list filter (paging is ignored).
    #[serde(default)]
    pub filter: ReviewsQuery,
    /// Accept only: the release to use. Each review's best release when
    /// empty.
    #[serde(default)]
    pub candidate_key: Option<String>,
    /// Apply only: the token the preview returned.
    #[serde(default)]
    pub preview_token: Option<String>,
}

fn bulk_action(raw: &str) -> Result<BulkAction, OperationError> {
    match raw {
        "keep_tagged" => Ok(BulkAction::KeepTagged),
        "exclude" => Ok(BulkAction::Exclude),
        "accept" => Ok(BulkAction::Accept),
        _ => Err(OperationError::Invalid(reasons::REVIEW_BULK_ACTION_INVALID)),
    }
}

fn skipped_view(skipped: Vec<(reasons::Reason, u32)>) -> Vec<SkippedReviews> {
    skipped
        .into_iter()
        .map(|(reason, count)| SkippedReviews {
            reason: reason.into(),
            count,
        })
        .collect()
}

/// Parse a bulk body; taking files out of the library is admin-only.
fn bulk_request(
    body: ReviewBulkBody,
    caller: &Curator,
) -> Result<(BulkAction, ReviewFilter, Option<String>, Option<String>), ApiError> {
    let action = bulk_action(&body.action)?;
    if action == BulkAction::Exclude {
        Admin::try_from(caller.principal().clone())?;
    }
    let filter = review_filter(body.filter, body.review_ids)?;
    let key = body.candidate_key.filter(|key| !key.is_empty());
    Ok((action, filter, key, body.preview_token))
}

/// Preview a bulk change: how many reviews it changes, which it skips and
/// why, and the token that applies exactly this change.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/bulk-preview",
    request_body = ReviewBulkBody,
    responses(
        (status = 200, description = "What the change would do", body = ReviewBulkPreviewResponse),
        (status = 400, description = "Unknown action or filter value"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required (admin to exclude)"),
    )
)]
pub async fn preview_bulk_reviews(
    State(ops): State<Operations>,
    caller: Curator,
    ValidJson(body): ValidJson<ReviewBulkBody>,
) -> Result<Json<ReviewBulkPreviewResponse>, ApiError> {
    let (action, filter, key, _) = bulk_request(body, &caller)?;
    let plan = blocking(move || ops.preview_bulk_reviews(action, &filter, key.as_deref())).await?;
    Ok(Json(ReviewBulkPreviewResponse {
        matched: plan.matched,
        eligible: u32::try_from(plan.items.len()).unwrap_or(u32::MAX),
        skipped: skipped_view(plan.skipped),
        preview_token: plan.token,
    }))
}

/// Apply a previewed bulk change. Refused when the selection changed
/// since the preview.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/bulk-apply",
    request_body = ReviewBulkBody,
    responses(
        (status = 200, description = "What changed and what did not", body = ReviewBulkResponse),
        (status = 400, description = "Unknown action or filter value, or no preview token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required (admin to exclude)"),
        (status = 409, description = "The selection changed since the preview"),
    )
)]
pub async fn apply_bulk_reviews(
    State(ops): State<Operations>,
    caller: Curator,
    ValidJson(body): ValidJson<ReviewBulkBody>,
) -> Result<Json<ReviewBulkResponse>, ApiError> {
    let actor = caller.user_id().to_owned();
    let (action, filter, key, token) = bulk_request(body, &caller)?;
    let Some(token) = token.filter(|token| !token.is_empty()) else {
        return Err(ApiError::invalid_input("Preview the change first"));
    };
    let Tally { done, skipped } =
        blocking(move || ops.apply_bulk_reviews(action, &filter, key.as_deref(), &token, &actor))
            .await?;
    Ok(Json(ReviewBulkResponse {
        applied: done,
        skipped: skipped_view(skipped),
    }))
}
