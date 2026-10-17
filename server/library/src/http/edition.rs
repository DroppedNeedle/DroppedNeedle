//! Edition routes: one album's edition and the person's say over it.
//!
//! - `GET /library/albums/{id}/edition`: where the edition stands and why.
//! - `PUT /library/albums/{id}/edition`: choose any release as the edition.
//! - `DELETE /library/albums/{id}/edition`: "Let DroppedNeedle choose".
//! - `POST /library/albums/{id}/edition/confirm`: "Looks right".
//! - `POST /library/albums/{id}/edition/undo`: take back the last change.
//! - `POST /library/albums/{id}/edition/custom`: keep the files as they are
//!   as a custom edition of a release's group (admins).
//! - `POST /library/albums/{id}/edition/leave-unmanaged`: drop the exact
//!   edition and keep Library Management away from the album (admins).
//! - `GET /library/albums/{id}/edition/plan`: what taking an edition would
//!   do to the files (kept, duplicate, other version, extra) and which
//!   tracks are missing. Planning another release than the album's own is
//!   for curators.
//! - `POST /library/albums/{id}/edition/extras/remove` and `.../restore`:
//!   move picked extras to the recycle bin, and put them back.
//! - `PUT /library/albums/{id}/edition/files/{track_id}`: match one file
//!   to a track of the album's edition by hand.
//! - `DELETE /library/albums/{id}/edition/files/{track_id}`: undo that
//!   match.
//! - `GET /library/unconfirmed`: albums whose match waits for a look.
//!
//! Changing an edition is for curators (admins and trusted users), as
//! pinning was; reading is open to every signed-in user.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post, put},
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::auth::{Principal, RequireAdmin, RequireCurator};
use super::models::PenaltyView;
use super::operations::{ReasonView, blocking};
use crate::identify::models::CandidateEvidence;
use crate::management::automatic::AutomaticWrites;
use crate::management::service::Management;
use crate::mutations::Mutations;
use crate::operations::models::{EditionChoice, OperationError};
use crate::operations::plan::{FileFate, PlanReading};
use crate::operations::reasons;
use crate::operations::service::Operations;
use crate::operations::status::{self, EditionStatus, WaitingAlbum};
use crate::wiring::LibrarySetup;

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
        .route(
            "/library/albums/{album_id}/edition/custom",
            post(seal_custom_edition),
        )
        .route(
            "/library/albums/{album_id}/edition/leave-unmanaged",
            post(leave_unmanaged),
        )
        .route("/library/albums/{album_id}/edition/plan", get(plan_edition))
        .route(
            "/library/albums/{album_id}/edition/extras/remove",
            post(remove_extras),
        )
        .route(
            "/library/albums/{album_id}/edition/extras/restore",
            post(restore_extras),
        )
        .route(
            "/library/albums/{album_id}/edition/files/{track_id}",
            put(match_edition_file).delete(unmatch_edition_file),
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
    /// The identity's row revision, 0 when the album has none. A guarded
    /// write echoes it.
    pub identity_revision: i64,
    /// `release` (a MusicBrainz release), `custom` (a custom edition),
    /// `group_only` (a release group with no release chosen) or `none`.
    pub edition_kind: String,
    /// Set while the chosen edition's files are still being placed on it.
    pub placing: Option<EditionPlacingView>,
    /// An identification of the album is queued or running, as after
    /// "Let DroppedNeedle choose".
    pub identifying: bool,
    /// The app writes the edition's tags and file names by itself after a
    /// choice: the album's folder manages it automatically and nothing
    /// holds it back.
    pub files_by_itself: bool,
    /// That write is still to happen.
    pub writing: bool,
}

/// The chosen edition's files are still being placed on it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionPlacingView {
    /// Failed tries so far; above 0 means MusicBrainz did not answer.
    pub attempts: u32,
}

impl EditionStatusView {
    fn of(status: EditionStatus, writes: AutomaticWrites) -> Self {
        Self {
            identity_revision: status.identity_revision,
            edition_kind: status.edition_kind.as_str().to_owned(),
            placing: status
                .placing_attempts
                .map(|attempts| EditionPlacingView { attempts }),
            identifying: status.identifying,
            files_by_itself: writes.by_itself,
            writing: writes.pending,
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

/// Which edition to plan.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EditionPlanQuery {
    /// Any MusicBrainz release; the album's current edition when absent.
    pub release_mbid: Option<String>,
}

/// One track of the planned edition.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionPlanTrackView {
    pub disc_number: u32,
    pub position: u32,
    pub title: String,
    pub recording_mbid: String,
    pub release_track_mbid: String,
    pub length_ms: Option<u64>,
    /// The file that holds this track; absent when the track is missing.
    pub local_track_id: Option<String>,
}

/// One of the album's files and what the edition makes of it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionPlanFileView {
    pub local_track_id: String,
    pub title: String,
    pub disc_number: u32,
    pub track_number: u32,
    /// The file's length, when the scan read it.
    pub length_ms: Option<u64>,
    /// `keep` (sits on its own edition track), `duplicate` (another copy
    /// of a song the edition already has a file for), `other_version`
    /// (the same title and artist as an edition track no file holds, but
    /// not placed on it; that track still counts as missing) or `extra`
    /// (the edition has no track for it). Choosing never deletes a file.
    pub fate: String,
    /// The edition track the file keeps, duplicates or is another version
    /// of.
    pub edition_disc_number: Option<u32>,
    pub edition_position: Option<u32>,
    /// A sentence about the file's tags, when they need one:
    /// `TRACK_ID_CORRECTED` on a kept file whose track ID belongs to
    /// another edition (it is matched by its recording, and the ID is
    /// corrected when the album's MusicBrainz tags are written), or
    /// `TRACK_ID_ELSEWHERE` on a file that is not kept because its track ID
    /// belongs to another edition and it has no recording ID. Absent
    /// otherwise.
    pub note: Option<ReasonView>,
    /// A person matched this file to its edition track by hand, and the
    /// match stands: "Undo match" can take it back.
    pub matched_by_hand: bool,
    /// DroppedNeedle has written this file since a person matched it by
    /// hand, so "Undo match" is refused until that write is undone from
    /// History. False whenever `matched_by_hand` is false.
    pub match_written: bool,
}

/// What MusicBrainz says about the planned edition.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionFactsView {
    pub title: String,
    pub disambiguation: Option<String>,
    /// Release date as MusicBrainz gives it (`YYYY`, `YYYY-MM` or
    /// `YYYY-MM-DD`).
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    /// One format per medium that names one, in medium order.
    pub formats: Vec<String>,
    pub disc_count: u32,
    pub track_count: u32,
    /// The first label.
    pub label: Option<String>,
    /// The first catalog number.
    pub catalog_number: Option<String>,
    pub barcode: Option<String>,
}

/// What taking an edition would do to the album, before anything changes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionPlanView {
    pub album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: String,
    pub title: String,
    pub artist_name: String,
    /// The edition is already the album's edition.
    pub is_current: bool,
    /// The edition belongs to the album's current release group (false
    /// for an album with none).
    pub same_album: bool,
    /// The album identity's row revision the plan was read at, 0 when the
    /// album has none. Matching a file by hand echoes it.
    pub identity_revision: i64,
    pub edition: EditionFactsView,
    pub kept_count: usize,
    /// Edition tracks no file holds, including those a file is another
    /// version of.
    pub missing_count: usize,
    pub duplicate_count: usize,
    pub other_version_count: usize,
    pub extra_count: usize,
    pub tracks: Vec<EditionPlanTrackView>,
    pub files: Vec<EditionPlanFileView>,
}

impl From<PlanReading> for EditionPlanView {
    fn from(reading: PlanReading) -> Self {
        let PlanReading {
            plan,
            identity_revision,
            matched_by_hand,
            match_written,
        } = reading;
        let count = |fate: FileFate| plan.files.iter().filter(|file| file.fate == fate).count();
        let (kept_count, duplicate_count, other_version_count, extra_count) = (
            count(FileFate::Keep),
            count(FileFate::Duplicate),
            count(FileFate::OtherVersion),
            count(FileFate::Extra),
        );
        let missing_count = plan.missing().count();
        let edition = plan.edition;
        Self {
            kept_count,
            missing_count,
            duplicate_count,
            other_version_count,
            extra_count,
            same_album: plan.same_album,
            identity_revision,
            edition: EditionFactsView {
                title: edition.title,
                disambiguation: edition.disambiguation,
                date: edition.date,
                country: edition.country,
                status: edition.status,
                formats: edition.formats,
                disc_count: edition.disc_count,
                track_count: edition.track_count,
                label: edition.label,
                catalog_number: edition.catalog_number,
                barcode: edition.barcode,
            },
            album_id: plan.local_album_id,
            release_group_mbid: plan.release_group_mbid,
            release_mbid: plan.release_mbid,
            title: plan.title,
            artist_name: plan.artist_name,
            is_current: plan.is_current,
            tracks: plan
                .tracks
                .into_iter()
                .map(|track| EditionPlanTrackView {
                    disc_number: track.disc,
                    position: track.position,
                    title: track.title,
                    recording_mbid: track.recording_mbid,
                    release_track_mbid: track.release_track_mbid,
                    length_ms: track.length_ms,
                    local_track_id: track.local_track_id,
                })
                .collect(),
            files: plan
                .files
                .into_iter()
                .map(|file| EditionPlanFileView {
                    matched_by_hand: matched_by_hand.contains(&file.local_track_id),
                    match_written: match_written.contains(&file.local_track_id),
                    local_track_id: file.local_track_id,
                    title: file.title,
                    disc_number: file.disc,
                    track_number: file.track,
                    length_ms: file.length_ms,
                    fate: file.fate.as_str().to_owned(),
                    edition_disc_number: file.edition_track.map(|(disc, _)| disc),
                    edition_position: file.edition_track.map(|(_, position)| position),
                    note: file.note.map(ReasonView::from),
                })
                .collect(),
        }
    }
}

/// Paging for the unconfirmed list.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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

fn read_status(
    ops: &Operations,
    management: &Management,
    album_id: &str,
) -> Result<EditionStatusView, OperationError> {
    let status = ops
        .read_with(|conn| status::edition_status(conn, album_id))?
        .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))?;
    let writes = management
        .writes_by_itself(album_id)
        .map_err(|error| OperationError::Store(error.to_string()))?;
    Ok(EditionStatusView::of(status, writes))
}

async fn status_of(state: &LibrarySetup, album_id: &str) -> Result<EditionStatusView, ApiError> {
    let ops = Operations::new(state);
    let management = Management::new(state);
    let album_id = album_id.to_owned();
    blocking(move || read_status(&ops, &management, &album_id)).await
}

/// Where the album's edition stands, and why.
#[utoipa::path(
    get,
    tag = "library management",
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
) -> Result<Json<EditionStatusView>, ApiError> {
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Choose the album's edition: any release, from this album's release
/// group or another. Files the edition has no track for keep their
/// recording; nothing automatic changes the choice afterwards.
#[utoipa::path(
    put,
    tag = "library management",
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
) -> Result<Json<EditionChoiceView>, ApiError> {
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
    tag = "library management",
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
) -> Result<Json<EditionStatusView>, ApiError> {
    let ops = Operations::new(&state);
    let (album, user) = (album_id.clone(), caller.0.user_id);
    blocking(move || ops.hand_back_edition(&album, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// "Looks right": confirm the album's unconfirmed best guess.
#[utoipa::path(
    post,
    tag = "library management",
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
) -> Result<Json<EditionStatusView>, ApiError> {
    let ops = Operations::new(&state);
    let (album, user) = (album_id.clone(), caller.0.user_id);
    blocking(move || ops.confirm_match(&album, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Take back the album's last edition change.
#[utoipa::path(
    post,
    tag = "library management",
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
) -> Result<Json<EditionStatusView>, ApiError> {
    let ops = Operations::new(&state);
    let (album, user) = (album_id.clone(), caller.0.user_id);
    blocking(move || ops.undo_edition_choice(&album, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Seal a custom edition.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CustomEditionBody {
    /// A MusicBrainz release of the group the files are kept under.
    pub release_mbid: String,
    /// The `identity_revision` the edition status showed.
    pub expected_identity_revision: i64,
}

/// Leave the album unmanaged.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LeaveUnmanagedBody {
    /// The `identity_revision` the edition status showed.
    pub expected_identity_revision: i64,
}

/// Keep the album's files as they are, as a custom edition of the
/// release's group: the tags, positions and file list are frozen as they
/// are now, and the release group becomes the album's chosen match.
/// Undo with `.../edition/undo`.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/edition/custom",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = CustomEditionBody,
    responses(
        (status = 200, description = "The album's edition", body = EditionStatusView),
        (status = 400, description = "Not a release id, or the files cannot be sealed under that release group (CUSTOM_EDITION_NOT_SEALABLE)"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album, or MusicBrainz does not know the release"),
        (status = 409, description = "The album changed since it was read (STALE_REVISION)"),
        (status = 503, description = "MusicBrainz is not answering"),
    )
)]
pub async fn seal_custom_edition(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<CustomEditionBody>,
) -> Result<Json<EditionStatusView>, ApiError> {
    Operations::new(&state)
        .seal_custom_edition(
            &album_id,
            &body.release_mbid,
            body.expected_identity_revision,
            &caller.0.user_id,
        )
        .await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Leave the album unmanaged: its exact edition goes (the release group
/// stays as the person's choice) and Library Management leaves its files
/// alone until "Manage it again". Undo with `.../edition/undo`.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/edition/leave-unmanaged",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = LeaveUnmanagedBody,
    responses(
        (status = 200, description = "The album's edition", body = EditionStatusView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown album"),
        (status = 409, description = "The album's edition changed since it was read (STALE_REVISION)"),
    )
)]
pub async fn leave_unmanaged(
    State(state): State<LibrarySetup>,
    caller: RequireAdmin,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<LeaveUnmanagedBody>,
) -> Result<Json<EditionStatusView>, ApiError> {
    let ops = Operations::new(&state);
    let (album, user) = (album_id.clone(), caller.0.user_id);
    blocking(move || ops.leave_unmanaged(&album, body.expected_identity_revision, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// What taking an edition would do: which files keep their place, which
/// are second copies, other versions or extras, and which edition tracks
/// are missing. Asks MusicBrainz for the release; changes nothing. The
/// album's own edition is open to every signed-in user; planning another
/// release is for curators, who can choose it.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/edition/plan",
    params(("album_id" = String, Path, description = "Local album id"), EditionPlanQuery),
    responses(
        (status = 200, description = "The plan", body = EditionPlanView),
        (status = 400, description = "Bad release id, or the album has no edition to plan"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required to plan another release"),
        (status = 404, description = "Unknown album, or MusicBrainz does not know the release"),
        (status = 503, description = "MusicBrainz is not answering"),
    )
)]
pub async fn plan_edition(
    State(state): State<LibrarySetup>,
    caller: Principal,
    Path(album_id): Path<String>,
    ValidQuery(query): ValidQuery<EditionPlanQuery>,
) -> Result<Json<EditionPlanView>, ApiError> {
    if query.release_mbid.is_some() && !caller.role.is_curator() {
        return Err(ApiError::forbidden("Curator role required"));
    }
    let plan = Operations::new(&state)
        .read_edition_plan(&album_id, query.release_mbid.as_deref())
        .await?;
    Ok(Json(plan.into()))
}

/// Match a file by hand.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MatchFileBody {
    /// A track of the album's current edition that no file holds.
    pub release_track_mbid: String,
    /// The `identity_revision` the edition plan showed.
    pub expected_identity_revision: i64,
}

/// Match one file to a track of the album's edition by hand.
///
/// A file the edition did not place goes on a track no file holds, and
/// then counts as that track. On an edition the app picked, the edition
/// becomes the person's, as "Looks right" makes it. No file is read or
/// written.
#[utoipa::path(
    put,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/edition/files/{track_id}",
    params(
        ("album_id" = String, Path, description = "Local album id"),
        ("track_id" = String, Path, description = "Local track id of the file"),
    ),
    request_body = MatchFileBody,
    responses(
        (status = 200, description = "The album's edition", body = EditionStatusView),
        (status = 400, description = "The album has no edition, or the track is not on it"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album, or the file is not one of its files"),
        (status = 409, description = "The edition changed (STALE_REVISION), the file is already on a track, another file holds the track, or the album is a custom edition"),
        (status = 503, description = "MusicBrainz is not answering"),
    )
)]
pub async fn match_edition_file(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path((album_id, track_id)): Path<(String, String)>,
    ValidJson(body): ValidJson<MatchFileBody>,
) -> Result<Json<EditionStatusView>, ApiError> {
    Operations::new(&state)
        .match_file(
            &album_id,
            &track_id,
            &body.release_track_mbid,
            body.expected_identity_revision,
            &caller.0.user_id,
        )
        .await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Undo a file's match by hand.
///
/// The file's identity goes back to what it was before the match. Plays
/// and favorites that moved to it stay.
#[utoipa::path(
    delete,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/edition/files/{track_id}",
    params(
        ("album_id" = String, Path, description = "Local album id"),
        ("track_id" = String, Path, description = "Local track id of the file"),
    ),
    responses(
        (status = 200, description = "The album's edition", body = EditionStatusView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album, not one of its files, or no match to undo"),
        (status = 409, description = "The match changed since (STALE_REVISION), or the app has written the file since"),
    )
)]
pub async fn unmatch_edition_file(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path((album_id, track_id)): Path<(String, String)>,
) -> Result<Json<EditionStatusView>, ApiError> {
    let ops = Operations::new(&state);
    let (album, track, user) = (album_id.clone(), track_id, caller.0.user_id);
    blocking(move || ops.unmatch_file(&album, &track, &user)).await?;
    Ok(Json(status_of(&state, &album_id).await?))
}

/// Extra files to move to the recycle bin.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RemoveExtrasBody {
    /// Local track ids the album's edition plan marks anything but `keep`.
    pub track_ids: Vec<String>,
}

/// What removing extras did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RemoveExtrasView {
    /// Pass to the undo route to put the files back.
    pub action_id: String,
    pub removed_track_ids: Vec<String>,
}

/// Take back a removal of extras.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RestoreExtrasBody {
    pub action_id: String,
}

/// Whether the files went back.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RestoreExtrasView {
    /// False when a file could not be put back; it stays in the recycle bin.
    pub restored: bool,
}

/// Move files the album's edition has no place for (second copies, other
/// versions and files not on it) to the recycle bin: every fate but
/// `keep`. Only files the current plan marks that way are accepted;
/// nothing runs on its own. Undo with
/// `.../edition/extras/restore`.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/edition/extras/remove",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = RemoveExtrasBody,
    responses(
        (status = 200, description = "Files recycled", body = RemoveExtrasView),
        (status = 400, description = "A picked file keeps its place on the edition, or nothing was picked"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album or track"),
        (status = 409, description = "Library busy or a file is outside the library folders"),
        (status = 422, description = "A file could not be recycled; nothing changed"),
        (status = 503, description = "MusicBrainz is not answering"),
    )
)]
pub async fn remove_extras(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<RemoveExtrasBody>,
) -> Result<Json<RemoveExtrasView>, ApiError> {
    let refuse = |reason| ApiError::from(OperationError::Invalid(reason));
    if body.track_ids.is_empty() {
        return Err(refuse(reasons::NOTHING_PICKED));
    }
    let plan = Operations::new(&state)
        .edition_plan(&album_id, None)
        .await?;
    let spare = |id: &String| {
        plan.files
            .iter()
            .any(|file| file.local_track_id == *id && file.fate != FileFate::Keep)
    };
    if !body.track_ids.iter().all(spare) {
        return Err(refuse(reasons::FILE_KEEPS_ITS_PLACE));
    }
    let actor = caller.0.user_id;
    let (action_id, removed_track_ids) = tokio::task::spawn_blocking(move || {
        Mutations::new(&state).recycle_album_files(&album_id, &body.track_ids, &actor)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(RemoveExtrasView {
        action_id,
        removed_track_ids,
    }))
}

/// Put back the files one removal of extras moved to the recycle bin.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/edition/extras/restore",
    params(("album_id" = String, Path, description = "Local album id")),
    request_body = RestoreExtrasBody,
    responses(
        (status = 200, description = "Files put back and rescanned", body = RestoreExtrasView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Nothing to take back"),
        (status = 409, description = "Another file now sits at an old path; nothing was put back"),
    )
)]
pub async fn restore_extras(
    State(state): State<LibrarySetup>,
    caller: RequireCurator,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<RestoreExtrasBody>,
) -> Result<Json<RestoreExtrasView>, ApiError> {
    let actor = caller.0.user_id;
    let restored = tokio::task::spawn_blocking(move || {
        Mutations::new(&state).restore_recycled(&album_id, &body.action_id, &actor)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(RestoreExtrasView { restored }))
}

/// Albums whose match is a best guess waiting for a look (or, with
/// `state=unmatched`, albums nothing fits). Newest first.
#[utoipa::path(
    get,
    tag = "library management",
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
) -> Result<Json<WaitingAlbumsResponse>, ApiError> {
    let unmatched = match query.state.as_deref() {
        None | Some("unconfirmed") => false,
        Some("unmatched") => true,
        Some(_) => {
            return Err(ApiError::invalid_input(
                "state must be unconfirmed or unmatched",
            ));
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
