//! Download-task HTTP: the admin reimport behind the request card.
//!
//! One route only: the request history card offers an admin reimport and
//! the server must answer it. The task, held and quarantine views are not
//! served yet. Auth reuses the requests principal and its translation
//! layer, so one layer serves both routers.

use std::sync::Arc;

use crate::acquire::dispatch::Journal;
use crate::acquire::requests::{auth::Principal, error::RequestsError};
use axum::{
    Json, Router,
    extract::{Path, State},
    response::IntoResponse,
    routing,
};
use serde::Serialize;
use utoipa::ToSchema;

/// Reimport outcome. The requeue puts the task back in line; the worker
/// reports fresh progress from there.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReimportResponse {
    /// Whether the task went back in line.
    pub success: bool,
    /// Task status after this call (`queued`).
    pub status: String,
    /// Failure text, when the requeue itself failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Requeue one failed or short-landed task for import without
/// re-searching. `POST /api/v3/downloads/tasks/{task_id}/reimport`.
/// Admin only. Missing tasks and tasks that fail the reimport guard
/// (no picked candidate linked) answer 404.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/tasks/{task_id}/reimport",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = ReimportResponse))
)]
pub async fn reimport_task_handler(
    State(journal): State<Arc<Journal>>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<impl IntoResponse, RequestsError> {
    principal.require_admin()?;
    let row = Journal::with_store_async(&journal, move |store| {
        store.reimport_task(&task_id, now_unix_f64())
    })
    .await
    .map_err(|cause| RequestsError::internal(&cause))?;
    let Some(row) = row else {
        return Err(RequestsError::NotFound);
    };
    Ok(Json(ReimportResponse {
        success: true,
        status: row.status.as_str().to_owned(),
        error_message: None,
    }))
}

/// Task routes without an auth layer. The app mounts this inside the
/// session gate under the shared principal-translation layer.
pub fn downloads_core_routes(journal: Arc<Journal>) -> Router {
    Router::new()
        .route(
            "/downloads/tasks/{task_id}/reimport",
            routing::post(reimport_task_handler),
        )
        .with_state(journal)
}

/// Task routes behind the header test gate.
#[cfg(any(test, feature = "test-support"))]
pub fn downloads_router(journal: Arc<Journal>) -> Router {
    downloads_core_routes(journal).layer(axum::middleware::from_fn(
        crate::acquire::requests::auth::gate,
    ))
}

/// Current unix time as the float seconds the journal stores.
fn now_unix_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
