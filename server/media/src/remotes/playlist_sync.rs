//! Navidrome playlist sync: the admin route that forces one sync now, and
//! the saved settings the route and the loop read.
//!
//! The loop and its ports live in `jobs::playlist_sync`. The route takes no
//! body: the target directory comes from saved settings, so no caller can
//! name an arbitrary directory to write into.

use axum::{Json, extract::State};

use droppedneedle_auth::Admin;
use droppedneedle_kernel::jobs::playlist_export::M3uPlaylistExporter;
use droppedneedle_kernel::jobs::playlist_sync::{
    DISABLED_MESSAGE, PlaylistExporter, PlaylistSyncResult, sync_config,
};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::NavidromeConnection;

/// Route state: the saved Navidrome connection plus the exporter.
#[derive(Clone)]
pub struct PlaylistSyncState<E> {
    /// The saved Navidrome connection, read on every call.
    pub connection: Live<NavidromeConnection>,
    /// Playlist file writer.
    pub exporter: E,
}

/// The production route state.
pub type ProdPlaylistSync = PlaylistSyncState<M3uPlaylistExporter>;

/// The route, admin only: the export writes files (a settings-wide
/// admin gate).
pub fn routes() -> Routes<ProdPlaylistSync> {
    Routes::new().admin(routes!(sync_now_route))
}

/// Run one Navidrome playlist sync now.
// The mounted form of `sync_now` over the production types.
#[utoipa::path(
    post,
    path = "/api/v3/settings/navidrome/playlist-sync",
    operation_id = "sync_now",
    tag = "remotes",
    responses(
        (status = 200, description = "Sync report", body = PlaylistSyncResult),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn sync_now_route(
    state: State<ProdPlaylistSync>,
    admin: Admin,
) -> Json<PlaylistSyncResult> {
    sync_now(state, admin).await
}

/// Run one sync with the saved settings; disabled settings answer the
/// disabled report without calling the exporter.
pub(crate) async fn sync_now<E>(
    State(state): State<PlaylistSyncState<E>>,
    _admin: Admin,
) -> Json<PlaylistSyncResult>
where
    E: PlaylistExporter,
{
    let Some(config) = sync_config(&state.connection) else {
        return Json(PlaylistSyncResult {
            success: false,
            message: DISABLED_MESSAGE.to_owned(),
            ..PlaylistSyncResult::default()
        });
    };
    Json(state.exporter.sync(config).await)
}
