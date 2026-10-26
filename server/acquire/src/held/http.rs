//! Held-list HTTP: the entries and their files, and the actions on them.
//!
//! Every route sits inside the session gate. An entry belongs to the user
//! its download or upload was for: they and admins act on it, and someone
//! else's answers 404. Matching an entry to an album is for curators
//! (admins and trusted users), as importing an upload is.

use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::service::{Acted, HeldError, HeldService, is_retry, retries_by_itself};
use super::store::{Entry, EntryOrigin, EntryState, HeldFile};
use crate::downloads::queue_rows::Viewer;
use crate::reason::{Reason, ReasonKind};
use crate::requests::error::RequestsError;
use droppedneedle_auth::{Curator, Principal, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_media::stream::routes::{
    DirectSpan, content_type_for_extension, file_range, range_header,
};

/// What a person can do with one held file, besides listening and
/// discarding, which every file offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeldFileAction {
    /// Import it, skipping the check that held it.
    Import,
    /// Our side could not take it: try again as it is. The app also tries
    /// again by itself.
    Retry,
    /// Make this lower-quality version your copy; yours goes to the
    /// recycle bin.
    UseThisVersion,
    /// Replace your copy, which is as good or better, with this file;
    /// yours goes to the recycle bin.
    UseThisFile,
    /// Ask AcoustID again.
    Recheck,
    /// Take the album's track as this song (a track request).
    GetFromAlbum,
}

/// The actions one held file offers. A file paired with no track offers
/// no import: picking the album (Match) is the way forward.
fn actions_for(file: &HeldFile) -> Vec<HeldFileAction> {
    let mut actions = Vec::new();
    match file.reason {
        Reason::AlbumVersionOnly => actions.push(HeldFileAction::GetFromAlbum),
        Reason::VersionLowerQuality => actions.push(HeldFileAction::UseThisVersion),
        Reason::NoBetterThanYours => actions.push(HeldFileAction::UseThisFile),
        _ if !file.is_paired() => {}
        reason if is_retry(reason) => actions.push(HeldFileAction::Retry),
        _ => actions.push(HeldFileAction::Import),
    }
    if matches!(
        file.reason,
        Reason::FingerprintMismatch | Reason::FingerprintUnconfirmed
    ) {
        actions.push(HeldFileAction::Recheck);
    }
    actions
}

/// One held file.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldFileResponse {
    pub id: i64,
    /// Its name (and folder) as it came.
    pub original_name: String,
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub track_title: Option<String>,
    pub file_format: Option<String>,
    pub duration_seconds: Option<f64>,
    pub expected_duration_seconds: Option<f64>,
    /// What the check that held it saw: the title and artist AcoustID heard
    /// or the file's tags name, and AcoustID's score.
    pub evidence_title: Option<String>,
    pub evidence_artist: Option<String>,
    pub evidence_score: Option<f64>,
    /// `soulseek`, `usenet`, `plugin:<key>` or `upload`.
    pub source: String,
    pub source_task_id: Option<String>,
    pub reason: Reason,
    /// The reason as one plain sentence.
    pub reason_text: String,
    /// What the person can do about it.
    pub reason_action: String,
    /// `library` when our side held it and it is retried as it is; `hold`
    /// when a person decides.
    pub reason_kind: ReasonKind,
    /// Specifics: which file, which marker.
    pub reason_detail: Option<String>,
    pub actions: Vec<HeldFileAction>,
    /// When it was held, in milliseconds.
    pub created_at: i64,
}

impl From<HeldFile> for HeldFileResponse {
    fn from(file: HeldFile) -> Self {
        Self {
            actions: actions_for(&file),
            id: file.id,
            original_name: file.original_name,
            release_mbid: file.pairing.release_mbid,
            release_track_mbid: file.pairing.release_track_mbid,
            recording_mbid: file.pairing.recording_mbid,
            disc_number: file.pairing.disc_number,
            track_number: file.pairing.track_number,
            track_title: file.pairing.track_title,
            file_format: file.file_format,
            duration_seconds: file.duration_seconds,
            expected_duration_seconds: file.pairing.expected_duration_seconds,
            evidence_title: file.evidence_title,
            evidence_artist: file.evidence_artist,
            evidence_score: file.evidence_score,
            source: file.source,
            source_task_id: file.source_task_id,
            reason: file.reason,
            reason_text: file.reason.message().to_owned(),
            reason_action: file.reason.action().to_owned(),
            reason_kind: file.reason.kind(),
            reason_detail: file.reason_detail,
            created_at: file.created_at,
        }
    }
}

/// One entry of the held list: one download or upload, with its files.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldEntryResponse {
    pub id: i64,
    pub origin: EntryOrigin,
    /// `held` while it waits, `placing` while an action runs on it.
    pub state: EntryState,
    pub release_group_mbid: Option<String>,
    /// The song, for a track download.
    pub recording_mbid: Option<String>,
    /// The upload's drop import item.
    pub drop_item_id: Option<i64>,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    /// When it was made, in milliseconds.
    pub created_at: i64,
    /// When its held files are removed, in milliseconds.
    pub expires_at: i64,
    /// When DroppedNeedle next retries the files offering Retry by itself,
    /// in milliseconds. `None` when no file offers Retry, or when the next
    /// try is at the held loop's next pass (within five minutes).
    pub next_retry_at: Option<i64>,
    pub files: Vec<HeldFileResponse>,
}

impl From<Entry> for HeldEntryResponse {
    fn from(entry: Entry) -> Self {
        let retries = entry.files.iter().any(retries_by_itself);
        Self {
            next_retry_at: entry.retry_at.filter(|_| retries),
            id: entry.id,
            origin: entry.origin,
            state: entry.state,
            release_group_mbid: entry.release_group_mbid,
            recording_mbid: entry.recording_mbid,
            drop_item_id: entry.drop_item_id,
            artist_name: entry.artist_name,
            album_title: entry.album_title,
            created_at: entry.created_at,
            expires_at: entry.expires_at,
            files: entry.files.into_iter().map(Into::into).collect(),
        }
    }
}

/// Entries waiting for a decision, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldListResponse {
    pub entries: Vec<HeldEntryResponse>,
}

/// Held list filter.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HeldListQuery {
    /// Only this album's entries.
    pub release_group_mbid: Option<String>,
    /// Only entries holding files of this download.
    pub task_id: Option<String>,
    /// Admins: every user's entries.
    #[serde(default)]
    pub all: bool,
}

/// The files an action takes; every held file of the entry when left out.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct HeldFilesRequest {
    #[serde(default)]
    pub file_ids: Option<Vec<i64>>,
}

/// The album a curator picked, and the files it takes (all when left out).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct HeldMatchRequest {
    pub release_group_mbid: String,
    #[serde(default)]
    pub file_ids: Option<Vec<i64>>,
}

/// Bulk re-check: these entries, in order, or every entry.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct HeldRecheckAllRequest {
    #[serde(default)]
    pub entry_ids: Option<Vec<i64>>,
}

/// What an action did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldActionResponse {
    /// Files now in the library.
    pub imported: usize,
    /// Files the library already held, so nothing was added.
    pub already_owned: usize,
    /// Files deleted.
    pub discarded: usize,
    /// Files still held.
    pub still_held: usize,
    /// Why the first file still held stays, when one does.
    pub message: Option<String>,
}

impl From<Acted> for HeldActionResponse {
    fn from(acted: Acted) -> Self {
        Self {
            imported: acted.imported,
            already_owned: acted.owned,
            discarded: acted.discarded,
            still_held: acted.held.len(),
            message: acted
                .held
                .first()
                .map(|reason| format!("{} {}", reason.message(), reason.action())),
        }
    }
}

/// How one file of a bulk re-check went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeldRecheckStatus {
    /// AcoustID now agrees; the file was imported.
    Imported,
    /// Still no confident agreement; the file stays held.
    StillHeld,
    /// The check or the import failed.
    Error,
}

/// One file of a bulk re-check.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldRecheckItem {
    pub entry_id: i64,
    pub file_id: i64,
    pub status: HeldRecheckStatus,
    /// Why it failed.
    pub message: Option<String>,
}

/// A bulk re-check's results.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldRecheckAllResponse {
    pub results: Vec<HeldRecheckItem>,
}

fn viewer(principal: &Principal) -> Viewer {
    Viewer {
        user_id: principal.user_id().to_owned(),
        admin: principal.is_admin(),
    }
}

impl From<HeldError> for ApiError {
    fn from(error: HeldError) -> Self {
        ApiError::from(match error {
            HeldError::NotFound(_) => RequestsError::NotFound,
            HeldError::Refused(message) => RequestsError::Conflict { message },
            error @ (HeldError::InUse | HeldError::NotSetUp) => RequestsError::Conflict {
                message: error.to_string(),
            },
            HeldError::Invalid(message) => RequestsError::InvalidInput { message },
            HeldError::Busy(operation) => RequestsError::Busy { operation },
            HeldError::Unavailable(cause) => RequestsError::internal(&cause),
        })
    }
}

/// Ids arrive as path text; a non-number is simply not found.
fn id_of(text: &str) -> Result<i64, ApiError> {
    text.parse()
        .map_err(|_| ApiError::from(RequestsError::NotFound))
}

/// Entries waiting for your decision, newest first, with when each
/// expires. `GET /api/v3/held`.
#[utoipa::path(
    get,
    tag = "held",
    path = "/api/v3/held",
    params(HeldListQuery),
    responses((status = 200, body = HeldListResponse))
)]
pub async fn list_handler(
    State(service): State<Arc<HeldService>>,
    User(principal): User,
    ValidQuery(query): ValidQuery<HeldListQuery>,
) -> Result<Json<HeldListResponse>, ApiError> {
    let entries = service
        .list(
            &viewer(&principal),
            query.release_group_mbid.filter(|mbid| !mbid.is_empty()),
            query.task_id.filter(|id| !id.is_empty()),
            query.all,
        )
        .await?;
    Ok(Json(HeldListResponse {
        entries: entries.into_iter().map(Into::into).collect(),
    }))
}

/// One entry with its held files. `GET /api/v3/held/{entry_id}`.
#[utoipa::path(
    get,
    tag = "held",
    path = "/api/v3/held/{entry_id}",
    params(("entry_id" = i64, Path, description = "Held entry id")),
    responses((status = 200, body = HeldEntryResponse))
)]
pub async fn get_handler(
    State(service): State<Arc<HeldService>>,
    User(principal): User,
    Path(entry_id): Path<String>,
) -> Result<Json<HeldEntryResponse>, ApiError> {
    let entry = service.get(&viewer(&principal), id_of(&entry_id)?).await?;
    Ok(Json(entry.into()))
}

/// Import an entry's held files (all, or those named), skipping the check
/// that held each. A file no better than your copy stays held until "Use
/// this file"; a lower-quality version replaces yours only by "Use this
/// version", and yours goes to the recycle bin.
/// `POST /api/v3/held/{entry_id}/import`.
#[utoipa::path(
    post,
    tag = "held",
    path = "/api/v3/held/{entry_id}/import",
    params(("entry_id" = i64, Path, description = "Held entry id")),
    request_body = HeldFilesRequest,
    responses((status = 200, body = HeldActionResponse))
)]
pub async fn import_handler(
    State(service): State<Arc<HeldService>>,
    User(principal): User,
    Path(entry_id): Path<String>,
    ValidJson(body): ValidJson<HeldFilesRequest>,
) -> Result<Json<HeldActionResponse>, ApiError> {
    let acted = service
        .import(
            &viewer(&principal),
            id_of(&entry_id)?,
            body.file_ids,
            principal.user_id(),
        )
        .await?;
    Ok(Json(acted.into()))
}

/// Import an entry's held files as the album a curator picked: the app
/// pairs them with its tracks. `POST /api/v3/held/{entry_id}/match`.
#[utoipa::path(
    post,
    tag = "held",
    path = "/api/v3/held/{entry_id}/match",
    params(("entry_id" = i64, Path, description = "Held entry id")),
    request_body = HeldMatchRequest,
    responses((status = 200, body = HeldActionResponse))
)]
pub async fn match_handler(
    State(service): State<Arc<HeldService>>,
    curator: Curator,
    Path(entry_id): Path<String>,
    ValidJson(body): ValidJson<HeldMatchRequest>,
) -> Result<Json<HeldActionResponse>, ApiError> {
    let viewer = Viewer {
        user_id: curator.user_id().to_owned(),
        admin: curator.is_admin(),
    };
    let acted = service
        .match_album(
            &viewer,
            id_of(&entry_id)?,
            &body.release_group_mbid,
            body.file_ids,
            curator.user_id(),
        )
        .await?;
    Ok(Json(acted.into()))
}

/// Ask AcoustID again about an entry's files it heard as another recording
/// or could not confirm; files it now confirms are imported.
/// `POST /api/v3/held/{entry_id}/recheck`.
#[utoipa::path(
    post,
    tag = "held",
    path = "/api/v3/held/{entry_id}/recheck",
    params(("entry_id" = i64, Path, description = "Held entry id")),
    request_body = HeldFilesRequest,
    responses((status = 200, body = HeldActionResponse))
)]
pub async fn recheck_handler(
    State(service): State<Arc<HeldService>>,
    User(principal): User,
    Path(entry_id): Path<String>,
    ValidJson(body): ValidJson<HeldFilesRequest>,
) -> Result<Json<HeldActionResponse>, ApiError> {
    let acted = service
        .recheck(
            &viewer(&principal),
            id_of(&entry_id)?,
            body.file_ids,
            principal.user_id(),
        )
        .await?;
    Ok(Json(acted.into()))
}

/// Re-check AcoustID holds across entries (at most 25 checks a call).
/// `POST /api/v3/held/recheck`.
#[utoipa::path(
    post,
    tag = "held",
    path = "/api/v3/held/recheck",
    request_body = HeldRecheckAllRequest,
    responses((status = 200, body = HeldRecheckAllResponse))
)]
pub async fn recheck_all_handler(
    State(service): State<Arc<HeldService>>,
    User(principal): User,
    ValidJson(body): ValidJson<HeldRecheckAllRequest>,
) -> Result<Json<HeldRecheckAllResponse>, ApiError> {
    let results = service
        .recheck_bulk(&viewer(&principal), body.entry_ids, principal.user_id())
        .await?;
    Ok(Json(HeldRecheckAllResponse {
        results: results
            .into_iter()
            .map(|item| {
                let (status, message) = match item.outcome {
                    Ok(true) => (HeldRecheckStatus::Imported, None),
                    Ok(false) => (HeldRecheckStatus::StillHeld, None),
                    Err(message) => (HeldRecheckStatus::Error, Some(message)),
                };
                HeldRecheckItem {
                    entry_id: item.entry_id,
                    file_id: item.file_id,
                    status,
                    message,
                }
            })
            .collect(),
    }))
}

/// Delete an entry's held files (all, or those named). For a download, the
/// album's automatic retry resumes. `POST /api/v3/held/{entry_id}/discard`.
#[utoipa::path(
    post,
    tag = "held",
    path = "/api/v3/held/{entry_id}/discard",
    params(("entry_id" = i64, Path, description = "Held entry id")),
    request_body = HeldFilesRequest,
    responses((status = 200, body = HeldActionResponse))
)]
pub async fn discard_handler(
    State(service): State<Arc<HeldService>>,
    User(principal): User,
    Path(entry_id): Path<String>,
    ValidJson(body): ValidJson<HeldFilesRequest>,
) -> Result<Json<HeldActionResponse>, ApiError> {
    let acted = service
        .discard(&viewer(&principal), id_of(&entry_id)?, body.file_ids)
        .await?;
    Ok(Json(acted.into()))
}

/// Play a held file, with byte ranges so the player can seek.
/// `GET /api/v3/held/files/{file_id}/audio`.
#[utoipa::path(
    get,
    tag = "held",
    path = "/api/v3/held/files/{file_id}/audio",
    params(("file_id" = i64, Path, description = "Held file id")),
    responses(
        (status = 200, description = "The held file", content_type = "application/octet-stream"),
        (status = 206, description = "The requested byte range", content_type = "application/octet-stream"),
        (status = 416, description = "The range lies outside the file")
    )
)]
pub async fn audio_handler(
    State(service): State<Arc<HeldService>>,
    User(principal): User,
    Path(file_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let path = service
        .audio_path(&viewer(&principal), id_of(&file_id)?)
        .await?;
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return Err(ApiError::from(RequestsError::NotFound));
    };
    let total = meta.len();
    let Some(span) = DirectSpan::for_request(range_header(&headers), total) else {
        let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
        if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
        return Ok(response);
    };
    let content_type = path
        .extension()
        .and_then(|ext| content_type_for_extension(&ext.to_string_lossy()))
        .unwrap_or("application/octet-stream");
    let mut response = Response::new(Body::from_stream(file_range(path, span.start, span.len)));
    *response.status_mut() = span.status();
    span.write_headers(response.headers_mut());
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    Ok(response)
}

/// The held-list routes. Match is for curators; the rest for the entry's
/// owner and admins, checked by the service.
pub fn routes() -> Routes<Arc<HeldService>> {
    Routes::new()
        .user(routes!(list_handler))
        .user(routes!(recheck_all_handler))
        .user(routes!(get_handler))
        .user(routes!(import_handler))
        .curator(routes!(match_handler))
        .user(routes!(recheck_handler))
        .user(routes!(discard_handler))
        .user(routes!(audio_handler))
}
