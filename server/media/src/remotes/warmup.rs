//! Jellyfin/Navidrome/Plex MBID warmup loops.
//!
//! Remote catalogs resolve faster when their MusicBrainz id index is warm,
//! so each integration rebuilds its index off-request: one loop per source
//! on the loop supervisor, whose name keeps any scope from rebuilding twice
//! at once. A failed pass is logged and the loop carries on.
//!
//! Cadences match v2's task schedule: Jellyfin builds once after a short
//! startup delay; Navidrome and Plex rebuild every four hours after
//! theirs. Nothing starts these loops yet.

use std::{future::Future, time::Duration};

use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickError, TickResult,
};

/// Jellyfin startup delay in seconds (v2 `warm_jellyfin_mbid_index`).
pub const JELLYFIN_STARTUP_DELAY: Duration = Duration::from_secs(8);
/// Navidrome startup delay in seconds (v2 `warm_navidrome_mbid_cache`).
pub const NAVIDROME_STARTUP_DELAY: Duration = Duration::from_secs(12);
/// Plex startup delay in seconds (v2 `warm_plex_mbid_cache`).
pub const PLEX_STARTUP_DELAY: Duration = Duration::from_secs(15);
/// Navidrome and Plex rebuild cadence (v2 sleeps `14400`, four hours).
pub const WARMUP_INTERVAL: Duration = Duration::from_secs(14_400);

/// Row code of a failed warmup pass.
const WARMUP_FAILED: &str = "WARMUP_FAILED";

/// Catalog scopes the loops warm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarmupScope {
    /// Jellyfin MBID index.
    Jellyfin,
    /// Navidrome MBID cache.
    Navidrome,
    /// Plex MBID cache.
    Plex,
}

impl WarmupScope {
    /// Scope key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Navidrome => "navidrome",
            Self::Plex => "plex",
        }
    }

    /// The scope's loop name on the supervisor.
    pub fn job_name(self) -> &'static str {
        match self {
            Self::Jellyfin => "playback-warmup:jellyfin",
            Self::Navidrome => "playback-warmup:navidrome",
            Self::Plex => "playback-warmup:plex",
        }
    }

    /// Startup delay before the first build (staggers boot work, v2 exact).
    pub fn startup_delay(self) -> Duration {
        match self {
            Self::Jellyfin => JELLYFIN_STARTUP_DELAY,
            Self::Navidrome => NAVIDROME_STARTUP_DELAY,
            Self::Plex => PLEX_STARTUP_DELAY,
        }
    }

    /// Rebuild interval after the first build. Jellyfin warms once (v2
    /// `warm_jellyfin_mbid_index` is a one-shot, not a loop); Navidrome and
    /// Plex rebuild every four hours.
    pub fn interval(self) -> Option<Duration> {
        match self {
            Self::Jellyfin => None,
            Self::Navidrome | Self::Plex => Some(WARMUP_INTERVAL),
        }
    }
}

/// What one warmup pass accomplished, for logs and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupStats {
    /// Scope key that warmed.
    pub scope: &'static str,
    /// Index entries added or refreshed.
    pub warmed: usize,
    /// Stale entries pruned.
    pub pruned: usize,
}

/// Start the warmup for `scope` on `supervisor`: after the startup delay,
/// one pass, then one every interval for the scopes that rebuild.
pub fn start<F, Fut>(
    supervisor: &Supervisor,
    scope: WarmupScope,
    work: F,
) -> Result<(), AlreadyRunning>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<WarmupStats, String>> + Send,
{
    let schedule = Schedule::new(scope.interval().unwrap_or(Duration::ZERO))
        .with_initial_delay(scope.startup_delay());
    supervisor.periodic(scope.job_name(), schedule, WarmupTick { scope, work })
}

/// One warmup pass.
struct WarmupTick<F> {
    scope: WarmupScope,
    work: F,
}

impl<F, Fut> Tick for WarmupTick<F>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<WarmupStats, String>> + Send,
{
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        let scope = self.scope;
        let outcome = (self.work)().await;
        let next = match scope.interval() {
            Some(_) => Next::Continue,
            None => Next::Finish,
        };
        match outcome {
            Ok(stats) => {
                tracing::debug!(
                    scope = scope.key(),
                    warmed = stats.warmed,
                    pruned = stats.pruned,
                    "mbid warmup pass done"
                );
                Ok(next)
            }
            // A one-time warmup that failed is not tried again.
            Err(cause) if next == Next::Finish => {
                tracing::error!(scope = scope.key(), %cause, "mbid warmup failed");
                Ok(next)
            }
            Err(cause) => Err(TickError::new(
                WARMUP_FAILED,
                format!("{}: {cause}", scope.key()),
            )),
        }
    }
}
