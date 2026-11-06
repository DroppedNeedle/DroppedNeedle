//! The import folder's status, for the setting's card: admins only, like
//! the setting itself.

use std::sync::Arc;

use axum::{Json, extract::State};
use droppedneedle_auth::Admin;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Serialize;
use utoipa::ToSchema;

use super::site::ImportFolderOverlap;
use super::{ImportFolderState, ImportFolders};

/// What the import folder is doing, or why it waits.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportFolderStatusResponse {
    pub state: ImportFolderState,
    /// With `overlaps`: the kind of folder it overlaps.
    pub overlap: Option<ImportFolderOverlap>,
    /// With `overlaps`: that folder's path.
    pub overlap_path: Option<String>,
    /// Albums found and still to go in.
    pub waiting: i64,
}

/// What the import folder is doing now, or why it waits.
/// `GET /api/v3/settings/library/import-folder/status`.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library/import-folder/status",
    responses((status = 200, body = ImportFolderStatusResponse))
)]
pub async fn import_folder_status_handler(
    State(folders): State<Arc<ImportFolders>>,
    _admin: Admin,
) -> Result<Json<ImportFolderStatusResponse>, ApiError> {
    let status = folders.status().await;
    let (overlap, overlap_path) = match status.overlap {
        Some((kind, path)) => (Some(kind), Some(path.to_string_lossy().into_owned())),
        None => (None, None),
    };
    Ok(Json(ImportFolderStatusResponse {
        state: status.state,
        overlap,
        overlap_path,
        waiting: status.waiting,
    }))
}

/// The import folder's routes, for admins.
pub fn routes() -> Routes<Arc<ImportFolders>> {
    Routes::new().admin(routes!(import_folder_status_handler))
}
