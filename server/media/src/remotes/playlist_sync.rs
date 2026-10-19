//! Navidrome playlist sync: the admin route that forces one sync now, and
//! the saved settings the route and the loop read.
//!
//! The loop and its ports live in `jobs::playlist_sync`. The route takes no
//! body: the target directory comes from saved settings, so no caller can
//! name an arbitrary directory to write into.

use axum::{Json, extract::State};

use droppedneedle_auth::Admin;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::playlist_export::M3uPlaylistExporter;
use droppedneedle_kernel::jobs::playlist_sync::{
    DISABLED_MESSAGE, PlaylistExporter, PlaylistSyncConfig, PlaylistSyncResult,
    PlaylistSyncSettings,
};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::NavidromeConnection;

/// Route state: settings plus exporter.
#[derive(Clone)]
pub struct PlaylistSyncState<T, E> {
    /// Saved-settings source.
    pub settings: T,
    /// Playlist file writer.
    pub exporter: E,
}

/// The production route state.
pub type ProdPlaylistSync = PlaylistSyncState<NavidromeSyncSettings, M3uPlaylistExporter>;

/// Playlist sync config from saved settings, re-read every cycle and every
/// route call. `None` (unreadable section, disabled, or an empty target
/// path) means skip quietly.
#[derive(Clone, Debug)]
pub struct NavidromeSyncSettings {
    connection: Live<NavidromeConnection>,
}

impl NavidromeSyncSettings {
    /// Read the Navidrome connection section through `connection`.
    pub fn new(connection: Live<NavidromeConnection>) -> Self {
        Self { connection }
    }
}

impl PlaylistSyncSettings for NavidromeSyncSettings {
    fn sync_config(&self) -> BoxFuture<'_, Option<PlaylistSyncConfig>> {
        Box::pin(async move {
            let Ok(connection) = self.connection.get() else {
                return None;
            };
            if !connection.enabled
                || !connection.playlist_sync_enabled
                || connection.playlist_sync_path.trim().is_empty()
            {
                return None;
            }
            Some(PlaylistSyncConfig {
                target_dir: connection.playlist_sync_path.clone(),
                scope: connection.playlist_sync_scope.clone(),
                remove_deleted: connection.playlist_sync_remove_deleted,
            })
        })
    }
}

/// The route, admin only: the export writes files (v2's settings-wide
/// admin gate).
pub fn routes() -> Routes<ProdPlaylistSync> {
    Routes::new().admin(routes!(sync_now_route))
}

/// The route at a relative path over any settings and exporter, for tests
/// that drive it with fakes: `POST /navidrome/playlist-sync`.
#[cfg(any(test, feature = "test-support"))]
pub fn router<T, E>(state: PlaylistSyncState<T, E>) -> axum::Router
where
    T: PlaylistSyncSettings,
    E: PlaylistExporter,
{
    axum::Router::new()
        .route(
            "/navidrome/playlist-sync",
            axum::routing::post(sync_now::<T, E>),
        )
        .with_state(state)
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
