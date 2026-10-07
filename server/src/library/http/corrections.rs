//! Catalog correction routes: split, merge and move tracks between albums,
//! reset manual grouping, and merge duplicate artists. Each has a preview
//! route and an apply route; the apply takes the preview's token.
//!
//! Every route is admin-only, as in v2. Store work runs on
//! `spawn_blocking`.

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::auth::RequireAdmin;
use super::error::{LibraryError, ValidJson};
use super::operations::ReasonView;
use crate::library::corrections::Corrections;
use crate::library::corrections::models::{
    Applied, ApplyMeta, ArtistMergeOutcome, ArtistMergeRequest, CorrectionError, IdentityChoice,
    MembershipKind, MembershipOutcome, MembershipRequest, ProviderChoice,
};
use crate::library::wiring::LibrarySetup;

/// Correction routes, relative for nesting under `/api/v3`.
pub fn corrections_router() -> Router<LibrarySetup> {
    Router::new()
        .route(
            "/library/albums/{album_id}/split-preview",
            post(preview_album_split),
        )
        .route("/library/albums/{album_id}/split", post(apply_album_split))
        .route("/library/albums/merge-preview", post(preview_album_merge))
        .route("/library/albums/merge", post(apply_album_merge))
        .route("/library/tracks/move-preview", post(preview_track_move))
        .route("/library/tracks/move", post(apply_track_move))
        .route(
            "/library/albums/{album_id}/reset-grouping-preview",
            post(preview_grouping_reset),
        )
        .route(
            "/library/albums/{album_id}/reset-grouping",
            post(apply_grouping_reset),
        )
        .route("/library/artists/merge-preview", post(preview_artist_merge))
        .route("/library/artists/merge", post(apply_artist_merge))
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/// A refused correction in the shared error envelope, with the action in
/// `details.action`.
pub struct CorrectionHttpError(CorrectionError);

impl IntoResponse for CorrectionHttpError {
    fn into_response(self) -> Response {
        use crate::error::envelope_response;
        let (status, reason) = match self.0 {
            CorrectionError::NotFound(reason) => (StatusCode::NOT_FOUND, reason),
            CorrectionError::Invalid(reason) => (StatusCode::BAD_REQUEST, reason),
            CorrectionError::Conflict(reason) => (StatusCode::CONFLICT, reason),
            CorrectionError::Store(cause) => return LibraryError::internal(&cause).into_response(),
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
    work: impl FnOnce() -> Result<T, CorrectionError> + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| LibraryError::internal(&cause).into_response())?
        .map_err(|error| CorrectionHttpError(error).into_response())
}

// ---------------------------------------------------------------------------
// DTOs.
// ---------------------------------------------------------------------------

/// What the receiving album keeps when the combined albums name different
/// editions.
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentityChoiceBody {
    /// Drop every competing edition; the album is identified again.
    #[default]
    Detach,
    /// The receiving album keeps its own edition.
    RetainManual,
}

impl From<IdentityChoiceBody> for IdentityChoice {
    fn from(body: IdentityChoiceBody) -> Self {
        match body {
            IdentityChoiceBody::Detach => Self::Detach,
            IdentityChoiceBody::RetainManual => Self::RetainManual,
        }
    }
}

/// What the surviving artist keeps when the merged artists name different
/// MusicBrainz artists.
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderChoiceBody {
    /// The survivor loses its MusicBrainz link.
    Detach,
    /// The survivor keeps its link, or takes the only one on offer.
    #[default]
    RetainSurvivor,
}

impl From<ProviderChoiceBody> for ProviderChoice {
    fn from(body: ProviderChoiceBody) -> Self {
        match body {
            ProviderChoiceBody::Detach => Self::Detach,
            ProviderChoiceBody::RetainSurvivor => Self::RetainSurvivor,
        }
    }
}

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
    pub identity_choice: IdentityChoiceBody,
}

/// A previewed membership change to apply.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MembershipApplyBody {
    pub track_ids: Vec<String>,
    #[serde(default)]
    pub expected_album_revisions: BTreeMap<String, i64>,
    #[serde(default)]
    pub target_album_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub album_artist_name: Option<String>,
    #[serde(default)]
    pub identity_choice: IdentityChoiceBody,
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
    pub provider_choice: ProviderChoiceBody,
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
    /// `kept`, `moved`, `cleared` or `remap_queued`.
    pub change: String,
    /// For a moved edition, the album it came from.
    pub from_album_id: Option<String>,
    pub release_mbid: Option<String>,
    pub reason: ReasonView,
}

/// A correction preview. Album and artist previews share this shape, as
/// in v2.
#[derive(Debug, Serialize, ToSchema)]
pub struct MembershipPreviewResponse {
    /// Send this back to apply exactly this change (valid 15 minutes).
    pub preview_token: String,
    pub source_album_ids: Vec<String>,
    /// The receiving album (the surviving artist for an artist merge).
    pub target_album_id: Option<String>,
    pub track_ids: Vec<String>,
    /// Competing editions or MusicBrainz artists.
    pub identity_conflicts: Vec<String>,
    /// Ids that will point at the receiving album or artist.
    pub aliases: Vec<String>,
    pub automatic_groups: Vec<AutomaticGroupView>,
    /// Artist merge: references that move to the survivor, by kind.
    pub reference_counts: BTreeMap<String, i64>,
    pub edition_changes: Vec<EditionChangeView>,
}

/// An applied correction.
#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogCorrectionResponse {
    /// `split`, `merge`, `move`, `reset` or `merge_artist`.
    pub kind: String,
    pub track_ids: Vec<String>,
    pub source_album_ids: Vec<String>,
    pub target_album_id: Option<String>,
    pub surviving_artist_id: Option<String>,
    pub retired_artist_ids: Vec<String>,
    pub catalog_revision: i64,
}

impl From<Applied> for CatalogCorrectionResponse {
    fn from(applied: Applied) -> Self {
        Self {
            kind: applied.kind,
            track_ids: applied.track_ids,
            source_album_ids: applied.source_album_ids,
            target_album_id: applied.target_album_id,
            surviving_artist_id: applied.surviving_artist_id,
            retired_artist_ids: applied.retired_artist_ids,
            catalog_revision: applied.catalog_revision,
        }
    }
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
                change: change.change.as_str().to_owned(),
                from_album_id: change.from_album_id,
                release_mbid: change.release_mbid,
                reason: change.reason.into(),
            })
            .collect(),
    }
}

fn artist_view(token: String, outcome: ArtistMergeOutcome) -> MembershipPreviewResponse {
    MembershipPreviewResponse {
        preview_token: token,
        source_album_ids: Vec::new(),
        target_album_id: Some(outcome.surviving_artist_id),
        track_ids: Vec::new(),
        identity_conflicts: outcome.identity_conflicts,
        aliases: outcome.retired_artist_ids,
        automatic_groups: Vec::new(),
        reference_counts: outcome.reference_counts,
        edition_changes: Vec::new(),
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
        identity_choice: body.identity_choice.into(),
    }
}

fn apply_request(
    kind: MembershipKind,
    album_id: Option<String>,
    body: MembershipApplyBody,
    actor: String,
) -> (MembershipRequest, ApplyMeta) {
    (
        MembershipRequest {
            kind,
            album_id,
            track_ids: body.track_ids,
            expected_album_revisions: body.expected_album_revisions,
            target_album_id: body.target_album_id,
            title: body.title,
            album_artist_name: body.album_artist_name,
            identity_choice: body.identity_choice.into(),
        },
        ApplyMeta {
            preview_token: body.preview_token,
            idempotency_key: body.idempotency_key.filter(|key| !key.trim().is_empty()),
            actor,
        },
    )
}

async fn preview_membership(
    state: LibrarySetup,
    actor: String,
    request: MembershipRequest,
) -> Result<Json<MembershipPreviewResponse>, Response> {
    let corrections = Corrections::new(&state);
    let previewed = blocking(move || corrections.preview_membership(&request, &actor)).await?;
    Ok(Json(membership_view(previewed.token, previewed.outcome)))
}

async fn apply_membership(
    state: LibrarySetup,
    request: MembershipRequest,
    meta: ApplyMeta,
) -> Result<Json<CatalogCorrectionResponse>, Response> {
    let corrections = Corrections::new(&state);
    let applied = blocking(move || corrections.apply_membership(&request, &meta)).await?;
    Ok(Json(applied.into()))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Preview splitting selected tracks off an album.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, Response> {
    let request = preview_request(MembershipKind::Split, Some(album_id), body);
    preview_membership(state, caller.0.user_id, request).await
}

/// Split selected tracks off an album into a new album (or into the target).
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/split",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Split applied", body = CatalogCorrectionResponse),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "The preview expired or the albums changed since"),
    )
)]
pub async fn apply_album_split(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, Response> {
    let (request, meta) = apply_request(
        MembershipKind::Split,
        Some(album_id),
        body,
        caller.0.user_id,
    );
    apply_membership(state, request, meta).await
}

/// Preview folding whole albums into the target album.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, Response> {
    let request = preview_request(MembershipKind::Merge, None, body);
    preview_membership(state, caller.0.user_id, request).await
}

/// Fold whole albums into the target album.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/merge",
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Merge applied", body = CatalogCorrectionResponse),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "The preview expired or the albums changed since"),
    )
)]
pub async fn apply_album_merge(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, Response> {
    let (request, meta) = apply_request(MembershipKind::Merge, None, body, caller.0.user_id);
    apply_membership(state, request, meta).await
}

/// Preview moving selected tracks onto the target album.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, Response> {
    let request = preview_request(MembershipKind::Move, None, body);
    preview_membership(state, caller.0.user_id, request).await
}

/// Move selected tracks onto the target album.
#[utoipa::path(
    post,
    path = "/api/v3/library/tracks/move",
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Move applied", body = CatalogCorrectionResponse),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, track or target"),
        (status = 409, description = "The preview expired or the albums changed since"),
    )
)]
pub async fn apply_track_move(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, Response> {
    let (request, meta) = apply_request(MembershipKind::Move, None, body, caller.0.user_id);
    apply_membership(state, request, meta).await
}

/// Preview handing an album's tracks back to automatic grouping.
#[utoipa::path(
    post,
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
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, Response> {
    let request = preview_request(MembershipKind::Reset, Some(album_id), body);
    preview_membership(state, caller.0.user_id, request).await
}

/// Hand an album's tracks back to automatic grouping.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/reset-grouping",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = MembershipApplyBody,
    responses(
        (status = 200, description = "Reset applied", body = CatalogCorrectionResponse),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album or track"),
        (status = 409, description = "The preview expired or the album changed since"),
    )
)]
pub async fn apply_grouping_reset(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, Response> {
    let (request, meta) = apply_request(
        MembershipKind::Reset,
        Some(album_id),
        body,
        caller.0.user_id,
    );
    apply_membership(state, request, meta).await
}

fn artist_request(
    source_artist_ids: Vec<String>,
    surviving_artist_id: String,
    expected_revisions: BTreeMap<String, i64>,
    provider_choice: ProviderChoiceBody,
) -> ArtistMergeRequest {
    ArtistMergeRequest {
        source_artist_ids,
        surviving_artist_id,
        expected_revisions,
        provider_choice: provider_choice.into(),
    }
}

/// Preview folding duplicate artists into one survivor.
#[utoipa::path(
    post,
    path = "/api/v3/library/artists/merge-preview",
    request_body = ArtistMergePreviewBody,
    responses(
        (status = 200, description = "What the merge would move", body = MembershipPreviewResponse),
        (status = 400, description = "No duplicate chosen, or a reserved artist would be merged away"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown or already merged artist"),
        (status = 409, description = "An artist changed since the page loaded"),
    )
)]
pub async fn preview_artist_merge(
    State(state): State<LibrarySetup>,
    _caller: RequireAdmin,
    ValidJson(body): ValidJson<ArtistMergePreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, Response> {
    let corrections = Corrections::new(&state);
    let request = artist_request(
        body.source_artist_ids,
        body.surviving_artist_id,
        body.expected_revisions,
        ProviderChoiceBody::default(),
    );
    let previewed = blocking(move || corrections.preview_artist_merge(&request)).await?;
    Ok(Json(artist_view(previewed.token, previewed.outcome)))
}

/// Fold duplicate artists into one survivor.
#[utoipa::path(
    post,
    path = "/api/v3/library/artists/merge",
    request_body = ArtistMergeApplyBody,
    responses(
        (status = 200, description = "Artists merged", body = CatalogCorrectionResponse),
        (status = 400, description = "Bad selection or token"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown or already merged artist"),
        (status = 409, description = "The preview expired or an artist changed since"),
    )
)]
pub async fn apply_artist_merge(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    ValidJson(body): ValidJson<ArtistMergeApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, Response> {
    let corrections = Corrections::new(&state);
    let request = artist_request(
        body.source_artist_ids,
        body.surviving_artist_id,
        body.expected_revisions,
        body.provider_choice,
    );
    let meta = ApplyMeta {
        preview_token: body.preview_token,
        idempotency_key: body.idempotency_key.filter(|key| !key.trim().is_empty()),
        actor: caller.0.user_id,
    };
    let applied = blocking(move || corrections.apply_artist_merge(&request, &meta)).await?;
    Ok(Json(applied.into()))
}
