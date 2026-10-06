//! Download-task HTTP: the admin reimport behind the request card.
//!
//! One route only: the request history card offers an admin reimport and
//! the server must answer it. The task, held and quarantine views are not
//! served yet. Auth reuses the requests principal and its translation
//! layer, so one layer serves both routers.

use std::sync::Arc;

use crate::acquire::requests::{auth::Principal, error::RequestsError, http::HttpError};
use crate::acquire::worker::{DownloadWorker, ReimportError};
use axum::{
    Json, Router,
    extract::{Path, State},
    response::IntoResponse,
    routing,
};
use serde::Serialize;
use utoipa::ToSchema;

/// Reimport outcome: the task as the import left it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReimportResponse {
    /// Whether the files reached the library (`completed` or `partial`).
    pub success: bool,
    /// Task status after the import (`completed`, `partial`, `failed`).
    pub status: String,
    /// Why the import did not complete, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Import a failed or short-landed task's files again, right away, from
/// the files its last download left on disk (no new search or download).
/// `POST /api/v3/downloads/tasks/{task_id}/reimport`. Admin only. Missing
/// tasks and tasks with no files to reimport answer 404; a download client
/// that is no longer configured answers 409.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/tasks/{task_id}/reimport",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = ReimportResponse))
)]
pub async fn reimport_task_handler(
    State(worker): State<Arc<DownloadWorker>>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    principal.require_admin()?;
    let row = worker
        .reimport(&task_id)
        .await
        .map_err(|error| match error {
            ReimportError::Unavailable(message) => RequestsError::Conflict { message },
            ReimportError::Journal(cause) => RequestsError::internal(&cause),
        })?;
    let Some(row) = row else {
        return Err(RequestsError::NotFound.into());
    };
    let status = row.status.as_str().to_owned();
    Ok(Json(ReimportResponse {
        success: matches!(status.as_str(), "completed" | "partial"),
        status,
        error_message: row.error_message,
    }))
}

/// Task routes without an auth layer. The app mounts this inside the
/// session gate under the shared principal-translation layer.
pub fn downloads_core_routes(worker: Arc<DownloadWorker>) -> Router {
    Router::new()
        .route(
            "/downloads/tasks/{task_id}/reimport",
            routing::post(reimport_task_handler),
        )
        .with_state(worker)
}

/// Task routes behind the header test gate.
#[cfg(any(test, feature = "test-support"))]
pub fn downloads_router(worker: Arc<DownloadWorker>) -> Router {
    downloads_core_routes(worker).layer(axum::middleware::from_fn(
        crate::acquire::requests::auth::gate,
    ))
}
