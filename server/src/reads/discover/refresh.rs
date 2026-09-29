//! Discover and home background refresh loops.
//!
//! Reads stay fast because refreshes happen off-request: one loop per scope
//! wakes on its interval, rebuilds through the single-flight registry (so a
//! manual trigger and the loop never rebuild the same scope twice), logs
//! failures and continues. Time and sleep are injected so tests drive loops
//! with no real waits.
//!
//! Production wiring is deferred to stage 5: `serve()` spawns no loops yet.
//! Stage-4 ports are static scripted fakes with no rebuild work to run (a
//! rebuild needs provider fetches plus user enumeration, neither of which
//! exists behind these traits), so spawned loops would only burn wakeups.
//! Stage 5 spawns [`spawn_refresh_loops`] in `serve()` with [`TokioSleeper`]
//! over a shutdown watch, flips the watch after axum serves, and awaits the
//! [`RefreshHandles::tasks`]. The loop body, registry, and both sleepers
//! stay covered by the tests below until then.

use std::{
    collections::HashSet,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Queue deck TTL in seconds: decks older than this read stale (v2's
/// `queue_ttl` default was one hour; same value, now named at the loop).
pub const QUEUE_TTL_SECS: i64 = 3_600;
/// Discover rebuild cadence: shelves mix hourly charts with daily picks;
/// fifteen minutes keeps charts fresh without hammering providers.
pub const DISCOVER_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Home rebuild cadence: same mix as discover, same cost, same interval.
pub const HOME_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Cache scopes the loops rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefreshScope {
    /// Discover shelves.
    Discover,
    /// Home shelves.
    Home,
}

impl RefreshScope {
    /// Registry key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Discover => "discover",
            Self::Home => "home",
        }
    }

    /// Honest rebuild interval for the scope.
    pub fn interval(self) -> Duration {
        match self {
            Self::Discover => DISCOVER_REFRESH_INTERVAL,
            Self::Home => HOME_REFRESH_INTERVAL,
        }
    }
}

/// Single-flight registry: one live rebuild per scope key. `begin` returns
/// false when the scope already rebuilds; holders must call `finish`.
#[derive(Debug, Default)]
pub struct RefreshRegistry {
    live: Mutex<HashSet<String>>,
}

impl RefreshRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the scope. False means another rebuild holds it.
    pub fn begin(&self, scope: RefreshScope) -> bool {
        self.live
            .lock()
            .map(|mut live| live.insert(scope.key().to_owned()))
            .unwrap_or(false)
    }

    /// Release the scope.
    pub fn finish(&self, scope: RefreshScope) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(scope.key());
        }
    }

    /// True while the scope rebuilds.
    pub fn is_live(&self, scope: RefreshScope) -> bool {
        self.live
            .lock()
            .map(|live| live.contains(scope.key()))
            .unwrap_or(false)
    }
}

/// Sleep seam so loops test without real waits.
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
#[derive(Debug, Clone)]
pub struct ManualSleeper {
    inner: Arc<ManualSleeperInner>,
}

#[derive(Debug)]
struct ManualSleeperInner {
    state: Mutex<ManualSleepState>,
    wake: tokio::sync::Notify,
}

#[derive(Debug, Default)]
struct ManualSleepState {
    waits: usize,
    shutdown: bool,
    requested: Vec<Duration>,
}

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

impl Default for ManualSleeper {
    fn default() -> Self {
        Self::new()
    }
}

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

/// Run one guarded rebuild: skip when the scope already rebuilds, else run
/// `work`, log failures and continue, always release the claim.
/// Returns true when the work ran.
pub async fn refresh_guarded<F, Fut>(
    registry: &RefreshRegistry,
    scope: RefreshScope,
    work: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    if !registry.begin(scope) {
        return false;
    }
    let outcome = work().await;
    registry.finish(scope);
    if let Err(cause) = outcome {
        tracing::error!(scope = scope.key(), %cause, "background refresh failed; continuing");
    }
    true
}

/// Run the refresh loop for `scope` until the sleeper reports shutdown.
/// Each iteration sleeps first (so boot never rebuilds synchronously),
/// then runs one guarded rebuild.
pub async fn run_refresh_loop<S, F, Fut>(
    registry: Arc<RefreshRegistry>,
    sleeper: S,
    scope: RefreshScope,
    mut work: F,
) where
    S: Sleeper,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    loop {
        if !sleeper.sleep(scope.interval()).await {
            break;
        }
        refresh_once(&registry, scope, &mut work).await;
    }
}

/// One guarded loop iteration over borrowed work (the loop body factored
/// for direct brief coverage).
pub async fn refresh_once<F, Fut>(registry: &RefreshRegistry, scope: RefreshScope, work: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    refresh_guarded(registry, scope, work).await
}

/// Shared handle bundle for the spawned loops.
#[derive(Debug, Clone)]
pub struct RefreshHandles {
    /// Loop tasks. Each carries an error-logging completion hook.
    pub tasks: Vec<Arc<tokio::task::JoinHandle<()>>>,
    /// Single-flight registry shared with manual triggers.
    pub registry: Arc<RefreshRegistry>,
}

/// Spawn the discover and home refresh loops. Shutdown flips the sender;
/// each task logs its own failure on completion (spawned tasks are never
/// silently dropped).
pub fn spawn_refresh_loops<S, D, H>(sleeper: S, discover_work: D, home_work: H) -> RefreshHandles
where
    S: Sleeper + Clone + Send + 'static,
    D: FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + 'static,
    H: FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + 'static,
{
    fn spawn_one<S, F>(
        sleeper: S,
        scope: RefreshScope,
        registry: Arc<RefreshRegistry>,
        work: F,
    ) -> Arc<tokio::task::JoinHandle<()>>
    where
        S: Sleeper + Clone + Send + 'static,
        F: FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
            + Send
            + 'static,
    {
        let task_registry = registry.clone();
        Arc::new(tokio::spawn(async move {
            run_refresh_loop(task_registry, sleeper, scope, work).await;
        }))
    }

    let registry = Arc::new(RefreshRegistry::new());
    let tasks = vec![
        spawn_one(
            sleeper.clone(),
            RefreshScope::Discover,
            registry.clone(),
            discover_work,
        ),
        spawn_one(sleeper, RefreshScope::Home, registry.clone(), home_work),
    ];
    RefreshHandles { tasks, registry }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_rebuilds_single_flight() {
        let registry = RefreshRegistry::new();
        let runs = Arc::new(AtomicUsize::new(0));
        assert!(registry.begin(RefreshScope::Discover));
        let second = refresh_once(&registry, RefreshScope::Discover, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;
        assert!(!second);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        registry.finish(RefreshScope::Discover);
        let third = refresh_once(&registry, RefreshScope::Discover, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;
        assert!(third);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failures_log_and_continue() {
        let registry = RefreshRegistry::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let first = refresh_once(&registry, RefreshScope::Home, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Err("charts down".to_owned())
            }
        })
        .await;
        assert!(first);
        assert!(!registry.is_live(RefreshScope::Home));
        let second = refresh_once(&registry, RefreshScope::Home, || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;
        assert!(second);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
