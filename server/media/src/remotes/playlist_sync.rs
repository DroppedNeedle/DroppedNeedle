//! Navidrome playlist sync: the admin route that forces one sync now.
//!
//! The loop and its ports live in `jobs::playlist_sync`. The route takes no
//! body: the target directory comes from saved settings, so no caller can
//! name an arbitrary directory to write into.
//!
//! The route mounts at `/navidrome/playlist-sync` for the settings router to
//! nest (v2 served it at `/settings/navidrome/playlist-sync`); the app nests
//! it inside the session gate with the other settings routes.

use axum::{Json, Router, extract::State, routing::post};

use droppedneedle_auth::Admin;
use droppedneedle_kernel::jobs::playlist_sync::{
    DISABLED_MESSAGE, PlaylistExporter, PlaylistSyncResult, PlaylistSyncSettings,
};

/// Route state: settings plus exporter.
#[derive(Clone)]
pub struct PlaylistSyncState<T, E> {
    /// Saved-settings source.
    pub settings: T,
    /// Playlist file writer.
    pub exporter: E,
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
    tag = "remotes",
    responses(
        (status = 200, description = "Sync report", body = PlaylistSyncResult),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
    )
)]
pub(crate) async fn sync_now<T, E>(
    State(state): State<PlaylistSyncState<T, E>>,
    _admin: Admin,
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
