//! Free Music HTTP: the task list and one task, history removal, and the
//! curator actions (cancel, retry).
//!
//! Reading and clearing your own downloads is open to every signed-in
//! user; cancelling and retrying are curator actions, like the rest of
//! the download queue (v2 posture). Admins may pass `all=true` to see or
//! clear everyone's. Live progress arrives as `free_music_updated`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::store::TaskRecord;
use super::{FreeMusic, FreeMusicError};
use crate::acquire::drop_import::Viewer;
use crate::acquire::requests::{
    auth::Principal,
    error::RequestsError,
    http::{HttpError, ValidQuery},
};

/// One Free Music download.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FreeMusicTaskResponse {
    pub id: String,
    pub user_id: String,
    /// `album` or `track`.
    pub kind: String,
    /// Release group (album) or recording (track).
    pub mbid: String,
    pub artist: String,
    pub title: String,
    /// `searching`, `downloading`, `importing`, `completed`, `failed` or
    /// `cancelled`.
    pub status: String,
    pub created_at: f64,
    pub updated_at: f64,
    /// The Internet Archive item being downloaded.
    pub identifier: String,
    pub licence_url: String,
    pub format: String,
    pub files_total: i64,
    pub files_completed: i64,
    pub bytes_total: i64,
    pub bytes_downloaded: i64,
    /// Why it failed, as one plain sentence.
    pub error: Option<String>,
    /// Stable code for the failure.
    pub error_code: Option<String>,
    /// What to do about it.
    pub error_action: Option<String>,
    /// The quality policy pinned when the request was made.
    pub quality_snapshot_hash: Option<String>,
    /// That policy in one line ("MP3 320 to lossless, FLAC first").
    pub quality_snapshot_summary: Option<String>,
}

/// Newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FreeMusicTasksResponse {
    pub tasks: Vec<FreeMusicTaskResponse>,
}

/// How many history entries went.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FreeMusicHistoryClearResponse {
    pub cleared: i64,
}

/// Scope filter.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FreeMusicScopeQuery {
    /// Admins: every user's tasks.
    #[serde(default)]
    pub all: bool,
}

impl From<TaskRecord> for FreeMusicTaskResponse {
    fn from(task: TaskRecord) -> Self {
        Self {
            id: task.id,
            user_id: task.user_id,
            kind: task.kind,
            mbid: task.mbid,
            artist: task.artist,
            title: task.title,
            status: task.status,
            created_at: task.created_at,
            updated_at: task.updated_at,
            identifier: task.identifier,
            licence_url: task.licence_url,
            format: task.format,
            files_total: task.files_total,
            files_completed: task.files_completed,
            bytes_total: task.bytes_total,
            bytes_downloaded: task.bytes_downloaded,
            error: task.error,
            error_code: task.error_code,
            error_action: task.error_action,
            quality_snapshot_hash: task.quality_snapshot_hash,
            quality_snapshot_summary: task.quality_snapshot_summary,
        }
    }
}

impl From<FreeMusicError> for HttpError {
    fn from(error: FreeMusicError) -> Self {
        HttpError(match error {
            FreeMusicError::Invalid(message) => RequestsError::InvalidInput { message },
            FreeMusicError::NotFound => RequestsError::NotFound,
            FreeMusicError::Unavailable(cause) => RequestsError::internal(&cause),
        })
    }
}

fn viewer(principal: &Principal) -> Viewer {
    Viewer {
        user_id: principal.user_id.clone(),
        admin: principal.role.is_admin(),
    }
}

fn curator(principal: &Principal) -> Result<Viewer, HttpError> {
    principal.require_curator().map_err(HttpError)?;
    Ok(viewer(principal))
}

/// Your Free Music downloads; admins may pass `all=true`.
/// `GET /api/v3/free-music/tasks`.
#[utoipa::path(
    get,
    path = "/api/v3/free-music/tasks",
    params(FreeMusicScopeQuery),
    responses((status = 200, body = FreeMusicTasksResponse))
)]
pub async fn list_tasks_handler(
    State(service): State<Arc<FreeMusic>>,
    principal: Principal,
    ValidQuery(query): ValidQuery<FreeMusicScopeQuery>,
) -> Result<Json<FreeMusicTasksResponse>, HttpError> {
    let tasks = service.list(&viewer(&principal), query.all).await?;
    Ok(Json(FreeMusicTasksResponse {
        tasks: tasks.into_iter().map(Into::into).collect(),
    }))
}

/// One Free Music download. `GET /api/v3/free-music/tasks/{task_id}`.
#[utoipa::path(
    get,
    path = "/api/v3/free-music/tasks/{task_id}",
    params(("task_id" = String, Path, description = "Free Music task id")),
    responses((status = 200, body = FreeMusicTaskResponse))
)]
pub async fn get_task_handler(
    State(service): State<Arc<FreeMusic>>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<FreeMusicTaskResponse>, HttpError> {
    Ok(Json(
        service.get(&viewer(&principal), &task_id).await?.into(),
    ))
}

/// Remove finished downloads from your history; admins may pass
/// `all=true`. `DELETE /api/v3/free-music/tasks`.
#[utoipa::path(
    delete,
    path = "/api/v3/free-music/tasks",
    params(FreeMusicScopeQuery),
    responses((status = 200, body = FreeMusicHistoryClearResponse))
)]
pub async fn clear_history_handler(
    State(service): State<Arc<FreeMusic>>,
    principal: Principal,
    ValidQuery(query): ValidQuery<FreeMusicScopeQuery>,
) -> Result<Json<FreeMusicHistoryClearResponse>, HttpError> {
    let cleared = service.clear(&viewer(&principal), query.all).await?;
    Ok(Json(FreeMusicHistoryClearResponse {
        cleared: i64::try_from(cleared).unwrap_or(i64::MAX),
    }))
}

/// Remove one finished download from your history.
/// `DELETE /api/v3/free-music/tasks/{task_id}`.
#[utoipa::path(
    delete,
    path = "/api/v3/free-music/tasks/{task_id}",
    params(("task_id" = String, Path, description = "Free Music task id")),
    responses((status = 200, body = FreeMusicHistoryClearResponse))
)]
pub async fn remove_task_handler(
    State(service): State<Arc<FreeMusic>>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<FreeMusicHistoryClearResponse>, HttpError> {
    service.remove(&viewer(&principal), &task_id).await?;
    Ok(Json(FreeMusicHistoryClearResponse { cleared: 1 }))
}

/// Stop a download that has not started importing.
/// `POST /api/v3/free-music/tasks/{task_id}/cancel`.
#[utoipa::path(
    post,
    path = "/api/v3/free-music/tasks/{task_id}/cancel",
    params(("task_id" = String, Path, description = "Free Music task id")),
    responses((status = 200, body = FreeMusicTaskResponse))
)]
pub async fn cancel_task_handler(
    State(service): State<Arc<FreeMusic>>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<FreeMusicTaskResponse>, HttpError> {
    let viewer = curator(&principal)?;
    Ok(Json(service.cancel(&viewer, &task_id).await?.into()))
}

/// Start a failed or cancelled download again. Sources that already
/// failed are not tried twice. `POST /api/v3/free-music/tasks/{task_id}/retry`.
#[utoipa::path(
    post,
    path = "/api/v3/free-music/tasks/{task_id}/retry",
    params(("task_id" = String, Path, description = "Free Music task id")),
    responses((status = 200, body = FreeMusicTaskResponse))
)]
pub async fn retry_task_handler(
    State(service): State<Arc<FreeMusic>>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<FreeMusicTaskResponse>, HttpError> {
    let viewer = curator(&principal)?;
    Ok(Json(service.retry(&viewer, &task_id).await?.into()))
}

/// Free Music routes without an auth layer, mounted inside the session
/// gate beside the download routes.
pub fn free_music_routes(service: Arc<FreeMusic>) -> Router {
    Router::new()
        .route(
            "/free-music/tasks",
            routing::get(list_tasks_handler).delete(clear_history_handler),
        )
        .route(
            "/free-music/tasks/{task_id}",
            routing::get(get_task_handler).delete(remove_task_handler),
        )
        .route(
            "/free-music/tasks/{task_id}/cancel",
            routing::post(cancel_task_handler),
        )
        .route(
            "/free-music/tasks/{task_id}/retry",
            routing::post(retry_task_handler),
        )
        .with_state(service)
}
