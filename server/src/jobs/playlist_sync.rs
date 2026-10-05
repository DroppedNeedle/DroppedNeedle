//! Navidrome playlist sync: route plus background loop.
//!
//! DroppedNeedle owns the playlists; Navidrome reads them as files. The loop
//! polls every [`SYNC_INTERVAL`] (polling, not hooking every mutation, so no
//! edit path can be missed) and the route lets an admin force one sync now.
//! Both read the target directory from saved settings and take no body, so no
//! caller can name an arbitrary directory to write into. Unchanged files are
//! never rewritten, and files Navidrome owns that are no longer ours are
//! removed only when the settings say so.
//!
//! The route mounts at `/navidrome/playlist-sync` for the settings router to
//! nest (v2 served it at `/settings/navidrome/playlist-sync`); the app nests
//! it inside the session gate with the other settings routes.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::{Schedule, SplitMix64, sleep_or_stop};
use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::roles::Role;

/// Registered name of the sync loop.
pub const JOB_NAME: &str = "navidrome-playlist-export";

/// v2 poll cadence, unchanged.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(300);

/// Default spread on the poll cadence.
pub const DEFAULT_JITTER: Duration = Duration::from_secs(30);

/// Message when sync is off, kept from v2.
pub const DISABLED_MESSAGE: &str = "Playlist sync is turned off. Enable it and save first.";

/// Where and what to sync, from saved settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSyncConfig {
    /// Directory Navidrome imports playlists from.
    pub target_dir: String,
    /// `public` or `all`, validated at the settings boundary.
    pub scope: String,
    /// Remove files that are no longer ours.
    pub remove_deleted: bool,
}

/// The saved Navidrome connection behind a seam, re-read every cycle and
/// every route call. `None` (or disabled/empty) means skip quietly.
pub trait PlaylistSyncSettings: Clone + Send + Sync + 'static {
    /// Current sync config, or nothing when syncing is off.
    fn sync_config(&self) -> BoxFuture<'_, Option<PlaylistSyncConfig>>;
}

/// The exporter that writes playlist files behind a seam.
pub trait PlaylistExporter: Clone + Send + Sync + 'static {
    /// Sync playlists into the configured directory and report what moved.
    fn sync(&self, config: PlaylistSyncConfig) -> BoxFuture<'_, PlaylistSyncResult>;
}

/// Sync report, field for field with the v2 result shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PlaylistSyncResult {
    /// False when nothing synced (disabled, or the exporter refused).
    pub success: bool,
    /// Human summary of the run.
    pub message: String,
    /// Files written or updated.
    pub written: u64,
    /// Files already current and left alone.
    pub unchanged: u64,
    /// Superseded files removed.
    pub removed: u64,
    /// Superseded files that could not be removed (retried next cycle).
    pub removal_failures: u64,
    /// Playlists skipped for having no tracks.
    pub skipped_empty: u64,
    /// Files in the directory that are not ours and were left alone.
    pub skipped_not_ours: u64,
    /// Tracks skipped for missing audio files.
    pub tracks_missing_files: u64,
    /// Tracks skipped for paths the export format cannot represent.
    pub tracks_unrepresentable: u64,
}

/// The cadence: v2's 5 minutes with a 30 s spread.
pub fn default_schedule() -> Schedule {
    Schedule::new(SYNC_INTERVAL)
        .with_jitter(DEFAULT_JITTER)
        .with_backoff(Duration::from_secs(10), Duration::from_secs(600))
}

/// Spawn the loop on a registry.
pub async fn spawn_on<S, T, E>(
    registry: &JobRegistry<S>,
    settings: T,
    exporter: E,
    schedule: Schedule,
) -> Result<(), super::registry::AlreadyRunning>
where
    S: RegistryStore,
    T: PlaylistSyncSettings,
    E: PlaylistExporter,
{
    let settings = Arc::new(settings);
    let exporter = Arc::new(exporter);
    registry
        .spawn(JOB_NAME, JobKind::Ephemeral, None, move |ctx| {
            let settings = Arc::clone(&settings);
            let exporter = Arc::clone(&exporter);
            async move { run(ctx, settings.as_ref(), exporter.as_ref(), &schedule).await }
        })
        .await
}

/// One loop cycle. Returns the cycle outcome for backoff: skipped cycles
/// (sync off) and clean syncs reset failures; only exporter-reported failure
/// counts. Removal failures log loud but do not count: the files stay and the
/// next cycle retries them.
pub async fn run_once<T, E>(settings: &T, exporter: &E) -> Result<(), ()>
where
    T: PlaylistSyncSettings,
    E: PlaylistExporter,
{
    let Some(config) = settings.sync_config().await else {
        return Ok(());
    };
    let result = exporter.sync(config).await;
    if result.removal_failures > 0 {
        tracing::error!(
            failures = result.removal_failures,
            message = %result.message,
            "Navidrome playlist export could not remove superseded files; they stay visible and retry next cycle"
        );
    } else if !result.success {
        tracing::warn!(message = %result.message, "Navidrome playlist export reported no success");
    }
    if result.success { Ok(()) } else { Err(()) }
}

/// Run until stop fires. The first cycle waits out one interval (v2 sleeps
/// before its first sync too); failures back off and never exit.
pub async fn run<S, T, E>(
    ctx: JobCtx<S>,
    settings: &T,
    exporter: &E,
    schedule: &Schedule,
) -> JobExit
where
    S: RegistryStore,
    T: PlaylistSyncSettings,
    E: PlaylistExporter,
{
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    let mut failures = 0_u32;
    loop {
        if sleep_or_stop(schedule.next_delay(&mut rng, failures), ctx.stop()).await {
            return JobExit::Stopped;
        }
        match run_once(settings, exporter).await {
            Ok(()) => failures = 0,
            Err(()) => failures = failures.saturating_add(1),
        }
        ctx.heartbeat().await;
    }
}

/// Role lookups for the route gate. The users store owns the rows; this
/// seam keeps jobs independent of that store. Async-native on
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
        let body = crate::error::ErrorEnvelope {
            error: crate::error::ErrorBody {
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
