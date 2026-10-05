//! Jellyfin/Navidrome/Plex MBID warmup loops.
//!
//! Remote catalogs resolve faster when their MusicBrainz id index is warm,
//! so each integration rebuilds its index off-request: one loop per source
//! wakes on its honest interval, rebuilds through the single-flight
//! registry (so a manual trigger and the loop never rebuild the same index
//! twice), logs failures and continues. Time and sleep are injected so
//! tests drive loops with no real waits.
//!
//! Cadences are v2-exact (`backend/core/tasks.py`): Jellyfin builds once
//! after a short startup delay; Navidrome and Plex rebuild every four
//! hours after theirs. Production wiring: the integrator spawns the three
//! loops with [`TokioSleeper`] over a shutdown watch (see the wiring block
//! in `mod.rs`), flips the watch after axum serves, and awaits the tasks.

use std::{
    collections::HashSet,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Jellyfin startup delay in seconds (v2 `warm_jellyfin_mbid_index`).
pub const JELLYFIN_STARTUP_DELAY: Duration = Duration::from_secs(8);
/// Navidrome startup delay in seconds (v2 `warm_navidrome_mbid_cache`).
pub const NAVIDROME_STARTUP_DELAY: Duration = Duration::from_secs(12);
/// Plex startup delay in seconds (v2 `warm_plex_mbid_cache`).
pub const PLEX_STARTUP_DELAY: Duration = Duration::from_secs(15);
/// Navidrome and Plex rebuild cadence (v2 sleeps `14400`, four hours).
pub const WARMUP_INTERVAL: Duration = Duration::from_secs(14_400);

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
    /// Registry key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Navidrome => "navidrome",
            Self::Plex => "plex",
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

/// What one warmup pass accomplished, for logs and briefs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupStats {
    /// Scope key that warmed.
    pub scope: &'static str,
    /// Index entries added or refreshed.
    pub warmed: usize,
    /// Stale entries pruned.
    pub pruned: usize,
}

/// Single-flight registry: one live warmup per scope key. `begin` returns
/// false when the scope already warms; holders must call `finish`.
#[derive(Debug, Default)]
pub struct WarmupRegistry {
    live: Mutex<HashSet<String>>,
}

impl WarmupRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the scope. False means another warmup holds it.
    pub fn begin(&self, scope: WarmupScope) -> bool {
        self.live
            .lock()
            .map(|mut live| live.insert(scope.key().to_owned()))
            .unwrap_or(false)
    }

    /// Release the scope.
    pub fn finish(&self, scope: WarmupScope) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(scope.key());
        }
    }

    /// True while the scope warms.
    pub fn is_live(&self, scope: WarmupScope) -> bool {
        self.live
            .lock()
            .map(|live| live.contains(scope.key()))
            .unwrap_or(false)
    }
}

/// Sleep seam so loops test without real waits. (The stage-5 refresh loops
/// carry an identical seam; unifying the two behind one shared sleeper is
/// a follow-up, not this slice.)
pub trait Sleeper: Send + Sync {
    /// Sleep `duration`, returning false when shutdown won the race.
    fn sleep(&self, duration: Duration) -> impl Future<Output = bool> + Send;
}

/// Production sleeper over tokio time and a shutdown watch.
#[derive(Debug, Clone)]
pub struct TokioSleeper {
    shutdown: tokio::sync::watch::Receiver<bool>,
}

impl TokioSleeper {
    /// Build a sleeper that wakes when `shutdown` flips true.
    pub fn new(shutdown: tokio::sync::watch::Receiver<bool>) -> Self {
        Self { shutdown }
    }
}

impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) -> bool {
        let mut shutdown = self.shutdown.clone();
        tokio::select! {
            () = tokio::time::sleep(duration) => !*shutdown.borrow(),
            _ = shutdown.changed() => false,
        }
    }
}

/// Manual sleeper for tests: `wake` releases one waiter, `shut_down`
/// releases all with false. Requested durations are recorded so briefs
/// can assert the honest intervals.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct ManualSleeper {
    inner: Arc<ManualSleeperInner>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
struct ManualSleeperInner {
    state: Mutex<ManualSleepState>,
    wake: tokio::sync::Notify,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
struct ManualSleepState {
    waits: usize,
    shutdown: bool,
    requested: Vec<Duration>,
}

#[cfg(any(test, feature = "test-support"))]
impl ManualSleeper {
    /// Build a fresh manual sleeper.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ManualSleeperInner {
                state: Mutex::new(ManualSleepState::default()),
                wake: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Release one waiter to continue the loop.
    pub fn wake(&self) {
        self.inner.wake.notify_one();
    }

    /// Shut the loop down: waiters return false.
    pub fn shut_down(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.shutdown = true;
        }
        self.inner.wake.notify_waiters();
    }

    /// Durations the loop requested, in order.
    pub fn requested(&self) -> Vec<Duration> {
        self.inner
            .state
            .lock()
            .map(|state| state.requested.clone())
            .unwrap_or_default()
    }

    /// Waiters currently parked in `sleep`.
    pub fn waits(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.waits)
            .unwrap_or(0)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for ManualSleeper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Sleeper for ManualSleeper {
    async fn sleep(&self, duration: Duration) -> bool {
        if let Ok(mut state) = self.inner.state.lock() {
            if state.shutdown {
                return false;
            }
            state.requested.push(duration);
            state.waits += 1;
        }
        self.inner.wake.notified().await;
        if let Ok(mut state) = self.inner.state.lock() {
            state.waits = state.waits.saturating_sub(1);
            if state.shutdown {
                return false;
            }
        }
        true
    }
}

/// Run one guarded warmup: skip when the scope already warms, else run
/// `work`, log failures and continue, always release the claim.
/// Returns true when the work ran.
pub async fn warmup_guarded<F, Fut>(registry: &WarmupRegistry, scope: WarmupScope, work: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<WarmupStats, String>>,
{
    if !registry.begin(scope) {
        return false;
    }
    let outcome = work().await;
    registry.finish(scope);
    match outcome {
        Ok(stats) => {
            tracing::debug!(
                scope = scope.key(),
                warmed = stats.warmed,
                pruned = stats.pruned,
                "mbid warmup pass done"
            );
        }
        Err(cause) => {
            tracing::error!(scope = scope.key(), %cause, "mbid warmup failed; continuing");
        }
    }
    true
}

/// Run the warmup loop for `scope` until the sleeper reports shutdown.
/// Each loop sleeps its startup delay first (so boot never warms
/// synchronously), runs one guarded pass, then repeats on the honest
/// interval; the one-shot Jellyfin scope returns after its first pass.
pub async fn run_warmup_loop<S, F, Fut>(
    registry: Arc<WarmupRegistry>,
    sleeper: S,
    scope: WarmupScope,
    mut work: F,
) where
    S: Sleeper,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<WarmupStats, String>>,
{
    if !sleeper.sleep(scope.startup_delay()).await {
        return;
    }
    warmup_once(&registry, scope, &mut work).await;
    let Some(interval) = scope.interval() else {
        return;
    };
    loop {
        if !sleeper.sleep(interval).await {
            break;
        }
        warmup_once(&registry, scope, &mut work).await;
    }
}

/// One guarded loop iteration over borrowed work (the loop body factored
/// for direct brief coverage).
pub async fn warmup_once<F, Fut>(registry: &WarmupRegistry, scope: WarmupScope, work: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<WarmupStats, String>>,
{
    warmup_guarded(registry, scope, work).await
}

/// Shared handle bundle for the spawned loops.
#[derive(Debug)]
pub struct WarmupHandles {
    /// Loop tasks, awaited at shutdown. Each carries an error-logging
    /// completion hook.
    pub tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Single-flight registry shared with manual triggers.
    pub registry: Arc<WarmupRegistry>,
}

/// Spawn the Jellyfin, Navidrome, and Plex warmup loops. Shutdown flips the
/// sleeper's watch; each task logs its own failure on completion (spawned
/// tasks are never silently dropped).
pub fn spawn_warmup_loops<S, J, N, P>(
    sleeper: S,
    jellyfin_work: J,
    navidrome_work: N,
    plex_work: P,
) -> WarmupHandles
where
    S: Sleeper + Clone + Send + 'static,
    J: FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<WarmupStats, String>> + Send>>
        + Send
        + 'static,
    N: FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<WarmupStats, String>> + Send>>
        + Send
        + 'static,
    P: FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<WarmupStats, String>> + Send>>
        + Send
        + 'static,
{
    fn spawn_one<S, F>(
        sleeper: S,
        scope: WarmupScope,
        registry: Arc<WarmupRegistry>,
        work: F,
    ) -> tokio::task::JoinHandle<()>
    where
        S: Sleeper + Clone + Send + 'static,
        F: FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<WarmupStats, String>> + Send>>
            + Send
            + 'static,
    {
        let task_registry = registry.clone();
        tokio::spawn(async move {
            run_warmup_loop(task_registry, sleeper, scope, work).await;
        })
    }

    let registry = Arc::new(WarmupRegistry::new());
    let tasks = vec![
        spawn_one(
            sleeper.clone(),
            WarmupScope::Jellyfin,
            registry.clone(),
            jellyfin_work,
        ),
        spawn_one(
            sleeper.clone(),
            WarmupScope::Navidrome,
            registry.clone(),
            navidrome_work,
        ),
        spawn_one(sleeper, WarmupScope::Plex, registry.clone(), plex_work),
    ];
    WarmupHandles { tasks, registry }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_warmups_single_flight() {
        let registry = WarmupRegistry::new();
        let runs = Arc::new(AtomicUsize::new(0));
        assert!(registry.begin(WarmupScope::Navidrome));
        let second = warmup_once(&registry, WarmupScope::Navidrome, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(WarmupStats {
                    scope: "navidrome",
                    warmed: 0,
                    pruned: 0,
                })
            }
        })
        .await;
        assert!(!second);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        registry.finish(WarmupScope::Navidrome);
        let third = warmup_once(&registry, WarmupScope::Navidrome, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(WarmupStats {
                    scope: "navidrome",
                    warmed: 0,
                    pruned: 0,
                })
            }
        })
        .await;
        assert!(third);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failures_log_and_continue() {
        let registry = WarmupRegistry::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let first = warmup_once(&registry, WarmupScope::Plex, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Err("plex down".to_owned())
            }
        })
        .await;
        assert!(first);
        assert!(!registry.is_live(WarmupScope::Plex));
        let second = warmup_once(&registry, WarmupScope::Plex, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(WarmupStats {
                    scope: "plex",
                    warmed: 3,
                    pruned: 1,
                })
            }
        })
        .await;
        assert!(second);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn jellyfin_is_one_shot() {
        assert_eq!(WarmupScope::Jellyfin.interval(), None);
        assert_eq!(
            WarmupScope::Navidrome.interval(),
            Some(Duration::from_secs(14_400))
        );
        assert_eq!(
            WarmupScope::Plex.interval(),
            Some(Duration::from_secs(14_400))
        );
    }
}
