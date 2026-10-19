//! Library precache supervisor: on-demand runs under a watchdog.
//!
//! Precache is not periodic; it runs when asked (after a sync or from the
//! admin UI) and holds [`JOB_NAME`] only while a run is live. Each run
//! pairs the phase work with a watchdog ticking every [`DEFAULT_WATCHDOG_TICK`]:
//! no progress for `stall_timeout` or a run past `max_timeout` cancels the
//! work and fails the run, exactly the v2 `orchestrator.py` contract. The
//! caller stops a run through the supervisor like any other job; the watchdog
//! tick is the recovery path, and the clamped tick floor bounds it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use super::BoxFuture;
use super::supervisor::{AlreadyRunning, Supervisor, TickCtx, TickError};

/// Name of a precache run while it is live.
pub const JOB_NAME: &str = "precache-library";

/// Name of the loop that starts a run once scans have gone quiet.
pub const AFTER_SCAN_JOB: &str = "precache-after-scan";

/// Row code of a run the watchdog stopped.
pub const WATCHDOG: &str = "PRECACHE_WATCHDOG";

/// Row code of a run that failed.
pub const FAILED: &str = "PRECACHE_FAILED";

/// Watchdog cadence from v2, unchanged.
pub const DEFAULT_WATCHDOG_TICK: Duration = Duration::from_secs(30);

/// Smallest watchdog tick the supervisor honors. A zero tick would spin the
/// watchdog against a hung run instead of sleeping between checks.
pub const MIN_WATCHDOG_TICK: Duration = Duration::from_millis(50);

/// How a precache run ended, for the caller that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrecacheOutcome {
    /// Every phase finished.
    Done,
    /// The watchdog cancelled a stalled or overlong run, with the reason v2
    /// reports (stall minutes and phase, or max-timeout hours).
    Watchdog(String),
    /// A phase raised, with its message.
    Failed(String),
    /// The supervisor cancelled the run (shutdown or admin cancel). The phases
    /// stop at their next await; partial work stays resumable.
    Cancelled,
}

/// Progress handle the phase work calls as it moves. The watchdog reads the
/// last-beat time; anything older than the stall timeout trips the run.
#[derive(Debug, Clone)]
pub struct Progress {
    last_at: Arc<Mutex<Instant>>,
    phase: Arc<Mutex<String>>,
}

impl Progress {
    /// Fresh progress stamped now, in the given phase.
    pub fn new(phase: &str) -> Self {
        Self {
            last_at: Arc::new(Mutex::new(Instant::now())),
            phase: Arc::new(Mutex::new(phase.to_owned())),
        }
    }

    /// Record progress, optionally moving to a new phase.
    pub fn beat(&self, phase: Option<&str>) {
        *self
            .last_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
        if let Some(next) = phase {
            *self
                .phase
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = next.to_owned();
        }
    }

    /// How long since the last beat.
    pub fn stalled_for(&self) -> Duration {
        Instant::now().saturating_duration_since(
            *self
                .last_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Current phase name for watchdog messages.
    pub fn phase(&self) -> String {
        self.phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The phase work: artist, album, discovery, and AudioDB passes behind one
/// seam. Implementations call `progress.beat` as they move; a run that never
/// beats trips the stall timeout.
pub trait PrecacheWork: Send + Sync + 'static {
    /// Run every phase to completion or the first failure.
    fn run(&self, progress: Progress) -> BoxFuture<'_, Result<(), String>>;
}

/// Watchdog limits: fixed in production, short in tests.
#[derive(Debug, Clone, Copy)]
pub struct PrecacheLimits {
    /// No progress for this long trips the run.
    pub stall_timeout: Duration,
    /// A run past this long trips whatever it is doing.
    pub max_timeout: Duration,
    /// How often the watchdog checks. Clamped to [`MIN_WATCHDOG_TICK`].
    pub watchdog_tick: Duration,
}

impl PrecacheLimits {
    /// Limits from the two timeouts plus the default tick.
    pub fn new(stall_timeout: Duration, max_timeout: Duration) -> Self {
        Self {
            stall_timeout,
            max_timeout,
            watchdog_tick: DEFAULT_WATCHDOG_TICK,
        }
    }

    /// Override the watchdog tick (tests run it fast).
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_watchdog_tick(mut self, tick: Duration) -> Self {
        self.watchdog_tick = tick;
        self
    }

    fn tick(&self) -> Duration {
        self.watchdog_tick.max(MIN_WATCHDOG_TICK)
    }
}

/// Start one supervised run. Rejected while a run is live, like any duplicate
/// job name; the name frees when the run lands. `on_end` is called once
/// with the outcome however the run ends: finished, cancelled, aborted at
/// its grace (as [`PrecacheOutcome::Cancelled`]) or panicked (as
/// [`PrecacheOutcome::Failed`]).
pub fn spawn_run<W: PrecacheWork>(
    supervisor: &Supervisor,
    work: W,
    limits: PrecacheLimits,
    on_end: impl FnOnce(&PrecacheOutcome) + Send + 'static,
) -> Result<PrecacheHandle, AlreadyRunning> {
    let outcome: Arc<Mutex<Option<PrecacheOutcome>>> = Arc::new(Mutex::new(None));
    let done = Arc::new(Notify::new());
    // Built before the start, so a run aborted before its first poll still
    // lands.
    let mut landing = Landing {
        outcome: Arc::clone(&outcome),
        done: Arc::clone(&done),
        on_end: Some(Box::new(on_end)),
    };
    supervisor.one_shot(JOB_NAME, move |ctx| async move {
        let result = supervise(&ctx, &work, &limits).await;
        landing.land(result.clone());
        match result {
            PrecacheOutcome::Done | PrecacheOutcome::Cancelled => Ok(()),
            PrecacheOutcome::Watchdog(cause) => Err(TickError::new(WATCHDOG, cause)),
            PrecacheOutcome::Failed(cause) => Err(TickError::new(FAILED, cause)),
        }
    })?;
    Ok(PrecacheHandle { outcome, done })
}

/// What runs once with a run's outcome.
type OnEnd = Box<dyn FnOnce(&PrecacheOutcome) + Send>;

/// Where a run's outcome lands. Dropped without one (the run was aborted
/// or panicked), it lands an outcome of its own, so waiters always return.
struct Landing {
    outcome: Arc<Mutex<Option<PrecacheOutcome>>>,
    done: Arc<Notify>,
    on_end: Option<OnEnd>,
}

impl Landing {
    fn land(&mut self, outcome: PrecacheOutcome) {
        let Some(on_end) = self.on_end.take() else {
            return;
        };
        on_end(&outcome);
        *self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outcome);
        self.done.notify_one();
    }
}

impl Drop for Landing {
    fn drop(&mut self) {
        let outcome = if std::thread::panicking() {
            PrecacheOutcome::Failed("the precache run panicked".to_owned())
        } else {
            PrecacheOutcome::Cancelled
        };
        self.land(outcome);
    }
}

/// Handle to a live run: waits for the outcome without polling.
#[derive(Debug, Clone)]
pub struct PrecacheHandle {
    outcome: Arc<Mutex<Option<PrecacheOutcome>>>,
    done: Arc<Notify>,
}

impl PrecacheHandle {
    /// Wait for the run to land and return how it ended. The stored permit
    /// on `done` closes the check-then-wait race: a run landing between the
    /// read and the sleep still wakes this call.
    pub async fn wait(&self) -> PrecacheOutcome {
        loop {
            if let Some(outcome) = self
                .outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
            {
                return outcome;
            }
            self.done.notified().await;
        }
    }
}

/// Drive one run to its outcome: work racing the watchdog and the stop.
async fn supervise<W: PrecacheWork>(
    ctx: &TickCtx,
    work: &W,
    limits: &PrecacheLimits,
) -> PrecacheOutcome {
    let started = Instant::now();
    let progress = Progress::new("starting");
    let work_future = work.run(progress.clone());
    tokio::pin!(work_future);
    loop {
        tokio::select! {
            biased;
            () = ctx.token().cancelled() => return PrecacheOutcome::Cancelled,
            result = &mut work_future => {
                return match result {
                    Ok(()) => PrecacheOutcome::Done,
                    Err(cause) => {
                        tracing::error!(%cause, "precache run failed");
                        PrecacheOutcome::Failed(cause)
                    }
                };
            }
            _ = tokio::time::sleep(limits.tick()) => {
                let elapsed = started.elapsed();
                if elapsed > limits.max_timeout {
                    let message = format!(
                        "Sync exceeded maximum timeout ({:.1}h)",
                        limits.max_timeout.as_secs_f64() / 3600.0
                    );
                    tracing::error!("{message}");
                    return PrecacheOutcome::Watchdog(message);
                }
                let stalled = progress.stalled_for();
                if stalled > limits.stall_timeout {
                    let message = format!(
                        "Sync stalled: no progress for {:.0} minutes during {} phase",
                        stalled.as_secs_f64() / 60.0,
                        progress.phase(),
                    );
                    tracing::error!("{message}");
                    return PrecacheOutcome::Watchdog(message);
                }
                ctx.heartbeat();
            }
        }
    }
}
