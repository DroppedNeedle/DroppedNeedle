//! Navidrome playlist sync: the background loop.
//!
//! DroppedNeedle owns the playlists; Navidrome reads them as files. The loop
//! polls every [`SYNC_INTERVAL`] (polling, not hooking every mutation, so no
//! edit path can be missed); the admin route in
//! `remotes::playlist_sync` forces one sync now. Both read the target
//! directory from saved settings and take no body, so no caller can name an
//! arbitrary directory to write into. Unchanged files are never rewritten,
//! and files Navidrome owns that are no longer ours are removed only when
//! the settings say so.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::{Schedule, SplitMix64, sleep_or_stop};

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
