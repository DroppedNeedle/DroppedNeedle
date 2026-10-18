//! Catalog correction routes: split, merge and move tracks between albums,
//! reset manual grouping, and merge duplicate artists. Each has a preview
//! route and an apply route; the apply takes the preview's token. Beside
//! them, the album rows a scan would not join by itself, which a person
//! joins or dismisses.
//!
//! Every route is admin-only, as in v2. Store work runs on
//! `spawn_blocking`.

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::refused;
use super::operations::ReasonView;
use crate::corrections::Corrections;
use crate::corrections::models::{
    Applied, ApplyMeta, ArtistMergeOutcome, ArtistMergeRequest, CorrectionError, IdentityChoice,
    MembershipKind, MembershipOutcome, MembershipRequest, PairEdition, PairJoinRequest, PairReason,
    PairSide, ProviderChoice, RetagPair,
};
use crate::wiring::LibrarySetup;
use droppedneedle_auth::Admin;

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
        .route("/library/retag-pairs", get(list_retag_pairs))
        .route(
            "/library/retag-pairs/{old_album_id}/{new_album_id}/join",
            post(join_retag_pair),
        )
        .route(
            "/library/retag-pairs/{old_album_id}/{new_album_id}/dismiss",
            post(dismiss_retag_pair),
        )
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

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CorrectionError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::from)
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
    /// What the survivor keeps when the artists name different MusicBrainz
    /// artists; the apply must send the same choice.
    #[serde(default)]
    pub provider_choice: ProviderChoiceBody,
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
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let corrections = Corrections::new(&state);
    let previewed = blocking(move || corrections.preview_membership(&request, &actor)).await?;
    Ok(Json(membership_view(previewed.token, previewed.outcome)))
}

async fn apply_membership(
    state: LibrarySetup,
    request: MembershipRequest,
    meta: ApplyMeta,
) -> Result<Json<CatalogCorrectionResponse>, ApiError> {
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
    State(state): State<LibrarySetup>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Split, Some(album_id), body);
    preview_membership(state, caller.user_id().to_owned(), request).await
}

/// Split selected tracks off an album into a new album (or into the target).
#[utoipa::path(
    post,
    tag = "library management",
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
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Split,
        Some(album_id),
        body,
        caller.user_id().to_owned(),
    );
    apply_membership(state, request, meta).await
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
    State(state): State<LibrarySetup>,
    caller: Admin,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Merge, None, body);
    preview_membership(state, caller.user_id().to_owned(), request).await
}

/// Fold whole albums into the target album.
#[utoipa::path(
    post,
    tag = "library management",
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
    caller: Admin,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Merge,
        None,
        body,
        caller.user_id().to_owned(),
    );
    apply_membership(state, request, meta).await
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
    State(state): State<LibrarySetup>,
    caller: Admin,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Move, None, body);
    preview_membership(state, caller.user_id().to_owned(), request).await
}

/// Move selected tracks onto the target album.
#[utoipa::path(
    post,
    tag = "library management",
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
    caller: Admin,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Move,
        None,
        body,
        caller.user_id().to_owned(),
    );
    apply_membership(state, request, meta).await
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
    State(state): State<LibrarySetup>,
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipPreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let request = preview_request(MembershipKind::Reset, Some(album_id), body);
    preview_membership(state, caller.user_id().to_owned(), request).await
}

/// Hand an album's tracks back to automatic grouping.
#[utoipa::path(
    post,
    tag = "library management",
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
    caller: Admin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<MembershipApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, ApiError> {
    let (request, meta) = apply_request(
        MembershipKind::Reset,
        Some(album_id),
        body,
        caller.user_id().to_owned(),
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
    tag = "library management",
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
    caller: Admin,
    ValidJson(body): ValidJson<ArtistMergePreviewBody>,
) -> Result<Json<MembershipPreviewResponse>, ApiError> {
    let corrections = Corrections::new(&state);
    let request = artist_request(
        body.source_artist_ids,
        body.surviving_artist_id,
        body.expected_revisions,
        body.provider_choice,
    );
    let actor = caller.user_id().to_owned();
    let previewed = blocking(move || corrections.preview_artist_merge(&request, &actor)).await?;
    Ok(Json(artist_view(previewed.token, previewed.outcome)))
}

/// Fold duplicate artists into one survivor.
#[utoipa::path(
    post,
    tag = "library management",
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
    caller: Admin,
    ValidJson(body): ValidJson<ArtistMergeApplyBody>,
) -> Result<Json<CatalogCorrectionResponse>, ApiError> {
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
        actor: caller.user_id().to_owned(),
    };
    let applied = blocking(move || corrections.apply_artist_merge(&request, &meta)).await?;
    Ok(Json(applied.into()))
}

// ---------------------------------------------------------------------------
// Retag pairs.
// ---------------------------------------------------------------------------

/// An album's edition on a retag pair.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetagPairEditionView {
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    /// Chosen by a person or carried over from v2.
    pub chosen: bool,
    /// A custom edition is active. It does not carry over a join.
    pub custom: bool,
}

/// Why the scan left a retag pair for a person.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetagPairReasonView {
    /// Each album holds a different edition a person chose.
    ChosenMatchesDiffer,
    /// The album was split against a rename: the old album's edition
    /// goes to the named album only.
    TracksElsewhere,
    /// The old album holds a custom edition, which does not carry over.
    CustomEdition,
    /// The old album was left unmanaged, kept as a custom edition or
    /// stepped back from, and lost its files in the scan in which the new
    /// album's files appeared beside it. Nothing shows they are one
    /// album, so the new album is left alone until "Manage it again".
    ReplacedHandsOff,
}

/// One album of a retag pair.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetagPairAlbumView {
    pub id: String,
    pub title: String,
    pub album_artist_name: String,
    /// Indexed tracks.
    pub track_count: i64,
    pub edition: Option<RetagPairEditionView>,
    /// The edition's revision, 0 for none. Send it back with a join.
    pub identity_revision: i64,
}

/// Two album rows that look like one retagged album, which the scan would
/// not join by itself.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetagPairView {
    /// The row the files left. It holds the favorites, plays and links.
    pub old_album: RetagPairAlbumView,
    /// The row the files sit on now, or the album it was merged into
    /// since. A join keeps this one.
    pub new_album: RetagPairAlbumView,
    pub reason: RetagPairReasonView,
    /// Two different chosen editions remain once a custom edition on the
    /// old album is left out: the join must name the one to keep.
    pub editions_differ: bool,
    /// When the scan recorded the pair (Unix milliseconds).
    pub recorded_at: i64,
}

/// The open retag pairs, newest first (at most 100), and how many there
/// are.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetagPairListResponse {
    pub pairs: Vec<RetagPairView>,
    pub total: i64,
}

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

fn pair_album_view(side: PairSide) -> RetagPairAlbumView {
    RetagPairAlbumView {
        id: side.id,
        title: side.title,
        album_artist_name: side.album_artist_name,
        track_count: side.track_count,
        edition: side
            .edition
            .map(|edition: PairEdition| RetagPairEditionView {
                release_group_mbid: edition.release_group_mbid,
                release_mbid: edition.release_mbid,
                chosen: edition.chosen,
                custom: edition.custom,
            }),
        identity_revision: side.identity_revision,
    }
}

fn pair_view(pair: RetagPair) -> RetagPairView {
    RetagPairView {
        old_album: pair_album_view(pair.old_album),
        new_album: pair_album_view(pair.new_album),
        reason: match pair.reason {
            PairReason::ChosenMatchesDiffer => RetagPairReasonView::ChosenMatchesDiffer,
            PairReason::TracksElsewhere => RetagPairReasonView::TracksElsewhere,
            PairReason::CustomEdition => RetagPairReasonView::CustomEdition,
            PairReason::ReplacedHandsOff => RetagPairReasonView::ReplacedHandsOff,
        },
        editions_differ: pair.editions_differ,
        recorded_at: pair.recorded_at,
    }
}

/// List album rows that look like one retagged album and wait for a
/// person to join or dismiss them.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/retag-pairs",
    responses(
        (status = 200, description = "Open pairs, newest first", body = RetagPairListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_retag_pairs(
    State(state): State<LibrarySetup>,
    _caller: Admin,
) -> Result<Json<RetagPairListResponse>, ApiError> {
    let corrections = Corrections::new(&state);
    let list = blocking(move || corrections.retag_pairs()).await?;
    Ok(Json(RetagPairListResponse {
        pairs: list.pairs.into_iter().map(pair_view).collect(),
        total: list.total,
    }))
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
    State(state): State<LibrarySetup>,
    caller: Admin,
    Path((old_album_id, new_album_id)): Path<(String, String)>,
    ValidJson(body): ValidJson<RetagPairJoinBody>,
) -> Result<Json<RetagPairJoinResponse>, ApiError> {
    let corrections = Corrections::new(&state);
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
    State(state): State<LibrarySetup>,
    _caller: Admin,
    Path((old_album_id, new_album_id)): Path<(String, String)>,
) -> Result<Json<RetagPairDismissResponse>, ApiError> {
    let corrections = Corrections::new(&state);
    let dismissed =
        blocking(move || corrections.dismiss_retag_pair(&old_album_id, &new_album_id)).await?;
    Ok(Json(RetagPairDismissResponse { dismissed }))
}
