//! Artist reconciliation routes: progress, duplicate artist groups, one
//! group's evidence, and marking a group as distinct people.
//!
//! Admin-only, as in v2. Store work runs on `spawn_blocking`.

use std::collections::BTreeMap;

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
use super::operations::ReasonView;
use crate::library::reconcile::models::{
    CreditEvidence, Group, GroupDetail, GroupState, Member, OwnedReference, ReconcileError,
};
use crate::library::reconcile::service::Reconcile;
use crate::library::wiring::LibrarySetup;

pub fn router() -> Router<LibrarySetup> {
    Router::new()
        .route(
            "/library/artists/reconciliation",
            get(artist_reconciliation),
        )
        .route(
            "/library/artists/duplicate-groups",
            get(list_artist_duplicate_groups),
        )
        .route(
            "/library/artists/duplicate-groups/{group_id}",
            get(get_artist_duplicate_group),
        )
        .route(
            "/library/artists/duplicate-groups/{group_id}/dismiss",
            post(dismiss_artist_duplicate_group),
        )
}

/// Why a reconciliation request failed, in the shared envelope.
#[derive(Debug)]
pub enum ReconcileHttpError {
    Library(LibraryError),
    Reconcile(ReconcileError),
}

impl From<LibraryError> for ReconcileHttpError {
    fn from(error: LibraryError) -> Self {
        Self::Library(error)
    }
}

impl From<ReconcileError> for ReconcileHttpError {
    fn from(error: ReconcileError) -> Self {
        Self::Reconcile(error)
    }
}

impl IntoResponse for ReconcileHttpError {
    fn into_response(self) -> Response {
        use crate::error::envelope_response;
        let error = match self {
            Self::Library(error) => return error.into_response(),
            Self::Reconcile(error) => error,
        };
        let (status, reason) = match error {
            ReconcileError::NotFound(reason) => (StatusCode::NOT_FOUND, reason),
            ReconcileError::Invalid(reason) => (StatusCode::BAD_REQUEST, reason),
            ReconcileError::Conflict(reason) => (StatusCode::CONFLICT, reason),
            ReconcileError::Store(cause) => return LibraryError::internal(&cause).into_response(),
        };
        envelope_response(
            status,
            reason.code,
            reason.message,
            Some(serde_json::json!({ "action": reason.action })),
        )
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ReconcileError> + Send + 'static,
) -> Result<T, ReconcileHttpError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause))?
        .map_err(ReconcileHttpError::from)
}

// ---------------------------------------------------------------------------
// DTOs.
// ---------------------------------------------------------------------------

/// What kind of duplicate group this is.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistGroupState {
    /// MusicBrainz proof is still missing for some records.
    WaitingForIdentity,
    /// The records point at different MusicBrainz artists.
    ProviderConflict,
    /// An album's artist credit could not be split cleanly.
    AmbiguousCreditStructure,
    /// Same name, no MusicBrainz evidence at all.
    SameNameOnly,
    /// Merged automatically already.
    ResolvedAutomatically,
}

impl From<GroupState> for ArtistGroupState {
    fn from(state: GroupState) -> Self {
        match state {
            GroupState::WaitingForIdentity => Self::WaitingForIdentity,
            GroupState::ProviderConflict => Self::ProviderConflict,
            GroupState::AmbiguousCreditStructure => Self::AmbiguousCreditStructure,
            GroupState::SameNameOnly => Self::SameNameOnly,
            GroupState::ResolvedAutomatically => Self::ResolvedAutomatically,
        }
    }
}

impl From<ArtistGroupState> for GroupState {
    fn from(state: ArtistGroupState) -> Self {
        match state {
            ArtistGroupState::WaitingForIdentity => Self::WaitingForIdentity,
            ArtistGroupState::ProviderConflict => Self::ProviderConflict,
            ArtistGroupState::AmbiguousCreditStructure => Self::AmbiguousCreditStructure,
            ArtistGroupState::SameNameOnly => Self::SameNameOnly,
            ArtistGroupState::ResolvedAutomatically => Self::ResolvedAutomatically,
        }
    }
}

/// Where the artist reconciliation pass stands.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistReconciliationProgress {
    /// The newest reconciliation job's state, or `idle`.
    pub state: String,
    pub completed_count: i64,
    pub expected_count: i64,
    /// Artist records merged automatically so far.
    pub automatically_resolved_count: i64,
    pub waiting_for_identity_count: usize,
    /// Groups that need an administrator's judgement.
    pub genuine_review_count: usize,
    pub provider_conflict_count: usize,
    pub ambiguous_credit_structure_count: usize,
    pub same_name_only_count: usize,
    pub operation_job_id: Option<String>,
}

/// One artist record in a group, with how often it is referenced.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistReconciliationMember {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub row_revision: i64,
    pub provider_mbid: Option<String>,
    pub album_credit_count: i64,
    pub track_credit_count: i64,
    pub primary_album_count: i64,
    pub favorite_count: i64,
    pub playlist_count: i64,
    pub history_count: i64,
    pub compatibility_id_count: i64,
    pub proven_credit_count: i64,
    pub active_credit_count: i64,
}

impl From<&Member> for ArtistReconciliationMember {
    fn from(member: &Member) -> Self {
        let c = &member.counts;
        Self {
            id: member.id.clone(),
            name: member.name.clone(),
            sort_name: member.sort_name.clone(),
            row_revision: member.row_revision,
            provider_mbid: member.provider_mbid.clone(),
            album_credit_count: c.album_credits,
            track_credit_count: c.track_credits,
            primary_album_count: c.primary_albums,
            favorite_count: c.favorites,
            playlist_count: c.playlist_snapshots,
            history_count: c.history,
            compatibility_id_count: c.compatibility_ids,
            proven_credit_count: c.proven_credits,
            active_credit_count: c.active_credits(),
        }
    }
}

/// One duplicate artist group.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistDuplicateGroupSummary {
    /// Stable for the same set of records.
    pub id: String,
    pub display_name: String,
    pub state: ArtistGroupState,
    pub member_count: usize,
    pub members: Vec<ArtistReconciliationMember>,
    pub provider_mbids: Vec<String>,
    /// The record a merge would keep, when the evidence names one.
    pub recommended_survivor_id: Option<String>,
    /// Credits, favorites, playlist entries, plays, and client ids a merge
    /// would move.
    pub affected_reference_count: i64,
    pub reason_code: String,
    /// Why the group is listed and what to do about it.
    pub reason: ReasonView,
    /// When an automatic merge resolved the group (unix seconds).
    pub resolved_at: Option<f64>,
}

impl From<&Group> for ArtistDuplicateGroupSummary {
    fn from(group: &Group) -> Self {
        Self {
            id: group.id.clone(),
            display_name: group.display_name.clone(),
            state: group.state.into(),
            member_count: group.members.len(),
            members: group.members.iter().map(Into::into).collect(),
            provider_mbids: group.provider_mbids.clone(),
            recommended_survivor_id: group.recommended_survivor_id.clone(),
            affected_reference_count: group.affected_reference_count,
            reason_code: group.reason_code.clone(),
            reason: group.reason.into(),
            resolved_at: group.resolved_at,
        }
    }
}

/// One page of duplicate artist groups.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistDuplicateGroupListResponse {
    pub items: Vec<ArtistDuplicateGroupSummary>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    /// Groups matching the filters.
    pub total: usize,
    /// Every group by state, before the filters.
    pub counts: BTreeMap<String, usize>,
}

/// A MusicBrainz credit proof behind one record's credit.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistCreditEvidence {
    /// `album` or `track`.
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_name: String,
    pub source_local_artist_id: Option<String>,
    pub local_artist_id: String,
    pub artist_mbid: String,
    pub canonical_name: String,
    pub credited_name: String,
    pub join_phrase: String,
    pub release_mbid: String,
    pub release_track_mbid: Option<String>,
    pub album_identity_revision: i64,
    pub track_identity_revision: Option<i64>,
    pub evidence_hash: String,
}

impl From<CreditEvidence> for ArtistCreditEvidence {
    fn from(e: CreditEvidence) -> Self {
        Self {
            subject_kind: e.subject_kind,
            subject_id: e.subject_id,
            subject_name: e.subject_name,
            source_local_artist_id: e.source_local_artist_id,
            local_artist_id: e.local_artist_id,
            artist_mbid: e.artist_mbid,
            canonical_name: e.canonical_name,
            credited_name: e.credited_name,
            join_phrase: e.join_phrase,
            release_mbid: e.release_mbid,
            release_track_mbid: e.release_track_mbid,
            album_identity_revision: e.album_identity_revision,
            track_identity_revision: e.track_identity_revision,
            evidence_hash: e.evidence_hash,
        }
    }
}

/// An album or track the group's records are credited on.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistOwnedReference {
    pub id: String,
    pub name: String,
    pub row_revision: i64,
    /// The album is matched to a MusicBrainz release.
    pub identity_ready: bool,
    /// Every indexed file maps to a track of that release.
    pub exact_track_mapping_ready: bool,
}

impl From<OwnedReference> for ArtistOwnedReference {
    fn from(r: OwnedReference) -> Self {
        Self {
            id: r.id,
            name: r.name,
            row_revision: r.row_revision,
            identity_ready: r.identity_ready,
            exact_track_mapping_ready: r.exact_track_mapping_ready,
        }
    }
}

/// One duplicate artist group with its evidence and references.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistDuplicateGroupDetail {
    #[serde(flatten)]
    pub summary: ArtistDuplicateGroupSummary,
    pub evidence: Vec<ArtistCreditEvidence>,
    pub releases: Vec<ArtistOwnedReference>,
    pub tracks: Vec<ArtistOwnedReference>,
    /// Reference totals over every member, by kind.
    pub reference_counts: BTreeMap<String, i64>,
    /// Each member's revision, to send back when dismissing.
    pub member_revisions: BTreeMap<String, i64>,
}

impl From<GroupDetail> for ArtistDuplicateGroupDetail {
    fn from(detail: GroupDetail) -> Self {
        let members = &detail.group.members;
        let total = |pick: fn(&Member) -> i64| members.iter().map(pick).sum::<i64>();
        let reference_counts = BTreeMap::from([
            (
                "album_credits".to_owned(),
                total(|m| m.counts.album_credits),
            ),
            (
                "track_credits".to_owned(),
                total(|m| m.counts.track_credits),
            ),
            (
                "primary_albums".to_owned(),
                total(|m| m.counts.primary_albums),
            ),
            ("favorites".to_owned(), total(|m| m.counts.favorites)),
            (
                "playlist_snapshots".to_owned(),
                total(|m| m.counts.playlist_snapshots),
            ),
            ("history".to_owned(), total(|m| m.counts.history)),
            (
                "compatibility_ids".to_owned(),
                total(|m| m.counts.compatibility_ids),
            ),
        ]);
        let member_revisions = members
            .iter()
            .map(|m| (m.id.clone(), m.row_revision))
            .collect();
        Self {
            summary: (&detail.group).into(),
            evidence: detail
                .references
                .evidence
                .into_iter()
                .map(Into::into)
                .collect(),
            releases: detail
                .references
                .releases
                .into_iter()
                .map(Into::into)
                .collect(),
            tracks: detail
                .references
                .tracks
                .into_iter()
                .map(Into::into)
                .collect(),
            reference_counts,
            member_revisions,
        }
    }
}

/// Group list filters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ArtistGroupListQuery {
    /// Page size, 1 to 100 (default 50).
    pub limit: Option<usize>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
    pub state: Option<ArtistGroupState>,
    /// Matches the group name or any member's name.
    pub search: Option<String>,
}

/// Mark a group's records as distinct people.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ArtistDuplicateGroupDismissRequest {
    /// Every member's `row_revision` as shown; a changed member refuses.
    pub expected_member_revisions: BTreeMap<String, i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistDuplicateGroupDismissResponse {
    pub group_id: String,
    /// Pairs of records now marked distinct.
    pub dismissed_pairs: usize,
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Artist reconciliation progress and open group counts.
#[utoipa::path(
    get,
    path = "/api/v3/library/artists/reconciliation",
    responses(
        (status = 200, description = "Progress", body = ArtistReconciliationProgress),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn artist_reconciliation(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
) -> Result<Json<ArtistReconciliationProgress>, ReconcileHttpError> {
    let service = Reconcile::new(&state);
    let p = blocking(move || service.progress()).await?;
    Ok(Json(ArtistReconciliationProgress {
        state: p.state,
        completed_count: p.completed_count,
        expected_count: p.expected_count,
        automatically_resolved_count: p.automatically_resolved_count,
        waiting_for_identity_count: p.waiting_for_identity_count,
        genuine_review_count: p.genuine_review_count,
        provider_conflict_count: p.provider_conflict_count,
        ambiguous_credit_structure_count: p.ambiguous_credit_structure_count,
        same_name_only_count: p.same_name_only_count,
        operation_job_id: p.operation_job_id,
    }))
}

/// List duplicate artist groups, sorted by name.
#[utoipa::path(
    get,
    path = "/api/v3/library/artists/duplicate-groups",
    params(ArtistGroupListQuery),
    responses(
        (status = 200, description = "One page of groups", body = ArtistDuplicateGroupListResponse),
        (status = 400, description = "Bad page size or cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_artist_duplicate_groups(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    ValidQuery(query): ValidQuery<ArtistGroupListQuery>,
) -> Result<Json<ArtistDuplicateGroupListResponse>, ReconcileHttpError> {
    let service = Reconcile::new(&state);
    let page = blocking(move || {
        service.list(
            query.limit.unwrap_or(50),
            query.cursor.as_deref(),
            query.state.map(Into::into),
            query.search.as_deref(),
        )
    })
    .await?;
    Ok(Json(ArtistDuplicateGroupListResponse {
        items: page.items.iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
        has_more: page.has_more,
        total: page.total,
        counts: page
            .counts
            .into_iter()
            .map(|(state, n)| (state.to_owned(), n))
            .collect(),
    }))
}

/// One duplicate artist group with its evidence, albums, and tracks.
#[utoipa::path(
    get,
    path = "/api/v3/library/artists/duplicate-groups/{group_id}",
    params(("group_id" = String, Path, description = "Group id")),
    responses(
        (status = 200, description = "Group detail", body = ArtistDuplicateGroupDetail),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "The group no longer exists"),
    )
)]
pub async fn get_artist_duplicate_group(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    Path(group_id): Path<String>,
) -> Result<Json<ArtistDuplicateGroupDetail>, ReconcileHttpError> {
    let service = Reconcile::new(&state);
    let detail = blocking(move || service.detail(&group_id)).await?;
    Ok(Json(detail.into()))
}

/// Mark a group's records as distinct people. The group comes back if any
/// member changes or another record with the name appears.
#[utoipa::path(
    post,
    path = "/api/v3/library/artists/duplicate-groups/{group_id}/dismiss",
    params(("group_id" = String, Path, description = "Group id")),
    request_body = ArtistDuplicateGroupDismissRequest,
    responses(
        (status = 200, description = "Group dismissed", body = ArtistDuplicateGroupDismissResponse),
        (status = 400, description = "Bad body, or the group was merged automatically"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "The group no longer exists"),
        (status = 409, description = "A member changed since the group was read"),
    )
)]
pub async fn dismiss_artist_duplicate_group(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(group_id): Path<String>,
    ValidJson(body): ValidJson<ArtistDuplicateGroupDismissRequest>,
) -> Result<Json<ArtistDuplicateGroupDismissResponse>, ReconcileHttpError> {
    let service = Reconcile::new(&state);
    let user_id = caller.0.user_id;
    let id = group_id.clone();
    let pairs =
        blocking(move || service.dismiss(&id, &body.expected_member_revisions, &user_id)).await?;
    Ok(Json(ArtistDuplicateGroupDismissResponse {
        group_id,
        dismissed_pairs: pairs,
    }))
}
