//! Navidrome playlist sync: the admin route that forces one sync now.
//!
//! The loop and its ports live in `jobs::playlist_sync`. The route takes no
//! body: the target directory comes from saved settings, so no caller can
//! name an arbitrary directory to write into.
//!
//! The route mounts at `/navidrome/playlist-sync` for the settings router to
//! nest (v2 served it at `/settings/navidrome/playlist-sync`); the app nests
//! it inside the session gate with the other settings routes.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::post,
};

use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::roles::Role;
use droppedneedle_kernel::jobs::playlist_sync::{
    DISABLED_MESSAGE, PlaylistExporter, PlaylistSyncResult, PlaylistSyncSettings,
};
use droppedneedle_kernel::jobs::registry::BoxFuture;

/// Role lookups for the route gate. The users store owns the rows; this
/// seam keeps the route independent of that store. Async-native on
/// purpose: the lookup awaits the store instead of bridging threads.
pub trait SyncRoles: Send + Sync {
    /// One user's role, or `None` when the account is gone (stale session)
    /// or the store failed. Both fail closed.
    fn role_of(&self, user_id: &str) -> BoxFuture<'_, Option<Role>>;
}

/// Route state: settings plus exporter plus the admin gate's role lookup.
#[derive(Clone)]
pub struct PlaylistSyncState<T, E> {
    /// Saved-settings source.
    pub settings: T,
    /// Playlist file writer.
    pub exporter: E,
    /// Role lookups for the admin gate.
    pub roles: Arc<dyn SyncRoles>,
}

/// An admin caller. Anything else is 401 (no session, gone account) or 403.
pub struct SyncAdmin;

impl<T, E> FromRequestParts<PlaylistSyncState<T, E>> for SyncAdmin
where
    T: Send + Sync,
    E: Send + Sync,
{
    type Rejection = SyncError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &PlaylistSyncState<T, E>,
    ) -> Result<Self, Self::Rejection> {
        let session = parts
            .extensions
            .get::<CurrentSession>()
            .ok_or(SyncError::Unauthorized)?;
        match state.roles.role_of(&session.user_id).await {
            Some(Role::Admin) => Ok(SyncAdmin),
            Some(_) => Err(SyncError::Forbidden),
            None => Err(SyncError::Unauthorized),
        }
    }
}

/// Route-level failures, rendered in the shared error envelope.
#[derive(Debug)]
pub enum SyncError {
    /// No session stashed, or the account behind it is gone.
    Unauthorized,
    /// Signed in, but not an admin.
    Forbidden,
}

impl IntoResponse for SyncError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Authentication required",
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, "FORBIDDEN", "Admin role required"),
        };
        let body = droppedneedle_kernel::error::ErrorEnvelope {
            error: droppedneedle_kernel::error::ErrorBody {
                code: code.to_owned(),
                message: message.to_owned(),
                details: None,
            },
        };
        if status == StatusCode::UNAUTHORIZED {
            (
                status,
                [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                Json(body),
            )
                .into_response()
        } else {
            (status, Json(body)).into_response()
        }
    }
}

/// Route for the settings router to nest: `POST /navidrome/playlist-sync`.
pub fn router<T, E>(state: PlaylistSyncState<T, E>) -> Router
where
    T: PlaylistSyncSettings,
    E: PlaylistExporter,
{
    Router::new()
        .route("/navidrome/playlist-sync", post(sync_now::<T, E>))
        .with_state(state)
}

// Admin-only: the export writes files, so v2's settings-wide admin gate
// applies here too. No request body: the target comes from saved settings.
/// Run one Navidrome playlist sync now.
#[utoipa::path(
    post,
    path = "/api/v3/settings/navidrome/playlist-sync",
    tag = "crate::jobs::playlist_sync",
    responses(
        (status = 200, description = "Sync report", body = PlaylistSyncResult),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
    )
)]
pub(crate) async fn sync_now<T, E>(
    State(state): State<PlaylistSyncState<T, E>>,
    _admin: SyncAdmin,
) -> Json<PlaylistSyncResult>
where
    T: PlaylistSyncSettings,
    E: PlaylistExporter,
{
    let Some(config) = state.settings.sync_config().await else {
        return Json(PlaylistSyncResult {
            success: false,
            message: DISABLED_MESSAGE.to_owned(),
            ..PlaylistSyncResult::default()
        });
    };
    Json(state.exporter.sync(config).await)
}
