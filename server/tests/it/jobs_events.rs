//! Events briefs: the daily watcher loop plus the registered kick.
//!
//! The watcher runs one catch-up sweep after boot (skipping recently swept
//! artists), then a full sweep each day at the admin's `poll_time`, which it
//! re-reads every tick. A failing sweep waits for the next slot instead of
//! retrying hot. The kick fires one immediate sweep after a settings save and
//! now registers in the job table, closing the v2 gap where the kick ran on
//! an untracked task. Wall time is scripted by hand; loop time is virtual.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use droppedneedle::jobs::events_kick::{self, KickOutcome};
use droppedneedle::jobs::events_watcher::{
    self, EventsWatcher, PollTimeSource, WallTime, WatchClock,
};
use droppedneedle::jobs::registry::{
    BoxFuture, JobRegistry, JobState, MemoryRegistryStore, RegistryStore,
};

type TestRegistry = JobRegistry<MemoryRegistryStore>;

fn registry() -> TestRegistry {
    JobRegistry::new(MemoryRegistryStore::new())
}

async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

/// Scripted watcher: counts sweeps and remembers the skip windows.
#[derive(Clone, Default)]
struct FakeWatcher {
    sweeps: Arc<AtomicU64>,
    windows: Arc<Mutex<Vec<Option<f64>>>>,
    failures_left: Arc<AtomicU64>,
}

impl FakeWatcher {
    fn fail_next(&self, times: u64) {
        self.failures_left.store(times, Ordering::SeqCst);
    }
}

impl EventsWatcher for FakeWatcher {
    fn run_sweep(&self, skip_recent_hours: Option<f64>) -> BoxFuture<'_, Result<(), String>> {
        let sweeps = Arc::clone(&self.sweeps);
        let windows = Arc::clone(&self.windows);
        let failures_left = Arc::clone(&self.failures_left);
        Box::pin(async move {
            sweeps.fetch_add(1, Ordering::SeqCst);
            windows.lock().unwrap().push(skip_recent_hours);
            if failures_left.load(Ordering::SeqCst) > 0 {
                failures_left.fetch_sub(1, Ordering::SeqCst);
                Err("sources down".to_owned())
            } else {
                Ok(())
            }
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

/// Hand-driven wall clock.
#[derive(Clone)]
struct ManualClock {
    now: Arc<Mutex<WallTime>>,
}

impl ManualClock {
    fn at(day: i64, minutes: u16) -> Self {
        Self {
            now: Arc::new(Mutex::new(WallTime {
                days_since_epoch: day,
                minutes_since_midnight: minutes,
            })),
        }
    }

    fn set(&self, day: i64, minutes: u16) {
        *self.now.lock().unwrap() = WallTime {
            days_since_epoch: day,
            minutes_since_midnight: minutes,
        };
    }
}

impl WatchClock for ManualClock {
    fn now(&self) -> WallTime {
        *self.now.lock().unwrap()
    }
}

async fn spawn_test_loop(
    registry: &TestRegistry,
    watcher: FakeWatcher,
    poll: FakePollTime,
    clock: ManualClock,
) {
    events_watcher::spawn_on(registry, watcher, poll, clock)
        .await
        .expect("spawn wins");
    settle().await;
}

#[tokio::test(start_paused = true)]
async fn watcher_catch_up_runs_once_with_skip_window() {
    let registry = registry();
    let watcher = FakeWatcher::default();
    spawn_test_loop(
        &registry,
        watcher.clone(),
        FakePollTime::new("06:00"),
        ManualClock::at(10, 0),
    )
    .await;

    // Default boot delay is 420 s; the catch-up sweep lands right after.
    tokio::time::advance(Duration::from_secs(420)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);
    assert_eq!(
        watcher.windows.lock().unwrap().as_slice(),
        &[Some(events_watcher::CATCHUP_SKIP_RECENT_HOURS)]
    );

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn watcher_daily_slot_fires_when_clock_crosses() {
    let registry = registry();
    let watcher = FakeWatcher::default();
    let clock = ManualClock::at(10, 5 * 60);
    spawn_test_loop(
        &registry,
        watcher.clone(),
        FakePollTime::new("06:00"),
        clock.clone(),
    )
    .await;

    // Boot catch-up first.
    tokio::time::advance(Duration::from_secs(420)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);

    // Still before the 06:00 slot: ticks pass, no sweep.
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);

    // The wall clock crosses into the slot: a full sweep fires.
    clock.set(10, 6 * 60 + 1);
    tokio::time::advance(Duration::from_secs(120)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 2);
    assert_eq!(watcher.windows.lock().unwrap()[1], None);

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn watcher_failing_sweep_waits_for_next_slot() {
    let registry = registry();
    let watcher = FakeWatcher::default();
    watcher.fail_next(10);
    let clock = ManualClock::at(10, 6 * 60 + 1);
    spawn_test_loop(
        &registry,
        watcher.clone(),
        FakePollTime::new("06:00"),
        clock.clone(),
    )
    .await;

    // The catch-up sweep fails and still counts as the last sweep.
    tokio::time::advance(Duration::from_secs(420)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);

    // Ticks pass without retrying: the failure waits out the daily slot.
    tokio::time::advance(Duration::from_secs(600)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);

    // Tomorrow's slot sweeps again (and fails again, without spinning).
    clock.set(11, 6 * 60 + 1);
    tokio::time::advance(Duration::from_secs(120)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 2);

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn watcher_rereads_poll_time_every_tick() {
    let registry = registry();
    let watcher = FakeWatcher::default();
    let clock = ManualClock::at(10, 5 * 60);
    let poll = FakePollTime::new("06:00");
    spawn_test_loop(&registry, watcher.clone(), poll.clone(), clock.clone()).await;

    tokio::time::advance(Duration::from_secs(420)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);

    // The admin moves the hour earlier; the new slot fires without a restart.
    *poll.value.lock().unwrap() = "05:30".to_owned();
    clock.set(10, 5 * 60 + 31);
    tokio::time::advance(Duration::from_secs(120)).await;
    settle().await;
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 2);

    registry.cancel_all(Duration::from_secs(5)).await;
}

// ---------------------------------------------------------------------------
// Kick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kick_starts_a_registered_sweep() {
    let registry = registry();
    let watcher = FakeWatcher::default();
    assert_eq!(
        events_kick::kick(&registry, watcher.clone()).await,
        KickOutcome::Started
    );
    // The sweep ran and the row landed as stopped.
    for _ in 0..500 {
        if !registry.is_running(events_kick::JOB_NAME) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(watcher.sweeps.load(Ordering::SeqCst), 1);
    let row = registry
        .store()
        .get_job(events_kick::JOB_NAME)
        .await
        .expect("kick row exists");
    assert_eq!(row.state, JobState::Stopped);
    assert!(row.last_heartbeat_at.is_some());
}

#[tokio::test(start_paused = true)]
async fn kick_while_running_is_skipped() {
    let registry = registry();
    // A sweep that holds its task until released keeps the kick live.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    struct Gate {
        sweeps: Arc<AtomicU64>,
        gate: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    }
    impl EventsWatcher for Gate {
        fn run_sweep(&self, skip: Option<f64>) -> BoxFuture<'_, Result<(), String>> {
            let _ = skip;
            let sweeps = Arc::clone(&self.sweeps);
            let gate = Arc::clone(&self.gate);
            Box::pin(async move {
                sweeps.fetch_add(1, Ordering::SeqCst);
                let rx = gate.lock().unwrap().take();
                if let Some(rx) = rx {
                    let _ = rx.await;
                }
                Ok(())
            })
        }
    }
    let sweeps = Arc::new(AtomicU64::new(0));
    let gate = Gate {
        sweeps: Arc::clone(&sweeps),
        gate: release_rx,
    };
    assert_eq!(
        events_kick::kick(&registry, gate).await,
        KickOutcome::Started
    );
    settle().await;
    assert!(registry.is_running(events_kick::JOB_NAME));

    // A second kick while the first is live is skipped, not queued.
    struct Never;
    impl EventsWatcher for Never {
        fn run_sweep(&self, _skip: Option<f64>) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async move {
                panic!("skipped kick must not sweep");
            })
        }
    }
    assert_eq!(
        events_kick::kick(&registry, Never).await,
        KickOutcome::AlreadyRunning
    );

    // Release the first sweep; the kick clears and a later kick starts fresh.
    let _ = release_tx.send(());
    for _ in 0..100 {
        if !registry.is_running(events_kick::JOB_NAME) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(!registry.is_running(events_kick::JOB_NAME));
    assert_eq!(sweeps.load(Ordering::SeqCst), 1);
}
