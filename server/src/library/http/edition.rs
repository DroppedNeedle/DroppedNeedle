//! Edition routes: one album's edition and the person's say over it.
//!
//! - `GET /library/albums/{id}/edition`: where the edition stands and why.
//! - `PUT /library/albums/{id}/edition`: choose any release as the edition.
//! - `DELETE /library/albums/{id}/edition`: "Let DroppedNeedle choose".
//! - `POST /library/albums/{id}/edition/confirm`: "Looks right".
//! - `POST /library/albums/{id}/edition/undo`: take back the last change.
//! - `GET /library/unconfirmed`: albums whose match waits for a look.
//!
//! Changing an edition is for curators (admins and trusted users), as
//! pinning was; reading is open to every signed-in user.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::auth::{Principal, RequireCurator};
use super::error::{LibraryError, ValidJson, ValidQuery};
use super::models::PenaltyView;
use super::operations::{OperationsHttpError, ReasonView, blocking};
use crate::library::identify::models::CandidateEvidence;
use crate::library::operations::models::{EditionChoice, OperationError};
use crate::library::operations::reasons;
use crate::library::operations::service::Operations;
use crate::library::operations::status::{self, EditionStatus, WaitingAlbum};
use crate::library::wiring::LibrarySetup;

/// Largest page of the unconfirmed list.
const PAGE_MAX: u32 = 200;

/// Edition routes, relative for nesting under `/api/v3`.
pub fn edition_router() -> Router<LibrarySetup> {
    Router::new()
        .route(
            "/library/albums/{album_id}/edition",
            get(get_edition)
                .put(choose_edition)
                .delete(hand_back_edition),
        )
        .route(
            "/library/albums/{album_id}/edition/confirm",
            post(confirm_edition),
        )
        .route(
            "/library/albums/{album_id}/edition/undo",
            post(undo_edition),
        )
        .route("/library/unconfirmed", get(list_unconfirmed))
}

/// One candidate the matcher scored, as the edition picker shows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionCandidateView {
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub album_title: String,
    pub album_artist_name: String,
    /// Files the release accounts for.
    pub matched_files: usize,
    /// One minus the distance.
    pub score: f64,
    /// Matcher distance, 0 for a perfect match.
    pub distance: f64,
    /// What the distance is made of, largest share first.
    pub penalties: Vec<PenaltyView>,
}

impl From<&CandidateEvidence> for EditionCandidateView {
    fn from(candidate: &CandidateEvidence) -> Self {
        Self {
            release_group_mbid: candidate.release_group_mbid.clone(),
            release_mbid: candidate.release_mbid.clone(),
            album_title: candidate.album_title.clone(),
            album_artist_name: candidate.album_artist_name.clone(),
            matched_files: candidate.supported_count(),
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
        }
    }
}

/// Where one album's edition stands.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionStatusView {
    pub album_id: String,
    /// `chosen` (by a person), `confirmed`, `unconfirmed` (a best guess),
    /// `unmatched` (keeps its own tags), or `unidentified`.
    pub state: String,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    /// Who chose the edition, for a chosen one.
    pub chosen_by_user_id: Option<String>,
    /// When it was chosen (epoch seconds), for a chosen one.
    pub chosen_at: Option<f64>,
    /// Why the edition is what it is, and what to do about it.
    pub reason: ReasonView,
    /// The closest candidates, best first (for a guess or no match).
    pub candidates: Vec<EditionCandidateView>,
    /// The last edition change can still be taken back.
    pub undo_available: bool,
}

impl From<EditionStatus> for EditionStatusView {
    fn from(status: EditionStatus) -> Self {
        Self {
            album_id: status.local_album_id,
            state: status.state.as_str().to_owned(),
            release_group_mbid: status.release_group_mbid,
            release_mbid: status.release_mbid,
            chosen_by_user_id: status.chosen_by_user_id,
            chosen_at: status.chosen_at,
            reason: status.reason.into(),
            candidates: status.candidates.iter().map(Into::into).collect(),
            undo_available: status.undo_available,
        }
    }
}

/// Choose an edition.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ChooseEditionBody {
    /// Any MusicBrainz release, from this album's release group or another.
    pub release_mbid: String,
}

/// A file a retag would rewrite to the chosen edition's tags.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RetagFileView {
    pub track_id: String,
    pub root_id: String,
    pub rel_path: String,
}

/// What choosing an edition did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionChoiceView {
    pub album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: String,
    /// Files placed on a track of the edition: pass these to a retag
    /// preview to write the edition's tags into them.
    pub retag_files: Vec<RetagFileView>,
    /// Files the edition has no track for. They keep their recording.
    pub extra_track_ids: Vec<String>,
    /// Titles of the edition's tracks no file holds.
    pub missing_titles: Vec<String>,
    /// The album's edition as it now stands.
    pub status: EditionStatusView,
}

/// Paging for the unconfirmed list.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct UnconfirmedQuery {
    /// `unconfirmed` (default) or `unmatched`.
    pub state: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// One album whose match waits for a look.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WaitingAlbumView {
    pub album_id: String,
    pub title: String,
    pub artist_name: String,
    /// `unconfirmed` or `unmatched`.
    pub state: String,
    pub release_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
    pub reason: ReasonView,
    pub updated_at: f64,
}

impl From<WaitingAlbum> for WaitingAlbumView {
    fn from(album: WaitingAlbum) -> Self {
        Self {
            album_id: album.local_album_id,
            title: album.title,
            artist_name: album.artist_name,
            state: album.state.as_str().to_owned(),
            release_mbid: album.release_mbid,
            release_group_mbid: album.release_group_mbid,
            reason: album.reason.into(),
            updated_at: album.updated_at,
        }
    }
}

/// One page of albums waiting for a look.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WaitingAlbumsResponse {
    pub items: Vec<WaitingAlbumView>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

fn read_status(ops: &Operations, album_id: &str) -> Result<EditionStatusView, OperationError> {
    ops.read_with(|conn| status::edition_status(conn, album_id))?
        .map(Into::into)
        .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))
}

async fn status_of(
    state: &LibrarySetup,
    album_id: &str,
) -> Result<EditionStatusView, OperationsHttpError> {
    let ops = Operations::new(state);
    let album_id = album_id.to_owned();
    blocking(move || read_status(&ops, &album_id)).await
}

/// Where the album's edition stands, and why.
#[utoipa::path(
    get,
    path = "/api/v3/library/albums/{album_id}/edition",
    params(("album_id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "The album's edition", body = EditionStatusView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album"),
    )
)]
pub async fn get_edition(
    State(state): State<LibrarySetup>,
    _caller: Principal,
    Path(album_id): Path<String>,
) -> Result<Json<EditionStatusView>, OperationsHttpError> {
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Choose the album's edition: any release, from this album's release
/// group or another. Files the edition has no track for keep their
/// recording; nothing automatic changes the choice afterwards.
#[utoipa::path(
    put,
    path = "/api/v3/library/albums/{album_id}/edition",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = ChooseEditionBody,
    responses(
        (status = 200, description = "The choice and what it did", body = EditionChoiceView),
        (status = 400, description = "Not a release id, or the release fits none of the files"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album, or MusicBrainz does not know the release"),
        (status = 503, description = "MusicBrainz is not answering"),
    )
)]
pub async fn choose_edition(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<ChooseEditionBody>,
) -> Result<Json<EditionChoiceView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let choice: EditionChoice = ops
        .choose_edition(&album_id, &body.release_mbid, Some(&caller.0.user_id))
        .await?;
    let status = status_of(&state, &album_id).await?;
    Ok(Json(EditionChoiceView {
        album_id: choice.local_album_id,
        release_group_mbid: choice.release_group_mbid,
        release_mbid: choice.release_mbid,
        retag_files: choice
            .placed
            .into_iter()
            .map(|file| RetagFileView {
                track_id: file.local_track_id,
                root_id: file.root_id,
                rel_path: file.relative_path,
            })
            .collect(),
        extra_track_ids: choice.extra_track_ids,
        missing_titles: choice.missing_titles,
        status,
    }))
}

/// "Let DroppedNeedle choose": drop the person's choice and pick the best
/// fit for the files again.
#[utoipa::path(
    delete,
    path = "/api/v3/library/albums/{album_id}/edition",
    params(("album_id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "The album's edition, identification queued", body = EditionStatusView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album"),
    )
)]
pub async fn hand_back_edition(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(album_id): Path<String>,
) -> Result<Json<EditionStatusView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let (album, user) = (album_id.clone(), caller.0.user_id);
    blocking(move || ops.hand_back_edition(&album, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// "Looks right": confirm the album's unconfirmed best guess.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/edition/confirm",
    params(("album_id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "The album's edition", body = EditionStatusView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album"),
        (status = 409, description = "Nothing waits for confirmation"),
    )
)]
pub async fn confirm_edition(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(album_id): Path<String>,
) -> Result<Json<EditionStatusView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let (album, user) = (album_id.clone(), caller.0.user_id);
    blocking(move || ops.confirm_match(&album, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Take back the album's last edition change.
#[utoipa::path(
    post,
    path = "/api/v3/library/albums/{album_id}/edition/undo",
    params(("album_id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "The album's edition", body = EditionStatusView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "No change to take back"),
        (status = 409, description = "The edition changed again since"),
    )
)]
pub async fn undo_edition(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(album_id): Path<String>,
) -> Result<Json<EditionStatusView>, OperationsHttpError> {
    let ops = Operations::new(&state);
    let (album, user) = (album_id.clone(), caller.0.user_id);
    blocking(move || ops.undo_edition_choice(&album, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Albums whose match is a best guess waiting for a look (or, with
/// `state=unmatched`, albums nothing fits). Newest first.
#[utoipa::path(
    get,
    path = "/api/v3/library/unconfirmed",
    params(UnconfirmedQuery),
    responses(
        (status = 200, description = "One page of albums", body = WaitingAlbumsResponse),
        (status = 400, description = "Unknown state"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_unconfirmed(
    State(state): State<LibrarySetup>,
    _caller: Principal,
    ValidQuery(query): ValidQuery<UnconfirmedQuery>,
) -> Result<Json<WaitingAlbumsResponse>, OperationsHttpError> {
    let unmatched = match query.state.as_deref() {
        None | Some("unconfirmed") => false,
        Some("unmatched") => true,
        Some(_) => {
            return Err(LibraryError::InvalidInput {
                message: "state must be unconfirmed or unmatched".to_owned(),
            }
            .into());
        }
    };
    let limit = query.limit.unwrap_or(50).clamp(1, PAGE_MAX);
    let offset = query.offset.unwrap_or(0);
    let ops = Operations::new(&state);
    let (items, total) = blocking(move || {
        ops.read_with(|conn| status::waiting_albums(conn, unmatched, limit, offset))
    })
    .await?;
    Ok(Json(WaitingAlbumsResponse {
        items: items.into_iter().map(Into::into).collect(),
        total,
        limit,
        offset,
    }))
}
