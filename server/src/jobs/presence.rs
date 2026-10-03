//! Now-playing presence loop: fold upstream sessions into one feed.
//!
//! Every [`POLL_INTERVAL`] the loop sweeps stale native sessions, then
//! reconciles each upstream integration (Jellyfin, Navidrome, Plex)
//! independently. A disabled source clears its own slice to empty; a failing
//! source logs and keeps its last slice, and never breaks the cycle or the
//! other sources; the sweep failing just logs and carries on. That
//! per-source isolation is the v2 contract from `now_playing_poller.py`,
//! kept verbatim.

use std::sync::Arc;
use std::time::Duration;

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::{Schedule, SplitMix64, sleep_or_stop};

/// Registered name of the presence loop.
pub const JOB_NAME: &str = "now-playing-presence";

/// v2 poll cadence, unchanged.
pub const POLL_INTERVAL: Duration = Duration::from_secs(4);

/// Default spread so presence polls drift against other 4 s timers.
pub const DEFAULT_JITTER: Duration = Duration::from_secs(1);

/// One upstream listening session, mapped into feed shape.
#[derive(Debug, Clone, PartialEq)]
pub struct PresenceSession {
    /// Stable per-source key (`jellyfin:{id}`, `navidrome:{user}:{player}...`,
    /// `plex:{id}`) so reconcile can diff.
    pub key: String,
    /// Who is listening, when the source names them.
    pub user_name: String,
    /// Player or device label.
    pub device_name: String,
    /// Track title; empty means the entry is skipped upstream of here.
    pub track_name: String,
    /// Track artist, empty when unknown.
    pub artist_name: String,
    /// Album title, when the source reports one.
    pub album_name: Option<String>,
    /// Cover URL in feed space (Navidrome entries point at the cover route).
    pub cover_url: String,
    /// True when paused (Navidrome never reports pause, so always false).
    pub is_paused: bool,
    /// Playback position in milliseconds.
    pub progress_ms: i64,
    /// Track length in milliseconds.
    pub duration_ms: i64,
}

/// Which upstream integrations are currently feeding presence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStatus {
    /// Jellyfin sessions enabled.
    pub jellyfin: bool,
    /// Navidrome now-playing enabled.
    pub navidrome: bool,
    /// Plex sessions enabled.
    pub plex: bool,
}

/// The presence feed itself: stale-session sweep plus per-source reconcile.
pub trait PresenceStore: Send + Sync + 'static {
    /// Drop expired native and compat sessions.
    fn sweep(&self) -> BoxFuture<'_, ()>;
    /// Replace one source's slice with the freshly polled sessions.
    fn reconcile(&self, source: &str, sessions: Vec<PresenceSession>) -> BoxFuture<'_, ()>;
}

/// Upstream integrations behind one seam.
pub trait PresenceSources: Send + Sync + 'static {
    /// Current enablement per source, re-read every cycle.
    fn status(&self) -> BoxFuture<'_, SourceStatus>;
    /// Current Jellyfin sessions, mapped. `Err` carries the cause for the log.
    fn poll_jellyfin(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>>;
    /// Current Navidrome entries, mapped.
    fn poll_navidrome(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>>;
    /// Current Plex sessions, mapped.
    fn poll_plex(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>>;
}

/// The stage-10 cadence: v2's 4 s with a 1 s spread.
pub fn default_schedule() -> Schedule {
    Schedule::new(POLL_INTERVAL)
        .with_jitter(DEFAULT_JITTER)
        .with_backoff(Duration::from_secs(1), Duration::from_secs(30))
}

/// Spawn the loop on a registry.
pub async fn spawn_on<S, T, P>(
    registry: &JobRegistry<S>,
    store: T,
    sources: P,
    schedule: Schedule,
) -> Result<(), super::registry::AlreadyRunning>
where
    S: RegistryStore,
    T: PresenceStore,
    P: PresenceSources,
{
    let store = Arc::new(store);
    let sources = Arc::new(sources);
    registry
        .spawn(JOB_NAME, JobKind::Ephemeral, None, move |ctx| {
            let store = Arc::clone(&store);
            let sources = Arc::clone(&sources);
            async move { run(ctx, store.as_ref(), sources.as_ref(), &schedule).await }
        })
        .await
}

/// One full cycle: sweep, then each enabled source polled and reconciled in
/// isolation. Returns the cycle outcome for backoff accounting: a sweep picks
/// up no failures (it cannot fail loudly), so only source polls count, and a
/// cycle with every source failing still schedules the next one.
pub async fn run_once<T, P>(store: &T, sources: &P) -> Result<(), ()>
where
    T: PresenceStore,
    P: PresenceSources,
{
    store.sweep().await;
    let status = sources.status().await;
    let mut failures = 0_u32;
    if status.jellyfin {
        match sources.poll_jellyfin().await {
            Ok(sessions) => store.reconcile("jellyfin", sessions).await,
            Err(cause) => {
                failures += 1;
                tracing::debug!(source = "jellyfin", %cause, "presence poll failed");
            }
        }
    } else {
        store.reconcile("jellyfin", Vec::new()).await;
    }
    if status.navidrome {
        match sources.poll_navidrome().await {
            Ok(sessions) => store.reconcile("navidrome", sessions).await,
            Err(cause) => {
                failures += 1;
                tracing::debug!(source = "navidrome", %cause, "presence poll failed");
            }
        }
    } else {
        store.reconcile("navidrome", Vec::new()).await;
    }
    if status.plex {
        match sources.poll_plex().await {
            Ok(sessions) => store.reconcile("plex", sessions).await,
            Err(cause) => {
                failures += 1;
                tracing::debug!(source = "plex", %cause, "presence poll failed");
            }
        }
    } else {
        store.reconcile("plex", Vec::new()).await;
    }
    if failures == 0 { Ok(()) } else { Err(()) }
}

/// Run until stop fires. Failures back the schedule off; they never exit.
pub async fn run<S, T, P>(ctx: JobCtx<S>, store: &T, sources: &P, schedule: &Schedule) -> JobExit
where
    S: RegistryStore,
    T: PresenceStore,
    P: PresenceSources,
{
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    let mut failures = 0_u32;
    loop {
        match run_once(store, sources).await {
            Ok(()) => failures = 0,
            Err(()) => failures = failures.saturating_add(1),
        }
        ctx.heartbeat().await;
        if sleep_or_stop(schedule.next_delay(&mut rng, failures), ctx.stop()).await {
            return JobExit::Stopped;
        }
    }
}
