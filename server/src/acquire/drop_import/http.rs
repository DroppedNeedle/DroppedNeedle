//! Drop-import HTTP: upload, the job list and one job, and the review
//! actions (match, discard).
//!
//! Curators only (admin or trusted), as in v2: importing writes into the
//! shared library, the same trust as auto-acquiring. Plain users request
//! an album and a curator imports it. Each curator sees their own jobs;
//! admins may list everyone's with `all=true`. Live progress arrives as
//! the `drop_import_updated` event.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, State, multipart::MultipartRejection},
    http::StatusCode,
    routing,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
use utoipa::{IntoParams, ToSchema};

use super::store::{ItemRecord, JobRecord};
use super::{DropError, DropImports, MAX_FILES_PER_DROP, Viewer, extract};
use crate::acquire::requests::{
    auth::Principal,
    error::RequestsError,
    http::{HttpError, ValidJson, ValidQuery},
};

/// The largest upload accepted in one request (all files together).
const MAX_UPLOAD_BYTES: usize = 8 * (1 << 30);

/// One album-shaped folder of an upload. Staged paths never leave the
/// server.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DropImportItemResponse {
    pub id: i64,
    pub folder_name: String,
    /// `processing`, `imported`, `skipped`, `needs_review`, `failed` or
    /// `discarded`.
    pub status: String,
    pub updated_at: f64,
    pub release_group_mbid: Option<String>,
    pub album_title: Option<String>,
    pub artist_name: Option<String>,
    pub files_total: i64,
    pub files_imported: i64,
    /// What happened, as one plain sentence.
    pub detail: Option<String>,
    /// Stable reason code when the item stopped short.
    pub reason_code: Option<String>,
    /// What to do about it.
    pub reason_action: Option<String>,
}

/// One upload and its items.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DropImportJobResponse {
    pub id: String,
    /// `processing`, `completed` or `failed`.
    pub status: String,
    pub created_at: f64,
    pub upload_name: String,
    pub user_id: String,
    pub user_name: String,
    /// Why the job failed, or notes on what was skipped.
    pub error: Option<String>,
    pub items: Vec<DropImportItemResponse>,
}

/// Newest jobs first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DropImportJobsResponse {
    pub jobs: Vec<DropImportJobResponse>,
}

/// The album a person picked for a review item.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DropImportMatchRequest {
    pub release_group_mbid: String,
}

/// List filter.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct DropImportJobsQuery {
    /// Admins: every user's jobs.
    #[serde(default)]
    pub all: bool,
}

/// The upload form: one or more `files` parts (audio or `.zip`).
#[derive(Debug, ToSchema)]
pub struct DropUploadForm {
    #[schema(value_type = Vec<String>, format = Binary)]
    pub files: Vec<Vec<u8>>,
}

impl From<ItemRecord> for DropImportItemResponse {
    fn from(item: ItemRecord) -> Self {
        Self {
            id: item.id,
            folder_name: item.folder_name,
            status: item.status,
            updated_at: item.updated_at,
            release_group_mbid: item.release_group_mbid,
            album_title: item.album_title,
            artist_name: item.artist_name,
            files_total: item.files_total,
            files_imported: item.files_imported,
            detail: item.detail,
            reason_code: item.reason_code,
            reason_action: item.reason_action,
        }
    }
}

impl From<JobRecord> for DropImportJobResponse {
    fn from(job: JobRecord) -> Self {
        Self {
            id: job.id,
            status: job.status,
            created_at: job.created_at,
            upload_name: job.upload_name,
            user_id: job.user_id,
            user_name: job.user_name,
            error: job.error,
            items: job.items.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<DropError> for HttpError {
    fn from(error: DropError) -> Self {
        HttpError(match error {
            DropError::Invalid(message) => RequestsError::InvalidInput { message },
            DropError::NotFound => RequestsError::NotFound,
            DropError::Conflict(message) => RequestsError::Conflict { message },
            DropError::Unavailable(cause) => RequestsError::internal(&cause),
        })
    }
}

fn invalid(message: impl Into<String>) -> HttpError {
    HttpError(RequestsError::InvalidInput {
        message: message.into(),
    })
}

/// Item ids are numbers; anything else names no item. Parsed after the
/// role check, so a plain user always sees 403.
fn item_number(raw: &str) -> Result<i64, HttpError> {
    raw.parse().map_err(|_| HttpError(RequestsError::NotFound))
}

fn curator(principal: &Principal) -> Result<Viewer, HttpError> {
    principal.require_curator().map_err(HttpError)?;
    Ok(Viewer {
        user_id: principal.user_id.clone(),
        admin: principal.role.is_admin(),
    })
}

/// Upload files to import: audio files or `.zip` archives, at most 25
/// per drop. Returns the new job; it keeps working in the background.
/// `POST /api/v3/import/uploads`.
#[utoipa::path(
    post,
    path = "/api/v3/import/uploads",
    request_body(content = DropUploadForm, content_type = "multipart/form-data"),
    responses((status = 202, body = DropImportJobResponse))
)]
pub async fn upload_handler(
    State(service): State<Arc<DropImports>>,
    principal: Principal,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<(StatusCode, Json<DropImportJobResponse>), HttpError> {
    let viewer = curator(&principal)?;
    let multipart = multipart.map_err(|_| invalid("Send the files as a multipart form."))?;
    let incoming = service.incoming_dir();
    let mut staged = Incoming::default();
    receive(multipart, &incoming, &mut staged.files).await?;
    let job = service
        .create_job(
            &viewer.user_id,
            &principal.display_name(),
            staged.files.clone(),
        )
        .await?;
    // The job owns the files now.
    staged.files.clear();
    Ok((StatusCode::ACCEPTED, Json(job.into())))
}

/// Uploaded files not yet handed to a job. Whatever is left when this
/// drops (a refused upload, a dropped connection, a failed job start) is
/// deleted, so half-written files never pile up in the incoming folder.
#[derive(Default)]
struct Incoming {
    files: Vec<(String, PathBuf)>,
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.files.drain(..).map(|(_, path)| path).collect();
        let remove = move || {
            for path in paths {
                if let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(path = %path.display(), %error, "unused upload not removed");
                }
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn_blocking(remove);
            }
            Err(_) => remove(),
        }
    }
}

/// Stream every `files` part to its own file under `incoming`, checking
/// names and the count before anything lands on disk for that part.
async fn receive(
    mut multipart: Multipart,
    incoming: &std::path::Path,
    staged: &mut Vec<(String, PathBuf)>,
) -> Result<(), HttpError> {
    tokio::fs::create_dir_all(incoming)
        .await
        .map_err(|error| HttpError(RequestsError::internal(&error)))?;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid("The upload was cut off or malformed."))?
    {
        if field.name() != Some("files") {
            continue;
        }
        let name = field.file_name().unwrap_or_default().to_owned();
        if !extract::accepted_upload(&name) {
            let shown = if name.is_empty() {
                "unnamed file"
            } else {
                name.as_str()
            };
            return Err(invalid(format!("Unsupported file type: {shown}")));
        }
        if staged.len() >= MAX_FILES_PER_DROP {
            return Err(invalid(format!(
                "Too many files in one drop (max {MAX_FILES_PER_DROP})."
            )));
        }
        let path = incoming.join(uuid::Uuid::new_v4().simple().to_string());
        staged.push((name, path.clone()));
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|error| HttpError(RequestsError::internal(&error)))?;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| invalid("The upload was cut off or malformed."))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|error| HttpError(RequestsError::internal(&error)))?;
        }
        file.sync_all()
            .await
            .map_err(|error| HttpError(RequestsError::internal(&error)))?;
    }
    if staged.is_empty() {
        return Err(invalid("No files were uploaded."));
    }
    Ok(())
}

/// Your recent import jobs; admins may pass `all=true` for everyone's.
/// `GET /api/v3/import/jobs`.
#[utoipa::path(
    get,
    path = "/api/v3/import/jobs",
    params(DropImportJobsQuery),
    responses((status = 200, body = DropImportJobsResponse))
)]
pub async fn list_jobs_handler(
    State(service): State<Arc<DropImports>>,
    principal: Principal,
    ValidQuery(query): ValidQuery<DropImportJobsQuery>,
) -> Result<Json<DropImportJobsResponse>, HttpError> {
    let viewer = curator(&principal)?;
    let jobs = service.list_jobs(&viewer, query.all).await?;
    Ok(Json(DropImportJobsResponse {
        jobs: jobs.into_iter().map(Into::into).collect(),
    }))
}

/// One import job. `GET /api/v3/import/jobs/{job_id}`.
#[utoipa::path(
    get,
    path = "/api/v3/import/jobs/{job_id}",
    params(("job_id" = String, Path, description = "Import job id")),
    responses((status = 200, body = DropImportJobResponse))
)]
pub async fn get_job_handler(
    State(service): State<Arc<DropImports>>,
    principal: Principal,
    Path(job_id): Path<String>,
) -> Result<Json<DropImportJobResponse>, HttpError> {
    let viewer = curator(&principal)?;
    Ok(Json(service.get_job(&viewer, &job_id).await?.into()))
}

/// Import a review item as the album you picked.
/// `POST /api/v3/import/items/{item_id}/match`.
#[utoipa::path(
    post,
    path = "/api/v3/import/items/{item_id}/match",
    params(("item_id" = i64, Path, description = "Import item id")),
    request_body = DropImportMatchRequest,
    responses((status = 200, body = DropImportItemResponse))
)]
pub async fn match_item_handler(
    State(service): State<Arc<DropImports>>,
    principal: Principal,
    Path(item_id): Path<String>,
    ValidJson(body): ValidJson<DropImportMatchRequest>,
) -> Result<Json<DropImportItemResponse>, HttpError> {
    let viewer = curator(&principal)?;
    let item_id = item_number(&item_id)?;
    let item = service
        .match_item(&viewer, item_id, &body.release_group_mbid)
        .await?;
    Ok(Json(item.into()))
}

/// Throw a review item's files away.
/// `POST /api/v3/import/items/{item_id}/discard`.
#[utoipa::path(
    post,
    path = "/api/v3/import/items/{item_id}/discard",
    params(("item_id" = i64, Path, description = "Import item id")),
    responses((status = 200, body = DropImportItemResponse))
)]
pub async fn discard_item_handler(
    State(service): State<Arc<DropImports>>,
    principal: Principal,
    Path(item_id): Path<String>,
) -> Result<Json<DropImportItemResponse>, HttpError> {
    let viewer = curator(&principal)?;
    let item_id = item_number(&item_id)?;
    Ok(Json(service.discard_item(&viewer, item_id).await?.into()))
}

/// Drop-import routes without an auth layer, mounted inside the session
/// gate beside the download routes.
pub fn drop_import_routes(service: Arc<DropImports>) -> Router {
    Router::new()
        .route(
            "/import/uploads",
            routing::post(upload_handler).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/import/jobs", routing::get(list_jobs_handler))
        .route("/import/jobs/{job_id}", routing::get(get_job_handler))
        .route(
            "/import/items/{item_id}/match",
            routing::post(match_item_handler),
        )
        .route(
            "/import/items/{item_id}/discard",
            routing::post(discard_item_handler),
        )
        .with_state(service)
}
