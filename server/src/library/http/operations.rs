//! Library operation routes: watch and control an operation job, start an
//! explicit re-identification and settle it, search MusicBrainz releases
//! for the edition finder, and undo an automatic edition.
//!
//! Every route is admin-only, as in v2. Store work runs on
//! `spawn_blocking`; the release search awaits MusicBrainz.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::auth::RequireAdmin;
use super::error::{LibraryError, ValidJson, ValidQuery};
use super::models::PenaltyView;
use crate::library::identify::models::EvidenceClass;
use crate::library::operations::models::{
    CandidateChoice, Control, DecisionMode, OperationDetail, OperationError, OperationJob,
    ReidentificationCandidate, ReidentifyInput,
};
use crate::library::operations::service::{Operations, RELEASE_PAGE_MAX};
use crate::library::wiring::LibrarySetup;

/// Matcher version reported on candidates scored by the matching engine.
const MATCHER_VERSION: &str = "library-matching-v1";

/// Operation routes, relative for nesting under `/api/v3`.
pub fn operations_router() -> Router<LibrarySetup> {
    Router::new()
        .route("/library/operations/{job_id}", get(get_operation))
        .route("/library/operations/{job_id}/pause", post(pause_operation))
        .route(
            "/library/operations/{job_id}/resume",
            post(resume_operation),
        )
        .route("/library/operations/{job_id}/stop", post(stop_operation))
        .route(
            "/library/operations/{job_id}/candidate",
            post(select_reidentification_candidate),
        )
        .route(
            "/library/albums/{album_id}/reidentify",
            post(reidentify_album),
        )
        .route(
            "/library/albums/{album_id}/reidentification/releases",
            get(search_reidentification_releases),
        )
        .route(
            "/library/albums/{album_id}/undo-automatic-edition",
            post(undo_automatic_edition),
        )
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/// Why an operation request failed, rendered in the shared envelope.
#[derive(Debug)]
pub enum OperationsHttpError {
    Library(LibraryError),
    Operation(OperationError),
}

impl From<LibraryError> for OperationsHttpError {
    fn from(error: LibraryError) -> Self {
        Self::Library(error)
    }
}

impl From<OperationError> for OperationsHttpError {
    fn from(error: OperationError) -> Self {
        Self::Operation(error)
    }
}

impl IntoResponse for OperationsHttpError {
    fn into_response(self) -> Response {
        use crate::error::{envelope_response, fault_response};
        let error = match self {
            Self::Library(error) => return error.into_response(),
            Self::Operation(error) => error,
        };
        match error {
            OperationError::NotFound(message) => {
                envelope_response(StatusCode::NOT_FOUND, "NOT_FOUND", message, None)
            }
            OperationError::Invalid(message) => {
                envelope_response(StatusCode::BAD_REQUEST, "INVALID_INPUT", message, None)
            }
            OperationError::MappingIncomplete(message) => envelope_response(
                StatusCode::BAD_REQUEST,
                "EXACT_RELEASE_MAPPING_INCOMPLETE",
                message,
                None,
            ),
            OperationError::NotSealable(message) => envelope_response(
                StatusCode::BAD_REQUEST,
                "CUSTOM_EDITION_NOT_SEALABLE",
                message,
                None,
            ),
            OperationError::Conflict(message) => {
                envelope_response(StatusCode::CONFLICT, "CONFLICT", message, None)
            }
            OperationError::Stale(message) => {
                envelope_response(StatusCode::CONFLICT, "STALE_REVISION", message, None)
            }
            OperationError::Unavailable(cause) => {
                let error_id = uuid::Uuid::new_v4().to_string();
                tracing::warn!(error_id, %cause, "MusicBrainz release search failed");
                fault_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    crate::error::UPSTREAM_ERROR,
                    crate::error::FIXED_UPSTREAM_MESSAGE,
                    &error_id,
                )
            }
            OperationError::Store(cause) => LibraryError::internal(&cause).into_response(),
        }
    }
}

/// Run blocking operation work off the async workers.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, OperationError> + Send + 'static,
) -> Result<T, OperationsHttpError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause))?
        .map_err(OperationsHttpError::from)
}

// ---------------------------------------------------------------------------
// DTOs.
// ---------------------------------------------------------------------------

/// One work item's outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperationWorkResultView {
    pub ordinal: i64,
    pub action: String,
    /// `pending`, `running`, `succeeded`, `failed`, or `skipped`.
    pub state: String,
    pub local_album_id: Option<String>,
    pub local_track_id: Option<String>,
    pub failure_code: Option<String>,
    /// What the work item recorded (for a re-identification: `outcome`,
    /// `reason_code`, `candidate_keys`).
    #[schema(value_type = Object)]
    pub result: serde_json::Value,
}

/// One local file against one candidate release.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperationTrackEvidenceView {
    pub local_track_id: String,
    /// `supported`, `unknown`, or `contradictory`.
    pub classification: String,
    /// What ties the file to the release track (`embedded_id`, `acoustid`,
    /// `title_position_length`), or why it does not.
    pub evidence_kinds: Vec<String>,
    pub candidate_track_title: Option<String>,
    pub candidate_disc_number: Option<u32>,
    pub candidate_track_position: Option<u32>,
    pub recording_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
}

/// The evidence behind one candidate release.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperationCandidateEvidenceView {
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub album_title: String,
    pub album_artist_name: String,
    pub artist_mbid: Option<String>,
    pub release_type: Option<String>,
    pub release_date: Option<String>,
    pub local_album_title: String,
    pub local_album_artist_name: String,
    /// `supported`, `unknown`, or `contradictory`.
    pub album_title_classification: String,
    /// `supported`, `unknown`, or `contradictory`.
    pub album_artist_classification: String,
    pub track_evidence: Vec<OperationTrackEvidenceView>,
    /// Release track titles no local file took.
    pub unmatched_expected_tracks: Vec<String>,
    /// One minus the distance.
    pub score: f64,
    /// Distance gap to the next candidate in the list.
    pub margin: f64,
    pub reason_code: String,
    pub matcher_version: String,
    /// Matcher distance, 0 for a perfect match.
    pub distance: f64,
    /// What the distance is made of, largest share first.
    pub penalties: Vec<PenaltyView>,
}

/// One candidate a re-identification offers.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReidentificationCandidateView {
    /// Choose the candidate by this key.
    pub candidate_key: String,
    /// Changes whenever the job re-evaluates.
    pub evidence_revision: String,
    pub evidence: OperationCandidateEvidenceView,
    /// The matcher would have accepted it on its own: no confirmation
    /// needed to choose it.
    pub automatic_safe: bool,
}

/// One operation job.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperationView {
    pub id: String,
    /// `explicit_reidentification`, `bulk_review_apply`, `repair`, or
    /// `library_management`.
    pub kind: String,
    /// `queued`, `running`, `paused`, `ready`, `succeeded`, `failed`,
    /// `cancelled`, or `stopped`.
    pub state: String,
    pub expected_work_count: i64,
    pub completed_count: i64,
    pub succeeded_count: i64,
    pub failed_count: i64,
    pub skipped_count: i64,
    /// A control the worker has not acted on yet: `none`, `pause`, `stop`.
    pub control_request: String,
    pub terminal_code: Option<String>,
    /// Echo this on every control and choice.
    pub row_revision: i64,
    pub event_revision: i64,
    pub created_at: f64,
    pub updated_at: f64,
    /// The first 100 work results.
    pub results: Vec<OperationWorkResultView>,
    pub results_truncated: bool,
    /// Candidates a re-identification waits on, best first.
    pub reidentification_candidates: Vec<ReidentificationCandidateView>,
    pub selected_reidentification_candidate_key: Option<String>,
}

/// Pause, resume, or stop a job.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OperationControlBody {
    /// The job's `row_revision` as last read.
    pub expected_row_revision: i64,
    /// Makes a retried control a no-op.
    pub idempotency_key: Option<String>,
}

/// How to settle a re-identification.
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionModeBody {
    /// Accept the candidate's exact release; every file must map to it.
    #[default]
    ExactRelease,
    /// Keep the release group and seal the files as they are.
    CustomEdition,
    /// Keep the album out of Library Management.
    LeaveUnmanaged,
}

/// Choose a re-identification candidate.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CandidateChoiceBody {
    /// The job's `row_revision` as last read.
    pub expected_row_revision: i64,
    /// The chosen candidate; empty only to leave the album unmanaged.
    #[serde(default)]
    pub candidate_key: String,
    /// Required for a candidate that is not automatic-safe, a release the
    /// administrator named, or a custom edition.
    #[serde(default)]
    pub confirmation: bool,
    #[serde(default)]
    pub decision_mode: DecisionModeBody,
}

/// Start an explicit re-identification.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ReidentifyBody {
    /// The album's revision as last read; refused when it moved.
    pub expected_album_revision: Option<i64>,
    /// The album's input revision as last read; refused when it moved.
    pub expected_input_revision: Option<String>,
    /// Makes a retried request return the same job.
    pub idempotency_key: Option<String>,
    /// Confirms a one-off lookup for files under a Local metadata policy.
    #[serde(default)]
    pub one_off_local_metadata: bool,
    /// Evaluate only this exact MusicBrainz release.
    pub release_mbid: Option<String>,
}

/// Undo the album's last automatic edition.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AutomaticEditionUndoBody {
    pub expected_album_revision: i64,
    pub expected_identity_revision: i64,
}

/// What the undo did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutomaticEditionUndoResponse {
    pub local_album_id: String,
    /// `restored` (the earlier identity is back) or `cleared_to_review`.
    pub outcome: String,
    /// The review the album went to, when there was no earlier identity.
    pub review_id: Option<String>,
}

/// Edition finder query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ReleaseSearchQuery {
    /// Release title (required).
    pub title: String,
    /// Artist name; blank searches every artist.
    #[serde(default)]
    pub artist: String,
    /// Page size, 1 to 12 (default 12).
    pub limit: Option<u32>,
    /// Page offset (default 0).
    pub offset: Option<u32>,
}

/// One MusicBrainz release in the edition finder.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReleaseEditionView {
    pub release_mbid: String,
    pub release_group_mbid: String,
    pub artist_name: String,
    pub title: String,
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    pub packaging: Option<String>,
    pub media_formats: Vec<String>,
    pub disc_count: u32,
    pub track_count: u32,
    pub label: Option<String>,
    pub catalogue_number: Option<String>,
    pub barcode: Option<String>,
    pub disambiguation: Option<String>,
    pub musicbrainz_url: String,
    /// MusicBrainz search score, 0 to 100.
    pub score: i64,
    pub belongs_to_current_release_group: bool,
    pub is_current_release: bool,
}

/// One page of the edition finder.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReleaseEditionSearchResponse {
    pub title_query: String,
    pub artist_query: String,
    pub items: Vec<ReleaseEditionView>,
    pub total: u64,
    pub offset: u64,
    pub limit: u32,
}

fn class_label(class: EvidenceClass) -> String {
    match class {
        EvidenceClass::Supported => "supported",
        EvidenceClass::Unknown => "unknown",
        EvidenceClass::Contradictory => "contradictory",
    }
    .to_owned()
}

fn candidate_view(
    job: &OperationJob,
    candidate: &ReidentificationCandidate,
    next_distance: Option<f64>,
) -> ReidentificationCandidateView {
    let evidence = &candidate.evidence;
    let track_evidence = evidence
        .track_evidence
        .iter()
        .map(|track| {
            let placed = candidate
                .tracks
                .iter()
                .find(|place| place.local_track_id == track.local_track_id);
            OperationTrackEvidenceView {
                local_track_id: track.local_track_id.clone(),
                classification: class_label(track.classification),
                evidence_kinds: track.evidence_kinds.clone(),
                candidate_track_title: placed.and_then(|place| place.title.clone()),
                candidate_disc_number: placed.and_then(|place| place.disc_number),
                candidate_track_position: placed.and_then(|place| place.position),
                recording_mbid: track.recording_mbid.clone(),
                release_track_mbid: track.release_track_mbid.clone(),
            }
        })
        .collect();
    ReidentificationCandidateView {
        candidate_key: candidate.candidate_key.clone(),
        evidence_revision: format!("{}:{}", job.id, job.event_revision),
        automatic_safe: candidate.automatic_safe,
        evidence: OperationCandidateEvidenceView {
            release_group_mbid: evidence.release_group_mbid.clone(),
            release_mbid: evidence.release_mbid.clone(),
            album_title: evidence.album_title.clone(),
            album_artist_name: evidence.album_artist_name.clone(),
            artist_mbid: candidate.artist_mbid.clone(),
            release_type: candidate.release_type.clone(),
            release_date: candidate.release_date.clone(),
            local_album_title: candidate.local_album_title.clone(),
            local_album_artist_name: candidate.local_album_artist_name.clone(),
            album_title_classification: class_label(candidate.album_title_classification),
            album_artist_classification: class_label(candidate.album_artist_classification),
            track_evidence,
            unmatched_expected_tracks: candidate.unmatched_expected_tracks.clone(),
            score: evidence.score,
            margin: next_distance.map_or(0.0, |next| (next - evidence.distance).max(0.0)),
            reason_code: evidence.reason_code.clone(),
            matcher_version: MATCHER_VERSION.to_owned(),
            distance: evidence.distance,
            penalties: evidence
                .penalties
                .iter()
                .map(|penalty| PenaltyView {
                    name: penalty.name.clone(),
                    share: penalty.share,
                })
                .collect(),
        },
    }
}

fn job_view(job: &OperationJob) -> OperationView {
    OperationView {
        id: job.id.clone(),
        kind: job.kind.clone(),
        state: job.state.as_str().to_owned(),
        expected_work_count: job.expected_work_count,
        completed_count: job.completed_count,
        succeeded_count: job.succeeded_count,
        failed_count: job.failed_count,
        skipped_count: job.skipped_count,
        control_request: job.control_request.as_str().to_owned(),
        terminal_code: job.terminal_code.clone(),
        row_revision: job.row_revision,
        event_revision: job.event_revision,
        created_at: job.created_at,
        updated_at: job.updated_at,
        results: Vec::new(),
        results_truncated: false,
        reidentification_candidates: Vec::new(),
        selected_reidentification_candidate_key: None,
    }
}

fn detail_view(detail: &OperationDetail) -> OperationView {
    let candidates = &detail.candidates;
    OperationView {
        results: detail
            .results
            .iter()
            .map(|result| OperationWorkResultView {
                ordinal: result.ordinal,
                action: result.action.clone(),
                state: result.state.clone(),
                local_album_id: result.local_album_id.clone(),
                local_track_id: result.local_track_id.clone(),
                failure_code: result.failure_code.clone(),
                result: result.result.clone(),
            })
            .collect(),
        results_truncated: detail.results_truncated,
        reidentification_candidates: candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let next = candidates.get(index + 1).map(|next| next.evidence.distance);
                candidate_view(&detail.job, candidate, next)
            })
            .collect(),
        selected_reidentification_candidate_key: detail.selected_candidate_key.clone(),
        ..job_view(&detail.job)
    }
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Get one operation job with its results and candidates.
#[utoipa::path(
    get,
    path = "/api/v3/library/operations/{job_id}",
    params(("job_id" = String, Path, description = "Operation job id")),
    responses(
        (status = 200, description = "Operation job", body = OperationView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
    )
)]
pub async fn get_operation(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
) -> Result<Json<OperationView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let detail = blocking(move || ops.get(&job_id)).await?;
    Ok(Json(detail_view(&detail)))
}

async fn control(
    state: LibrarySetup,
    job_id: String,
    control: Control,
    body: OperationControlBody,
) -> Result<Json<OperationView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let job = blocking(move || {
        ops.control(
            &job_id,
            control,
            body.expected_row_revision,
            body.idempotency_key.as_deref(),
        )
    })
    .await?;
    Ok(Json(job_view(&job)))
}

/// Pause a job. A running job pauses at its next checkpoint.
#[utoipa::path(
    post,
    path = "/api/v3/library/operations/{job_id}/pause",
    params(("job_id" = String, Path, description = "Operation job id")),
    request_body = OperationControlBody,
    responses(
        (status = 200, description = "Operation job", body = OperationView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "Stale revision or reused idempotency key"),
    )
)]
pub async fn pause_operation(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<OperationControlBody>,
) -> Result<Json<OperationView>, OperationsHttpError> {
    control(state, job_id, Control::Pause, body).await
}

/// Resume a paused or stopped job, or one that failed while MusicBrainz
/// was down.
#[utoipa::path(
    post,
    path = "/api/v3/library/operations/{job_id}/resume",
    params(("job_id" = String, Path, description = "Operation job id")),
    request_body = OperationControlBody,
    responses(
        (status = 200, description = "Operation job", body = OperationView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "Stale revision or reused idempotency key"),
    )
)]
pub async fn resume_operation(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<OperationControlBody>,
) -> Result<Json<OperationView>, OperationsHttpError> {
    control(state, job_id, Control::Resume, body).await
}

/// Stop a job. A running job stops at its next checkpoint.
#[utoipa::path(
    post,
    path = "/api/v3/library/operations/{job_id}/stop",
    params(("job_id" = String, Path, description = "Operation job id")),
    request_body = OperationControlBody,
    responses(
        (status = 200, description = "Operation job", body = OperationView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "Stale revision or reused idempotency key"),
    )
)]
pub async fn stop_operation(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<OperationControlBody>,
) -> Result<Json<OperationView>, OperationsHttpError> {
    control(state, job_id, Control::Stop, body).await
}

/// Settle a re-identification: seal a candidate's exact release, seal a
/// custom edition, or leave the album unmanaged.
#[utoipa::path(
    post,
    path = "/api/v3/library/operations/{job_id}/candidate",
    params(("job_id" = String, Path, description = "Re-identification job id")),
    request_body = CandidateChoiceBody,
    responses(
        (status = 200, description = "Settled job", body = OperationView),
        (status = 400, description = "Confirmation missing, the release does not map every file, or the custom edition cannot be sealed"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown job"),
        (status = 409, description = "The job or the album changed since it was read"),
    )
)]
pub async fn select_reidentification_candidate(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<CandidateChoiceBody>,
) -> Result<Json<OperationView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let user_id = caller.0.user_id;
    let choice = CandidateChoice {
        expected_row_revision: body.expected_row_revision,
        candidate_key: body.candidate_key,
        confirmation: body.confirmation,
        decision_mode: match body.decision_mode {
            DecisionModeBody::ExactRelease => DecisionMode::ExactRelease,
            DecisionModeBody::CustomEdition => DecisionMode::CustomEdition,
            DecisionModeBody::LeaveUnmanaged => DecisionMode::LeaveUnmanaged,
        },
    };
    let detail = blocking(move || {
        ops.select_candidate(&job_id, &choice, &user_id)?;
        ops.get(&job_id)
    })
    .await?;
    Ok(Json(detail_view(&detail)))
}

/// Start an explicit re-identification of one album. The job scores every
/// candidate and waits for an administrator's choice; nothing seals on
/// its own.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/reidentify",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = ReidentifyBody,
    responses(
        (status = 200, description = "Re-identification job (new or the one this key started)", body = OperationView),
        (status = 400, description = "Bad body or release MBID"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, or no indexed files"),
        (status = 409, description = "The album moved, is excluded, or needs the Local metadata confirmation"),
    )
)]
pub async fn reidentify_album(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<ReidentifyBody>,
) -> Result<Json<OperationView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let user_id = caller.0.user_id;
    let input = ReidentifyInput {
        expected_album_revision: body.expected_album_revision,
        expected_input_revision: body.expected_input_revision,
        idempotency_key: body.idempotency_key.filter(|key| !key.trim().is_empty()),
        one_off_local_metadata: body.one_off_local_metadata,
        release_mbid: body.release_mbid.filter(|mbid| !mbid.trim().is_empty()),
    };
    let job = blocking(move || ops.reidentify(&album_id, &user_id, input)).await?;
    Ok(Json(job_view(&job)))
}

/// Search MusicBrainz releases for an album's edition finder.
#[utoipa::path(
    get,
    path = "/api/v3/library/albums/{album_id}/reidentification/releases",
    params(("album_id" = String, Path, description = "Local album id"), ReleaseSearchQuery),
    responses(
        (status = 200, description = "One page of releases", body = ReleaseEditionSearchResponse),
        (status = 400, description = "Missing title or bad paging"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album"),
        (status = 503, description = "MusicBrainz is unavailable"),
    )
)]
pub async fn search_reidentification_releases(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidQuery(query): ValidQuery<ReleaseSearchQuery>,
) -> Result<Json<ReleaseEditionSearchResponse>, OperationsHttpError> {
    let limit = query.limit.unwrap_or(RELEASE_PAGE_MAX);
    if !(1..=RELEASE_PAGE_MAX).contains(&limit)
        || query.title.chars().count() > 250
        || query.artist.chars().count() > 250
    {
        return Err(OperationError::Invalid(
            "Titles and artists are at most 250 characters; pages hold 1 to 12 releases.".into(),
        )
        .into());
    }
    let search = Operations::new(&state)
        .search_releases(
            &album_id,
            &query.title,
            &query.artist,
            limit,
            query.offset.unwrap_or(0),
        )
        .await?;
    let same = |left: &str, right: Option<&String>| {
        right.is_some_and(|right| left.eq_ignore_ascii_case(right))
    };
    Ok(Json(ReleaseEditionSearchResponse {
        items: search
            .page
            .items
            .iter()
            .map(|item| ReleaseEditionView {
                musicbrainz_url: format!("https://musicbrainz.org/release/{}", item.release_mbid),
                belongs_to_current_release_group: same(
                    &item.release_group_mbid,
                    search.current_release_group_mbid.as_ref(),
                ),
                is_current_release: same(&item.release_mbid, search.current_release_mbid.as_ref()),
                release_mbid: item.release_mbid.clone(),
                release_group_mbid: item.release_group_mbid.clone(),
                artist_name: item.artist_name.clone(),
                title: item.title.clone(),
                date: item.date.clone(),
                country: item.country.clone(),
                status: item.status.clone(),
                packaging: item.packaging.clone(),
                media_formats: item.media_formats.clone(),
                disc_count: item.disc_count,
                track_count: item.track_count,
                label: item.label.clone(),
                catalogue_number: item.catalogue_number.clone(),
                barcode: item.barcode.clone(),
                disambiguation: item.disambiguation.clone(),
                score: item.score,
            })
            .collect(),
        title_query: search.title_query,
        artist_query: search.artist_query,
        total: search.page.total,
        offset: search.page.offset,
        limit: search.limit,
    }))
}

/// Undo the album's last automatic edition: the identity it replaced comes
/// back, or the album goes to review when there was none. Pins stay.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/undo-automatic-edition",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = AutomaticEditionUndoBody,
    responses(
        (status = 200, description = "What the undo did", body = AutomaticEditionUndoResponse),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "No reversible automatic edition"),
        (status = 409, description = "The identity changed since the automatic acceptance"),
    )
)]
pub async fn undo_automatic_edition(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<AutomaticEditionUndoBody>,
) -> Result<Json<AutomaticEditionUndoResponse>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let user_id = caller.0.user_id;
    let undone = blocking(move || {
        ops.undo_automatic_edition(
            &album_id,
            body.expected_album_revision,
            body.expected_identity_revision,
            &user_id,
        )
    })
    .await?;
    Ok(Json(AutomaticEditionUndoResponse {
        local_album_id: undone.local_album_id,
        outcome: undone.outcome,
        review_id: undone.review_id,
    }))
}
