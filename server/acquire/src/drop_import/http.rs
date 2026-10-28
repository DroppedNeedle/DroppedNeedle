//! Drop-import HTTP: upload, the job list and one job. Files an upload
//! left waiting are acted on through the held list ([`crate::held`]).
//!
//! Curators only (admin or trusted): importing writes into the
//! shared library, the same trust as auto-acquiring. Plain users request
//! an album and a curator imports it. Each curator sees their own jobs;
//! admins may list everyone's with `all=true`. Live progress arrives as
//! the `drop_import_updated` event.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    extract::{
        DefaultBodyLimit, Multipart, Path, State,
        multipart::{MultipartError, MultipartRejection},
    },
    http::{HeaderMap, StatusCode, header},
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
use utoipa::{IntoParams, ToSchema};

use super::store::{ItemRecord, JobRecord};
use super::{DropError, DropImports, MAX_FILES_PER_DROP, Viewer, extract};
use crate::reason::Reason;
use crate::requests::error::RequestsError;
use droppedneedle_auth::Curator;
use droppedneedle_kernel::error::{ApiError, PAYLOAD_TOO_LARGE};
use droppedneedle_kernel::extract::ValidQuery;
use droppedneedle_kernel::mount::{Routes, routes};

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
    /// Why the item stopped short, when it did.
    pub reason_code: Option<Reason>,
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

/// List filter.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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
            reason_action: item.reason_code.map(|reason| reason.action().to_owned()),
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

impl From<DropError> for ApiError {
    fn from(error: DropError) -> Self {
        ApiError::from(match error {
            DropError::Invalid(message) => RequestsError::InvalidInput { message },
            DropError::NotFound => RequestsError::NotFound,
            DropError::Conflict(message) => RequestsError::Conflict { message },
            DropError::Unavailable(cause) => RequestsError::internal(&cause),
        })
    }
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::from(RequestsError::InvalidInput {
        message: message.into(),
    })
}

fn too_large() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        PAYLOAD_TOO_LARGE,
        "Uploads are limited to 8 GB.",
    )
}

/// A body that went over the limit is 413; anything else that stops the
/// form part way is a cut-off or malformed upload.
fn unreadable(error: MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        too_large()
    } else {
        invalid("The upload was cut off or malformed.")
    }
}

fn viewer(curator: &Curator) -> Viewer {
    Viewer {
        user_id: curator.user_id().to_owned(),
        admin: curator.is_admin(),
    }
}

/// Upload files to import: audio files or `.zip` archives, at most 25
/// per drop and 8 GB in all. Returns the new job; it keeps working in the
/// background. `POST /api/v3/import/uploads`.
#[utoipa::path(
    post,
    tag = "imports",
    path = "/api/v3/import/uploads",
    request_body(content = DropUploadForm, content_type = "multipart/form-data"),
    responses((status = 202, body = DropImportJobResponse))
)]
pub async fn upload_handler(
    State(service): State<Arc<DropImports>>,
    curator: Curator,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<(StatusCode, Json<DropImportJobResponse>), ApiError> {
    let viewer = viewer(&curator);
    // Refused before anything is written when the sender says up front
    // that the body is over the limit.
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_UPLOAD_BYTES as u64) {
        return Err(too_large());
    }
    let multipart = multipart.map_err(|_| invalid("Send the files as a multipart form."))?;
    tracing::info!(bytes = declared, "drop upload started");
    let incoming = service.incoming_dir();
    let mut staged = Incoming::default();
    let bytes = receive(multipart, &incoming, &mut staged.files).await?;
    let files = staged.files.len();
    let job = service
        .create_job(
            &viewer.user_id,
            curator.username_display(),
            staged.files.clone(),
        )
        .await?;
    // The job owns the files now.
    staged.files.clear();
    tracing::info!(job_id = %job.id, files, bytes, "drop upload received");
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
/// Answers the bytes written.
async fn receive(
    mut multipart: Multipart,
    incoming: &std::path::Path,
    staged: &mut Vec<(String, PathBuf)>,
) -> Result<u64, ApiError> {
    let mut bytes = 0u64;
    tokio::fs::create_dir_all(incoming)
        .await
        .map_err(|error| ApiError::from(RequestsError::internal(&error)))?;
    while let Some(mut field) = multipart.next_field().await.map_err(unreadable)? {
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
            .map_err(|error| ApiError::from(RequestsError::internal(&error)))?;
        while let Some(chunk) = field.chunk().await.map_err(unreadable)? {
            bytes += chunk.len() as u64;
            file.write_all(&chunk)
                .await
                .map_err(|error| ApiError::from(RequestsError::internal(&error)))?;
        }
        file.sync_all()
            .await
            .map_err(|error| ApiError::from(RequestsError::internal(&error)))?;
    }
    if staged.is_empty() {
        return Err(invalid("No files were uploaded."));
    }
    Ok(bytes)
}

/// Your recent import jobs; admins may pass `all=true` for everyone's.
/// `GET /api/v3/import/jobs`.
#[utoipa::path(
    get,
    tag = "imports",
    path = "/api/v3/import/jobs",
    params(DropImportJobsQuery),
    responses((status = 200, body = DropImportJobsResponse))
)]
pub async fn list_jobs_handler(
    State(service): State<Arc<DropImports>>,
    curator: Curator,
    ValidQuery(query): ValidQuery<DropImportJobsQuery>,
) -> Result<Json<DropImportJobsResponse>, ApiError> {
    let viewer = viewer(&curator);
    let jobs = service.list_jobs(&viewer, query.all).await?;
    Ok(Json(DropImportJobsResponse {
        jobs: jobs.into_iter().map(Into::into).collect(),
    }))
}

/// One import job. `GET /api/v3/import/jobs/{job_id}`.
#[utoipa::path(
    get,
    tag = "imports",
    path = "/api/v3/import/jobs/{job_id}",
    params(("job_id" = String, Path, description = "Import job id")),
    responses((status = 200, body = DropImportJobResponse))
)]
pub async fn get_job_handler(
    State(service): State<Arc<DropImports>>,
    curator: Curator,
    Path(job_id): Path<String>,
) -> Result<Json<DropImportJobResponse>, ApiError> {
    let viewer = viewer(&curator);
    Ok(Json(service.get_job(&viewer, &job_id).await?.into()))
}

/// Drop-import routes, for an admin or trusted user. The upload takes a
/// body up to `MAX_UPLOAD_BYTES`.
pub fn routes() -> Routes<Arc<DropImports>> {
    let (schemas, paths, upload) = routes!(upload_handler);
    let upload = upload.layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES));
    Routes::new()
        .curator((schemas, paths, upload))
        .curator(routes!(list_jobs_handler))
        .curator(routes!(get_job_handler))
}
