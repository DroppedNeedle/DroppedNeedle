//! Live-events watcher: a daily sweep at the admin's hour.
//!
//! The watcher walks every distinct followed artist through the live-event
//! sources once a day at the configured `poll_time`, plus one catch-up sweep
//! shortly after boot that skips artists swept in the last
//! [`CATCHUP_SKIP_RECENT_HOURS`] so restarts do not re-spend the day's
//! provider quota. The scheduler ticks every [`SCHEDULER_TICK`], re-reading
//! both the watcher and the poll time each tick, so a settings save takes
//! effect within a minute without a restart. A failing sweep still waits for
//! the next slot; the loop only exits on shutdown. All of that is the v2
//! `run_events_watcher_periodically` contract.
//!
//! One intended change: v2 schedules in server-local wall time while v3
//! schedules in UTC (the container pins `TZ=UTC`, so the two agree in
//! production) because the standard library cannot name the local zone
//! portably. The [`WatchClock`] seam owns that choice; tests drive a manual
//! clock.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::sleep_or_stop;

/// Registered name of the watcher loop.
pub const JOB_NAME: &str = "events-watcher";

/// Boot hold-off before the catch-up sweep, from v2.
pub const INITIAL_DELAY: Duration = Duration::from_secs(420);
/// Scheduler tick, from v2.
pub const SCHEDULER_TICK: Duration = Duration::from_secs(60);
/// Default spread on the scheduler tick.
pub const DEFAULT_JITTER: Duration = Duration::from_secs(5);
/// Catch-up sweeps skip artists swept within this window, from v2.
pub const CATCHUP_SKIP_RECENT_HOURS: f64 = 20.0;

/// Fallback poll time when the saved value is garbage, from v2.
const FALLBACK_POLL_MINUTES: u16 = 6 * 60;

/// How a sweep ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepEnd {
    /// Walked every artist it meant to.
    Finished,
    /// Saw the stop signal and quit between artists.
    Stopped,
}

/// Sweep the live-event sources behind a seam.
pub trait EventsWatcher: Send + Sync + 'static {
    /// Run one full sweep. `skip_recent_hours` narrows the catch-up sweep to
    /// artists not swept within the window; daily sweeps pass nothing. A
    /// sweep walks artists for up to an hour, so it watches `stop` between
    /// artists and returns [`SweepEnd::Stopped`] when it fires.
    fn run_sweep(
        &self,
        skip_recent_hours: Option<f64>,
        stop: Arc<Notify>,
    ) -> BoxFuture<'_, Result<SweepEnd, String>>;
}

/// No sweep wired (states without a database): every sweep finishes at
/// once without walking anything.
impl<W: EventsWatcher> EventsWatcher for Option<W> {
    fn run_sweep(
        &self,
        skip_recent_hours: Option<f64>,
        stop: Arc<Notify>,
    ) -> BoxFuture<'_, Result<SweepEnd, String>> {
        match self {
            Some(watcher) => watcher.run_sweep(skip_recent_hours, stop),
            None => Box::pin(async { Ok(SweepEnd::Finished) }),
        }
    }
}

/// The admin's `poll_time` (`HH:MM`), re-read every tick.
pub trait PollTimeSource: Send + Sync + 'static {
    /// Current configured poll time.
    fn poll_time(&self) -> String;
}

/// Wall time in whole days plus minutes since midnight. Minute resolution is
/// all a daily scheduler needs, and the shape keeps the clock seam free of
/// date libraries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WallTime {
    /// Days since the unix epoch.
    pub days_since_epoch: i64,
    /// Minutes since midnight, 0-1439.
    pub minutes_since_midnight: u16,
}

impl WallTime {
    /// Midnight `days` after the epoch.
    #[cfg(any(test, feature = "test-support"))]
    pub fn day(days_since_epoch: i64) -> Self {
        Self {
            days_since_epoch,
            minutes_since_midnight: 0,
        }
    }
}

/// Clock behind a seam so tests drive wall time by hand.
pub trait WatchClock: Clone + Send + Sync + 'static {
    /// Current wall time.
    fn now(&self) -> WallTime;
}

/// Production clock: UTC days and minutes from the system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemWatchClock;

impl WatchClock for SystemWatchClock {
    fn now(&self) -> WallTime {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|span| span.as_secs())
            .unwrap_or(0);
        WallTime {
            days_since_epoch: (secs / 86_400) as i64,
            minutes_since_midnight: ((secs % 86_400) / 60) as u16,
        }
    }
}

/// First wall time with wall clock `poll` strictly after `after`. Garbage in
/// the `HH:MM` falls back to 06:00 (the schema validates at the API boundary,
/// so this only ever sees old or hand-edited rows).
pub fn next_daily_occurrence(poll: &str, after: WallTime) -> WallTime {
    let minutes = parse_poll_time(poll);
    let candidate = WallTime {
        days_since_epoch: after.days_since_epoch,
        minutes_since_midnight: minutes,
    };
    if candidate <= after {
        WallTime {
            days_since_epoch: after.days_since_epoch + 1,
            minutes_since_midnight: minutes,
        }
    } else {
        candidate
    }
}

/// Parse `HH:MM` into minutes since midnight, or 06:00 on any garbage.
fn parse_poll_time(poll: &str) -> u16 {
    let mut parts = poll.split(':');
    let parsed = match (parts.next(), parts.next(), parts.next()) {
        (Some(hour), Some(minute), None) => match (hour.parse::<u16>(), minute.parse::<u16>()) {
            (Ok(hour), Ok(minute)) if hour <= 23 && minute <= 59 => Some(hour * 60 + minute),
            _ => None,
        },
        _ => None,
    };
    parsed.unwrap_or(FALLBACK_POLL_MINUTES)
}

/// Spawn the loop on a registry.
pub async fn spawn_on<S, W, P, C>(
    registry: &JobRegistry<S>,
    watcher: W,
    poll_source: P,
    clock: C,
) -> Result<(), super::registry::AlreadyRunning>
where
    S: RegistryStore,
    W: EventsWatcher,
    P: PollTimeSource,
    C: WatchClock,
{
    let watcher = Arc::new(watcher);
    let poll_source = Arc::new(poll_source);
    let clock = Arc::new(clock);
    registry
        .spawn(JOB_NAME, JobKind::Ephemeral, None, move |ctx| {
            let watcher = Arc::clone(&watcher);
            let poll_source = Arc::clone(&poll_source);
            let clock = Arc::clone(&clock);
            async move {
                run(
                    ctx,
                    watcher.as_ref(),
                    poll_source.as_ref(),
                    clock.as_ref(),
                    &LoopConfig::default(),
                )
                .await
            }
        })
        .await
}

/// Loop pacing. Defaults are the v2 constants; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct LoopConfig {
    /// Boot hold-off before the catch-up sweep.
    pub initial_delay: Duration,
    /// Scheduler tick between slot checks.
    pub tick: Duration,
    /// Spread added to each tick.
    pub jitter: Duration,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            initial_delay: INITIAL_DELAY,
            tick: SCHEDULER_TICK,
            jitter: DEFAULT_JITTER,
        }
    }
}

/// Run until stop fires. Exactly one sleep per iteration, error path included.
pub async fn run<S, W, P, C>(
    ctx: JobCtx<S>,
    watcher: &W,
    poll_source: &P,
    clock: &C,
    config: &LoopConfig,
) -> JobExit
where
    S: RegistryStore,
    W: EventsWatcher,
    P: PollTimeSource,
    C: WatchClock,
{
    if sleep_or_stop(config.initial_delay, ctx.stop()).await {
        return JobExit::Stopped;
    }
    let mut rng_state =
        super::schedule::SplitMix64::new(super::schedule::SplitMix64::seed_from_time());
    let mut last_sweep: Option<WallTime> = None;
    loop {
        let now = clock.now();
        let due = match last_sweep {
            None => Some(Some(CATCHUP_SKIP_RECENT_HOURS)),
            Some(previous) => {
                (now >= next_daily_occurrence(&poll_source.poll_time(), previous)).then_some(None)
            }
        };
        if let Some(skip_recent_hours) = due {
            match watcher
                .run_sweep(skip_recent_hours, Arc::clone(ctx.stop()))
                .await
            {
                Ok(SweepEnd::Stopped) => return JobExit::Stopped,
                Ok(SweepEnd::Finished) => {}
                Err(cause) => tracing::error!(%cause, "events watcher sweep failed"),
            }
            // A failing sweep still waits for the next slot.
            last_sweep = Some(now);
            ctx.heartbeat().await;
        }
        let tick = config
            .tick
            .saturating_add(rng_state.below_duration(config.jitter))
            .max(super::schedule::ABSOLUTE_MIN_DELAY);
        if sleep_or_stop(tick, ctx.stop()).await {
            return JobExit::Stopped;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_slot_today_when_still_ahead() {
        let after = WallTime {
            days_since_epoch: 10,
            minutes_since_midnight: 300,
        };
        assert_eq!(
            next_daily_occurrence("06:00", after),
            WallTime {
                days_since_epoch: 10,
                minutes_since_midnight: 360,
            }
        );
    }

    #[test]
    fn next_slot_tomorrow_once_passed() {
        let after = WallTime {
            days_since_epoch: 10,
            minutes_since_midnight: 360,
        };
        // Strictly after: the exact slot rolls to tomorrow.
        assert_eq!(
            next_daily_occurrence("06:00", after),
            WallTime {
                days_since_epoch: 11,
                minutes_since_midnight: 360,
            }
        );
    }

    #[test]
    fn garbage_poll_time_falls_back_to_six() {
        let after = WallTime::day(10);
        for poll in ["nope", "25:00", "10:99", "10", "10:00:00", ""] {
            assert_eq!(
                next_daily_occurrence(poll, after),
                WallTime {
                    days_since_epoch: 10,
                    minutes_since_midnight: 360,
                },
                "{poll:?}"
            );
        }
    }
}
