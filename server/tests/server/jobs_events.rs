//! The events watcher: one catch-up run with a skip window, and the poll
//! time re-read on every tick.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use droppedneedle_kernel::clock::ManualClock;
use droppedneedle_kernel::db::DbRuntime;
use droppedneedle_kernel::jobs::events_watcher::{self, EventsWatcher, PollTimeSource, SweepEnd};
use droppedneedle_kernel::jobs::{BoxFuture, CancellationToken, Schedule, Supervisor};

use crate::common::{ScratchDir, scratch_runtime};

/// Scripted watcher: counts sweeps and remembers the skip windows.
#[derive(Clone, Default)]
struct FakeWatcher {
    sweeps: Arc<AtomicU64>,
    windows: Arc<Mutex<Vec<Option<f64>>>>,
}

impl EventsWatcher for FakeWatcher {
    fn run_sweep(
        &self,
        skip_recent_hours: Option<f64>,
        _stop: CancellationToken,
    ) -> BoxFuture<'_, Result<SweepEnd, String>> {
        let sweeps = Arc::clone(&self.sweeps);
        let windows = Arc::clone(&self.windows);
        Box::pin(async move {
            sweeps.fetch_add(1, Ordering::SeqCst);
            windows.lock().unwrap().push(skip_recent_hours);
            Ok(SweepEnd::Finished)
        })
    }
}

/// Scripted poll-time setting, flippable mid-run.
#[derive(Clone)]
struct FakePollTime {
    value: Arc<Mutex<String>>,
}

impl FakePollTime {
    fn new(value: &str) -> Self {
        Self {
            value: Arc::new(Mutex::new(value.to_owned())),
        }
    }
}

impl PollTimeSource for FakePollTime {
    fn poll_time(&self) -> String {
        self.value.lock().unwrap().clone()
    }
}

/// Unix milliseconds at `minutes` past midnight on `day`.
fn at(day: i64, minutes: i64) -> i64 {
    (day * 86_400 + minutes * 60) * 1000
}

/// A supervisor on `clock` over a scratch database, with the watcher loop
/// started on the shipped cadence minus its jitter, and the clock paused.
async fn start_watcher(
    tag: &str,
    clock: &ManualClock,
    watcher: FakeWatcher,
    poll: FakePollTime,
) -> (Supervisor, DbRuntime, ScratchDir) {
    let dir = ScratchDir::new(tag);
    let runtime = scratch_runtime(&dir.join("app.db")).await;
    let supervisor = Supervisor::start(
        runtime.db().clone(),
        Arc::new(clock.clone()),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    tokio::time::pause();
    events_watcher::spawn_on(
        &supervisor,
        watcher,
        poll,
        Schedule::new(events_watcher::SCHEDULER_TICK)
            .with_initial_delay(events_watcher::INITIAL_DELAY),
    )
    .expect("spawn wins");
    (supervisor, runtime, dir)
}

async fn stop(supervisor: Supervisor, runtime: DbRuntime) {
    tokio::time::resume();
    supervisor.shutdown(Duration::from_secs(5)).await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn watcher_catch_up_runs_once_with_skip_window() {
    let watcher = FakeWatcher::default();
    let clock = ManualClock::at(at(10, 0));
    let (supervisor, runtime, _dir) = start_watcher(
        "events-catch-up",
        &clock,
        watcher.clone(),
        FakePollTime::new("06:00"),
    )
    .await;

    // The boot hold-off is 420 s; the catch-up sweep lands right after, and
    // the next minute's tick sweeps nothing.
    tokio::time::sleep(Duration::from_secs(421)).await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        watcher.windows.lock().unwrap().as_slice(),
        &[Some(events_watcher::CATCHUP_SKIP_RECENT_HOURS)]
    );
    stop(supervisor, runtime).await;
}

#[tokio::test]
async fn watcher_rereads_poll_time_every_tick() {
    let watcher = FakeWatcher::default();
    let clock = ManualClock::at(at(10, 5 * 60));
    let poll = FakePollTime::new("06:00");
    let (supervisor, runtime, _dir) =
        start_watcher("events-reread", &clock, watcher.clone(), poll.clone()).await;

    tokio::time::sleep(Duration::from_secs(421)).await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);

    // The admin moves the hour earlier; the new slot fires without a restart.
    *poll.value.lock().unwrap() = "05:30".to_owned();
    clock.set(at(10, 5 * 60 + 31));
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 2);
    stop(supervisor, runtime).await;
}
