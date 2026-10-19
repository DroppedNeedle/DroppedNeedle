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
//! portably. Wall time comes from the supervisor's [`Clock`]; tests set a
//! manual one.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::BoxFuture;
use super::schedule::Schedule;
use super::supervisor::{AlreadyRunning, Next, Supervisor, Tick, TickCtx, TickError, TickResult};
use crate::clock::Clock;

/// Name of the watcher loop.
pub const JOB_NAME: &str = "events-watcher";

/// Row code of a failed sweep.
pub const FAILED: &str = "EVENTS_SWEEP_FAILED";

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
    /// Saw the stop and quit between artists.
    Stopped,
}

/// Sweep the live-event sources behind a seam.
pub trait EventsWatcher: Send + Sync + 'static {
    /// Run one full sweep. `skip_recent_hours` narrows the catch-up sweep to
    /// artists not swept within the window; daily sweeps pass nothing. A
    /// sweep walks artists for up to an hour, so it watches `stop` between
    /// artists and returns [`SweepEnd::Stopped`] once it is cancelled.
    fn run_sweep(
        &self,
        skip_recent_hours: Option<f64>,
        stop: CancellationToken,
    ) -> BoxFuture<'_, Result<SweepEnd, String>>;
}

/// No sweep wired (states without a database): every sweep finishes at
/// once without walking anything.
impl<W: EventsWatcher> EventsWatcher for Option<W> {
    fn run_sweep(
        &self,
        skip_recent_hours: Option<f64>,
        stop: CancellationToken,
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
/// all a daily scheduler needs, and the shape keeps the scheduler free of
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

    /// The UTC day and minute of a unix millisecond time.
    pub fn from_unix_ms(now_ms: i64) -> Self {
        let secs = now_ms.div_euclid(1000);
        Self {
            days_since_epoch: secs.div_euclid(86_400),
            minutes_since_midnight: (secs.rem_euclid(86_400) / 60) as u16,
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

/// The scheduler's pacing: a turn a minute after the boot hold-off.
pub fn default_schedule() -> Schedule {
    Schedule::new(SCHEDULER_TICK)
        .with_initial_delay(INITIAL_DELAY)
        .with_jitter(DEFAULT_JITTER)
}

/// One scheduler turn: sweep when the catch-up or the day's slot is due.
struct WatcherTick<W, P> {
    watcher: W,
    poll_source: P,
    clock: Arc<dyn Clock>,
    last_sweep: Option<WallTime>,
}

impl<W: EventsWatcher, P: PollTimeSource> Tick for WatcherTick<W, P> {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let now = WallTime::from_unix_ms(self.clock.now_ms());
        let due = match self.last_sweep {
            None => Some(Some(CATCHUP_SKIP_RECENT_HOURS)),
            Some(previous) => (now
                >= next_daily_occurrence(&self.poll_source.poll_time(), previous))
            .then_some(None),
        };
        let Some(skip_recent_hours) = due else {
            return Ok(Next::Continue);
        };
        let swept = self
            .watcher
            .run_sweep(skip_recent_hours, ctx.token().clone())
            .await;
        // A failing sweep still waits for the next slot.
        self.last_sweep = Some(now);
        match swept {
            Ok(_) => Ok(Next::Continue),
            Err(cause) => Err(TickError::new(FAILED, cause)),
        }
    }
}

/// Start the watcher loop on `schedule` (normally [`default_schedule`]),
/// reading wall time from the supervisor's clock.
pub fn spawn_on<W, P>(
    supervisor: &Supervisor,
    watcher: W,
    poll_source: P,
    schedule: Schedule,
) -> Result<(), AlreadyRunning>
where
    W: EventsWatcher,
    P: PollTimeSource,
{
    supervisor.periodic(
        JOB_NAME,
        schedule,
        WatcherTick {
            watcher,
            poll_source,
            clock: Arc::clone(supervisor.clock()),
            last_sweep: None,
        },
    )
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
