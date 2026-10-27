//! Catalog correction routes: split, merge and move tracks between albums,
//! reset manual grouping, and merge duplicate artists. Each has a preview
//! route and an apply route; the apply takes the preview's token. Beside
//! them, the album rows a scan would not join by itself, which a person
//! joins or dismisses.
//!
//! Every route is admin-only, as in v2. Store work runs on a blocking
//! thread.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::blocking;
use super::error::refused;
use super::operations::ReasonView;
use crate::corrections::Corrections;
use crate::corrections::models::{
    Applied, ApplyMeta, ArtistMergeOutcome, ArtistMergeRequest, CorrectionError, EditionChangeKind,
    IdentityChoice, MembershipKind, MembershipOutcome, MembershipRequest, PairJoinRequest,
    ProviderChoice, RetagPairList,
};
use crate::wiring::LibraryServices;
use droppedneedle_auth::Admin;

/// Correction routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .admin(routes!(preview_album_split))
        .admin(routes!(apply_album_split))
        .admin(routes!(preview_album_merge))
        .admin(routes!(apply_album_merge))
        .admin(routes!(preview_track_move))
        .admin(routes!(apply_track_move))
        .admin(routes!(preview_grouping_reset))
        .admin(routes!(apply_grouping_reset))
        .admin(routes!(preview_artist_merge))
        .admin(routes!(apply_artist_merge))
        .admin(routes!(list_retag_pairs))
        .admin(routes!(join_retag_pair))
        .admin(routes!(dismiss_retag_pair))
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

impl From<CorrectionError> for ApiError {
    fn from(error: CorrectionError) -> Self {
        match error {
            CorrectionError::NotFound(reason) => refused(StatusCode::NOT_FOUND, reason),
            CorrectionError::Invalid(reason) => refused(StatusCode::BAD_REQUEST, reason),
            CorrectionError::Conflict(reason) => refused(StatusCode::CONFLICT, reason),
            CorrectionError::Store(cause) => ApiError::internal(cause),
            CorrectionError::Busy(cause) => ApiError::unavailable(Unavailable::ServerBusy, cause),
        }
    }
}

// ---------------------------------------------------------------------------
// DTOs.
// ---------------------------------------------------------------------------

/// A membership change to preview.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MembershipPreviewBody {
    /// The tracks to split off, move, or reset. For a merge, any track of
    /// each album to fold in: merges always take whole albums.
    pub track_ids: Vec<String>,
    /// Album revisions as the page showed them, by album id. Zero or
    /// missing means unknown; the preview token still guards the change.
    #[serde(default)]
    pub expected_album_revisions: BTreeMap<String, i64>,
    /// The album that receives the tracks (merge and move; optional for a
    /// split, which otherwise makes a new album).
    #[serde(default)]
    pub target_album_id: Option<String>,
    /// Split only: the new album's title (default: the source's).
    #[serde(default)]
    pub title: Option<String>,
    /// Split only: the new album's album artist (default: the source's).
    #[serde(default)]
    pub album_artist_name: Option<String>,
    /// How competing editions settle; shown in the preview.
    #[serde(default)]
    pub identity_choice: IdentityChoice,
}

/// A previewed membership change to apply: the previewed change and its
/// token.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MembershipApplyBody {
    #[serde(flatten)]
    pub change: MembershipPreviewBody,
    /// The token the preview returned.
    pub preview_token: String,
    /// Replaying a key returns the first result instead of applying twice.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// An artist merge to preview.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ArtistMergePreviewBody {
    /// The duplicates (the survivor may be listed too).
    pub source_artist_ids: Vec<String>,
    /// The artist everything moves to.
    pub surviving_artist_id: String,
    /// Artist revisions as the page showed them; zero or missing means
    /// unknown.
    #[serde(default)]
    pub expected_revisions: BTreeMap<String, i64>,
    /// What the survivor keeps when the artists name different MusicBrainz
    /// artists; the apply must send the same choice.
    #[serde(default)]
    pub provider_choice: ProviderChoice,
}

/// A previewed artist merge to apply.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ArtistMergeApplyBody {
    pub source_artist_ids: Vec<String>,
    pub surviving_artist_id: String,
    #[serde(default)]
    pub expected_revisions: BTreeMap<String, i64>,
    /// The token the preview returned.
    pub preview_token: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub provider_choice: ProviderChoice,
}

/// Where a set of tracks ends up.
#[derive(Debug, Serialize, ToSchema)]
pub struct AutomaticGroupView {
    /// The receiving album; null for an album the change creates.
    pub local_album_id: Option<String>,
    pub title: String,
    pub album_artist_name: String,
    pub track_ids: Vec<String>,
    /// `MANUAL_SPLIT`, `MANUAL_MERGE`, `MANUAL_MOVE`, `AUTOMATIC_GROUPING`
    /// or `AUTOMATIC_NEW_ALBUM`.
    pub reason_code: String,
    pub created: bool,
}

/// What happens to one album's edition.
#[derive(Debug, Serialize, ToSchema)]
pub struct EditionChangeView {
    pub local_album_id: String,
    pub album_title: String,
    pub change: EditionChangeKind,
    /// For a moved edition, the album it came from.
    pub from_album_id: Option<String>,
    pub release_mbid: Option<String>,
    pub reason: ReasonView,
}

/// An album membership preview.
#[derive(Debug, Serialize, ToSchema)]
pub struct MembershipPreviewResponse {
    /// Send this back to apply exactly this change (valid 15 minutes).
    pub preview_token: String,
    pub source_album_ids: Vec<String>,
    /// The receiving album.
    pub target_album_id: Option<String>,
    pub track_ids: Vec<String>,
    /// Competing editions.
    pub identity_conflicts: Vec<String>,
    /// Album ids that will point at the receiving album.
    pub aliases: Vec<String>,
    pub automatic_groups: Vec<AutomaticGroupView>,
    /// Always empty for an album change.
    pub reference_counts: BTreeMap<String, i64>,
    pub edition_changes: Vec<EditionChangeView>,
}

/// An artist merge preview.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtistMergePreviewResponse {
    /// Send this back to apply exactly this merge (valid 15 minutes).
    pub preview_token: String,
    /// The artist everything moves to.
    pub surviving_artist_id: String,
    /// Competing MusicBrainz artists.
    pub identity_conflicts: Vec<String>,
    /// Artist ids that will point at the survivor.
    pub aliases: Vec<String>,
    /// References that move to the survivor, by kind.
    pub reference_counts: BTreeMap<String, i64>,
}

fn membership_view(token: String, outcome: MembershipOutcome) -> MembershipPreviewResponse {
    let created: Vec<&str> = outcome
        .groups
        .iter()
        .filter(|group| group.created)
        .map(|group| group.album_id.as_str())
        .collect();
    let hide = |id: &str| (!created.contains(&id)).then(|| id.to_owned());
    MembershipPreviewResponse {
        preview_token: token,
        source_album_ids: outcome.source_album_ids.clone(),
        target_album_id: outcome.target_album_id.clone(),
        track_ids: outcome.track_ids.clone(),
        identity_conflicts: outcome.identity_conflicts.clone(),
        aliases: outcome.retired.iter().map(|(id, _)| id.clone()).collect(),
        automatic_groups: outcome
            .groups
            .iter()
            .map(|group| AutomaticGroupView {
                local_album_id: hide(&group.album_id),
                title: group.title.clone(),
                album_artist_name: group.album_artist_name.clone(),
                track_ids: group.track_ids.clone(),
                reason_code: group.reason_code.to_owned(),
                created: group.created,
            })
            .collect(),
        reference_counts: BTreeMap::new(),
        edition_changes: outcome
            .edition_changes
            .into_iter()
            .map(|change| EditionChangeView {
                local_album_id: hide(&change.album_id).unwrap_or_default(),
                album_title: change.album_title,
                change: change.change,
                from_album_id: change.from_album_id,
                release_mbid: change.release_mbid,
                reason: change.reason.into(),
            })
            .collect(),
    }
}

fn artist_view(token: String, outcome: ArtistMergeOutcome) -> ArtistMergePreviewResponse {
    ArtistMergePreviewResponse {
        preview_token: token,
        surviving_artist_id: outcome.surviving_artist_id,
        identity_conflicts: outcome.identity_conflicts,
        aliases: outcome.retired_artist_ids,
        reference_counts: outcome.reference_counts,
    }
}

fn preview_request(
    kind: MembershipKind,
    album_id: Option<String>,
    body: MembershipPreviewBody,
) -> MembershipRequest {
    MembershipRequest {
        kind,
        album_id,
        track_ids: body.track_ids,
        expected_album_revisions: body.expected_album_revisions,
        target_album_id: body.target_album_id,
        title: body.title,
        album_artist_name: body.album_artist_name,
        identity_choice: body.identity_choice,
    }
}

fn apply_request(
    kind: MembershipKind,
    album_id: Option<String>,
    body: MembershipApplyBody,
    actor: String,
) -> (MembershipRequest, ApplyMeta) {
    (
        preview_request(kind, album_id, body.change),
        ApplyMeta {
            preview_token: body.preview_token,
            idempotency_key: body.idempotency_key.filter(|key| !key.trim().is_empty()),
            actor,
        },
    )
}

async fn preview_membership(
    corrections: Corrections,
    actor: String,
    request: MembershipRequest,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let previewed = blocking(move || corrections.preview_membership(&request, &actor)).await?;
    Ok(Json(membership_view(previewed.token, previewed.outcome)))
}

async fn apply_membership(
    corrections: Corrections,
    request: MembershipRequest,
    meta: ApplyMeta,
) -> Result<Json<Applied>, ApiError> {
    Ok(Json(
        blocking(move || corrections.apply_membership(&request, &meta)).await?,
    ))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Preview splitting selected tracks off an album.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/split-preview",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = MembershipPreviewBody,
    responses(
        (status = 200, description = "What the split would do", body = MembershipPreviewResponse),
        (status = 400, description = "No tracks, a track from another album, or nothing left behind"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "The album changed since the page loaded"),
    )
)]
pub async fn preview_album_split(
    State(corrections): State<Corrections>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Split, Some(album_id), body);
    preview_membership(corrections, caller.user_id().to_owned(), request).await
}

/// Split selected tracks off an album into a new album (or into the target).
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/split",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Split applied", body = Applied),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "The preview expired or the albums changed since"),
    )
)]
pub async fn apply_album_split(
    State(corrections): State<Corrections>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<Applied>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Split,
        Some(album_id),
        body,
        caller.user_id().to_owned(),
    );
    apply_membership(corrections, request, meta).await
}

/// Preview folding whole albums into the target album.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/merge-preview",
    request_body = MembershipPreviewBody,
    responses(
        (status = 200, description = "What the merge would do", body = MembershipPreviewResponse),
        (status = 400, description = "No tracks, no target, or the target is the source"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "An album changed since the page loaded"),
    )
)]
pub async fn preview_album_merge(
    State(corrections): State<Corrections>,
    caller: Admin,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Merge, None, body);
    preview_membership(corrections, caller.user_id().to_owned(), request).await
}

/// Fold whole albums into the target album.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/merge",
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Merge applied", body = Applied),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "The preview expired or the albums changed since"),
    )
)]
pub async fn apply_album_merge(
    State(corrections): State<Corrections>,
    caller: Admin,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<Applied>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Merge,
        None,
        body,
        caller.user_id().to_owned(),
    );
    apply_membership(corrections, request, meta).await
}

/// Preview moving selected tracks onto the target album.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/tracks/move-preview",
    request_body = MembershipPreviewBody,
    responses(
        (status = 200, description = "What the move would do", body = MembershipPreviewResponse),
        (status = 400, description = "No tracks, no target, or the tracks are already there"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "An album changed since the page loaded"),
    )
)]
pub async fn preview_track_move(
    State(corrections): State<Corrections>,
    caller: Admin,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Move, None, body);
    preview_membership(corrections, caller.user_id().to_owned(), request).await
}

/// Move selected tracks onto the target album.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/tracks/move",
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Move applied", body = Applied),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "The preview expired or the albums changed since"),
    )
)]
pub async fn apply_track_move(
    State(corrections): State<Corrections>,
    caller: Admin,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<Applied>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Move,
        None,
        body,
        caller.user_id().to_owned(),
    );
    apply_membership(corrections, request, meta).await
}

/// Preview handing an album's tracks back to automatic grouping.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/reset-grouping-preview",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = MembershipPreviewBody,
    responses(
        (status = 200, description = "Where the tracks would land", body = MembershipPreviewResponse),
        (status = 400, description = "No tracks, a track from another album, or nothing grouped by hand"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album or track"),
        (status = 409, description = "The album changed since the page loaded"),
    )
)]
pub async fn preview_grouping_reset(
    State(corrections): State<Corrections>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Reset, Some(album_id), body);
    preview_membership(corrections, caller.user_id().to_owned(), request).await
}

/// Hand an album's tracks back to automatic grouping.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/reset-grouping",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Reset applied", body = Applied),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album or track"),
        (status = 409, description = "The preview expired or the album changed since"),
    )
)]
pub async fn apply_grouping_reset(
    State(corrections): State<Corrections>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<Applied>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Reset,
        Some(album_id),
        body,
        caller.user_id().to_owned(),
    );
    apply_membership(corrections, request, meta).await
}

fn artist_request(
    source_artist_ids: Vec<String>,
    surviving_artist_id: String,
    expected_revisions: BTreeMap<String, i64>,
    provider_choice: ProviderChoice,
) -> ArtistMergeRequest {
    ArtistMergeRequest {
        source_artist_ids,
        surviving_artist_id,
        expected_revisions,
        provider_choice,
    }
}

/// Preview folding duplicate artists into one survivor.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/artists/merge-preview",
    request_body = ArtistMergePreviewBody,
    responses(
        (status = 200, description = "What the merge would move", body = ArtistMergePreviewResponse),
        (status = 400, description = "No duplicate chosen, or a reserved artist would be merged away"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown or already merged artist"),
        (status = 409, description = "An artist changed since the page loaded"),
    )
)]
pub async fn preview_artist_merge(
    State(corrections): State<Corrections>,
    caller: Admin,
    ValidJson(body): ValidJson<ArtistMergePreviewBody>,
) -> Result<Json<ArtistMergePreviewResponse>, ApiError> {
    let request = artist_request(
        body.source_artist_ids,
        body.surviving_artist_id,
        body.expected_revisions,
        body.provider_choice,
    );
    let previewed = blocking(move || corrections.preview_artist_merge(&request, &caller)).await?;
    Ok(Json(artist_view(previewed.token, previewed.outcome)))
}

/// Fold duplicate artists into one survivor.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/artists/merge",
    request_body = ArtistMergeApplyBody,
    responses(
        (status = 200, description = "Artists merged", body = Applied),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown or already merged artist"),
        (status = 409, description = "The preview expired or an artist changed since"),
    )
)]
pub async fn apply_artist_merge(
    State(corrections): State<Corrections>,
    caller: Admin,
    ValidJson(body): ValidJson<ArtistMergeApplyBody>,
) -> Result<Json<Applied>, ApiError> {
    let request = artist_request(
        body.source_artist_ids,
        body.surviving_artist_id,
        body.expected_revisions,
        body.provider_choice,
    );
    let meta = ApplyMeta {
        preview_token: body.preview_token,
        idempotency_key: body.idempotency_key.filter(|key| !key.trim().is_empty()),
        actor: caller.user_id().to_owned(),
    };
    Ok(Json(
        blocking(move || corrections.apply_artist_merge(&request, &meta, &caller)).await?,
    ))
}

// ---------------------------------------------------------------------------
// Retag pairs.
// ---------------------------------------------------------------------------

/// Join a retag pair.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RetagPairJoinBody {
    /// The album whose chosen edition stays. Required when
    /// `editions_differ` is true, and must be absent otherwise.
    #[serde(default)]
    pub keep_edition_of: Option<String>,
    /// Each album's `identity_revision` as shown, by album id. Both albums
    /// must be named.
    pub expected_identity_revisions: BTreeMap<String, i64>,
}

/// A joined retag pair.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetagPairJoinResponse {
    /// The album that stays. The old album's id now opens it.
    pub album_id: String,
}

/// A dismissed retag pair.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetagPairDismissResponse {
    /// False when the pair was dismissed already.
    pub dismissed: bool,
}

/// List album rows that look like one retagged album and wait for a
/// person to join or dismiss them.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/retag-pairs",
    responses(
        (status = 200, description = "Open pairs, newest first", body = RetagPairList),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_retag_pairs(
    State(corrections): State<Corrections>,
    _caller: Admin,
) -> Result<Json<RetagPairList>, ApiError> {
    Ok(Json(blocking(move || corrections.retag_pairs()).await?))
}

/// Join a retag pair: the old album is merged into the new one. A custom
/// edition on the old album is dropped, and so is the edition not kept
/// when the two hold different chosen editions; each leaves a review on
/// the album that stays. No file is written.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/retag-pairs/{old_album_id}/{new_album_id}/join",
    params(
        ("old_album_id" = String, Path, description = "The pair's old album"),
        ("new_album_id" = String, Path, description = "The pair's new album"),
    ),
    request_body = RetagPairJoinBody,
    responses(
        (status = 200, description = "Joined, or joined already", body = RetagPairJoinResponse),
        (status = 400, description = "An album's revision is missing, or the edition to keep is missing, not needed or not one of the two"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "No such pair"),
        (status = 409, description = "The pair was dismissed, an album was merged, removed or changed its edition, or work is open on one"),
    )
)]
pub async fn join_retag_pair(
    State(corrections): State<Corrections>,
    caller: Admin,
    Path((old_album_id, new_album_id)): Path<(String, String)>,
    ValidJson(body): ValidJson<RetagPairJoinBody>,
) -> Result<Json<RetagPairJoinResponse>, ApiError> {
    let request = PairJoinRequest {
        old_album_id,
        new_album_id,
        keep_edition_of: body.keep_edition_of,
        expected_identity_revisions: body.expected_identity_revisions,
        actor: caller.user_id().to_owned(),
    };
    let album_id = blocking(move || corrections.join_retag_pair(&request)).await?;
    Ok(Json(RetagPairJoinResponse { album_id }))
}

/// Mark a retag pair as two different albums. A scan never joins it then.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/retag-pairs/{old_album_id}/{new_album_id}/dismiss",
    params(
        ("old_album_id" = String, Path, description = "The pair's old album"),
        ("new_album_id" = String, Path, description = "The pair's new album"),
    ),
    responses(
        (status = 200, description = "Dismissed, or dismissed already", body = RetagPairDismissResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "No such pair"),
    )
)]
pub async fn dismiss_retag_pair(
    State(corrections): State<Corrections>,
    _caller: Admin,
    Path((old_album_id, new_album_id)): Path<(String, String)>,
) -> Result<Json<RetagPairDismissResponse>, ApiError> {
    let dismissed =
        blocking(move || corrections.dismiss_retag_pair(&old_album_id, &new_album_id)).await?;
    Ok(Json(RetagPairDismissResponse { dismissed }))
}
