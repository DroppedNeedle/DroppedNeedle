//! Identification review routes: list and read reviews, and settle one by
//! choosing a candidate, keeping the album as tagged, dismissing it,
//! taking the match off, taking the files out (and back), or starting
//! identification again. Each action sends the review's `row_revision`
//! as the caller read it, and a review that changed since is refused with
//! `STALE_REVISION`. Bulk changes are previewed first and applied with
//! the preview's token, which only the person who previewed can use.
//!
//! Reading is open to every signed-in user, as before. Choosing, keeping
//! tags, dismissing and detaching are curator work, like other identity
//! decisions. Excluding, restoring and retrying change what the library
//! holds or start a job, so they are admin-only, as in v2.

use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::page::{Page, PageRequest};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::blocking;
use super::models::{
    ApproveBody, CandidateView, IdentityView, PenaltyView, ReasonCount, ReviewActionBody,
    ReviewBulkPreviewResponse, ReviewResolveResponse, ReviewRetryBody, ReviewStateCounts,
    ReviewView, ReviewsResponse, SharedCandidateView, SkippedReviews, TrackEvidenceView,
};
use super::operations::{OperationView, job_view};
use crate::identify::models::{AlbumIdentity, CandidateEvidence, ReviewState};
use crate::operations::models::{Control, OperationError};
use crate::operations::reasons::{self, review_reason};
use crate::operations::reviews::{
    BulkAction, FolderScope, ReviewAction, ReviewFilter, ReviewJobAction, ReviewOrder, ReviewRow,
    parse_state,
};
use crate::operations::service::{Operations, ReviewJob};
use crate::wiring::LibraryServices;
use droppedneedle_auth::{Admin, Curator, User};

const PAGE_DEFAULT: usize = 50;
/// Largest review page.
const PAGE_MAX: usize = 200;

/// Review routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .user(routes!(list_reviews))
        .admin(routes!(retry_pending_reviews))
        .curator(routes!(preview_bulk_reviews))
        .curator(routes!(apply_bulk_reviews))
        .curator(routes!(list_review_jobs))
        .curator(routes!(control_review_job))
        .user(routes!(get_review))
        .curator(routes!(approve_review))
        .curator(routes!(keep_tagged_review))
        .curator(routes!(dismiss_review))
        .curator(routes!(detach_review))
        .admin(routes!(exclude_review))
        .admin(routes!(restore_review))
        .admin(routes!(retry_review))
}

/// How the review list is sorted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSort {
    /// Most recently changed first.
    #[default]
    Newest,
    /// Least recently changed first.
    Oldest,
    /// Albums with the most files first.
    Tracks,
    /// By album title.
    Title,
    /// By album artist, then title.
    Artist,
    /// By library folder, then title.
    Root,
    /// By reason, so each reason's reviews sit together.
    Reason,
}

/// Review list filter and paging. A bulk change takes the same filter.
#[derive(Debug, Clone, Default, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct ReviewsQuery {
    /// `pending` (default), `approved`, `rejected`, `dismissed`,
    /// `excluded`, `resolved` (approved or dismissed) or `all`.
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
    /// Sort order; newest change first by default.
    pub sort: Option<ReviewSort>,
    /// Page size, 1 to 200 (default 50); more is read as 200.
    pub limit: Option<u32>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
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
            recording_mbid: evidence.recording_mbid.clone(),
            release_track_mbid: evidence.release_track_mbid.clone(),
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
        row_revision: row.row_revision,
    }
}

fn identity_view(identity: &AlbumIdentity) -> IdentityView {
    IdentityView {
        release_mbid: identity.release_mbid.clone(),
        release_group_mbid: identity.release_group_mbid.clone(),
        decision_source: identity.decision_source,
    }
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
    let paging =
        PageRequest::<u32>::read(query.limit, query.cursor.as_deref(), PAGE_DEFAULT, PAGE_MAX)?;
    let mut filter = review_filter(query, Vec::new())?;
    filter.limit = paging.limit as u32;
    filter.offset = paging.after.unwrap_or(0);
    let (offset, limit) = (filter.offset, filter.limit);
    let listing = blocking(move || ops.reviews(&filter)).await?;
    let reviews = listing.rows.iter().map(review_view).collect();
    let mut state_counts = ReviewStateCounts::default();
    for (state, count) in listing.state_counts {
        *match state {
            ReviewState::Pending => &mut state_counts.pending,
            ReviewState::Approved => &mut state_counts.approved,
            ReviewState::Rejected => &mut state_counts.rejected,
            ReviewState::Dismissed => &mut state_counts.dismissed,
            ReviewState::Excluded => &mut state_counts.excluded,
        } = count;
    }
    Ok(Json(ReviewsResponse {
        page: Page::at_offset(reviews, offset.into(), limit.into(), listing.total),
        state_counts,
        reason_counts: listing
            .reason_counts
            .into_iter()
            .map(|(code, count)| ReasonCount {
                reason: review_reason(&code).into(),
                reason_code: code,
                count,
            })
            .collect(),
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
    let states = if query.state.is_none() {
        vec![ReviewState::Pending]
    } else {
        parse_state(query.state.as_deref())?
    };
    let order = match query.sort.unwrap_or_default() {
        ReviewSort::Newest => ReviewOrder::Newest,
        ReviewSort::Oldest => ReviewOrder::Oldest,
        ReviewSort::Tracks => ReviewOrder::MostTracks,
        ReviewSort::Title => ReviewOrder::Title,
        ReviewSort::Artist => ReviewOrder::Artist,
        ReviewSort::Root => ReviewOrder::Root,
        ReviewSort::Reason => ReviewOrder::Reason,
    };
    Ok(ReviewFilter {
        states,
        album_id: query.album_id.filter(|id| !id.is_empty()),
        reason_code: query.reason_code.filter(|code| !code.is_empty()),
        search: query.search,
        review_ids,
        scopes: query
            .root_id
            .filter(|id| !id.is_empty())
            .map(|root_id| FolderScope {
                root_id,
                relative_path: ".".to_owned(),
            })
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
        limit: PAGE_DEFAULT as u32,
        offset: 0,
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
    let row = blocking(move || ops.review(&id)).await?;
    Ok(Json(review_view(&row)))
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
        (status = 409, description = "Review already settled, or changed since it was read (STALE_REVISION)"),
    )
)]
pub async fn approve_review(
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ApproveBody>,
) -> Result<Json<ReviewResolveResponse>, ApiError> {
    if body.candidate_key.is_empty() {
        return Err(ApiError::invalid_input("Candidate key must not be empty"));
    }
    let actor = caller.user_id().to_owned();
    let (row, identity) = blocking(move || {
        ops.approve_review(&id, body.expected_revision, &body.candidate_key, &actor)
    })
    .await?;
    Ok(Json(ReviewResolveResponse {
        review: review_view(&row),
        identity: identity.as_ref().map(identity_view),
    }))
}

async fn act(
    ops: Operations,
    id: String,
    action: ReviewAction,
    body: ReviewActionBody,
    actor: String,
) -> Result<Json<ReviewView>, ApiError> {
    let row =
        blocking(move || ops.settle_review(&id, action, body.expected_revision, &actor)).await?;
    Ok(Json(review_view(&row)))
}

/// Keep the album as tagged: nothing seals, and the review settles.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/{id}/keep-tagged",
    params(("id" = String, Path, description = "Review id")),
    request_body = ReviewActionBody,
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled, or changed since it was read (STALE_REVISION)"),
    )
)]
pub async fn keep_tagged_review(
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ReviewActionBody>,
) -> Result<Json<ReviewView>, ApiError> {
    let actor = caller.user_id().to_owned();
    act(ops, id, ReviewAction::KeepTagged, body, actor).await
}

/// Close a review without a decision. The album stays as it is.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/{id}/dismiss",
    params(("id" = String, Path, description = "Review id")),
    request_body = ReviewActionBody,
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled, or changed since it was read (STALE_REVISION)"),
    )
)]
pub async fn dismiss_review(
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ReviewActionBody>,
) -> Result<Json<ReviewView>, ApiError> {
    let actor = caller.user_id().to_owned();
    act(ops, id, ReviewAction::Dismiss, body, actor).await
}

/// Take the album's MusicBrainz match off and keep its own tags. The
/// album shows as unmatched, and the change can be undone from the album
/// like an edition choice.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/{id}/detach",
    params(("id" = String, Path, description = "Review id")),
    request_body = ReviewActionBody,
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled, or changed since it was read (STALE_REVISION)"),
    )
)]
pub async fn detach_review(
    State(ops): State<Operations>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ReviewActionBody>,
) -> Result<Json<ReviewView>, ApiError> {
    let actor = caller.user_id().to_owned();
    act(ops, id, ReviewAction::Detach, body, actor).await
}

/// Take the album's files out of the library. Nothing is deleted; scans
/// keep the files out until the review is restored.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/{id}/exclude",
    params(("id" = String, Path, description = "Review id")),
    request_body = ReviewActionBody,
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "Review already settled, or changed since it was read (STALE_REVISION)"),
    )
)]
pub async fn exclude_review(
    State(ops): State<Operations>,
    caller: Admin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ReviewActionBody>,
) -> Result<Json<ReviewView>, ApiError> {
    let actor = caller.user_id().to_owned();
    act(ops, id, ReviewAction::Exclude, body, actor).await
}

/// Put an excluded album's files back into the library.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/{id}/restore",
    params(("id" = String, Path, description = "Review id")),
    request_body = ReviewActionBody,
    responses(
        (status = 200, description = "Settled review", body = ReviewView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown review id"),
        (status = 409, description = "The review's album was not excluded, or the review changed since it was read (STALE_REVISION)"),
    )
)]
pub async fn restore_review(
    State(ops): State<Operations>,
    caller: Admin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ReviewActionBody>,
) -> Result<Json<ReviewView>, ApiError> {
    let actor = caller.user_id().to_owned();
    act(ops, id, ReviewAction::Restore, body, actor).await
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
        (status = 409, description = "Review already settled or changed since it was read (STALE_REVISION), album excluded, or needs the Local metadata confirmation"),
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
    let job = blocking(move || {
        ops.retry_review(
            &id,
            body.expected_revision,
            &actor,
            key,
            body.confirm_local_metadata,
        )
    })
    .await?;
    Ok(Json(job_view(&job)))
}

/// Start identification again for every pending review a filter selects
/// (all of them by default), as one background job that can be paused,
/// resumed and stopped. With `preview`, nothing starts: the answer counts
/// what would start and why the rest would not.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/retry-pending",
    request_body = ReviewRetryAllBody,
    responses(
        (status = 200, description = "The job, or what would start", body = ReviewRetryAllResponse),
        (status = 400, description = "Unknown filter value, folder or sub-folder"),
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
    let (preview, confirm, scope_ids) = (body.preview, body.confirm_local_metadata, body.scope_ids);
    let answer = blocking(move || {
        if !scope_ids.is_empty() {
            filter.scopes = ops.folder_scopes(&scope_ids)?;
        }
        if preview {
            let tally = ops.preview_retry_pending(&actor, confirm, &filter)?;
            return Ok(ReviewRetryAllResponse {
                preview,
                started: tally.done,
                skipped: skipped_view(tally.skipped),
                job: None,
            });
        }
        let job = ops.retry_pending_reviews(&actor, confirm, &filter)?;
        Ok::<_, OperationError>(ReviewRetryAllResponse {
            preview,
            started: job.as_ref().map_or(0, work_count),
            skipped: Vec::new(),
            job: job.as_ref().map(job_view),
        })
    })
    .await?;
    Ok(Json(answer))
}

fn work_count(job: &crate::operations::models::OperationJob) -> u32 {
    u32::try_from(job.expected_work_count).unwrap_or(u32::MAX)
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
    /// Albums in these library folders and sub-folder rules, picked the
    /// way a scan picks them (overrides the filter's folder).
    #[serde(default)]
    pub scope_ids: Vec<String>,
    /// Count what would start and why the rest would not; start nothing.
    #[serde(default)]
    pub preview: bool,
}

/// What a "retry all" started, or with `preview` would start.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewRetryAllResponse {
    /// True when nothing started: the counts are what a real run would do.
    pub preview: bool,
    /// Albums whose identification would start, or that the job will try.
    pub started: u32,
    /// Preview only: reviews that would stay open, grouped by why.
    pub skipped: Vec<SkippedReviews>,
    /// The job that retries them; none for a preview or when nothing was
    /// pending.
    pub job: Option<OperationView>,
}

/// A bulk review change: what to do, and to which reviews.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReviewBulkBody {
    pub action: BulkAction,
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
    let action = body.action;
    if action == BulkAction::Exclude {
        Admin::try_from(caller.principal().clone())?;
    }
    let filter = review_filter(body.filter, body.review_ids)?;
    let key = body.candidate_key.filter(|key| !key.is_empty());
    Ok((action, filter, key, body.preview_token))
}

/// Preview a bulk change: how many reviews it changes, which it skips and
/// why, the releases every selected review shares, and the token that
/// applies exactly this change. The token is good for 15 minutes, and only
/// for the person who previewed.
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
    let actor = caller.user_id().to_owned();
    let (action, filter, key, _) = bulk_request(body, &caller)?;
    let (plan, token) =
        blocking(move || ops.preview_bulk_reviews(action, &filter, key.as_deref(), &actor)).await?;
    Ok(Json(ReviewBulkPreviewResponse {
        matched: plan.matched,
        eligible: u32::try_from(plan.items.len()).unwrap_or(u32::MAX),
        skipped: skipped_view(plan.skipped),
        preview_token: token,
        shared_candidates: plan
            .shared
            .into_iter()
            .map(|shared| SharedCandidateView {
                candidate_key: shared.candidate_key,
                release_group_mbid: shared.release_group_mbid,
                release_mbid: shared.release_mbid,
                album_title: shared.album_title,
                album_artist_name: shared.album_artist_name,
                lowest_score: shared.lowest_score,
            })
            .collect(),
    }))
}

/// Apply a previewed bulk change as one background job that can be
/// paused, resumed and stopped. Refused when the token is not the caller's
/// or is too old, or the selection changed since the preview.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/bulk-apply",
    request_body = ReviewBulkBody,
    responses(
        (status = 200, description = "The job, and the reviews the preview left alone", body = ReviewBulkResponse),
        (status = 400, description = "Unknown action or filter value, or no readable preview token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required (admin to exclude)"),
        (status = 409, description = "The preview is too old or another person's, or the selection changed since"),
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
    let (job, skipped) =
        blocking(move || ops.apply_bulk_reviews(action, &filter, key.as_deref(), &token, &actor))
            .await?;
    Ok(Json(ReviewBulkResponse {
        queued: job.as_ref().map_or(0, work_count),
        skipped: skipped_view(skipped),
        job: job.as_ref().map(job_view),
    }))
}

/// What a bulk review change started.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewBulkResponse {
    /// Reviews the job will change, each checked again when it is reached.
    pub queued: u32,
    /// Reviews the preview left alone, grouped by why.
    pub skipped: Vec<SkippedReviews>,
    /// The job; none when the preview left nothing to change.
    pub job: Option<OperationView>,
}

/// A review job: a bulk change or retry running in the background.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewJobView {
    #[serde(flatten)]
    pub job: OperationView,
    /// What it does to each review; none when its work is gone.
    pub action: Option<ReviewJobAction>,
    /// Reviews it left alone so far, grouped by why.
    pub skipped: Vec<SkippedReviews>,
}

/// The review jobs to show.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewJobsResponse {
    pub jobs: Vec<ReviewJobView>,
}

fn review_job_view(job: ReviewJob) -> ReviewJobView {
    ReviewJobView {
        job: job_view(&job.job),
        action: job.action,
        skipped: skipped_view(job.skipped),
    }
}

/// Review jobs still running, queued or paused, and those that ended in
/// the last five minutes, newest first.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/reviews/jobs",
    responses(
        (status = 200, description = "Review jobs", body = ReviewJobsResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
    )
)]
pub async fn list_review_jobs(
    State(ops): State<Operations>,
    _caller: Curator,
) -> Result<Json<ReviewJobsResponse>, ApiError> {
    let jobs = blocking(move || ops.review_jobs()).await?;
    Ok(Json(ReviewJobsResponse {
        jobs: jobs.into_iter().map(review_job_view).collect(),
    }))
}

/// What to do with a review job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewJobControl {
    /// Hold it after the review in hand.
    Pause,
    /// Go on where it paused or stopped.
    Resume,
    /// End it after the review in hand.
    Stop,
}

/// Pause, resume or stop a review job.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReviewJobControlBody {
    pub control: ReviewJobControl,
}

/// Pause, resume or stop a review job. A running job halts between
/// reviews.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/reviews/jobs/{job_id}/control",
    params(("job_id" = String, Path, description = "Review job id")),
    request_body = ReviewJobControlBody,
    responses(
        (status = 200, description = "The review job", body = OperationView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown review job"),
    )
)]
pub async fn control_review_job(
    State(ops): State<Operations>,
    _caller: Curator,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<ReviewJobControlBody>,
) -> Result<Json<OperationView>, ApiError> {
    let control = match body.control {
        ReviewJobControl::Pause => Control::Pause,
        ReviewJobControl::Resume => Control::Resume,
        ReviewJobControl::Stop => Control::Stop,
    };
    let job = blocking(move || ops.control_review_job(&job_id, control)).await?;
    Ok(Json(job_view(&job)))
}
